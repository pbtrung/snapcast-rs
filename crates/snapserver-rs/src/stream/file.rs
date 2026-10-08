//! File stream reader — reads PCM/WAV from a file, loops on EOF.

use anyhow::Result;
use snapcast_proto::SampleFormat;
use snapcast_server::AudioFrame;
use snapcast_server::time::ChunkTimestamper;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::uri::StreamUri;
use super::{PumpEnd, pump_pcm};

/// Start reading PCM from a file, looping on EOF.
pub fn start(
    uri: StreamUri,
    format: SampleFormat,
    chunk_frames: usize,
    tx: mpsc::Sender<AudioFrame>,
) -> Result<JoinHandle<()>> {
    let path = uri.path.clone();
    let chunk_bytes = chunk_frames * format.frame_size() as usize;
    let chunk_duration =
        std::time::Duration::from_micros((chunk_frames as u64 * 1_000_000) / format.rate() as u64);

    Ok(tokio::spawn(async move {
        let mut ts = ChunkTimestamper::new(format.rate());
        loop {
            match tokio::fs::File::open(&path).await {
                Ok(mut file) => {
                    tracing::info!(path, "File stream opened");
                    // Skip a canonical 44-byte WAV header if present. Raw PCM
                    // keeps its first bytes, or every frame after would be
                    // misaligned.
                    let mut header = [0u8; 4];
                    let mut prefix: &[u8] = &[];
                    if file.read_exact(&mut header).await.is_ok() {
                        if &header == b"RIFF" {
                            let mut skip = [0u8; 40];
                            let _ = file.read_exact(&mut skip).await;
                        } else {
                            prefix = &header;
                        }
                    }

                    // Paced reads so a finite file plays back in real time.
                    match pump_pcm(
                        &mut prefix.chain(&mut file),
                        &mut ts,
                        chunk_frames,
                        chunk_bytes,
                        &tx,
                        Some(chunk_duration),
                    )
                    .await
                    {
                        PumpEnd::SourceEnded => {} // EOF → reopen and loop
                        PumpEnd::TxClosed => return,
                    }
                }
                Err(e) => {
                    tracing::warn!(path, error = %e, "Cannot open file, retrying");
                }
            }
            ts.reset();
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: the first 4 bytes of a raw (non-WAV) file were read to
    /// sniff for `RIFF` and then dropped, shifting all audio after them.
    #[tokio::test]
    async fn raw_pcm_keeps_its_first_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("raw.pcm");
        // 48 kHz 16-bit mono: 2-byte frames, 2-frame chunks.
        std::fs::write(&path, [1u8, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        let uri = StreamUri::parse(&format!("file://{}?name=f", path.display())).unwrap();
        let (tx, mut rx) = mpsc::channel(4);
        let handle = start(uri, SampleFormat::new(48000, 16, 1), 2, tx).unwrap();
        let mut chunks = Vec::new();
        for _ in 0..2 {
            let frame = rx.recv().await.unwrap();
            let snapcast_server::AudioData::Pcm(data) = frame.data else {
                panic!("expected PCM");
            };
            chunks.push(data);
        }
        handle.abort();
        assert_eq!(chunks, [vec![1, 2, 3, 4], vec![5, 6, 7, 8]]);
    }
}
