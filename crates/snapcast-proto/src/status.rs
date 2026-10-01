//! Snapcast status types matching the JSON-RPC wire format.
//!
//! Shared between the embedded server (serialize) and the process backend
//! talking to C++ snapserver (deserialize). Also used by embedders like SnapDog.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Result of `Server.GetStatus`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerStatus {
    /// Full server state.
    pub server: Server,
}

/// Top-level server state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Server {
    /// Server host and version info.
    pub server: ServerInfo,
    /// All groups (each containing its clients).
    pub groups: Vec<Group>,
    /// All configured streams.
    pub streams: Vec<Stream>,
}

/// Server host and software information.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerInfo {
    /// Server host details.
    pub host: Host,
    /// Snapserver software info.
    pub snapserver: Snapserver,
}

/// Snapserver software information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapserver {
    /// Software name.
    pub name: String,
    /// Binary protocol version.
    #[serde(rename = "protocolVersion")]
    pub protocol_version: u32,
    /// JSON-RPC control protocol version.
    #[serde(rename = "controlProtocolVersion")]
    pub control_protocol_version: u32,
    /// Software version string.
    pub version: String,
}

impl Default for Snapserver {
    fn default() -> Self {
        Self {
            // C++ snapserver reports "Snapserver"; control clients such as
            // Snapweb rely on it.
            name: crate::DEFAULT_SERVER_NAME.into(),
            protocol_version: crate::PROTOCOL_VERSION,
            control_protocol_version: crate::CONTROL_PROTOCOL_VERSION,
            version: env!("CARGO_PKG_VERSION").into(),
        }
    }
}

// ── Host ──────────────────────────────────────────────────────

/// Host identification and platform info.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Host {
    /// CPU architecture.
    #[serde(default)]
    pub arch: String,
    /// IP address.
    #[serde(default)]
    pub ip: String,
    /// MAC address.
    #[serde(default)]
    pub mac: String,
    /// Hostname.
    #[serde(default)]
    pub name: String,
    /// Operating system.
    #[serde(default)]
    pub os: String,
}

// ── Client ────────────────────────────────────────────────────

/// A Snapcast client (speaker endpoint).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Client {
    /// Unique client ID.
    pub id: String,
    /// Whether currently connected.
    pub connected: bool,
    /// Client configuration (persisted).
    pub config: ClientConfig,
    /// Host information.
    pub host: Host,
    /// Snapclient software info.
    #[serde(default)]
    pub snapclient: Snapclient,
    /// Last-seen timestamp.
    #[serde(default, rename = "lastSeen")]
    pub last_seen: LastSeen,
}

/// Client configuration (persisted across restarts).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClientConfig {
    /// Multi-instance identifier.
    #[serde(default)]
    pub instance: u32,
    /// Additional latency in milliseconds.
    pub latency: i32,
    /// Display name.
    pub name: String,
    /// Volume settings.
    pub volume: Volume,
}

/// Volume state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Volume {
    /// Mute state.
    pub muted: bool,
    /// Volume percentage (0–100).
    pub percent: u16,
}

/// Snapclient software information.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Snapclient {
    /// Software name.
    #[serde(default)]
    pub name: String,
    /// Protocol version.
    #[serde(default, rename = "protocolVersion")]
    pub protocol_version: u32,
    /// Software version string.
    #[serde(default)]
    pub version: String,
}

/// Last-seen timestamp (seconds + microseconds since epoch).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LastSeen {
    /// Seconds since epoch.
    #[serde(default)]
    pub sec: u64,
    /// Microseconds.
    #[serde(default)]
    pub usec: u64,
}

// ── Group ─────────────────────────────────────────────────────

/// A group of clients sharing the same stream.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Group {
    /// Unique group ID.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Stream ID this group is playing.
    pub stream_id: String,
    /// Group mute state.
    pub muted: bool,
    /// Clients in this group.
    pub clients: Vec<Client>,
}

// ── Stream ────────────────────────────────────────────────────

/// An audio stream source.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Stream {
    /// Stream ID.
    pub id: String,
    /// Stream properties (MPRIS-style metadata).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<StreamProperties>,
    /// Playback status.
    pub status: StreamStatus,
    /// Source URI.
    pub uri: StreamUri,
}

/// Stream playback status.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StreamStatus {
    /// No audio data flowing.
    #[default]
    Idle,
    /// Audio data actively streaming.
    Playing,
    /// Stream disabled by configuration.
    Disabled,
    /// Status not recognized.
    Unknown,
}

impl From<&str> for StreamStatus {
    fn from(s: &str) -> Self {
        match s {
            "playing" => Self::Playing,
            "idle" => Self::Idle,
            "disabled" => Self::Disabled,
            _ => Self::Unknown,
        }
    }
}

/// Parsed stream URI components.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StreamUri {
    /// URI fragment.
    #[serde(default)]
    pub fragment: String,
    /// Host component.
    #[serde(default)]
    pub host: String,
    /// Path component.
    #[serde(default)]
    pub path: String,
    /// Query parameters.
    #[serde(default)]
    pub query: HashMap<String, String>,
    /// Raw URI string.
    pub raw: String,
    /// URI scheme (pipe, tcp, process, etc.).
    #[serde(default)]
    pub scheme: String,
}

/// Stream properties (MPRIS-style metadata and capabilities).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StreamProperties {
    /// Playback status (Playing, Paused, Stopped).
    #[serde(default)]
    pub playback_status: Option<String>,
    /// Loop status (None, Track, Playlist).
    #[serde(default)]
    pub loop_status: Option<String>,
    /// Shuffle mode.
    #[serde(default)]
    pub shuffle: Option<bool>,
    /// Volume (0–100).
    #[serde(default)]
    pub volume: Option<u16>,
    /// Mute state.
    #[serde(default)]
    pub mute: Option<bool>,
    /// Playback rate.
    #[serde(default)]
    pub rate: Option<f64>,
    /// Position in seconds.
    #[serde(default)]
    pub position: Option<f64>,
    /// Can skip to next track.
    #[serde(default)]
    pub can_go_next: bool,
    /// Can skip to previous track.
    #[serde(default)]
    pub can_go_previous: bool,
    /// Can start playback.
    #[serde(default)]
    pub can_play: bool,
    /// Can pause playback.
    #[serde(default)]
    pub can_pause: bool,
    /// Can seek within track.
    #[serde(default)]
    pub can_seek: bool,
    /// Can control playback at all.
    #[serde(default)]
    pub can_control: bool,
    /// Track metadata (artist, title, album, etc.).
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}
