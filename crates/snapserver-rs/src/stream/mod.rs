//! Stream readers for the snapserver-rs binary.

use std::time::Duration;

use snapcast_server::time::ChunkTimestamper;
use snapcast_server::{AudioData, AudioFrame};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;

pub(crate) mod airplay;
pub(crate) mod file;
pub(crate) mod librespot;
pub(crate) mod pipe;
pub(crate) mod process;
pub(crate) mod tcp;
pub(crate) mod uri;

/// How far a source may fall behind realtime before its timestamps are
/// re-anchored at the current time (see [`ChunkTimestamper::resync_if_behind`]).
const MAX_SOURCE_LAG_USEC: i64 = 200_000;

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
/// long (= `chunk_frames` frames) and is timestamped via `ts`. When `pace` is
/// `Some`, reads are rate-limited to that interval — used by the file reader so
/// a finite file plays back in real time; the other sources (pipe, socket,
/// child stdout) block naturally and pass `None`.
pub(crate) async fn pump_pcm<R: AsyncReadExt + Unpin>(
    reader: &mut R,
    ts: &mut ChunkTimestamper,
    chunk_frames: usize,
    chunk_bytes: usize,
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
            data: AudioData::Pcm(buf.clone()),
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
        tokio::spawn(async move { pump_pcm(&mut reader, &mut ts, 960, 960 * 4, &tx, None).await });

        let frame = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let age_ms = (snapcast_server::time::now_usec() - frame.timestamp_usec) / 1000;
        assert!(age_ms < 100, "chunk stamped {age_ms} ms in the past");
    }
}
