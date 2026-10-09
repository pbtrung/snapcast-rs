//! Stream readers for the snapserver-rs binary.

use std::time::Duration;

use snapcast_server::time::ChunkTimestamper;
use snapcast_server::{AudioData, AudioFrame};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;

pub(crate) mod file;
pub(crate) mod pipe;
pub(crate) mod process;
pub(crate) mod tcp;
pub(crate) mod uri;

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
}

impl PcmEncoding {
    /// Read the `encoding` parameter of a source URI.
    pub(crate) fn from_uri(uri: &uri::StreamUri) -> anyhow::Result<Self> {
        match uri.param("encoding") {
            None | Some("int") => Ok(Self::Int),
            Some("float") => Ok(Self::Float),
            Some(other) => anyhow::bail!("source encoding must be int or float, got {other:?}"),
        }
    }

    /// Bytes in `frames` frames of a source with this encoding.
    pub(crate) fn chunk_bytes(self, format: snapcast_server::SampleFormat, frames: usize) -> usize {
        match self {
            Self::Int => frames * format.frame_size() as usize,
            Self::Float => frames * format.channels() as usize * 4,
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
        }
    }
}

/// Why [`pump_pcm`] returned.
pub(crate) enum PumpEnd {
    /// The source ended (EOF / disconnect / process exit). The caller may
    /// reopen the source and pump again.
    SourceEnded,
    /// The audio channel was closed (the consumer is gone). The caller should
    /// stop and clean up.
    TxClosed,
}

/// Read fixed-size PCM chunks from `reader`, timestamp them, and forward to `tx`.
///
/// The shared inner loop of every stream reader: each chunk is `chunk_bytes`
/// long (= `chunk_frames` frames, see [`PcmEncoding::chunk_bytes`]), decoded
/// per `encoding` and timestamped via `ts`. When `pace` is
/// `Some`, reads are rate-limited to that interval — used by the file reader so
/// a finite file plays back in real time; the other sources (pipe, socket,
/// child stdout) block naturally and pass `None`.
pub(crate) async fn pump_pcm<R: AsyncReadExt + Unpin>(
    reader: &mut R,
    ts: &mut ChunkTimestamper,
    chunk_frames: usize,
    chunk_bytes: usize,
    encoding: PcmEncoding,
    tx: &mpsc::Sender<AudioFrame>,
    pace: Option<Duration>,
) -> PumpEnd {
    let mut buf = vec![0u8; chunk_bytes];
    let mut interval = pace.map(|period| {
        let mut iv = tokio::time::interval(period);
        // After a stall (e.g. a writer pausing), continue at realtime
        // instead of bursting the missed reads.
        iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        iv
    });
    loop {
        if let Some(iv) = interval.as_mut() {
            iv.tick().await;
        }
        if reader.read_exact(&mut buf).await.is_err() {
            return PumpEnd::SourceEnded;
        }
        // Audio that arrives later than realtime (late start, stall) is
        // stamped from now on, so clients don't discard it as too old.
        if let Some(lag) = ts.resync_if_behind(MAX_SOURCE_LAG_USEC) {
            tracing::info!(
                lag_ms = lag / 1000,
                "Source behind realtime, resyncing timestamps"
            );
        }
        let frame = AudioFrame {
            timestamp_usec: ts.next(chunk_frames as u32),
            data: encoding.audio_data(&buf),
        };
        if tx.send(frame).await.is_err() {
            return PumpEnd::TxClosed;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    /// A source whose first audio arrives well after it was opened (like
    /// ffmpeg with a lookahead filter) gets chunks stamped at arrival, not at
    /// open time, so clients don't drop them as too old.
    #[tokio::test]
    async fn late_source_is_stamped_at_arrival() {
        let (mut writer, mut reader) = tokio::io::duplex(64 * 1024);
        let mut ts = ChunkTimestamper::new(48000);
        let (tx, mut rx) = mpsc::channel(8);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(600)).await;
            writer.write_all(&[0u8; 960 * 4]).await.unwrap();
            // Keep the source open so the pump waits instead of ending.
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        tokio::spawn(async move {
            pump_pcm(
                &mut reader,
                &mut ts,
                960,
                960 * 4,
                PcmEncoding::Int,
                &tx,
                None,
            )
            .await
        });

        let frame = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let age_ms = (snapcast_server::time::now_usec() - frame.timestamp_usec) / 1000;
        assert!(age_ms < 100, "chunk stamped {age_ms} ms in the past");
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
        let format = snapcast_server::SampleFormat::new(48000, 16, 2);
        let chunk_bytes = PcmEncoding::Float.chunk_bytes(format, 960);
        assert_eq!(chunk_bytes, 960 * 2 * 4);
        let samples: Vec<f32> = (0..960 * 2).map(|i| i as f32 / 4096.0 - 0.25).collect();
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut reader = bytes.as_slice();
        let mut ts = ChunkTimestamper::new(48000);
        let (tx, mut rx) = mpsc::channel(8);
        pump_pcm(
            &mut reader,
            &mut ts,
            960,
            chunk_bytes,
            PcmEncoding::Float,
            &tx,
            None,
        )
        .await;
        let AudioData::F32(got) = rx.recv().await.unwrap().data else {
            panic!("expected f32 data");
        };
        assert_eq!(got, samples);
    }
}
