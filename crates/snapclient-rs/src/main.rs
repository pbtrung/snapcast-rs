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

    let settings = cli.into_settings()?;

    #[cfg(unix)]
    if let Some(ref daemon) = settings.daemon {
        daemonize(daemon)?;
    }

    warn_unsupported_options(&settings);

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

        // Apply volume, report status to systemd. The controller logs the
        // connection, sync and codec events itself.
        tokio::spawn(async move {
            // A ServerSettings update arrives as both ServerSettings and
            // VolumeChanged, and often leaves the volume as it was.
            let mut applied = None;
            while let Some(event) = events.recv().await {
                match event {
                    ClientEvent::Connected { .. } => applied = None,
                    ClientEvent::Disconnected { .. } => {}
                    ClientEvent::ServerSettings { volume, muted, .. }
                    | ClientEvent::VolumeChanged { volume, muted } => {
                        if applied == Some((volume, muted)) {
                            continue;
                        }
                        applied = Some((volume, muted));
                        tracing::info!(volume, muted, "Volume changed");
                        // Volume is 0-100; never wrap a bogus larger value.
                        mixer.set_volume(volume.min(100) as u8, muted);
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
                    ClientEvent::TimeSyncComplete { .. } => {
                        #[cfg(target_os = "linux")]
                        let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);
                    }
                    ClientEvent::StreamStarted { codec, format } => {
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
                        #[cfg(not(target_os = "linux"))]
                        let _ = (codec, format);
                    }
                }
            }
        });

        // SIGINT, and on Unix SIGTERM/SIGHUP too, as C++ snapclient
        tokio::spawn(async move {
            shutdown_signal().await;
            tracing::info!("Received shutdown signal, shutting down");
            #[cfg(target_os = "linux")]
            let _ = sd_notify::notify(&[sd_notify::NotifyState::Stopping]);
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

/// Resolve on the first shutdown signal.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let (Ok(mut term), Ok(mut hup)) = (
            signal(SignalKind::terminate()),
            signal(SignalKind::hangup()),
        ) else {
            tokio::signal::ctrl_c().await.ok();
            return;
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
            _ = hup.recv() => {}
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await.ok();
}

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
        tracing::warn!(user, "--user is not supported, ignoring");
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
