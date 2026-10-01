//! Wire-format constants for the `f32lz4` codec.
//!
//! Single source of truth for the byte-level contract that the server encoder
//! (`snapcast-server`) and the client decoder (`snapcast-client`) must agree
//! on exactly. These values were previously duplicated as literals in both
//! crates, where they could drift apart with
//! no compile error — a drift would surface only as a silent decode
//! failure at runtime. Define them once here; both ends reference them.

/// Codec-header magic identifying an `f32lz4` stream.
pub const F32LZ4_MAGIC: &[u8; 4] = b"F32L";

/// Length of the base `f32lz4` codec header in bytes.
///
/// Layout: `MAGIC(4) + sample_rate: u32(4) + channels: u16(2) + bits: u16(2)`.
pub const F32LZ4_HEADER_LEN: usize = 12;
