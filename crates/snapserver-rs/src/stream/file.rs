//! File stream reader — reads PCM/WAV from a file, loops on EOF.

use anyhow::Result;
use snapcast_proto::SampleFormat;
use snapcast_server::AudioFrame;
use snapcast_server::time::ChunkTimestamper;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::uri::StreamUri;
use super::{PcmEncoding, PumpEnd, RETRY_DELAY, frames_duration, pump_pcm};

/// Configure reading PCM from a file, looping on EOF; the returned closure
/// starts it.
pub fn reader(
    uri: StreamUri,
    format: SampleFormat,
    chunk_frames: usize,
) -> Result<impl FnOnce(mpsc::Sender<AudioFrame>) -> JoinHandle<()>> {
    let path = uri.path.clone();
    let encoding = PcmEncoding::from_uri(&uri)?;
    let chunk_bytes = encoding.chunk_bytes(format, chunk_frames);
    let chunk_duration = frames_duration(format, chunk_frames);

    Ok(move |tx: mpsc::Sender<AudioFrame>| {
        tokio::spawn(async move {
            let mut ts = ChunkTimestamper::new(format.rate());
            loop {
                match tokio::fs::File::open(&path).await {
                    Ok(mut file) => {
                        tracing::info!(path, "File stream opened");
                        match skip_wav_header(&mut file).await {
                            // Paced reads so a finite file plays back in real time.
                            Ok(prefix) => match pump_pcm(
                                &mut prefix.as_slice().chain(&mut file),
                                &mut ts,
                                chunk_frames,
                                chunk_bytes,
                                encoding,
                                &tx,
                                Some(chunk_duration),
                            )
                            .await
                            {
                                PumpEnd::SourceEnded => {} // EOF → reopen and loop
                                PumpEnd::TxClosed => return,
                            },
                            Err(e) => {
                                tracing::warn!(path, error = %e, "Invalid WAV file, retrying");
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(path, error = %e, "Cannot open file, retrying");
                    }
                }
                ts.reset();
                tokio::time::sleep(RETRY_DELAY).await;
            }
        })
    })
}

/// Skip a WAV file's header: the RIFF/WAVE preamble and every chunk before
/// the `data` chunk (`fmt `, `LIST`, `fact`, ...), so playback starts at the
/// first sample whatever the header's length. Returns the bytes read from a
/// file that is not a WAV file: they are raw PCM and must not be dropped, or
/// every frame after them would be misaligned.
async fn skip_wav_header<R: AsyncRead + Unpin>(file: &mut R) -> std::io::Result<Vec<u8>> {
    let mut preamble = Vec::with_capacity(12);
    (&mut *file).take(12).read_to_end(&mut preamble).await?;
    if preamble.len() < 12 || &preamble[..4] != b"RIFF" || &preamble[8..] != b"WAVE" {
        return Ok(preamble);
    }
    loop {
        let mut header = [0u8; 8];
        file.read_exact(&mut header).await?;
        if &header[..4] == b"data" {
            return Ok(Vec::new());
        }
        // Chunks are padded to an even length.
        let size = u64::from(u32::from_le_bytes([
            header[4], header[5], header[6], header[7],
        ]));
        let skip = size + (size & 1);
        let skipped = tokio::io::copy(&mut (&mut *file).take(skip), &mut tokio::io::sink()).await?;
        if skipped < skip {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
    }
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
        let handle = reader(uri, SampleFormat::new(48000, 16, 1), 2).unwrap()(tx);
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

    /// Regression: only a canonical 44-byte header was skipped, so a WAV
    /// file with more chunks before `data` (here a `LIST` chunk of odd
    /// length) played its tail as noise and was misaligned after it.
    #[tokio::test]
    async fn wav_header_is_skipped_up_to_the_data_chunk() {
        let mut wav = b"RIFF\0\0\0\0WAVE".to_vec();
        wav.extend_from_slice(b"fmt \x10\0\0\0");
        wav.extend_from_slice(&[0u8; 16]);
        wav.extend_from_slice(b"LIST\x03\0\0\0abc\0");
        wav.extend_from_slice(b"data\x04\0\0\0");
        wav.extend_from_slice(&[1, 2, 3, 4]);
        let mut reader = wav.as_slice();
        assert!(skip_wav_header(&mut reader).await.unwrap().is_empty());
        assert_eq!(reader, [1, 2, 3, 4]);

        // Raw PCM, even shorter than a RIFF preamble, is kept whole.
        let mut reader: &[u8] = &[9, 8, 7];
        assert_eq!(skip_wav_header(&mut reader).await.unwrap(), [9, 8, 7]);

        // A WAV file without a data chunk is rejected.
        let mut reader: &[u8] = b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0";
        assert!(skip_wav_header(&mut reader).await.is_err());
    }
}
