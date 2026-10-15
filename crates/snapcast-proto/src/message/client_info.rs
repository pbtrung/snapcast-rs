//! Client Info message (type=7).
//!
//! Sent from client to server to report volume and mute state changes.

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

use crate::message::base::ProtoError;
use crate::message::wire;

/// Client info JSON payload.
///
/// Absent keys take the C++ defaults (`volume` 100, `muted` false), as C++
/// `ClientInfo` reads them with `get(key, def)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientInfo {
    /// Playback volume (0–100).
    #[serde(default = "wire::default_volume")]
    pub volume: u16,
    /// Whether the client is muted.
    #[serde(default)]
    pub muted: bool,
}

impl ClientInfo {
    /// Deserialize client info from a reader.
    pub fn read_from<R: Read>(r: &mut R) -> Result<Self, ProtoError> {
        wire::read_json(r)
    }

    /// Serialize client info to a writer.
    pub fn write_to<W: Write>(&self, w: &mut W) -> Result<(), ProtoError> {
        wire::write_json(w, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let original = ClientInfo {
            volume: 75,
            muted: false,
        };
        let mut buf = Vec::new();
        original.write_to(&mut buf).unwrap();
        let mut cursor = std::io::Cursor::new(&buf);
        let decoded = ClientInfo::read_from(&mut cursor).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn deserialize_cpp_json() {
        let json = r#"{"volume":100,"muted":false}"#;
        let ci: ClientInfo = serde_json::from_str(json).unwrap();
        assert_eq!(ci.volume, 100);
        assert!(!ci.muted);
    }

    #[test]
    fn missing_keys_take_cpp_defaults() {
        let ci: ClientInfo = serde_json::from_str(r#"{"muted":true}"#).unwrap();
        assert_eq!(ci.volume, 100);
        assert!(ci.muted);
        let ci: ClientInfo = serde_json::from_str("{}").unwrap();
        assert_eq!((ci.volume, ci.muted), (100, false));
    }
}
