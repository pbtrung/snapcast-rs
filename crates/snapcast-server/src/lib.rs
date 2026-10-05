#![deny(unsafe_code)]
#![warn(clippy::redundant_closure)]
#![warn(clippy::implicit_clone)]
#![warn(clippy::uninlined_format_args)]
#![warn(missing_docs)]

//! Snapcast server library — embeddable synchronized multiroom audio server.
//!
//! See also: [`snapcast-client`](https://docs.rs/snapcast-client) for the client library.
//! # Architecture
//!
//! The server is built around a channel-based API matching `snapcast-client`:
//!
//! - [`SnapServer`] is the main entry point
//! - [`ServerEvent`] flows from server → consumer (client connected, stream status, …)
//! - [`ServerCommand`] flows from consumer → server (typed mutations, stop)
//! - [`ClientAcceptor`] hands the server streaming clients the embedder accepted
//!   on other transports (e.g. WebSocket), alongside the TCP listener
//!
//! # Example
//!
//! ```no_run
//! use snapcast_server::{SnapServer, ServerConfig, ServerEvent, ServerCommand};
//!
//! # async fn example() -> anyhow::Result<()> {
//! let config = ServerConfig::default();
//! let (mut server, mut events) = SnapServer::new(config);
//! let _audio_tx = server.add_stream("default");
//! let cmd = server.command_sender();
//!
//! tokio::spawn(async move {
//!     while let Some(event) = events.recv().await {
//!         match event {
//!             ServerEvent::ClientConnected { id, ref hello } => {
//!                 tracing::info!(id, name = hello.host_name, "Client connected");
//!             }
//!             _ => {}
//!         }
//!     }
//! });
//!
//! // The library opens no ports: the embedder binds and hands in the listener.
//! let listener = tokio::net::TcpListener::bind("0.0.0.0:1704").await?;
//! server.serve(listener).await?;
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, mpsc};

// Re-export proto types that embedders need
pub use snapcast_proto::SampleFormat;
pub use snapcast_proto::message::hello::Hello;
pub use snapcast_proto::{DEFAULT_SAMPLE_FORMAT, DEFAULT_STREAM_PORT};

const EVENT_CHANNEL_SIZE: usize = 256;
const COMMAND_CHANNEL_SIZE: usize = 64;
const INCOMING_CLIENT_CHANNEL_SIZE: usize = 16;
const AUDIO_CHANNEL_SIZE: usize = 256;

/// Channel size for F32 embedded sources — backpressure from encoder pacing.
const F32_CHANNEL_SIZE: usize = 1;

/// Audio data pushed by the consumer — either f32 or raw PCM.
#[derive(Debug, Clone)]
pub enum AudioData {
    /// Interleaved f32 samples (from DSP, EQ, AirPlay receivers).
    /// Range: -1.0 to 1.0.
    F32(Vec<f32>),
    /// Raw interleaved PCM bytes at the stream's configured sample format
    /// (from pipe/file/process readers). Byte order: little-endian.
    Pcm(Vec<u8>),
}

/// A timestamped audio frame for server input.
#[derive(Debug, Clone)]
pub struct AudioFrame {
    /// Audio samples.
    pub data: AudioData,
    /// Timestamp in microseconds (server time).
    pub timestamp_usec: i64,
}

/// Buffered sender for F32 audio that handles chunking, timestamping, and gap detection.
///
/// Accumulates variable-size F32 sample buffers and emits fixed-size 20ms chunks
/// with monotonic timestamps. Automatically resets on playback gaps (>500ms).
///
/// Created by [`SnapServer::add_f32_stream`].
pub struct F32AudioSender {
    tx: mpsc::Sender<AudioFrame>,
    buf: Vec<f32>,
    chunk_samples: usize,
    channels: u16,
    sample_rate: u32,
    ts: Option<time::ChunkTimestamper>,
    last_send: std::time::Instant,
}

impl F32AudioSender {
    fn new(tx: mpsc::Sender<AudioFrame>, sample_rate: u32, channels: u16) -> Self {
        let chunk_samples = (sample_rate as usize * 20 / 1000) * channels as usize;
        Self {
            tx,
            buf: Vec::with_capacity(chunk_samples * 2),
            chunk_samples,
            channels,
            sample_rate,
            ts: None,
            last_send: std::time::Instant::now(),
        }
    }

    /// Push interleaved F32 samples. Variable-size input is accumulated and
    /// emitted as fixed 20ms chunks. Returns when all complete chunks are sent.
    pub async fn send(
        &mut self,
        samples: &[f32],
    ) -> Result<(), mpsc::error::SendError<AudioFrame>> {
        let now = std::time::Instant::now();
        if now.duration_since(self.last_send) > std::time::Duration::from_millis(500) {
            self.ts = None;
            self.buf.clear();
        }
        self.last_send = now;

        self.buf.extend_from_slice(samples);
        let ch = self.channels.max(1) as usize;
        while self.buf.len() >= self.chunk_samples {
            let chunk: Vec<f32> = self.buf.drain(..self.chunk_samples).collect();
            let frames = (self.chunk_samples / ch) as u32;
            let ts = self
                .ts
                .get_or_insert_with(|| time::ChunkTimestamper::new(self.sample_rate));
            let timestamp_usec = ts.next(frames);
            self.tx
                .send(AudioFrame {
                    data: AudioData::F32(chunk),
                    timestamp_usec,
                })
                .await?;
        }
        Ok(())
    }

    /// Flush remaining samples (< 20ms) as a final short chunk.
    /// Call at end-of-track to avoid losing the last few milliseconds.
    pub async fn flush(&mut self) -> Result<(), mpsc::error::SendError<AudioFrame>> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let chunk: Vec<f32> = std::mem::take(&mut self.buf);
        let ch = self.channels.max(1) as usize;
        let frames = (chunk.len() / ch) as u32;
        let ts = self
            .ts
            .get_or_insert_with(|| time::ChunkTimestamper::new(self.sample_rate));
        let timestamp_usec = ts.next(frames);
        self.tx
            .send(AudioFrame {
                data: AudioData::F32(chunk),
                timestamp_usec,
            })
            .await
    }
}

/// An encoded audio chunk ready to be sent to clients.
#[derive(Debug, Clone)]
pub struct WireChunkData {
    /// Stream ID this chunk belongs to.
    pub stream_id: String,
    /// Server timestamp in microseconds.
    pub timestamp_usec: i64,
    /// Encoded audio data, shared by every session the chunk is broadcast to.
    pub data: bytes::Bytes,
}

pub mod auth;
pub(crate) mod command;
pub(crate) mod encoder;
pub(crate) mod session;
pub mod state;
pub use state::ServerState;
pub mod status;
pub mod time;

/// Settings update pushed to a streaming client via binary protocol.
#[derive(Debug, Clone)]
pub struct ClientSettingsUpdate {
    /// Target client ID.
    pub client_id: String,
    /// Buffer size in ms.
    pub buffer_ms: i32,
    /// Latency offset in ms.
    pub latency: i32,
    /// Volume (0–100).
    pub volume: u16,
    /// Mute state.
    pub muted: bool,
}

/// Events emitted by the server to the consumer.
#[derive(Debug)]
#[non_exhaustive]
pub enum ServerEvent {
    /// Server state changed and should be persisted by the embedder.
    ///
    /// Carries a snapshot taken right after the mutation. The library performs
    /// no persistence itself; an embedder that wants durability should debounce
    /// these and write the latest snapshot off the event loop.
    StateChanged(state::ServerState),
    /// A client connected via the binary protocol.
    ClientConnected {
        /// Unique client identifier.
        id: String,
        /// The client's Hello message with all connection metadata.
        hello: snapcast_proto::message::hello::Hello,
    },
    /// A client disconnected.
    ClientDisconnected {
        /// Unique client identifier.
        id: String,
    },
    /// A client's volume changed.
    ClientVolumeChanged {
        /// Client ID.
        client_id: String,
        /// New volume (0–100).
        volume: u16,
        /// Mute state.
        muted: bool,
    },
    /// A client's latency changed.
    ClientLatencyChanged {
        /// Client ID.
        client_id: String,
        /// New latency in ms.
        latency: i32,
    },
    /// A client's name changed.
    ClientNameChanged {
        /// Client ID.
        client_id: String,
        /// New name.
        name: String,
    },
    /// A group's stream assignment changed.
    GroupStreamChanged {
        /// Group ID.
        group_id: String,
        /// New stream ID.
        stream_id: String,
    },
    /// A group's mute state changed.
    GroupMuteChanged {
        /// Group ID.
        group_id: String,
        /// Mute state.
        muted: bool,
    },
    /// A stream's status changed (playing, idle, unknown).
    StreamStatus {
        /// Stream identifier.
        stream_id: String,
        /// New status.
        status: String,
    },
    /// Stream metadata/properties changed.
    StreamMetaChanged {
        /// Stream identifier.
        stream_id: String,
        /// Updated properties.
        metadata: std::collections::HashMap<String, serde_json::Value>,
    },
    /// A group's name changed.
    GroupNameChanged {
        /// Group ID.
        group_id: String,
        /// New name.
        name: String,
    },
    /// Server state changed — groups were reorganized (created, deleted, merged).
    ///
    /// Emitted after structural changes like `SetGroupClients` or `DeleteClient`
    /// when the group topology changes. Mirrors `Server.OnUpdate` in the C++ snapserver.
    /// The consumer should re-read server status via `GetStatus`.
    ServerUpdated,
    /// A stream control command was received (play, pause, next, seek, etc.).
    ///
    /// The library forwards this to the embedder since it doesn't own stream readers.
    StreamControl {
        /// Stream ID.
        stream_id: String,
        /// Command name.
        command: String,
        /// Optional parameters.
        params: serde_json::Value,
    },
}

/// Commands the consumer sends to the server.
#[derive(Debug)]
#[non_exhaustive]
pub enum ServerCommand {
    /// Set a client's volume.
    SetClientVolume {
        /// Client ID.
        client_id: String,
        /// Volume (0–100).
        volume: u16,
        /// Mute state.
        muted: bool,
    },
    /// Set a client's latency offset.
    SetClientLatency {
        /// Client ID.
        client_id: String,
        /// Latency in milliseconds.
        latency: i32,
    },
    /// Set a client's display name.
    SetClientName {
        /// Client ID.
        client_id: String,
        /// New name.
        name: String,
    },
    /// Assign a stream to a group.
    SetGroupStream {
        /// Group ID.
        group_id: String,
        /// Stream ID.
        stream_id: String,
    },
    /// Mute/unmute a group.
    SetGroupMute {
        /// Group ID.
        group_id: String,
        /// Mute state.
        muted: bool,
    },
    /// Set a group's display name.
    SetGroupName {
        /// Group ID.
        group_id: String,
        /// New name.
        name: String,
    },
    /// Move clients to a group.
    SetGroupClients {
        /// Group ID.
        group_id: String,
        /// Client IDs.
        clients: Vec<String>,
    },
    /// Delete a client from the server.
    DeleteClient {
        /// Client ID.
        client_id: String,
    },
    /// Set stream metadata (artist, title, album, etc.).
    SetStreamMeta {
        /// Stream ID.
        stream_id: String,
        /// Metadata key-value pairs.
        metadata: std::collections::HashMap<String, serde_json::Value>,
    },
    /// Request dynamic stream addition from an application shell.
    ///
    /// The embeddable library does not own stream readers. Binaries or embedders
    /// must create streams before [`SnapServer::run`] or implement their own
    /// orchestration around this command.
    AddStream {
        /// Stream source URI (e.g. `pipe:///tmp/snapfifo?name=default`).
        uri: String,
        /// Response: the stream ID assigned.
        response_tx: tokio::sync::oneshot::Sender<Result<String, String>>,
    },
    /// Remove a stream source.
    RemoveStream {
        /// Stream ID to remove.
        stream_id: String,
    },
    /// Forward a control command to a stream (play, pause, next, etc.).
    StreamControl {
        /// Stream ID.
        stream_id: String,
        /// Command name (e.g. "next", "previous", "pause", "seek").
        command: String,
        /// Optional command parameter (e.g. seek position).
        params: serde_json::Value,
    },
    /// Get full server status.
    GetStatus {
        /// Response channel.
        response_tx: tokio::sync::oneshot::Sender<status::ServerStatus>,
    },
    /// Stop the server gracefully.
    Stop,
}

/// Default codec based on compiled features.
fn default_codec() -> &'static str {
    #[cfg(feature = "flac")]
    return snapcast_proto::CODEC_FLAC;
    #[cfg(not(feature = "flac"))]
    return snapcast_proto::CODEC_PCM;
}

/// Server configuration for the embeddable library.
pub struct ServerConfig {
    /// Audio buffer size in milliseconds. Default: 1000.
    pub buffer_ms: u32,
    /// Codec name, e.g. "flac", "pcm".
    /// Default: "flac" (feature-dependent: flac > pcm).
    pub codec: String,
    /// Default sample format. Default: 48000:16:2.
    pub sample_format: String,

    /// Auth validator for streaming clients. `None` = no auth required.
    pub auth: Option<std::sync::Arc<dyn auth::AuthValidator>>,
    /// Client filter — called after Hello to accept/reject connections.
    /// `None` = accept all clients.
    pub client_filter: Option<std::sync::Arc<dyn auth::ClientFilter>>,
    /// Initial server state to seed on startup (clients, groups). `None` = empty.
    ///
    /// The library performs no file I/O: the embedder loads this snapshot (e.g.
    /// from disk) before [`SnapServer::new`] and persists subsequent
    /// [`ServerEvent::StateChanged`] events itself.
    pub initial_state: Option<state::ServerState>,
    /// Send audio data to muted clients. Default: false (skip muted, saves bandwidth).
    pub send_audio_to_muted: bool,
    /// Close a streaming client's session once nothing was received from it
    /// for this long, or a write to it stalls for this long. Snapclients
    /// send a time sync request every second, so this only trips on dead or
    /// half-open connections. `None` = never. Default: 10 s.
    pub client_idle_timeout: Option<std::time::Duration>,
    /// Delete clients from the server state (as `DeleteClient` does) once
    /// they have been disconnected for this long. `None` = keep them forever.
    /// Default: 2 days.
    pub remove_disconnected_clients_after: Option<std::time::Duration>,
}

/// Default for [`ServerConfig::client_idle_timeout`].
pub const DEFAULT_CLIENT_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Default for [`ServerConfig::remove_disconnected_clients_after`].
pub const DEFAULT_REMOVE_DISCONNECTED_CLIENTS_AFTER: std::time::Duration =
    std::time::Duration::from_secs(2 * 24 * 60 * 60);

/// Longest pause between sweeps for clients to remove.
const MAX_CLIENT_SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            buffer_ms: snapcast_proto::DEFAULT_BUFFER_MS,
            codec: default_codec().into(),
            sample_format: snapcast_proto::DEFAULT_SAMPLE_FORMAT_STRING.into(),

            auth: None,
            client_filter: None,
            initial_state: None,
            send_audio_to_muted: false,
            client_idle_timeout: Some(DEFAULT_CLIENT_IDLE_TIMEOUT),
            remove_disconnected_clients_after: Some(DEFAULT_REMOVE_DISCONNECTED_CLIENTS_AFTER),
        }
    }
}

/// Per-stream configuration. If `None`, inherits from [`ServerConfig`].
#[derive(Debug, Clone, Default)]
pub struct StreamConfig {
    /// Codec override (e.g. "flac", "opus", "pcm").
    pub codec: Option<String>,
    /// Sample format override (e.g. "48000:16:2").
    pub sample_format: Option<String>,
    /// Source URI (e.g. "pipe:///tmp/snapfifo?name=default"), reported in the
    /// stream's status.
    pub uri: Option<String>,
}

/// A byte-stream transport carrying binary-protocol frames for one client.
///
/// Implemented for every `AsyncRead + AsyncWrite` type; the server only needs
/// whole frames written per `write_all` + `flush` pair, which is what lets a
/// message-based transport such as a WebSocket be adapted to it.
pub trait ClientTransport: AsyncRead + AsyncWrite + Unpin + Send + 'static {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send + 'static> ClientTransport for T {}

/// Hands streaming clients accepted outside the TCP listener to a running
/// [`SnapServer`]. Obtain one with [`SnapServer::client_acceptor`].
///
/// The library still opens no ports: the embedder accepts the connection
/// (for example a WebSocket upgrade on its HTTP server) and passes the
/// resulting transport in.
#[derive(Clone)]
pub struct ClientAcceptor {
    tx: mpsc::Sender<session::IncomingClient>,
}

impl ClientAcceptor {
    /// Serve a streaming client over `transport`. `peer` is the remote
    /// address, reported as the client's IP in the server status.
    ///
    /// # Errors
    /// Fails if the server has stopped (or was never started with
    /// [`SnapServer::serve`]).
    pub async fn accept(
        &self,
        transport: impl ClientTransport,
        peer: std::net::SocketAddr,
    ) -> anyhow::Result<()> {
        self.tx
            .send(session::IncomingClient {
                transport: Box::new(transport),
                peer,
            })
            .await
            .map_err(|_| anyhow::anyhow!("server is not running"))
    }
}

/// The embeddable Snapcast server.
pub struct SnapServer {
    config: ServerConfig,
    /// Externally accepted client transports → session server.
    client_tx: mpsc::Sender<session::IncomingClient>,
    client_rx: Option<mpsc::Receiver<session::IncomingClient>>,
    event_tx: mpsc::Sender<ServerEvent>,
    command_tx: mpsc::Sender<ServerCommand>,
    command_rx: Option<mpsc::Receiver<ServerCommand>>,
    /// Named audio streams — each gets its own encoder at run().
    streams: Vec<(String, StreamConfig, mpsc::Receiver<AudioFrame>)>,
    /// Broadcast channel for encoded chunks → sessions.
    chunk_tx: broadcast::Sender<WireChunkData>,
}

/// Spawn a per-stream encode loop on a dedicated thread.
///
/// Receives `AudioFrame`, passes `AudioData` directly to the encoder,
/// and broadcasts encoded `WireChunkData` to sessions. While no session is
/// connected, frames are still received and paced but not encoded; the
/// encoder is reset when the first session arrives.
fn spawn_stream_encoder(
    stream_id: String,
    mut rx: mpsc::Receiver<AudioFrame>,
    mut enc: Box<dyn encoder::Encoder>,
    chunk_tx: broadcast::Sender<WireChunkData>,
    sample_rate: u32,
    channels: u16,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("encoder runtime");

        rt.block_on(async {
            let mut next_tick: Option<tokio::time::Instant> = None;
            let mut idle = false;
            while let Some(frame) = rx.recv().await {
                // Pace F32 sources to realtime (pipe sources pace naturally via blocking read)
                if let AudioData::F32(ref samples) = frame.data {
                    let num_frames = samples.len() / channels.max(1) as usize;
                    let chunk_dur = std::time::Duration::from_micros(
                        (num_frames as u64 * 1_000_000) / sample_rate as u64,
                    );
                    let now = tokio::time::Instant::now();
                    let tick = next_tick.get_or_insert(now);
                    // Reset on gap (>500ms behind wall clock)
                    if now.checked_duration_since(*tick + chunk_dur)
                        > Some(std::time::Duration::from_millis(500))
                    {
                        *tick = now;
                    }
                    *tick += chunk_dur;
                    tokio::time::sleep_until(*tick).await;
                }
                if chunk_tx.receiver_count() == 0 {
                    if !idle {
                        idle = true;
                        tracing::debug!(stream = %stream_id, "No clients, pausing encoding");
                    }
                    continue;
                }
                if idle {
                    idle = false;
                    enc.reset();
                    tracing::debug!(stream = %stream_id, "Client connected, resuming encoding");
                }
                match enc.encode(&frame.data) {
                    // One WireChunk per packet: clients decode each chunk as a
                    // single codec packet. Empty while the encoder buffers.
                    Ok(packets) => {
                        for packet in packets {
                            let _ = chunk_tx.send(WireChunkData {
                                stream_id: stream_id.clone(),
                                timestamp_usec: frame.timestamp_usec
                                    + packet.offset_frames * 1_000_000 / i64::from(sample_rate),
                                data: packet.data.into(),
                            });
                        }
                    }
                    Err(e) => {
                        tracing::warn!(stream = %stream_id, error = %e, "Encode failed");
                    }
                }
            }
        });
    });
}

/// Parse a configured sample format and require it to be concrete PCM.
fn parse_pcm_format(value: &str) -> anyhow::Result<SampleFormat> {
    let format: SampleFormat = value
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid sample_format '{value}': {e}"))?;
    format
        .validate_concrete_pcm()
        .map_err(|e| anyhow::anyhow!("invalid sample_format '{value}': {e}"))?;
    Ok(format)
}

impl SnapServer {
    /// Create a new server. Returns the server and event receiver.
    pub fn new(config: ServerConfig) -> (Self, mpsc::Receiver<ServerEvent>) {
        let (event_tx, event_rx) = mpsc::channel(EVENT_CHANNEL_SIZE);
        let (command_tx, command_rx) = mpsc::channel(COMMAND_CHANNEL_SIZE);
        let (chunk_tx, _) = broadcast::channel(256);
        let (client_tx, client_rx) = mpsc::channel(INCOMING_CLIENT_CHANNEL_SIZE);
        let server = Self {
            config,
            client_tx,
            client_rx: Some(client_rx),
            event_tx,
            command_tx,
            command_rx: Some(command_rx),
            streams: Vec::new(),
            chunk_tx,
        };
        (server, event_rx)
    }

    /// Add a named audio stream. Returns a sender for pushing audio frames.
    ///
    /// Uses the server's default codec and sample format.
    pub fn add_stream(&mut self, name: &str) -> mpsc::Sender<AudioFrame> {
        self.add_stream_with_config(name, StreamConfig::default())
    }

    /// Add a named F32 audio stream with automatic chunking and timestamping.
    ///
    /// Returns an [`F32AudioSender`] that accepts variable-size F32 sample buffers
    /// and handles 20ms chunking, monotonic timestamps, and gap detection internally.
    ///
    /// # Errors
    /// Returns an error if the server's `sample_format` cannot be parsed or is
    /// not concrete PCM.
    pub fn add_f32_stream(&mut self, name: &str) -> Result<F32AudioSender, String> {
        let sf = parse_pcm_format(&self.config.sample_format).map_err(|e| e.to_string())?;
        let (tx, rx) = mpsc::channel(F32_CHANNEL_SIZE);
        self.streams
            .push((name.to_string(), StreamConfig::default(), rx));
        Ok(F32AudioSender::new(tx, sf.rate(), sf.channels()))
    }

    /// Add a named audio stream with per-stream codec/format overrides.
    pub fn add_stream_with_config(
        &mut self,
        name: &str,
        config: StreamConfig,
    ) -> mpsc::Sender<AudioFrame> {
        let (tx, rx) = mpsc::channel(AUDIO_CHANNEL_SIZE);
        self.streams.push((name.to_string(), config, rx));
        tx
    }

    /// Get a cloneable command sender.
    pub fn command_sender(&self) -> mpsc::Sender<ServerCommand> {
        self.command_tx.clone()
    }

    /// Get a cloneable handle for serving clients over transports other than
    /// the TCP listener passed to [`serve`](Self::serve).
    pub fn client_acceptor(&self) -> ClientAcceptor {
        ClientAcceptor {
            tx: self.client_tx.clone(),
        }
    }

    /// Access the server configuration.
    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    /// Run the server. Blocks until stopped or a fatal error occurs.
    pub async fn serve(&mut self, listener: tokio::net::TcpListener) -> anyhow::Result<()> {
        let mut command_rx = self
            .command_rx
            .take()
            .ok_or_else(|| anyhow::anyhow!("serve() already called"))?;

        let event_tx = self.event_tx.clone();

        let sample_format = parse_pcm_format(&self.config.sample_format)?;

        anyhow::ensure!(
            !self.streams.is_empty(),
            "No streams configured — call add_stream() before run()"
        );

        tracing::info!(
            local_addr = ?listener.local_addr().ok(),
            "Snapserver starting"
        );

        // Create default encoder — used for codec header and first default stream
        let default_enc_config = encoder::EncoderConfig {
            codec: self.config.codec.clone(),
            format: sample_format,
            options: String::new(),
        };
        let default_enc = encoder::create(&default_enc_config)?;

        // Spawn per-stream encode loops — reuse default_enc for first default stream
        let chunk_tx = self.chunk_tx.clone();
        let streams = std::mem::take(&mut self.streams);
        let mut default_enc = Some(default_enc);

        // Shared state for command handlers — seeded from the embedder-supplied
        // snapshot (the library reads no files).
        let mut initial_state = self.config.initial_state.take().unwrap_or_default();
        // No session exists yet, whatever the snapshot recorded (e.g. one
        // saved while clients were connected).
        for client in initial_state.clients.values_mut() {
            client.connected = false;
        }
        let shared_state = Arc::new(tokio::sync::Mutex::new(initial_state));

        // Create session server before stream registration
        // (first_stream_name set in loop below, but SessionServer only needs it for default routing)
        let first_name = streams
            .first()
            .map(|(n, _, _)| n.clone())
            .unwrap_or_default();
        let session_srv = Arc::new(session::SessionServer::new(session::SessionServerConfig {
            buffer_ms: self.config.buffer_ms as i32,
            auth: self.config.auth.clone(),
            client_filter: self.config.client_filter.clone(),
            shared_state: Arc::clone(&shared_state),
            default_stream: first_name.clone(),
            send_audio_to_muted: self.config.send_audio_to_muted,
            idle_timeout: self.config.client_idle_timeout,
        }));

        for (name, stream_cfg, rx) in streams {
            {
                let mut s = shared_state.lock().await;
                let uri = stream_cfg.uri.clone().unwrap_or_default();
                if let Some(existing) = s.streams.iter_mut().find(|st| st.id == name) {
                    if !uri.is_empty() {
                        existing.uri = uri;
                    }
                } else {
                    s.streams.push(state::StreamInfo {
                        id: name.clone(),
                        status: "idle".into(),
                        uri,
                        properties: Default::default(),
                    });
                }
            }
            let mut active_format = sample_format;
            let enc = if stream_cfg.codec.is_none() && stream_cfg.sample_format.is_none() {
                if let Some(enc) = default_enc.take() {
                    enc
                } else {
                    encoder::create(&default_enc_config)?
                }
            } else {
                let stream_codec = stream_cfg.codec.as_deref().unwrap_or(&self.config.codec);
                let stream_format = match stream_cfg.sample_format.as_deref() {
                    Some(s) => {
                        parse_pcm_format(s).map_err(|e| anyhow::anyhow!("stream '{name}': {e}"))?
                    }
                    None => sample_format,
                };
                active_format = stream_format;
                encoder::create(&encoder::EncoderConfig {
                    codec: stream_codec.to_string(),
                    format: stream_format,
                    options: String::new(),
                })?
            };
            tracing::info!(stream = %name, codec = enc.name(), format = %active_format, "Stream registered");
            session_srv
                .register_stream_codec(&name, enc.name(), enc.header())
                .await;
            spawn_stream_encoder(
                name,
                rx,
                enc,
                chunk_tx.clone(),
                active_format.rate(),
                active_format.channels(),
            );
        }

        let incoming_clients = self
            .client_rx
            .take()
            .ok_or_else(|| anyhow::anyhow!("serve() already called"))?;
        let session_for_run = Arc::clone(&session_srv);
        let session_event_tx = event_tx.clone();
        let session_chunk_tx = self.chunk_tx.clone();
        let session_handle = tokio::spawn(async move {
            if let Err(e) = session_for_run
                .run(
                    listener,
                    incoming_clients,
                    session_chunk_tx,
                    session_event_tx,
                )
                .await
            {
                tracing::error!(error = %e, "Session server error");
            }
        });

        let dispatcher = command::Dispatcher {
            shared_state: Arc::clone(&shared_state),
            session_srv: Arc::clone(&session_srv),
            event_tx: event_tx.clone(),
            buffer_ms: self.config.buffer_ms as i32,
        };

        let remove_after = self.config.remove_disconnected_clients_after;
        let mut client_sweep =
            tokio::time::interval(remove_after.map_or(MAX_CLIENT_SWEEP_INTERVAL, |d| {
                d.min(MAX_CLIENT_SWEEP_INTERVAL)
            }));
        client_sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        // Main loop
        loop {
            tokio::select! {
                _ = client_sweep.tick(), if remove_after.is_some() => {
                    if let Some(after) = remove_after {
                        dispatcher.remove_disconnected_clients(after).await;
                    }
                }
                cmd = command_rx.recv() => {
                    match cmd {
                        Some(ServerCommand::Stop) | None => {
                            tracing::info!("Server stopped");
                            session_handle.abort();
                            return Ok(());
                        }
                        Some(cmd) => dispatcher.dispatch(cmd).await,
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// Encoder that counts calls and echoes its input.
    struct CountingEncoder {
        encodes: Arc<AtomicUsize>,
        resets: Arc<AtomicUsize>,
    }

    impl encoder::Encoder for CountingEncoder {
        fn name(&self) -> &str {
            "count"
        }

        fn header(&self) -> &[u8] {
            &[]
        }

        fn encode(&mut self, input: &AudioData) -> anyhow::Result<Vec<encoder::EncodedPacket>> {
            self.encodes.fetch_add(1, Ordering::SeqCst);
            let AudioData::Pcm(data) = input else {
                anyhow::bail!("expected Pcm");
            };
            Ok(vec![encoder::EncodedPacket {
                data: data.clone(),
                offset_frames: 0,
            }])
        }

        fn reset(&mut self) {
            self.resets.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn pcm_frame(timestamp_usec: i64) -> AudioFrame {
        AudioFrame {
            data: AudioData::Pcm(vec![0u8; 960 * 4]),
            timestamp_usec,
        }
    }

    /// Wait until the encode loop has taken every queued frame, then give it a
    /// moment to finish handling the last one (Pcm frames are not paced).
    async fn drain(tx: &mpsc::Sender<AudioFrame>) {
        while tx.capacity() < tx.max_capacity() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    /// Encoder that splits every input into two packets 480 frames apart,
    /// the first starting 100 frames before the input.
    struct SplittingEncoder;

    impl encoder::Encoder for SplittingEncoder {
        fn name(&self) -> &str {
            "split"
        }

        fn header(&self) -> &[u8] {
            &[]
        }

        fn encode(&mut self, _input: &AudioData) -> anyhow::Result<Vec<encoder::EncodedPacket>> {
            Ok(vec![
                encoder::EncodedPacket {
                    data: vec![1],
                    offset_frames: -100,
                },
                encoder::EncodedPacket {
                    data: vec![2],
                    offset_frames: 380,
                },
            ])
        }
    }

    #[tokio::test]
    async fn each_packet_is_its_own_chunk_with_offset_timestamp() {
        let (tx, rx) = mpsc::channel(AUDIO_CHANNEL_SIZE);
        let (chunk_tx, mut chunks) = broadcast::channel(16);
        spawn_stream_encoder(
            "s".into(),
            rx,
            Box::new(SplittingEncoder),
            chunk_tx,
            48000,
            2,
        );

        tx.send(pcm_frame(1_000_000)).await.unwrap();
        let first = chunks.recv().await.unwrap();
        let second = chunks.recv().await.unwrap();
        assert_eq!(&first.data[..], [1]);
        assert_eq!(&second.data[..], [2]);
        // 100 frames at 48 kHz = 2083 µs; 380 frames = 7916 µs.
        assert_eq!(first.timestamp_usec, 1_000_000 - 2083);
        assert_eq!(second.timestamp_usec, 1_000_000 + 7916);
    }

    #[tokio::test]
    async fn serve_rejects_non_concrete_sample_formats() {
        for (server_format, stream_format) in [("0:16:2", None), ("48000:16:2", Some("48000:*:2"))]
        {
            let (mut server, _events) = SnapServer::new(ServerConfig {
                sample_format: server_format.into(),
                ..Default::default()
            });
            server.add_stream_with_config(
                "s",
                StreamConfig {
                    sample_format: stream_format.map(Into::into),
                    ..Default::default()
                },
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let err = server.serve(listener).await.unwrap_err().to_string();
            assert!(err.contains("invalid sample_format"), "{err}");
        }
        let (mut server, _events) = SnapServer::new(ServerConfig {
            sample_format: "48000:16:0".into(),
            ..Default::default()
        });
        assert!(server.add_f32_stream("f").is_err());
    }

    #[tokio::test]
    async fn encoder_pauses_while_no_client_is_connected() {
        let encodes = Arc::new(AtomicUsize::new(0));
        let resets = Arc::new(AtomicUsize::new(0));
        let enc = Box::new(CountingEncoder {
            encodes: Arc::clone(&encodes),
            resets: Arc::clone(&resets),
        });
        let (tx, rx) = mpsc::channel(AUDIO_CHANNEL_SIZE);
        let (chunk_tx, _) = broadcast::channel(16);
        spawn_stream_encoder("s".into(), rx, enc, chunk_tx.clone(), 48000, 2);

        for ts in 1..=3 {
            tx.send(pcm_frame(ts)).await.unwrap();
        }
        drain(&tx).await;
        assert_eq!(
            encodes.load(Ordering::SeqCst),
            0,
            "no client: nothing encoded"
        );

        let mut chunks = chunk_tx.subscribe();
        tx.send(pcm_frame(4)).await.unwrap();
        let chunk = tokio::time::timeout(std::time::Duration::from_secs(2), chunks.recv())
            .await
            .expect("chunk after a client connected")
            .unwrap();
        assert_eq!(chunk.timestamp_usec, 4);
        assert_eq!(encodes.load(Ordering::SeqCst), 1);
        assert_eq!(resets.load(Ordering::SeqCst), 1, "reset once on resume");

        tx.send(pcm_frame(5)).await.unwrap();
        chunks.recv().await.unwrap();
        assert_eq!(
            resets.load(Ordering::SeqCst),
            1,
            "no reset while clients stay"
        );
    }
}
