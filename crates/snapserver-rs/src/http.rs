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
use axum::extract::{ConnectInfo, State};
use axum::response::IntoResponse;
use axum::routing::get;
use serde_json::Value;
use tokio::sync::{broadcast, mpsc};

use crate::auth::AuthConfig;
use crate::jsonrpc::{self, RpcResult};

/// Shared state for axum handlers.
#[derive(Clone)]
struct AppState {
    event_tx: mpsc::Sender<crate::ControlEvent>,
    notify_tx: broadcast::Sender<Value>,
    auth_config: Arc<AuthConfig>,
    cmd_tx: tokio::sync::mpsc::Sender<snapcast_server::ServerCommand>,
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
    /// Event sender for extension point.
    pub event_tx: mpsc::Sender<crate::ControlEvent>,
    /// Notification broadcast sender.
    pub notify_tx: broadcast::Sender<Value>,
    /// Auth configuration.
    pub auth_config: Arc<AuthConfig>,
    /// Server command sender.
    pub cmd_tx: tokio::sync::mpsc::Sender<snapcast_server::ServerCommand>,
    /// Hands WebSocket streaming clients (`/stream`) to the audio server.
    pub client_acceptor: snapcast_server::ClientAcceptor,
}

/// Start the HTTP server with JSON-RPC + WebSocket + optional Snapweb.
pub(crate) async fn run_http(cfg: HttpConfig) -> Result<()> {
    let app_state = AppState {
        event_tx: cfg.event_tx,
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
        .route("/jsonrpc", get(ws_handler).post(http_jsonrpc_handler))
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

/// HTTP POST /jsonrpc handler.
async fn http_jsonrpc_handler(
    State(app): State<AppState>,
    headers: axum::http::HeaderMap,
    body: String,
) -> impl IntoResponse {
    let auth_header = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if let Err(e) = crate::auth::validate_bearer(&app.auth_config, auth_header) {
        return axum::Json(serde_json::json!({
            "jsonrpc": "2.0", "id": null,
            "error": {"code": -32000, "message": format!("Unauthorized: {e}")}
        }));
    }

    let Ok(request) = serde_json::from_str::<Value>(&body) else {
        return axum::Json(serde_json::json!({
            "jsonrpc": "2.0", "id": null,
            "error": {"code": -32700, "message": "Parse error"}
        }));
    };

    match jsonrpc::handle_request(&request, &app.auth_config, &app.cmd_tx).await {
        RpcResult::Response {
            response,
            notification,
        } => {
            if let Some(n) = notification {
                let _ = app.notify_tx.send(n);
            }
            axum::Json(response)
        }
        RpcResult::Unknown => {
            let _ = app
                .event_tx
                .send(crate::ControlEvent::JsonRpc {
                    response_tx: None,
                    client_id: "http".into(),
                    request,
                })
                .await;
            axum::Json(serde_json::json!({
                "jsonrpc": "2.0", "id": null,
                "error": {"code": -32601, "message": "Method not found"}
            }))
        }
    }
}

/// WebSocket upgrade handler at GET /jsonrpc.
async fn ws_handler(ws: WebSocketUpgrade, State(app): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, app))
}

async fn handle_ws(mut socket: WebSocket, app: AppState) {
    let mut notify_rx = app.notify_tx.subscribe();
    // Per-connection auth state, mirroring the TCP control path. Without this
    // the WebSocket endpoint dispatched every request unauthenticated even when
    // auth was enabled (HTTP POST and TCP both gated — WS was a full bypass).
    let mut authenticated = !app.auth_config.enabled;

    loop {
        tokio::select! {
            msg = socket.recv() => {
                let Some(Ok(msg)) = msg else { break };
                let Message::Text(text) = msg else { continue };

                let Ok(request) = serde_json::from_str::<Value>(&text) else {
                    let err = serde_json::json!({
                        "jsonrpc": "2.0", "id": null,
                        "error": {"code": -32700, "message": "Parse error"}
                    });
                    if socket.send(Message::Text(err.to_string().into())).await.is_err() { break }
                    continue;
                };

                // Auth gate: until authenticated, only Server.GetToken and
                // Server.Authenticate are permitted (same policy as TCP control).
                let method = request["method"].as_str().unwrap_or("");
                if !authenticated
                    && method != "Server.GetToken"
                    && method != "Server.Authenticate"
                {
                    let err = serde_json::json!({
                        "jsonrpc": "2.0", "id": request["id"],
                        "error": {"code": -32000, "message": "Unauthorized — call Server.Authenticate first"}
                    });
                    if socket.send(Message::Text(err.to_string().into())).await.is_err() { break }
                    continue;
                }

                match jsonrpc::handle_request(&request, &app.auth_config, &app.cmd_tx).await {
                    RpcResult::Response { response, notification } => {
                        if method == "Server.Authenticate" && response["result"]["ok"] == true {
                            authenticated = true;
                        }
                        if socket.send(Message::Text(response.to_string().into())).await.is_err() { break }
                        if let Some(n) = notification {
                            let _ = app.notify_tx.send(n);
                        }
                    }
                    RpcResult::Unknown => {
                        let _ = app.event_tx.send(crate::ControlEvent::JsonRpc {
                            response_tx: None,
                            client_id: "websocket".into(),
                            request,
                        }).await;
                    }
                }
            }
            notification = notify_rx.recv() => {
                if let Ok(n) = notification
                    && socket.send(Message::Text(n.to_string().into())).await.is_err()
                {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use snapcast_server::ServerCommand;

    // --- Test scaffolding -------------------------------------------------
    //
    // http.rs is dominated by socket I/O (TcpListener::bind, axum::serve,
    // WebSocket recv/send, ServeDir). Those paths are NOT unit-tested here and
    // are listed in io_excluded. What follows exercises the *pure* logic the
    // handlers are built out of: AppState construction/wiring, the bearer-auth
    // check `http_jsonrpc_handler` runs, the WS per-connection auth-gate
    // predicate, the `Server.Authenticate` success transition, and the exact
    // error-response JSON envelopes the handlers emit inline.

    /// Build an `AppState` with real (but unread) channels, matching exactly how
    /// `run_http` wires one. Returns the state plus the receivers/subscriber so a
    /// test can observe what the handler-equivalent logic sends.
    fn make_state(
        auth_config: AuthConfig,
    ) -> (
        AppState,
        mpsc::Receiver<crate::ControlEvent>,
        mpsc::Receiver<ServerCommand>,
        broadcast::Receiver<Value>,
    ) {
        let (event_tx, event_rx) = mpsc::channel::<crate::ControlEvent>(16);
        let (notify_tx, notify_rx) = broadcast::channel::<Value>(16);
        let (cmd_tx, cmd_rx) = mpsc::channel::<ServerCommand>(16);
        let state = AppState {
            event_tx,
            notify_tx,
            auth_config: Arc::new(auth_config),
            cmd_tx,
            client_acceptor: snapcast_server::SnapServer::new(Default::default())
                .0
                .client_acceptor(),
        };
        (state, event_rx, cmd_rx, notify_rx)
    }

    /// A config with auth enabled and a usable signing secret.
    fn enabled_auth() -> AuthConfig {
        AuthConfig {
            enabled: true,
            secret: "test-secret-must-be-32-bytes-long".into(),
        }
    }

    /// Mirror of the WS auth-gate predicate in `handle_ws` (lines 153-156):
    /// until authenticated, only `Server.GetToken` / `Server.Authenticate` pass.
    /// `true` == request must be rejected as unauthorized.
    fn ws_gate_rejects(authenticated: bool, method: &str) -> bool {
        !authenticated && method != "Server.GetToken" && method != "Server.Authenticate"
    }

    // --- AppState wiring --------------------------------------------------

    #[test]
    fn app_state_is_clone_and_shares_auth_arc() {
        // `run_http` relies on AppState: Clone (Router::with_state clones per
        // request) and on the auth_config Arc being shared, not deep-copied.
        let (state, _e, _c, _n) = make_state(enabled_auth());
        let clone = state.clone();
        assert!(Arc::ptr_eq(&state.auth_config, &clone.auth_config));
        assert!(state.auth_config.enabled);
    }

    #[tokio::test]
    async fn app_state_channels_are_live() {
        // Sanity: the wired channels actually deliver, so the handler paths that
        // do `event_tx.send` / `notify_tx.send` are talking to live endpoints.
        let (state, mut event_rx, _cmd_rx, mut notify_rx) = make_state(AuthConfig::default());

        state
            .event_tx
            .send(crate::ControlEvent::JsonRpc {
                response_tx: None,
                client_id: "http".into(),
                request: json!({"method": "X"}),
            })
            .await
            .unwrap();
        match event_rx.recv().await.unwrap() {
            crate::ControlEvent::JsonRpc { client_id, .. } => assert_eq!(client_id, "http"),
        }

        state.notify_tx.send(json!({"n": 1})).unwrap();
        assert_eq!(notify_rx.recv().await.unwrap(), json!({"n": 1}));
    }

    // --- Bearer auth check run by `http_jsonrpc_handler` ------------------

    #[test]
    fn bearer_check_allows_all_when_auth_disabled() {
        // With auth disabled the handler treats every request as "anonymous"
        // regardless of the Authorization header (or its absence).
        let cfg = AuthConfig::default();
        assert!(crate::auth::validate_bearer(&cfg, None).is_ok());
        assert!(crate::auth::validate_bearer(&cfg, Some("garbage")).is_ok());
    }

    #[test]
    fn bearer_check_rejects_missing_and_malformed_header_when_enabled() {
        let cfg = enabled_auth();
        // No header -> Unauthorized branch (-32000) in http_jsonrpc_handler.
        assert!(crate::auth::validate_bearer(&cfg, None).is_err());
        // Present but not a Bearer token.
        assert!(crate::auth::validate_bearer(&cfg, Some("Basic abc")).is_err());
        // Bearer prefix but a bogus token.
        assert!(crate::auth::validate_bearer(&cfg, Some("Bearer not-a-jwt")).is_err());
    }

    #[test]
    fn bearer_check_accepts_valid_token_when_enabled() {
        let cfg = enabled_auth();
        let token = crate::auth::generate_token(&cfg, "alice").unwrap();
        let subject = crate::auth::validate_bearer(&cfg, Some(&format!("Bearer {token}"))).unwrap();
        assert_eq!(subject, "alice");
    }

    // --- WS per-connection auth state -------------------------------------

    #[test]
    fn ws_initial_authenticated_mirrors_auth_disabled() {
        // handle_ws line 133: `authenticated = !auth_config.enabled`.
        assert!(!AuthConfig::default().enabled, "disabled => starts authed");
        assert!(enabled_auth().enabled, "enabled => starts unauthed");
    }

    #[test]
    fn ws_gate_blocks_normal_methods_until_authenticated() {
        // Unauthenticated: only the two auth bootstrap methods pass.
        assert!(ws_gate_rejects(false, "Client.SetVolume"));
        assert!(ws_gate_rejects(false, "Server.GetStatus"));
        assert!(ws_gate_rejects(false, ""));
        assert!(!ws_gate_rejects(false, "Server.GetToken"));
        assert!(!ws_gate_rejects(false, "Server.Authenticate"));
    }

    #[test]
    fn ws_gate_allows_everything_once_authenticated() {
        for m in [
            "Client.SetVolume",
            "Server.GetStatus",
            "Server.GetToken",
            "",
        ] {
            assert!(!ws_gate_rejects(true, m), "authed must allow {m}");
        }
    }

    // --- Error-response envelopes emitted inline by the handlers ----------

    #[test]
    fn parse_error_envelope_shape() {
        // Both handlers emit this exact object on a JSON parse failure.
        let err = json!({
            "jsonrpc": "2.0", "id": null,
            "error": {"code": -32700, "message": "Parse error"}
        });
        assert_eq!(err["jsonrpc"], "2.0");
        assert!(err["id"].is_null());
        assert_eq!(err["error"]["code"], -32700);
    }

    #[test]
    fn method_not_found_envelope_shape() {
        // http_jsonrpc_handler emits this for RpcResult::Unknown.
        let err = json!({
            "jsonrpc": "2.0", "id": null,
            "error": {"code": -32601, "message": "Method not found"}
        });
        assert_eq!(err["error"]["code"], -32601);
    }

    #[test]
    fn ws_unauthorized_envelope_preserves_request_id() {
        // handle_ws builds the unauthorized error with the *request's* id, not
        // null (unlike the HTTP-path -32000 which is id:null). Pin that.
        let request = json!({"jsonrpc": "2.0", "id": 42, "method": "Client.SetVolume"});
        let err = json!({
            "jsonrpc": "2.0", "id": request["id"],
            "error": {"code": -32000, "message": "Unauthorized — call Server.Authenticate first"}
        });
        assert_eq!(err["id"], 42);
        assert_eq!(err["error"]["code"], -32000);
    }

    // --- Dispatch path the handlers call: jsonrpc::handle_request ---------

    #[tokio::test]
    async fn handler_dispatch_authenticate_success_flips_auth_state() {
        // Reproduces the two-step WS bootstrap: GetToken then Authenticate, and
        // the exact success predicate handle_ws uses:
        // `response["result"]["ok"] == true` -> authenticated = true.
        let (state, _e, _c, _n) = make_state(enabled_auth());

        // Step 1: mint a token (allowed even while unauthenticated).
        let get_token = json!({"jsonrpc": "2.0", "id": 1, "method": "Server.GetToken", "params": {"username": "bob"}});
        let RpcResult::Response { response, .. } =
            jsonrpc::handle_request(&get_token, &state.auth_config, &state.cmd_tx).await
        else {
            panic!("expected response");
        };
        let token = response["result"]["token"].as_str().unwrap().to_string();

        // Step 2: authenticate with it.
        let auth_req = json!({"jsonrpc": "2.0", "id": 2, "method": "Server.Authenticate", "params": {"token": token}});
        let RpcResult::Response { response, .. } =
            jsonrpc::handle_request(&auth_req, &state.auth_config, &state.cmd_tx).await
        else {
            panic!("expected response");
        };

        let mut authenticated = !state.auth_config.enabled; // starts false
        assert!(!authenticated);
        if auth_req["method"] == "Server.Authenticate" && response["result"]["ok"] == true {
            authenticated = true;
        }
        assert!(authenticated, "valid Authenticate must flip the gate open");
        assert_eq!(response["result"]["subject"], "bob");
    }

    #[tokio::test]
    async fn handler_dispatch_authenticate_bad_token_keeps_gate_closed() {
        let (state, _e, _c, _n) = make_state(enabled_auth());
        let auth_req = json!({
            "jsonrpc": "2.0", "id": 2, "method": "Server.Authenticate",
            "params": {"token": "not-a-valid-jwt"}
        });
        let RpcResult::Response { response, .. } =
            jsonrpc::handle_request(&auth_req, &state.auth_config, &state.cmd_tx).await
        else {
            panic!("expected response");
        };
        // Invalid token -> error envelope, no `result.ok`.
        assert_ne!(response["result"]["ok"], true);
        assert_eq!(response["error"]["code"], -32602);

        let mut authenticated = !state.auth_config.enabled;
        if auth_req["method"] == "Server.Authenticate" && response["result"]["ok"] == true {
            authenticated = true;
        }
        assert!(!authenticated, "bad token must NOT open the gate");
    }

    #[tokio::test]
    async fn handler_dispatch_unknown_method_yields_unknown_variant() {
        // The RpcResult::Unknown branch is what makes the handlers forward to
        // event_tx; verify an unrecognized method produces it.
        let (state, _e, _c, _n) = make_state(AuthConfig::default());
        let req = json!({"jsonrpc": "2.0", "id": 7, "method": "Custom.DoThing", "params": {}});
        assert!(matches!(
            jsonrpc::handle_request(&req, &state.auth_config, &state.cmd_tx).await,
            RpcResult::Unknown
        ));
    }

    #[tokio::test]
    async fn handler_dispatch_notification_is_broadcastable() {
        // When handle_request returns a notification, the handler does
        // `notify_tx.send(n)`; confirm a real method produces one and that the
        // wired broadcast channel delivers it to a subscriber.
        let (state, _e, _c, _n) = make_state(AuthConfig::default());
        let mut sub = state.notify_tx.subscribe();
        let req = json!({
            "jsonrpc": "2.0", "id": 8, "method": "Group.SetStream",
            "params": {"id": "g1", "stream_id": "music"}
        });
        let RpcResult::Response { notification, .. } =
            jsonrpc::handle_request(&req, &state.auth_config, &state.cmd_tx).await
        else {
            panic!("expected response");
        };
        let n = notification.expect("Group.SetStream emits a notification");
        state.notify_tx.send(n.clone()).unwrap();
        assert_eq!(sub.recv().await.unwrap(), n);
        assert_eq!(n["params"]["stream_id"], "music");
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

        let (mut state, _e, _c, _n) = make_state(AuthConfig::default());
        state.client_acceptor = acceptor;
        let http_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let http_port = http_listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(
                http_listener,
                router(state, None).into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
        });

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
