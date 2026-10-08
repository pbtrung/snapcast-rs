mod auth;
mod config;
mod control;
mod http;
mod jsonrpc;
mod notify;
mod stream;
mod ws_transport;

use clap::Parser;
use snapcast_server::{ServerCommand, ServerEvent, SnapServer};

/// Snapcast server — synchronized multiroom audio server.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    /// Config file path
    #[arg(short, long, default_value = "/etc/snapserver.conf")]
    config: String,

    /// TCP port for binary protocol (client connections)
    #[arg(long)]
    stream_port: Option<u16>,

    /// Bind address for binary protocol (client connections)
    #[arg(long)]
    stream_bind_address: Option<String>,

    /// TCP port for JSON-RPC control
    #[arg(long)]
    control_port: Option<u16>,

    /// Bind address for JSON-RPC control
    #[arg(long)]
    control_bind_address: Option<String>,

    /// HTTP port for JSON-RPC + Snapweb
    #[arg(long)]
    http_port: Option<u16>,

    /// Bind address for HTTP JSON-RPC + Snapweb
    #[arg(long)]
    http_bind_address: Option<String>,

    /// Path to Snapweb static files
    #[arg(long)]
    doc_root: Option<String>,

    /// Audio buffer size in milliseconds
    #[arg(long)]
    buffer: Option<u32>,

    /// Default codec: pcm, flac, opus
    #[arg(long)]
    codec: Option<String>,

    /// Default sample format
    #[arg(long)]
    sampleformat: Option<String>,

    /// Stream source URI (can be specified multiple times)
    #[arg(long = "source")]
    sources: Vec<String>,

    /// Require authentication on the control/HTTP/WebSocket APIs
    #[arg(long = "auth")]
    auth: bool,

    /// Secret used to sign/verify control-API auth tokens (required with --auth)
    #[arg(long = "auth-secret")]
    auth_secret: Option<String>,

    /// Log filter
    #[arg(long, default_value = "info")]
    logfilter: String,
}

/// Parse a sample format and require concrete PCM, so a typo or wildcard is
/// reported instead of silently falling back to the default format.
fn parse_pcm_format(value: &str) -> anyhow::Result<snapcast_proto::SampleFormat> {
    let format: snapcast_proto::SampleFormat = value
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid sample format '{value}': {e}"))?;
    format.validate_concrete_pcm()?;
    Ok(format)
}

/// Library stream config for a configured source whose reader produces
/// `format`. The encoder must use that same format, or a non-default
/// `sampleformat` is mis-framed before encoding.
fn source_stream_config(
    source: &str,
    format: snapcast_proto::SampleFormat,
) -> snapcast_server::StreamConfig {
    snapcast_server::StreamConfig {
        sample_format: Some(format.to_string()),
        uri: Some(source.trim().trim_matches(['\'', '"']).to_string()),
        ..Default::default()
    }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(&cli.logfilter)
        .init();

    // Load config file, then merge CLI overrides
    let file_config = config::parse_config_file(&cli.config);
    let server_config = config::merge_cli(
        file_config,
        config::CliOverrides {
            stream_bind_address: cli.stream_bind_address,
            stream_port: cli.stream_port,
            control_bind_address: cli.control_bind_address,
            control_port: cli.control_port,
            http_bind_address: cli.http_bind_address,
            http_port: cli.http_port,
            doc_root: cli.doc_root,
            buffer: cli.buffer,
            codec: cli.codec,
            sampleformat: cli.sampleformat,
            sources: cli.sources,
            auth_enabled: cli.auth,
            auth_secret: cli.auth_secret,
        },
    );

    // Validate auth before doing anything else: refuse to start an enabled-but-
    // secretless config that would otherwise sign tokens with an empty key.
    server_config.auth.validate()?;
    if server_config.auth.enabled {
        tracing::info!("Control API authentication: ENABLED");
    } else {
        tracing::warn!(
            "Control API authentication: DISABLED — anyone who can reach the \
             control/HTTP/WebSocket ports has full control of the server"
        );
    }

    let codec = server_config.server.codec.clone();
    let default_format = parse_pcm_format(&server_config.server.sample_format)
        .map_err(|e| anyhow::anyhow!("[stream] sampleformat: {e}"))?;

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let (mut server, mut events) = SnapServer::new(server_config.server);

        // Ctrl-C handler — must be first so it works even if setup fails
        let cmd = server.command_sender();
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            tracing::info!("Received Ctrl-C, shutting down");
            cmd.send(ServerCommand::Stop).await.ok();
            // Force exit after 2s or on second Ctrl+C
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_secs(2));
                tracing::warn!("Graceful shutdown timed out, forcing exit");
                std::process::exit(1);
            });
            // Second Ctrl+C → immediate exit
            tokio::signal::ctrl_c().await.ok();
            std::process::exit(1);
        });

        // Set up streams from configured sources
        for source in &server_config.sources {
            let parsed = match stream::uri::StreamUri::parse(source) {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!(source, error = %e, "Skipping malformed stream URI");
                    continue;
                }
            };
            let name = parsed.param("name").unwrap_or("default").to_string();
            let format = match parsed.param("sampleformat") {
                Some(s) => match parse_pcm_format(s) {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::error!(source, error = %e, "Skipping stream with invalid sampleformat");
                        continue;
                    }
                },
                None => default_format,
            };

            let tx = server.add_stream_with_config(&name, source_stream_config(source, format));

            // Chunk size matches codec block size:
            // FLAC: 1152 frames (the encoder's fixed block size)
            // Others: 20 ms
            const FLAC_BLOCK_FRAMES: usize = 1152;
            const DEFAULT_CHUNK_MS: usize = 20;
            let codec_name = codec
                .split_once(':')
                .map_or(codec.as_str(), |(name, _)| name);
            let chunk_frames = match codec_name {
                "flac" => FLAC_BLOCK_FRAMES,
                _ => (format.rate() as usize * DEFAULT_CHUNK_MS) / 1000, // 20ms
            };

            // Start stream reader
            let reader_handle = match parsed.scheme.as_str() {
                "pipe" => stream::pipe::start(parsed, format, chunk_frames, tx),
                "file" => stream::file::start(parsed, format, chunk_frames, tx),
                "process" => stream::process::start(parsed, format, chunk_frames, tx),
                "tcp" => stream::tcp::start(parsed, format, chunk_frames, tx),
                other => {
                    tracing::error!(scheme = other, "Unsupported stream scheme");
                    continue;
                }
            };

            if let Err(e) = reader_handle {
                tracing::error!(source, error = %e, "Failed to start stream reader");
            }
        }

        // JSON-RPC control servers
        let (notify_tx, _) = tokio::sync::broadcast::channel::<serde_json::Value>(256);
        let auth_cfg = std::sync::Arc::new(server_config.auth.clone());

        // TCP JSON-RPC control
        let control_cfg = control::ControlConfig {
            bind_address: server_config.control_bind_address.clone(),
            port: server_config.control_port,
            notify_tx: notify_tx.clone(),
            auth_config: std::sync::Arc::clone(&auth_cfg),
            cmd_tx: server.command_sender(),
        };
        tokio::spawn(async move {
            if let Err(e) = control::run_tcp(control_cfg).await {
                tracing::error!(error = %e, "Control server error");
            }
        });

        // HTTP/WebSocket + Snapweb
        let http_cfg = http::HttpConfig {
            bind_address: server_config.http_bind_address.clone(),
            port: server_config.http_port,
            doc_root: server_config.doc_root.clone(),
            notify_tx: notify_tx.clone(),
            auth_config: std::sync::Arc::clone(&auth_cfg),
            cmd_tx: server.command_sender(),
            client_acceptor: server.client_acceptor(),
        };
        tokio::spawn(async move {
            if let Err(e) = http::run_http(http_cfg).await {
                tracing::error!(error = %e, "HTTP server error");
            }
        });

        // Broadcast server events as JSON-RPC notifications. This is the only
        // source of change notifications: the library emits an event for every
        // mutating control command as well as for audio-client activity.
        let event_notify_tx = notify_tx.clone();
        let event_cmd_tx = server.command_sender();
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                let notification: Option<serde_json::Value> = match event {
                    ServerEvent::ClientConnected { id, .. } => {
                        let client_json = get_client_from_status(&event_cmd_tx, &id).await;
                        Some(serde_json::json!({
                            "jsonrpc": "2.0",
                            "method": "Client.OnConnect",
                            "params": {"id": id, "client": client_json}
                        }))
                    }
                    ServerEvent::ClientDisconnected { id } => {
                        let client_json = get_client_from_status(&event_cmd_tx, &id).await;
                        Some(serde_json::json!({
                            "jsonrpc": "2.0",
                            "method": "Client.OnDisconnect",
                            "params": {"id": id, "client": client_json}
                        }))
                    }
                    ServerEvent::ClientVolumeChanged {
                        client_id,
                        volume,
                        muted,
                    } => Some(notify::client_on_volume_changed(&client_id, volume, muted)),
                    ServerEvent::ClientLatencyChanged { client_id, latency } => {
                        Some(notify::client_on_latency_changed(&client_id, latency))
                    }
                    ServerEvent::ClientNameChanged { client_id, name } => {
                        Some(notify::client_on_name_changed(&client_id, &name))
                    }
                    ServerEvent::GroupStreamChanged {
                        group_id,
                        stream_id,
                    } => Some(notify::group_on_stream_changed(&group_id, &stream_id)),
                    ServerEvent::GroupMuteChanged { group_id, muted } => {
                        Some(notify::group_on_mute(&group_id, muted))
                    }
                    ServerEvent::GroupNameChanged { group_id, name } => {
                        Some(notify::group_on_name_changed(&group_id, &name))
                    }
                    ServerEvent::StreamStatus { stream_id, status } => {
                        tracing::info!(stream_id, status, "Stream status");
                        // Fetch full stream object for the notification
                        let full_status = get_full_status(&event_cmd_tx).await;
                        let stream_json = full_status["server"]["streams"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .find(|s| s["id"].as_str() == Some(&stream_id))
                            .cloned()
                            .unwrap_or_default();
                        Some(serde_json::json!({
                            "jsonrpc": "2.0",
                            "method": "Stream.OnUpdate",
                            "params": {"id": stream_id, "stream": stream_json}
                        }))
                    }
                    ServerEvent::StreamMetaChanged {
                        stream_id,
                        metadata,
                    } => Some(serde_json::json!({
                        "jsonrpc": "2.0",
                        "method": "Stream.OnProperties",
                        "params": {"id": stream_id, "properties": metadata}
                    })),
                    ServerEvent::ServerUpdated => {
                        let status = get_full_status(&event_cmd_tx).await;
                        Some(serde_json::json!({
                            "jsonrpc": "2.0",
                            "method": "Server.OnUpdate",
                            "params": status
                        }))
                    }
                    _ => None,
                };
                if let Some(n) = notification {
                    let _ = event_notify_tx.send(n);
                }
            }
        });

        // The library owns no port; the binary binds the audio listener and
        // hands it to serve().
        let listener = tokio::net::TcpListener::bind((
            server_config.stream_bind_address.as_str(),
            server_config.stream_port,
        ))
        .await?;
        server.serve(listener).await
    })
}

/// Fetch full server status as JSON via GetStatus command.
async fn get_full_status(cmd_tx: &tokio::sync::mpsc::Sender<ServerCommand>) -> serde_json::Value {
    let (tx, rx) = tokio::sync::oneshot::channel();
    if cmd_tx
        .send(ServerCommand::GetStatus { response_tx: tx })
        .await
        .is_ok()
        && let Ok(status) = rx.await
    {
        return serde_json::to_value(status).unwrap_or_default();
    }
    serde_json::Value::Null
}

/// Find a client in the current status by ID.
async fn get_client_from_status(
    cmd_tx: &tokio::sync::mpsc::Sender<ServerCommand>,
    client_id: &str,
) -> serde_json::Value {
    let status = get_full_status(cmd_tx).await;
    jsonrpc::find_client(&status, client_id)
        .cloned()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pcm_format_rejects_instead_of_falling_back() {
        assert_eq!(
            parse_pcm_format("44100:24:2").unwrap(),
            snapcast_proto::SampleFormat::new(44100, 24, 2)
        );
        for bad in ["48000:16", "0:16:2", "48000:*:2", "48000:12:2", "garbage"] {
            assert!(parse_pcm_format(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn source_stream_config_carries_source_format_to_encoder() {
        let format = snapcast_proto::SampleFormat::new(44100, 24, 2);
        let cfg = source_stream_config("'pipe:///music?sampleformat=44100:24:2'", format);
        assert_eq!(cfg.sample_format.as_deref(), Some("44100:24:2"));
        assert_eq!(
            cfg.uri.as_deref(),
            Some("pipe:///music?sampleformat=44100:24:2")
        );
    }
}
