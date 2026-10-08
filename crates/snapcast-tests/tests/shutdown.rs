//! Wave-2 integration tests: graceful shutdown / disconnect handling.
//!
//! These tests exercise what happens when audio is flowing and either the
//! server goes away or a client asks to stop.
//!
//! # How a flowing client actually gets disconnected server-side
//!
//! Verified against the implementation (crates/snapcast-server/src/{lib,session}.rs):
//!
//! * `ServerCommand::Stop` aborts only the *accept loop* task; the per-client
//!   `handle_client` tasks are detached `tokio::spawn`s, so they keep running.
//! * Each client session's write loop blocks on `broadcast::Receiver::recv()`
//!   for encoded audio chunks and only bails when that broadcast is **Closed**
//!   (session.rs `RecvError::Closed => bail!`).
//! * The broadcast closes only when the last `broadcast::Sender` clone drops.
//!   Those clones live in (a) each stream-encoder task — which exits when its
//!   audio input `mpsc::Sender`s all drop — and (b) the `serve()` future, which
//!   drops them when it returns after `Stop`.
//!
//! Therefore "drop the server / its audio listener" means: drop every audio
//! input sender (ending the encoder tasks) *and* stop the server (ending
//! `serve()`), which closes the broadcast and disconnects flowing clients. A
//! probe confirmed that dropping audio alone or `Stop` alone leaves the client
//! connected; both together disconnect it within a few hundred ms.
//!
//! All synchronization is event-driven (`expect_event` on the client side and a
//! `expect_server_event` on the server side), never fixed sleeps.

use snapcast_client::ClientEvent;
use snapcast_server::{AudioData, AudioFrame, ServerCommand, ServerEvent};
use snapcast_tests::{TestClient, connect_client, expect_event, expect_server_event, start_server};
use tokio::sync::mpsc;

/// One ~10ms chunk of interleaved stereo f32 audio at 48kHz/2ch (960 samples).
/// Cheap to encode; keeps tests fast.
fn tone_frame(timestamp_usec: i64) -> AudioFrame {
    let samples: Vec<f32> = (0..960).map(|i| (i as f32 / 960.0) * 2.0 - 1.0).collect();
    AudioFrame {
        data: AudioData::F32(samples),
        timestamp_usec,
    }
}

/// Wait until the client has actually decoded audio, proving the stream is
/// flowing before we tear anything down. Pushes frames while polling so the
/// pipeline is primed regardless of buffering.
async fn wait_for_audio_flowing(client: &mut TestClient, audio_tx: &mpsc::Sender<AudioFrame>) {
    // The stream must have started first.
    expect_event(&mut client.events, 2000, |e| {
        matches!(e, ClientEvent::StreamStarted { .. }).then_some(())
    })
    .await;

    let mut ts = 1_000_000_000i64;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        // Keep feeding audio so the client always has something to decode.
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
                return; // audio is flowing
            }
            Ok(None) => panic!("Audio channel closed before any audio flowed"),
            Err(_) => assert!(
                tokio::time::Instant::now() < deadline,
                "Timed out waiting for audio to flow to the client"
            ),
        }
    }
}

/// After audio is flowing, tearing down the server's audio path (dropping every
/// audio input sender, ending the encoder tasks) and stopping the server must
/// drive the connected client to `Disconnected` within a bounded timeout.
#[tokio::test]
async fn server_stop_disconnects_flowing_client() {
    let mut server = start_server().await;
    // Take sole ownership of the audio sender so we can drop *every* clone.
    let audio_tx = server.audio_tx;
    let mut client = connect_client(server.port).await;

    // Confirm the client is truly connected and audio is flowing.
    wait_for_audio_flowing(&mut client, &audio_tx).await;

    // Confirm the server considers the client connected too.
    let _client_id = expect_server_event(&mut server.events, 2000, |e| match e {
        ServerEvent::ClientConnected { id, .. } => Some(id),
        _ => None,
    })
    .await;

    // Drop the audio listener (ends the encoder task, releasing its broadcast
    // sender) and stop the server (ends serve(), releasing its senders). This
    // closes the audio broadcast, which each live client session observes.
    drop(audio_tx);
    server.cmd.send(ServerCommand::Stop).await.unwrap();

    // The client must transition to Disconnected within the timeout. The
    // controller retries afterward, but the FIRST Disconnected is what we
    // assert — the client noticed the peer went away.
    let reason = expect_event(&mut client.events, 3000, |e| match e {
        ClientEvent::Disconnected { reason } => Some(reason),
        _ => None,
    })
    .await;

    assert!(
        !reason.is_empty(),
        "Disconnected event should carry a non-empty reason"
    );
}

/// With more than one client attached and audio flowing, tearing the server
/// down must disconnect ALL of them, not just the first.
#[tokio::test]
async fn server_stop_disconnects_all_clients() {
    let server = start_server().await;
    let audio_tx = server.audio_tx;
    let mut client_a = connect_client(server.port).await;
    let mut client_b = connect_client(server.port).await;

    // Prime both clients so audio is genuinely flowing to each.
    wait_for_audio_flowing(&mut client_a, &audio_tx).await;
    wait_for_audio_flowing(&mut client_b, &audio_tx).await;

    drop(audio_tx);
    server.cmd.send(ServerCommand::Stop).await.unwrap();

    // Both clients must observe Disconnected within the timeout.
    expect_event(&mut client_a.events, 3000, |e| {
        matches!(e, ClientEvent::Disconnected { .. }).then_some(())
    })
    .await;
    expect_event(&mut client_b.events, 3000, |e| {
        matches!(e, ClientEvent::Disconnected { .. }).then_some(())
    })
    .await;
}

/// A client-initiated Stop must cleanly end the session: the server observes a
/// ClientDisconnected, and the client task terminates without hanging or
/// panicking.
#[tokio::test]
async fn client_stop_cleanly_ends_session() {
    let mut server = start_server().await;
    let audio_tx = server.audio_tx.clone();
    let mut client = connect_client(server.port).await;

    // Audio flowing => a fully live session, not a half-open handshake.
    wait_for_audio_flowing(&mut client, &audio_tx).await;

    let client_id = expect_server_event(&mut server.events, 2000, |e| match e {
        ServerEvent::ClientConnected { id, .. } => Some(id),
        _ => None,
    })
    .await;
    assert!(!client_id.is_empty());

    // Client asks to stop. This returns Ok(()) from the client's run loop, so
    // NO Disconnected event is emitted client-side — the session simply ends.
    client
        .cmd
        .send(snapcast_client::ClientCommand::Stop)
        .await
        .unwrap();

    // The server must see this client leave, promptly and cleanly.
    expect_server_event(&mut server.events, 3000, |e| match e {
        ServerEvent::ClientDisconnected { id } if id == client_id => Some(()),
        _ => None,
    })
    .await;

    // No hang, no panic: the client task returned Ok(()) and dropped its event
    // sender, so the event channel drains to closed. A clean Stop must NEVER
    // surface a Disconnected (that would mean the reconnect loop ran instead of
    // a graceful shutdown). Bounding the drain also proves there is no hang.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(3000);
    loop {
        match tokio::time::timeout_at(deadline, client.events.recv()).await {
            Ok(Some(ClientEvent::Disconnected { reason })) => {
                panic!("Clean client Stop must not emit Disconnected (got: {reason})");
            }
            Ok(Some(_)) => continue, // drain benign trailing events
            Ok(None) => break,       // channel closed => client task finished cleanly
            Err(_) => panic!("Client task did not shut down cleanly after Stop (hang)"),
        }
    }
}
