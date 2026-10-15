//! Process stream reader — captures stdout PCM from a child process.

use anyhow::Result;
use snapcast_proto::SampleFormat;
use snapcast_server::AudioFrame;
use snapcast_server::time::ChunkTimestamper;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::uri::StreamUri;
use super::{PcmEncoding, PumpEnd, RETRY_DELAY, pump_pcm};

/// Configure a child process whose stdout PCM is read, restarted whenever
/// it exits; the returned closure starts it.
pub fn reader(
    uri: StreamUri,
    format: SampleFormat,
    chunk_frames: usize,
) -> Result<impl FnOnce(mpsc::Sender<AudioFrame>) -> JoinHandle<()>> {
    let path = uri.path.clone();
    let params = uri.param("params").unwrap_or("").to_string();
    let args = split_args(&params);
    let encoding = PcmEncoding::from_uri(&uri)?;
    let chunk_bytes = encoding.chunk_bytes(format, chunk_frames);

    Ok(move |tx: mpsc::Sender<AudioFrame>| {
        tokio::spawn(async move {
            let mut ts = ChunkTimestamper::new(format.rate());
            loop {
                tracing::info!(path, params, "Starting process stream");
                let mut child = match Command::new(&path)
                    .args(&args)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::null())
                    .kill_on_drop(true)
                    .spawn()
                {
                    Ok(child) => child,
                    Err(e) => {
                        tracing::error!(path, error = %e, "Failed to start process");
                        tokio::time::sleep(RETRY_DELAY).await;
                        continue;
                    }
                };

                let Some(mut stdout) = child.stdout.take() else {
                    let _ = child.kill().await;
                    continue;
                };

                if let PumpEnd::TxClosed = pump_pcm(
                    &mut stdout,
                    &mut ts,
                    chunk_frames,
                    chunk_bytes,
                    encoding,
                    &tx,
                    None,
                )
                .await
                {
                    let _ = child.kill().await;
                    return;
                }

                let _ = child.kill().await;
                tracing::info!(path, "Process exited, restarting");
                ts.reset();
                tokio::time::sleep(RETRY_DELAY).await;
            }
        })
    })
}

/// Split the `params` source parameter into arguments at whitespace, as C++
/// snapserver's command line parsing does: double quotes group an argument
/// containing spaces, e.g. `--name "Living Room"`, and are removed. Single
/// quotes are kept as they are (`Bob's`).
fn split_args(params: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut arg = String::new();
    let mut in_arg = false;
    let mut quoted = false;
    for c in params.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                in_arg = true;
            }
            c if quoted => arg.push(c),
            c if c.is_whitespace() => {
                if in_arg {
                    args.push(std::mem::take(&mut arg));
                    in_arg = false;
                }
            }
            c => {
                arg.push(c);
                in_arg = true;
            }
        }
    }
    if in_arg {
        args.push(arg);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: params were split at every space, so a quoted argument
    /// such as a device name with a space reached the process in pieces.
    #[test]
    fn split_args_honors_quotes() {
        assert_eq!(
            split_args(r#" --name "Living Room"  -b 320 --x="a b"c "" Bob's"#),
            [
                "--name",
                "Living Room",
                "-b",
                "320",
                "--x=a bc",
                "",
                "Bob's"
            ]
        );
        assert!(split_args("   ").is_empty());
        // An unterminated quote runs to the end.
        assert_eq!(split_args(r#"a "b c"#), ["a", "b c"]);
    }
}
