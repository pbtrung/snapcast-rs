//! TCP stream reader — accepts a TCP connection and reads PCM from it.

use anyhow::Result;
use snapcast_proto::SampleFormat;
use snapcast_server::AudioFrame;
use snapcast_server::time::ChunkTimestamper;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::uri::StreamUri;
use super::{PcmEncoding, PumpEnd, pump_pcm};

/// Start a TCP listener that reads PCM from connecting clients.
pub fn start(
    uri: StreamUri,
    format: SampleFormat,
    chunk_frames: usize,
    tx: mpsc::Sender<AudioFrame>,
) -> Result<JoinHandle<()>> {
    let host = if uri.host.is_empty() {
        snapcast_proto::DEFAULT_BIND_ADDRESS.to_string()
    } else {
        uri.host.clone()
    };
    let port = if uri.port == 0 { 4953 } else { uri.port };
    let encoding = PcmEncoding::from_uri(&uri)?;
    let chunk_bytes = encoding.chunk_bytes(format, chunk_frames);

    Ok(tokio::spawn(async move {
        let listener = match TcpListener::bind((host.as_str(), port)).await {
            Ok(l) => {
                tracing::info!(bind_address = %host, port, "TCP stream listening");
                l
            }
            Err(e) => {
                tracing::error!(bind_address = %host, port, error = %e, "Failed to bind TCP stream");
                return;
            }
        };

        let mut ts = ChunkTimestamper::new(format.rate());
        loop {
            match listener.accept().await {
                Ok((mut stream, peer)) => {
                    tracing::info!(%peer, "TCP stream client connected");
                    match pump_pcm(
                        &mut stream,
                        &mut ts,
                        chunk_frames,
                        chunk_bytes,
                        encoding,
                        &tx,
                        None,
                    )
                    .await
                    {
                        PumpEnd::SourceEnded => {
                            tracing::info!(%peer, "TCP stream client disconnected");
                            ts.reset();
                        }
                        PumpEnd::TxClosed => return,
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "TCP accept failed");
                }
            }
        }
    }))
}
