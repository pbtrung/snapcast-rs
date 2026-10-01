//! Protocol message types.

pub mod base;
pub mod client_info;
pub mod codec_header;
pub mod error;
pub mod factory;
pub mod hello;
pub mod server_settings;
pub mod time;
pub mod wire;
pub mod wire_chunk;

/// Message type identifiers matching the C++ `message_type` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    /// Base message (type 0), header only.
    Base,
    /// Codec header (type 1).
    CodecHeader,
    /// Encoded audio chunk (type 2).
    WireChunk,
    /// Server settings (type 3).
    ServerSettings,
    /// Time sync (type 4).
    Time,
    /// Client hello (type 5).
    Hello,
    // 6 = StreamTags (deprecated, but C++ server still sends it)
    /// Stream tags / metadata (type 6, deprecated).
    StreamTags,
    /// Client info (type 7).
    ClientInfo,
    /// Error (type 8).
    Error,
    /// Unrecognized message type — payload is skipped/ignored.
    Unknown(u16),
}

impl MessageType {
    /// Parse from a raw `u16` value. Never fails — unknown types become `Unknown(n)`.
    pub fn from_u16(value: u16) -> Self {
        match value {
            0 => Self::Base,
            1 => Self::CodecHeader,
            2 => Self::WireChunk,
            3 => Self::ServerSettings,
            4 => Self::Time,
            5 => Self::Hello,
            6 => Self::StreamTags,
            7 => Self::ClientInfo,
            8 => Self::Error,
            _ => Self::Unknown(value),
        }
    }
}

impl From<MessageType> for u16 {
    fn from(mt: MessageType) -> Self {
        match mt {
            MessageType::Base => 0,
            MessageType::CodecHeader => 1,
            MessageType::WireChunk => 2,
            MessageType::ServerSettings => 3,
            MessageType::Time => 4,
            MessageType::Hello => 5,
            MessageType::StreamTags => 6,
            MessageType::ClientInfo => 7,
            MessageType::Error => 8,
            MessageType::Unknown(id) => id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_all_message_types() {
        let types = [
            MessageType::Base,
            MessageType::CodecHeader,
            MessageType::WireChunk,
            MessageType::ServerSettings,
            MessageType::Time,
            MessageType::Hello,
            MessageType::ClientInfo,
            MessageType::Error,
        ];
        for mt in types {
            let raw: u16 = mt.into();
            assert_eq!(MessageType::from_u16(raw), mt);
        }
    }

    #[test]
    fn unknown_message_type_returns_unknown() {
        assert_eq!(MessageType::from_u16(6), MessageType::StreamTags);
        assert_eq!(MessageType::from_u16(9), MessageType::Unknown(9));
        assert_eq!(
            MessageType::from_u16(u16::MAX),
            MessageType::Unknown(u16::MAX)
        );
    }
}
