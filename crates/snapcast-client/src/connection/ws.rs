//! WebSocket connection to a snapserver.
//!
//! Connects to the server's streaming endpoint
//! ([`snapcast_proto::WS_STREAM_PATH`]); every binary message carries exactly
//! one binary-protocol frame.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::task::{Context as TaskContext, Poll};

use anyhow::{Context, Result};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use snapcast_proto::MessageType;
use snapcast_proto::message::base::BaseMessage;
use snapcast_proto::message::factory::{self, MessagePayload, TypedMessage};
use snapcast_proto::types::Timeval;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

/// WebSocket transport stream type (always plain TCP; no TLS support).
type WsStream = WebSocketStream<StampedStream>;

/// A TCP stream that records when its last read returned data.
///
/// The WebSocket layer reads the socket itself; a message's arrival time is
/// that of the read completing it, as on the plain TCP transport. The time
/// (µs) is shared with the reader task, which only has the stream's read
/// half.
struct StampedStream {
    inner: TcpStream,
    read_at: Arc<AtomicI64>,
}

impl AsyncRead for StampedStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let filled = buf.filled().len();
        let poll = Pin::new(&mut self.inner).poll_read(cx, buf);
        if matches!(poll, Poll::Ready(Ok(()))) && buf.filled().len() > filled {
            self.read_at
                .store(snapcast_proto::time::now_usec(), Ordering::Relaxed);
        }
        poll
    }
}

impl AsyncWrite for StampedStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// URL of the server's WebSocket streaming endpoint, bracketing IPv6 hosts.
fn stream_url(scheme: &str, host: &str, port: u16) -> String {
    let path = snapcast_proto::WS_STREAM_PATH;
    if host.contains(':') {
        format!("{scheme}://[{host}]:{port}{path}")
    } else {
        format!("{scheme}://{host}:{port}{path}")
    }
}

/// Send one binary Snapcast frame over the WebSocket stream.
async fn send_frame(
    ws: &mut SplitSink<WsStream, Message>,
    msg_type: MessageType,
    payload: &MessagePayload,
) -> Result<()> {
    let mut base = BaseMessage {
        msg_type,
        id: 0,
        refers_to: 0,
        sent: Timeval::default(),
        received: Timeval::default(),
        size: 0,
    };
    super::stamp_sent(&mut base);
    let frame =
        factory::serialize(&mut base, payload).map_err(|e| anyhow::anyhow!("serialize: {e}"))?;
    ws.send(Message::Binary(frame.into())).await?;
    Ok(())
}

/// Receive one binary Snapcast frame from the WebSocket stream.
async fn recv_frame(ws: &mut SplitStream<WsStream>, read_at: &AtomicI64) -> Result<TypedMessage> {
    loop {
        let msg = ws
            .next()
            .await
            .context("WebSocket stream ended")?
            .context("WebSocket error")?;
        match msg {
            Message::Binary(data) => {
                // Each binary message carries exactly one complete frame.
                let mut buf = data.to_vec();
                let mut msg = factory::take_frame(&mut buf)
                    .map_err(|e| anyhow::anyhow!("parse frame: {e}"))?
                    .context("incomplete frame in WebSocket message")?;
                anyhow::ensure!(
                    buf.is_empty(),
                    "{} trailing bytes after frame in WebSocket message",
                    buf.len()
                );
                msg.base.received = Timeval::from_usec(read_at.load(Ordering::Relaxed));
                return Ok(msg);
            }
            Message::Close(_) => anyhow::bail!("WebSocket closed"),
            _ => continue, // skip text/ping/pong
        }
    }
}

/// WebSocket transport for Snapcast binary frames.
pub struct WsConnection {
    sink: Option<SplitSink<WsStream, Message>>,
    reader: Option<super::Reader>,
    host: String,
    port: u16,
}

impl WsConnection {
    /// Create a new WebSocket connection descriptor.
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            sink: None,
            reader: None,
            host: host.to_string(),
            port,
        }
    }

    /// Establish the WebSocket connection.
    pub async fn connect(&mut self) -> Result<()> {
        let url = stream_url(snapcast_proto::SCHEME_WS, &self.host, self.port);
        let tcp = TcpStream::connect((self.host.as_str(), self.port))
            .await
            .with_context(|| format!("connecting to {}:{}", self.host, self.port))?;
        // Time sync messages are tiny; don't let Nagle delay them.
        tcp.set_nodelay(true).context("setting TCP_NODELAY")?;
        let read_at = Arc::new(AtomicI64::new(0));
        let stream = StampedStream {
            inner: tcp,
            read_at: Arc::clone(&read_at),
        };
        let (ws, _) = tokio_tungstenite::client_async(&url, stream)
            .await
            .with_context(|| format!("WebSocket connect to {url}"))?;
        let (sink, mut stream) = ws.split();
        self.reader = Some(super::Reader::spawn(|tx| async move {
            while super::forward(&tx, recv_frame(&mut stream, &read_at).await).await {}
        }));
        self.sink = Some(sink);
        Ok(())
    }

    /// Close the WebSocket connection.
    pub fn disconnect(&mut self) {
        self.sink = None;
        self.reader = None;
    }

    /// Send one binary Snapcast frame.
    pub async fn send(&mut self, msg_type: MessageType, payload: &MessagePayload) -> Result<()> {
        send_frame(
            self.sink.as_mut().context("not connected")?,
            msg_type,
            payload,
        )
        .await
    }

    /// Receive one binary Snapcast frame.
    pub async fn recv(&mut self) -> Result<TypedMessage> {
        self.reader.as_mut().context("not connected")?.recv().await
    }
}

#[cfg(test)]
mod tests {
    use super::stream_url;

    #[test]
    fn stream_url_targets_stream_endpoint() {
        assert_eq!(stream_url("ws", "host", 1780), "ws://host:1780/stream");
        assert_eq!(stream_url("ws", "::1", 1780), "ws://[::1]:1780/stream");
    }
}
