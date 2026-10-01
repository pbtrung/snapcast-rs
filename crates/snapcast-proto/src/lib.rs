// `deny` (not `forbid`) so the single, audited monotonic-clock FFI in `time`
// can opt in via `#[allow(unsafe_code)]`. All other code remains unsafe-free.
#![deny(unsafe_code)]
#![warn(clippy::redundant_closure)]
#![warn(clippy::implicit_clone)]
#![warn(clippy::uninlined_format_args)]
#![warn(missing_docs)]

//! Snapcast binary protocol implementation.
//!
//! This crate implements the Snapcast binary wire protocol, providing
//! serialization and deserialization for all message types exchanged
//! between snapclient and snapserver.
//!
//! # Protocol Overview
//!
//! Every message consists of a [`BaseMessage`] header followed by a typed payload.
//! All multi-byte integers are little-endian.
//!
//! See the [protocol documentation](https://github.com/snapcast/snapcast/blob/master/doc/binary_protocol.md)
//! for the full specification.

pub mod message;
pub mod sample_format;
pub mod status;
pub mod time;
pub mod types;

pub use message::MessageType;
pub use message::base::{BaseMessage, ProtoError};
pub use sample_format::SampleFormat;
pub use types::Timeval;

/// Default TCP port for binary protocol (streaming clients).
pub const DEFAULT_STREAM_PORT: u16 = 1704;
/// Default TCP port for JSON-RPC control.
pub const DEFAULT_CONTROL_PORT: u16 = 1705;
/// Default HTTP port for JSON-RPC + Snapweb.
pub const DEFAULT_HTTP_PORT: u16 = 1780;
/// Snapcast binary protocol version.
pub const PROTOCOL_VERSION: u32 = 2;
/// Snapcast JSON-RPC control protocol version.
pub const CONTROL_PROTOCOL_VERSION: u32 = 1;
/// Default sample format: 48000 Hz, 16-bit, stereo.
pub const DEFAULT_SAMPLE_FORMAT: SampleFormat = SampleFormat::new(48000, 16, 2);
/// Maximum absolute value of a 24-bit signed integer sample (2^23 - 1).
pub const PCM_24BIT_MAX: f32 = 8_388_607.0;
/// Default sample format string used by config files and command-line defaults.
pub const DEFAULT_SAMPLE_FORMAT_STRING: &str = "48000:16:2";
/// Default playout buffer size in milliseconds.
pub const DEFAULT_BUFFER_MS: u32 = 1000;
/// Default mDNS service type for Snapcast discovery.
pub const DEFAULT_MDNS_SERVICE_TYPE: &str = "_snapcast._tcp.local.";
/// Default client display name.
pub const DEFAULT_CLIENT_NAME: &str = "Snapclient";
/// Default server display name.
pub const DEFAULT_SERVER_NAME: &str = "Snapserver";
/// Default TCP bind address for server listeners.
pub const DEFAULT_BIND_ADDRESS: &str = "0.0.0.0";
/// Maximum accepted binary protocol payload size.
pub const DEFAULT_MAX_PAYLOAD_SIZE: u32 = 2 * 1024 * 1024;
/// Plain TCP streaming transport scheme.
pub const SCHEME_TCP: &str = "tcp";
/// WebSocket streaming transport scheme.
pub const SCHEME_WS: &str = "ws";
/// HTTP path of the WebSocket endpoint for streaming clients (`ws://host:1780/stream`).
///
/// Each binary WebSocket message carries exactly one binary-protocol frame.
pub const WS_STREAM_PATH: &str = "/stream";
/// Raw PCM codec name.
pub const CODEC_PCM: &str = "pcm";
/// FLAC codec name.
pub const CODEC_FLAC: &str = "flac";
/// Opus codec name.
pub const CODEC_OPUS: &str = "opus";
