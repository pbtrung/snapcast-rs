//! WebSocket connection to a snapserver.
//!
//! Connects to the server's streaming endpoint
//! ([`snapcast_proto::WS_STREAM_PATH`]); every binary message carries exactly
//! one binary-protocol frame.

use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};

use anyhow::{Context, Result};
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
/// that of the read completing it, as on the plain TCP transport.
struct StampedStream {
    inner: TcpStream,
    read_at: Timeval,
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
            self.read_at = super::steady_time_of_day();
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
    ws: &mut WsStream,
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
async fn recv_frame(ws: &mut WsStream) -> Result<TypedMessage> {
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
                msg.base.received = ws.get_ref().read_at;
                return Ok(msg);
            }
            Message::Close(_) => anyhow::bail!("WebSocket closed"),
            _ => continue, // skip text/ping/pong
        }
    }
}

/// WebSocket transport for Snapcast binary frames.
pub struct WsConnection {
    ws: Option<WsStream>,
    host: String,
    port: u16,
}

impl WsConnection {
    /// Create a new WebSocket connection descriptor.
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            ws: None,
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
        let stream = StampedStream {
            inner: tcp,
            read_at: Timeval::default(),
        };
        let (ws, _) = tokio_tungstenite::client_async(&url, stream)
            .await
            .with_context(|| format!("WebSocket connect to {url}"))?;
        self.ws = Some(ws);
        Ok(())
    }

    /// Close the WebSocket connection.
    pub fn disconnect(&mut self) {
        self.ws = None;
    }

    /// Send one binary Snapcast frame.
    pub async fn send(&mut self, msg_type: MessageType, payload: &MessagePayload) -> Result<()> {
        send_frame(
            self.ws.as_mut().context("not connected")?,
            msg_type,
            payload,
        )
        .await
    }

    /// Receive one binary Snapcast frame.
    pub async fn recv(&mut self) -> Result<TypedMessage> {
        recv_frame(self.ws.as_mut().context("not connected")?).await
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
