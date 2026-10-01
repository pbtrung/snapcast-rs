//! WebSocket connection to a snapserver.
//!
//! Connects to the server's streaming endpoint
//! ([`snapcast_proto::WS_STREAM_PATH`]); every binary message carries exactly
//! one binary-protocol frame.

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use snapcast_proto::MessageType;
use snapcast_proto::message::base::BaseMessage;
use snapcast_proto::message::factory::{self, MessagePayload, TypedMessage};
use snapcast_proto::types::Timeval;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

/// WebSocket transport stream type (always plain TCP; no TLS support).
type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

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
                msg.base.received = super::steady_time_of_day();
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
        let (ws, _) = tokio_tungstenite::connect_async(&url)
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
