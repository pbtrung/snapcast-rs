//! Server Settings message (type=3).
//!
//! Sent from server to client. Contains buffer size, latency, volume, and mute state.

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

use crate::message::base::ProtoError;
use crate::message::wire;

/// Server settings JSON payload.
///
/// Absent keys take the C++ defaults (`bufferMs` 0, `latency` 0, `volume`
/// 100, `muted` false), as C++ `ServerSettings` reads them with `get(key, def)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerSettings {
    /// Buffer size in milliseconds.
    #[serde(default)]
    pub buffer_ms: i32,
    /// Additional latency in milliseconds.
    #[serde(default)]
    pub latency: i32,
    /// Playback volume (0–100).
    #[serde(default = "wire::default_volume")]
    pub volume: u16,
    /// Whether the client is muted.
    #[serde(default)]
    pub muted: bool,
}

impl ServerSettings {
    /// Deserialize server settings from a reader.
    pub fn read_from<R: Read>(r: &mut R) -> Result<Self, ProtoError> {
        wire::read_json(r)
    }

    /// Serialize server settings to a writer.
    pub fn write_to<W: Write>(&self, w: &mut W) -> Result<(), ProtoError> {
        wire::write_json(w, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let original = ServerSettings {
            buffer_ms: 1000,
            latency: 0,
            volume: 100,
            muted: false,
        };
        let mut buf = Vec::new();
        original.write_to(&mut buf).unwrap();
        let mut cursor = std::io::Cursor::new(&buf);
        let decoded = ServerSettings::read_from(&mut cursor).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn deserialize_cpp_json() {
        let json = r#"{"bufferMs":1000,"latency":0,"muted":false,"volume":100}"#;
        let ss: ServerSettings = serde_json::from_str(json).unwrap();
        assert_eq!(ss.buffer_ms, 1000);
        assert_eq!(ss.volume, 100);
        assert!(!ss.muted);
    }

    #[test]
    fn missing_keys_take_cpp_defaults() {
        let ss: ServerSettings = serde_json::from_str(r#"{"bufferMs":800}"#).unwrap();
        assert_eq!(ss.buffer_ms, 800);
        assert_eq!(ss.latency, 0);
        assert_eq!(ss.volume, 100);
        assert!(!ss.muted);
    }

    #[test]
    fn json_field_names_match_cpp() {
        let ss = ServerSettings {
            buffer_ms: 500,
            latency: 10,
            volume: 80,
            muted: true,
        };
        let json_str = serde_json::to_string(&ss).unwrap();
        assert!(json_str.contains("\"bufferMs\""));
        assert!(json_str.contains("\"latency\""));
        assert!(json_str.contains("\"volume\""));
        assert!(json_str.contains("\"muted\""));
    }
}
