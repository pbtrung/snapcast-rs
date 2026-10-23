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
///
/// A WAV file must hold the source's sample rate and channel count, as
/// 16-bit, 24-bit (packed or padded to 4 bytes) or 32-bit integer PCM of the
/// source's bit depth, or as 32-bit float. A file that exists already is
/// checked here, so a mismatch is reported at startup.
pub fn reader(
    uri: StreamUri,
    format: SampleFormat,
    chunk_frames: usize,
) -> Result<impl FnOnce(mpsc::Sender<AudioFrame>) -> JoinHandle<()>> {
    let path = uri.path.clone();
    let chunking = Chunking::from_uri(&uri, format, chunk_frames)?;
    if let Some(wav) = wav_format_of(&path)? {
        chunking
            .for_wav(&wav)
            .map_err(|e| anyhow::anyhow!("{path}: {e}"))?;
    }

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
                        let header = read_wav_header(&mut file).await.and_then(|h| {
                            let file_chunking = match &h.format {
                                Some(wav) => {
                                    chunking.for_wav(wav).map_err(std::io::Error::other)?
                                }
                                None => chunking,
                            };
                            Ok((h, file_chunking))
                        });
                        match header {
                            Ok((WavHeader { prefix, len, .. }, file_chunking)) => match pump_pcm(
                                &mut prefix.as_slice().chain((&mut file).take(len)),
                                &mut ts,
                                &file_chunking,
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

/// The sample layout a WAV file's `fmt ` chunk declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WavFormat {
    /// IEEE float samples (else integer PCM).
    pub float: bool,
    pub channels: u16,
    pub rate: u32,
    pub bits: u16,
    /// Bytes per frame.
    pub block_align: u16,
}

/// What [`read_wav_header`] found before the audio.
#[derive(Debug, PartialEq, Eq)]
struct WavHeader {
    /// Bytes already read from a file that is not a WAV file: they are raw
    /// PCM and must not be dropped, or every frame after them would be
    /// misaligned.
    prefix: Vec<u8>,
    /// Bytes of audio that follow: the `data` chunk's size, so chunks after
    /// it (`LIST`, `id3 `) aren't played as noise, or unlimited for raw PCM
    /// and a WAV header written without a size (0 or 0xFFFFFFFF, as when
    /// streaming).
    len: u64,
    /// The declared format; `None` for raw PCM.
    format: Option<WavFormat>,
}

const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

/// Read a WAV file's header: the RIFF/WAVE preamble and every chunk before
/// the `data` chunk (`fmt `, `LIST`, `fact`, ...), so playback starts at the
/// first sample whatever the header's length. The `fmt ` chunk is parsed;
/// a WAV file without one, or with an encoding other than integer PCM or
/// float, is an error.
async fn read_wav_header<R: AsyncRead + Unpin>(file: &mut R) -> std::io::Result<WavHeader> {
    let invalid = |msg: String| std::io::Error::new(std::io::ErrorKind::InvalidData, msg);
    let mut preamble = Vec::with_capacity(12);
    (&mut *file).take(12).read_to_end(&mut preamble).await?;
    if preamble.len() < 12 || &preamble[..4] != b"RIFF" || &preamble[8..] != b"WAVE" {
        return Ok(WavHeader {
            prefix: preamble,
            len: u64::MAX,
            format: None,
        });
    }
    let mut format = None;
    loop {
        let mut header = [0u8; 8];
        file.read_exact(&mut header).await?;
        let size = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
        if &header[..4] == b"data" {
            let len = match size {
                0 | u32::MAX => u64::MAX,
                size => u64::from(size),
            };
            let format = format.ok_or_else(|| invalid("no fmt chunk before the data".into()))?;
            return Ok(WavHeader {
                prefix: Vec::new(),
                len,
                format: Some(format),
            });
        }
        // Chunks are padded to an even length.
        let size = u64::from(size);
        let mut skip = size + (size & 1);
        if &header[..4] == b"fmt " {
            if size < 16 {
                return Err(invalid(format!("fmt chunk too small ({size} bytes)")));
            }
            // Up to the extensible format's sub-format code.
            let mut fmt = vec![0u8; size.min(26) as usize];
            file.read_exact(&mut fmt).await?;
            skip -= fmt.len() as u64;
            let u16_at = |at: usize| u16::from_le_bytes([fmt[at], fmt[at + 1]]);
            let mut code = u16_at(0);
            if code == WAVE_FORMAT_EXTENSIBLE && fmt.len() >= 26 {
                code = u16_at(24);
            }
            let float = match code {
                WAVE_FORMAT_PCM => false,
                WAVE_FORMAT_IEEE_FLOAT => true,
                other => {
                    return Err(invalid(format!(
                        "unsupported WAV encoding {other:#06x} (integer PCM or float only)"
                    )));
                }
            };
            format = Some(WavFormat {
                float,
                channels: u16_at(2),
                rate: u32::from_le_bytes([fmt[4], fmt[5], fmt[6], fmt[7]]),
                block_align: u16_at(12),
                bits: u16_at(14),
            });
        }
        let skipped = tokio::io::copy(&mut (&mut *file).take(skip), &mut tokio::io::sink()).await?;
        if skipped < skip {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
    }
}

/// The format a WAV file at `path` declares, if it exists and is one.
fn wav_format_of(path: &str) -> Result<Option<WavFormat>> {
    use futures_util::FutureExt;
    use std::io::Read;
    let Ok(file) = std::fs::File::open(path) else {
        // Not there yet: checked once the reader opens it.
        return Ok(None);
    };
    // A header is far smaller; a slice reader is always ready.
    let mut head = Vec::new();
    file.take(1 << 20).read_to_end(&mut head)?;
    let header = read_wav_header(&mut head.as_slice())
        .now_or_never()
        .expect("reading a slice never waits");
    match header {
        Ok(header) => Ok(header.format),
        // A header longer than what was read: checked when opened.
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
        Err(e) => Err(anyhow::anyhow!("{path}: invalid WAV file: {e}")),
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
        let mut wav = b"RIFF\0\0\0\0WAVE".to_vec();
        wav.extend_from_slice(&fmt_chunk(1, 1, 48000, 16));
        wav.extend_from_slice(b"data\x04\0\0\0");
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
        wav.extend_from_slice(&fmt_chunk(1, 2, 44100, 16));
        wav.extend_from_slice(b"LIST\x03\0\0\0abc\0");
        wav.extend_from_slice(b"data\x04\0\0\0");
        wav.extend_from_slice(&[1, 2, 3, 4]);
        let mut reader = wav.as_slice();
        let header = read_wav_header(&mut reader).await.unwrap();
        assert_eq!((header.prefix, header.len), (vec![], 4));
        assert_eq!(
            header.format,
            Some(WavFormat {
                float: false,
                channels: 2,
                rate: 44100,
                bits: 16,
                block_align: 4,
            })
        );
        assert_eq!(reader, [1, 2, 3, 4]);

        // A data chunk without a size (as streamed) runs to the end.
        let mut wav = b"RIFF\0\0\0\0WAVE".to_vec();
        wav.extend_from_slice(&fmt_chunk(1, 2, 48000, 16));
        wav.extend_from_slice(b"data\xff\xff\xff\xff\x01\x02");
        assert_eq!(
            read_wav_header(&mut wav.as_slice()).await.unwrap().len,
            u64::MAX
        );

        // Raw PCM, even shorter than a RIFF preamble, is kept whole.
        let mut reader: &[u8] = &[9, 8, 7];
        let header = read_wav_header(&mut reader).await.unwrap();
        assert_eq!(
            (header.prefix, header.len, header.format),
            (vec![9, 8, 7], u64::MAX, None)
        );

        // A WAV file without a data chunk, or without a fmt chunk, or of a
        // compressed format is rejected.
        let mut reader: &[u8] = b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0";
        assert!(read_wav_header(&mut reader).await.is_err());
        let mut reader: &[u8] = b"RIFF\0\0\0\0WAVEdata\x04\0\0\0\x01\x02\x03\x04";
        assert!(read_wav_header(&mut reader).await.is_err());
        let mut wav = b"RIFF\0\0\0\0WAVE".to_vec();
        wav.extend_from_slice(&fmt_chunk(2, 2, 48000, 4)); // ADPCM
        wav.extend_from_slice(b"data\0\0\0\0");
        assert!(read_wav_header(&mut wav.as_slice()).await.is_err());
    }

    /// A `fmt ` chunk declaring `code` (1 PCM, 3 float).
    fn fmt_chunk(code: u16, channels: u16, rate: u32, bits: u16) -> Vec<u8> {
        let block_align = channels * bits.div_ceil(8);
        let mut c = b"fmt \x10\0\0\0".to_vec();
        c.extend_from_slice(&code.to_le_bytes());
        c.extend_from_slice(&channels.to_le_bytes());
        c.extend_from_slice(&rate.to_le_bytes());
        c.extend_from_slice(&(rate * u32::from(block_align)).to_le_bytes());
        c.extend_from_slice(&block_align.to_le_bytes());
        c.extend_from_slice(&bits.to_le_bytes());
        c
    }

    fn wav_file(dir: &tempfile::TempDir, fmt: Vec<u8>, data: &[u8]) -> StreamUri {
        let path = dir.path().join("f.wav");
        let mut wav = b"RIFF\0\0\0\0WAVE".to_vec();
        wav.extend_from_slice(&fmt);
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
        wav.extend_from_slice(data);
        std::fs::write(&path, wav).unwrap();
        StreamUri::parse(&format!("file://{}?name=f", path.display())).unwrap()
    }

    /// Regression: the `fmt ` chunk was skipped, so a 44.1 kHz file played at
    /// the wrong speed under a 48 kHz source format, and float or packed
    /// 24-bit files as noise.
    #[test]
    fn mismatched_wav_format_is_rejected_at_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let format = SampleFormat::new(48000, 16, 2);
        let uri = wav_file(&dir, fmt_chunk(1, 2, 44100, 16), &[]);
        let err = reader(uri, format, 960).err().unwrap().to_string();
        assert!(err.contains("sampleformat=44100:16:2"), "{err}");
        let uri = wav_file(&dir, fmt_chunk(1, 1, 48000, 16), &[]);
        assert!(reader(uri, format, 960).is_err());
        let uri = wav_file(&dir, fmt_chunk(1, 2, 48000, 24), &[]);
        assert!(reader(uri, format, 960).is_err());
        let uri = wav_file(&dir, fmt_chunk(1, 2, 48000, 16), &[]);
        assert!(reader(uri, format, 960).is_ok());
    }

    #[tokio::test]
    async fn float_and_packed_24_bit_wav_files_are_converted() {
        let dir = tempfile::tempdir().unwrap();
        let uri = wav_file(&dir, fmt_chunk(3, 1, 48000, 32), &0.5f32.to_le_bytes());
        let (tx, mut rx) = mpsc::channel(4);
        let handle = reader(uri, SampleFormat::new(48000, 16, 1), 1).unwrap()(tx);
        let frame = rx.recv().await.unwrap();
        handle.abort();
        assert!(matches!(frame.data, snapcast_server::AudioData::F32(s) if s == [0.5]));

        let dir = tempfile::tempdir().unwrap();
        let uri = wav_file(&dir, fmt_chunk(1, 1, 48000, 24), &[0xFE, 0xFF, 0xFF]);
        let (tx, mut rx) = mpsc::channel(4);
        let handle = reader(uri, SampleFormat::new(48000, 24, 1), 1).unwrap()(tx);
        let frame = rx.recv().await.unwrap();
        handle.abort();
        let snapcast_server::AudioData::Pcm(pcm) = frame.data else {
            panic!("expected PCM");
        };
        assert_eq!(i32::from_le_bytes(pcm.try_into().unwrap()), -2);
    }
}
