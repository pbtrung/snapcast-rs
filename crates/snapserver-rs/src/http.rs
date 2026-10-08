//! HTTP/WebSocket control server + Snapweb static file serving.
//!
//! Routes: `GET /jsonrpc` (WebSocket JSON-RPC control), `POST /jsonrpc`
//! (HTTP JSON-RPC), `GET /stream` (WebSocket streaming clients speaking the
//! binary protocol, as in C++ snapserver) and optionally Snapweb.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tokio::sync::{broadcast, mpsc};

use crate::auth::AuthConfig;
use crate::jsonrpc::{self, MAX_REQUEST_LEN};
use crate::notify::{self, Notification};

/// Shared state for axum handlers.
#[derive(Clone)]
struct AppState {
    notify_tx: broadcast::Sender<Notification>,
    auth_config: Arc<AuthConfig>,
    cmd_tx: mpsc::Sender<snapcast_server::ServerCommand>,
    client_acceptor: snapcast_server::ClientAcceptor,
}

/// Configuration for the HTTP server.
pub(crate) struct HttpConfig {
    /// TCP bind address.
    pub bind_address: String,
    /// HTTP port.
    pub port: u16,
    /// Snapweb document root (None = disabled).
    pub doc_root: Option<String>,
    /// Notification broadcast sender.
    pub notify_tx: broadcast::Sender<Notification>,
    /// Auth configuration.
    pub auth_config: Arc<AuthConfig>,
    /// Server command sender.
    pub cmd_tx: mpsc::Sender<snapcast_server::ServerCommand>,
    /// Hands WebSocket streaming clients (`/stream`) to the audio server.
    pub client_acceptor: snapcast_server::ClientAcceptor,
}

/// Start the HTTP server with JSON-RPC + WebSocket + optional Snapweb.
pub(crate) async fn run_http(cfg: HttpConfig) -> Result<()> {
    let app_state = AppState {
        notify_tx: cfg.notify_tx,
        auth_config: cfg.auth_config,
        cmd_tx: cfg.cmd_tx,
        client_acceptor: cfg.client_acceptor,
    };
    let app = router(app_state, cfg.doc_root.as_deref());

    let listener = tokio::net::TcpListener::bind((cfg.bind_address.as_str(), cfg.port)).await?;
    tracing::info!(
        bind_address = %cfg.bind_address,
        port = cfg.port,
        "HTTP/WebSocket server listening"
    );
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

/// Build the HTTP router. Must be served with
/// `into_make_service_with_connect_info::<SocketAddr>()` (`/stream` logs the peer).
fn router(app_state: AppState, doc_root: Option<&str>) -> Router {
    let mut app = Router::new()
        .route(
            "/jsonrpc",
            get(ws_handler)
                .post(http_jsonrpc_handler)
                .layer(DefaultBodyLimit::max(MAX_REQUEST_LEN)),
        )
        .route("/stream", get(stream_ws_handler))
        .with_state(app_state);

    if let Some(root) = doc_root {
        let serve = tower_http::services::ServeDir::new(root);
        app = app.fallback_service(serve);
        tracing::info!(doc_root = root, "Serving Snapweb");
    }
    app
}

/// WebSocket upgrade handler at GET /stream: a streaming client speaking the
/// Snapcast binary protocol, one frame per binary message.
async fn stream_ws_handler(
    ws: WebSocketUpgrade,
    State(app): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| async move {
        let transport = crate::ws_transport::WsTransport::new(socket);
        if let Err(e) = app.client_acceptor.accept(transport, peer).await {
            tracing::warn!(%peer, error = %e, "Dropping WebSocket stream client");
        }
    })
}

/// HTTP POST /jsonrpc handler. Stateless: when auth is enabled every request
/// must carry a valid `Authorization` header, `Bearer <token>` or
/// `Basic <base64(name:password)>`; otherwise the answer is HTTP 401 with a
/// JSON-RPC 401 error.
async fn http_jsonrpc_handler(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    body: String,
) -> Response {
    let auth_header = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if let Err(e) = crate::auth::validate_authorization(&app.auth_config, auth_header) {
        tracing::debug!(error = %e, "Rejecting unauthenticated HTTP JSON-RPC request");
        // A Bearer challenge: a Basic one would make browsers open a login
        // dialog over Snapweb.
        return (
            StatusCode::UNAUTHORIZED,
            [(axum::http::header::WWW_AUTHENTICATE, "Bearer")],
            axum::Json(serde_json::json!({
                "jsonrpc": "2.0", "id": null,
                "error": {"code": jsonrpc::UNAUTHORIZED, "message": jsonrpc::UNAUTHORIZED_MESSAGE}
            })),
        )
            .into_response();
    }

    // Untagged: an HTTP client has no notification stream to keep clean.
    match jsonrpc::handle_message(&body, &mut true, &app.auth_config, &app.cmd_tx, None).await {
        Some(reply) => axum::Json(reply).into_response(),
        // Only notifications: nothing to answer.
        None => StatusCode::NO_CONTENT.into_response(),
    }
}

/// WebSocket upgrade handler at GET /jsonrpc.
async fn ws_handler(ws: WebSocketUpgrade, State(app): State<AppState>) -> impl IntoResponse {
    ws.max_message_size(MAX_REQUEST_LEN)
        .on_upgrade(move |socket| handle_ws(socket, app))
}

async fn handle_ws(mut socket: WebSocket, app: AppState) {
    let mut notify_rx = app.notify_tx.subscribe();
    // Per-connection auth state, same policy as the TCP control server.
    let mut authenticated = !app.auth_config.enabled;
    let connection_id = notify::connection_id();

    loop {
        tokio::select! {
            msg = socket.recv() => {
                let Some(Ok(msg)) = msg else { break };
                let Message::Text(text) = msg else { continue };
                if let Some(reply) =
                    jsonrpc::handle_message(&text, &mut authenticated, &app.auth_config, &app.cmd_tx, Some(connection_id)).await
                    && socket.send(Message::Text(reply.to_string().into())).await.is_err()
                {
                    break;
                }
            }
            notification = notify_rx.recv() => {
                match notification {
                    // Notifications carry server state: only for
                    // authenticated connections. Our own requests' were
                    // answered by their responses.
                    Ok(n) if !authenticated || n.origin == Some(connection_id) => {}
                    Ok(n) => {
                        if socket.send(Message::Text(n.message.to_string().into())).await.is_err() { break }
                    }
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        tracing::warn!(missed, "WebSocket control client missed notifications");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use snapcast_server::ServerCommand;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Build an `AppState` with real (but unread) channels, matching how
    /// `run_http` wires one.
    fn make_state(auth_config: AuthConfig) -> (AppState, mpsc::Receiver<ServerCommand>) {
        let (notify_tx, _) = broadcast::channel::<Notification>(16);
        let (cmd_tx, cmd_rx) = mpsc::channel::<ServerCommand>(16);
        let state = AppState {
            notify_tx,
            auth_config: Arc::new(auth_config),
            cmd_tx,
            client_acceptor: snapcast_server::SnapServer::new(Default::default())
                .0
                .client_acceptor(),
        };
        (state, cmd_rx)
    }

    /// A config with auth enabled, a usable signing secret and user `bob:pw`.
    fn enabled_auth() -> AuthConfig {
        AuthConfig {
            enabled: true,
            secret: "test-secret-must-be-32-bytes-long".into(),
            users: vec!["bob:pw".parse().unwrap()],
        }
    }

    /// Serve `state` on an ephemeral port and return the port.
    async fn serve(state: AppState) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(
                listener,
                router(state, None).into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
        });
        port
    }

    /// POST `body` to /jsonrpc and return (status code, response body).
    async fn post(port: u16, body: &str, auth: Option<&str>) -> (u16, String) {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        let auth = auth.map_or(String::new(), |a| format!("Authorization: {a}\r\n"));
        let request = format!(
            "POST /jsonrpc HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\
             {auth}Connection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        let status = response[9..12].parse().unwrap();
        let body = response
            .split_once("\r\n\r\n")
            .map_or("", |(_, b)| b)
            .to_string();
        (status, body)
    }

    #[test]
    fn app_state_is_clone_and_shares_auth_arc() {
        // `run_http` relies on AppState: Clone (Router::with_state clones per
        // request) and on the auth_config Arc being shared, not deep-copied.
        let (state, _c) = make_state(enabled_auth());
        let clone = state.clone();
        assert!(Arc::ptr_eq(&state.auth_config, &clone.auth_config));
    }

    // --- POST /jsonrpc (end-to-end) ---------------------------------------

    /// Regression: an unknown method was answered with `"id": null`.
    #[tokio::test]
    async fn post_unknown_method_echoes_request_id() {
        let (state, _c) = make_state(AuthConfig::default());
        let port = serve(state).await;
        let (status, body) = post(
            port,
            r#"{"jsonrpc":"2.0","id":5,"method":"Custom.DoThing"}"#,
            None,
        )
        .await;
        assert_eq!(status, 200);
        let reply: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(reply["id"], 5);
        assert_eq!(reply["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn post_notification_gets_no_content() {
        let (state, _c) = make_state(AuthConfig::default());
        let port = serve(state).await;
        let (status, body) = post(
            port,
            r#"{"jsonrpc":"2.0","method":"Server.GetRPCVersion"}"#,
            None,
        )
        .await;
        assert_eq!(status, 204);
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn post_requires_credentials_when_auth_enabled() {
        let (state, _c) = make_state(enabled_auth());
        let token = crate::auth::generate_token(&state.auth_config, "bob").unwrap();
        let port = serve(state).await;
        let request = r#"{"jsonrpc":"2.0","id":1,"method":"Server.GetRPCVersion"}"#;

        // "Ym9iOnB3" is base64("bob:pw"), "Ym9iOng=" base64("bob:x").
        for auth in [None, Some("Bearer not-a-jwt"), Some("Basic Ym9iOng=")] {
            let (status, body) = post(port, request, auth).await;
            assert_eq!(status, 401, "{auth:?}");
            let reply: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(
                reply["error"],
                serde_json::json!({"code": 401, "message": "Unauthorized"})
            );
        }

        for auth in [format!("Bearer {token}"), "Basic Ym9iOnB3".to_string()] {
            let (status, body) = post(port, request, Some(&auth)).await;
            assert_eq!(status, 200, "{auth}");
            let reply: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(reply["result"]["major"], 2);
        }
    }

    /// Open a WebSocket control connection to /jsonrpc.
    async fn ws_connect(
        port: u16,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/jsonrpc"))
            .await
            .unwrap()
            .0
    }

    /// The next text message on a WebSocket control connection, as JSON.
    async fn ws_next<S>(ws: &mut S) -> Value
    where
        S: futures_util::Stream<
                Item = Result<
                    tokio_tungstenite::tungstenite::Message,
                    tokio_tungstenite::tungstenite::Error,
                >,
            > + Unpin,
    {
        use futures_util::StreamExt;
        let deadline = std::time::Duration::from_secs(5);
        loop {
            let msg = tokio::time::timeout(deadline, ws.next())
                .await
                .expect("no message on the WebSocket")
                .expect("WebSocket closed")
                .unwrap();
            if let tokio_tungstenite::tungstenite::Message::Text(text) = msg {
                return serde_json::from_str(&text).unwrap();
            }
        }
    }

    async fn ws_send<S>(ws: &mut S, request: Value)
    where
        S: futures_util::Sink<tokio_tungstenite::tungstenite::Message> + Unpin,
        S::Error: std::fmt::Debug,
    {
        use futures_util::SinkExt;
        ws.send(tokio_tungstenite::tungstenite::Message::text(
            request.to_string(),
        ))
        .await
        .unwrap();
    }

    /// Wait until every subscriber has taken the queued notifications.
    async fn wait_delivered(notify_tx: &broadcast::Sender<Notification>) {
        while !notify_tx.is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    /// Regression: an unauthenticated WebSocket control connection received
    /// every change notification.
    #[tokio::test]
    async fn websocket_notifications_need_authentication() {
        let (state, _c) = make_state(enabled_auth());
        let notify_tx = state.notify_tx.clone();
        let port = serve(state).await;
        let mut ws = ws_connect(port).await;

        // A reply proves the connection is set up and subscribed.
        ws_send(
            &mut ws,
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "Server.GetRPCVersion"}),
        )
        .await;
        assert_eq!(ws_next(&mut ws).await["result"]["major"], 2);

        notify_tx
            .send(Notification {
                origin: None,
                message: serde_json::json!({"method": "Test.Hidden"}),
            })
            .unwrap();
        wait_delivered(&notify_tx).await;

        ws_send(
            &mut ws,
            serde_json::json!({
                "jsonrpc": "2.0", "id": 2, "method": "Server.Authenticate",
                "params": {"scheme": "Plain", "param": "bob:pw"}
            }),
        )
        .await;
        assert_eq!(ws_next(&mut ws).await["result"], "ok");

        notify_tx
            .send(Notification {
                origin: None,
                message: serde_json::json!({"method": "Test.Shown"}),
            })
            .unwrap();
        assert_eq!(ws_next(&mut ws).await["method"], "Test.Shown");
    }

    /// Like C++ snapserver, a control connection gets the response to its own
    /// change request but not the notification; the others get it, and
    /// changes made over HTTP are announced to every connection.
    #[tokio::test]
    async fn websocket_requester_gets_no_echo_of_its_change() {
        let (mut server, events) =
            snapcast_server::SnapServer::new(snapcast_server::ServerConfig::default());
        let _audio_tx = server.add_stream("default");
        let audio_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (mut state, _c) = make_state(AuthConfig::default());
        state.cmd_tx = server.command_sender();
        tokio::spawn(notify::forward_events(
            events,
            server.command_sender(),
            state.notify_tx.clone(),
        ));
        tokio::spawn(async move { server.serve(audio_listener).await });
        let port = serve(state).await;

        let mut a = ws_connect(port).await;
        let mut b = ws_connect(port).await;
        for ws in [&mut a, &mut b] {
            // A reply proves the connection is set up and subscribed.
            ws_send(
                ws,
                serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "Server.GetRPCVersion"}),
            )
            .await;
            assert_eq!(ws_next(ws).await["id"], 1);
        }

        ws_send(
            &mut a,
            serde_json::json!({
                "jsonrpc": "2.0", "id": 2, "method": "Client.SetVolume",
                "params": {"id": "c1", "volume": {"percent": 30, "muted": false}}
            }),
        )
        .await;
        let reply = ws_next(&mut a).await;
        assert_eq!(reply["id"], 2);
        assert_eq!(reply["result"]["volume"]["percent"], 30);
        let n = ws_next(&mut b).await;
        assert_eq!(n["method"], "Client.OnVolumeChanged");
        assert_eq!(n["params"]["volume"]["percent"], 30);

        // Notifications are delivered in order: had A been sent its own
        // Client.OnVolumeChanged, it would come before this one.
        let (status, _) = post(
            port,
            r#"{"jsonrpc":"2.0","id":3,"method":"Client.SetName","params":{"id":"c1","name":"Den"}}"#,
            None,
        )
        .await;
        assert_eq!(status, 200);
        for ws in [&mut a, &mut b] {
            let n = ws_next(ws).await;
            assert_eq!(n["method"], "Client.OnNameChanged");
            assert_eq!(n["params"]["name"], "Den");
        }
    }

    // --- WebSocket streaming endpoint (end-to-end) ---------------------------

    /// A real snapcast-client connects over `ws://.../stream` to a real
    /// SnapServer behind the HTTP router, completes the handshake, syncs time
    /// and receives decoded audio.
    #[tokio::test]
    async fn websocket_stream_client_receives_audio() {
        use snapcast_client::{ClientConfig, ClientEvent, SnapClient};

        let (mut server, _server_events) =
            snapcast_server::SnapServer::new(snapcast_server::ServerConfig::default());
        let audio_tx = server.add_stream("default");
        let acceptor = server.client_acceptor();
        let audio_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        tokio::spawn(async move { server.serve(audio_listener).await });

        let (mut state, _c) = make_state(AuthConfig::default());
        state.client_acceptor = acceptor;
        let http_port = serve(state).await;

        let (mut client, mut events, mut client_audio) = SnapClient::new(ClientConfig {
            scheme: snapcast_proto::SCHEME_WS.into(),
            host: "127.0.0.1".into(),
            port: http_port,
            ..ClientConfig::default()
        });
        tokio::spawn(async move { client.run().await });

        let wait = |secs| tokio::time::Instant::now() + std::time::Duration::from_secs(secs);
        let deadline = wait(5);
        loop {
            match tokio::time::timeout_at(deadline, events.recv()).await {
                Ok(Some(ClientEvent::StreamStarted { codec, .. })) => {
                    assert_eq!(codec, "flac");
                    break;
                }
                Ok(Some(_)) => {}
                other => panic!("no StreamStarted over WebSocket: {other:?}"),
            }
        }
        let deadline = wait(10);
        loop {
            match tokio::time::timeout_at(deadline, events.recv()).await {
                Ok(Some(ClientEvent::TimeSyncComplete { .. })) => break,
                Ok(Some(_)) => {}
                other => panic!("no time sync over WebSocket: {other:?}"),
            }
        }

        let samples: Vec<f32> = (0..2304).map(|i| ((i as f32) * 0.01).sin() * 0.5).collect();
        let mut ts = 1_000_000_000;
        for _ in 0..20 {
            audio_tx
                .send(snapcast_server::AudioFrame {
                    data: snapcast_server::AudioData::F32(samples.clone()),
                    timestamp_usec: ts,
                })
                .await
                .unwrap();
            ts += 24_000;
        }

        let frame = tokio::time::timeout_at(wait(5), client_audio.recv())
            .await
            .expect("no audio over WebSocket")
            .expect("audio channel closed");
        assert_eq!(frame.sample_rate, 48000);
        assert_eq!(frame.channels, 2);
        assert!(!frame.samples.is_empty());
    }
}
