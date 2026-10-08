//! Wave-2 integration tests: client reconnection & resync.
//!
//! Coverage:
//!   * `client_stop_after_handshake_cleanly_ends_session` — a client-initiated
//!     `Stop` right after the handshake (before any audio) cleanly ends the
//!     session: the server observes `ClientDisconnected`, and the client does
//!     *not* surface a spurious `Disconnected` failure event. (`shutdown.rs`
//!     covers the same with audio flowing.)
//!   * `client_to_dead_address_emits_disconnected` — a client pointed at an
//!     address with no server emits `Disconnected`.
//!   * `fresh_server_completes_full_handshake_and_sync` — after an old
//!     server/client pair is torn down, a *fresh* server brought up on a new
//!     port and a *new* client complete the full handshake/sync sequence
//!     (Connected -> ServerSettings -> StreamStarted -> TimeSyncComplete),
//!     proving recovery of the whole pipeline.
//!
//! On why we don't drop a live server and watch an *existing* client
//! reconnect: the server spawns each accepted client onto a **detached**
//! `tokio::spawn` task (see `session.rs::run`), and those per-client tasks are
//! not reachable from any server handle. Dropping every `TestServer` handle (or
//! sending `ServerCommand::Stop`) stops the accept loop and frees the port, but
//! within a single test runtime the orphaned per-client task keeps the socket
//! open, so an already-connected client does not promptly observe the drop.
//! The harness therefore cannot restart a server *under a live client* in
//! process, and we take the documented fallback: assert clean client Stop,
//! dead-address Disconnected, and a full fresh-server + fresh-client resync.
//!
//! All synchronization is event-driven (channel `recv` under a deadline); there
//! are no fixed "settle" sleeps.

use snapcast_client::ClientEvent;
use snapcast_server::ServerEvent;
use snapcast_tests::{connect_client, expect_event, expect_server_event, start_server};
use tokio::sync::mpsc;

/// Push `count` short silence frames with monotonically increasing timestamps,
/// so the client's audio/sync pipeline has chunks to time-align against. This
/// keeps `TimeSyncComplete` prompt and deterministic. Best-effort: stops early
/// if the receiver is gone.
async fn pump_silence(audio_tx: &mpsc::Sender<snapcast_server::AudioFrame>, count: usize) {
    let mut ts: i64 = 2_000_000_000; // well away from 0
    for _ in 0..count {
        let frame = snapcast_server::AudioFrame {
            data: snapcast_server::AudioData::F32(vec![0.0; 960]),
            timestamp_usec: ts,
        };
        if audio_tx.send(frame).await.is_err() {
            break;
        }
        ts += 10_000; // 10 ms per frame
    }
}

/// Drive a freshly connected client through the full connect/sync sequence:
/// Connected -> ServerSettings -> StreamStarted -> TimeSyncComplete.
async fn expect_full_sync(
    events: &mut mpsc::Receiver<ClientEvent>,
    audio_tx: &mpsc::Sender<snapcast_server::AudioFrame>,
) {
    // Prime the pipeline with audio before waiting on the handshake events.
    pump_silence(audio_tx, 40).await;

    expect_event(events, 2000, |e| {
        matches!(e, ClientEvent::Connected { .. }).then_some(())
    })
    .await;

    let (buffer_ms, volume) = expect_event(events, 2000, |e| match e {
        ClientEvent::ServerSettings {
            buffer_ms, volume, ..
        } => Some((buffer_ms, volume)),
        _ => None,
    })
    .await;
    assert!(buffer_ms > 0, "expected a positive buffer_ms");
    assert!(volume > 0, "expected a positive volume");

    expect_event(events, 2000, |e| match e {
        ClientEvent::StreamStarted { codec, format } => {
            assert!(!codec.is_empty(), "codec name must be non-empty");
            assert!(format.rate() > 0, "sample rate must be positive");
            Some(())
        }
        _ => None,
    })
    .await;

    // Time sync can take a few real-time seconds; keep audio trickling in.
    pump_silence(audio_tx, 40).await;
    expect_event(events, 10_000, |e| match e {
        ClientEvent::TimeSyncComplete { .. } => Some(()),
        _ => None,
    })
    .await;
}

/// Client-initiated `Stop` right after the handshake must cleanly end the
/// session: the server observes a `ClientDisconnected`, and the client's own
/// run loop returns `Ok` *without* surfacing a spurious `Disconnected` failure
/// event.
#[tokio::test]
async fn client_stop_after_handshake_cleanly_ends_session() {
    let mut server = start_server().await;
    let mut client = connect_client(server.port).await;

    // Get to a known-good state and learn our server-side id.
    expect_event(&mut client.events, 2000, |e| match e {
        ClientEvent::ServerSettings { .. } => Some(()),
        _ => None,
    })
    .await;

    let client_id = expect_server_event(&mut server.events, 2000, |e| match e {
        ServerEvent::ClientConnected { id, .. } => Some(id),
        _ => None,
    })
    .await;
    assert!(!client_id.is_empty());

    // Client asks to stop.
    client
        .cmd
        .send(snapcast_client::ClientCommand::Stop)
        .await
        .ok();

    // Server must observe the disconnect for exactly this client.
    expect_server_event(&mut server.events, 3000, |e| match e {
        ServerEvent::ClientDisconnected { id } if id == client_id => Some(()),
        _ => None,
    })
    .await;

    // The client's event stream should now close (run() returned Ok, so the
    // event sender was dropped) WITHOUT ever emitting a Disconnected failure.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(2000);
    loop {
        match tokio::time::timeout_at(deadline, client.events.recv()).await {
            // Channel closed: the run loop exited cleanly. Expected terminal.
            Ok(None) => break,
            // A late benign event is fine to ignore, but a Disconnected here
            // would mean the clean stop was mis-reported as a failure.
            Ok(Some(ClientEvent::Disconnected { reason })) => {
                panic!("clean client Stop emitted a Disconnected failure: {reason}");
            }
            Ok(Some(_)) => continue,
            Err(_) => panic!("client event stream neither closed nor produced a terminal event"),
        }
    }
}

/// A client pointed at an address with no server must emit `Disconnected`.
#[tokio::test]
async fn client_to_dead_address_emits_disconnected() {
    // Bind and immediately drop a listener to obtain a port that is (almost
    // certainly) free, so the connect attempt is refused rather than hanging.
    let dead_port = {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
        // listener dropped here
    };

    let mut client = connect_client(dead_port).await;

    let reason = expect_event(&mut client.events, 5000, |e| match e {
        ClientEvent::Disconnected { reason } => Some(reason),
        _ => None,
    })
    .await;
    assert!(
        !reason.is_empty(),
        "Disconnected to a dead address should carry a reason"
    );

    client
        .cmd
        .send(snapcast_client::ClientCommand::Stop)
        .await
        .ok();
}

/// Recovery of the full pipeline: an old server/client pair is brought up and
/// then fully torn down (client Stop, then old server handles dropped). A brand
/// new server is started and a brand new client completes the entire
/// handshake/sync sequence again — Connected -> ServerSettings -> StreamStarted
/// -> TimeSyncComplete — proving a fresh server can serve after a prior one.
#[tokio::test]
async fn fresh_server_completes_full_handshake_and_sync() {
    // --- First generation: bring a server + client fully up, then tear down.
    let mut server1 = start_server().await;
    let mut client1 = connect_client(server1.port).await;

    expect_full_sync(&mut client1.events, &server1.audio_tx).await;

    let id1 = expect_server_event(&mut server1.events, 2000, |e| match e {
        ServerEvent::ClientConnected { id, .. } => Some(id),
        _ => None,
    })
    .await;
    assert!(!id1.is_empty());

    // Stop the first client cleanly, and confirm the first server sees it go.
    client1
        .cmd
        .send(snapcast_client::ClientCommand::Stop)
        .await
        .ok();
    expect_server_event(&mut server1.events, 3000, |e| match e {
        ServerEvent::ClientDisconnected { id } if id == id1 => Some(()),
        _ => None,
    })
    .await;

    // Drop every handle to the first server: stops its accept loop and frees
    // the port. (Its detached per-client task, if any, has already exited via
    // the clean client Stop above.)
    drop(server1);
    drop(client1);

    // --- Second generation: a FRESH server + FRESH client must resync fully.
    let mut server2 = start_server().await;
    let mut client2 = connect_client(server2.port).await;

    expect_full_sync(&mut client2.events, &server2.audio_tx).await;

    // The fresh server must observe the new client's completed handshake.
    let id2 = expect_server_event(&mut server2.events, 2000, |e| match e {
        ServerEvent::ClientConnected { id, .. } => Some(id),
        _ => None,
    })
    .await;
    assert!(
        !id2.is_empty(),
        "fresh server should see the reconnected client's handshake"
    );

    // Clean shutdown of the second generation.
    client2
        .cmd
        .send(snapcast_client::ClientCommand::Stop)
        .await
        .ok();
}
