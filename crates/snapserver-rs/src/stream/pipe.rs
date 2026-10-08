//! Pipe stream reader — reads PCM from a named pipe (FIFO).
//!
//! Like C++ snapserver: with `mode=create` (the default) the FIFO is created
//! when missing, `mode=read` only opens an existing one, and reads are paced
//! at realtime so a writer that delivers faster than realtime (or a regular
//! file at the path) cannot flood the clients.

use anyhow::Result;
use snapcast_proto::SampleFormat;
use snapcast_server::AudioFrame;
use snapcast_server::time::ChunkTimestamper;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::uri::StreamUri;
use super::{PumpEnd, pump_pcm};

/// Start reading PCM from a named pipe.
pub fn start(
    uri: StreamUri,
    format: SampleFormat,
    chunk_frames: usize,
    tx: mpsc::Sender<AudioFrame>,
) -> Result<JoinHandle<()>> {
    let path = uri.path.clone();
    let create = match uri.param("mode") {
        None | Some("create") => true,
        Some("read") => false,
        Some(other) => anyhow::bail!("pipe source mode must be create or read, got {other:?}"),
    };
    let chunk_bytes = chunk_frames * format.frame_size() as usize;
    let chunk_duration =
        std::time::Duration::from_micros((chunk_frames as u64 * 1_000_000) / format.rate() as u64);

    Ok(tokio::spawn(async move {
        let mut warned_not_fifo = false;
        loop {
            let is_fifo = match prepare_fifo(&path, create) {
                Ok(is_fifo) => is_fifo,
                Err(e) => {
                    tracing::warn!(path, error = %e, "Could not create FIFO");
                    true
                }
            };
            if !is_fifo && !warned_not_fifo && std::path::Path::new(&path).exists() {
                warned_not_fifo = true;
                tracing::warn!(
                    path,
                    "Pipe source path is a regular file, not a FIFO (created by a writer that \
                     started before snapserver?); following it like `tail -f`, but it grows \
                     without bound. Remove it and start snapserver first so it creates the FIFO"
                );
            }
            // A regular file is followed, so EOF never replays it from the
            // start. A FIFO that hits EOF (writer gone, non-Linux) is reopened.
            let opened: std::io::Result<Box<dyn tokio::io::AsyncRead + Unpin + Send>> = if is_fifo {
                open_fifo(&path).map(|rx| Box::new(rx) as _)
            } else {
                tokio::fs::File::open(&path)
                    .await
                    .map(|file| Box::new(Follow::new(file, chunk_duration)) as _)
            };
            match opened {
                Ok(mut reader) => {
                    tracing::info!(path, "Pipe stream opened");
                    let mut ts = ChunkTimestamper::new(format.rate());
                    match pump_pcm(
                        &mut reader,
                        &mut ts,
                        chunk_frames,
                        chunk_bytes,
                        &tx,
                        Some(chunk_duration),
                    )
                    .await
                    {
                        PumpEnd::SourceEnded => {
                            tracing::debug!(path, "Pipe read ended, reopening");
                        }
                        PumpEnd::TxClosed => return,
                    }
                }
                Err(e) => {
                    tracing::debug!(path, error = %e, "Pipe not available, retrying");
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }))
}

/// Open a FIFO for reading without tying up a thread: the read end is
/// nonblocking and driven by the reactor. A blocking `open`/`read` on the
/// blocking pool would wait for a writer indefinitely, and the runtime waits
/// for blocking tasks on shutdown, so an idle pipe source stalled every exit.
///
/// On Linux the FIFO is opened read-write, so it never reports EOF when a
/// writer goes away; reads simply wait for the next writer.
fn open_fifo(path: &str) -> std::io::Result<tokio::net::unix::pipe::Receiver> {
    let mut options = tokio::net::unix::pipe::OpenOptions::new();
    #[cfg(target_os = "linux")]
    options.read_write(true);
    options.open_receiver(path)
}

/// Reader that treats EOF as "no data yet" and retries after `poll_every`,
/// like `tail -f`.
struct Follow<R> {
    inner: R,
    poll_every: std::time::Duration,
    sleep: Option<std::pin::Pin<Box<tokio::time::Sleep>>>,
}

impl<R> Follow<R> {
    fn new(inner: R, poll_every: std::time::Duration) -> Self {
        Self {
            inner,
            poll_every,
            sleep: None,
        }
    }
}

impl<R: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for Follow<R> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        use std::task::{Poll, ready};
        loop {
            if let Some(sleep) = self.sleep.as_mut() {
                ready!(sleep.as_mut().poll(cx));
                self.sleep = None;
            }
            let before = buf.filled().len();
            ready!(std::pin::Pin::new(&mut self.inner).poll_read(cx, buf))?;
            if buf.filled().len() > before || buf.remaining() == 0 {
                return Poll::Ready(Ok(()));
            }
            // EOF for now: wait for the file to grow.
            let poll_every = self.poll_every;
            self.sleep = Some(Box::pin(tokio::time::sleep(poll_every)));
        }
    }
}

/// Make sure `path` is a FIFO. Returns whether it is one. When it is missing
/// and `create` is set, it is created (mode 0666, minus the umask).
fn prepare_fifo(path: &str, create: bool) -> std::io::Result<bool> {
    use std::os::unix::fs::FileTypeExt;
    match std::fs::metadata(path) {
        Ok(meta) => Ok(meta.file_type().is_fifo()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && create => {
            let c_path = std::ffi::CString::new(path)?;
            // SAFETY: `c_path` is a valid NUL-terminated string that outlives the call.
            if unsafe { libc::mkfifo(c_path.as_ptr(), 0o666) } != 0 {
                let err = std::io::Error::last_os_error();
                // Another process may have created it in the meantime.
                if err.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(err);
                }
            }
            tracing::info!(path, "Created FIFO");
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::FileTypeExt;

    #[test]
    fn creates_missing_fifo_in_create_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapfifo");
        let path = path.to_str().unwrap();
        assert!(prepare_fifo(path, true).unwrap());
        assert!(std::fs::metadata(path).unwrap().file_type().is_fifo());
        // An existing FIFO is left alone.
        assert!(prepare_fifo(path, true).unwrap());
    }

    #[test]
    fn read_mode_does_not_create() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapfifo");
        assert!(!prepare_fifo(path.to_str().unwrap(), false).unwrap());
        assert!(!path.exists());
    }

    #[test]
    fn regular_file_is_reported_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapfifo");
        std::fs::write(&path, b"pcm").unwrap();
        assert!(!prepare_fifo(path.to_str().unwrap(), true).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"pcm");
    }

    /// A growing regular file is followed: data appended after EOF is read
    /// on the same handle, so nothing is replayed from the start.
    #[tokio::test]
    async fn follows_a_growing_regular_file() {
        use tokio::io::AsyncReadExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapfifo");
        std::fs::write(&path, b"ab").unwrap();
        let file = tokio::fs::File::open(&path).await.unwrap();
        let mut reader = Follow::new(file, std::time::Duration::from_millis(5));
        let appender = {
            let path = path.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
                f.write_all(b"cd").unwrap();
            })
        };
        let mut buf = [0u8; 4];
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            reader.read_exact(&mut buf),
        )
        .await
        .expect("waited for the appended bytes")
        .unwrap();
        assert_eq!(&buf, b"abcd");
        appender.await.unwrap();
    }

    /// A source that has far more data ready than realtime (here a regular
    /// file, as left behind when the writer created the path) is still read
    /// at realtime instead of flooding the encoder and clients.
    #[tokio::test]
    async fn reads_are_paced_at_realtime() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapfifo");
        let format = SampleFormat::new(48000, 16, 2);
        // 10 s of silence available immediately.
        std::fs::write(&path, vec![0u8; 48000 * 4 * 10]).unwrap();
        let uri = StreamUri::parse(&format!("pipe://{}?name=t", path.display())).unwrap();
        let (tx, mut rx) = mpsc::channel(1024);
        let handle = start(uri, format, 960, tx).unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        handle.abort();
        let mut chunks = 0;
        while rx.try_recv().is_ok() {
            chunks += 1;
        }
        // 500 ms of 20 ms chunks is 25; unpaced it would be all 500 at once.
        assert!((15..=40).contains(&chunks), "{chunks} chunks in 500 ms");
    }

    /// Regression: the FIFO was opened with a blocking `open` on the blocking
    /// pool, which never returns without a writer and kept the runtime from
    /// shutting down. Now the reader waits on the reactor, and data from a
    /// writer that connects later still arrives.
    #[test]
    fn idle_fifo_does_not_block_runtime_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapfifo");
        let uri = StreamUri::parse(&format!("pipe://{}?name=t", path.display())).unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        rt.block_on(async {
            start(uri, SampleFormat::new(48000, 16, 2), 960, tx).unwrap();
            // Let the reader create and open the FIFO with no writer.
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            let mut writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            std::io::Write::write_all(&mut writer, &[0u8; 960 * 4]).unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
                .await
                .expect("chunk from a writer that connected later")
                .unwrap();
        });
        let started = std::time::Instant::now();
        drop(rt);
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn rejects_unknown_mode() {
        let uri = StreamUri::parse("pipe:///tmp/x?name=t&mode=bogus").unwrap();
        let (tx, _rx) = mpsc::channel(1);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let _guard = rt.enter();
        assert!(start(uri, SampleFormat::new(48000, 16, 2), 960, tx).is_err());
    }
}
