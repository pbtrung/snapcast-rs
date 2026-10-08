//! Inactive streaming clients: sessions that go silent are closed after the
//! idle timeout, and clients disconnected for long enough are deleted from
//! the server state.

use std::time::Duration;

use snapcast_server::{ServerConfig, ServerEvent};
use snapcast_tests::{
    RawClient, client_connected, connect_client_with_id, expect_server_event, start_server_with,
};

fn config(idle: Option<Duration>, remove_after: Option<Duration>) -> ServerConfig {
    ServerConfig {
        client_idle_timeout: idle,
        remove_disconnected_clients_after: remove_after,
        ..Default::default()
    }
}

#[tokio::test]
async fn silent_session_is_closed_after_idle_timeout() {
    let mut server = start_server_with(config(Some(Duration::from_millis(400)), None)).await;
    // Taken before connecting: the server's idle clock starts when it reads the
    // Hello, which is later, so the close can never come sooner than 400 ms
    // after this, however late the test observes the events below.
    let started = tokio::time::Instant::now();
    // Sends its Hello, then nothing — like a peer that lost power.
    let mut silent = RawClient::connect(server.port, "silent").await;
    expect_server_event(&mut server.events, 2000, |e| {
        matches!(e, ServerEvent::ClientConnected { ref id, .. } if id == "silent").then_some(())
    })
    .await;

    expect_server_event(&mut server.events, 3000, |e| {
        matches!(e, ServerEvent::ClientDisconnected { ref id } if id == "silent").then_some(())
    })
    .await;
    assert!(
        started.elapsed() >= Duration::from_millis(400),
        "not closed before the timeout"
    );
    assert!(silent.closed_within(1000).await, "socket closed by server");
    assert_eq!(
        client_connected(&server.cmd, "silent").await,
        Some(false),
        "kept in the state, shown as disconnected"
    );
}

#[tokio::test]
async fn connection_without_hello_is_closed_after_idle_timeout() {
    use tokio::io::AsyncReadExt;
    let server = start_server_with(config(Some(Duration::from_millis(300)), None)).await;
    let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", server.port))
        .await
        .unwrap();
    let mut buf = [0u8; 16];
    let read = tokio::time::timeout(Duration::from_secs(3), sock.read(&mut buf))
        .await
        .expect("server closes a connection that never says Hello");
    assert!(matches!(read, Ok(0) | Err(_)));
}

#[tokio::test]
async fn syncing_client_stays_connected() {
    // Snapclient sends a time sync request every second.
    let mut server = start_server_with(config(Some(Duration::from_millis(1500)), None)).await;
    let _client = connect_client_with_id(server.port, "alive").await;
    expect_server_event(&mut server.events, 2000, |e| {
        matches!(e, ServerEvent::ClientConnected { ref id, .. } if id == "alive").then_some(())
    })
    .await;
    let disconnected = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            if let Some(ServerEvent::ClientDisconnected { .. }) = server.events.recv().await {
                return;
            }
        }
    })
    .await;
    assert!(disconnected.is_err(), "active client must not be dropped");
    assert_eq!(client_connected(&server.cmd, "alive").await, Some(true));
}

#[tokio::test]
async fn long_disconnected_client_is_removed_from_state() {
    let mut server = start_server_with(config(None, Some(Duration::from_millis(300)))).await;

    // A connected client is never removed, however long it stays.
    let keep = RawClient::connect(server.port, "keep").await;
    let gone = RawClient::connect(server.port, "gone").await;
    for _ in 0..2 {
        expect_server_event(&mut server.events, 2000, |e| {
            matches!(e, ServerEvent::ClientConnected { .. }).then_some(())
        })
        .await;
    }
    drop(gone);
    expect_server_event(&mut server.events, 2000, |e| {
        matches!(e, ServerEvent::ClientDisconnected { ref id } if id == "gone").then_some(())
    })
    .await;
    assert_eq!(client_connected(&server.cmd, "gone").await, Some(false));

    expect_server_event(&mut server.events, 3000, |e| match e {
        ServerEvent::StateChanged(state) if !state.clients.contains_key("gone") => Some(()),
        _ => None,
    })
    .await;
    assert_eq!(client_connected(&server.cmd, "gone").await, None, "deleted");
    assert_eq!(client_connected(&server.cmd, "keep").await, Some(true));
    drop(keep);
}
