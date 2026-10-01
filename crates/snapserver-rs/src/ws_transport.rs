//! Byte-stream adapter that carries the Snapcast binary protocol over a
//! WebSocket, for streaming clients connecting to `ws://host:1780/stream`.
//!
//! Matches C++ snapserver: every binary WebSocket message holds exactly one
//! complete frame (base header + payload). The snapcast-server session loop
//! writes each frame with one `write_all` followed by `flush`, so each
//! `poll_write` becomes one message; incoming messages are exposed as a
//! contiguous byte stream that the session's frame reader reassembles.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket};
use futures_util::{Sink, Stream};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

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
                Some(Ok(Message::Binary(data))) => self.pending = data,
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
