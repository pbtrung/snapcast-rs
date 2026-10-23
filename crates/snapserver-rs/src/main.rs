mod auth;
mod config;
mod control;
mod http;
mod jsonrpc;
mod notify;
mod stream;
mod ws_transport;

use clap::Parser;
use snapcast_server::{ServerCommand, SnapServer};

const DEFAULT_CONFIG_PATH: &str = "/etc/snapserver.conf";

/// Snapcast server — synchronized multiroom audio server.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    /// Config file path [default: /etc/snapserver.conf, used only if present]
    #[arg(short, long)]
    config: Option<String>,

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

    /// Control-API user as <name>:<password> (repeatable; replaces the config
    /// file's users). Visible in the process list: prefer the config file
    #[arg(long = "auth-user", value_name = "NAME:PASSWORD")]
    auth_users: Vec<auth::AuthUser>,

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
/// `sampleformat` is mis-framed before encoding. `codec` is the source's own
/// `codec` parameter, if any (as in C++ snapserver).
fn source_stream_config(
    source: &str,
    format: snapcast_proto::SampleFormat,
    codec: Option<&str>,
) -> snapcast_server::StreamConfig {
    snapcast_server::StreamConfig {
        codec: codec.map(str::to_string),
        sample_format: Some(format.to_string()),
        uri: Some(source.trim().trim_matches(['\'', '"']).to_string()),
    }
}

/// Frames per chunk read from a source: `chunk_ms` when configured, else
/// one FLAC block for FLAC (the encoder's fixed block size) and 20 ms for
/// the other codecs.
fn chunk_frames(
    codec: &str,
    chunk_ms: Option<u32>,
    format: snapcast_proto::SampleFormat,
) -> anyhow::Result<usize> {
    const FLAC_BLOCK_FRAMES: usize = 1152;
    const DEFAULT_CHUNK_MS: u32 = 20;
    let codec_name = codec.split_once(':').map_or(codec, |(name, _)| name);
    let chunk_ms = match chunk_ms {
        Some(ms) => ms,
        None if codec_name == snapcast_proto::CODEC_FLAC => return Ok(FLAC_BLOCK_FRAMES),
        None => DEFAULT_CHUNK_MS,
    };
    let frames = u64::from(format.rate()) * u64::from(chunk_ms) / 1000;
    anyhow::ensure!(
        (1..=u64::from(format.rate())).contains(&frames),
        "chunk_ms must be between 1 and 1000, got {chunk_ms}"
    );
    Ok(frames as usize)
}

/// Bind a listener, so a port that is taken stops the server at startup
/// instead of leaving it running without that service.
async fn bind(what: &str, address: &str, port: u16) -> anyhow::Result<tokio::net::TcpListener> {
    let listener = tokio::net::TcpListener::bind((address, port))
        .await
        .map_err(|e| anyhow::anyhow!("{what}: cannot listen on {address}:{port}: {e}"))?;
    tracing::info!(bind_address = address, port, "{what} listening");
    Ok(listener)
}

/// Wait for Ctrl-C or, on Unix, SIGTERM (how service managers stop the
/// server).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
                return;
            }
            Err(e) => tracing::warn!(error = %e, "Cannot handle SIGTERM"),
        }
    }
    tokio::signal::ctrl_c().await.ok();
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(&cli.logfilter)
        .init();

    // Load config file, then merge CLI overrides
    // A file named with -c must exist; the default one is optional.
    let file_config = match &cli.config {
        Some(path) => config::parse_config_file(path, true)?,
        None => config::parse_config_file(DEFAULT_CONFIG_PATH, false)?,
    };
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
            auth_users: cli.auth_users,
        },
    );

    // Validate auth before doing anything else: refuse to start an enabled
    // config without a secret (tokens would be signed with an empty key) or
    // without a user to log in as.
    server_config.auth.validate()?;
    if server_config.auth.enabled {
        tracing::info!(
            users = server_config.auth.users.len(),
            "Control API authentication: ENABLED"
        );
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
        let (mut server, events) = SnapServer::new(server_config.server);

        // Shutdown handler — must be first so it works even if setup fails
        let cmd = server.command_sender();
        tokio::spawn(async move {
            shutdown_signal().await;
            tracing::info!("Shutting down");
            cmd.send(ServerCommand::Stop).await.ok();
            // Force exit after 2s or on a second signal
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_secs(2));
                tracing::warn!("Graceful shutdown timed out, forcing exit");
                std::process::exit(1);
            });
            shutdown_signal().await;
            std::process::exit(1);
        });

        // The library owns no port; the binary binds every listener up front.
        let stream_listener = bind(
            "Stream server",
            &server_config.stream_bind_address,
            server_config.stream_port,
        )
        .await?;
        let control_listener = match server_config.control_enabled {
            true => Some(
                bind(
                    "Control server (TCP)",
                    &server_config.control_bind_address,
                    server_config.control_port,
                )
                .await?,
            ),
            false => None,
        };
        let http_listener = match server_config.http_enabled {
            true => Some(
                bind(
                    "HTTP/WebSocket server",
                    &server_config.http_bind_address,
                    server_config.http_port,
                )
                .await?,
            ),
            false => None,
        };

        // Set up streams from configured sources
        let mut names = std::collections::HashSet::new();
        for source in &server_config.sources {
            let parsed = match stream::uri::StreamUri::parse(source) {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!(source, error = %e, "Skipping malformed stream URI");
                    continue;
                }
            };
            let name = parsed.param("name").unwrap_or("default").to_string();
            if names.contains(&name) {
                tracing::error!(source, name, "Skipping stream with a duplicate name");
                continue;
            }
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
            let source_codec = parsed.param("codec").map(str::to_string);
            let chunk_ms = match parsed.param("chunk_ms").map(str::parse).transpose() {
                Ok(ms) => ms.or(server_config.chunk_ms),
                Err(e) => {
                    tracing::error!(source, error = %e, "Skipping stream with invalid chunk_ms");
                    continue;
                }
            };
            let chunk_frames = match chunk_frames(
                source_codec.as_deref().unwrap_or(&codec),
                chunk_ms,
                format,
            ) {
                Ok(frames) => frames,
                Err(e) => {
                    tracing::error!(source, error = %e, "Skipping stream");
                    continue;
                }
            };

            // Check the source before registering the stream, so one that
            // can't be read doesn't show up as a stream that never plays.
            let reader = match stream::reader(parsed, format, chunk_frames) {
                Ok(reader) => reader,
                Err(e) => {
                    tracing::error!(source, error = %e, "Skipping stream");
                    continue;
                }
            };
            let tx = server.add_stream_with_config(
                &name,
                source_stream_config(source, format, source_codec.as_deref()),
            );
            reader(tx);
            names.insert(name);
        }

        // JSON-RPC control servers
        let (notify_tx, _) = tokio::sync::broadcast::channel::<notify::Notification>(256);
        let auth_cfg = std::sync::Arc::new(server_config.auth.clone());

        // TCP JSON-RPC control
        if let Some(listener) = control_listener {
            let control_cfg = control::ControlConfig {
                notify_tx: notify_tx.clone(),
                auth_config: std::sync::Arc::clone(&auth_cfg),
                cmd_tx: server.command_sender(),
            };
            tokio::spawn(control::serve(listener, control_cfg));
        }

        // HTTP/WebSocket + Snapweb
        if let Some(listener) = http_listener {
            let http_cfg = http::HttpConfig {
                doc_root: server_config.doc_root.clone(),
                notify_tx: notify_tx.clone(),
                auth_config: std::sync::Arc::clone(&auth_cfg),
                cmd_tx: server.command_sender(),
                client_acceptor: server.client_acceptor(),
                allowed_origins: server_config.http_allowed_origins.clone(),
            };
            tokio::spawn(async move {
                if let Err(e) = http::serve(listener, http_cfg).await {
                    tracing::error!(error = %e, "HTTP server error");
                }
            });
        }

        // Broadcast server events as JSON-RPC notifications.
        tokio::spawn(notify::forward_events(
            events,
            server.command_sender(),
            notify_tx.clone(),
        ));

        server.serve(stream_listener).await
    })
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
        let cfg = source_stream_config("'pipe:///music?sampleformat=44100:24:2'", format, None);
        assert_eq!(cfg.sample_format.as_deref(), Some("44100:24:2"));
        assert_eq!(
            cfg.uri.as_deref(),
            Some("pipe:///music?sampleformat=44100:24:2")
        );
        assert_eq!(cfg.codec, None);
        let cfg = source_stream_config("pipe:///music?codec=pcm", format, Some("pcm"));
        assert_eq!(cfg.codec.as_deref(), Some("pcm"));
    }

    #[test]
    fn chunk_frames_follow_codec_and_chunk_ms() {
        let format = snapcast_proto::SampleFormat::new(48000, 16, 2);
        assert_eq!(chunk_frames("flac", None, format).unwrap(), 1152);
        assert_eq!(chunk_frames("flac:8", None, format).unwrap(), 1152);
        assert_eq!(chunk_frames("opus", None, format).unwrap(), 960);
        assert_eq!(chunk_frames("pcm", Some(10), format).unwrap(), 480);
        assert_eq!(chunk_frames("flac", Some(26), format).unwrap(), 1248);
        assert!(chunk_frames("pcm", Some(0), format).is_err());
        assert!(chunk_frames("pcm", Some(5000), format).is_err());
    }
}
