//! Client reconnection.
//!
//! - A client whose server goes away reports `Disconnected` and, once a
//!   server answers on its address again, reconnects by itself and completes
//!   the whole handshake and time sync again (with the same client id).
//! - A client pointed at an address with no server reports `Disconnected`.
//! - A client-initiated `Stop` right after the handshake ends the session
//!   without a spurious `Disconnected` (`shutdown.rs` covers the same with
//!   audio flowing).
//!
//! The client connects through a [`Relay`], so the server behind its address
//! can be replaced without rebinding the freed port, which a parallel test
//! may have taken in the meantime.

use snapcast_client::{ClientCommand, ClientConfig, ClientEvent};
use snapcast_server::ServerCommand;
use snapcast_tests::{
    FLAC_BLOCK_FRAMES, Relay, collect_audio, connect_client, connect_client_with,
    expect_clean_stop, expect_client_disconnected, expect_client_id_connected, expect_event,
    start_server, tone_frame,
};

#[tokio::test]
async fn client_reconnects_after_the_server_restarts() {
    const ID: &str = "reconnecting";
    let mut server1 = start_server().await;
    let relay = Relay::start(server1.port).await;
    let mut client = connect_client_with(ClientConfig {
        host: "127.0.0.1".into(),
        port: relay.port,
        host_id: ID.into(),
        ..ClientConfig::default()
    })
    .await;

    expect_event(&mut client.events, 2000, |e| {
        matches!(e, ClientEvent::ServerSettings { .. }).then_some(())
    })
    .await;
    expect_client_id_connected(&mut server1.events, ID).await;

    // Stopping the server ends every session; the client notices at once.
    server1.cmd.send(ServerCommand::Stop).await.unwrap();
    let reason = expect_event(&mut client.events, 3000, |e| match e {
        ClientEvent::Disconnected { reason } => Some(reason),
        _ => None,
    })
    .await;
    assert!(!reason.is_empty(), "Disconnected carries a reason");

    // A new server behind the same address: the client's retry loop finds it.
    let mut server2 = start_server().await;
    relay.set_upstream(server2.port);
    expect_event(&mut client.events, 5000, |e| {
        matches!(e, ClientEvent::ServerSettings { .. }).then_some(())
    })
    .await;
    expect_client_id_connected(&mut server2.events, ID).await;
    expect_event(&mut client.events, 2000, |e| {
        matches!(e, ClientEvent::StreamStarted { .. }).then_some(())
    })
    .await;

    // Audio flows again, and the time sync of the new connection completes.
    let mut ts = 1_000_000_000i64;
    for _ in 0..20 {
        server2.audio_tx.send(tone_frame(ts)).await.unwrap();
        ts += 10_000;
    }
    collect_audio(&mut client.audio_rx, 2 * FLAC_BLOCK_FRAMES * 2, 5000).await;
    expect_event(&mut client.events, 10_000, |e| {
        matches!(e, ClientEvent::TimeSyncComplete { .. }).then_some(())
    })
    .await;

    client.cmd.send(ClientCommand::Stop).await.ok();
    expect_client_disconnected(&mut server2.events, ID).await;
}

#[tokio::test]
async fn client_to_dead_address_emits_disconnected() {
    // A socket bound but not listening holds the port, so no parallel test's
    // server can take it, and refuses connections.
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind(([127, 0, 0, 1], 0).into()).unwrap();
    let dead_port = socket.local_addr().unwrap().port();

    let mut client = connect_client(dead_port).await;
    let reason = expect_event(&mut client.events, 5000, |e| match e {
        ClientEvent::Disconnected { reason } => Some(reason),
        ClientEvent::Connected { .. } => panic!("nothing listens on the dead port"),
        _ => None,
    })
    .await;
    assert!(!reason.is_empty(), "Disconnected carries a reason");

    // Stop also ends the retry loop: the event channel closes.
    client.cmd.send(ClientCommand::Stop).await.ok();
    let closed = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while client.events.recv().await.is_some() {}
    })
    .await;
    assert!(closed.is_ok(), "Stop ends a client that is retrying");
}

#[tokio::test]
async fn client_stop_after_handshake_cleanly_ends_session() {
    const ID: &str = "stopping";
    let mut server = start_server().await;
    let mut client = snapcast_tests::connect_client_with_id(server.port, ID).await;

    expect_event(&mut client.events, 2000, |e| {
        matches!(e, ClientEvent::ServerSettings { .. }).then_some(())
    })
    .await;
    expect_client_id_connected(&mut server.events, ID).await;

    client.cmd.send(ClientCommand::Stop).await.ok();
    expect_client_disconnected(&mut server.events, ID).await;
    expect_clean_stop(&mut client.events).await;
}
