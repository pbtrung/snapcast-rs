//! JSON-RPC control API — method handlers for Snapcast control protocol.
//!
//! Change notifications (`Client.OnVolumeChanged`, `Server.OnUpdate`, ...)
//! are not built here: every mutating command makes the library emit a
//! `ServerEvent`, which `main.rs` turns into the notification for all control
//! clients. Building them here as well delivered each notification twice.

use serde_json::{Value, json};
use snapcast_server::ServerCommand;
use tokio::sync::mpsc;

use crate::auth::{self, AuthConfig};

/// Largest accepted JSON-RPC message (TCP line, WebSocket message or HTTP
/// body), so a peer cannot make the server buffer unbounded input.
pub(crate) const MAX_REQUEST_LEN: usize = 1024 * 1024;

/// JSON-RPC error codes.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;
/// Not authenticated, or the credentials were rejected (as C++ snapserver).
pub(crate) const UNAUTHORIZED: i64 = 401;
pub(crate) const UNAUTHORIZED_MESSAGE: &str = "Unauthorized";

/// Bind a required string parameter, or return an `INVALID_PARAMS` error naming
/// the missing field. Replaces the `let Some(x) = params["k"].as_str() else {
/// return err(...) }` boilerplate repeated by every handler.
macro_rules! require_str {
    ($params:expr, $key:literal, $id:expr) => {
        match $params[$key].as_str() {
            Some(value) => value,
            None => return err($id, INVALID_PARAMS, concat!("missing '", $key, "'")),
        }
    };
}

/// Handle one raw JSON-RPC message from a control connection (a TCP line, a
/// WebSocket text message or an HTTP body) and return the reply, if any.
///
/// `authenticated` is the connection's auth state: until it is set, only
/// `Server.Authenticate`, `Server.GetToken` and `Server.GetRPCVersion` are
/// dispatched, and a successful `Server.Authenticate` sets it. Batches are answered with an
/// array. Notifications (requests without an `id`) are ignored without a
/// reply, as in C++ snapserver.
pub(crate) async fn handle_message(
    text: &str,
    authenticated: &mut bool,
    auth_config: &AuthConfig,
    cmd_tx: &mpsc::Sender<ServerCommand>,
) -> Option<Value> {
    let Ok(message) = serde_json::from_str::<Value>(text) else {
        return Some(err(&Value::Null, PARSE_ERROR, "Parse error"));
    };
    match message {
        Value::Array(batch) if batch.is_empty() => {
            Some(err(&Value::Null, INVALID_REQUEST, "Invalid Request"))
        }
        Value::Array(batch) => {
            let mut responses = Vec::new();
            for entry in &batch {
                if let Some(response) =
                    handle_entry(entry, authenticated, auth_config, cmd_tx).await
                {
                    responses.push(response);
                }
            }
            (!responses.is_empty()).then_some(Value::Array(responses))
        }
        single => handle_entry(&single, authenticated, auth_config, cmd_tx).await,
    }
}

/// Handle one request object of a message or batch.
async fn handle_entry(
    request: &Value,
    authenticated: &mut bool,
    auth_config: &AuthConfig,
    cmd_tx: &mpsc::Sender<ServerCommand>,
) -> Option<Value> {
    let Some(object) = request.as_object() else {
        return Some(err(&Value::Null, INVALID_REQUEST, "Invalid Request"));
    };
    let Some(id) = object.get("id") else {
        tracing::debug!(?request, "Ignoring JSON-RPC notification");
        return None;
    };
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return Some(err(id, INVALID_REQUEST, "Invalid Request"));
    };
    if !*authenticated
        && !matches!(
            method,
            "Server.Authenticate" | "Server.GetToken" | "Server.GetRPCVersion"
        )
    {
        return Some(err(id, UNAUTHORIZED, UNAUTHORIZED_MESSAGE));
    }
    let response = handle_request(request, auth_config, cmd_tx).await;
    if method == "Server.Authenticate" && response["result"] == "ok" {
        *authenticated = true;
    }
    Some(response)
}

/// Fetch server status via GetStatus command, serialized to JSON.
async fn get_status(cmd_tx: &mpsc::Sender<ServerCommand>) -> Option<Value> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    cmd_tx
        .send(ServerCommand::GetStatus { response_tx: tx })
        .await
        .ok()?;
    let status = rx.await.ok()?;
    serde_json::to_value(status).ok()
}

/// Find a client in a serialized server status by ID.
pub(crate) fn find_client<'a>(status: &'a Value, client_id: &str) -> Option<&'a Value> {
    status["server"]["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|g| g["clients"].as_array().into_iter().flatten())
        .find(|c| c["id"].as_str() == Some(client_id))
}

/// Handle a JSON-RPC request object and return its response. All state
/// access goes through ServerCommand.
pub(crate) async fn handle_request(
    request: &Value,
    auth_config: &AuthConfig,
    cmd_tx: &mpsc::Sender<ServerCommand>,
) -> Value {
    let id = &request["id"];
    let method = request["method"].as_str().unwrap_or("");
    let params = &request["params"];

    match method {
        // --- Server ---
        "Server.GetRPCVersion" => ok(id, json!({"major": 2, "minor": 0, "patch": 0})),
        "Server.GetStatus" => match get_status(cmd_tx).await {
            Some(status) => ok(id, status),
            None => err(id, INTERNAL_ERROR, "status unavailable"),
        },
        "Server.DeleteClient" => {
            let client_id = require_str!(params, "id", id);
            let _ = cmd_tx
                .send(ServerCommand::DeleteClient {
                    client_id: client_id.to_string(),
                })
                .await;
            // Like C++ snapserver, the result is the updated server status.
            match get_status(cmd_tx).await {
                Some(status) => ok(id, status),
                None => err(id, INTERNAL_ERROR, "status unavailable"),
            }
        }

        // --- Client ---
        "Client.GetStatus" => {
            let client_id = require_str!(params, "id", id);
            let Some(status) = get_status(cmd_tx).await else {
                return err(id, INTERNAL_ERROR, "status unavailable");
            };
            match find_client(&status, client_id) {
                Some(c) => ok(id, json!({"client": c})),
                None => err(id, INTERNAL_ERROR, "client not found"),
            }
        }
        "Client.SetVolume" => {
            let client_id = require_str!(params, "id", id);
            let requested = &params["volume"];
            let mut percent = requested["percent"].as_u64().map(|p| p.min(100) as u16);
            let mut muted = requested["muted"].as_bool();
            // Like C++ snapserver, a field left out keeps its current value,
            // so `{"muted": true}` mutes without touching the level.
            if (percent.is_none() || muted.is_none())
                && let Some(status) = get_status(cmd_tx).await
                && let Some(client) = find_client(&status, client_id)
            {
                let current = &client["config"]["volume"];
                percent = percent.or(current["percent"].as_u64().map(|p| p as u16));
                muted = muted.or(current["muted"].as_bool());
            }
            let (volume, muted) = (percent.unwrap_or(100), muted.unwrap_or(false));
            let _ = cmd_tx
                .send(ServerCommand::SetClientVolume {
                    client_id: client_id.to_string(),
                    volume,
                    muted,
                })
                .await;
            ok(id, json!({"volume": {"percent": volume, "muted": muted}}))
        }
        "Client.SetLatency" => {
            let client_id = require_str!(params, "id", id);
            let Some(latency) = params["latency"]
                .as_i64()
                .and_then(|l| i32::try_from(l).ok())
            else {
                return err(id, INVALID_PARAMS, "missing or invalid 'latency'");
            };
            let _ = cmd_tx
                .send(ServerCommand::SetClientLatency {
                    client_id: client_id.to_string(),
                    latency,
                })
                .await;
            ok(id, json!({"latency": latency}))
        }
        "Client.SetName" => {
            let client_id = require_str!(params, "id", id);
            let name = require_str!(params, "name", id);
            let _ = cmd_tx
                .send(ServerCommand::SetClientName {
                    client_id: client_id.to_string(),
                    name: name.to_string(),
                })
                .await;
            ok(id, json!({"name": name}))
        }

        // --- Group ---
        "Group.GetStatus" => {
            let group_id = require_str!(params, "id", id);
            let Some(status) = get_status(cmd_tx).await else {
                return err(id, INTERNAL_ERROR, "status unavailable");
            };
            let group = status["server"]["groups"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|g| g["id"].as_str() == Some(group_id));
            match group {
                Some(g) => ok(id, json!({"group": g})),
                None => err(id, INTERNAL_ERROR, "group not found"),
            }
        }
        "Group.SetMute" => {
            let group_id = require_str!(params, "id", id);
            let Some(muted) = params["mute"].as_bool() else {
                return err(id, INVALID_PARAMS, "missing 'mute'");
            };
            let _ = cmd_tx
                .send(ServerCommand::SetGroupMute {
                    group_id: group_id.to_string(),
                    muted,
                })
                .await;
            ok(id, json!({"mute": muted}))
        }
        "Group.SetStream" => {
            let group_id = require_str!(params, "id", id);
            let stream_id = require_str!(params, "stream_id", id);
            let _ = cmd_tx
                .send(ServerCommand::SetGroupStream {
                    group_id: group_id.to_string(),
                    stream_id: stream_id.to_string(),
                })
                .await;
            ok(id, json!({"stream_id": stream_id}))
        }
        "Group.SetClients" => {
            let group_id = require_str!(params, "id", id);
            let Some(clients) = params["clients"].as_array() else {
                return err(id, INVALID_PARAMS, "missing 'clients'");
            };
            let client_ids: Vec<String> = clients
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            let _ = cmd_tx
                .send(ServerCommand::SetGroupClients {
                    group_id: group_id.to_string(),
                    clients: client_ids,
                })
                .await;
            match get_status(cmd_tx).await {
                Some(status) => ok(id, status),
                None => err(id, INTERNAL_ERROR, "status unavailable"),
            }
        }
        "Group.SetName" => {
            let group_id = require_str!(params, "id", id);
            let name = require_str!(params, "name", id);
            let _ = cmd_tx
                .send(ServerCommand::SetGroupName {
                    group_id: group_id.to_string(),
                    name: name.to_string(),
                })
                .await;
            ok(id, json!({"name": name}))
        }

        // --- Stream ---
        "Stream.SetProperty" => {
            let stream_id = require_str!(params, "id", id);
            let metadata = params["properties"]
                .as_object()
                .map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default();
            let _ = cmd_tx
                .send(ServerCommand::SetStreamMeta {
                    stream_id: stream_id.to_string(),
                    metadata,
                })
                .await;
            ok(
                id,
                json!({"id": stream_id, "properties": &params["properties"]}),
            )
        }
        "Stream.Control" => {
            let stream_id = require_str!(params, "id", id);
            let command = require_str!(params, "command", id);
            let _ = cmd_tx
                .send(ServerCommand::StreamControl {
                    stream_id: stream_id.to_string(),
                    command: command.to_string(),
                    params: params["params"].clone(),
                })
                .await;
            ok(id, json!({"id": stream_id}))
        }
        "Stream.AddStream" => {
            let stream_uri = require_str!(params, "streamUri", id);
            let (tx, rx) = tokio::sync::oneshot::channel();
            let _ = cmd_tx
                .send(ServerCommand::AddStream {
                    uri: stream_uri.to_string(),
                    response_tx: tx,
                })
                .await;
            match rx.await {
                Ok(Ok(stream_id)) => ok(id, json!({"stream_id": stream_id})),
                Ok(Err(e)) => err(id, INVALID_PARAMS, &e),
                Err(_) => err(id, INTERNAL_ERROR, "command failed"),
            }
        }
        "Stream.RemoveStream" => {
            let stream_id = require_str!(params, "id", id);
            let _ = cmd_tx
                .send(ServerCommand::RemoveStream {
                    stream_id: stream_id.to_string(),
                })
                .await;
            ok(id, json!({"stream_id": stream_id}))
        }

        // --- Auth ---
        "Server.GetToken" => {
            let username = require_str!(params, "username", id);
            let password = require_str!(params, "password", id);
            if auth_config.secret.is_empty() {
                return err(id, INTERNAL_ERROR, "No auth secret configured");
            }
            if !auth::verify_credentials(auth_config, username, password) {
                return err(id, UNAUTHORIZED, UNAUTHORIZED_MESSAGE);
            }
            match auth::generate_token(auth_config, username) {
                Ok(token) => ok(id, json!({"token": token})),
                Err(e) => err(id, INTERNAL_ERROR, &format!("token generation failed: {e}")),
            }
        }
        "Server.Authenticate" => {
            // C++ snapserver's {"scheme", "param"}; a bare {"token"} is the
            // earlier form of a Bearer token.
            let (scheme, param) = match params["token"].as_str() {
                Some(token) if params.get("scheme").is_none() => ("Bearer", token),
                _ => (
                    require_str!(params, "scheme", id),
                    require_str!(params, "param", id),
                ),
            };
            if !auth_config.enabled {
                // Nothing to check: the connection is already authenticated.
                return ok(id, json!("ok"));
            }
            match auth::authenticate(auth_config, scheme, param) {
                Ok(user) => {
                    tracing::info!(user, "Control client authenticated");
                    ok(id, json!("ok"))
                }
                Err(auth::AuthFailure::Unauthorized) => err(id, UNAUTHORIZED, UNAUTHORIZED_MESSAGE),
                Err(auth::AuthFailure::UnsupportedScheme) => err(
                    id,
                    INVALID_PARAMS,
                    "unsupported scheme (expected Basic, Plain or Bearer)",
                ),
            }
        }

        _ => err(id, METHOD_NOT_FOUND, "Method not found"),
    }
}

fn ok(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn err(id: &Value, code: i64, msg: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg}})
}

#[cfg(test)]
mod tests {
    use super::*;
    use snapcast_server::{ServerCommand, status};

    /// Spawn a mock command handler that processes GetStatus and SetClientVolume.
    fn mock_server() -> (AuthConfig, tokio::sync::mpsc::Sender<ServerCommand>) {
        let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel::<ServerCommand>(16);
        tokio::spawn(async move {
            let mut volume: u16 = 100;
            let mut muted = false;
            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    ServerCommand::GetStatus { response_tx } => {
                        let _ = response_tx.send(status::ServerStatus {
                            server: status::Server {
                                groups: vec![status::Group {
                                    id: "g1".into(),
                                    stream_id: "default".into(),
                                    clients: vec![status::Client {
                                        id: "c1".into(),
                                        connected: true,
                                        config: status::ClientConfig {
                                            volume: status::Volume {
                                                percent: volume,
                                                muted,
                                            },
                                            ..Default::default()
                                        },
                                        host: status::Host {
                                            name: "host1".into(),
                                            mac: "mac1".into(),
                                            ..Default::default()
                                        },
                                        ..Default::default()
                                    }],
                                    ..Default::default()
                                }],
                                streams: vec![status::Stream {
                                    id: "default".into(),
                                    status: status::StreamStatus::Playing,
                                    ..Default::default()
                                }],
                                ..Default::default()
                            },
                        });
                    }
                    ServerCommand::SetClientVolume {
                        volume: v,
                        muted: m,
                        ..
                    } => {
                        volume = v;
                        muted = m;
                    }
                    _ => {}
                }
            }
        });
        (AuthConfig::default(), cmd_tx)
    }

    #[tokio::test]
    async fn server_get_status() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({"jsonrpc": "2.0", "id": 1, "method": "Server.GetStatus", "params": {}});
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert!(response["result"]["server"]["groups"].is_array());
    }

    #[tokio::test]
    async fn client_set_volume() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 2,
            "method": "Client.SetVolume",
            "params": {"id": "c1", "volume": {"percent": 50, "muted": true}}
        });
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert_eq!(response["result"]["volume"]["percent"], 50);

        // Verify state updated via GetStatus
        tokio::task::yield_now().await;
        let status = get_status(&cmd_tx).await.unwrap();
        assert_eq!(
            status["server"]["groups"][0]["clients"][0]["config"]["volume"]["percent"],
            50
        );
    }

    #[tokio::test]
    async fn unknown_method_is_method_not_found() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({"jsonrpc": "2.0", "id": 3, "method": "Client.SetEq", "params": {}});
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert_eq!(response["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(response["id"], 3);
    }

    #[tokio::test]
    async fn group_set_stream() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 4,
            "method": "Group.SetStream",
            "params": {"id": "g1", "stream_id": "music"}
        });
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert_eq!(response["result"]["stream_id"], "music");
    }

    // === Wave-4 additions ===================================================
    //
    // These cover the remaining request->response logic of every dispatchable
    // method: happy path plus the error paths (missing/invalid params, unknown
    // method, malformed method field, wrong id echoing). Socket transport and
    // the real ServerCommand executor are out of scope — the mock command sink
    // stands in for state. Fire-and-forget commands need no mock reply, so most
    // handlers exercise here via the shared `mock_server()`; handlers that await
    // a reply (`Stream.AddStream`) use the dedicated helper below.

    /// A command sink that answers `AddStream` with the supplied result and
    /// still serves `GetStatus`.
    fn mock_server_addstream(
        result: Result<String, String>,
    ) -> (AuthConfig, tokio::sync::mpsc::Sender<ServerCommand>) {
        let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel::<ServerCommand>(16);
        tokio::spawn(async move {
            let mut result = Some(result);
            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    ServerCommand::AddStream { response_tx, .. } => {
                        if let Some(r) = result.take() {
                            let _ = response_tx.send(r);
                        }
                    }
                    ServerCommand::GetStatus { response_tx } => {
                        let _ = response_tx.send(status::ServerStatus::default());
                    }
                    _ => {}
                }
            }
        });
        (AuthConfig::default(), cmd_tx)
    }

    /// An auth-enabled config with a real secret and the users `alice:se:cret`
    /// and `bob:pw`.
    fn auth_enabled() -> (AuthConfig, tokio::sync::mpsc::Sender<ServerCommand>) {
        let (_disabled, cmd_tx) = mock_server();
        let config = AuthConfig {
            enabled: true,
            secret: "wave4-test-secret-at-least-32-bytes!".into(),
            users: vec!["alice:se:cret".parse().unwrap(), "bob:pw".parse().unwrap()],
        };
        (config, cmd_tx)
    }

    // --- Envelope helpers ------------------------------------------------

    #[test]
    fn ok_envelope_shape() {
        let response = ok(&json!(7), json!({"a": 1}));
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 7);
        assert_eq!(response["result"]["a"], 1);
        assert!(response.get("error").is_none());
    }

    #[test]
    fn err_envelope_shape() {
        let response = err(&json!("abc"), INVALID_PARAMS, "boom");
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], "abc");
        assert_eq!(response["error"]["code"], INVALID_PARAMS);
        assert_eq!(response["error"]["message"], "boom");
        assert!(response.get("result").is_none());
    }

    // --- Server.* --------------------------------------------------------

    #[tokio::test]
    async fn server_get_rpc_version() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({"jsonrpc": "2.0", "id": 10, "method": "Server.GetRPCVersion"});
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert_eq!(
            response["result"],
            json!({"major": 2, "minor": 0, "patch": 0})
        );
    }

    #[tokio::test]
    async fn server_delete_client_returns_server_status() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 11,
            "method": "Server.DeleteClient", "params": {"id": "c1"}
        });
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        // C++ snapserver answers with the updated status, not the id.
        assert!(response["result"]["server"]["groups"].is_array());
        assert_eq!(response["id"], 11);
    }

    #[tokio::test]
    async fn server_delete_client_missing_id_is_invalid_params() {
        let (auth_config, cmd_tx) = mock_server();
        let req =
            json!({"jsonrpc": "2.0", "id": 12, "method": "Server.DeleteClient", "params": {}});
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert_eq!(response["error"]["code"], INVALID_PARAMS);
        assert_eq!(response["error"]["message"], "missing 'id'");
    }

    // --- Client.* --------------------------------------------------------

    #[tokio::test]
    async fn client_get_status_found() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 20,
            "method": "Client.GetStatus", "params": {"id": "c1"}
        });
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert_eq!(response["result"]["client"]["id"], "c1");
    }

    #[tokio::test]
    async fn client_get_status_not_found() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 21,
            "method": "Client.GetStatus", "params": {"id": "nope"}
        });
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert_eq!(response["error"]["code"], INTERNAL_ERROR);
        assert_eq!(response["error"]["message"], "client not found");
    }

    #[tokio::test]
    async fn client_get_status_missing_id() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({"jsonrpc": "2.0", "id": 22, "method": "Client.GetStatus", "params": {}});
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert_eq!(response["error"]["message"], "missing 'id'");
    }

    #[tokio::test]
    async fn client_set_volume_clamps_above_100() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 23,
            "method": "Client.SetVolume",
            "params": {"id": "c1", "volume": {"percent": 250, "muted": false}}
        });
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        // percent > 100 is clamped to 100.
        assert_eq!(response["result"]["volume"]["percent"], 100);
    }

    #[tokio::test]
    async fn client_set_volume_keeps_current_value_of_missing_fields() {
        let (auth_config, cmd_tx) = mock_server();
        let set = |id: u64, volume: Value| {
            json!({
                "jsonrpc": "2.0", "id": id,
                "method": "Client.SetVolume", "params": {"id": "c1", "volume": volume}
            })
        };
        let response =
            handle_request(&set(24, json!({"percent": 30})), &auth_config, &cmd_tx).await;
        assert_eq!(
            response["result"]["volume"],
            json!({"percent": 30, "muted": false})
        );

        // Muting alone must not reset the level (regression: it went to 100).
        let response =
            handle_request(&set(25, json!({"muted": true})), &auth_config, &cmd_tx).await;
        assert_eq!(
            response["result"]["volume"],
            json!({"percent": 30, "muted": true})
        );

        // A wrong-typed field counts as missing.
        let response = handle_request(
            &set(26, json!({"percent": 40, "muted": "no"})),
            &auth_config,
            &cmd_tx,
        )
        .await;
        assert_eq!(
            response["result"]["volume"],
            json!({"percent": 40, "muted": true})
        );

        // An unknown client falls back to 100 % unmuted.
        let req = json!({
            "jsonrpc": "2.0", "id": 27,
            "method": "Client.SetVolume", "params": {"id": "ghost"}
        });
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert_eq!(
            response["result"]["volume"],
            json!({"percent": 100, "muted": false})
        );
    }

    #[tokio::test]
    async fn client_set_volume_missing_id() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 26,
            "method": "Client.SetVolume", "params": {"volume": {"percent": 10}}
        });
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert_eq!(response["error"]["message"], "missing 'id'");
    }

    #[tokio::test]
    async fn client_set_latency_happy_path() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 27,
            "method": "Client.SetLatency", "params": {"id": "c1", "latency": 100}
        });
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert_eq!(response["result"]["latency"], 100);
    }

    #[tokio::test]
    async fn client_set_latency_requires_an_i32_latency() {
        let (auth_config, cmd_tx) = mock_server();
        for params in [
            json!({"id": "c1"}),
            json!({"id": "c1", "latency": "10"}),
            json!({"id": "c1", "latency": 1_i64 << 40}),
        ] {
            let req = json!({
                "jsonrpc": "2.0", "id": 28,
                "method": "Client.SetLatency", "params": params
            });
            let response = handle_request(&req, &auth_config, &cmd_tx).await;
            assert_eq!(response["error"]["code"], INVALID_PARAMS, "{params}");
        }
    }

    #[tokio::test]
    async fn client_set_name_happy_path() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 29,
            "method": "Client.SetName", "params": {"id": "c1", "name": "Kitchen"}
        });
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert_eq!(response["result"]["name"], "Kitchen");
    }

    #[tokio::test]
    async fn client_set_name_requires_name() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 30,
            "method": "Client.SetName", "params": {"id": "c1"}
        });
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert_eq!(response["error"]["message"], "missing 'name'");

        // An empty name is still a valid name.
        let req = json!({
            "jsonrpc": "2.0", "id": 31,
            "method": "Client.SetName", "params": {"id": "c1", "name": ""}
        });
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert_eq!(response["result"]["name"], "");
    }

    // --- Group.* ---------------------------------------------------------

    #[tokio::test]
    async fn group_get_status_found() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 40,
            "method": "Group.GetStatus", "params": {"id": "g1"}
        });
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert_eq!(response["result"]["group"]["id"], "g1");
    }

    #[tokio::test]
    async fn group_get_status_not_found() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 41,
            "method": "Group.GetStatus", "params": {"id": "ghost"}
        });
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert_eq!(response["error"]["message"], "group not found");
    }

    #[tokio::test]
    async fn group_set_mute_happy_path() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 42,
            "method": "Group.SetMute", "params": {"id": "g1", "mute": true}
        });
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        // Group uses the "mute" key (not "muted").
        assert_eq!(response["result"]["mute"], true);
    }

    #[tokio::test]
    async fn group_set_mute_requires_mute() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 43,
            "method": "Group.SetMute", "params": {"id": "g1"}
        });
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert_eq!(response["error"]["message"], "missing 'mute'");
    }

    #[tokio::test]
    async fn group_set_stream_missing_stream_id() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 44,
            "method": "Group.SetStream", "params": {"id": "g1"}
        });
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert_eq!(response["error"]["message"], "missing 'stream_id'");
    }

    #[tokio::test]
    async fn group_set_clients_returns_server_status() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 45,
            "method": "Group.SetClients",
            "params": {"id": "g1", "clients": ["c1", "c2"]}
        });
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        // Result is the fresh full status.
        assert!(response["result"]["server"]["groups"].is_array());
    }

    #[tokio::test]
    async fn group_set_clients_missing_array_is_invalid_params() {
        let (auth_config, cmd_tx) = mock_server();
        // `clients` is an object, not an array -> as_array() fails.
        let req = json!({
            "jsonrpc": "2.0", "id": 46,
            "method": "Group.SetClients",
            "params": {"id": "g1", "clients": {"not": "an array"}}
        });
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert_eq!(response["error"]["message"], "missing 'clients'");
    }

    #[tokio::test]
    async fn group_set_name_happy_path() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 47,
            "method": "Group.SetName", "params": {"id": "g1", "name": "Main Room"}
        });
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert_eq!(response["result"]["name"], "Main Room");
    }

    // --- Stream.* --------------------------------------------------------

    #[tokio::test]
    async fn stream_set_property_happy_path() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 50,
            "method": "Stream.SetProperty",
            "params": {"id": "default", "properties": {"artist": "Test"}}
        });
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert_eq!(response["result"]["id"], "default");
        assert_eq!(response["result"]["properties"]["artist"], "Test");
    }

    #[tokio::test]
    async fn stream_set_property_missing_id() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 51,
            "method": "Stream.SetProperty", "params": {"properties": {}}
        });
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert_eq!(response["error"]["message"], "missing 'id'");
    }

    #[tokio::test]
    async fn stream_control_happy_path() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 52,
            "method": "Stream.Control",
            "params": {"id": "default", "command": "next", "params": {}}
        });
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert_eq!(response["result"]["id"], "default");
    }

    #[tokio::test]
    async fn stream_control_missing_command() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 53,
            "method": "Stream.Control", "params": {"id": "default"}
        });
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert_eq!(response["error"]["message"], "missing 'command'");
    }

    #[tokio::test]
    async fn stream_add_stream_success() {
        let (auth_config, cmd_tx) = mock_server_addstream(Ok("stream-42".into()));
        let req = json!({
            "jsonrpc": "2.0", "id": 54,
            "method": "Stream.AddStream",
            "params": {"streamUri": "pipe:///tmp/snapfifo?name=default"}
        });
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert_eq!(response["result"]["stream_id"], "stream-42");
    }

    #[tokio::test]
    async fn stream_add_stream_backend_error() {
        let (auth_config, cmd_tx) = mock_server_addstream(Err("bad uri".into()));
        let req = json!({
            "jsonrpc": "2.0", "id": 55,
            "method": "Stream.AddStream", "params": {"streamUri": "bogus://x"}
        });
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert_eq!(response["error"]["code"], INVALID_PARAMS);
        assert_eq!(response["error"]["message"], "bad uri");
    }

    #[tokio::test]
    async fn stream_add_stream_missing_uri() {
        let (auth_config, cmd_tx) = mock_server_addstream(Ok("unused".into()));
        let req = json!({"jsonrpc": "2.0", "id": 56, "method": "Stream.AddStream", "params": {}});
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert_eq!(response["error"]["message"], "missing 'streamUri'");
    }

    #[tokio::test]
    async fn stream_remove_stream_happy_path() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": 57,
            "method": "Stream.RemoveStream", "params": {"id": "default"}
        });
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert_eq!(response["result"]["stream_id"], "default");
    }

    // --- Auth ------------------------------------------------------------

    fn request(id: u64, method: &str, params: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
    }

    fn assert_unauthorized(response: &Value) {
        assert_eq!(
            response["error"],
            json!({"code": 401, "message": "Unauthorized"}),
            "{response}"
        );
    }

    #[tokio::test]
    async fn server_get_token_checks_credentials() {
        let (auth_config, cmd_tx) = auth_enabled();
        let get_token = |username: &str, password: &str| {
            request(
                60,
                "Server.GetToken",
                json!({"username": username, "password": password}),
            )
        };
        let response = handle_request(&get_token("alice", "se:cret"), &auth_config, &cmd_tx).await;
        let token = response["result"]["token"].as_str().expect("token issued");
        // Round-trip: the issued token validates back to the requested subject.
        assert_eq!(auth::validate_token(&auth_config, token).unwrap(), "alice");

        // Unknown user and wrong password get the same answer.
        for (user, password) in [("alice", "wrong"), ("mallory", "se:cret"), ("bob", "")] {
            let response = handle_request(&get_token(user, password), &auth_config, &cmd_tx).await;
            assert_unauthorized(&response);
        }

        let response = handle_request(
            &request(61, "Server.GetToken", json!({"username": "alice"})),
            &auth_config,
            &cmd_tx,
        )
        .await;
        assert_eq!(response["error"]["message"], "missing 'password'");
    }

    #[tokio::test]
    async fn server_get_token_fails_without_secret() {
        // Default AuthConfig: auth disabled, no secret, no users.
        let (auth_config, cmd_tx) = mock_server();
        let req = request(
            62,
            "Server.GetToken",
            json!({"username": "bob", "password": "pw"}),
        );
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert_eq!(response["error"]["code"], INTERNAL_ERROR);
        assert_eq!(response["error"]["message"], "No auth secret configured");
    }

    #[tokio::test]
    async fn server_authenticate_accepts_basic_plain_and_bearer() {
        use base64::Engine;
        let (auth_config, cmd_tx) = auth_enabled();
        let token = auth::generate_token(&auth_config, "alice").unwrap();
        let basic = base64::engine::general_purpose::STANDARD.encode("alice:se:cret");
        for params in [
            json!({"scheme": "Basic", "param": basic}),
            json!({"scheme": "basic", "param": basic}),
            json!({"scheme": "Plain", "param": "bob:pw"}),
            json!({"scheme": "Bearer", "param": token}),
            json!({"token": token}),
        ] {
            let response = handle_request(
                &request(63, "Server.Authenticate", params.clone()),
                &auth_config,
                &cmd_tx,
            )
            .await;
            assert_eq!(response["result"], "ok", "{params}");
        }
    }

    #[tokio::test]
    async fn server_authenticate_rejects_bad_credentials_alike() {
        use base64::Engine;
        let (auth_config, cmd_tx) = auth_enabled();
        let b64 = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
        for params in [
            json!({"scheme": "Basic", "param": b64("alice:wrong")}),
            json!({"scheme": "Basic", "param": b64("mallory:se:cret")}),
            json!({"scheme": "Basic", "param": "%%% not base64"}),
            json!({"scheme": "Plain", "param": "bob:wrong"}),
            json!({"scheme": "Plain", "param": "nobody"}),
            json!({"scheme": "Bearer", "param": "not.a.jwt"}),
            json!({"token": "not.a.jwt"}),
        ] {
            let response = handle_request(
                &request(64, "Server.Authenticate", params.clone()),
                &auth_config,
                &cmd_tx,
            )
            .await;
            assert_unauthorized(&response);
        }
    }

    #[tokio::test]
    async fn server_authenticate_param_errors() {
        let (auth_config, cmd_tx) = auth_enabled();
        let response = handle_request(
            &request(
                65,
                "Server.Authenticate",
                json!({"scheme": "Digest", "param": "x"}),
            ),
            &auth_config,
            &cmd_tx,
        )
        .await;
        assert_eq!(response["error"]["code"], INVALID_PARAMS);

        let response = handle_request(
            &request(66, "Server.Authenticate", json!({})),
            &auth_config,
            &cmd_tx,
        )
        .await;
        assert_eq!(response["error"]["message"], "missing 'scheme'");
    }

    #[tokio::test]
    async fn server_authenticate_is_ok_when_auth_disabled() {
        let (auth_config, cmd_tx) = mock_server();
        let req = request(
            67,
            "Server.Authenticate",
            json!({"scheme": "Plain", "param": "anyone:anything"}),
        );
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert_eq!(response["result"], "ok");
    }

    // --- Dispatch edge cases --------------------------------------------

    #[tokio::test]
    async fn missing_or_non_string_method_is_invalid_request() {
        let (auth_config, cmd_tx) = mock_server();
        let mut authenticated = true;
        for text in [
            r#"{"jsonrpc": "2.0", "id": 70, "params": {}}"#,
            r#"{"jsonrpc": "2.0", "id": 71, "method": 12345}"#,
        ] {
            let reply = handle_message(text, &mut authenticated, &auth_config, &cmd_tx)
                .await
                .unwrap();
            assert_eq!(reply["error"]["code"], INVALID_REQUEST, "{text}");
            assert!(reply["id"].is_number());
        }
    }

    #[tokio::test]
    async fn string_id_is_echoed_in_response() {
        let (auth_config, cmd_tx) = mock_server();
        let req = json!({
            "jsonrpc": "2.0", "id": "req-abc",
            "method": "Server.GetRPCVersion"
        });
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert_eq!(response["id"], "req-abc");
    }

    #[tokio::test]
    async fn null_id_error_response_still_echoes_null() {
        let (auth_config, cmd_tx) = mock_server();
        // Missing id -> Value::Null; an error response must still carry it.
        let req = json!({"method": "Server.DeleteClient", "params": {}});
        let response = handle_request(&req, &auth_config, &cmd_tx).await;
        assert!(response["id"].is_null());
        assert_eq!(response["error"]["message"], "missing 'id'");
    }

    // --- handle_message: framing shared by TCP, WebSocket and HTTP ------

    #[tokio::test]
    async fn message_parse_error_has_null_id() {
        let (auth_config, cmd_tx) = mock_server();
        let reply = handle_message("{not json", &mut true, &auth_config, &cmd_tx)
            .await
            .unwrap();
        assert_eq!(reply["error"]["code"], PARSE_ERROR);
        assert!(reply["id"].is_null());
    }

    #[tokio::test]
    async fn message_notification_gets_no_reply() {
        let (auth_config, cmd_tx) = mock_server();
        let text = r#"{"jsonrpc": "2.0", "method": "Server.GetRPCVersion"}"#;
        assert!(
            handle_message(text, &mut true, &auth_config, &cmd_tx)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn message_unknown_method_echoes_id() {
        let (auth_config, cmd_tx) = mock_server();
        let text = r#"{"jsonrpc": "2.0", "id": 9, "method": "Custom.DoThing"}"#;
        let reply = handle_message(text, &mut true, &auth_config, &cmd_tx)
            .await
            .unwrap();
        assert_eq!(reply["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(reply["id"], 9);
    }

    #[tokio::test]
    async fn message_auth_gate_opens_on_successful_authenticate() {
        let (auth_config, cmd_tx) = auth_enabled();
        let mut authenticated = false;
        let status = r#"{"jsonrpc": "2.0", "id": 1, "method": "Server.GetStatus"}"#;
        let reply = handle_message(status, &mut authenticated, &auth_config, &cmd_tx)
            .await
            .unwrap();
        assert_unauthorized(&reply);
        assert_eq!(reply["id"], 1);

        // Allowed before authentication.
        let version = r#"{"jsonrpc": "2.0", "id": 2, "method": "Server.GetRPCVersion"}"#;
        let reply = handle_message(version, &mut authenticated, &auth_config, &cmd_tx)
            .await
            .unwrap();
        assert_eq!(reply["result"]["major"], 2);
        let token = r#"{"jsonrpc": "2.0", "id": 3, "method": "Server.GetToken", "params": {"username": "bob", "password": "pw"}}"#;
        let reply = handle_message(token, &mut authenticated, &auth_config, &cmd_tx)
            .await
            .unwrap();
        assert!(reply["result"]["token"].is_string());
        assert!(!authenticated, "a token alone does not authenticate");

        let bad = r#"{"jsonrpc": "2.0", "id": 4, "method": "Server.Authenticate", "params": {"scheme": "Plain", "param": "bob:nope"}}"#;
        handle_message(bad, &mut authenticated, &auth_config, &cmd_tx).await;
        assert!(!authenticated, "bad credentials must not open the gate");

        let good = r#"{"jsonrpc": "2.0", "id": 5, "method": "Server.Authenticate", "params": {"scheme": "Plain", "param": "bob:pw"}}"#;
        handle_message(good, &mut authenticated, &auth_config, &cmd_tx).await;
        assert!(authenticated);
        let reply = handle_message(status, &mut authenticated, &auth_config, &cmd_tx)
            .await
            .unwrap();
        assert!(reply["result"]["server"].is_object());
    }

    #[tokio::test]
    async fn message_batch_answers_requests_in_order() {
        let (auth_config, cmd_tx) = mock_server();
        let text = r#"[
            {"jsonrpc": "2.0", "id": 1, "method": "Server.GetRPCVersion"},
            {"jsonrpc": "2.0", "method": "Server.GetRPCVersion"},
            {"jsonrpc": "2.0", "id": 2, "method": "Nope"}
        ]"#;
        let reply = handle_message(text, &mut true, &auth_config, &cmd_tx)
            .await
            .unwrap();
        let replies = reply.as_array().unwrap();
        assert_eq!(replies.len(), 2, "the notification gets no entry");
        assert_eq!(replies[0]["id"], 1);
        assert_eq!(replies[1]["error"]["code"], METHOD_NOT_FOUND);

        let reply = handle_message("[]", &mut true, &auth_config, &cmd_tx)
            .await
            .unwrap();
        assert_eq!(reply["error"]["code"], INVALID_REQUEST);
    }
}
