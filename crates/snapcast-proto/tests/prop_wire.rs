//! Wave-3 property/fuzz tests for the Snapcast wire framing layer.
//!
//! Three concerns are exercised:
//!
//! 1. **No-panic on arbitrary bytes**: [`BaseMessage::read_from`] fed any
//!    `Vec<u8>` (including inputs shorter than [`BaseMessage::HEADER_SIZE`] and
//!    large inputs) must return `Ok`/`Err`, never panic or overflow.
//! 2. **No-panic on arbitrary header + payload**: [`factory::deserialize`] fed
//!    an arbitrary [`BaseMessage`] header plus arbitrary payload bytes must
//!    never panic — in particular with a huge declared `base.size` and/or a
//!    mismatched payload length.
//! 3. **Round-trip**: a generated valid header survives
//!    `write_to`/`to_bytes` -> `read_from`, and a simple typed payload survives
//!    `factory::serialize` -> `factory::deserialize`, reproducing the fields.
//!
//! A dedicated property also asserts the *bounded-allocation* guard: a frame
//! that declares a multi-gigabyte internal length prefix must be rejected with
//! [`ProtoError::PayloadTooLarge`] (or a truncation I/O error) rather than
//! attempting a giant allocation that would hang/OOM.

use std::io::Cursor;

use proptest::prelude::*;

use snapcast_proto::DEFAULT_MAX_PAYLOAD_SIZE;
use snapcast_proto::MessageType;
use snapcast_proto::message::base::{BaseMessage, ProtoError};
use snapcast_proto::message::codec_header::CodecHeader;
use snapcast_proto::message::error::Error as ErrorMsg;
use snapcast_proto::message::factory::{self, MessagePayload};
use snapcast_proto::message::time::Time;
use snapcast_proto::message::wire_chunk::WireChunk;
use snapcast_proto::types::Timeval;

// ---------------------------------------------------------------------------
// Strategies
// ---------------------------------------------------------------------------

/// Any raw u16 message-type discriminant, biased toward the known range but
/// still covering unknown/high values.
fn arb_msg_type() -> impl Strategy<Value = MessageType> {
    (0u16..=0xFFFF).prop_map(MessageType::from_u16)
}

fn arb_timeval() -> impl Strategy<Value = Timeval> {
    (any::<i32>(), any::<i32>()).prop_map(|(sec, usec)| Timeval { sec, usec })
}

/// A structurally valid `BaseMessage` header. `size` is kept in a modest range
/// so the round-trip property does not have to allocate megabytes; the
/// oversized-size behaviour is covered separately.
fn arb_base_message() -> impl Strategy<Value = BaseMessage> {
    (
        arb_msg_type(),
        any::<u16>(),
        any::<u16>(),
        arb_timeval(),
        arb_timeval(),
        0u32..=100_000u32,
    )
        .prop_map(
            |(msg_type, id, refers_to, sent, received, size)| BaseMessage {
                msg_type,
                id,
                refers_to,
                sent,
                received,
                size,
            },
        )
}

proptest! {
    // -----------------------------------------------------------------------
    // (1) NO-PANIC: BaseMessage::read_from over arbitrary bytes.
    // -----------------------------------------------------------------------

    /// Arbitrary bytes (including < HEADER_SIZE) must never panic the header
    /// decoder. Reads are bounded (fixed-size integer reads), so the only
    /// outcomes are a fully decoded header or a truncation I/O error.
    #[test]
    fn prop_base_read_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..4096)) {
        let mut cursor = Cursor::new(&bytes);
        match BaseMessage::read_from(&mut cursor) {
            Ok(_) => {
                // A full 26-byte header was available.
                prop_assert!(bytes.len() >= BaseMessage::HEADER_SIZE);
            }
            Err(ProtoError::Io(_)) => {
                // Truncated input: fewer than HEADER_SIZE bytes were readable.
                prop_assert!(bytes.len() < BaseMessage::HEADER_SIZE);
            }
            Err(other) => {
                // The header path performs no JSON / length-prefixed reads, so
                // no other error variant is reachable here.
                prop_assert!(false, "unexpected error from header decode: {other:?}");
            }
        }
    }

    /// Even a byte vector far longer than a header must be handled without
    /// panic — the decoder only consumes the first 26 bytes.
    #[test]
    fn prop_base_read_large_input_never_panics(
        bytes in prop::collection::vec(any::<u8>(), 4096..16_384),
    ) {
        let mut cursor = Cursor::new(&bytes);
        let res = BaseMessage::read_from(&mut cursor);
        // Any sufficiently long input yields a decoded header (size field is
        // just stored, never validated here).
        prop_assert!(res.is_ok());
    }

    // -----------------------------------------------------------------------
    // (2) NO-PANIC: factory::deserialize over arbitrary header + payload.
    // -----------------------------------------------------------------------

    /// An arbitrary header (any msg_type, any declared `size`, including a huge
    /// one) plus arbitrary payload bytes must never panic and must never
    /// attempt a giant allocation. `deserialize` slices the *provided* payload
    /// and only allocates via the length-checked wire readers, so a mismatched
    /// or absurd `base.size` is harmless.
    #[test]
    fn prop_factory_deserialize_never_panics(
        base in arb_base_message(),
        declared_size in prop_oneof![
            Just(0u32),
            Just(u32::MAX),
            Just(DEFAULT_MAX_PAYLOAD_SIZE),
            Just(DEFAULT_MAX_PAYLOAD_SIZE.wrapping_add(1)),
            any::<u32>(),
        ],
        payload in prop::collection::vec(any::<u8>(), 0..4096),
    ) {
        // Override `size` with a possibly-absurd declared length that does not
        // match the real payload length — this must not drive any allocation.
        let mut base = base;
        base.size = declared_size;
        let _ = factory::deserialize(base, &payload);
        // Reaching here without panic/hang/OOM is the property.
    }

    /// Focused guard check: build a *well-formed* frame prefix for a
    /// length-prefixed message type (CodecHeader/Error/WireChunk) whose first
    /// internal u32 length prefix claims a multi-gigabyte size. The wire reader
    /// must reject it with `PayloadTooLarge` (or, for a sub-cap-but-truncated
    /// value, an I/O error) instead of trying to allocate gigabytes.
    #[test]
    fn prop_oversized_internal_length_is_bounded(
        which in 0u8..3,
        claimed_len in DEFAULT_MAX_PAYLOAD_SIZE..=u32::MAX,
    ) {
        // CodecHeader: [u32 codec_len][..]; a huge codec_len must be rejected.
        // Error:       [u32 code][u32 error_len][..]; huge error_len rejected.
        // WireChunk:   [8B timeval][u32 payload_len][..]; huge payload_len.
        let (msg_type, payload) = match which {
            0 => {
                let mut p = Vec::new();
                p.extend_from_slice(&claimed_len.to_le_bytes());
                (MessageType::CodecHeader, p)
            }
            1 => {
                let mut p = Vec::new();
                p.extend_from_slice(&0u32.to_le_bytes()); // code
                p.extend_from_slice(&claimed_len.to_le_bytes()); // error_len
                (MessageType::Error, p)
            }
            _ => {
                let mut p = Vec::new();
                p.extend_from_slice(&Timeval::default().sec.to_le_bytes());
                p.extend_from_slice(&Timeval::default().usec.to_le_bytes());
                p.extend_from_slice(&claimed_len.to_le_bytes()); // payload_len
                (MessageType::WireChunk, p)
            }
        };

        let base = BaseMessage {
            msg_type,
            id: 0,
            refers_to: 0,
            sent: Timeval::default(),
            received: Timeval::default(),
            size: payload.len() as u32,
        };

        match factory::deserialize(base, &payload) {
            Err(ProtoError::PayloadTooLarge { len, max }) => {
                prop_assert_eq!(len, claimed_len as usize);
                prop_assert_eq!(max, DEFAULT_MAX_PAYLOAD_SIZE as usize);
            }
            // A claimed length above the cap must never succeed and must never
            // surface as a plain I/O error here, because the guard fires before
            // any read of the (non-existent) body.
            other => prop_assert!(
                false,
                "expected PayloadTooLarge for claimed_len={claimed_len}, got {other:?}"
            ),
        }
    }

    // -----------------------------------------------------------------------
    // (3) ROUND-TRIP: header field reproduction + simple typed payload.
    // -----------------------------------------------------------------------

    /// A generated valid header serializes to exactly HEADER_SIZE bytes and
    /// round-trips back to an equal `BaseMessage`.
    #[test]
    fn prop_base_header_round_trip(msg in arb_base_message()) {
        let bytes = msg.to_bytes().expect("serialize header");
        prop_assert_eq!(bytes.len(), BaseMessage::HEADER_SIZE);

        let mut cursor = Cursor::new(&bytes);
        let decoded = BaseMessage::read_from(&mut cursor).expect("deserialize header");
        prop_assert_eq!(msg, decoded);
    }

    /// A full `Time` frame (`serialize` -> read header -> `deserialize`)
    /// reproduces the header fields and the payload, and the header's `size`
    /// is updated to the real payload length by `serialize`.
    #[test]
    fn prop_time_frame_round_trip(
        latency in arb_timeval(),
        id in any::<u16>(),
        refers_to in any::<u16>(),
        sent in arb_timeval(),
        received in arb_timeval(),
    ) {
        let mut base = BaseMessage {
            msg_type: MessageType::Time,
            id,
            refers_to,
            sent,
            received,
            size: 0,
        };
        let payload = MessagePayload::Time(Time { latency });

        let frame = factory::serialize(&mut base, &payload).expect("serialize frame");
        // serialize() must set size to the real payload length.
        prop_assert_eq!(base.size, Time::SIZE);
        prop_assert_eq!(frame.len(), BaseMessage::HEADER_SIZE + Time::SIZE as usize);

        // Split frame into header + payload and deserialize.
        let mut cursor = Cursor::new(&frame);
        let decoded_base = BaseMessage::read_from(&mut cursor).expect("read header");
        prop_assert_eq!(decoded_base.msg_type, MessageType::Time);
        prop_assert_eq!(decoded_base.id, id);
        prop_assert_eq!(decoded_base.refers_to, refers_to);
        prop_assert_eq!(decoded_base.sent, sent);
        prop_assert_eq!(decoded_base.received, received);
        prop_assert_eq!(decoded_base.size, Time::SIZE);

        let payload_bytes = &frame[BaseMessage::HEADER_SIZE..];
        let typed = factory::deserialize(decoded_base, payload_bytes).expect("deserialize payload");
        match typed.payload {
            MessagePayload::Time(t) => prop_assert_eq!(t.latency, latency),
            other => prop_assert!(false, "expected Time payload, got {other:?}"),
        }
    }

    /// A `CodecHeader` frame round-trips through serialize/deserialize with the
    /// codec string and payload bytes preserved. Bounds are kept small (few KB)
    /// so the test stays fast.
    #[test]
    fn prop_codec_header_frame_round_trip(
        codec in prop_oneof![
            Just("flac".to_string()),
            Just("opus".to_string()),
            Just("pcm".to_string()),
            "[a-zA-Z0-9]{0,16}",
        ],
        payload_bytes in prop::collection::vec(any::<u8>(), 0..2048),
    ) {
        let ch = CodecHeader { codec: codec.clone(), payload: payload_bytes.clone() };
        let mut base = BaseMessage {
            msg_type: MessageType::CodecHeader,
            id: 0,
            refers_to: 0,
            sent: Timeval::default(),
            received: Timeval::default(),
            size: 0,
        };
        let frame = factory::serialize(&mut base, &MessagePayload::CodecHeader(ch))
            .expect("serialize codec header");

        let mut cursor = Cursor::new(&frame);
        let decoded_base = BaseMessage::read_from(&mut cursor).expect("read header");
        let payload_slice = &frame[BaseMessage::HEADER_SIZE..];
        let typed =
            factory::deserialize(decoded_base, payload_slice).expect("deserialize codec header");
        match typed.payload {
            MessagePayload::CodecHeader(c) => {
                prop_assert_eq!(c.codec, codec);
                prop_assert_eq!(c.payload, payload_bytes);
            }
            other => prop_assert!(false, "expected CodecHeader payload, got {other:?}"),
        }
    }

    /// An `Error` frame round-trips with all three fields preserved.
    #[test]
    fn prop_error_frame_round_trip(
        code in any::<u32>(),
        error in "[a-zA-Z0-9 ]{0,32}",
        message in "[a-zA-Z0-9 ]{0,64}",
    ) {
        let err = ErrorMsg { code, error: error.clone(), message: message.clone() };
        let mut base = BaseMessage {
            msg_type: MessageType::Error,
            id: 0,
            refers_to: 0,
            sent: Timeval::default(),
            received: Timeval::default(),
            size: 0,
        };
        let frame = factory::serialize(&mut base, &MessagePayload::Error(err))
            .expect("serialize error");

        let mut cursor = Cursor::new(&frame);
        let decoded_base = BaseMessage::read_from(&mut cursor).expect("read header");
        let payload_slice = &frame[BaseMessage::HEADER_SIZE..];
        let typed = factory::deserialize(decoded_base, payload_slice).expect("deserialize error");
        match typed.payload {
            MessagePayload::Error(e) => {
                prop_assert_eq!(e.code, code);
                prop_assert_eq!(e.error, error);
                prop_assert_eq!(e.message, message);
            }
            other => prop_assert!(false, "expected Error payload, got {other:?}"),
        }
    }

    /// A `WireChunk` frame round-trips with timestamp and payload preserved.
    #[test]
    fn prop_wire_chunk_frame_round_trip(
        timestamp in arb_timeval(),
        payload_bytes in prop::collection::vec(any::<u8>(), 0..2048),
    ) {
        let wc = WireChunk { timestamp, payload: payload_bytes.clone() };
        let mut base = BaseMessage {
            msg_type: MessageType::WireChunk,
            id: 0,
            refers_to: 0,
            sent: Timeval::default(),
            received: Timeval::default(),
            size: 0,
        };
        let frame = factory::serialize(&mut base, &MessagePayload::WireChunk(wc))
            .expect("serialize wire chunk");

        let mut cursor = Cursor::new(&frame);
        let decoded_base = BaseMessage::read_from(&mut cursor).expect("read header");
        let payload_slice = &frame[BaseMessage::HEADER_SIZE..];
        let typed =
            factory::deserialize(decoded_base, payload_slice).expect("deserialize wire chunk");
        match typed.payload {
            MessagePayload::WireChunk(w) => {
                prop_assert_eq!(w.timestamp, timestamp);
                prop_assert_eq!(w.payload, payload_bytes);
            }
            other => prop_assert!(false, "expected WireChunk payload, got {other:?}"),
        }
    }
}
