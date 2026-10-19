//! Graceful shutdown and disconnect handling while audio is flowing.
//!
//! `ServerCommand::Stop` ends `serve()`, which aborts the session task that
//! owns every client session (a `JoinSet`), so connected clients are
//! disconnected even while the stream's audio input is still open.

use snapcast_client::ClientEvent;
use snapcast_server::{AudioFrame, ServerCommand};
use snapcast_tests::{
    TestClient, connect_client_with_id, expect_clean_stop, expect_client_disconnected,
    expect_client_id_connected, expect_event, start_server, tone_frame,
};
use tokio::sync::mpsc;

/// Wait until the client has decoded audio, proving the stream is flowing
/// before anything is torn down. Pushes frames while polling.
async fn wait_for_audio_flowing(client: &mut TestClient, audio_tx: &mpsc::Sender<AudioFrame>) {
    expect_event(&mut client.events, 2000, |e| {
        matches!(e, ClientEvent::StreamStarted { .. }).then_some(())
    })
    .await;

    let mut ts = 1_000_000_000i64;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        for _ in 0..8 {
            audio_tx.send(tone_frame(ts)).await.unwrap();
            ts += 10_000;
        }
        match tokio::time::timeout(
            std::time::Duration::from_millis(200),
            client.audio_rx.recv(),
        )
        .await
        {
            Ok(Some(frame)) => {
                assert!(!frame.samples.is_empty(), "decoded frame had no samples");
                return;
            }
            Ok(None) => panic!("Audio channel closed before any audio flowed"),
            Err(_) => assert!(
                tokio::time::Instant::now() < deadline,
                "Timed out waiting for audio to flow to the client"
            ),
        }
    }
}

/// The client's next `Disconnected` (it retries afterwards).
async fn expect_disconnected(client: &mut TestClient) -> String {
    expect_event(&mut client.events, 3000, |e| match e {
        ClientEvent::Disconnected { reason } => Some(reason),
        _ => None,
    })
    .await
}

/// Stopping the server disconnects a client that is receiving audio, while
/// the audio input stays open (the sender is kept alive to the end).
#[tokio::test]
async fn server_stop_disconnects_flowing_client() {
    let mut server = start_server().await;
    let mut client = connect_client_with_id(server.port, "shutdown-1").await;
    wait_for_audio_flowing(&mut client, &server.audio_tx).await;
    expect_client_id_connected(&mut server.events, "shutdown-1").await;

    server.cmd.send(ServerCommand::Stop).await.unwrap();
    let reason = expect_disconnected(&mut client).await;
    assert!(!reason.is_empty(), "Disconnected carries a reason");
    drop(server);
}

/// With several clients attached, stopping the server disconnects all of
/// them, not just the first.
#[tokio::test]
async fn server_stop_disconnects_all_clients() {
    let server = start_server().await;
    // Distinct ids: with the same one, each connect replaces the other's session.
    let mut client_a = connect_client_with_id(server.port, "shutdown-a").await;
    let mut client_b = connect_client_with_id(server.port, "shutdown-b").await;
    wait_for_audio_flowing(&mut client_a, &server.audio_tx).await;
    wait_for_audio_flowing(&mut client_b, &server.audio_tx).await;

    server.cmd.send(ServerCommand::Stop).await.unwrap();
    expect_disconnected(&mut client_a).await;
    expect_disconnected(&mut client_b).await;
}

/// A client-initiated Stop ends the session cleanly: the server sees the
/// client leave, and the client's run loop returns without a `Disconnected`.
#[tokio::test]
async fn client_stop_cleanly_ends_session() {
    let mut server = start_server().await;
    let mut client = connect_client_with_id(server.port, "shutdown-2").await;
    wait_for_audio_flowing(&mut client, &server.audio_tx).await;
    expect_client_id_connected(&mut server.events, "shutdown-2").await;

    client
        .cmd
        .send(snapcast_client::ClientCommand::Stop)
        .await
        .unwrap();
    expect_client_disconnected(&mut server.events, "shutdown-2").await;
    expect_clean_stop(&mut client.events).await;
}
