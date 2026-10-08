//! Client session management — binary protocol server for snapclients.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use snapcast_proto::MessageType;
use snapcast_proto::message::base::BaseMessage;
use snapcast_proto::message::codec_header::CodecHeader;
use snapcast_proto::message::factory::{self, MessagePayload, TypedMessage};
use snapcast_proto::message::hello::Hello;
use snapcast_proto::message::server_settings::ServerSettings;
use snapcast_proto::message::time::Time;
use snapcast_proto::types::Timeval;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, broadcast, mpsc, oneshot, watch};
use tokio::task::JoinSet;

use crate::ClientSettingsUpdate;
use crate::ServerEvent;
use crate::WireChunkData;
use crate::time::now_usec;

/// Pause after a failed `accept`, so persistent failures (e.g. out of file
/// descriptors) do not spin.
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(100);

/// Unsent bytes the kernel may hold for a streaming client
/// (`TCP_NOTSENT_LOWAT`). A time reply queues behind whatever audio is
/// already in the socket, and every 4 KiB there delay it by 16 ms on a
/// 2 Mbit/s link. Audio is written in realtime chunks of at most a few KiB
/// (20 ms of 48 kHz stereo PCM is 3.75 KiB), so a backlog beyond one chunk
/// buys no throughput; with this limit it waits in the session instead,
/// where time replies overtake it.
#[cfg(any(target_os = "linux", target_os = "android"))]
const NOTSENT_LOWAT: u32 = 4096;

/// Received messages buffered between a session's reader and its writer.
const INCOMING_QUEUE: usize = 16;

/// Byte range of `sent` in a serialized base header.
const SENT_RANGE: std::ops::Range<usize> = 6..14;

// ── Routing ───────────────────────────────────────────────────

/// Per-session routing state — updated when groups/streams/mute change.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionRouting {
    /// Stream ID this client should receive audio from.
    pub stream_id: String,
    /// Client is muted.
    pub client_muted: bool,
    /// Client's group is muted.
    pub group_muted: bool,
}

/// Per-stream codec header, stored at stream registration time.
#[derive(Debug, Clone)]
pub struct StreamCodecInfo {
    /// Codec name (e.g. "flac", "pcm").
    pub codec: String,
    /// Encoded codec header bytes.
    pub header: Vec<u8>,
}

// ── Session context (private fields, method API) ──────────────

/// Shared context for all sessions.
///
/// # Lock ordering
///
/// When acquiring multiple locks, always follow this order to prevent deadlocks:
///
/// 0. `sessions` (active session per client id)
/// 1. `shared_state` (server state — groups, clients, streams)
/// 2. `routing_senders` (per-client watch channels)
/// 3. `settings_senders` (per-client watch channels)
/// 4. `codec_headers` (per-stream codec info)
///
/// Never hold a higher-numbered lock while acquiring a lower-numbered one.
/// In practice, most paths only need one or two locks:
/// - Routing updates: `shared_state` → `routing_senders`
/// - Settings push: `settings_senders` only
/// - Codec lookup: `codec_headers` only
/// - Client registration and cleanup: `sessions` held while registering or
///   releasing everything else
struct SessionContext {
    buffer_ms: i32,
    auth: Option<Arc<dyn crate::auth::AuthValidator>>,
    client_filter: Option<Arc<dyn crate::auth::ClientFilter>>,
    send_audio_to_muted: bool,
    /// Close a session that sends nothing for this long (`None` = never).
    idle_timeout: Option<Duration>,
    /// Latest settings per client. A watch channel, so pushing never waits
    /// on a session that is stuck writing to its peer.
    settings_senders: Mutex<HashMap<String, watch::Sender<ClientSettingsUpdate>>>,
    routing_senders: Mutex<HashMap<String, watch::Sender<SessionRouting>>>,
    codec_headers: Mutex<HashMap<String, StreamCodecInfo>>,
    /// The one live session per client id. A reconnect with the same id
    /// replaces (and cancels) the previous session.
    sessions: Mutex<HashMap<String, ActiveSession>>,
    next_generation: AtomicU64,
    shared_state: Arc<tokio::sync::Mutex<crate::state::ServerState>>,
    default_stream: String,
}

/// Ownership record for a client's live session.
struct ActiveSession {
    /// Distinguishes this session from earlier/later ones with the same id.
    generation: u64,
    /// Fired (or dropped) to stop the session when it is replaced.
    cancel: oneshot::Sender<()>,
}

/// What [`SessionContext::register_session`] hands a new session.
struct Registration {
    generation: u64,
    /// Resolves when a newer session for the same client id takes over.
    cancelled: oneshot::Receiver<()>,
    settings_rx: watch::Receiver<ClientSettingsUpdate>,
    routing_rx: watch::Receiver<SessionRouting>,
    /// Settings to answer the Hello with.
    initial_settings: ClientSettingsUpdate,
    /// The client was in no group and got a new one.
    joins_new_group: bool,
}

impl SessionContext {
    /// Make a new session the owner of `hello.id`, cancelling any previous
    /// session for that id, and register its client and channels.
    ///
    /// `sessions` is held throughout, so registrations of two sessions for
    /// the same id never interleave: otherwise a session replaced while
    /// registering could overwrite its successor's channels.
    async fn register_session(&self, hello: &Hello, peer: SocketAddr) -> Registration {
        let client_id = &hello.id;
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        let (cancel, cancelled) = oneshot::channel();
        let mut sessions = self.sessions.lock().await;
        let previous = sessions.insert(client_id.clone(), ActiveSession { generation, cancel });
        if let Some(previous) = previous {
            tracing::info!(id = %client_id, "Client reconnected, replacing previous session");
            let _ = previous.cancel.send(());
        }

        let (initial_settings, initial_routing, joins_new_group) = {
            let mut s = self.shared_state.lock().await;
            let c = s.get_or_create_client(client_id, &hello.host_name, &hello.mac);
            c.update_from_hello(hello, &peer.ip().to_string());
            c.connected = true;
            let initial_settings = ClientSettingsUpdate {
                client_id: client_id.clone(),
                buffer_ms: self.buffer_ms,
                latency: c.config.latency,
                volume: c.config.volume.percent,
                muted: c.config.volume.muted,
            };
            let joins_new_group = !s.groups.iter().any(|g| g.clients.contains(client_id));
            s.group_for_client(client_id, &self.default_stream);
            let initial_routing =
                Self::build_routing(&s, client_id).unwrap_or_else(|| SessionRouting {
                    stream_id: self.default_stream.clone(),
                    client_muted: false,
                    group_muted: false,
                });
            (initial_settings, initial_routing, joins_new_group)
        };

        let (routing_tx, routing_rx) = watch::channel(initial_routing);
        self.routing_senders
            .lock()
            .await
            .insert(client_id.clone(), routing_tx);
        let (settings_tx, settings_rx) = watch::channel(initial_settings.clone());
        self.settings_senders
            .lock()
            .await
            .insert(client_id.clone(), settings_tx);
        drop(sessions);

        Registration {
            generation,
            cancelled,
            settings_rx,
            routing_rx,
            initial_settings,
            joins_new_group,
        }
    }

    /// Release a session's registrations. Only the session that still owns
    /// `client_id` touches the shared maps and state, so a replaced session
    /// ending late cannot unregister its successor. Returns whether this
    /// session was the owner (and so should report the disconnect).
    async fn release_session(&self, client_id: &str, generation: u64) -> bool {
        // Held throughout so a successor cannot register in between.
        let mut sessions = self.sessions.lock().await;
        if sessions
            .get(client_id)
            .is_none_or(|s| s.generation != generation)
        {
            return false;
        }
        sessions.remove(client_id);
        {
            let mut s = self.shared_state.lock().await;
            if let Some(c) = s.clients.get_mut(client_id) {
                c.connected = false;
                c.touch();
            }
        }
        self.routing_senders.lock().await.remove(client_id);
        self.settings_senders.lock().await.remove(client_id);
        true
    }

    /// Build routing for a single client from server state.
    /// State lock must be held by caller.
    fn build_routing(state: &crate::state::ServerState, client_id: &str) -> Option<SessionRouting> {
        let group = state
            .groups
            .iter()
            .find(|g| g.clients.contains(&client_id.to_string()))?;
        let client_muted = state
            .clients
            .get(client_id)
            .map(|c| c.config.volume.muted)
            .unwrap_or(false);
        Some(SessionRouting {
            stream_id: group.stream_id.clone(),
            client_muted,
            group_muted: group.muted,
        })
    }

    /// Send routing update to a single client's watch channel.
    async fn push_routing(&self, client_id: &str) {
        let s = self.shared_state.lock().await;
        if let Some(routing) = Self::build_routing(&s, client_id) {
            let senders = self.routing_senders.lock().await;
            if let Some(tx) = senders.get(client_id) {
                let _ = tx.send(routing);
            }
        }
    }

    /// Send routing updates to all clients in a group.
    async fn push_routing_for_group(&self, group_id: &str) {
        let s = self.shared_state.lock().await;
        let senders = self.routing_senders.lock().await;
        let Some(group) = s.groups.iter().find(|g| g.id == group_id) else {
            return;
        };
        for client_id in &group.clients {
            if let Some(routing) = Self::build_routing(&s, client_id)
                && let Some(tx) = senders.get(client_id)
            {
                let _ = tx.send(routing);
            }
        }
    }

    /// Send routing updates to all connected sessions.
    async fn push_routing_all(&self) {
        let s = self.shared_state.lock().await;
        let senders = self.routing_senders.lock().await;
        for group in &s.groups {
            for client_id in &group.clients {
                if let Some(routing) = Self::build_routing(&s, client_id)
                    && let Some(tx) = senders.get(client_id)
                {
                    let _ = tx.send(routing);
                }
            }
        }
    }

    /// Get codec header for a stream. Returns None if not registered.
    async fn codec_header_for(&self, stream_id: &str) -> Option<StreamCodecInfo> {
        self.codec_headers.lock().await.get(stream_id).cloned()
    }
}

// ── Session server (public API) ───────────────────────────────

/// Manages all streaming client sessions.
pub struct SessionServer {
    ctx: Arc<SessionContext>,
}

/// Construction options for [`SessionServer`].
pub(crate) struct SessionServerConfig {
    /// Client playout buffer in milliseconds.
    pub buffer_ms: i32,
    /// Optional streaming client authentication.
    pub auth: Option<Arc<dyn crate::auth::AuthValidator>>,
    /// Optional streaming client filter.
    pub client_filter: Option<Arc<dyn crate::auth::ClientFilter>>,
    /// Shared server state.
    pub shared_state: Arc<tokio::sync::Mutex<crate::state::ServerState>>,
    /// Initial default stream id.
    pub default_stream: String,
    /// Whether muted clients still receive audio.
    pub send_audio_to_muted: bool,
    /// Close a session that sends nothing for this long (`None` = never).
    pub idle_timeout: Option<Duration>,
}

impl SessionServer {
    /// Create a new session server.
    pub(crate) fn new(config: SessionServerConfig) -> Self {
        Self {
            ctx: Arc::new(SessionContext {
                buffer_ms: config.buffer_ms,
                auth: config.auth,
                client_filter: config.client_filter,
                send_audio_to_muted: config.send_audio_to_muted,
                idle_timeout: config.idle_timeout,
                settings_senders: Mutex::new(HashMap::new()),
                routing_senders: Mutex::new(HashMap::new()),
                codec_headers: Mutex::new(HashMap::new()),
                sessions: Mutex::new(HashMap::new()),
                next_generation: AtomicU64::new(0),
                shared_state: config.shared_state,
                default_stream: config.default_stream,
            }),
        }
    }

    /// Register a stream's codec header (called during stream setup).
    pub async fn register_stream_codec(&self, stream_id: &str, codec: &str, header: &[u8]) {
        self.ctx.codec_headers.lock().await.insert(
            stream_id.to_string(),
            StreamCodecInfo {
                codec: codec.to_string(),
                header: header.to_vec(),
            },
        );
    }

    /// Push a settings update to a specific streaming client. Never waits
    /// for the client: a session that has not yet written an earlier update
    /// writes only the latest one.
    pub async fn push_settings(&self, update: ClientSettingsUpdate) {
        let senders = self.ctx.settings_senders.lock().await;
        if let Some(tx) = senders.get(&update.client_id) {
            tx.send_replace(update);
        }
    }

    /// Update routing for a single client.
    pub async fn update_routing_for_client(&self, client_id: &str) {
        self.ctx.push_routing(client_id).await;
    }

    /// Update routing for all clients in a group.
    pub async fn update_routing_for_group(&self, group_id: &str) {
        self.ctx.push_routing_for_group(group_id).await;
    }

    /// Update routing for all connected sessions (after structural changes).
    pub async fn update_routing_all(&self) {
        self.ctx.push_routing_all().await;
    }

    /// Run the session server — accepts connections and spawns per-client
    /// tasks. Runs until dropped, which also ends every client session.
    pub async fn run(
        &self,
        listener: TcpListener,
        mut incoming: mpsc::Receiver<IncomingClient>,
        chunk_rx: broadcast::Sender<WireChunkData>,
        event_tx: mpsc::Sender<ServerEvent>,
    ) {
        tracing::info!(
            local_addr = ?listener.local_addr().ok(),
            "Stream server accepting clients"
        );

        // Owned here so that dropping `run` (server stop) aborts every session.
        let mut clients = JoinSet::new();
        let mut incoming_open = true;
        loop {
            tokio::select! {
                accepted = listener.accept() => match accepted {
                    Ok((stream, peer)) => {
                        stream.set_nodelay(true).ok();
                        let ka = socket2::TcpKeepalive::new().with_time(Duration::from_secs(10));
                        let sock = socket2::SockRef::from(&stream);
                        sock.set_tcp_keepalive(&ka).ok();
                        #[cfg(any(target_os = "linux", target_os = "android"))]
                        sock.set_tcp_notsent_lowat(NOTSENT_LOWAT).ok();
                        self.spawn_client(&mut clients, stream, peer, "tcp", &chunk_rx, &event_tx);
                    }
                    // Failures such as an aborted handshake or running out of
                    // file descriptors are transient: keep accepting.
                    Err(e) => {
                        tracing::warn!(error = %e, "Accepting a client failed");
                        tokio::time::sleep(ACCEPT_ERROR_BACKOFF).await;
                    }
                },
                client = incoming.recv(), if incoming_open => match client {
                    Some(client) => {
                        self.spawn_client(&mut clients, client.transport, client.peer, "external", &chunk_rx, &event_tx);
                    }
                    None => incoming_open = false,
                },
                Some(joined) = clients.join_next() => {
                    if let Err(e) = joined
                        && e.is_panic()
                    {
                        tracing::error!(error = %e, "Client session panicked");
                    }
                }
            }
        }
    }

    fn spawn_client<S>(
        &self,
        clients: &mut JoinSet<()>,
        stream: S,
        peer: SocketAddr,
        transport: &'static str,
        chunk_rx: &broadcast::Sender<WireChunkData>,
        event_tx: &mpsc::Sender<ServerEvent>,
    ) where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        tracing::info!(%peer, transport, "Client connecting");
        let chunk_sub = chunk_rx.subscribe();
        let ctx = Arc::clone(&self.ctx);
        let event_tx = event_tx.clone();
        clients.spawn(async move {
            let result = handle_client(stream, peer, chunk_sub, &ctx, event_tx).await;
            if let Err(e) = result {
                tracing::debug!(%peer, error = %e, "Client session ended");
            }
        });
    }
}

/// A client connection accepted by the embedder on a non-TCP transport.
pub(crate) struct IncomingClient {
    pub transport: Box<dyn crate::ClientTransport>,
    pub peer: SocketAddr,
}

// ── Client handler ────────────────────────────────────────────

async fn handle_client<S>(
    stream: S,
    peer: SocketAddr,
    chunk_rx: broadcast::Receiver<WireChunkData>,
    ctx: &SessionContext,
    event_tx: mpsc::Sender<ServerEvent>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (reader, mut writer) = tokio::io::split(stream);
    let mut frames = FrameReader::new(reader);
    let hello_msg = within_idle_timeout(ctx, frames.next())
        .await
        .context("waiting for Hello")??;
    let hello_id = hello_msg.base.id;
    let hello = match hello_msg.payload {
        MessagePayload::Hello(h) => h,
        _ => anyhow::bail!("expected Hello, got {:?}", hello_msg.base.msg_type),
    };

    let client_id = hello.id.clone();
    tracing::info!(id = %client_id, name = %hello.host_name, mac = %hello.mac, "Client hello");

    // Client filter — reject before auth if not accepted
    if ctx
        .client_filter
        .as_ref()
        .is_some_and(|f| !f.accept(&hello))
    {
        tracing::warn!(id = %client_id, mac = %hello.mac, host = %hello.host_name, "Client rejected by filter");
        return Ok(());
    }

    if let Some(validator) = &ctx.auth {
        within_idle_timeout(
            ctx,
            validate_auth(validator.as_ref(), &hello, &mut writer, &client_id),
        )
        .await
        .context("authenticating")??;
    }

    let Registration {
        generation,
        cancelled,
        settings_rx,
        routing_rx,
        initial_settings,
        joins_new_group,
    } = ctx.register_session(&hello, peer).await;
    let initial_stream_id = routing_rx.borrow().stream_id.clone();

    // A client outside any group changes the topology. C++ snapserver
    // announces that with Server.OnUpdate, and control clients such as
    // Snapweb need it before Client.OnConnect to know about the client.
    if joins_new_group {
        let _ = event_tx.send(ServerEvent::ServerUpdated).await;
    }
    let _ = event_tx
        .send(ServerEvent::ClientConnected {
            id: client_id.clone(),
            hello: hello.clone(),
        })
        .await;

    // From here on the session is registered, so every exit path must run
    // the release below. The watchdog runs beside the session rather than
    // inside its loop, so it also fires while a write to a stalled peer is
    // blocked (nothing is read meanwhile, so activity stops advancing).
    let activity = Activity::new();
    let session = async {
        match send_initial_messages(
            &mut writer,
            ctx,
            &initial_settings,
            hello_id,
            &initial_stream_id,
            &client_id,
        )
        .await
        {
            Ok(()) => {
                session_loop(SessionLoop {
                    frames,
                    writer,
                    chunk_rx,
                    settings_rx,
                    routing_rx,
                    event_tx: event_tx.clone(),
                    client_id: client_id.clone(),
                    cancelled,
                    activity: &activity,
                    ctx,
                })
                .await
            }
            Err(e) => Err(e),
        }
    };
    let result = tokio::select! {
        result = session => result,
        idle = activity.idle_for(ctx.idle_timeout) => {
            tracing::info!(id = %client_id, idle = ?idle, "Removing inactive session");
            Err(anyhow::anyhow!("client inactive for {idle:?}"))
        }
    };

    if ctx.release_session(&client_id, generation).await {
        let _ = event_tx
            .send(ServerEvent::ClientDisconnected { id: client_id })
            .await;
    }

    result
}

/// Run `fut`, failing if it takes longer than the idle timeout.
async fn within_idle_timeout<F: std::future::Future>(
    ctx: &SessionContext,
    fut: F,
) -> Result<F::Output> {
    match ctx.idle_timeout {
        Some(timeout) => tokio::time::timeout(timeout, fut)
            .await
            .map_err(|_| anyhow::anyhow!("client inactive for {timeout:?}")),
        None => Ok(fut.await),
    }
}

/// When a session last received anything from its client.
struct Activity {
    start: tokio::time::Instant,
    /// Microseconds after `start` of the last received message.
    last_usec: AtomicU64,
}

impl Activity {
    fn new() -> Self {
        Self {
            start: tokio::time::Instant::now(),
            last_usec: AtomicU64::new(0),
        }
    }

    /// Record that a message was just received.
    fn touch(&self) {
        let usec = self.start.elapsed().as_micros() as u64;
        self.last_usec.store(usec, Ordering::Relaxed);
    }

    /// Resolve once nothing was received for `timeout`, with the idle time.
    /// Never resolves for `None`.
    async fn idle_for(&self, timeout: Option<Duration>) -> Duration {
        let Some(timeout) = timeout else {
            return std::future::pending().await;
        };
        loop {
            let last = self.start + Duration::from_micros(self.last_usec.load(Ordering::Relaxed));
            let deadline = last + timeout;
            if tokio::time::Instant::now() >= deadline {
                return last.elapsed();
            }
            tokio::time::sleep_until(deadline).await;
        }
    }
}

/// Send the post-Hello handshake: `ServerSettings` (answering the Hello) and
/// the codec header of the client's initial stream.
async fn send_initial_messages<W: AsyncWrite + Unpin>(
    writer: &mut W,
    ctx: &SessionContext,
    settings: &ClientSettingsUpdate,
    hello_id: u16,
    initial_stream_id: &str,
    client_id: &str,
) -> Result<()> {
    // ServerSettings (refers_to must match Hello id for client's pending request)
    let ss_frame = serialize_msg(
        MessageType::ServerSettings,
        &MessagePayload::ServerSettings(server_settings(settings)),
        hello_id,
    )?;
    write_frame(writer, &ss_frame)
        .await
        .context("write server settings")?;

    // CodecHeader for client's stream
    match ctx.codec_header_for(initial_stream_id).await {
        Some(info) => {
            send_msg(
                writer,
                MessageType::CodecHeader,
                &MessagePayload::CodecHeader(CodecHeader {
                    codec: info.codec,
                    payload: info.header,
                }),
            )
            .await?;
        }
        None => {
            tracing::warn!(stream = %initial_stream_id, client = %client_id, "No codec header registered for stream");
        }
    }

    Ok(())
}

// ── Session loop ──

struct SessionLoop<'a, S> {
    frames: FrameReader<ReadHalf<S>>,
    writer: WriteHalf<S>,
    chunk_rx: broadcast::Receiver<WireChunkData>,
    settings_rx: watch::Receiver<ClientSettingsUpdate>,
    routing_rx: watch::Receiver<SessionRouting>,
    event_tx: mpsc::Sender<ServerEvent>,
    client_id: String,
    /// Resolves when a newer session for the same client id takes over.
    cancelled: oneshot::Receiver<()>,
    /// Updated on every received message, for the idle watchdog.
    activity: &'a Activity,
    ctx: &'a SessionContext,
}

async fn session_loop<S>(args: SessionLoop<'_, S>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let SessionLoop {
        mut frames,
        mut writer,
        mut chunk_rx,
        mut settings_rx,
        mut routing_rx,
        event_tx,
        client_id,
        mut cancelled,
        activity,
        ctx,
    } = args;
    let mut routing = routing_rx.borrow().clone();
    let mut audio = AudioCursor::default();

    // Reading runs beside writing, so a request is read (and stamped) when
    // it arrives even while a write to a slow client is pending.
    let (msg_tx, mut msg_rx) = mpsc::channel(INCOMING_QUEUE);
    let reader = async move {
        loop {
            let msg = frames.next().await?;
            activity.touch();
            if msg_tx.send(msg).await.is_err() {
                return Ok::<(), anyhow::Error>(());
            }
        }
    };

    // Biased: requests (time sync) and control go ahead of queued audio.
    let writer_loop = async {
        loop {
            tokio::select! {
                biased;
                _ = &mut cancelled => {
                    tracing::debug!(id = %client_id, "Session replaced by a newer connection");
                    return Ok(());
                }
                Some(msg) = msg_rx.recv() => {
                    match msg.payload {
                        MessagePayload::Time(_t) => {
                            // Reply first: anything done before the write
                            // counts as network latency for the client.
                            // latency = server_received - client_sent (c2s one-way estimate)
                            let latency = msg.base.received - msg.base.sent;
                            let mut frame = serialize_time_reply(latency, msg.base.id, msg.base.received)?;
                            write_time_reply(&mut writer, &mut frame).await.context("write time")?;
                            if let Some(c) = ctx.shared_state.lock().await.clients.get_mut(&client_id) {
                                c.touch();
                            }
                        }
                        MessagePayload::ClientInfo(info) => {
                            {
                                let mut s = ctx.shared_state.lock().await;
                                if let Some(c) = s.clients.get_mut(&client_id) {
                                    c.config.volume.percent = info.volume;
                                    c.config.volume.muted = info.muted;
                                }
                            }
                            // The mute flag gates audio for this session.
                            ctx.push_routing(&client_id).await;
                            let _ = event_tx.send(ServerEvent::ClientVolumeChanged {
                                client_id: client_id.clone(),
                                volume: info.volume,
                                muted: info.muted,
                            }).await;
                        }
                        _ => {}
                    }
                }
                Ok(()) = settings_rx.changed() => {
                    let update = settings_rx.borrow_and_update().clone();
                    write_settings(&mut writer, &update).await?;
                }
                Ok(()) = routing_rx.changed() => {
                    let new = routing_rx.borrow().clone();
                    if new.stream_id != routing.stream_id {
                        tracing::debug!(old = %routing.stream_id, new = %new.stream_id, "Stream switch");
                        if let Some(info) = ctx.codec_header_for(&new.stream_id).await {
                            let frame = serialize_msg(
                                MessageType::CodecHeader,
                                &MessagePayload::CodecHeader(CodecHeader {
                                    codec: info.codec,
                                    payload: info.header,
                                }),
                                0,
                            )?;
                            write_frame(&mut writer, &frame).await.context("write codec header")?;
                        }
                    }
                    routing = new;
                }
                chunk = chunk_rx.recv() => {
                    let chunk = match chunk {
                        Ok(c) => c,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            // Reported with the audio it cost once the
                            // next chunk is sent.
                            tracing::debug!(id = %client_id, skipped = n, "Broadcast lagged");
                            audio.lagged += n;
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            tracing::warn!("Broadcast closed");
                            anyhow::bail!("broadcast closed");
                        }
                    };
                    if !should_send_chunk(&chunk, &routing, ctx.send_audio_to_muted) {
                        continue;
                    }
                    if let Some((chunks, skipped_usec)) = audio.sent(&chunk) {
                        tracing::warn!(
                            id = %client_id,
                            dropped_chunks = chunks,
                            skipped_ms = skipped_usec as f64 / 1000.0,
                            "Broadcast lagged: client too slow, audio skipped"
                        );
                    }
                    write_chunk(&mut writer, chunk).await?;
                }
            }
        }
    };

    tokio::select! {
        result = reader => result,
        result = writer_loop => result,
    }
}

// ── Helpers ───────────────────────────────────────────────────

/// Timestamps of the audio sent to a session, to tell how much audio a
/// broadcast lag dropped.
#[derive(Debug, Default)]
struct AudioCursor {
    /// Stream and timestamp of the last chunk sent.
    last: Option<(String, i64)>,
    /// Timestamp step between the last two chunks sent: the duration of a
    /// chunk.
    step_usec: i64,
    /// Broadcast messages dropped since the last chunk sent (any stream).
    lagged: u64,
}

impl AudioCursor {
    /// Record that `chunk` is sent. After a lag, returns the number of
    /// dropped messages and the audio skipped before `chunk` (µs, from the
    /// timestamps; 0 if it is on another stream than the last one).
    fn sent(&mut self, chunk: &WireChunkData) -> Option<(u64, i64)> {
        let ts = chunk.timestamp_usec;
        let same_stream = self
            .last
            .as_ref()
            .filter(|(stream, _)| *stream == chunk.stream_id)
            .map(|&(_, last)| last);
        let lag = std::mem::take(&mut self.lagged);
        let report = (lag > 0).then(|| {
            let skipped = same_stream.map_or(0, |last| ts - (last + self.step_usec));
            (lag, skipped.max(0))
        });
        match same_stream {
            Some(last) if lag == 0 => self.step_usec = ts - last,
            Some(_) => {}
            None => self.step_usec = 0,
        }
        self.last = Some((chunk.stream_id.clone(), ts));
        report
    }
}

/// Decide whether to send a chunk to this session.
#[inline]
fn should_send_chunk(
    chunk: &WireChunkData,
    routing: &SessionRouting,
    send_audio_to_muted: bool,
) -> bool {
    if chunk.stream_id != routing.stream_id {
        return false;
    }
    if !send_audio_to_muted && (routing.client_muted || routing.group_muted) {
        return false;
    }
    true
}

async fn write_chunk<W: AsyncWrite + Unpin>(writer: &mut W, chunk: WireChunkData) -> Result<()> {
    let frame = serialize_wire_chunk(&chunk)?;
    write_frame(writer, &frame).await.context("write chunk")
}

/// Serialize a wire chunk frame, copying the shared payload once.
///
/// Equivalent to `serialize_msg` with a `WireChunk` payload, which would copy
/// the payload into an owned `WireChunk` and again into the frame. The frame
/// stays contiguous because a WebSocket transport turns each write into one
/// message.
fn serialize_wire_chunk(chunk: &WireChunkData) -> Result<Vec<u8>> {
    let payload_size = 8 + 4 + chunk.data.len();
    let base = BaseMessage {
        msg_type: MessageType::WireChunk,
        id: 0,
        refers_to: 0,
        sent: now_timeval(),
        received: Timeval::default(),
        size: u32::try_from(payload_size).context("wire chunk too large")?,
    };
    let mut frame = Vec::with_capacity(BaseMessage::HEADER_SIZE + payload_size);
    base.write_to(&mut frame)
        .map_err(|e| anyhow::anyhow!("serialize: {e}"))?;
    Timeval::from_usec(chunk.timestamp_usec)
        .write_to(&mut frame)
        .map_err(|e| anyhow::anyhow!("serialize: {e}"))?;
    frame.extend_from_slice(&(chunk.data.len() as u32).to_le_bytes());
    frame.extend_from_slice(&chunk.data);
    Ok(frame)
}

fn server_settings(update: &ClientSettingsUpdate) -> ServerSettings {
    ServerSettings {
        buffer_ms: update.buffer_ms,
        latency: update.latency,
        volume: update.volume,
        muted: update.muted,
    }
}

async fn write_settings<W: AsyncWrite + Unpin>(
    writer: &mut W,
    update: &ClientSettingsUpdate,
) -> Result<()> {
    let frame = serialize_msg(
        MessageType::ServerSettings,
        &MessagePayload::ServerSettings(server_settings(update)),
        0,
    )?;
    write_frame(writer, &frame)
        .await
        .context("write settings")?;
    tracing::debug!(
        volume = update.volume,
        latency = update.latency,
        "Pushed settings"
    );
    Ok(())
}

async fn validate_auth<S: AsyncWrite + Unpin>(
    validator: &dyn crate::auth::AuthValidator,
    hello: &Hello,
    stream: &mut S,
    client_id: &str,
) -> Result<()> {
    let auth_result = match &hello.auth {
        Some(a) => validator.validate(&a.scheme, &a.param),
        None => Err(crate::auth::AuthError::Unauthorized(
            "Authentication required".into(),
        )),
    };
    match auth_result {
        Ok(result) => {
            if !result
                .permissions
                .iter()
                .any(|p| p == crate::auth::PERM_STREAMING)
            {
                let err = snapcast_proto::message::error::Error {
                    code: 403,
                    message: "Forbidden".into(),
                    error: "Permission 'Streaming' missing".into(),
                };
                send_msg(stream, MessageType::Error, &MessagePayload::Error(err)).await?;
                anyhow::bail!("Client {client_id}: missing Streaming permission");
            }
            tracing::info!(id = %client_id, user = %result.username, "Authenticated");
            Ok(())
        }
        Err(e) => {
            let err = snapcast_proto::message::error::Error {
                code: e.code() as u32,
                message: e.message().to_string(),
                error: e.message().to_string(),
            };
            send_msg(stream, MessageType::Error, &MessagePayload::Error(err)).await?;
            anyhow::bail!("Client {client_id}: {e}");
        }
    }
}

fn serialize_msg(
    msg_type: MessageType,
    payload: &MessagePayload,
    refers_to: u16,
) -> Result<Vec<u8>> {
    let mut base = BaseMessage {
        msg_type,
        id: 0,
        refers_to,
        sent: now_timeval(),
        received: Timeval::default(),
        size: 0,
    };
    factory::serialize(&mut base, payload).map_err(|e| anyhow::anyhow!("serialize: {e}"))
}

/// Time reply: like C++ snapserver, `received` carries when the request
/// arrived and `sent` when the reply goes out.
fn serialize_time_reply(latency: Timeval, refers_to: u16, received: Timeval) -> Result<Vec<u8>> {
    let mut base = BaseMessage {
        msg_type: MessageType::Time,
        id: 0,
        refers_to,
        sent: now_timeval(),
        received,
        size: 0,
    };
    factory::serialize(&mut base, &MessagePayload::Time(Time { latency }))
        .map_err(|e| anyhow::anyhow!("serialize: {e}"))
}

/// Write a serialized time reply, stamping its `sent` time right before
/// every write attempt: a reply that waits for a congested socket must not
/// count that wait as network latency (the client takes `sent` as the time
/// the reply left).
async fn write_time_reply<W: AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &mut [u8],
) -> std::io::Result<()> {
    let written = std::future::poll_fn(|cx| {
        now_timeval().write_to(&mut &mut frame[SENT_RANGE])?;
        std::pin::Pin::new(&mut *writer).poll_write(cx, frame)
    })
    .await?;
    writer.write_all(&frame[written..]).await?;
    writer.flush().await
}

async fn send_msg<W: AsyncWrite + Unpin>(
    stream: &mut W,
    msg_type: MessageType,
    payload: &MessagePayload,
) -> Result<()> {
    let frame = serialize_msg(msg_type, payload, 0)?;
    write_frame(stream, &frame).await.context("write message")
}

/// Write one complete frame and flush it.
///
/// Message-oriented transports (WebSocket adapters) send a message per write
/// and only transmit on flush, so every frame is flushed individually.
async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, frame: &[u8]) -> std::io::Result<()> {
    writer.write_all(frame).await?;
    writer.flush().await
}

/// Cancel-safe reader of whole frames from a byte stream.
///
/// Incoming bytes are buffered until a complete frame is available, so
/// `next()` can lose a `tokio::select!` race without dropping a partially
/// read frame.
struct FrameReader<R> {
    reader: R,
    buf: Vec<u8>,
    /// When the last read returned: the arrival time of the frames it
    /// completed, however long they wait in `buf` to be taken.
    read_at: Timeval,
}

impl<R: AsyncRead + Unpin> FrameReader<R> {
    fn new(reader: R) -> Self {
        Self {
            reader,
            buf: Vec::new(),
            read_at: Timeval::default(),
        }
    }

    async fn next(&mut self) -> Result<TypedMessage> {
        loop {
            if let Some(mut msg) =
                factory::take_frame(&mut self.buf).map_err(|e| anyhow::anyhow!("parse: {e}"))?
            {
                msg.base.received = self.read_at;
                return Ok(msg);
            }
            self.buf.reserve(8192);
            let n = self
                .reader
                .read_buf(&mut self.buf)
                .await
                .context("read frame")?;
            self.read_at = now_timeval();
            anyhow::ensure!(n > 0, "connection closed");
        }
    }
}

fn now_timeval() -> Timeval {
    Timeval::from_usec(now_usec())
}

// ── Tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(stream_id: &str) -> WireChunkData {
        WireChunkData {
            stream_id: stream_id.to_string(),
            timestamp_usec: 0,
            data: vec![0u8; 64].into(),
        }
    }

    fn routing(stream_id: &str, client_muted: bool, group_muted: bool) -> SessionRouting {
        SessionRouting {
            stream_id: stream_id.to_string(),
            client_muted,
            group_muted,
        }
    }

    // ── serialize_wire_chunk ──────────────────────────────────

    #[test]
    fn wire_chunk_frame_matches_factory_serialization() {
        let data = WireChunkData {
            stream_id: "z1".into(),
            timestamp_usec: 1_234_567_890,
            data: (0..=255u8).collect::<Vec<_>>().into(),
        };
        let mut frame = serialize_wire_chunk(&data).unwrap();
        let msg = factory::take_frame(&mut frame).unwrap().unwrap();
        assert!(frame.is_empty());
        assert_eq!(msg.base.msg_type, MessageType::WireChunk);
        let MessagePayload::WireChunk(wc) = msg.payload else {
            panic!("expected WireChunk, got {:?}", msg.payload);
        };
        assert_eq!(wc.timestamp, Timeval::from_usec(data.timestamp_usec));
        assert_eq!(wc.payload, data.data);
    }

    // ── AudioCursor ───────────────────────────────────────────

    fn chunk_at(stream_id: &str, timestamp_usec: i64) -> WireChunkData {
        WireChunkData {
            stream_id: stream_id.into(),
            timestamp_usec,
            data: vec![0u8; 4].into(),
        }
    }

    #[test]
    fn audio_cursor_measures_audio_dropped_by_a_lag() {
        let mut audio = AudioCursor::default();
        assert_eq!(audio.sent(&chunk_at("s", 0)), None);
        assert_eq!(audio.sent(&chunk_at("s", 20_000)), None);
        // Five 20 ms chunks dropped: the next one is 120 ms after the last.
        audio.lagged = 5;
        assert_eq!(audio.sent(&chunk_at("s", 140_000)), Some((5, 100_000)));
        // The step is still one chunk, not the gap.
        audio.lagged = 1;
        assert_eq!(audio.sent(&chunk_at("s", 180_000)), Some((1, 20_000)));
        assert_eq!(audio.sent(&chunk_at("s", 200_000)), None);
    }

    #[test]
    fn audio_cursor_reports_lag_across_stream_switch() {
        let mut audio = AudioCursor::default();
        audio.sent(&chunk_at("a", 0));
        audio.lagged = 3;
        assert_eq!(audio.sent(&chunk_at("b", 5_000_000)), Some((3, 0)));
    }

    // ── should_send_chunk ─────────────────────────────────────

    #[test]
    fn matching_stream_unmuted_sends() {
        assert!(should_send_chunk(
            &chunk("z1"),
            &routing("z1", false, false),
            false
        ));
    }

    #[test]
    fn wrong_stream_skips() {
        assert!(!should_send_chunk(
            &chunk("z2"),
            &routing("z1", false, false),
            false
        ));
    }

    #[tokio::test]
    async fn read_frame_rejects_oversized_payload_before_allocation() {
        let header = BaseMessage {
            msg_type: MessageType::Time,
            id: 1,
            refers_to: 0,
            sent: Timeval::default(),
            received: Timeval::default(),
            size: snapcast_proto::DEFAULT_MAX_PAYLOAD_SIZE + 1,
        }
        .to_bytes()
        .unwrap();
        let mut frames = FrameReader::new(std::io::Cursor::new(header));

        let err = frames.next().await.unwrap_err();
        assert!(err.to_string().contains("payload too large"));
    }

    #[test]
    fn sent_range_locates_the_sent_field() {
        let sent = Timeval {
            sec: 0x0102_0304,
            usec: 0x0506_0708,
        };
        let frame = serialize_time_reply(Timeval::default(), 1, Timeval::default()).unwrap();
        let mut patched = frame.clone();
        sent.write_to(&mut &mut patched[SENT_RANGE]).unwrap();
        let msg = factory::take_frame(&mut patched).unwrap().unwrap();
        assert_eq!(msg.base.sent, sent);
    }

    #[tokio::test]
    async fn time_reply_is_stamped_when_the_socket_takes_it() {
        // A full pipe: the reply has to wait until the peer reads.
        let (mut server, client) = tokio::io::duplex(64);
        server.write_all(&[0u8; 64]).await.unwrap();
        let mut frame = serialize_time_reply(Timeval::default(), 1, Timeval::default()).unwrap();
        let writer = tokio::spawn(async move {
            write_time_reply(&mut server, &mut frame).await.unwrap();
            server
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let drained_at = now_usec();
        let mut frames = FrameReader::new(client);
        let mut filler = [0u8; 64];
        frames.reader.read_exact(&mut filler).await.unwrap();
        let reply = frames.next().await.unwrap();
        assert!(
            reply.base.sent.to_usec() >= drained_at,
            "sent must not include the wait for the socket"
        );
        drop(writer.await.unwrap());
    }

    #[tokio::test]
    async fn frame_reader_stamps_on_read() {
        let mut bytes = Vec::new();
        for _ in 0..2 {
            let frame = serialize_msg(MessageType::Time, &MessagePayload::Time(Time::default()), 0)
                .unwrap();
            bytes.extend_from_slice(&frame);
        }
        let mut frames = FrameReader::new(std::io::Cursor::new(bytes));
        let first = frames.next().await.unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
        let second = frames.next().await.unwrap();
        assert_eq!(first.base.received, second.base.received);
    }

    #[test]
    fn client_muted_skips() {
        assert!(!should_send_chunk(
            &chunk("z1"),
            &routing("z1", true, false),
            false
        ));
    }

    #[test]
    fn group_muted_skips() {
        assert!(!should_send_chunk(
            &chunk("z1"),
            &routing("z1", false, true),
            false
        ));
    }

    #[test]
    fn send_audio_to_muted_overrides() {
        assert!(should_send_chunk(
            &chunk("z1"),
            &routing("z1", true, true),
            true
        ));
    }

    #[test]
    fn wrong_stream_ignores_send_audio_to_muted() {
        assert!(!should_send_chunk(
            &chunk("z2"),
            &routing("z1", false, false),
            true
        ));
    }

    // ── build_routing ─────────────────────────────────────────

    #[test]
    fn build_routing_finds_client_in_group() {
        let mut state = crate::state::ServerState::default();
        state.get_or_create_client("c1", "host", "mac");
        state.group_for_client("c1", "stream1");
        let r = SessionContext::build_routing(&state, "c1").unwrap();
        assert_eq!(r.stream_id, "stream1");
        assert!(!r.client_muted);
        assert!(!r.group_muted);
    }

    #[test]
    fn build_routing_reflects_mute() {
        let mut state = crate::state::ServerState::default();
        let c = state.get_or_create_client("c1", "host", "mac");
        c.config.volume.muted = true;
        state.group_for_client("c1", "stream1");
        if let Some(g) = state
            .groups
            .iter_mut()
            .find(|g| g.clients.contains(&"c1".to_string()))
        {
            g.muted = true;
        }
        let r = SessionContext::build_routing(&state, "c1").unwrap();
        assert!(r.client_muted);
        assert!(r.group_muted);
    }

    #[test]
    fn build_routing_returns_none_for_unknown_client() {
        let state = crate::state::ServerState::default();
        assert!(SessionContext::build_routing(&state, "unknown").is_none());
    }

    // ── watch integration ─────────────────────────────────────

    #[test]
    fn routing_watch_delivers_updates() {
        let (tx, rx) = watch::channel(routing("z1", false, false));
        assert_eq!(rx.borrow().stream_id, "z1");
        tx.send(routing("z2", true, false)).unwrap();
        assert_eq!(rx.borrow().stream_id, "z2");
        assert!(rx.borrow().client_muted);
    }

    #[test]
    fn unmute_cycle() {
        let r_muted = routing("z1", true, false);
        let r_unmuted = routing("z1", false, false);
        assert!(!should_send_chunk(&chunk("z1"), &r_muted, false));
        assert!(should_send_chunk(&chunk("z1"), &r_unmuted, false));
    }

    // ── live sessions ─────────────────────────────────────────

    fn test_server(state: crate::state::ServerState) -> SessionServer {
        SessionServer::new(SessionServerConfig {
            buffer_ms: 1000,
            auth: None,
            client_filter: None,
            shared_state: Arc::new(Mutex::new(state)),
            default_stream: "default".into(),
            send_audio_to_muted: false,
            idle_timeout: None,
        })
    }

    fn hello(id: &str) -> Hello {
        Hello {
            mac: "00:11:22:33:44:55".into(),
            host_name: "host".into(),
            version: "0".into(),
            client_name: "test".into(),
            os: "os".into(),
            arch: "arch".into(),
            instance: 1,
            id: id.into(),
            snap_stream_protocol_version: 2,
            auth: None,
        }
    }

    type ClientSide<S> = (FrameReader<ReadHalf<S>>, WriteHalf<S>);

    /// Send Hello over `stream` and read the ServerSettings answering it.
    async fn handshake<S: AsyncRead + AsyncWrite>(stream: S, id: &str) -> ClientSide<S> {
        let (reader, mut writer) = tokio::io::split(stream);
        let mut frames = FrameReader::new(reader);
        send_msg(
            &mut writer,
            MessageType::Hello,
            &MessagePayload::Hello(hello(id)),
        )
        .await
        .unwrap();
        let msg = frames.next().await.unwrap();
        assert!(matches!(msg.payload, MessagePayload::ServerSettings(_)));
        (frames, writer)
    }

    /// Serve one client over an in-memory pipe of `capacity` bytes. The
    /// session lives as long as `chunk_tx`.
    async fn connect_duplex(
        server: &SessionServer,
        clients: &mut JoinSet<()>,
        chunk_tx: &broadcast::Sender<WireChunkData>,
        id: &str,
        capacity: usize,
    ) -> ClientSide<tokio::io::DuplexStream> {
        let (client, served) = tokio::io::duplex(capacity);
        let (event_tx, _) = mpsc::channel(16);
        let peer = "127.0.0.1:1".parse().unwrap();
        server.spawn_client(clients, served, peer, "test", chunk_tx, &event_tx);
        handshake(client, id).await
    }

    async fn routing_of(server: &SessionServer, id: &str) -> SessionRouting {
        server.ctx.routing_senders.lock().await[id].borrow().clone()
    }

    #[tokio::test]
    async fn client_unmuting_itself_resumes_audio() {
        let mut state = crate::state::ServerState::default();
        state
            .get_or_create_client("c1", "host", "mac")
            .config
            .volume
            .muted = true;
        let server = test_server(state);
        let mut clients = JoinSet::new();
        let (chunk_tx, _) = broadcast::channel(16);
        let (_frames, mut writer) =
            connect_duplex(&server, &mut clients, &chunk_tx, "c1", 4096).await;
        assert!(routing_of(&server, "c1").await.client_muted);

        send_msg(
            &mut writer,
            MessageType::ClientInfo,
            &MessagePayload::ClientInfo(snapcast_proto::message::client_info::ClientInfo {
                volume: 50,
                muted: false,
            }),
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while routing_of(&server, "c1").await.client_muted {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("routing follows the client's own unmute");
    }

    #[tokio::test]
    async fn push_settings_does_not_wait_for_a_stalled_client() {
        let server = test_server(crate::state::ServerState::default());
        let mut clients = JoinSet::new();
        let (chunk_tx, _) = broadcast::channel(16);
        // Room for about one settings frame: the session stalls on the next
        // write while the client reads nothing.
        let (mut frames, _writer) =
            connect_duplex(&server, &mut clients, &chunk_tx, "c1", 64).await;
        let update = |volume| ClientSettingsUpdate {
            client_id: "c1".into(),
            buffer_ms: 1000,
            latency: 0,
            volume,
            muted: false,
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            for volume in 0..=100 {
                server.push_settings(update(volume)).await;
            }
        })
        .await
        .expect("push_settings must not block on the session");

        // The client still ends up with the latest settings.
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let MessagePayload::ServerSettings(ss) = frames.next().await.unwrap().payload
                    && ss.volume == 100
                {
                    break;
                }
            }
        })
        .await
        .expect("latest settings delivered");
    }

    #[tokio::test]
    async fn dropping_run_ends_client_sessions() {
        let server = Arc::new(test_server(crate::state::ServerState::default()));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (chunk_tx, _) = broadcast::channel(16);
        let (_incoming_tx, incoming_rx) = mpsc::channel(1);
        let (event_tx, _events) = mpsc::channel(16);
        let run = tokio::spawn({
            let server = Arc::clone(&server);
            async move { server.run(listener, incoming_rx, chunk_tx, event_tx).await }
        });

        let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let (mut frames, _writer) = handshake(stream, "c1").await;
        run.abort();
        let err = tokio::time::timeout(Duration::from_secs(2), frames.next())
            .await
            .expect("session closed after the server stopped")
            .unwrap_err();
        assert!(err.to_string().contains("connection closed"), "{err}");
    }

    #[test]
    fn stream_switch_changes_filter() {
        let r1 = routing("z1", false, false);
        let r2 = routing("z2", false, false);
        assert!(should_send_chunk(&chunk("z1"), &r1, false));
        assert!(!should_send_chunk(&chunk("z1"), &r2, false));
        assert!(should_send_chunk(&chunk("z2"), &r2, false));
    }
}
