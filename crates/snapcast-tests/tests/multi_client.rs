//! Multi-client isolation.
//!
//! Several clients against one server, each with its own host id (which the
//! server uses as the client id, so every client gets its own group). Server
//! commands route the groups to different streams and mute a client, and
//! each client's decoded audio is checked:
//!
//! - a client receives only the audio of the stream its group is assigned to
//!   (the two streams carry different signals, told apart by content),
//! - a muted client receives no audio at all.

use snapcast_client::ClientEvent;
use snapcast_server::{AudioFrame, ServerCommand, ServerEvent};
use snapcast_tests::{
    TestClient, TwoStreamServer, client_group, connect_client_with_id, constant_frame,
    expect_client_connected, expect_event, expect_server_event, server_status,
    start_two_stream_server, tone_frame,
};
use tokio::sync::mpsc;

/// Level of the constant signal on `stream_b`; `stream_a` carries a ramp.
const LEVEL_B: f32 = 0.25;

/// What a client decoded, in decoded frames (one per FLAC block).
#[derive(Debug, Default)]
struct Heard {
    frames: usize,
    /// Frames holding nothing but the `stream_b` level.
    from_b: usize,
}

/// Push 300 ms of audio on both streams: a ramp on `stream_a`, the constant
/// [`LEVEL_B`] on `stream_b`.
async fn push_both(server: &TwoStreamServer) {
    push(&server.stream_a, tone_frame).await;
    push(&server.stream_b, |ts| constant_frame(LEVEL_B, ts)).await;
}

async fn push(stream: &mpsc::Sender<AudioFrame>, frame: impl Fn(i64) -> AudioFrame) {
    let mut ts = 1_000_000_000i64;
    for _ in 0..30 {
        stream.send(frame(ts)).await.unwrap();
        ts += 10_000;
    }
}

/// Collect the ids of the next `n` distinct clients the server sees connect.
async fn wait_for_n_connects(events: &mut mpsc::Receiver<ServerEvent>, n: usize) -> Vec<String> {
    let mut ids = Vec::new();
    while ids.len() < n {
        let id = expect_client_connected(events).await;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

/// Wait for `Connected`, `ServerSettings` and `StreamStarted`, so any
/// WireChunk that arrives afterwards is decoded onto `audio_rx`.
async fn expect_ready(client: &mut TestClient) {
    expect_event(&mut client.events, 5000, |e| {
        matches!(e, ClientEvent::Connected { .. }).then_some(())
    })
    .await;
    expect_event(&mut client.events, 5000, |e| {
        matches!(e, ClientEvent::ServerSettings { .. }).then_some(())
    })
    .await;
    expect_event(&mut client.events, 5000, |e| {
        matches!(e, ClientEvent::StreamStarted { .. }).then_some(())
    })
    .await;
}

/// Drain the client's decoded audio until `quiet_ms` pass without a frame.
/// Proving that a client hears nothing needs such a window.
async fn drain_audio(client: &mut TestClient, quiet_ms: u64) -> Heard {
    let mut heard = Heard::default();
    let quiet = std::time::Duration::from_millis(quiet_ms);
    while let Ok(Some(frame)) = tokio::time::timeout(quiet, client.audio_rx.recv()).await {
        assert_eq!((frame.sample_rate, frame.channels), (48000, 2));
        heard.frames += 1;
        if frame.samples.iter().all(|&s| (s - LEVEL_B).abs() < 1e-3) {
            heard.from_b += 1;
        }
    }
    heard
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
    expect_server_event(&mut server.events, 2000, |e| match e {
        ServerEvent::GroupStreamChanged {
            group_id: g,
            stream_id: s,
        } if g == group_id && s == stream_id => Some(()),
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
    expect_server_event(&mut server.events, 2000, |e| match e {
        ServerEvent::ClientVolumeChanged {
            client_id: c,
            muted: true,
            ..
        } if c == client_id => Some(()),
        _ => None,
    })
    .await;
}

/// Three clients all complete the handshake, and the server tracks each in
/// a group of its own on the default stream.
#[tokio::test]
async fn three_clients_get_their_own_groups() {
    let mut server = start_two_stream_server().await;
    let ids = ["client-a", "client-b", "client-c"];
    let mut clients = Vec::new();
    for id in ids {
        clients.push(connect_client_with_id(server.port, id).await);
    }
    for client in &mut clients {
        expect_ready(client).await;
    }

    let mut seen = wait_for_n_connects(&mut server.events, 3).await;
    seen.sort();
    assert_eq!(seen, ids, "client ids are the host ids");

    let status = server_status(&server.cmd).await;
    assert_eq!(status.server.groups.len(), 3);
    for group in &status.server.groups {
        assert_eq!(group.clients.len(), 1, "one client per group");
        assert_eq!(
            group.stream_id, "stream_a",
            "the first stream is the default"
        );
    }
}

/// Three clients, groups routed to `stream_a`, `stream_b` and `stream_b`, the
/// last client muted. Each client hears exactly its stream; the muted one
/// hears nothing.
#[tokio::test]
async fn clients_receive_only_their_assigned_stream_audio() {
    let mut server = start_two_stream_server().await;
    let mut on_a = connect_client_with_id(server.port, "client-a").await;
    let mut on_b = connect_client_with_id(server.port, "client-b").await;
    let mut muted = connect_client_with_id(server.port, "client-c").await;
    expect_ready(&mut on_a).await;
    expect_ready(&mut on_b).await;
    expect_ready(&mut muted).await;
    wait_for_n_connects(&mut server.events, 3).await;

    let (group_b, _) = client_group(&server.cmd, "client-b").await;
    let (group_c, _) = client_group(&server.cmd, "client-c").await;
    route_group_to_stream(&mut server, &group_b, "stream_b").await;
    route_group_to_stream(&mut server, &group_c, "stream_b").await;
    mute_client(&mut server, "client-c").await;

    push_both(&server).await;
    let a = drain_audio(&mut on_a, 400).await;
    let b = drain_audio(&mut on_b, 400).await;
    let c = drain_audio(&mut muted, 400).await;

    assert!(a.frames > 0, "stream_a client hears audio: {a:?}");
    assert_eq!(a.from_b, 0, "stream_a client hears only stream_a: {a:?}");
    assert!(b.frames > 0, "stream_b client hears audio: {b:?}");
    assert_eq!(
        b.from_b, b.frames,
        "stream_b client hears only stream_b: {b:?}"
    );
    assert_eq!(c.frames, 0, "the muted client hears nothing: {c:?}");
}

/// Audio pushed only on `stream_a` reaches the client on `stream_a` and not
/// the one moved to `stream_b`.
#[tokio::test]
async fn unassigned_stream_audio_is_not_delivered() {
    let mut server = start_two_stream_server().await;
    let mut listener = connect_client_with_id(server.port, "listener").await;
    let mut isolated = connect_client_with_id(server.port, "isolated").await;
    expect_ready(&mut listener).await;
    expect_ready(&mut isolated).await;
    wait_for_n_connects(&mut server.events, 2).await;

    let (group, _) = client_group(&server.cmd, "isolated").await;
    route_group_to_stream(&mut server, &group, "stream_b").await;

    push(&server.stream_a, tone_frame).await;
    let heard = drain_audio(&mut listener, 400).await;
    let leaked = drain_audio(&mut isolated, 400).await;
    assert!(heard.frames > 0, "the stream_a client hears stream_a");
    assert_eq!(leaked.frames, 0, "the stream_b client hears nothing");
}
