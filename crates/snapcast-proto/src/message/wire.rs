//! Wire format helpers for length-prefixed strings and byte arrays.

use std::io::{Read, Write};

use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::DEFAULT_MAX_PAYLOAD_SIZE;
use crate::message::base::ProtoError;

/// Validate an untrusted incoming length against [`DEFAULT_MAX_PAYLOAD_SIZE`].
///
/// A malicious or corrupt frame can claim a multi-gigabyte length; allocating
/// or buffering that much on the strength of an untrusted value is an
/// unbounded-allocation DoS. Reject oversized lengths up front instead.
pub(crate) fn check_incoming_len(len: u32) -> Result<usize, ProtoError> {
    if len > DEFAULT_MAX_PAYLOAD_SIZE {
        return Err(ProtoError::PayloadTooLarge {
            len: len as usize,
            max: DEFAULT_MAX_PAYLOAD_SIZE as usize,
        });
    }
    Ok(len as usize)
}

/// Read a u32 LE length prefix, checked by [`check_incoming_len`].
fn read_checked_len<R: Read>(r: &mut R) -> Result<usize, ProtoError> {
    check_incoming_len(r.read_u32::<LittleEndian>()?)
}

/// Read a length-prefixed string (u32 LE length + UTF-8 bytes).
///
/// Invalid UTF-8 is replaced with U+FFFD rather than rejected.
pub fn read_string<R: Read>(r: &mut R) -> Result<String, ProtoError> {
    let buf = read_bytes(r)?;
    // Valid UTF-8 (the normal case) reuses the buffer; only a lossy fix-up copies.
    Ok(String::from_utf8(buf)
        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()))
}

/// Write a length-prefixed string (u32 LE length + UTF-8 bytes).
pub fn write_string<W: Write>(w: &mut W, s: &str) -> Result<(), ProtoError> {
    write_bytes(w, s.as_bytes())
}

/// Read a length-prefixed byte array (u32 LE length + bytes).
pub fn read_bytes<R: Read>(r: &mut R) -> Result<Vec<u8>, ProtoError> {
    let len = read_checked_len(r)?;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

/// Write a length-prefixed byte array (u32 LE length + bytes).
///
/// A field longer than `u32::MAX` bytes cannot be length-prefixed; it is
/// rejected rather than written with a truncated prefix that would desync the
/// peer's framing.
pub fn write_bytes<W: Write>(w: &mut W, data: &[u8]) -> Result<(), ProtoError> {
    w.write_u32::<LittleEndian>(checked_u32_len(data.len())?)?;
    w.write_all(data)?;
    Ok(())
}

/// Convert an outgoing length to the u32 the wire carries, or fail.
pub(crate) fn checked_u32_len(len: usize) -> Result<u32, ProtoError> {
    u32::try_from(len).map_err(|_| ProtoError::PayloadTooLarge {
        len,
        max: u32::MAX as usize,
    })
}

/// Read a length-prefixed JSON payload (u32 LE length + UTF-8 JSON).
///
/// Shared by the JSON-bodied messages (Hello, ServerSettings, ClientInfo) so
/// the read/parse path lives in one place. A serde failure surfaces as
/// [`ProtoError::Json`], not mislabeled as an I/O error.
///
/// The bytes are parsed directly (no separate UTF-8 pass or lossy copy), so
/// invalid UTF-8 is a JSON error, as it is for C++ `json::parse`.
pub fn read_json<R: Read, T: DeserializeOwned>(r: &mut R) -> Result<T, ProtoError> {
    let json = read_bytes(r)?;
    Ok(serde_json::from_slice(&json)?)
}

/// Write a value as a length-prefixed JSON payload (u32 LE length + UTF-8 JSON).
pub fn write_json<W: Write, T: Serialize>(w: &mut W, value: &T) -> Result<(), ProtoError> {
    let json = serde_json::to_string(value)?;
    write_string(w, &json)
}

/// Default `volume` of the JSON-bodied messages when the key is absent,
/// matching C++ `get("volume", static_cast<uint16_t>(100))`.
pub(crate) fn default_volume() -> u16 {
    100
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_string() {
        let mut buf = Vec::new();
        write_string(&mut buf, "flac").unwrap();
        // 4 bytes length + 4 bytes "flac"
        assert_eq!(buf, [0x04, 0x00, 0x00, 0x00, b'f', b'l', b'a', b'c']);
        let mut cursor = std::io::Cursor::new(&buf);
        let s = read_string(&mut cursor).unwrap();
        assert_eq!(s, "flac");
    }

    #[test]
    fn round_trip_bytes() {
        let data = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let mut buf = Vec::new();
        write_bytes(&mut buf, &data).unwrap();
        let mut cursor = std::io::Cursor::new(&buf);
        let decoded = read_bytes(&mut cursor).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn invalid_utf8_string_is_replaced() {
        let mut buf = Vec::new();
        write_bytes(&mut buf, &[b'a', 0xFF, b'b']).unwrap();
        let mut cursor = std::io::Cursor::new(&buf);
        assert_eq!(read_string(&mut cursor).unwrap(), "a\u{FFFD}b");
    }

    #[test]
    fn empty_string() {
        let mut buf = Vec::new();
        write_string(&mut buf, "").unwrap();
        assert_eq!(buf, [0x00, 0x00, 0x00, 0x00]);
        let mut cursor = std::io::Cursor::new(&buf);
        let s = read_string(&mut cursor).unwrap();
        assert_eq!(s, "");
    }

    #[test]
    fn oversized_length_is_rejected_without_allocating() {
        // A 4-byte prefix claiming ~4 GiB, with no payload bytes following.
        // The guard must reject it instead of attempting `vec![0u8; 0xFFFFFFFF]`.
        let prefix = [0xFF, 0xFF, 0xFF, 0xFF];

        let mut cursor = std::io::Cursor::new(&prefix);
        match read_string(&mut cursor) {
            Err(ProtoError::PayloadTooLarge { len, max }) => {
                assert_eq!(len, 0xFFFF_FFFF);
                assert_eq!(max, DEFAULT_MAX_PAYLOAD_SIZE as usize);
            }
            other => panic!("expected PayloadTooLarge, got {other:?}"),
        }

        let mut cursor = std::io::Cursor::new(&prefix);
        assert!(matches!(
            read_bytes(&mut cursor),
            Err(ProtoError::PayloadTooLarge { .. })
        ));
    }

    #[test]
    fn invalid_utf8_json_is_a_json_error() {
        // nlohmann::json::parse rejects invalid UTF-8; so must we, rather than
        // silently substituting U+FFFD into e.g. a client's host name.
        let mut buf = Vec::new();
        write_bytes(&mut buf, b"{\"k\":\"\xFF\"}").unwrap();
        let res: Result<serde_json::Value, _> = read_json(&mut std::io::Cursor::new(&buf));
        assert!(matches!(res, Err(ProtoError::Json(_))), "{res:?}");
    }

    #[test]
    fn max_allowed_length_passes_guard() {
        // Exactly the cap is accepted by the length check (then fails on the
        // truncated body via read_exact — i.e. NOT PayloadTooLarge).
        let mut prefix = Vec::new();
        prefix.extend_from_slice(&DEFAULT_MAX_PAYLOAD_SIZE.to_le_bytes());
        let mut cursor = std::io::Cursor::new(&prefix);
        assert!(matches!(read_bytes(&mut cursor), Err(ProtoError::Io(_))));
    }
}
