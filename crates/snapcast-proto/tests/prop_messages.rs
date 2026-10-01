//! Wave-3 property/fuzz tests: per-message round-trip identity.
//!
//! For every [`MessagePayload`] variant reachable with the crate's **default**
//! features (Hello, ServerSettings, ClientInfo, CodecHeader, WireChunk, Time,
//! Error, and the raw StreamTags variant), we generate arbitrary *valid*
//! instances — random strings including unicode and empty, full-range numeric
//! fields, and arbitrary byte payloads — then serialize and deserialize and
//! assert the decoded value equals the input.
//!
//! Two round-trip paths are exercised for each typed message:
//!
//! 1. **Direct**: `msg.write_to(&mut buf)` → `T::read_from(&mut cursor)`,
//!    asserting equality. Every message type here derives `PartialEq`, so this
//!    is a straight `prop_assert_eq!`.
//! 2. **Full frame via the factory**: [`factory::serialize`] produces a complete
//!    `BaseMessage` header + payload frame; we then split off the 26-byte header,
//!    re-read it, and hand the payload bytes to [`factory::deserialize`]. The
//!    returned [`MessagePayload`] does not itself derive `PartialEq`, so we match
//!    the variant and compare the inner (PartialEq) value.
//!
//! Finally, two no-panic fuzz properties feed arbitrary bytes into the two
//! untrusted parsing entry points (`BaseMessage::read_from` and
//! `factory::deserialize`) and require they return `Ok`/`Err` but never panic
//! or overflow.

use std::io::Cursor;

use proptest::prelude::*;

use snapcast_proto::MessageType;
use snapcast_proto::message::base::BaseMessage;
use snapcast_proto::message::client_info::ClientInfo;
use snapcast_proto::message::codec_header::CodecHeader;
use snapcast_proto::message::error::Error;
use snapcast_proto::message::factory::{self, MessagePayload};
use snapcast_proto::message::hello::{Auth, Hello};
use snapcast_proto::message::server_settings::ServerSettings;
use snapcast_proto::message::time::Time;
use snapcast_proto::message::wire_chunk::WireChunk;
use snapcast_proto::types::Timeval;

// Cap generated payload sizes so cases stay fast and never approach the 2 MiB
// protocol limit or truly OOM. A few KB is plenty to exercise length-prefix and
// copy logic.
const MAX_PAYLOAD: usize = 4096;

// ---------------------------------------------------------------------------
// Strategies for arbitrary *valid* instances.
// ---------------------------------------------------------------------------

/// Arbitrary UTF-8 string: unicode, control chars, and empty are all in-domain.
/// `\PC*` matches any sequence of Unicode scalar values (proptest's default
/// `.*` excludes some code points); we deliberately bound the length.
fn arb_string() -> impl Strategy<Value = String> {
    // Use the full `.*` regex plus explicit interesting cases (empty, unicode,
    // JSON-hostile characters like quotes/backslashes/newlines) mixed in.
    prop_oneof![
        9 => any::<String>(),
        1 => Just(String::new()),
        1 => Just("héllo \"wörld\"\n\t\\ 🎵 \u{0}".to_string()),
    ]
}

fn arb_bytes() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..=MAX_PAYLOAD)
}

fn arb_timeval() -> impl Strategy<Value = Timeval> {
    (any::<i32>(), any::<i32>()).prop_map(|(sec, usec)| Timeval { sec, usec })
}

fn arb_time() -> impl Strategy<Value = Time> {
    arb_timeval().prop_map(|latency| Time { latency })
}

fn arb_auth() -> impl Strategy<Value = Auth> {
    (arb_string(), arb_string()).prop_map(|(scheme, param)| Auth { scheme, param })
}

fn arb_hello() -> impl Strategy<Value = Hello> {
    (
        arb_string(), // mac
        arb_string(), // host_name
        arb_string(), // version
        arb_string(), // client_name
        arb_string(), // os
        arb_string(), // arch
        any::<u32>(), // instance
        arb_string(), // id
        any::<u32>(), // snap_stream_protocol_version
        proptest::option::of(arb_auth()),
    )
        .prop_map(
            |(mac, host_name, version, client_name, os, arch, instance, id, spv, auth)| Hello {
                mac,
                host_name,
                version,
                client_name,
                os,
                arch,
                instance,
                id,
                snap_stream_protocol_version: spv,
                auth,
            },
        )
}

fn arb_server_settings() -> impl Strategy<Value = ServerSettings> {
    (any::<i32>(), any::<i32>(), any::<u16>(), any::<bool>()).prop_map(
        |(buffer_ms, latency, volume, muted)| ServerSettings {
            buffer_ms,
            latency,
            volume,
            muted,
        },
    )
}

fn arb_client_info() -> impl Strategy<Value = ClientInfo> {
    (any::<u16>(), any::<bool>()).prop_map(|(volume, muted)| ClientInfo { volume, muted })
}

fn arb_codec_header() -> impl Strategy<Value = CodecHeader> {
    (arb_string(), arb_bytes()).prop_map(|(codec, payload)| CodecHeader { codec, payload })
}

fn arb_wire_chunk() -> impl Strategy<Value = WireChunk> {
    (arb_timeval(), arb_bytes()).prop_map(|(timestamp, payload)| WireChunk { timestamp, payload })
}

fn arb_error() -> impl Strategy<Value = Error> {
    (any::<u32>(), arb_string(), arb_string()).prop_map(|(code, error, message)| Error {
        code,
        error,
        message,
    })
}

// ---------------------------------------------------------------------------
// Full-frame round-trip helper via the factory.
//
// Serializes `payload` into a complete wire frame, re-reads the base header,
// then deserializes the typed payload from the remaining bytes. The returned
// `MessagePayload` is matched by the caller against the expected inner value.
// ---------------------------------------------------------------------------

fn frame_round_trip(msg_type: MessageType, payload: MessagePayload) -> MessagePayload {
    let mut base = BaseMessage {
        msg_type,
        id: 0,
        refers_to: 0,
        sent: Timeval::default(),
        received: Timeval::default(),
        size: 0,
    };
    let frame = factory::serialize(&mut base, &payload).expect("serialize frame");

    // The serialized `base.size` must exactly describe the payload bytes.
    assert_eq!(frame.len(), BaseMessage::HEADER_SIZE + base.size as usize);

    let mut cursor = Cursor::new(&frame);
    let header = BaseMessage::read_from(&mut cursor).expect("read header");
    assert_eq!(header.msg_type, msg_type);

    let payload_bytes = &frame[BaseMessage::HEADER_SIZE..];
    let typed = factory::deserialize(header, payload_bytes).expect("deserialize payload");
    typed.payload
}

proptest! {
    // ---- Direct write_to / read_from round-trips (PartialEq) --------------

    #[test]
    fn prop_time_round_trip(msg in arb_time()) {
        let mut buf = Vec::new();
        msg.write_to(&mut buf).expect("serialize");
        let decoded = Time::read_from(&mut Cursor::new(&buf)).expect("deserialize");
        prop_assert_eq!(msg, decoded);
    }

    #[test]
    fn prop_hello_round_trip(msg in arb_hello()) {
        let mut buf = Vec::new();
        msg.write_to(&mut buf).expect("serialize");
        let decoded = Hello::read_from(&mut Cursor::new(&buf)).expect("deserialize");
        prop_assert_eq!(msg, decoded);
    }

    #[test]
    fn prop_server_settings_round_trip(msg in arb_server_settings()) {
        let mut buf = Vec::new();
        msg.write_to(&mut buf).expect("serialize");
        let decoded = ServerSettings::read_from(&mut Cursor::new(&buf)).expect("deserialize");
        prop_assert_eq!(msg, decoded);
    }

    #[test]
    fn prop_client_info_round_trip(msg in arb_client_info()) {
        let mut buf = Vec::new();
        msg.write_to(&mut buf).expect("serialize");
        let decoded = ClientInfo::read_from(&mut Cursor::new(&buf)).expect("deserialize");
        prop_assert_eq!(msg, decoded);
    }

    #[test]
    fn prop_codec_header_round_trip(msg in arb_codec_header()) {
        let mut buf = Vec::new();
        msg.write_to(&mut buf).expect("serialize");
        let decoded = CodecHeader::read_from(&mut Cursor::new(&buf)).expect("deserialize");
        prop_assert_eq!(msg, decoded);
    }

    #[test]
    fn prop_wire_chunk_round_trip(msg in arb_wire_chunk()) {
        let mut buf = Vec::new();
        msg.write_to(&mut buf).expect("serialize");
        let decoded = WireChunk::read_from(&mut Cursor::new(&buf)).expect("deserialize");
        prop_assert_eq!(msg, decoded);
    }

    #[test]
    fn prop_error_round_trip(msg in arb_error()) {
        let mut buf = Vec::new();
        msg.write_to(&mut buf).expect("serialize");
        let decoded = Error::read_from(&mut Cursor::new(&buf)).expect("deserialize");
        prop_assert_eq!(msg, decoded);
    }

    // ---- Full-frame round-trips through the factory ----------------------

    #[test]
    fn prop_frame_time(msg in arb_time()) {
        let out = frame_round_trip(MessageType::Time, MessagePayload::Time(msg.clone()));
        match out {
            MessagePayload::Time(t) => prop_assert_eq!(t, msg),
            other => prop_assert!(false, "expected Time, got {:?}", other),
        }
    }

    #[test]
    fn prop_frame_hello(msg in arb_hello()) {
        let out = frame_round_trip(MessageType::Hello, MessagePayload::Hello(msg.clone()));
        match out {
            MessagePayload::Hello(h) => prop_assert_eq!(h, msg),
            other => prop_assert!(false, "expected Hello, got {:?}", other),
        }
    }

    #[test]
    fn prop_frame_server_settings(msg in arb_server_settings()) {
        let out = frame_round_trip(
            MessageType::ServerSettings,
            MessagePayload::ServerSettings(msg.clone()),
        );
        match out {
            MessagePayload::ServerSettings(s) => prop_assert_eq!(s, msg),
            other => prop_assert!(false, "expected ServerSettings, got {:?}", other),
        }
    }

    #[test]
    fn prop_frame_client_info(msg in arb_client_info()) {
        let out = frame_round_trip(
            MessageType::ClientInfo,
            MessagePayload::ClientInfo(msg.clone()),
        );
        match out {
            MessagePayload::ClientInfo(c) => prop_assert_eq!(c, msg),
            other => prop_assert!(false, "expected ClientInfo, got {:?}", other),
        }
    }

    #[test]
    fn prop_frame_codec_header(msg in arb_codec_header()) {
        let out = frame_round_trip(
            MessageType::CodecHeader,
            MessagePayload::CodecHeader(msg.clone()),
        );
        match out {
            MessagePayload::CodecHeader(c) => prop_assert_eq!(c, msg),
            other => prop_assert!(false, "expected CodecHeader, got {:?}", other),
        }
    }

    #[test]
    fn prop_frame_wire_chunk(msg in arb_wire_chunk()) {
        let out = frame_round_trip(
            MessageType::WireChunk,
            MessagePayload::WireChunk(msg.clone()),
        );
        match out {
            MessagePayload::WireChunk(w) => prop_assert_eq!(w, msg),
            other => prop_assert!(false, "expected WireChunk, got {:?}", other),
        }
    }

    #[test]
    fn prop_frame_error(msg in arb_error()) {
        let out = frame_round_trip(MessageType::Error, MessagePayload::Error(msg.clone()));
        match out {
            MessagePayload::Error(e) => prop_assert_eq!(e, msg),
            other => prop_assert!(false, "expected Error, got {:?}", other),
        }
    }

    // StreamTags carries raw bytes; the factory copies them verbatim on both
    // serialize and deserialize, so round-trip identity is on the raw Vec.
    #[test]
    fn prop_frame_stream_tags(data in arb_bytes()) {
        let out = frame_round_trip(
            MessageType::StreamTags,
            MessagePayload::StreamTags(data.clone()),
        );
        match out {
            MessagePayload::StreamTags(d) => prop_assert_eq!(d, data),
            other => prop_assert!(false, "expected StreamTags, got {:?}", other),
        }
    }

    // ---- No-panic fuzz on untrusted parsing entry points -----------------

    /// Feeding arbitrary bytes to the header parser must never panic; Ok or Err
    /// are both acceptable outcomes.
    #[test]
    fn prop_base_message_read_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..=64)) {
        let mut cursor = Cursor::new(&bytes);
        let _ = BaseMessage::read_from(&mut cursor);
    }

    /// Feeding an arbitrary message type + arbitrary payload bytes to the
    /// factory dispatcher must never panic (a claimed length larger than the
    /// buffer must surface as an error, not an overflow or OOM).
    #[test]
    fn prop_factory_deserialize_never_panics(
        msg_type_raw in 0u16..=20u16,
        payload in prop::collection::vec(any::<u8>(), 0..=MAX_PAYLOAD),
    ) {
        let base = BaseMessage {
            msg_type: MessageType::from_u16(msg_type_raw),
            id: 0,
            refers_to: 0,
            sent: Timeval::default(),
            received: Timeval::default(),
            size: payload.len() as u32,
        };
        let _ = factory::deserialize(base, &payload);
    }
}
