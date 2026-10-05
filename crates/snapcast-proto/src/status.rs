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
#[serde(rename_all = "camelCase")]
pub struct StreamProperties {
    /// Playback status (Playing, Paused, Stopped).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playback_status: Option<String>,
    /// Loop status (None, Track, Playlist).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_status: Option<String>,
    /// Shuffle mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shuffle: Option<bool>,
    /// Volume (0–100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<u16>,
    /// Mute state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mute: Option<bool>,
    /// Playback rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<f64>,
    /// Position in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

impl StreamUri {
    /// Split a stream source URI (`scheme://host/path?key=value#fragment`)
    /// into its components, percent-decoding the path, query and fragment.
    ///
    /// Never fails: anything unparsable is left in `raw` with the other
    /// components empty.
    pub fn parse(raw: &str) -> Self {
        let mut uri = Self {
            raw: raw.to_string(),
            ..Default::default()
        };
        let Some((scheme, rest)) = raw.split_once("://") else {
            return uri;
        };
        let (rest, fragment) = rest.split_once('#').unwrap_or((rest, ""));
        let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
        let (host, path) = rest.find('/').map_or((rest, ""), |i| rest.split_at(i));
        uri.scheme = scheme.to_string();
        uri.host = host.to_string();
        uri.path = percent_decode(path);
        uri.fragment = percent_decode(fragment);
        uri.query = query
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| {
                let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                (percent_decode(k), percent_decode(v))
            })
            .collect();
        uri
    }
}

/// Decode `%XX` escapes in a URI component.
///
/// Escapes are decoded to bytes and the whole result is then read as UTF-8
/// once, so both literal and percent-encoded multi-byte characters survive
/// (invalid UTF-8 becomes U+FFFD). A `%` not followed by two hex digits is
/// kept verbatim. `+` is not treated as a space.
pub fn percent_decode(s: &str) -> String {
    fn hex(b: u8) -> Option<u8> {
        char::from(b).to_digit(16).map(|d| d as u8)
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(hi), Some(lo)) = (
                bytes.get(i + 1).copied().and_then(hex),
                bytes.get(i + 2).copied().and_then(hex),
            )
        {
            out.push(hi << 4 | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_uri_parse_pipe() {
        let uri = StreamUri::parse("pipe:///tmp/snap%20fifo?name=Radio&sampleformat=48000:16:2");
        assert_eq!(uri.scheme, "pipe");
        assert_eq!(uri.host, "");
        assert_eq!(uri.path, "/tmp/snap fifo");
        assert_eq!(uri.query["name"], "Radio");
        assert_eq!(uri.query["sampleformat"], "48000:16:2");
        assert_eq!(uri.fragment, "");
        assert_eq!(
            uri.raw,
            "pipe:///tmp/snap%20fifo?name=Radio&sampleformat=48000:16:2"
        );
    }

    #[test]
    fn stream_uri_parse_tcp_with_fragment() {
        let uri = StreamUri::parse("tcp://0.0.0.0:4953?name=TCP#frag");
        assert_eq!(uri.scheme, "tcp");
        assert_eq!(uri.host, "0.0.0.0:4953");
        assert_eq!(uri.path, "");
        assert_eq!(uri.query["name"], "TCP");
        assert_eq!(uri.fragment, "frag");
    }

    #[test]
    fn stream_uri_parse_invalid_keeps_raw() {
        let uri = StreamUri::parse("not a uri");
        assert_eq!(uri.raw, "not a uri");
        assert_eq!(uri.scheme, "");
        assert!(uri.query.is_empty());
    }

    #[test]
    fn percent_decode_malformed_kept() {
        assert_eq!(percent_decode("a%2"), "a%2");
        assert_eq!(percent_decode("%zz%41"), "%zzA");
        assert_eq!(
            percent_decode("%+1%-1"),
            "%+1%-1",
            "signs are not hex digits"
        );
        assert_eq!(percent_decode("100%"), "100%");
    }

    #[test]
    fn percent_decode_utf8() {
        assert_eq!(
            percent_decode("/Musik/Über"),
            "/Musik/Über",
            "literal UTF-8"
        );
        assert_eq!(percent_decode("%C3%9Cber"), "Über", "encoded UTF-8");
        assert_eq!(percent_decode("caf%C3%A9%20bar"), "café bar");
        assert_eq!(percent_decode("%FF"), "\u{FFFD}", "invalid UTF-8");
    }

    #[test]
    fn stream_uri_parse_decodes_query_keys() {
        let uri = StreamUri::parse("pipe:///x?na%6De=a%26b&flag");
        assert_eq!(uri.query["name"], "a&b");
        assert_eq!(uri.query["flag"], "");
    }
}
