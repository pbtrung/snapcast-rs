//! Control-API behaviour that Snapweb (and other C++-snapserver control
//! clients) rely on.

use snapcast_server::{ServerCommand, ServerEvent};
use snapcast_tests::{connect_client_with_id, start_server};
use tokio::sync::mpsc;

/// The next connect/topology event, skipping unrelated ones.
async fn next_connect_event(events: &mut mpsc::Receiver<ServerEvent>) -> ServerEvent {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(
                e @ (ServerEvent::ServerUpdated
                | ServerEvent::ClientConnected { .. }
                | ServerEvent::ClientDisconnected { .. }),
            )) => return e,
            Ok(Some(_)) => continue,
            other => panic!("no connect event: {other:?}"),
        }
    }
}

/// A client joining no existing group changes the topology, which C++
/// snapserver announces with Server.OnUpdate before Client.OnConnect. A
/// reconnecting client already has a group, so only ClientConnected follows.
#[tokio::test]
async fn new_client_announces_server_update_before_connect() {
    let mut server = start_server().await;

    let client = connect_client_with_id(server.port, "compat-1").await;
    assert!(matches!(
        next_connect_event(&mut server.events).await,
        ServerEvent::ServerUpdated
    ));
    assert!(matches!(
        next_connect_event(&mut server.events).await,
        ServerEvent::ClientConnected { id, .. } if id == "compat-1"
    ));

    client
        .cmd
        .send(snapcast_client::ClientCommand::Stop)
        .await
        .ok();
    assert!(matches!(
        next_connect_event(&mut server.events).await,
        ServerEvent::ClientDisconnected { .. }
    ));

    let _client = connect_client_with_id(server.port, "compat-1").await;
    assert!(matches!(
        next_connect_event(&mut server.events).await,
        ServerEvent::ClientConnected { id, .. } if id == "compat-1"
    ));
}

/// The status carries what the client reported in Hello, its IP and when it
/// was last seen, like C++ snapserver.
#[tokio::test]
async fn status_reports_hello_details() {
    let mut server = start_server().await;
    let _client = connect_client_with_id(server.port, "compat-2").await;
    while !matches!(
        next_connect_event(&mut server.events).await,
        ServerEvent::ClientConnected { .. }
    ) {}

    let (tx, rx) = tokio::sync::oneshot::channel();
    server
        .cmd
        .send(ServerCommand::GetStatus { response_tx: tx })
        .await
        .unwrap();
    let status = rx.await.unwrap();
    assert_eq!(status.server.server.snapserver.name, "Snapserver");
    let client = status
        .server
        .groups
        .iter()
        .flat_map(|g| &g.clients)
        .find(|c| c.id == "compat-2")
        .expect("client in status");
    assert!(client.connected);
    assert_eq!(client.host.ip, "127.0.0.1");
    assert_eq!(client.host.os, std::env::consts::OS);
    assert_eq!(client.host.arch, std::env::consts::ARCH);
    assert_eq!(client.snapclient.name, "Snapclient");
    assert_eq!(
        client.snapclient.protocol_version,
        snapcast_proto::PROTOCOL_VERSION
    );
    assert!(!client.snapclient.version.is_empty());
    assert_eq!(client.config.instance, 1);
    assert!(client.last_seen.sec > 0);
}
