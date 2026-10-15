//! Integration test helpers.

pub mod sync;

use snapcast_client::{ClientConfig, ClientEvent, SnapClient};
use snapcast_proto::message::hello::Hello;
use snapcast_server::status::ServerStatus;
use snapcast_server::{AudioData, AudioFrame, ServerConfig, ServerEvent, SnapServer};
use tokio::sync::mpsc;

/// Interleaved f32 samples per test frame: 10 ms of 48 kHz stereo.
const FRAME_SAMPLES: usize = 960;

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
