//! Wire Chunk message (type=2).
//!
//! Sent from server to client. Contains a timestamp and encoded audio data.

use std::io::{Read, Write};

use crate::message::base::ProtoError;
use crate::message::wire;
use crate::types::Timeval;

/// Wire chunk payload — a timestamped piece of encoded audio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireChunk {
    /// Server-time timestamp when this audio was captured.
    pub timestamp: Timeval,
    /// Encoded audio payload.
    pub payload: Vec<u8>,
}

impl WireChunk {
    /// Deserialize a wire chunk from a reader.
    pub fn read_from<R: Read>(r: &mut R) -> Result<Self, ProtoError> {
        let timestamp = Timeval::read_from(r)?;
        let payload = wire::read_bytes(r)?;
        Ok(Self { timestamp, payload })
    }

    /// Serialize a wire chunk to a writer.
    pub fn write_to<W: Write>(&self, w: &mut W) -> Result<(), ProtoError> {
        self.timestamp.write_to(w)?;
        wire::write_bytes(w, &self.payload)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let original = WireChunk {
            timestamp: Timeval {
                sec: 1000,
                usec: 500_000,
            },
            payload: vec![0xAA, 0xBB, 0xCC, 0xDD],
        };
        let mut buf = Vec::new();
        original.write_to(&mut buf).unwrap();
        let mut cursor = std::io::Cursor::new(&buf);
        let decoded = WireChunk::read_from(&mut cursor).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn known_bytes() {
        let expected: Vec<u8> = vec![
            0xE8, 0x03, 0x00, 0x00, // timestamp.sec = 1000
            0x00, 0x00, 0x00, 0x00, // timestamp.usec = 0
            0x02, 0x00, 0x00, 0x00, // payload len = 2
            0xAB, 0xCD, // payload
        ];
        let msg = WireChunk {
            timestamp: Timeval { sec: 1000, usec: 0 },
            payload: vec![0xAB, 0xCD],
        };
        let mut buf = Vec::new();
        msg.write_to(&mut buf).unwrap();
        assert_eq!(buf, expected);
    }
}
