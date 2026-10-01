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

    let mut settings = cli.into_settings()?;

    #[cfg(unix)]
    if let Some(ref daemon) = settings.daemon {
        daemonize(daemon)?;
    }

    // mDNS discovery if no host specified
    #[cfg(feature = "mdns")]
    if settings.server.host.is_empty() {
        tracing::info!("No server specified, browsing mDNS for _snapcast._tcp...");
        match discover_snapcast() {
            Ok((host, port)) => {
                settings.server.host = host;
                settings.server.port = port;
            }
            Err(e) => anyhow::bail!("mDNS discovery failed: {e}"),
        }
    }

    tracing::info!(
        server = %format!(
            "{}://{}:{}",
            settings.server.scheme, settings.server.host, settings.server.port
        ),
        instance = settings.instance,
        "snapclient-rs starting"
    );

    let mixer_str = match settings.player.mixer.mode {
        snapcast_client::config::MixerMode::Software => "software".to_string(),
        snapcast_client::config::MixerMode::Hardware => {
            format!("hardware:{}", settings.player.mixer.parameter)
        }
        snapcast_client::config::MixerMode::None => "none".to_string(),
        _ => "software".to_string(),
    };
    let (mixer, volume_state) = mixer::Mixer::from_str(&mixer_str);
    let mixer = std::sync::Arc::new(mixer);

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

    rt.block_on(async {
        let (mut client, events, audio_rx) = SnapClient::new(config);
        let cmd = client.command_sender();

        // Audio output: cpal callback reads from Stream directly
        let player_stream = std::sync::Arc::clone(&client.stream);
        let player_tp = std::sync::Arc::clone(&client.time_provider);
        let player_vol = volume_state.clone();
        tokio::spawn(async move {
            player::play_audio(audio_rx, player_stream, player_tp, player_vol).await;
        });

        // Log events + apply volume
        let event_mixer = mixer.clone();
        let mut events = events;
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                match event {
                    ClientEvent::Connected { host, port } => {
                        tracing::info!(host, port, "Connected");
                    }
                    ClientEvent::Disconnected { .. } => {}
                    ClientEvent::ServerSettings { volume, muted, .. } => {
                        tracing::info!(volume, muted, "Initial server settings received");
                        event_mixer.set_volume(volume as u8, muted);
                    }
                    ClientEvent::VolumeChanged { volume, muted } => {
                        tracing::info!(volume, muted, "Volume changed");
                        event_mixer.set_volume(volume as u8, muted);
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
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_secs(2));
                std::process::exit(0);
            });
        });

        client.run().await
    })?;

    tracing::info!("snapclient-rs terminated");
    Ok(())
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
                let host = info
                    .get_addresses()
                    .iter()
                    .next()
                    .map(|a| a.to_string())
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
