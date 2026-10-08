//! Control server — JSON-RPC over TCP for Snapcast control clients.

use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc};

use crate::auth::AuthConfig;
use crate::jsonrpc::{self, MAX_REQUEST_LEN};

/// Configuration for the control server.
pub(crate) struct ControlConfig {
    /// TCP bind address.
    pub bind_address: String,
    /// TCP port.
    pub port: u16,
    /// Notification broadcast sender.
    pub notify_tx: broadcast::Sender<Value>,
    /// Auth configuration.
    pub auth_config: Arc<AuthConfig>,
    /// Server command sender.
    pub cmd_tx: mpsc::Sender<snapcast_server::ServerCommand>,
}

/// Runs the JSON-RPC control server on a TCP port.
pub(crate) async fn run_tcp(cfg: ControlConfig) -> Result<()> {
    let listener = TcpListener::bind((cfg.bind_address.as_str(), cfg.port)).await?;
    tracing::info!(
        bind_address = %cfg.bind_address,
        port = cfg.port,
        "Control server (TCP) listening"
    );

    loop {
        let (stream, peer) = listener.accept().await?;
        tracing::debug!(%peer, "Control client connected");

        let mut notify_rx = cfg.notify_tx.subscribe();
        let auth_config = Arc::clone(&cfg.auth_config);
        let cmd_tx = cfg.cmd_tx.clone();

        tokio::spawn(async move {
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let mut line = Vec::new();
            let mut authenticated = !auth_config.enabled;

            loop {
                tokio::select! {
                    read = read_line(&mut reader, &mut line) => {
                        let text = match read {
                            Ok(Some(text)) => text,
                            Ok(None) => break,
                            Err(e) => {
                                tracing::warn!(%peer, error = %e, "Dropping control client");
                                break;
                            }
                        };
                        if text.trim().is_empty() { continue; }
                        if let Some(reply) =
                            jsonrpc::handle_message(&text, &mut authenticated, &auth_config, &cmd_tx).await
                            && send_json(&mut writer, &reply).await.is_err()
                        {
                            break;
                        }
                    }
                    notification = notify_rx.recv() => {
                        match notification {
                            Ok(n) => {
                                if send_json(&mut writer, &n).await.is_err() { break; }
                            }
                            Err(broadcast::error::RecvError::Lagged(missed)) => {
                                tracing::warn!(%peer, missed, "Control client missed notifications");
                            }
                            Err(broadcast::error::RecvError::Closed) => break,
                        }
                    }
                }
            }
            tracing::debug!(%peer, "Control client disconnected");
        });
    }
}

/// Read the next `\n`-terminated line (without the line ending), or `None`
/// at EOF. Errors on a line longer than [`MAX_REQUEST_LEN`].
///
/// Cancel safe, like [`AsyncBufReadExt::read_until`]: when used in
/// `select!`, a partial line stays in `buf` and the next call continues it.
async fn read_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
) -> std::io::Result<Option<String>> {
    let limit = (MAX_REQUEST_LEN + 1).saturating_sub(buf.len()) as u64;
    (&mut *reader).take(limit).read_until(b'\n', buf).await?;
    if buf.last() != Some(&b'\n') && buf.len() > MAX_REQUEST_LEN {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "request line too long",
        ));
    }
    if buf.is_empty() {
        return Ok(None);
    }
    let line = String::from_utf8_lossy(&std::mem::take(buf)).into_owned();
    Ok(Some(line.trim_end_matches(['\r', '\n']).to_string()))
}

async fn send_json<W: AsyncWriteExt + Unpin>(writer: &mut W, value: &Value) -> Result<()> {
    let mut msg = serde_json::to_string(value)?;
    msg.push('\n');
    writer.write_all(msg.as_bytes()).await.context("write json")
}

#[cfg(test)]
mod tests {
    //! Unit tests for the control-server wire framing: [`read_line`] and
    //! [`send_json`], against in-memory buffers. Request handling itself lives
    //! in [`jsonrpc::handle_message`] and is tested there.

    use super::*;

    /// A single value is serialised and terminated with exactly one newline.
    #[tokio::test]
    async fn send_json_appends_single_newline() {
        let mut buf: Vec<u8> = Vec::new();
        let value = serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": {"ok": true}});

        send_json(&mut buf, &value).await.expect("send_json ok");

        let out = String::from_utf8(buf).expect("utf8");
        assert!(out.ends_with('\n'), "must be newline-terminated: {out:?}");
        assert_eq!(
            out.matches('\n').count(),
            1,
            "exactly one framing newline, got: {out:?}"
        );
    }

    /// The bytes written are exactly `serde_json::to_string(value)` + `\n`, so a
    /// client parsing a line back gets the same value.
    #[tokio::test]
    async fn send_json_matches_serde_serialization_plus_newline() {
        let mut buf: Vec<u8> = Vec::new();
        let value = serde_json::json!({"a": 1, "b": [true, null, "x"]});

        send_json(&mut buf, &value).await.expect("send_json ok");

        let out = String::from_utf8(buf).expect("utf8");
        let expected = format!("{}\n", serde_json::to_string(&value).unwrap());
        assert_eq!(out, expected);

        // And the framed line round-trips back to the original value.
        let line = out.strip_suffix('\n').unwrap();
        let reparsed: Value = serde_json::from_str(line).expect("reparse");
        assert_eq!(reparsed, value);
    }

    /// Two sequential sends append onto the same writer, producing two
    /// independently parseable newline-delimited frames (the on-wire protocol).
    #[tokio::test]
    async fn send_json_frames_multiple_messages() {
        let mut buf: Vec<u8> = Vec::new();
        let first = serde_json::json!({"id": 1});
        let second = serde_json::json!({"id": 2});

        send_json(&mut buf, &first).await.expect("first");
        send_json(&mut buf, &second).await.expect("second");

        let out = String::from_utf8(buf).expect("utf8");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "two frames expected: {out:?}");
        assert_eq!(serde_json::from_str::<Value>(lines[0]).unwrap(), first);
        assert_eq!(serde_json::from_str::<Value>(lines[1]).unwrap(), second);
    }

    /// A JSON `null` value is still framed (it is a valid whole message, e.g. a
    /// bare notification body); it must not be dropped or produce empty output.
    #[tokio::test]
    async fn send_json_handles_null_value() {
        let mut buf: Vec<u8> = Vec::new();

        send_json(&mut buf, &Value::Null)
            .await
            .expect("send_json ok");

        let out = String::from_utf8(buf).expect("utf8");
        assert_eq!(out, "null\n");
    }

    /// The parse-error envelope that the accept loop emits on malformed input
    /// serialises to a well-formed, newline-framed JSON-RPC error line.
    #[tokio::test]
    async fn send_json_frames_parse_error_envelope() {
        let mut buf: Vec<u8> = Vec::new();
        let err = serde_json::json!({
            "jsonrpc": "2.0", "id": null,
            "error": {"code": -32700, "message": "Parse error"}
        });

        send_json(&mut buf, &err).await.expect("send_json ok");

        let out = String::from_utf8(buf).expect("utf8");
        let line = out.strip_suffix('\n').expect("trailing newline");
        let parsed: Value = serde_json::from_str(line).expect("valid json line");
        assert_eq!(parsed["jsonrpc"], "2.0");
        assert_eq!(parsed["error"]["code"], -32700);
        assert_eq!(parsed["error"]["message"], "Parse error");
    }

    /// Non-ASCII content survives serialisation and round-trips unchanged, so
    /// the newline framing is byte-safe for the whole message.
    #[tokio::test]
    async fn send_json_preserves_unicode_and_stays_parseable() {
        let mut buf: Vec<u8> = Vec::new();
        let value = serde_json::json!({"name": "Wohnzimmer — Über", "emoji": "🎵"});

        send_json(&mut buf, &value).await.expect("send_json ok");

        let out = String::from_utf8(buf).expect("utf8");
        // Exactly one framing newline even with embedded multi-byte chars.
        assert_eq!(out.matches('\n').count(), 1);
        let line = out.strip_suffix('\n').unwrap();
        let reparsed: Value = serde_json::from_str(line).expect("reparse");
        assert_eq!(reparsed, value);
    }

    #[tokio::test]
    async fn read_line_splits_lines_and_strips_line_endings() {
        let mut reader: &[u8] = b"{\"a\":1}\r\n\n{\"b\":2}";
        let mut buf = Vec::new();
        let mut lines = Vec::new();
        while let Some(line) = read_line(&mut reader, &mut buf).await.unwrap() {
            lines.push(line);
        }
        assert_eq!(lines, [r#"{"a":1}"#, "", r#"{"b":2}"#]);
    }

    /// Regression: a peer streaming bytes without a newline used to grow the
    /// line buffer without bound.
    #[tokio::test]
    async fn read_line_rejects_overlong_lines() {
        let at_limit = format!("{}\n", "x".repeat(MAX_REQUEST_LEN));
        let mut reader = at_limit.as_bytes();
        let mut buf = Vec::new();
        let line = read_line(&mut reader, &mut buf).await.unwrap().unwrap();
        assert_eq!(line.len(), MAX_REQUEST_LEN);

        let too_long = vec![b'x'; MAX_REQUEST_LEN * 2];
        let mut reader = too_long.as_slice();
        let err = read_line(&mut reader, &mut buf).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(buf.len() <= MAX_REQUEST_LEN + 1, "buffer stayed bounded");
    }
}
