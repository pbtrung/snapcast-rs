//! Connection layer.
//!
//! Transports: plain TCP (`tcp://`, port 1704) and WebSocket (`ws://`,
//! feature `websocket`, the server's HTTP port 1780). The WebSocket transport
//! connects to [`snapcast_proto::WS_STREAM_PATH`] and carries one
//! binary-protocol frame per binary message, as C++ snapserver/snapclient do.

#[cfg(feature = "websocket")]
pub mod ws;

use std::collections::VecDeque;
use std::time::Duration;

use anyhow::{Context, Result};
use snapcast_proto::MessageType;
use snapcast_proto::message::base::BaseMessage;
use snapcast_proto::message::factory::{self, MessagePayload, TypedMessage};
use snapcast_proto::types::Timeval;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

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

/// TCP connection to a snapserver.
pub struct TcpConnection {
    stream: Option<TcpStream>,
    /// Received bytes not yet assembled into a complete frame.
    read_buf: Vec<u8>,
    host: String,
    port: u16,
    /// Messages read by [`TcpConnection::send_request`] while waiting for its
    /// response, handed out by [`TcpConnection::recv`] in arrival order.
    queued: VecDeque<TypedMessage>,
    next_id: u16,
}

/// Unified connection over supported transports.
pub enum SnapConnection {
    /// Plain TCP connection.
    Tcp(TcpConnection),
    #[cfg(feature = "websocket")]
    /// WebSocket connection (boxed: the stream state is much larger than TCP's).
    Ws(Box<ws::WsConnection>),
}

impl SnapConnection {
    /// Create a new connection based on the scheme.
    pub fn new(scheme: &str, host: &str, port: u16) -> Result<Self> {
        match scheme {
            snapcast_proto::SCHEME_TCP => Ok(Self::Tcp(TcpConnection::new(host, port))),
            #[cfg(feature = "websocket")]
            snapcast_proto::SCHEME_WS => Ok(Self::Ws(Box::new(ws::WsConnection::new(host, port)))),
            #[cfg(not(feature = "websocket"))]
            snapcast_proto::SCHEME_WS => {
                anyhow::bail!("ws:// requires snapcast-client's `websocket` feature")
            }
            scheme => anyhow::bail!("unsupported scheme: {scheme}"),
        }
    }

    /// Establish the connection.
    pub async fn connect(&mut self) -> Result<()> {
        match self {
            Self::Tcp(c) => c.connect().await,
            #[cfg(feature = "websocket")]
            Self::Ws(c) => c.connect().await,
        }
    }

    /// Close the connection.
    pub fn disconnect(&mut self) {
        match self {
            Self::Tcp(c) => c.disconnect(),
            #[cfg(feature = "websocket")]
            Self::Ws(c) => c.disconnect(),
        }
    }

    /// Send a message.
    pub async fn send(&mut self, msg_type: MessageType, payload: &MessagePayload) -> Result<()> {
        match self {
            Self::Tcp(c) => c.send(msg_type, payload).await,
            #[cfg(feature = "websocket")]
            Self::Ws(c) => c.send(msg_type, payload).await,
        }
    }

    /// Receive the next message.
    pub async fn recv(&mut self) -> Result<TypedMessage> {
        match self {
            Self::Tcp(c) => c.recv().await,
            #[cfg(feature = "websocket")]
            Self::Ws(c) => c.recv().await,
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
            queued: VecDeque::new(),
            next_id: 1,
        }
    }

    /// Establish the TCP connection.
    pub async fn connect(&mut self) -> Result<()> {
        // A (host, port) pair also resolves bare IPv6 literals such as "::1",
        // which a "host:port" string cannot express.
        let stream = TcpStream::connect((self.host.as_str(), self.port))
            .await
            .with_context(|| format!("connecting to {}:{}", self.host, self.port))?;
        // Time sync messages are tiny; don't let Nagle delay them.
        stream.set_nodelay(true).context("setting TCP_NODELAY")?;
        self.stream = Some(stream);
        self.read_buf.clear();
        self.queued.clear();
        self.next_id = 1;
        Ok(())
    }

    /// Close the connection.
    pub fn disconnect(&mut self) {
        self.stream = None;
        self.queued.clear();
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
    ///
    /// Reads the connection itself while waiting; any other message that
    /// arrives meanwhile is queued and returned by later [`recv`](Self::recv)
    /// calls, in order.
    pub async fn send_request(
        &mut self,
        msg_type: MessageType,
        payload: &MessagePayload,
        timeout: Duration,
    ) -> Result<TypedMessage> {
        let id = self.next_id;
        // 0 means "refers to nothing", so a response could never match it.
        self.next_id = self.next_id.checked_add(1).unwrap_or(1);

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

        tokio::time::timeout(timeout, async {
            loop {
                let stream = self.stream.as_mut().context("not connected")?;
                let msg = read_frame(stream, &mut self.read_buf).await?;
                if msg.base.refers_to == id {
                    return Ok(msg);
                }
                self.queued.push_back(msg);
            }
        })
        .await
        .context("request timed out")?
    }

    /// Receive the next message, starting with any queued while a
    /// [`send_request`](Self::send_request) waited for its response.
    pub async fn recv(&mut self) -> Result<TypedMessage> {
        if let Some(msg) = self.queued.pop_front() {
            return Ok(msg);
        }
        let stream = self.stream.as_mut().context("not connected")?;
        read_frame(stream, &mut self.read_buf).await
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
    fn rejects_unknown_scheme() {
        assert!(SnapConnection::new("wss", "localhost", 1788).is_err());
    }

    #[cfg(feature = "websocket")]
    #[test]
    fn snapconnection_new_accepts_ws() {
        let conn = SnapConnection::new(snapcast_proto::SCHEME_WS, "localhost", 1780).unwrap();
        assert!(matches!(conn, SnapConnection::Ws(_)));
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
        conn.next_id = u16::MAX; // exercise the wrap
        let payload = MessagePayload::Time(Time::default());
        let res = conn
            .send_request(MessageType::Time, &payload, Duration::from_millis(10))
            .await;
        assert!(res.is_err(), "not connected");
        assert_eq!(conn.next_id, 1, "next_id wraps past u16::MAX, skipping 0");
    }

    #[test]
    fn disconnect_clears_stream_and_queue() {
        let mut conn = TcpConnection::new("localhost", 1704);
        conn.queued.push_back(TypedMessage {
            base: BaseMessage {
                msg_type: MessageType::Time,
                id: 0,
                refers_to: 0,
                sent: Timeval::default(),
                received: Timeval::default(),
                size: 0,
            },
            payload: MessagePayload::Time(Time::default()),
        });
        conn.disconnect();
        assert!(conn.stream.is_none());
        assert!(conn.queued.is_empty());
    }

    /// Server stand-in: reads one request, sends an unrelated message and
    /// then the response to it.
    async fn answer_one_request(listener: tokio::net::TcpListener) {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let req = read_frame(&mut sock, &mut buf).await.unwrap();
        let mut unrelated = BaseMessage {
            msg_type: MessageType::Time,
            id: 0,
            refers_to: 0,
            sent: Timeval::default(),
            received: Timeval::default(),
            size: 0,
        };
        let payload = MessagePayload::Time(Time {
            latency: Timeval { sec: 0, usec: 1 },
        });
        write_frame(&mut sock, &mut unrelated, &payload)
            .await
            .unwrap();
        let mut reply = BaseMessage {
            refers_to: req.base.id,
            ..unrelated
        };
        let payload = MessagePayload::Time(Time {
            latency: Timeval { sec: 0, usec: 2 },
        });
        write_frame(&mut sock, &mut reply, &payload).await.unwrap();
        // Keep the socket open until the client is done.
        let _ = read_frame(&mut sock, &mut buf).await;
    }

    #[tokio::test]
    async fn send_request_receives_its_response_and_queues_others() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(answer_one_request(listener));

        let mut conn = TcpConnection::new("127.0.0.1", port);
        conn.connect().await.unwrap();
        let payload = MessagePayload::Time(Time::default());
        let reply = conn
            .send_request(MessageType::Time, &payload, Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(reply.base.refers_to, 1);
        let MessagePayload::Time(t) = reply.payload else {
            panic!("expected Time");
        };
        assert_eq!(t.latency.usec, 2);

        let queued = conn.recv().await.unwrap();
        assert_eq!(queued.base.refers_to, 0);
        let MessagePayload::Time(t) = queued.payload else {
            panic!("expected Time");
        };
        assert_eq!(t.latency.usec, 1, "unrelated message is kept for recv");
    }

    #[tokio::test]
    async fn connect_to_bare_ipv6_literal() {
        // Skip where the host has no IPv6 loopback.
        let Ok(listener) = tokio::net::TcpListener::bind("[::1]:0").await else {
            return;
        };
        let port = listener.local_addr().unwrap().port();
        let mut conn = TcpConnection::new("::1", port);
        conn.connect().await.unwrap();
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
