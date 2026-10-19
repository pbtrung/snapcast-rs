//! Streaming-client authentication: the Hello's credentials are checked by
//! the server's `AuthValidator`, and a rejected client gets an Error message
//! with the C++ code and short name before the connection is dropped.

use snapcast_client::{ClientConfig, ClientEvent};
use snapcast_server::ServerConfig;
use snapcast_server::auth::{Role, StaticAuthValidator, User};
use snapcast_tests::{
    TestClient, TestServer, connect_client_with, expect_event, start_server_with,
};
use std::sync::Arc;

fn auth_server_config() -> ServerConfig {
    let user = |name: &str, role: &str| User {
        name: name.into(),
        password: "secret".into(),
        role: role.into(),
    };
    ServerConfig {
        auth: Some(Arc::new(StaticAuthValidator::new(
            vec![user("player", "streaming"), user("viewer", "control")],
            vec![
                Role {
                    name: "streaming".into(),
                    permissions: vec!["Streaming".into()],
                },
                Role {
                    name: "control".into(),
                    permissions: vec!["Control".into()],
                },
            ],
        ))),
        ..ServerConfig::default()
    }
}

/// Start an auth-enabled server and connect a client logging in with Basic
/// `credentials` (`user:password`), or with no credentials at all.
async fn connect_with_credentials(credentials: Option<&str>) -> (TestServer, TestClient) {
    use base64::Engine;
    let server = start_server_with(auth_server_config()).await;
    let client = connect_client_with(ClientConfig {
        host: "127.0.0.1".into(),
        port: server.port,
        auth: credentials.map(|c| snapcast_client::config::Auth {
            scheme: "Basic".into(),
            param: base64::engine::general_purpose::STANDARD.encode(c),
        }),
        ..ClientConfig::default()
    })
    .await;
    (server, client)
}

/// The client's first `Disconnected` reason; fails if the handshake succeeds.
async fn rejection_reason(client: &mut TestClient) -> String {
    expect_event(&mut client.events, 2000, |e| match e {
        ClientEvent::Disconnected { reason } => Some(reason),
        ClientEvent::ServerSettings { .. } => panic!("handshake should have been rejected"),
        _ => None,
    })
    .await
}

#[tokio::test]
async fn auth_success() {
    let (_server, mut client) = connect_with_credentials(Some("player:secret")).await;
    expect_event(&mut client.events, 2000, |e| match e {
        ClientEvent::ServerSettings { .. } => Some(()),
        ClientEvent::Disconnected { reason } => panic!("auth should succeed: {reason}"),
        _ => None,
    })
    .await;
}

#[tokio::test]
async fn wrong_password_is_unauthorized() {
    let (_server, mut client) = connect_with_credentials(Some("player:wrong")).await;
    let reason = rejection_reason(&mut client).await;
    assert!(
        reason.contains("Unauthorized") && reason.contains("code 401"),
        "{reason}"
    );
}

#[tokio::test]
async fn missing_credentials_are_unauthorized() {
    let (_server, mut client) = connect_with_credentials(None).await;
    let reason = rejection_reason(&mut client).await;
    assert!(
        reason.contains("Unauthorized") && reason.contains("code 401"),
        "{reason}"
    );
}

#[tokio::test]
async fn user_without_streaming_permission_is_forbidden() {
    let (_server, mut client) = connect_with_credentials(Some("viewer:secret")).await;
    let reason = rejection_reason(&mut client).await;
    assert!(
        reason.contains("Forbidden") && reason.contains("code 403"),
        "{reason}"
    );
}
