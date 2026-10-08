//! Process stream reader — captures stdout PCM from a child process.

use anyhow::Result;
use snapcast_proto::SampleFormat;
use snapcast_server::AudioFrame;
use snapcast_server::time::ChunkTimestamper;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::uri::StreamUri;
use super::{PumpEnd, pump_pcm};

/// Start a child process and read PCM from its stdout.
pub fn start(
    uri: StreamUri,
    format: SampleFormat,
    chunk_frames: usize,
    tx: mpsc::Sender<AudioFrame>,
) -> Result<JoinHandle<()>> {
    let path = uri.path.clone();
    let params = uri.param("params").unwrap_or("").to_string();
    let chunk_bytes = chunk_frames * format.frame_size() as usize;

    Ok(tokio::spawn(async move {
        let mut ts = ChunkTimestamper::new(format.rate());
        loop {
            tracing::info!(path, params, "Starting process stream");
            let mut child = match Command::new(&path)
                .args(params.split_whitespace())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .spawn()
            {
                Ok(child) => child,
                Err(e) => {
                    tracing::error!(path, error = %e, "Failed to start process");
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    continue;
                }
            };

            let Some(mut stdout) = child.stdout.take() else {
                let _ = child.kill().await;
                continue;
            };

            if let PumpEnd::TxClosed =
                pump_pcm(&mut stdout, &mut ts, chunk_frames, chunk_bytes, &tx, None).await
            {
                let _ = child.kill().await;
                return;
            }

            let _ = child.kill().await;
            tracing::info!(path, "Process exited, restarting");
            ts.reset();
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }))
}
