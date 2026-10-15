use snapcast_client::{ClientConfig, ClientEvent};
use snapcast_server::ServerConfig;
use snapcast_server::auth::{Role, StaticAuthValidator, User};
use snapcast_tests::{TestClient, TestServer, connect_client_with, start_server_with};
use std::sync::Arc;

fn auth_server_config() -> ServerConfig {
    ServerConfig {
        auth: Some(Arc::new(StaticAuthValidator::new(
            vec![User {
                name: "player".into(),
                password: "secret".into(),
                role: "streaming".into(),
            }],
            vec![Role {
                name: "streaming".into(),
                permissions: vec!["Streaming".into()],
            }],
        ))),
        ..ServerConfig::default()
    }
}

/// Start an auth-enabled server and connect a client logging in with
/// `credentials` (`user:password`). The server handle keeps its stream open.
async fn connect_with_credentials(credentials: &str) -> (TestServer, TestClient) {
    use base64::Engine;
    let server = start_server_with(auth_server_config()).await;
    let client = connect_client_with(ClientConfig {
        host: "127.0.0.1".into(),
        port: server.port,
        auth: Some(snapcast_client::config::Auth {
            scheme: "Basic".into(),
            param: base64::engine::general_purpose::STANDARD.encode(credentials),
        }),
        ..ClientConfig::default()
    })
    .await;
    (server, client)
}

#[tokio::test]
async fn auth_success() {
    let (_server, mut client) = connect_with_credentials("player:secret").await;

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(2000);
    loop {
        match tokio::time::timeout_at(deadline, client.events.recv()).await {
            Ok(Some(ClientEvent::ServerSettings { .. })) => break, // success
            Ok(Some(_)) => continue,
            _ => panic!("Auth should have succeeded — timed out"),
        }
    }
}

#[tokio::test]
async fn auth_rejection() {
    let (_server, mut client) = connect_with_credentials("player:wrong").await;

    // Client should get Disconnected (server sends Error and drops connection)
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(2000);
    loop {
        match tokio::time::timeout_at(deadline, client.events.recv()).await {
            Ok(Some(ClientEvent::Disconnected { reason })) => {
                assert!(
                    reason.contains("401")
                        || reason.contains("Unauthorized")
                        || reason.contains("rejected"),
                    "Expected auth rejection, got: {reason}"
                );
                break;
            }
            Ok(Some(ClientEvent::ServerSettings { .. })) => {
                panic!("Should not have received ServerSettings with wrong password");
            }
            Ok(Some(_)) => continue,
            _ => panic!("Timed out waiting for auth rejection"),
        }
    }
}
