//! File stream reader — reads PCM/WAV from a file, loops on EOF.

use anyhow::Result;
use snapcast_proto::SampleFormat;
use snapcast_server::AudioFrame;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::uri::StreamUri;
use super::{Chunking, PumpEnd, RETRY_DELAY, pump_pcm};

/// Configure reading PCM from a file, looping on EOF; the returned closure
/// starts it.
pub fn reader(
    uri: StreamUri,
    format: SampleFormat,
    chunk_frames: usize,
) -> Result<impl FnOnce(mpsc::Sender<AudioFrame>) -> JoinHandle<()>> {
    let path = uri.path.clone();
    let chunking = Chunking::from_uri(&uri, format, chunk_frames)?;

    Ok(move |tx: mpsc::Sender<AudioFrame>| {
        tokio::spawn(async move {
            let mut ts = chunking.timestamper();
            let mut opened = false;
            loop {
                match tokio::fs::File::open(&path).await {
                    Ok(mut file) => {
                        if !opened {
                            tracing::info!(path, "File stream opened");
                            opened = true;
                        }
                        match skip_wav_header(&mut file).await {
                            Ok((prefix, len)) => match pump_pcm(
                                &mut prefix.as_slice().chain((&mut file).take(len)),
                                &mut ts,
                                &chunking,
                                &tx,
                            )
                            .await
                            {
                                // EOF: play it again from the start, without
                                // a gap and with contiguous timestamps.
                                PumpEnd::SourceEnded { chunks } if chunks > 0 => continue,
                                // Too short to hold a chunk: don't spin on it.
                                PumpEnd::SourceEnded { .. } => {}
                                PumpEnd::TxClosed => return,
                            },
                            Err(e) => {
                                tracing::warn!(path, error = %e, "Invalid WAV file, retrying");
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(path, error = %e, "Cannot open file, retrying");
                        opened = false;
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
/// first sample whatever the header's length.
///
/// Returns the bytes already read from a file that is not a WAV file (they
/// are raw PCM and must not be dropped, or every frame after them would be
/// misaligned), and how many bytes of audio follow: the `data` chunk's
/// size, so chunks after it (`LIST`, `id3 `) aren't played as noise, or
/// unlimited for raw PCM and a WAV header written without a size (0 or
/// 0xFFFFFFFF, as when streaming).
async fn skip_wav_header<R: AsyncRead + Unpin>(file: &mut R) -> std::io::Result<(Vec<u8>, u64)> {
    let mut preamble = Vec::with_capacity(12);
    (&mut *file).take(12).read_to_end(&mut preamble).await?;
    if preamble.len() < 12 || &preamble[..4] != b"RIFF" || &preamble[8..] != b"WAVE" {
        return Ok((preamble, u64::MAX));
    }
    loop {
        let mut header = [0u8; 8];
        file.read_exact(&mut header).await?;
        let size = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
        if &header[..4] == b"data" {
            let len = match size {
                0 | u32::MAX => u64::MAX,
                size => u64::from(size),
            };
            return Ok((Vec::new(), len));
        }
        // Chunks are padded to an even length.
        let size = u64::from(size);
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

    /// Regression: the file was reopened a second after EOF, leaving a gap
    /// on every loop, and a chunk after the WAV `data` chunk (here `LIST`)
    /// was played as audio.
    #[tokio::test]
    async fn wav_file_loops_without_a_gap_or_trailing_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("loop.wav");
        let mut wav = b"RIFF\0\0\0\0WAVEdata\x04\0\0\0".to_vec();
        wav.extend_from_slice(&[1, 2, 3, 4]);
        wav.extend_from_slice(b"LIST\x04\0\0\0abcd");
        std::fs::write(&path, wav).unwrap();
        let uri = StreamUri::parse(&format!("file://{}?name=f", path.display())).unwrap();
        let (tx, mut rx) = mpsc::channel(4);
        // 48 kHz 16-bit mono, 2-frame chunks: the file holds one chunk.
        let handle = reader(uri, SampleFormat::new(48000, 16, 1), 2).unwrap()(tx);
        let mut frames = Vec::new();
        for _ in 0..3 {
            let frame = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv())
                .await
                .expect("next loop came late")
                .unwrap();
            frames.push(frame);
        }
        handle.abort();
        for frame in &frames {
            let snapcast_server::AudioData::Pcm(data) = &frame.data else {
                panic!("expected PCM");
            };
            assert_eq!(data, &[1, 2, 3, 4]);
        }
        // Each loop follows the last one by one chunk duration.
        let stamps: Vec<i64> = frames.iter().map(|f| f.timestamp_usec).collect();
        assert!(stamps.windows(2).all(|w| w[1] - w[0] < 1000), "{stamps:?}");
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
        assert_eq!(skip_wav_header(&mut reader).await.unwrap(), (vec![], 4));
        assert_eq!(reader, [1, 2, 3, 4]);

        // A data chunk without a size (as streamed) runs to the end.
        let mut reader: &[u8] = b"RIFF\0\0\0\0WAVEdata\xff\xff\xff\xff\x01\x02";
        assert_eq!(skip_wav_header(&mut reader).await.unwrap().1, u64::MAX);

        // Raw PCM, even shorter than a RIFF preamble, is kept whole.
        let mut reader: &[u8] = &[9, 8, 7];
        assert_eq!(
            skip_wav_header(&mut reader).await.unwrap(),
            (vec![9, 8, 7], u64::MAX)
        );

        // A WAV file without a data chunk is rejected.
        let mut reader: &[u8] = b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0";
        assert!(skip_wav_header(&mut reader).await.is_err());
    }
}
