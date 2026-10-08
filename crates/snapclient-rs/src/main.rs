mod cli;
mod logging;
mod mixer;
mod player;

use clap::Parser;
use snapcast_client::{ClientCommand, ClientConfig, ClientEvent, SnapClient};

fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();

    logging::init(&cli.logsink, &cli.logfilter)?;

    if cli.list {
        list_devices(&cli.player);
        return Ok(());
    }

    #[cfg_attr(not(feature = "mdns"), allow(unused_mut))]
    let mut settings = cli.into_settings()?;

    #[cfg(unix)]
    if let Some(ref daemon) = settings.daemon {
        daemonize(daemon)?;
    }

    warn_unsupported_options(&settings);

    // mDNS discovery if no host specified (the default URL names the service)
    #[cfg(feature = "mdns")]
    if settings.server.host.is_empty() || settings.server.host == MDNS_SERVICE_HOST {
        tracing::info!("No server specified, browsing mDNS for _snapcast._tcp...");
        match discover_snapcast() {
            Ok((host, port)) => {
                settings.server.host = host;
                settings.server.port = port;
            }
            Err(e) => anyhow::bail!("mDNS discovery failed: {e}"),
        }
    }
    #[cfg(not(feature = "mdns"))]
    if settings.server.host.is_empty() || settings.server.host == MDNS_SERVICE_HOST {
        anyhow::bail!("no server URL given, and mDNS discovery is not built in (`mdns` feature)");
    }

    tracing::info!(
        server = %format!(
            "{}://{}:{}",
            settings.server.scheme, settings.server.host, settings.server.port
        ),
        instance = settings.instance,
        "snapclient-rs starting"
    );

    let (mixer, volume_state) = mixer::Mixer::new(&settings.player.mixer);

    let config = ClientConfig {
        scheme: settings.server.scheme.clone(),
        host: settings.server.host.clone(),
        port: settings.server.port,
        auth: settings.server.auth.clone(),
        instance: settings.instance,
        host_id: settings.host_id.clone(),
        latency: settings.player.latency,
        ..ClientConfig::default()
    };
    let rt = tokio::runtime::Runtime::new()?;

    let result = rt.block_on(async {
        // The player reads the shared Stream directly; the decoded-audio
        // channel is for embedders, so drop its receiver.
        let (mut client, mut events, _) = SnapClient::new(config);
        let cmd = client.command_sender();

        // Audio output: cpal callback reads from Stream directly
        let player_stream = std::sync::Arc::clone(&client.stream);
        let player_tp = std::sync::Arc::clone(&client.time_provider);
        tokio::spawn(player::play_audio(player_stream, player_tp, volume_state));

        // Log events + apply volume
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                match event {
                    ClientEvent::Connected { host, port } => {
                        tracing::info!(host, port, "Connected");
                    }
                    ClientEvent::Disconnected { .. } => {}
                    ClientEvent::ServerSettings { volume, muted, .. } => {
                        tracing::info!(volume, muted, "Initial server settings received");
                        mixer.set_volume(volume as u8, muted);
                    }
                    ClientEvent::VolumeChanged { volume, muted } => {
                        tracing::info!(volume, muted, "Volume changed");
                        mixer.set_volume(volume as u8, muted);
                        #[cfg(target_os = "linux")]
                        {
                            let status = format!(
                                "Volume: {}%{}",
                                volume,
                                if muted { " (muted)" } else { "" }
                            );
                            let _ = sd_notify::notify(&[sd_notify::NotifyState::Status(&status)]);
                        }
                    }
                    ClientEvent::TimeSyncComplete { diff_ms } => {
                        tracing::info!(diff_ms, "Time sync complete");
                        #[cfg(target_os = "linux")]
                        let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);
                    }
                    ClientEvent::StreamStarted { codec, format } => {
                        tracing::info!(%codec, %format, "Stream started");
                        #[cfg(target_os = "linux")]
                        {
                            let status = format!(
                                "Playing {} ({} Hz, {} bits, {} ch)",
                                codec,
                                format.rate(),
                                format.bits(),
                                format.channels()
                            );
                            let _ = sd_notify::notify(&[sd_notify::NotifyState::Status(&status)]);
                        }
                    }
                }
            }
        });

        // Ctrl-C
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            tracing::info!("Received Ctrl-C, shutting down");
            cmd.send(ClientCommand::Stop).await.ok();
            // Watchdog: exit even if shutdown stalls.
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_secs(2));
                std::process::exit(0);
            });
        });

        client.run().await
    });
    // Don't wait for the audio thread: it only ends on a format change.
    rt.shutdown_background();
    result?;

    tracing::info!("snapclient-rs terminated");
    Ok(())
}

/// Host of the default URL: browse mDNS for a server instead of resolving it.
const MDNS_SERVICE_HOST: &str = "_snapcast._tcp";

/// Warn about upstream snapclient options that are accepted but not acted on.
fn warn_unsupported_options(settings: &snapcast_client::config::ClientSettings) {
    let player = &settings.player;
    if !player.player_name.is_empty() {
        tracing::warn!(player = %player.player_name, "--player is not supported, ignoring");
    }
    if player.pcm_device.name != "default" {
        tracing::warn!(soundcard = %player.pcm_device.name, "--soundcard is not supported, ignoring");
    }
    if player.sample_format != snapcast_client::SampleFormat::default() {
        tracing::warn!(sampleformat = %player.sample_format, "--sampleformat is not supported, ignoring");
    }
}

fn list_devices(player: &str) {
    let player_name = player.split(':').next().unwrap_or("");
    match player_name {
        #[cfg(target_os = "macos")]
        "coreaudio" | "" => {
            println!("0: Default Output\nCoreAudio default output device\n");
        }
        _ => println!("No device listing available for '{player_name}'"),
    }
}

#[cfg(unix)]
fn daemonize(daemon: &snapcast_client::config::DaemonSettings) -> anyhow::Result<()> {
    if let Some(priority) = daemon.priority {
        let priority = priority.clamp(-20, 19);
        unsafe {
            libc::setpriority(libc::PRIO_PROCESS, 0, priority);
        }
        tracing::info!(priority, "Process priority set");
    }

    if let Some(ref user) = daemon.user {
        tracing::info!(user, "Would drop privileges to user (not yet implemented)");
    }

    unsafe {
        let pid = libc::fork();
        if pid < 0 {
            anyhow::bail!("fork failed");
        }
        if pid > 0 {
            std::process::exit(0);
        }
        libc::setsid();
    }

    tracing::info!("Daemonized");
    Ok(())
}

#[cfg(feature = "mdns")]
fn discover_snapcast() -> anyhow::Result<(String, u16)> {
    use std::time::Duration;
    let mdns = mdns_sd::ServiceDaemon::new()?;
    let service_type = "_snapcast._tcp.local.";
    let receiver = mdns.browse(service_type)?;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);

    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            mdns.stop_browse(service_type).ok();
            anyhow::bail!("timed out after 5s");
        }
        match receiver.recv_timeout(remaining) {
            Ok(mdns_sd::ServiceEvent::ServiceResolved(info)) => {
                // Prefer IPv4: IPv6 results are often link-local, which need a
                // scope id to connect.
                let host = info
                    .get_addresses_v4()
                    .into_iter()
                    .next()
                    .map(|a| a.to_string())
                    .or_else(|| info.get_addresses().iter().next().map(|a| a.to_string()))
                    .unwrap_or_else(|| info.get_hostname().trim_end_matches('.').to_string());
                let port = info.get_port();
                tracing::info!(host = %host, port, "Discovered snapserver via mDNS");
                mdns.stop_browse(service_type).ok();
                return Ok((host, port));
            }
            Ok(_) => continue,
            Err(_) => {
                mdns.stop_browse(service_type).ok();
                anyhow::bail!("mDNS discovery timed out after 5s");
            }
        }
    }
}
