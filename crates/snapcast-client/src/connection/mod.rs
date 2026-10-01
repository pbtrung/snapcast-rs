//! Connection layer.
//!
//! TCP is the supported Snapcast audio transport. The WebSocket modules are
//! kept feature-gated for future interoperability work, but they are not
//! selected by [`SnapConnection::new`] until the server and client can speak a
//! verified binary audio-streaming WebSocket contract.

#[cfg(feature = "websocket")]
pub mod ws;
#[cfg(feature = "tls")]
pub mod wss;

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use snapcast_proto::MessageType;
use snapcast_proto::message::base::BaseMessage;
use snapcast_proto::message::factory::{self, MessagePayload, TypedMessage};
use snapcast_proto::types::Timeval;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::oneshot;

/// Read a complete frame (header + payload) from an async reader.
///
/// Bytes are accumulated in `buf` across calls and only consumed once a whole
/// frame has arrived, so this is cancel-safe inside `tokio::select!`. Bytes
/// past the returned frame stay in `buf` for the next call.
async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
) -> Result<TypedMessage> {
    loop {
        if let Some(mut msg) =
            factory::take_frame(buf).map_err(|e| anyhow::anyhow!("parsing frame: {e}"))?
        {
            // Stamp received time using steady clock (matching C++ steadytimeofday)
            msg.base.received = steady_time_of_day();
            return Ok(msg);
        }
        buf.reserve(8192);
        let n = reader.read_buf(buf).await.context("reading frame")?;
        anyhow::ensure!(n > 0, "connection closed by server");
    }
}

/// Write a complete frame (header + payload) to an async writer.
async fn write_frame<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    base: &mut BaseMessage,
    payload: &MessagePayload,
) -> Result<()> {
    let frame =
        factory::serialize(base, payload).map_err(|e| anyhow::anyhow!("serializing: {e}"))?;
    writer.write_all(&frame).await.context("writing frame")?;
    Ok(())
}

/// Pending request waiting for a response.
struct PendingRequest {
    tx: oneshot::Sender<TypedMessage>,
}

/// TCP connection to a snapserver.
pub struct TcpConnection {
    stream: Option<TcpStream>,
    /// Received bytes not yet assembled into a complete frame.
    read_buf: Vec<u8>,
    host: String,
    port: u16,
    pending: HashMap<u16, PendingRequest>,
    next_id: u16,
}

/// Unified connection over supported transports.
pub enum SnapConnection {
    /// Plain TCP connection.
    Tcp(TcpConnection),
    #[cfg(feature = "websocket")]
    /// WebSocket (non-secure) connection.
    Ws(ws::WsConnection),
    #[cfg(feature = "tls")]
    /// WebSocket over TLS (secure) connection.
    Wss(wss::WssConnection),
}

impl SnapConnection {
    /// Create a new connection based on the scheme.
    pub fn new(scheme: &str, host: &str, port: u16) -> Result<Self> {
        match scheme {
            snapcast_proto::SCHEME_TCP => Ok(Self::Tcp(TcpConnection::new(host, port))),
            snapcast_proto::SCHEME_WS | snapcast_proto::SCHEME_WSS => anyhow::bail!(
                "websocket audio transport is not supported yet; use tcp:// for Snapcast audio"
            ),
            _ => anyhow::bail!("unsupported scheme: {scheme}"),
        }
    }

    /// Establish the connection.
    pub async fn connect(&mut self) -> Result<()> {
        match self {
            Self::Tcp(c) => c.connect().await,
            #[cfg(feature = "websocket")]
            Self::Ws(c) => c.connect().await,
            #[cfg(feature = "tls")]
            Self::Wss(c) => c.connect().await,
        }
    }

    /// Close the connection.
    pub fn disconnect(&mut self) {
        match self {
            Self::Tcp(c) => c.disconnect(),
            #[cfg(feature = "websocket")]
            Self::Ws(c) => c.disconnect(),
            #[cfg(feature = "tls")]
            Self::Wss(c) => c.disconnect(),
        }
    }

    /// Send a message.
    pub async fn send(&mut self, msg_type: MessageType, payload: &MessagePayload) -> Result<()> {
        match self {
            Self::Tcp(c) => c.send(msg_type, payload).await,
            #[cfg(feature = "websocket")]
            Self::Ws(c) => c.send(msg_type, payload).await,
            #[cfg(feature = "tls")]
            Self::Wss(c) => c.send(msg_type, payload).await,
        }
    }

    /// Receive the next message.
    pub async fn recv(&mut self) -> Result<TypedMessage> {
        match self {
            Self::Tcp(c) => c.recv().await,
            #[cfg(feature = "websocket")]
            Self::Ws(c) => c.recv().await,
            #[cfg(feature = "tls")]
            Self::Wss(c) => c.recv().await,
        }
    }
}

impl TcpConnection {
    /// Create a new connection to the given host and port.
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            stream: None,
            read_buf: Vec::new(),
            host: host.to_string(),
            port,
            pending: HashMap::new(),
            next_id: 1,
        }
    }

    /// Establish the TCP connection.
    pub async fn connect(&mut self) -> Result<()> {
        let addr = format!("{}:{}", self.host, self.port);
        let stream = TcpStream::connect(&addr)
            .await
            .with_context(|| format!("connecting to {addr}"))?;
        self.stream = Some(stream);
        self.read_buf.clear();
        self.pending.clear();
        self.next_id = 1;
        Ok(())
    }

    /// Close the connection.
    pub fn disconnect(&mut self) {
        self.stream = None;
        self.pending.clear();
    }

    fn stream_mut(&mut self) -> Result<&mut TcpStream> {
        self.stream.as_mut().context("not connected")
    }

    /// Send a message without waiting for a response.
    pub async fn send(&mut self, msg_type: MessageType, payload: &MessagePayload) -> Result<()> {
        let stream = self.stream_mut()?;
        let mut base = BaseMessage {
            msg_type,
            id: 0,
            refers_to: 0,
            sent: Timeval::default(),
            received: Timeval::default(),
            size: 0,
        };
        stamp_sent(&mut base);
        write_frame(stream, &mut base, payload).await
    }

    /// Send a request and wait for the response (matched by `refersTo`).
    pub async fn send_request(
        &mut self,
        msg_type: MessageType,
        payload: &MessagePayload,
        timeout: Duration,
    ) -> Result<TypedMessage> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);

        let (tx, rx) = oneshot::channel();
        self.pending.insert(id, PendingRequest { tx });

        let stream = self.stream_mut()?;
        let mut base = BaseMessage {
            msg_type,
            id,
            refers_to: 0,
            sent: Timeval::default(),
            received: Timeval::default(),
            size: 0,
        };
        stamp_sent(&mut base);
        write_frame(stream, &mut base, payload).await?;

        tokio::time::timeout(timeout, rx)
            .await
            .context("request timed out")?
            .context("response channel closed")
    }

    /// Receive the next message. If it's a response to a pending request,
    /// deliver it to the waiting caller and receive again.
    pub async fn recv(&mut self) -> Result<TypedMessage> {
        loop {
            let stream = self.stream.as_mut().context("not connected")?;
            let msg = read_frame(stream, &mut self.read_buf).await?;

            if msg.base.refers_to != 0
                && let Some(pending) = self.pending.remove(&msg.base.refers_to)
            {
                let _ = pending.tx.send(msg);
                continue;
            }
            return Ok(msg);
        }
    }
}

pub(super) fn stamp_sent(base: &mut BaseMessage) {
    let tv = steady_time_of_day();
    base.sent = tv;
}

/// Matches the C++ `chronos::steadytimeofday` — monotonic clock time.
/// On macOS/Linux this samples the same clock domain as the C++ snapserver.
pub(super) fn steady_time_of_day() -> Timeval {
    let usec = snapcast_proto::time::now_usec();
    Timeval {
        sec: (usec / 1_000_000) as i32,
        usec: (usec % 1_000_000) as i32,
    }
}

/// Current time in microseconds using the steady clock.
///
/// Single source of truth lives in [`snapcast_proto::time`] so the client and
/// server cannot drift onto different clock domains.
pub fn now_usec() -> i64 {
    snapcast_proto::time::now_usec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use snapcast_proto::message::time::Time;

    /// Test frame read/write with in-memory buffers (no network needed).
    #[tokio::test]
    async fn write_and_read_frame() {
        let payload = MessagePayload::Time(Time {
            latency: Timeval { sec: 0, usec: 1234 },
        });
        let mut base = BaseMessage {
            msg_type: MessageType::Time,
            id: 42,
            refers_to: 0,
            sent: Timeval { sec: 1, usec: 0 },
            received: Timeval::default(),
            size: 0,
        };

        // Write to buffer
        let mut buf = Vec::new();
        write_frame(&mut buf, &mut base, &payload).await.unwrap();

        // Size should be header + payload
        assert_eq!(buf.len(), BaseMessage::HEADER_SIZE + Time::SIZE as usize);

        // Read back
        let mut cursor = std::io::Cursor::new(&buf);
        let mut rbuf = Vec::new();
        let msg = read_frame(&mut cursor, &mut rbuf).await.unwrap();
        assert_eq!(msg.base.msg_type, MessageType::Time);
        assert_eq!(msg.base.id, 42);
        match msg.payload {
            MessagePayload::Time(t) => assert_eq!(t.latency.usec, 1234),
            _ => panic!("expected Time"),
        }
    }

    #[tokio::test]
    async fn write_and_read_error_frame() {
        use snapcast_proto::message::error::Error;

        let payload = MessagePayload::Error(Error {
            code: 401,
            error: "Unauthorized".into(),
            message: "bad auth".into(),
        });
        let mut base = BaseMessage {
            msg_type: MessageType::Error,
            id: 0,
            refers_to: 7,
            sent: Timeval::default(),
            received: Timeval::default(),
            size: 0,
        };

        let mut buf = Vec::new();
        write_frame(&mut buf, &mut base, &payload).await.unwrap();

        let mut cursor = std::io::Cursor::new(&buf);

        let mut rbuf = Vec::new();
        let msg = read_frame(&mut cursor, &mut rbuf).await.unwrap();
        assert_eq!(msg.base.refers_to, 7);
        match msg.payload {
            MessagePayload::Error(e) => {
                assert_eq!(e.code, 401);
                assert_eq!(e.error, "Unauthorized");
            }
            _ => panic!("expected Error"),
        }
    }

    #[tokio::test]
    async fn write_and_read_multiple_frames() {
        let frames: Vec<(MessageType, MessagePayload)> = vec![
            (MessageType::Time, MessagePayload::Time(Time::default())),
            (
                MessageType::ClientInfo,
                MessagePayload::ClientInfo(snapcast_proto::message::client_info::ClientInfo {
                    volume: 80,
                    muted: false,
                }),
            ),
        ];

        let mut buf = Vec::new();
        for (mt, payload) in &frames {
            let mut base = BaseMessage {
                msg_type: *mt,
                id: 0,
                refers_to: 0,
                sent: Timeval::default(),
                received: Timeval::default(),
                size: 0,
            };
            write_frame(&mut buf, &mut base, payload).await.unwrap();
        }

        // Read both back
        let mut cursor = std::io::Cursor::new(&buf);
        let mut rbuf = Vec::new();
        let msg1 = read_frame(&mut cursor, &mut rbuf).await.unwrap();
        assert_eq!(msg1.base.msg_type, MessageType::Time);
        let msg2 = read_frame(&mut cursor, &mut rbuf).await.unwrap();
        assert_eq!(msg2.base.msg_type, MessageType::ClientInfo);
    }

    #[test]
    fn tcp_connection_new() {
        let conn = TcpConnection::new("localhost", 1704);
        assert!(conn.stream.is_none());
        assert_eq!(conn.host, "localhost");
        assert_eq!(conn.port, 1704);
    }

    #[test]
    fn rejects_websocket_audio_scheme() {
        assert!(SnapConnection::new("ws", "localhost", 1780).is_err());
        assert!(SnapConnection::new("wss", "localhost", 1788).is_err());
    }

    #[test]
    fn snapconnection_new_accepts_tcp() {
        let conn = SnapConnection::new(snapcast_proto::SCHEME_TCP, "localhost", 1704).unwrap();
        assert!(matches!(conn, SnapConnection::Tcp(_)));
    }

    #[test]
    fn snapconnection_new_rejects_unknown_scheme() {
        assert!(SnapConnection::new("gopher", "localhost", 70).is_err());
    }

    // ---- read_frame error paths ----

    #[tokio::test]
    async fn read_frame_empty_reader_errors() {
        // Header read_exact fails immediately on an empty reader.
        let mut cursor = std::io::Cursor::new(Vec::<u8>::new());
        let mut rbuf = Vec::new();
        assert!(read_frame(&mut cursor, &mut rbuf).await.is_err());
    }

    #[tokio::test]
    async fn read_frame_truncated_header_errors() {
        // Fewer than HEADER_SIZE bytes → header read_exact fails.
        let mut cursor = std::io::Cursor::new(vec![0u8; BaseMessage::HEADER_SIZE - 1]);
        let mut rbuf = Vec::new();
        assert!(read_frame(&mut cursor, &mut rbuf).await.is_err());
    }

    #[tokio::test]
    async fn read_frame_truncated_payload_errors() {
        // A valid frame with its last payload byte chopped off: the header parses,
        // but the payload read_exact runs short.
        let payload = MessagePayload::Time(Time {
            latency: Timeval { sec: 0, usec: 5 },
        });
        let mut base = BaseMessage {
            msg_type: MessageType::Time,
            id: 1,
            refers_to: 0,
            sent: Timeval::default(),
            received: Timeval::default(),
            size: 0,
        };
        let mut buf = Vec::new();
        write_frame(&mut buf, &mut base, &payload).await.unwrap();
        buf.truncate(buf.len() - 1);
        let mut cursor = std::io::Cursor::new(buf);
        let mut rbuf = Vec::new();
        assert!(read_frame(&mut cursor, &mut rbuf).await.is_err());
    }

    // ---- TcpConnection "not connected" paths ----

    #[tokio::test]
    async fn send_when_not_connected_errors() {
        let mut conn = TcpConnection::new("localhost", 1704);
        let payload = MessagePayload::Time(Time::default());
        assert!(conn.send(MessageType::Time, &payload).await.is_err());
    }

    #[tokio::test]
    async fn recv_when_not_connected_errors() {
        let mut conn = TcpConnection::new("localhost", 1704);
        assert!(conn.recv().await.is_err());
    }

    #[tokio::test]
    async fn send_request_when_not_connected_errors_and_advances_id() {
        let mut conn = TcpConnection::new("localhost", 1704);
        conn.next_id = u16::MAX; // exercise the wrapping_add
        let payload = MessagePayload::Time(Time::default());
        let res = conn
            .send_request(MessageType::Time, &payload, Duration::from_millis(10))
            .await;
        assert!(res.is_err(), "not connected");
        assert_eq!(conn.next_id, 0, "next_id wraps past u16::MAX");
    }

    #[test]
    fn disconnect_clears_stream_and_pending() {
        let mut conn = TcpConnection::new("localhost", 1704);
        let (tx, _rx) = oneshot::channel();
        conn.pending.insert(5, PendingRequest { tx });
        assert_eq!(conn.pending.len(), 1);
        conn.disconnect();
        assert!(conn.stream.is_none());
        assert!(conn.pending.is_empty());
    }

    // ---- steady-clock helpers ----

    #[test]
    fn time_helpers_produce_positive_values() {
        assert!(now_usec() > 0);
        let tv = steady_time_of_day();
        assert!(tv.sec > 0 || tv.usec > 0);
        let mut base = BaseMessage {
            msg_type: MessageType::Time,
            id: 0,
            refers_to: 0,
            sent: Timeval::default(),
            received: Timeval::default(),
            size: 0,
        };
        stamp_sent(&mut base);
        assert!(base.sent.sec > 0 || base.sent.usec > 0);
    }
}
