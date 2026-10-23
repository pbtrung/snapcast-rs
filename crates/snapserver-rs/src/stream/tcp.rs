//! TCP stream reader — reads PCM from a TCP connection. Like C++ snapserver,
//! `mode=server` (the default) listens and reads from connecting clients,
//! `mode=client` connects to the given host and port, reconnecting when the
//! connection fails or ends.

use anyhow::Result;
use snapcast_proto::SampleFormat;
use snapcast_server::AudioFrame;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::uri::StreamUri;
use super::{Chunking, PumpEnd, RETRY_DELAY, flush_encoder, pump_pcm};

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
    let chunking = Chunking::from_uri(&uri, format, chunk_frames)?;
    // Bound now, so a port that is taken is reported at startup rather than
    // leaving a stream that never plays.
    let listener = match client {
        true => None,
        false => {
            let listener = std::net::TcpListener::bind((host.as_str(), port))
                .map_err(|e| anyhow::anyhow!("cannot listen on {host}:{port}: {e}"))?;
            listener.set_nonblocking(true)?;
            tracing::info!(bind_address = %host, port, "TCP stream listening");
            Some(listener)
        }
    };

    Ok(move |tx: mpsc::Sender<AudioFrame>| {
        tokio::spawn(async move {
            let pump = async |stream: &mut TcpStream| {
                let mut ts = chunking.timestamper();
                let end = pump_pcm(stream, &mut ts, &chunking, &tx).await;
                if let PumpEnd::SourceEnded { .. } = end {
                    flush_encoder(&mut ts, &tx).await;
                }
                end
            };

            if client {
                loop {
                    match TcpStream::connect((host.as_str(), port)).await {
                        Ok(mut stream) => {
                            tracing::info!(host, port, "TCP stream connected");
                            match pump(&mut stream).await {
                                PumpEnd::SourceEnded { .. } => {
                                    tracing::info!(host, port, "TCP stream disconnected");
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

            let listener = match listener.map(TcpListener::from_std) {
                Some(Ok(l)) => l,
                Some(Err(e)) => {
                    tracing::error!(bind_address = %host, port, error = %e, "Failed to listen for the TCP stream");
                    return;
                }
                None => unreachable!("bound unless in client mode"),
            };
            loop {
                match listener.accept().await {
                    Ok((mut stream, peer)) => {
                        tracing::info!(%peer, "TCP stream client connected");
                        match pump(&mut stream).await {
                            PumpEnd::SourceEnded { .. } => {
                                tracing::info!(%peer, "TCP stream client disconnected");
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

    /// Regression: the listener was bound inside the reader task, so a
    /// taken port left a registered stream that never played.
    #[test]
    fn taken_port_fails_configuration() {
        let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = taken.local_addr().unwrap().port();
        let uri = StreamUri::parse(&format!("tcp://127.0.0.1:{port}?name=t")).unwrap();
        let err = reader(uri, SampleFormat::new(48000, 16, 2), 960)
            .err()
            .expect("port in use");
        assert!(err.to_string().contains("cannot listen"), "{err}");
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
