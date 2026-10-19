//! Integration test helpers.

pub mod sync;

use std::sync::Arc;
use std::sync::atomic::{AtomicU16, Ordering};

use snapcast_client::{ClientConfig, ClientEvent, SnapClient};
use snapcast_proto::message::hello::Hello;
use snapcast_server::status::ServerStatus;
use snapcast_server::{AudioData, AudioFrame, ServerConfig, ServerEvent, SnapServer};
use tokio::sync::mpsc;

/// Interleaved f32 samples per test frame: 10 ms of 48 kHz stereo.
const FRAME_SAMPLES: usize = 960;

/// FLAC block size of the server's encoder, in frames (its `BLOCK_SIZE` is
/// private). Each block reaches the client as one decoded audio frame.
pub const FLAC_BLOCK_FRAMES: usize = 1152;

/// One 10 ms frame of a non-silent ramp in [-1, 1) (48 kHz stereo f32).
pub fn tone_frame(timestamp_usec: i64) -> AudioFrame {
    let samples = (0..FRAME_SAMPLES)
        .map(|i| (i as f32 / FRAME_SAMPLES as f32) * 2.0 - 1.0)
        .collect();
    AudioFrame {
        data: AudioData::F32(samples),
        timestamp_usec,
    }
}

/// One 10 ms frame of the constant `level` (0.0 for silence).
pub fn constant_frame(level: f32, timestamp_usec: i64) -> AudioFrame {
    AudioFrame {
        data: AudioData::F32(vec![level; FRAME_SAMPLES]),
        timestamp_usec,
    }
}

/// Bind an ephemeral 127.0.0.1 port, spawn `server.serve()` on it, and return
/// the actual bound port. The library opens no port itself, so tests bind here;
/// reading the bound port avoids port collisions between parallel tests.
///
/// No startup wait is needed: the socket is already listening when this
/// returns, so a client that connects before the accept loop runs just waits
/// in the kernel backlog.
pub async fn spawn_serving(mut server: SnapServer) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        server.serve(listener).await.ok();
    });
    port
}

/// Server handle with event receiver and audio sender.
pub struct TestServer {
    pub events: mpsc::Receiver<ServerEvent>,
    pub audio_tx: mpsc::Sender<snapcast_server::AudioFrame>,
    pub cmd: mpsc::Sender<snapcast_server::ServerCommand>,
    pub port: u16,
}

/// Start a default server on a random port. Returns the handle once serving.
pub async fn start_server() -> TestServer {
    start_server_with(ServerConfig::default()).await
}

/// Start a server with `config` and one `default` stream on a random port.
pub async fn start_server_with(config: ServerConfig) -> TestServer {
    let (mut server, events) = SnapServer::new(config);
    let audio_tx = server.add_stream("default");
    let cmd = server.command_sender();
    let port = spawn_serving(server).await;
    TestServer {
        events,
        audio_tx,
        cmd,
        port,
    }
}

/// Server handle with two streams, `stream_a` (the default, as the first
/// registered) and `stream_b`, for routing tests.
pub struct TwoStreamServer {
    pub events: mpsc::Receiver<ServerEvent>,
    pub stream_a: mpsc::Sender<snapcast_server::AudioFrame>,
    pub stream_b: mpsc::Sender<snapcast_server::AudioFrame>,
    pub cmd: mpsc::Sender<snapcast_server::ServerCommand>,
    pub port: u16,
}

/// Start a default-config server with streams `stream_a` and `stream_b`.
pub async fn start_two_stream_server() -> TwoStreamServer {
    let (mut server, events) = SnapServer::new(ServerConfig::default());
    let stream_a = server.add_stream("stream_a");
    let stream_b = server.add_stream("stream_b");
    let cmd = server.command_sender();
    let port = spawn_serving(server).await;
    TwoStreamServer {
        events,
        stream_a,
        stream_b,
        cmd,
        port,
    }
}

/// Client handle with event receiver.
pub struct TestClient {
    pub events: mpsc::Receiver<ClientEvent>,
    pub audio_rx: mpsc::Receiver<snapcast_client::AudioFrame>,
    pub cmd: mpsc::Sender<snapcast_client::ClientCommand>,
}

/// Connect a client to the given server port, with an empty host id.
///
/// An empty host id makes the server derive the client id from the machine MAC —
/// fine for single-client tests, but it collapses multiple concurrent clients
/// into one id/group. Multi-client tests must give each client a distinct id via
/// [`connect_client_with_id`].
pub async fn connect_client(port: u16) -> TestClient {
    connect_client_with_id(port, "").await
}

/// Connect a client with an explicit `host_id`, so the server assigns it a
/// distinct client id/group. Required for tests with more than one concurrent
/// client.
pub async fn connect_client_with_id(port: u16, host_id: &str) -> TestClient {
    connect_client_with(ClientConfig {
        host: "127.0.0.1".into(),
        port,
        host_id: host_id.to_string(),
        ..ClientConfig::default()
    })
    .await
}

/// Run a client with `config` in the background.
pub async fn connect_client_with(config: ClientConfig) -> TestClient {
    let (mut client, events, audio_rx) = SnapClient::new(config);
    let cmd = client.command_sender();
    tokio::spawn(async move {
        client.run().await.ok();
    });
    TestClient {
        events,
        audio_rx,
        cmd,
    }
}

/// Wait for a specific event, with timeout.
pub async fn expect_event<F, T>(
    events: &mut mpsc::Receiver<ClientEvent>,
    timeout_ms: u64,
    mut f: F,
) -> T
where
    F: FnMut(ClientEvent) -> Option<T>,
{
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(event)) => {
                if let Some(val) = f(event) {
                    return val;
                }
            }
            Ok(None) => panic!("Event channel closed"),
            _ => panic!("Timed out waiting for expected event"),
        }
    }
}

/// Wait until the server reports any client connected; returns its id.
pub async fn expect_client_connected(events: &mut mpsc::Receiver<ServerEvent>) -> String {
    expect_server_event(events, 3000, |e| match e {
        ServerEvent::ClientConnected { id, .. } => Some(id),
        _ => None,
    })
    .await
}

/// Wait until the server reports client `id` connected.
pub async fn expect_client_id_connected(events: &mut mpsc::Receiver<ServerEvent>, id: &str) {
    expect_server_event(events, 3000, |e| {
        matches!(e, ServerEvent::ClientConnected { id: ref got, .. } if got == id).then_some(())
    })
    .await
}

/// Wait until the server reports client `id` disconnected.
pub async fn expect_client_disconnected(events: &mut mpsc::Receiver<ServerEvent>, id: &str) {
    expect_server_event(events, 3000, |e| {
        matches!(e, ServerEvent::ClientDisconnected { id: ref got } if got == id).then_some(())
    })
    .await
}

/// Assert that no server event matching `f` arrives within `window_ms`.
/// Proving an absence needs a fixed window; keep it as short as the
/// property allows.
pub async fn expect_no_server_event<F>(
    events: &mut mpsc::Receiver<ServerEvent>,
    window_ms: u64,
    what: &str,
    mut f: F,
) where
    F: FnMut(&ServerEvent) -> bool,
{
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(window_ms);
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(event)) if f(&event) => panic!("{what}: {event:?}"),
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return,
        }
    }
}

/// After a `ClientCommand::Stop`: the client's event channel closes (its run
/// loop returned) without a `Disconnected`, which would mean the reconnect
/// loop ran instead of a clean shutdown.
pub async fn expect_clean_stop(events: &mut mpsc::Receiver<ClientEvent>) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(None) => return,
            Ok(Some(ClientEvent::Disconnected { reason })) => {
                panic!("clean client Stop emitted Disconnected: {reason}")
            }
            Ok(Some(_)) => {}
            Err(_) => panic!("client did not shut down after Stop"),
        }
    }
}

/// Receive decoded audio until at least `min_samples` interleaved samples
/// arrived, failing after `timeout_ms`. Returns the frames in order.
pub async fn collect_audio(
    audio_rx: &mut mpsc::Receiver<snapcast_client::AudioFrame>,
    min_samples: usize,
    timeout_ms: u64,
) -> Vec<snapcast_client::AudioFrame> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let mut frames = Vec::new();
    let mut total = 0;
    while total < min_samples {
        let frame = tokio::time::timeout_at(deadline, audio_rx.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out with {total} of {min_samples} samples"))
            .expect("audio channel closed");
        total += frame.samples.len();
        frames.push(frame);
    }
    frames
}

/// A TCP relay on a port of its own that forwards each new connection to the
/// current upstream port. Clients keep one address while the server behind
/// it is replaced, without rebinding a freed port that a parallel test may
/// have taken meanwhile.
pub struct Relay {
    /// Port clients connect to.
    pub port: u16,
    upstream: Arc<AtomicU16>,
}

impl Relay {
    /// Start relaying to `127.0.0.1:upstream`.
    pub async fn start(upstream: u16) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let upstream = Arc::new(AtomicU16::new(upstream));
        let target = Arc::clone(&upstream);
        tokio::spawn(async move {
            while let Ok((mut client, _)) = listener.accept().await {
                let port = target.load(Ordering::SeqCst);
                tokio::spawn(async move {
                    // No upstream: dropping `client` closes it, as a dead
                    // server would.
                    let Ok(mut server) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await
                    else {
                        return;
                    };
                    tokio::io::copy_bidirectional(&mut client, &mut server)
                        .await
                        .ok();
                });
            }
        });
        Self { port, upstream }
    }

    /// Forward connections accepted from now on to `127.0.0.1:upstream`.
    pub fn set_upstream(&self, upstream: u16) {
        self.upstream.store(upstream, Ordering::SeqCst);
    }
}

/// A `Hello` from a bare test connection with client id `id`.
pub(crate) fn test_hello(id: &str) -> Hello {
    Hello {
        mac: "00:00:00:00:00:00".into(),
        host_name: format!("raw-{id}"),
        version: "0.0.0".into(),
        client_name: "RawClient".into(),
        os: "test".into(),
        arch: "test".into(),
        instance: 1,
        id: id.into(),
        snap_stream_protocol_version: snapcast_proto::PROTOCOL_VERSION,
        auth: None,
    }
}

/// A bare binary-protocol connection, for tests that need control over what
/// the client sends (or that it sends nothing) beyond what `SnapClient` does.
pub struct RawClient {
    stream: tokio::net::TcpStream,
    buf: Vec<u8>,
}

impl RawClient {
    /// Connect and send a `Hello` with client id `id`.
    pub async fn connect(port: u16, id: &str) -> Self {
        use snapcast_proto::message::factory::MessagePayload;

        let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        let mut client = Self {
            stream,
            buf: Vec::new(),
        };
        client
            .send(
                snapcast_proto::MessageType::Hello,
                &MessagePayload::Hello(test_hello(id)),
            )
            .await;
        client
    }

    /// Send one message.
    pub async fn send(
        &mut self,
        msg_type: snapcast_proto::MessageType,
        payload: &snapcast_proto::message::factory::MessagePayload,
    ) {
        use tokio::io::AsyncWriteExt;
        let mut base = snapcast_proto::BaseMessage {
            msg_type,
            id: 1,
            refers_to: 0,
            sent: Default::default(),
            received: Default::default(),
            size: 0,
        };
        let frame = snapcast_proto::message::factory::serialize(&mut base, payload).unwrap();
        self.stream.write_all(&frame).await.unwrap();
    }

    /// Receive the next message, or `None` once the server closed the
    /// connection (or `timeout_ms` passed without a message).
    pub async fn recv(
        &mut self,
        timeout_ms: u64,
    ) -> Option<snapcast_proto::message::factory::TypedMessage> {
        use tokio::io::AsyncReadExt;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
        loop {
            if let Some(msg) = snapcast_proto::message::factory::take_frame(&mut self.buf).unwrap()
            {
                return Some(msg);
            }
            self.buf.reserve(8192);
            match tokio::time::timeout_at(deadline, self.stream.read_buf(&mut self.buf)).await {
                Ok(Ok(n)) if n > 0 => {}
                _ => return None,
            }
        }
    }

    /// Whether the server closed this connection within `timeout_ms`,
    /// discarding any messages received before that.
    pub async fn closed_within(&mut self, timeout_ms: u64) -> bool {
        use tokio::io::AsyncReadExt;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
        let mut scratch = [0u8; 8192];
        loop {
            match tokio::time::timeout_at(deadline, self.stream.read(&mut scratch)).await {
                Ok(Ok(0)) | Ok(Err(_)) => return true,
                Ok(Ok(_)) => {}
                Err(_) => return false,
            }
        }
    }
}

/// Server-side analogue of [`expect_event`]: wait for a matching `ServerEvent`.
pub async fn expect_server_event<F, T>(
    events: &mut mpsc::Receiver<ServerEvent>,
    timeout_ms: u64,
    mut f: F,
) -> T
where
    F: FnMut(ServerEvent) -> Option<T>,
{
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(event)) => {
                if let Some(val) = f(event) {
                    return val;
                }
            }
            Ok(None) => panic!("Server event channel closed"),
            _ => panic!("Timed out waiting for expected server event"),
        }
    }
}

/// Fetch the server status.
pub async fn server_status(cmd: &mpsc::Sender<snapcast_server::ServerCommand>) -> ServerStatus {
    let (tx, rx) = tokio::sync::oneshot::channel();
    cmd.send(snapcast_server::ServerCommand::GetStatus { response_tx: tx })
        .await
        .unwrap();
    rx.await.unwrap()
}

/// Look up whether `client_id` is reported connected in the server status.
pub async fn client_connected(
    cmd: &mpsc::Sender<snapcast_server::ServerCommand>,
    client_id: &str,
) -> Option<bool> {
    server_status(cmd)
        .await
        .server
        .groups
        .iter()
        .flat_map(|g| &g.clients)
        .find(|c| c.id == client_id)
        .map(|c| c.connected)
}

/// Look up the `(group id, stream id)` of the group holding `client_id` in the
/// server status. Panics if the client is in no group.
pub async fn client_group(
    cmd: &mpsc::Sender<snapcast_server::ServerCommand>,
    client_id: &str,
) -> (String, String) {
    server_status(cmd)
        .await
        .server
        .groups
        .iter()
        .find(|g| g.clients.iter().any(|c| c.id == client_id))
        .map(|g| (g.id.clone(), g.stream_id.clone()))
        .unwrap_or_else(|| panic!("Client {client_id} not found in any group"))
}
