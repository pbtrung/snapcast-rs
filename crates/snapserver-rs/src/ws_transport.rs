//! Byte-stream adapter that carries the Snapcast binary protocol over a
//! WebSocket, for streaming clients connecting to `ws://host:1780/stream`.
//!
//! Matches C++ snapserver: every binary WebSocket message holds exactly one
//! complete frame (base header + payload). The snapcast-server session loop
//! writes each frame with one `write_all` followed by `flush`, so each
//! `poll_write` becomes one message; incoming messages are exposed as a
//! contiguous byte stream that the session's frame reader reassembles.
//!
//! Like C++ snapserver, the WebSocket message boundary defines the frame: the
//! header's `size` field is rewritten to the message's actual payload length.
//! Snapweb sets `size` to header + payload, which C++ snapserver ignores
//! because it never reads `size` on WebSocket sessions.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket};
use futures_util::{Sink, Stream};
use snapcast_proto::message::base::BaseMessage;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// Byte offset of the `size` field in the 26-byte base header.
const SIZE_OFFSET: usize = BaseMessage::HEADER_SIZE - 4;

/// Turn one binary message into one frame whose header `size` matches the
/// payload the message actually carries. `None` if it can't hold a header.
fn frame_from_message(data: &[u8]) -> Option<Bytes> {
    let payload_len = data.len().checked_sub(BaseMessage::HEADER_SIZE)?;
    let mut frame = data.to_vec();
    frame[SIZE_OFFSET..BaseMessage::HEADER_SIZE]
        .copy_from_slice(&(payload_len as u32).to_le_bytes());
    Some(Bytes::from(frame))
}

/// A WebSocket presented as an `AsyncRead + AsyncWrite` frame transport.
pub(crate) struct WsTransport {
    ws: WebSocket,
    /// Unread bytes of the last received binary message.
    pending: Bytes,
}

impl WsTransport {
    pub(crate) fn new(ws: WebSocket) -> Self {
        Self {
            ws,
            pending: Bytes::new(),
        }
    }
}

impl AsyncRead for WsTransport {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            if !self.pending.is_empty() {
                let n = self.pending.len().min(buf.remaining());
                let chunk = self.pending.split_to(n);
                buf.put_slice(&chunk);
                return Poll::Ready(Ok(()));
            }
            match ready!(Pin::new(&mut self.ws).poll_next(cx)) {
                Some(Ok(Message::Binary(data))) => match frame_from_message(&data) {
                    Some(frame) => self.pending = frame,
                    None => tracing::warn!(
                        len = data.len(),
                        "Dropping WebSocket message shorter than a frame header"
                    ),
                },
                // A close frame or end of stream reads as EOF.
                Some(Ok(Message::Close(_))) | None => return Poll::Ready(Ok(())),
                // Text, ping and pong carry no protocol data (pongs are
                // answered by the WebSocket implementation itself).
                Some(Ok(_)) => {}
                Some(Err(e)) => return Poll::Ready(Err(io::Error::other(e))),
            }
        }
    }
}

impl AsyncWrite for WsTransport {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        ready!(Pin::new(&mut self.ws).poll_ready(cx)).map_err(io::Error::other)?;
        Pin::new(&mut self.ws)
            .start_send(Message::Binary(Bytes::copy_from_slice(buf)))
            .map_err(io::Error::other)?;
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.ws)
            .poll_flush(cx)
            .map_err(io::Error::other)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.ws)
            .poll_close(cx)
            .map_err(io::Error::other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_field_is_rewritten_to_payload_length() {
        // Snapweb-style message: size counts the header too (26 + 8).
        let mut msg = vec![0u8; BaseMessage::HEADER_SIZE + 8];
        msg[SIZE_OFFSET..BaseMessage::HEADER_SIZE].copy_from_slice(&34u32.to_le_bytes());
        let frame = frame_from_message(&msg).unwrap();
        let size = u32::from_le_bytes(
            frame[SIZE_OFFSET..BaseMessage::HEADER_SIZE]
                .try_into()
                .unwrap(),
        );
        assert_eq!(size, 8);
        assert_eq!(frame.len(), msg.len());
    }

    #[test]
    fn rejects_messages_shorter_than_a_header() {
        assert!(frame_from_message(&[0u8; 10]).is_none());
        assert!(frame_from_message(&[0u8; BaseMessage::HEADER_SIZE]).is_some());
    }
}
