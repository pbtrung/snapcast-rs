#![deny(unsafe_code)]
#![warn(clippy::redundant_closure)]
#![warn(clippy::implicit_clone)]
#![warn(clippy::uninlined_format_args)]
#![warn(missing_docs)]

//! Snapcast client library — embeddable synchronized multiroom audio client.
//!
//! See also: [`snapcast-server`](https://docs.rs/snapcast-server) for the server library.
//!
//! # Example
//! ```no_run
//! use snapcast_client::{SnapClient, ClientConfig, ClientEvent, ClientCommand};
//!
//! # async fn example() -> anyhow::Result<()> {
//! let config = ClientConfig::default();
//! let (mut client, mut events, mut audio_rx) = SnapClient::new(config);
//! let cmd = client.command_sender();
//!
//! // React to events in a separate task
//! tokio::spawn(async move {
//!     while let Some(event) = events.recv().await {
//!         match event {
//!             ClientEvent::VolumeChanged { volume, muted } => {
//!                 println!("Volume: {volume}, muted: {muted}");
//!             }
//!             _ => {}
//!         }
//!     }
//! });
//!
//! // Stop on Ctrl-C
//! let stop = cmd.clone();
//! tokio::spawn(async move {
//!     tokio::signal::ctrl_c().await.ok();
//!     stop.send(ClientCommand::Stop).await.ok();
//! });
//!
//! client.run().await?;
//! # Ok(())
//! # }
//! ```

pub mod config;
pub mod connection;
pub(crate) mod controller;
pub mod decoder;
pub(crate) mod double_buffer;
pub mod stream;
pub mod time_provider;

#[cfg(feature = "resampler")]
pub mod resampler;

use tokio::sync::mpsc;

const EVENT_CHANNEL_SIZE: usize = 256;
const COMMAND_CHANNEL_SIZE: usize = 64;
const AUDIO_CHANNEL_SIZE: usize = 256;

// Re-export proto types that embedders need
pub use snapcast_proto::SampleFormat;
pub use snapcast_proto::{DEFAULT_STREAM_PORT, PROTOCOL_VERSION};

/// Interleaved f32 audio frame produced by the client library.
#[derive(Debug, Clone)]
pub struct AudioFrame {
    /// Interleaved f32 samples (channel-interleaved).
    pub samples: Vec<f32>,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Number of channels.
    pub channels: u16,
    /// Server timestamp in microseconds (for sync).
    pub timestamp_usec: i64,
}

/// Events emitted by the client to the consumer.
#[derive(Debug, Clone)]
pub enum ClientEvent {
    /// Connected to server.
    Connected {
        /// Server hostname or IP.
        host: String,
        /// Server port.
        port: u16,
    },
    /// Disconnected from server.
    Disconnected {
        /// Reason for disconnection.
        reason: String,
    },
    /// Audio stream started with the given format.
    StreamStarted {
        /// Codec name (e.g. "flac", "opus").
        codec: String,
        /// PCM sample format.
        format: SampleFormat,
    },
    /// Server settings received or updated.
    ServerSettings {
        /// Playout buffer in milliseconds.
        buffer_ms: i32,
        /// Additional latency in milliseconds.
        latency: i32,
        /// Volume (0–100).
        volume: u16,
        /// Mute state.
        muted: bool,
    },
    /// Volume changed by the server.
    VolumeChanged {
        /// Volume (0–100).
        volume: u16,
        /// Mute state.
        muted: bool,
    },
    /// Time sync completed.
    TimeSyncComplete {
        /// Clock difference to server in milliseconds.
        diff_ms: f64,
    },
}

/// Commands the consumer sends to the client.
#[derive(Debug, Clone)]
pub enum ClientCommand {
    /// Set volume (0–100) and mute state.
    SetVolume {
        /// Volume (0–100).
        volume: u16,
        /// Mute state.
        muted: bool,
    },
    /// Stop the client gracefully.
    Stop,
}

/// Configuration for the embeddable client.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Connection scheme: "tcp" or "ws" (`websocket` feature).
    pub scheme: String,
    /// Server hostname or IP. Must be set: the library does no discovery
    /// (an empty host fails every connection attempt).
    pub host: String,
    /// Server port. Default: 1704.
    pub port: u16,
    /// Optional authentication for Hello handshake.
    pub auth: Option<crate::config::Auth>,
    /// Instance id (for multiple clients on one host).
    pub instance: u32,
    /// Unique host identifier (default: MAC address).
    pub host_id: String,
    /// Additional latency in milliseconds (subtracted from buffer).
    pub latency: i32,
    /// Client name sent in Hello. Default: "Snapclient".
    pub client_name: String,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            scheme: snapcast_proto::SCHEME_TCP.into(),
            host: String::new(),
            port: snapcast_proto::DEFAULT_STREAM_PORT,
            auth: None,
            instance: 1,
            host_id: String::new(),
            latency: 0,
            client_name: snapcast_proto::DEFAULT_CLIENT_NAME.into(),
        }
    }
}

/// The embeddable Snapcast client.
pub struct SnapClient {
    config: ClientConfig,
    event_tx: mpsc::Sender<ClientEvent>,
    command_tx: mpsc::Sender<ClientCommand>,
    command_rx: Option<mpsc::Receiver<ClientCommand>>,
    audio_tx: mpsc::Sender<AudioFrame>,
    /// Shared time provider — accessible by the binary for audio output.
    pub time_provider: std::sync::Arc<std::sync::Mutex<time_provider::TimeProvider>>,
    /// Shared stream — accessible by the binary for audio output.
    pub stream: std::sync::Arc<std::sync::Mutex<stream::Stream>>,
}

impl SnapClient {
    /// Create a new client. Returns the client, event receiver, and audio output receiver.
    pub fn new(
        config: ClientConfig,
    ) -> (
        Self,
        mpsc::Receiver<ClientEvent>,
        mpsc::Receiver<AudioFrame>,
    ) {
        let (event_tx, event_rx) = mpsc::channel(EVENT_CHANNEL_SIZE);
        let (command_tx, command_rx) = mpsc::channel(COMMAND_CHANNEL_SIZE);
        let (audio_tx, audio_rx) = mpsc::channel(AUDIO_CHANNEL_SIZE);
        let time_provider =
            std::sync::Arc::new(std::sync::Mutex::new(time_provider::TimeProvider::new()));
        let stream = std::sync::Arc::new(std::sync::Mutex::new(stream::Stream::new(
            SampleFormat::default(),
        )));
        let client = Self {
            config,
            event_tx,
            command_tx,
            command_rx: Some(command_rx),
            audio_tx,
            time_provider,
            stream,
        };
        (client, event_rx, audio_rx)
    }

    /// Get a cloneable command sender.
    pub fn command_sender(&self) -> mpsc::Sender<ClientCommand> {
        self.command_tx.clone()
    }

    /// Run the client, reconnecting after connection errors, until a
    /// [`ClientCommand::Stop`] arrives. Fails early on an invalid configuration
    /// (e.g. an unsupported scheme).
    pub async fn run(&mut self) -> anyhow::Result<()> {
        let command_rx = self
            .command_rx
            .take()
            .ok_or_else(|| anyhow::anyhow!("run() already called"))?;

        let mut ctrl = controller::Controller::new(
            self.config.clone(),
            self.event_tx.clone(),
            command_rx,
            self.audio_tx.clone(),
            std::sync::Arc::clone(&self.time_provider),
            std::sync::Arc::clone(&self.stream),
        )?;
        ctrl.run().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_rejects_unsupported_scheme() {
        let config = ClientConfig {
            scheme: "wss".into(),
            host: "localhost".into(),
            port: snapcast_proto::DEFAULT_HTTP_PORT,
            ..ClientConfig::default()
        };
        let (mut client, _events, _audio_rx) = SnapClient::new(config);

        let err = client.run().await.unwrap_err();
        assert!(err.to_string().contains("unsupported scheme"));
    }
}
