//! Stream readers for the snapserver-rs binary.

use std::time::Duration;

use snapcast_server::time::ChunkTimestamper;
use snapcast_server::{AudioData, AudioFrame, SampleFormat};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub(crate) mod file;
pub(crate) mod pipe;
pub(crate) mod process;
pub(crate) mod tcp;
pub(crate) mod uri;

/// A configured source reader, started by calling it with the stream's audio
/// sender.
pub(crate) type Reader = Box<dyn FnOnce(mpsc::Sender<AudioFrame>) -> JoinHandle<()>>;

/// Configure the reader for `uri` without starting it. The scheme and the
/// reader's parameters are checked here, so the caller registers the stream
/// only for a source that can be read: a rejected one would otherwise show
/// up in the status as a stream that never plays.
pub(crate) fn reader(
    uri: uri::StreamUri,
    format: SampleFormat,
    chunk_frames: usize,
) -> anyhow::Result<Reader> {
    Ok(match uri.scheme.as_str() {
        "pipe" => Box::new(pipe::reader(uri, format, chunk_frames)?),
        "file" => Box::new(file::reader(uri, format, chunk_frames)?),
        "process" => Box::new(process::reader(uri, format, chunk_frames)?),
        "tcp" => Box::new(tcp::reader(uri, format, chunk_frames)?),
        other => anyhow::bail!("unsupported stream scheme {other:?}"),
    })
}

/// Duration of `frames` frames at `format`'s rate.
fn frames_duration(format: SampleFormat, frames: usize) -> Duration {
    Duration::from_micros(frames as u64 * 1_000_000 / u64::from(format.rate()))
}

/// How long a reader waits before reopening, reconnecting or restarting its
/// source.
const RETRY_DELAY: Duration = Duration::from_secs(1);

/// How far a source may fall behind realtime before its timestamps are
/// re-anchored at the current time (see [`ChunkTimestamper::resync_if_behind`]).
const MAX_SOURCE_LAG_USEC: i64 = 200_000;

/// How samples are encoded in a source's raw byte stream, set with the
/// `encoding` source parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PcmEncoding {
    /// Signed little-endian integers of the stream's `sampleformat` bit
    /// depth (the default, as in C++ snapserver).
    Int,
    /// 32-bit little-endian floats (`encoding=float`, ffmpeg `-f f32le`),
    /// forwarded as f32 so encoders that work in float (Opus) skip the
    /// round trip through integer PCM. The `sampleformat` bit depth is then
    /// only what integer encoders (PCM, FLAC) convert to.
    Float,
    /// Packed 24-bit little-endian integers, three bytes per sample (as in
    /// WAV files), padded to the four bytes 24-bit PCM is sent in.
    Int24Packed,
}

impl PcmEncoding {
    /// Read the `encoding` parameter of a source URI.
    fn from_uri(uri: &uri::StreamUri) -> anyhow::Result<Self> {
        match uri.param("encoding") {
            None | Some("int") => Ok(Self::Int),
            Some("float") => Ok(Self::Float),
            Some(other) => anyhow::bail!("source encoding must be int or float, got {other:?}"),
        }
    }

    /// Bytes in one frame of a source with this encoding.
    fn frame_bytes(self, format: SampleFormat) -> usize {
        let channels = format.channels() as usize;
        match self {
            Self::Int => format.frame_size() as usize,
            Self::Float => channels * 4,
            Self::Int24Packed => channels * 3,
        }
    }

    fn audio_data(self, buf: &[u8]) -> AudioData {
        match self {
            Self::Int => AudioData::Pcm(buf.to_vec()),
            Self::Float => AudioData::F32(
                buf.as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| f32::from_le_bytes(*b))
                    .collect(),
            ),
            Self::Int24Packed => AudioData::Pcm(
                buf.as_chunks::<3>()
                    .0
                    .iter()
                    .flat_map(|&[a, b, c]| [a, b, c, if c & 0x80 != 0 { 0xFF } else { 0 }])
                    .collect(),
            ),
        }
    }
}

/// How a source's byte stream is cut into chunks: the sample format, the
/// frames per chunk and the sample encoding, parsed from the source URI the
/// same way by every reader.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Chunking {
    format: SampleFormat,
    frames: usize,
    bytes: usize,
    encoding: PcmEncoding,
}

impl Chunking {
    /// Chunks of `frames` frames of `format`, encoded as the URI's
    /// `encoding` parameter says.
    pub(crate) fn from_uri(
        uri: &uri::StreamUri,
        format: SampleFormat,
        frames: usize,
    ) -> anyhow::Result<Self> {
        Ok(Self::with_encoding(
            PcmEncoding::from_uri(uri)?,
            format,
            frames,
        ))
    }

    fn with_encoding(encoding: PcmEncoding, format: SampleFormat, frames: usize) -> Self {
        Self {
            format,
            frames,
            bytes: frames * encoding.frame_bytes(format),
            encoding,
        }
    }

    /// How long one chunk plays.
    pub(crate) fn duration(&self) -> Duration {
        frames_duration(self.format, self.frames)
    }

    /// The chunking for a WAV source whose `fmt ` chunk is `wav`: its
    /// encoding is what the file declares, which must match the source's
    /// sample format.
    pub(crate) fn for_wav(&self, wav: &file::WavFormat) -> anyhow::Result<Self> {
        let format = self.format;
        let declared = format!("{}:{}:{}", wav.rate, wav.bits, wav.channels);
        anyhow::ensure!(
            wav.rate == format.rate() && wav.channels == format.channels(),
            "WAV file is {declared}, but the source's sampleformat is {format}: \
             set sampleformat={}:{}:{} on the source",
            wav.rate,
            if wav.float { format.bits() } else { wav.bits },
            wav.channels
        );
        let channels = usize::from(wav.channels);
        let encoding = match (wav.float, wav.bits) {
            (true, 32) => PcmEncoding::Float,
            (false, bits) if bits == format.bits() => {
                match usize::from(wav.block_align) / channels.max(1) {
                    3 if bits == 24 => PcmEncoding::Int24Packed,
                    n if n == format.sample_size() as usize => PcmEncoding::Int,
                    _ => anyhow::bail!(
                        "unsupported WAV block alignment {} for {declared}",
                        wav.block_align
                    ),
                }
            }
            (false, _) => anyhow::bail!(
                "WAV file is {declared}, but the source's sampleformat is {format}: \
                 set sampleformat={declared} on the source"
            ),
            (true, bits) => anyhow::bail!("unsupported {bits}-bit float WAV file"),
        };
        Ok(Self::with_encoding(encoding, format, self.frames))
    }

    /// A timestamper anchored now, for a source that was just opened.
    pub(crate) fn timestamper(&self) -> ChunkTimestamper {
        ChunkTimestamper::new(self.format.rate())
    }
}

/// Why [`pump_pcm`] returned.
pub(crate) enum PumpEnd {
    /// The source ended (EOF / disconnect / process exit). The caller may
    /// reopen the source and pump again.
    SourceEnded {
        /// Chunks forwarded before the end.
        chunks: u64,
    },
    /// The audio channel was closed (the consumer is gone). The caller should
    /// stop and clean up.
    TxClosed,
}

/// Tell the encoder that the source ended (see `snapcast_server::AudioFrame`):
/// the audio it holds toward an incomplete packet is sent now, instead of
/// being held until the source restarts.
pub(crate) async fn flush_encoder(ts: &mut ChunkTimestamper, tx: &mpsc::Sender<AudioFrame>) {
    let frame = AudioFrame {
        timestamp_usec: ts.next(0),
        data: AudioData::Pcm(Vec::new()),
    };
    let _ = tx.send(frame).await;
}

/// Read fixed-size PCM chunks from `reader`, timestamp them, and forward to `tx`.
///
/// The shared inner loop of every stream reader: chunks are cut and decoded
/// as `chunking` says and timestamped via `ts`. Like C++ snapserver, every
/// source is read at realtime: a chunk is read no earlier than the
/// timestamp it gets. A source that delivers faster (a regular file, a
/// process or TCP peer writing as fast as it can, e.g. librespot) is held
/// back by backpressure instead of flooding the encoder and clients with
/// audio stamped ever further in the future; one that fell slightly behind
/// (a short stall) is read without waiting until it has caught up again.
///
/// The whole frames of a last, shorter chunk before the source ends are
/// forwarded too.
pub(crate) async fn pump_pcm<R: AsyncReadExt + Unpin>(
    reader: &mut R,
    ts: &mut ChunkTimestamper,
    chunking: &Chunking,
    tx: &mpsc::Sender<AudioFrame>,
) -> PumpEnd {
    let mut buf = vec![0u8; chunking.bytes];
    let frame_bytes = chunking.encoding.frame_bytes(chunking.format);
    let mut chunks = 0;
    // The timestamp the next chunk will get, unless the source falls behind.
    let mut next_due_usec = None;
    loop {
        if let Some(due) = next_due_usec {
            let ahead = due - snapcast_server::time::now_usec();
            if ahead > 0 {
                tokio::time::sleep(Duration::from_micros(ahead as u64)).await;
            }
        }
        let mut filled = 0;
        while filled < buf.len() {
            match reader.read(&mut buf[filled..]).await {
                Ok(0) | Err(_) => break,
                Ok(n) => filled += n,
            }
        }
        let ended = filled < buf.len();
        // A partial frame at the end is dropped, whole frames are not.
        let frames = filled / frame_bytes;
        if frames > 0 {
            // Audio that arrives later than realtime (late start, stall) is
            // stamped from now on, so clients don't discard it as too old.
            if let Some(lag) = ts.resync_if_behind(MAX_SOURCE_LAG_USEC) {
                tracing::info!(
                    lag_ms = lag / 1000,
                    "Source behind realtime, resyncing timestamps"
                );
            }
            let timestamp_usec = ts.next(frames as u32);
            next_due_usec =
                Some(timestamp_usec + frames_duration(chunking.format, frames).as_micros() as i64);
            let frame = AudioFrame {
                timestamp_usec,
                data: chunking.encoding.audio_data(&buf[..frames * frame_bytes]),
            };
            if tx.send(frame).await.is_err() {
                return PumpEnd::TxClosed;
            }
            chunks += 1;
        }
        if ended {
            return PumpEnd::SourceEnded { chunks };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    /// 48 kHz 16-bit stereo chunks of 960 frames (20 ms), encoded as
    /// `query` (appended to the URI) says.
    fn chunking(query: &str) -> Chunking {
        let uri = uri::StreamUri::parse(&format!("pipe:///f?name=t{query}")).unwrap();
        Chunking::from_uri(&uri, SampleFormat::new(48000, 16, 2), 960).unwrap()
    }

    /// A source whose first audio arrives well after it was opened (like
    /// ffmpeg with a lookahead filter) gets chunks stamped at arrival, not at
    /// open time, so clients don't drop them as too old.
    #[tokio::test]
    async fn late_source_is_stamped_at_arrival() {
        let (mut writer, mut reader) = tokio::io::duplex(64 * 1024);
        let chunking = chunking("");
        let mut ts = chunking.timestamper();
        let (tx, mut rx) = mpsc::channel(8);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(600)).await;
            writer.write_all(&[0u8; 960 * 4]).await.unwrap();
            // Keep the source open so the pump waits instead of ending.
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        tokio::spawn(async move { pump_pcm(&mut reader, &mut ts, &chunking, &tx).await });

        let frame = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let age_ms = (snapcast_server::time::now_usec() - frame.timestamp_usec) / 1000;
        assert!(age_ms < 100, "chunk stamped {age_ms} ms in the past");
    }

    /// Regression: only pipe and file sources were paced, so a process or
    /// TCP source writing faster than realtime (librespot, ffmpeg without
    /// `-re`) was read as fast as it wrote, stamped ever further ahead.
    #[tokio::test]
    async fn fast_source_is_read_at_realtime() {
        // 10 s of audio available at once.
        let data = vec![0u8; 48000 * 4 * 10];
        let mut reader = data.as_slice();
        let chunking = chunking("");
        let mut ts = chunking.timestamper();
        let (tx, mut rx) = mpsc::channel(1024);
        let _ = tokio::time::timeout(
            Duration::from_millis(500),
            pump_pcm(&mut reader, &mut ts, &chunking, &tx),
        )
        .await;
        let mut chunks = 0;
        while let Ok(frame) = rx.try_recv() {
            chunks += 1;
            let ahead_ms = (frame.timestamp_usec - snapcast_server::time::now_usec()) / 1000;
            assert!(ahead_ms <= 20, "chunk stamped {ahead_ms} ms ahead");
        }
        // 500 ms of 20 ms chunks is 25; unpaced it would be all 500 at once.
        assert!((15..=40).contains(&chunks), "{chunks} chunks in 500 ms");
    }

    /// Regression: after a short stall, reads went on at realtime from the
    /// stall's end, so the audio fell further behind its timestamps with
    /// every stall until a resync cut a gap into it. Now the backlog is read
    /// at once and the timestamps stay contiguous.
    #[tokio::test]
    async fn short_stall_is_caught_up() {
        let (mut writer, mut reader) = tokio::io::duplex(8 * 1024 * 1024);
        let chunking = chunking("");
        let mut ts = chunking.timestamper();
        let (tx, mut rx) = mpsc::channel(1024);
        tokio::spawn(async move {
            // 200 ms of audio, then nothing for 150 ms, then plenty.
            writer.write_all(&[0u8; 9600 * 4]).await.unwrap();
            tokio::time::sleep(Duration::from_millis(350)).await;
            writer.write_all(&vec![0u8; 48000 * 4 * 10]).await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let _ = tokio::time::timeout(
            Duration::from_millis(1000),
            pump_pcm(&mut reader, &mut ts, &chunking, &tx),
        )
        .await;
        let mut stamps = Vec::new();
        while let Ok(frame) = rx.try_recv() {
            stamps.push(frame.timestamp_usec);
        }
        // 1 s of 20 ms chunks is 50; without catching up it was 42.
        assert!((47..=53).contains(&stamps.len()), "{} chunks", stamps.len());
        assert!(stamps.windows(2).all(|w| w[1] - w[0] == 20_000), "gap");
    }

    /// Regression: the last, shorter chunk was dropped whole at EOF.
    #[tokio::test]
    async fn last_partial_chunk_keeps_its_whole_frames() {
        // 1060 frames and a stray byte, in 960-frame chunks.
        let data = vec![0u8; 1060 * 4 + 1];
        let mut reader = data.as_slice();
        let chunking = chunking("");
        let mut ts = chunking.timestamper();
        let (tx, mut rx) = mpsc::channel(8);
        let end = pump_pcm(&mut reader, &mut ts, &chunking, &tx).await;
        assert!(matches!(end, PumpEnd::SourceEnded { chunks: 2 }));
        let mut lens = Vec::new();
        let mut stamps = Vec::new();
        while let Ok(frame) = rx.try_recv() {
            let AudioData::Pcm(data) = frame.data else {
                panic!("expected PCM");
            };
            lens.push(data.len());
            stamps.push(frame.timestamp_usec);
        }
        assert_eq!(lens, [960 * 4, 100 * 4]);
        assert_eq!(stamps[1] - stamps[0], 20_000);
    }

    #[test]
    fn packed_24_bit_is_padded_and_sign_extended() {
        let data = PcmEncoding::Int24Packed.audio_data(&[0x01, 0x02, 0x03, 0xFF, 0xFF, 0xFF]);
        let AudioData::Pcm(pcm) = data else {
            panic!("expected PCM");
        };
        assert_eq!(pcm, [0x01, 0x02, 0x03, 0x00, 0xFF, 0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn encoding_parameter() {
        let enc = |q: &str| {
            PcmEncoding::from_uri(&uri::StreamUri::parse(&format!("pipe:///f?name=t{q}")).unwrap())
        };
        assert_eq!(enc("").unwrap(), PcmEncoding::Int);
        assert_eq!(enc("&encoding=int").unwrap(), PcmEncoding::Int);
        assert_eq!(enc("&encoding=float").unwrap(), PcmEncoding::Float);
        assert!(enc("&encoding=f32").is_err());
    }

    /// Float chunks are 4 bytes per sample whatever the `sampleformat` bit
    /// depth, and arrive as f32 samples.
    #[tokio::test]
    async fn float_source_is_forwarded_as_f32() {
        let chunking = chunking("&encoding=float");
        assert_eq!(chunking.bytes, 960 * 2 * 4);
        let samples: Vec<f32> = (0..960 * 2).map(|i| i as f32 / 4096.0 - 0.25).collect();
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut reader = bytes.as_slice();
        let mut ts = chunking.timestamper();
        let (tx, mut rx) = mpsc::channel(8);
        pump_pcm(&mut reader, &mut ts, &chunking, &tx).await;
        let AudioData::F32(got) = rx.recv().await.unwrap().data else {
            panic!("expected f32 data");
        };
        assert_eq!(got, samples);
    }
}
