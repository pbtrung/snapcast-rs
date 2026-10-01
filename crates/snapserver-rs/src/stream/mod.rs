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
        let frame = AudioFrame {
            timestamp_usec: ts.next(chunk_frames as u32),
            data: AudioData::Pcm(buf.clone()),
        };
        if tx.send(frame).await.is_err() {
            return PumpEnd::TxClosed;
        }
    }
}
