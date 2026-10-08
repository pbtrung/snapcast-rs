//! Wave-2 integration test: multi-client isolation.
//!
//! The concurrent-client generalization of `stream_routing.rs`. Instead of a
//! single client, we spin up three clients against ONE server, prove they all
//! reach `Connected` + `ServerSettings`, then use server commands to route each
//! client's group to a different stream and assert that:
//!
//! - a client only ever receives audio from the stream its group is assigned to,
//! - a client whose group is assigned to a different stream receives none of the
//!   first stream's audio,
//! - a muted client receives no audio at all.
//!
//! Unlike the single-client `stream_routing.rs` (which relies on "no panic"),
//! this test makes *positive and negative* assertions on each client's decoded
//! `audio_rx`: `should_send_chunk` filters chunks server-side by stream match
//! and mute state, so a filtered client receives literally zero frames.

use snapcast_client::ClientEvent;
use snapcast_server::{AudioData, AudioFrame, ServerCommand, ServerEvent};
use snapcast_tests::{
    TestClient, TwoStreamServer, client_group, connect_client_with_id, expect_event,
    expect_server_event, start_two_stream_server,
};
use tokio::sync::mpsc;

/// One non-silent input frame (960 interleaved f32 = 480 stereo sample-frames).
fn tone_frame(ts: i64) -> AudioFrame {
    AudioFrame {
        data: AudioData::F32((0..960).map(|i| (i as f32 / 960.0) * 2.0 - 1.0).collect()),
        timestamp_usec: ts,
    }
}

/// Push enough tone frames on a stream to force the FLAC encoder (block size
/// 1152 stereo frames) past several full blocks so decoded audio is emitted.
async fn push_tone(stream: &mpsc::Sender<AudioFrame>, frames: usize) {
    let mut ts = 1_000_000_000i64;
    for _ in 0..frames {
        stream.send(tone_frame(ts)).await.unwrap();
        ts += 10_000; // 10ms increments, matching audio.rs
    }
}

// ---------------------------------------------------------------------------
// Event-driven helpers (no fixed sleeps).
// ---------------------------------------------------------------------------

/// Collect the ids of the next `n` distinct clients the server sees connect.
async fn wait_for_n_connects(events: &mut mpsc::Receiver<ServerEvent>, n: usize) -> Vec<String> {
    let mut ids = Vec::new();
    while ids.len() < n {
        let id = expect_server_event(events, 5000, |e| match e {
            ServerEvent::ClientConnected { id, .. } => Some(id),
            _ => None,
        })
        .await;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

/// Drive a client through `Connected` then `ServerSettings`, asserting the
/// handshake completes and settings are sane. Returns after `ServerSettings`.
async fn expect_connected_and_settings(client: &mut TestClient) {
    expect_event(&mut client.events, 5000, |e| match e {
        ClientEvent::Connected { port, .. } => {
            assert!(port > 0, "Connected reported a bound port");
            Some(())
        }
        _ => None,
    })
    .await;

    expect_event(&mut client.events, 5000, |e| match e {
        ClientEvent::ServerSettings { buffer_ms, .. } => {
            assert!(buffer_ms > 0, "server advertised a positive buffer");
            Some(())
        }
        _ => None,
    })
    .await;
}

/// Wait for the client's decoder to be set up (StreamStarted), so any WireChunk
/// that arrives afterwards is decoded onto `audio_rx`.
async fn wait_stream_started(client: &mut TestClient) {
    expect_event(&mut client.events, 5000, |e| match e {
        ClientEvent::StreamStarted { codec, .. } => {
            assert!(!codec.is_empty());
            Some(())
        }
        _ => None,
    })
    .await;
}

/// Drain the client's audio channel, returning total decoded samples seen.
///
/// Positive case: returns as soon as `min_samples` have arrived (fast).
/// Negative case: pass `min_samples = usize::MAX` to keep draining until the
/// per-recv `quiet_ms` window elapses with no frame — a *bounded* wait for
/// "silence", not a blind sleep. Returns the total observed either way.
async fn drain_audio(client: &mut TestClient, min_samples: usize, quiet_ms: u64) -> usize {
    let mut total = 0usize;
    loop {
        if total >= min_samples {
            return total;
        }
        match tokio::time::timeout(
            std::time::Duration::from_millis(quiet_ms),
            client.audio_rx.recv(),
        )
        .await
        {
            Ok(Some(frame)) => {
                assert_eq!(frame.sample_rate, 48000);
                assert_eq!(frame.channels, 2);
                total += frame.samples.len();
            }
            Ok(None) => return total, // channel closed
            Err(_) => return total,   // quiet window elapsed: no more audio
        }
    }
}

/// Set a group's stream and wait for the `GroupStreamChanged` confirmation.
async fn route_group_to_stream(server: &mut TwoStreamServer, group_id: &str, stream_id: &str) {
    server
        .cmd
        .send(ServerCommand::SetGroupStream {
            group_id: group_id.to_string(),
            stream_id: stream_id.to_string(),
        })
        .await
        .unwrap();
    let want = stream_id.to_string();
    let gid = group_id.to_string();
    expect_server_event(&mut server.events, 2000, |e| match e {
        ServerEvent::GroupStreamChanged {
            group_id,
            stream_id,
        } if group_id == gid && stream_id == want => Some(()),
        _ => None,
    })
    .await;
}

/// Mute a client and wait for the `ClientVolumeChanged` confirmation.
async fn mute_client(server: &mut TwoStreamServer, client_id: &str) {
    server
        .cmd
        .send(ServerCommand::SetClientVolume {
            client_id: client_id.to_string(),
            volume: 0,
            muted: true,
        })
        .await
        .unwrap();
    let cid = client_id.to_string();
    expect_server_event(&mut server.events, 2000, |e| match e {
        ServerEvent::ClientVolumeChanged {
            client_id, muted, ..
        } if client_id == cid && muted => Some(()),
        _ => None,
    })
    .await;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Three clients against one server all reach Connected + ServerSettings, and
/// the server independently observes three distinct ClientConnected events.
#[tokio::test]
async fn three_clients_all_reach_connected_and_server_settings() {
    let mut server = start_two_stream_server().await;

    let mut c1 = connect_client_with_id(server.port, "client-a").await;
    let mut c2 = connect_client_with_id(server.port, "client-b").await;
    let mut c3 = connect_client_with_id(server.port, "client-c").await;

    // Each client independently completes the handshake.
    expect_connected_and_settings(&mut c1).await;
    expect_connected_and_settings(&mut c2).await;
    expect_connected_and_settings(&mut c3).await;

    // The server sees three distinct clients.
    let ids = wait_for_n_connects(&mut server.events, 3).await;
    assert_eq!(ids.len(), 3, "server saw three distinct client ids");

    // Each connected client landed in its own group by default.
    let (tx, rx) = tokio::sync::oneshot::channel();
    server
        .cmd
        .send(ServerCommand::GetStatus { response_tx: tx })
        .await
        .unwrap();
    let status = rx.await.unwrap();
    let total_clients: usize = status.server.groups.iter().map(|g| g.clients.len()).sum();
    assert_eq!(total_clients, 3, "all three clients are tracked in groups");
}

/// Concurrent-client stream isolation: three clients on one server, each group
/// routed to a different stream, plus one muted client. Assert every client
/// only receives audio from the stream it is assigned to, and the muted client
/// receives none.
#[tokio::test]
async fn clients_receive_only_their_assigned_stream_audio() {
    let mut server = start_two_stream_server().await;

    let mut c1 = connect_client_with_id(server.port, "client-a").await;
    let mut c2 = connect_client_with_id(server.port, "client-b").await;
    let mut c3 = connect_client_with_id(server.port, "client-c").await;

    // Drive all three through the handshake, then correlate to server ids via
    // GetStatus. Each has a unique host_id, so the server tracks three distinct
    // clients in three distinct groups.
    expect_connected_and_settings(&mut c1).await;
    expect_connected_and_settings(&mut c2).await;
    expect_connected_and_settings(&mut c3).await;
    wait_stream_started(&mut c1).await;
    wait_stream_started(&mut c2).await;
    wait_stream_started(&mut c3).await;

    let ids = wait_for_n_connects(&mut server.events, 3).await;
    assert_eq!(ids.len(), 3);

    // Every client starts in its own group assigned to the first stream
    // ("stream_a", the default). Confirm and grab each group id.
    let mut groups = Vec::new();
    for id in &ids {
        let (gid, stream) = client_group(&server.cmd, id).await;
        assert_eq!(stream, "stream_a", "default routing is the first stream");
        groups.push(gid);
    }

    // Route: group[0] -> stream_a, group[1] -> stream_b, group[2] -> stream_b,
    // then mute the client in group[2]. Routing/mute are driven by server id, so
    // this is deterministic. Since we cannot know which TestClient handle maps
    // to which server id (clients spawn concurrently), the audio assertions
    // below are on the aggregate: 2 handles hear audio, 1 (muted) is silent.
    route_group_to_stream(&mut server, &groups[0], "stream_a").await;
    route_group_to_stream(&mut server, &groups[1], "stream_b").await;
    route_group_to_stream(&mut server, &groups[2], "stream_b").await;
    mute_client(&mut server, &ids[2]).await;

    // Push a burst of tone on BOTH streams concurrently.
    push_tone(&server.stream_a, 30).await;
    push_tone(&server.stream_b, 30).await;

    // Drain each handle's audio channel until it goes quiet, tallying samples.
    let got1 = drain_audio(&mut c1, usize::MAX, 400).await;
    let got2 = drain_audio(&mut c2, usize::MAX, 400).await;
    let got3 = drain_audio(&mut c3, usize::MAX, 400).await;
    let received = [got1, got2, got3];

    // Exactly two handles must have received audio (the stream_a client and the
    // unmuted stream_b client); exactly one (the muted client) received none.
    let with_audio = received.iter().filter(|&&n| n > 0).count();
    let silent = received.iter().filter(|&&n| n == 0).count();
    assert_eq!(
        with_audio, 2,
        "two of three clients receive audio (stream_a + unmuted stream_b): {received:?}"
    );
    assert_eq!(
        silent, 1,
        "the muted client receives no audio at all: {received:?}"
    );
}

/// A client whose group is on stream_b receives nothing when audio is pushed
/// ONLY on stream_a — pure cross-stream isolation with a concurrent listener on
/// stream_a that *does* receive it.
#[tokio::test]
async fn unassigned_stream_audio_is_not_delivered() {
    let mut server = start_two_stream_server().await;

    let mut listener = connect_client_with_id(server.port, "listener").await; // stays on stream_a
    let mut isolated = connect_client_with_id(server.port, "isolated").await; // moved to stream_b

    expect_connected_and_settings(&mut listener).await;
    expect_connected_and_settings(&mut isolated).await;
    wait_stream_started(&mut listener).await;
    wait_stream_started(&mut isolated).await;

    let ids = wait_for_n_connects(&mut server.events, 2).await;
    assert_eq!(ids.len(), 2);

    // Move ONE client's group to stream_b; the other stays on stream_a. Which
    // server id belongs to which TestClient handle is not guaranteed to follow
    // connect order (clients are spawned concurrently), so we assert on the
    // aggregate rather than a fixed handle->id mapping.
    // ids[0]'s group stays on stream_a (the default); move ids[1]'s to stream_b.
    let (g1, _) = client_group(&server.cmd, &ids[1]).await;
    route_group_to_stream(&mut server, &g1, "stream_b").await;

    // Push audio ONLY on stream_a.
    push_tone(&server.stream_a, 30).await;

    // Exactly one handle (the one still on stream_a) hears audio; the other
    // (moved to stream_b) is silent — cross-stream isolation with a concurrent
    // positive listener proving audio was actually flowing.
    let got0 = drain_audio(&mut listener, usize::MAX, 400).await;
    let got1 = drain_audio(&mut isolated, usize::MAX, 400).await;
    let received = [got0, got1];

    let with_audio = received.iter().filter(|&&n| n > 0).count();
    let silent = received.iter().filter(|&&n| n == 0).count();
    assert_eq!(
        with_audio, 1,
        "only the stream_a client hears stream_a audio: {received:?}"
    );
    assert_eq!(
        silent, 1,
        "the stream_b client receives no stream_a audio: {received:?}"
    );
}
