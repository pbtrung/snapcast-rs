//! Message factory — deserialize any typed message from a BaseMessage header + payload bytes.

use std::io::Cursor;

use crate::message::MessageType;
use crate::message::base::{BaseMessage, ProtoError};
use crate::message::client_info::ClientInfo;
use crate::message::codec_header::CodecHeader;
use crate::message::error::Error;
use crate::message::hello::Hello;
use crate::message::server_settings::ServerSettings;
use crate::message::time::Time;
use crate::message::wire_chunk::WireChunk;

/// A deserialized typed message with its base header.
#[derive(Debug)]
pub struct TypedMessage {
    /// The base message header.
    pub base: BaseMessage,
    /// The deserialized typed payload.
    pub payload: MessagePayload,
}

/// The typed payload of a protocol message.
#[derive(Debug)]
pub enum MessagePayload {
    /// Time sync payload.
    Time(Time),
    /// Client hello payload.
    Hello(Hello),
    /// Server settings payload.
    ServerSettings(ServerSettings),
    /// Codec header payload.
    CodecHeader(CodecHeader),
    /// Encoded audio chunk payload.
    WireChunk(WireChunk),
    /// Client info payload.
    ClientInfo(ClientInfo),
    /// Error payload.
    Error(Error),
    /// Stream tags (type 6, deprecated). Raw bytes.
    StreamTags(Vec<u8>),
}

/// Deserialize a typed message from a base header and raw payload bytes.
pub fn deserialize(base: BaseMessage, payload: &[u8]) -> Result<TypedMessage, ProtoError> {
    let mut cursor = Cursor::new(payload);
    let msg = match base.msg_type {
        MessageType::Time => MessagePayload::Time(Time::read_from(&mut cursor)?),
        MessageType::Hello => MessagePayload::Hello(Hello::read_from(&mut cursor)?),
        MessageType::ServerSettings => {
            MessagePayload::ServerSettings(ServerSettings::read_from(&mut cursor)?)
        }
        MessageType::CodecHeader => {
            MessagePayload::CodecHeader(CodecHeader::read_from(&mut cursor)?)
        }
        MessageType::WireChunk => MessagePayload::WireChunk(WireChunk::read_from(&mut cursor)?),
        MessageType::ClientInfo => MessagePayload::ClientInfo(ClientInfo::read_from(&mut cursor)?),
        MessageType::Error => MessagePayload::Error(Error::read_from(&mut cursor)?),
        MessageType::StreamTags => MessagePayload::StreamTags(payload.to_vec()),
        MessageType::Base | MessageType::Unknown(_) => MessagePayload::StreamTags(payload.to_vec()),
    };
    Ok(TypedMessage { base, payload: msg })
}

/// Serialize a typed message into a complete wire frame (BaseMessage header + payload).
///
/// The `base` header's `size` field will be set to the payload size.
pub fn serialize(base: &mut BaseMessage, payload: &MessagePayload) -> Result<Vec<u8>, ProtoError> {
    let mut payload_buf = Vec::new();
    match payload {
        MessagePayload::Time(m) => m.write_to(&mut payload_buf)?,
        MessagePayload::Hello(m) => m.write_to(&mut payload_buf)?,
        MessagePayload::ServerSettings(m) => m.write_to(&mut payload_buf)?,
        MessagePayload::CodecHeader(m) => m.write_to(&mut payload_buf)?,
        MessagePayload::WireChunk(m) => m.write_to(&mut payload_buf)?,
        MessagePayload::ClientInfo(m) => m.write_to(&mut payload_buf)?,
        MessagePayload::Error(m) => m.write_to(&mut payload_buf)?,
        MessagePayload::StreamTags(data) => payload_buf.extend_from_slice(data),
    }
    base.size = payload_buf.len() as u32;

    let mut frame = Vec::with_capacity(BaseMessage::HEADER_SIZE + payload_buf.len());
    base.write_to(&mut frame)?;
    frame.extend_from_slice(&payload_buf);
    Ok(frame)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Timeval;

    fn make_base(msg_type: MessageType, size: u32) -> BaseMessage {
        BaseMessage {
            msg_type,
            id: 0,
            refers_to: 0,
            sent: Timeval::default(),
            received: Timeval::default(),
            size,
        }
    }

    #[test]
    fn deserialize_time() {
        let payload = [0x00, 0x00, 0x00, 0x00, 0xDC, 0x05, 0x00, 0x00];
        let base = make_base(MessageType::Time, payload.len() as u32);
        let msg = deserialize(base, &payload).unwrap();
        match msg.payload {
            MessagePayload::Time(t) => assert_eq!(t.latency.usec, 1500),
            _ => panic!("expected Time"),
        }
    }

    #[test]
    fn deserialize_server_settings() {
        let json = r#"{"bufferMs":1000,"latency":0,"muted":false,"volume":100}"#;
        let mut payload = Vec::new();
        crate::message::wire::write_string(&mut payload, json).unwrap();
        let base = make_base(MessageType::ServerSettings, payload.len() as u32);
        let msg = deserialize(base, &payload).unwrap();
        match msg.payload {
            MessagePayload::ServerSettings(ss) => {
                assert_eq!(ss.buffer_ms, 1000);
                assert_eq!(ss.volume, 100);
            }
            _ => panic!("expected ServerSettings"),
        }
    }

    #[test]
    fn deserialize_codec_header() {
        let mut payload = Vec::new();
        let ch = CodecHeader {
            codec: "flac".into(),
            payload: vec![0x66, 0x4C, 0x61, 0x43],
        };
        ch.write_to(&mut payload).unwrap();
        let base = make_base(MessageType::CodecHeader, payload.len() as u32);
        let msg = deserialize(base, &payload).unwrap();
        match msg.payload {
            MessagePayload::CodecHeader(c) => {
                assert_eq!(c.codec, "flac");
                assert_eq!(c.payload, vec![0x66, 0x4C, 0x61, 0x43]);
            }
            _ => panic!("expected CodecHeader"),
        }
    }

    #[test]
    fn deserialize_wire_chunk() {
        let mut payload = Vec::new();
        let wc = WireChunk {
            timestamp: Timeval { sec: 1, usec: 0 },
            payload: vec![0xAA, 0xBB],
        };
        wc.write_to(&mut payload).unwrap();
        let base = make_base(MessageType::WireChunk, payload.len() as u32);
        let msg = deserialize(base, &payload).unwrap();
        match msg.payload {
            MessagePayload::WireChunk(w) => {
                assert_eq!(w.timestamp.sec, 1);
                assert_eq!(w.payload, vec![0xAA, 0xBB]);
            }
            _ => panic!("expected WireChunk"),
        }
    }

    #[test]
    fn deserialize_error() {
        let mut payload = Vec::new();
        let err = Error {
            code: 401,
            error: "Unauthorized".into(),
            message: "bad creds".into(),
        };
        err.write_to(&mut payload).unwrap();
        let base = make_base(MessageType::Error, payload.len() as u32);
        let msg = deserialize(base, &payload).unwrap();
        match msg.payload {
            MessagePayload::Error(e) => {
                assert_eq!(e.code, 401);
                assert_eq!(e.error, "Unauthorized");
            }
            _ => panic!("expected Error"),
        }
    }

    #[test]
    fn deserialize_base_type_returns_raw() {
        let base = make_base(MessageType::Base, 0);
        let msg = deserialize(base, &[]).unwrap();
        assert!(matches!(msg.payload, MessagePayload::StreamTags(_)));
    }

    #[test]
    fn full_frame_round_trip_time() {
        let payload = MessagePayload::Time(Time {
            latency: Timeval {
                sec: 5,
                usec: 999_000,
            },
        });
        let mut base = make_base(MessageType::Time, 0);
        let frame = serialize(&mut base, &payload).unwrap();

        // Deserialize: read header, then payload
        let mut cursor = std::io::Cursor::new(&frame);
        let decoded_base = BaseMessage::read_from(&mut cursor).unwrap();
        assert_eq!(decoded_base.size, Time::SIZE);
        let payload_bytes = &frame[BaseMessage::HEADER_SIZE..];
        let msg = deserialize(decoded_base, payload_bytes).unwrap();
        match msg.payload {
            MessagePayload::Time(t) => {
                assert_eq!(t.latency.sec, 5);
                assert_eq!(t.latency.usec, 999_000);
            }
            _ => panic!("expected Time"),
        }
    }

    #[test]
    fn full_frame_round_trip_error() {
        let payload = MessagePayload::Error(Error {
            code: 403,
            error: "Forbidden".into(),
            message: "not allowed".into(),
        });
        let mut base = make_base(MessageType::Error, 0);
        let frame = serialize(&mut base, &payload).unwrap();

        let mut cursor = std::io::Cursor::new(&frame);
        let decoded_base = BaseMessage::read_from(&mut cursor).unwrap();
        let payload_bytes = &frame[BaseMessage::HEADER_SIZE..];
        let msg = deserialize(decoded_base, payload_bytes).unwrap();
        match msg.payload {
            MessagePayload::Error(e) => {
                assert_eq!(e.code, 403);
                assert_eq!(e.error, "Forbidden");
                assert_eq!(e.message, "not allowed");
            }
            _ => panic!("expected Error"),
        }
    }

    #[test]
    fn serialize_sets_correct_size() {
        let payload = MessagePayload::ClientInfo(ClientInfo {
            volume: 50,
            muted: true,
        });
        let mut base = make_base(MessageType::ClientInfo, 0);
        assert_eq!(base.size, 0);
        let frame = serialize(&mut base, &payload).unwrap();
        // size should now be set
        assert!(base.size > 0);
        assert_eq!(frame.len(), BaseMessage::HEADER_SIZE + base.size as usize);
    }
}
