//! Codec Header message (type=1).
//!
//! Sent from server to client after the Hello handshake. Contains the
//! codec name and codec-specific header data needed to initialize the decoder.

use std::io::{Read, Write};

use crate::message::base::ProtoError;
use crate::message::wire;

/// Codec header payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodecHeader {
    /// Codec name: "pcm", "flac", "opus", or "null".
    pub codec: String,
    /// Codec-specific header bytes (e.g. FLAC stream header, RIFF header).
    pub payload: Vec<u8>,
}

impl CodecHeader {
    /// Wire size: u32+codec + u32+payload.
    pub fn wire_size(&self) -> u32 {
        wire::string_wire_size(&self.codec) + wire::bytes_wire_size(&self.payload)
    }

    /// Deserialize a codec header from a reader.
    pub fn read_from<R: Read>(r: &mut R) -> Result<Self, ProtoError> {
        let codec = wire::read_string(r)?;
        let payload = wire::read_bytes(r)?;
        Ok(Self { codec, payload })
    }

    /// Serialize a codec header to a writer.
    pub fn write_to<W: Write>(&self, w: &mut W) -> Result<(), ProtoError> {
        wire::write_string(w, &self.codec)?;
        wire::write_bytes(w, &self.payload)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_flac() {
        let original = CodecHeader {
            codec: "flac".into(),
            payload: vec![0x66, 0x4C, 0x61, 0x43], // "fLaC" magic
        };
        let mut buf = Vec::new();
        original.write_to(&mut buf).unwrap();
        let mut cursor = std::io::Cursor::new(&buf);
        let decoded = CodecHeader::read_from(&mut cursor).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn known_bytes_pcm() {
        // codec="pcm" (3 chars), payload=empty
        let expected: Vec<u8> = vec![
            0x03, 0x00, 0x00, 0x00, // codec len = 3
            b'p', b'c', b'm', // "pcm"
            0x00, 0x00, 0x00, 0x00, // payload len = 0
        ];
        let msg = CodecHeader {
            codec: "pcm".into(),
            payload: vec![],
        };
        let mut buf = Vec::new();
        msg.write_to(&mut buf).unwrap();
        assert_eq!(buf, expected);
    }

    #[test]
    fn wire_size() {
        let msg = CodecHeader {
            codec: "opus".into(),
            payload: vec![0; 12],
        };
        // (4+4) + (4+12) = 24
        assert_eq!(msg.wire_size(), 24);
    }
}
