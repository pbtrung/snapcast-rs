//! TCP stream reader — reads PCM from a TCP connection. Like C++ snapserver,
//! `mode=server` (the default) listens and reads from connecting clients,
//! `mode=client` connects to the given host and port, reconnecting when the
//! connection fails or ends.

use anyhow::Result;
use snapcast_proto::SampleFormat;
use snapcast_server::AudioFrame;
use snapcast_server::time::ChunkTimestamper;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::uri::StreamUri;
use super::{PcmEncoding, PumpEnd, RETRY_DELAY, pump_pcm};

/// Configure reading PCM over TCP; the returned closure starts it.
pub fn reader(
    uri: StreamUri,
    format: SampleFormat,
    chunk_frames: usize,
) -> Result<impl FnOnce(mpsc::Sender<AudioFrame>) -> JoinHandle<()>> {
    let client = match uri.param("mode") {
        None | Some("server") => false,
        Some("client") => true,
        Some(other) => anyhow::bail!("tcp source mode must be server or client, got {other:?}"),
    };
    let host = if uri.host.is_empty() {
        anyhow::ensure!(
            !client,
            "tcp source in client mode needs a host to connect to"
        );
        snapcast_proto::DEFAULT_BIND_ADDRESS.to_string()
    } else {
        uri.host.clone()
    };
    let port = uri.port;
    let encoding = PcmEncoding::from_uri(&uri)?;
    let chunk_bytes = encoding.chunk_bytes(format, chunk_frames);

    Ok(move |tx: mpsc::Sender<AudioFrame>| {
        tokio::spawn(async move {
            let mut ts = ChunkTimestamper::new(format.rate());
            let pump = async |stream: &mut TcpStream, ts: &mut ChunkTimestamper| {
                pump_pcm(stream, ts, chunk_frames, chunk_bytes, encoding, &tx, None).await
            };

            if client {
                loop {
                    match TcpStream::connect((host.as_str(), port)).await {
                        Ok(mut stream) => {
                            tracing::info!(host, port, "TCP stream connected");
                            match pump(&mut stream, &mut ts).await {
                                PumpEnd::SourceEnded => {
                                    tracing::info!(host, port, "TCP stream disconnected");
                                    ts.reset();
                                }
                                PumpEnd::TxClosed => return,
                            }
                        }
                        Err(e) => {
                            tracing::debug!(host, port, error = %e, "TCP stream connect failed, retrying");
                        }
                    }
                    tokio::time::sleep(RETRY_DELAY).await;
                }
            }

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
            loop {
                match listener.accept().await {
                    Ok((mut stream, peer)) => {
                        tracing::info!(%peer, "TCP stream client connected");
                        match pump(&mut stream, &mut ts).await {
                            PumpEnd::SourceEnded => {
                                tracing::info!(%peer, "TCP stream client disconnected");
                                ts.reset();
                            }
                            PumpEnd::TxClosed => return,
                        }
                    }
                    Err(e) => {
                        // E.g. out of file descriptors: back off instead
                        // of spinning on the error.
                        tracing::error!(error = %e, "TCP accept failed");
                        tokio::time::sleep(RETRY_DELAY).await;
                    }
                }
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn rejects_unknown_mode_and_client_without_host() {
        let format = SampleFormat::new(48000, 16, 2);
        let uri = StreamUri::parse("tcp://127.0.0.1:4953?name=t&mode=bogus").unwrap();
        assert!(reader(uri, format, 960).is_err());
        let uri = StreamUri::parse("tcp://?name=t&mode=client").unwrap();
        assert!(reader(uri, format, 960).is_err());
    }

    /// `mode=client` connects out to the source and reads PCM from it.
    #[tokio::test]
    async fn client_mode_reads_from_the_server_it_connects_to() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let uri = StreamUri::parse(&format!("tcp://127.0.0.1:{port}?name=t&mode=client")).unwrap();
        let (tx, mut rx) = mpsc::channel(4);
        let handle = reader(uri, SampleFormat::new(48000, 16, 1), 2).unwrap()(tx);

        let (mut source, _) = listener.accept().await.unwrap();
        source.write_all(&[1, 2, 3, 4]).await.unwrap();
        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("no chunk")
            .unwrap();
        handle.abort();
        let snapcast_server::AudioData::Pcm(data) = frame.data else {
            panic!("expected PCM");
        };
        assert_eq!(data, [1, 2, 3, 4]);
    }
}
