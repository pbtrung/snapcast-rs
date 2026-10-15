//! Hello message (type=5).
//!
//! Sent by the client when connecting to the server. Contains client
//! identification and optional authentication info as a JSON payload.

use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

use crate::message::base::ProtoError;
use crate::message::wire;

/// Authentication info (optional).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Auth {
    /// Authentication scheme (e.g. "Basic").
    pub scheme: String,
    /// Scheme-specific parameter (e.g. base64 credentials).
    pub param: String,
}

/// Hello message JSON payload.
///
/// Like C++ `Hello`, an absent `Instance` or `SnapStreamProtocolVersion`
/// reads as 1 and an absent `ID` as the MAC address, so Hellos from older
/// clients that predate those keys are still accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", from = "HelloJson")]
pub struct Hello {
    /// Client MAC address.
    #[serde(rename = "MAC")]
    pub mac: String,
    /// Client hostname.
    pub host_name: String,
    /// Client software version.
    pub version: String,
    /// Client application name.
    pub client_name: String,
    /// Client operating system.
    #[serde(rename = "OS")]
    pub os: String,
    /// Client CPU architecture.
    pub arch: String,
    /// Client instance number.
    pub instance: u32,
    /// Unique client identifier.
    #[serde(rename = "ID")]
    pub id: String,
    /// Protocol version supported by the client.
    pub snap_stream_protocol_version: u32,
    /// Optional authentication info.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<Auth>,
}

/// Deserialization shape of [`Hello`]: the optional keys, before the C++
/// defaults are applied.
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct HelloJson {
    #[serde(rename = "MAC")]
    mac: String,
    host_name: String,
    version: String,
    client_name: String,
    #[serde(rename = "OS")]
    os: String,
    arch: String,
    #[serde(default = "default_one")]
    instance: u32,
    #[serde(rename = "ID")]
    id: Option<String>,
    #[serde(default = "default_one")]
    snap_stream_protocol_version: u32,
    #[serde(default)]
    auth: Option<Auth>,
}

fn default_one() -> u32 {
    1
}

impl From<HelloJson> for Hello {
    fn from(h: HelloJson) -> Self {
        Self {
            id: h.id.unwrap_or_else(|| h.mac.clone()),
            mac: h.mac,
            host_name: h.host_name,
            version: h.version,
            client_name: h.client_name,
            os: h.os,
            arch: h.arch,
            instance: h.instance,
            snap_stream_protocol_version: h.snap_stream_protocol_version,
            auth: h.auth,
        }
    }
}

impl Hello {
    /// Deserialize a Hello message from a reader.
    pub fn read_from<R: Read>(r: &mut R) -> Result<Self, ProtoError> {
        wire::read_json(r)
    }

    /// Serialize a Hello message to a writer.
    pub fn write_to<W: Write>(&self, w: &mut W) -> Result<(), ProtoError> {
        wire::write_json(w, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_hello() -> Hello {
        Hello {
            mac: "00:11:22:33:44:55".into(),
            host_name: "testhost".into(),
            version: "0.32.0".into(),
            client_name: "Snapclient".into(),
            os: "Linux".into(),
            arch: "x86_64".into(),
            instance: 1,
            id: "00:11:22:33:44:55".into(),
            snap_stream_protocol_version: 2,
            auth: None,
        }
    }

    #[test]
    fn round_trip() {
        let original = sample_hello();
        let mut buf = Vec::new();
        original.write_to(&mut buf).unwrap();
        let mut cursor = std::io::Cursor::new(&buf);
        let decoded = Hello::read_from(&mut cursor).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn round_trip_with_auth() {
        let mut hello = sample_hello();
        hello.auth = Some(Auth {
            scheme: "Basic".into(),
            param: "dXNlcjpwYXNz".into(),
        });
        let mut buf = Vec::new();
        hello.write_to(&mut buf).unwrap();
        let mut cursor = std::io::Cursor::new(&buf);
        let decoded = Hello::read_from(&mut cursor).unwrap();
        assert_eq!(hello, decoded);
        assert_eq!(decoded.auth.unwrap().scheme, "Basic");
    }

    #[test]
    fn json_field_names_match_cpp() {
        let hello = sample_hello();
        let json_str = serde_json::to_string(&hello).unwrap();
        // Verify C++ field names are used
        assert!(json_str.contains("\"MAC\""));
        assert!(json_str.contains("\"HostName\""));
        assert!(json_str.contains("\"SnapStreamProtocolVersion\""));
        assert!(json_str.contains("\"ID\""));
        assert!(json_str.contains("\"OS\""));
        // Auth should be absent when None
        assert!(!json_str.contains("\"Auth\""));
    }

    #[test]
    fn deserialize_cpp_json() {
        // JSON as the C++ server would produce
        let json = r#"{"Arch":"x86_64","ClientName":"Snapclient","HostName":"myhost","ID":"aa:bb:cc:dd:ee:ff","Instance":1,"MAC":"aa:bb:cc:dd:ee:ff","OS":"Arch Linux","SnapStreamProtocolVersion":2,"Version":"0.32.0"}"#;
        let hello: Hello = serde_json::from_str(json).unwrap();
        assert_eq!(hello.mac, "aa:bb:cc:dd:ee:ff");
        assert_eq!(hello.snap_stream_protocol_version, 2);
        assert!(hello.auth.is_none());
    }

    #[test]
    fn missing_optional_keys_take_cpp_defaults() {
        // A Hello from a client predating the ID / Instance / protocol keys.
        let json = r#"{"Arch":"armv7l","ClientName":"Snapclient","HostName":"pi","MAC":"aa:bb:cc:dd:ee:ff","OS":"Raspbian","Version":"0.15.0"}"#;
        let hello: Hello = serde_json::from_str(json).unwrap();
        assert_eq!(hello.id, "aa:bb:cc:dd:ee:ff");
        assert_eq!(hello.instance, 1);
        assert_eq!(hello.snap_stream_protocol_version, 1);
        assert!(hello.auth.is_none());

        // An explicit ID, even an empty one, is kept as sent.
        let json =
            r#"{"Arch":"","ClientName":"","HostName":"","ID":"","MAC":"m","OS":"","Version":""}"#;
        let hello: Hello = serde_json::from_str(json).unwrap();
        assert_eq!(hello.id, "");
    }

    #[test]
    fn malformed_json_is_a_json_error_not_io() {
        // A length-prefixed but non-JSON payload must surface as ProtoError::Json,
        // not be mislabeled as an I/O error.
        let mut buf = Vec::new();
        wire::write_string(&mut buf, "{ this is not valid json").unwrap();
        let mut cursor = std::io::Cursor::new(&buf);
        match Hello::read_from(&mut cursor) {
            Err(ProtoError::Json(_)) => {}
            other => panic!("expected ProtoError::Json, got {other:?}"),
        }
    }
}
