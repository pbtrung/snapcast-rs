//! Error message (type=8).
//!
//! Sent from server to client, e.g. for authentication failures.

use std::io::{Read, Write};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};

use crate::message::base::ProtoError;
use crate::message::wire;

/// Error message payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// Numeric error code.
    pub code: u32,
    /// Short error name.
    pub error: String,
    /// Human-readable error description.
    pub message: String,
}

impl Error {
    /// Deserialize an error message from a reader.
    pub fn read_from<R: Read>(r: &mut R) -> Result<Self, ProtoError> {
        let code = r.read_u32::<LittleEndian>()?;
        let error = wire::read_string(r)?;
        let message = wire::read_string(r)?;
        Ok(Self {
            code,
            error,
            message,
        })
    }

    /// Serialize an error message to a writer.
    pub fn write_to<W: Write>(&self, w: &mut W) -> Result<(), ProtoError> {
        w.write_u32::<LittleEndian>(self.code)?;
        wire::write_string(w, &self.error)?;
        wire::write_string(w, &self.message)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let original = Error {
            code: 401,
            error: "Unauthorized".into(),
            message: "Authentication required".into(),
        };
        let mut buf = Vec::new();
        original.write_to(&mut buf).unwrap();
        let mut cursor = std::io::Cursor::new(&buf);
        let decoded = Error::read_from(&mut cursor).unwrap();
        assert_eq!(original, decoded);
    }

    #[test]
    fn known_bytes_empty() {
        let expected: Vec<u8> = vec![
            0x01, 0x00, 0x00, 0x00, // code = 1
            0x02, 0x00, 0x00, 0x00, // error len = 2
            b'o', b'k', // "ok"
            0x00, 0x00, 0x00, 0x00, // message len = 0
        ];
        let msg = Error {
            code: 1,
            error: "ok".into(),
            message: String::new(),
        };
        let mut buf = Vec::new();
        msg.write_to(&mut buf).unwrap();
        assert_eq!(buf, expected);
    }
}
