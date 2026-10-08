//! Time sync message (type=4).
//!
//! Sent by the client to the server and echoed back. Used to compute
//! the clock difference between client and server.
//!
//! Payload: a single [`Timeval`]. The client sends it zeroed; the server's
//! reply carries `received - sent` of the request, i.e. the client-to-server
//! delta (one-way latency plus clock offset), not a round-trip time.

use std::io::{Read, Write};

use crate::message::base::ProtoError;
use crate::types::Timeval;

/// Time sync message payload (8 bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Time {
    /// Client-to-server delta (`received - sent` of the request) in the
    /// server's reply; zero in the client's request.
    pub latency: Timeval,
}

impl Time {
    /// Payload size in bytes.
    pub const SIZE: u32 = 8;

    /// Create a new Time message with zero latency.
    pub fn new() -> Self {
        Self {
            latency: Timeval::default(),
        }
    }

    /// Deserialize a Time message from a reader.
    pub fn read_from<R: Read>(r: &mut R) -> Result<Self, ProtoError> {
        Ok(Self {
            latency: Timeval::read_from(r)?,
        })
    }

    /// Serialize a Time message to a writer.
    pub fn write_to<W: Write>(&self, w: &mut W) -> Result<(), ProtoError> {
        self.latency.write_to(w)?;
        Ok(())
    }
}

impl Default for Time {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Payload bytes for latency = {sec: 0, usec: 1500}
    const TIME_PAYLOAD: [u8; 8] = [
        0x00, 0x00, 0x00, 0x00, // latency.sec = 0
        0xDC, 0x05, 0x00, 0x00, // latency.usec = 1500
    ];

    #[test]
    fn serialize() {
        let msg = Time {
            latency: Timeval { sec: 0, usec: 1500 },
        };
        let mut buf = Vec::new();
        msg.write_to(&mut buf).unwrap();
        assert_eq!(buf.as_slice(), &TIME_PAYLOAD);
    }

    #[test]
    fn deserialize() {
        let mut cursor = std::io::Cursor::new(&TIME_PAYLOAD);
        let msg = Time::read_from(&mut cursor).unwrap();
        assert_eq!(msg.latency.sec, 0);
        assert_eq!(msg.latency.usec, 1500);
    }

    #[test]
    fn round_trip() {
        let original = Time {
            latency: Timeval {
                sec: 42,
                usec: 123_456,
            },
        };
        let mut buf = Vec::new();
        original.write_to(&mut buf).unwrap();
        assert_eq!(buf.len(), Time::SIZE as usize);
        let mut cursor = std::io::Cursor::new(&buf);
        let decoded = Time::read_from(&mut cursor).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn default_is_zero() {
        let msg = Time::new();
        assert_eq!(msg.latency, Timeval::default());
    }
}
