//! Single-client stream routing: `SetGroupStream` and mute/unmute reach the
//! server state and the connected client. `multi_client.rs` checks what
//! several clients decode.

use snapcast_client::ClientEvent;
use snapcast_server::{ServerCommand, ServerEvent};
use snapcast_tests::{
    TestClient, client_group, collect_audio, connect_client, constant_frame,
    expect_client_connected, expect_event, expect_server_event, start_two_stream_server,
};

/// Wait for the client's next `StreamStarted`.
async fn expect_stream_started(client: &mut TestClient) {
    expect_event(&mut client.events, 2000, |e| {
        matches!(e, ClientEvent::StreamStarted { .. }).then_some(())
    })
    .await;
}

/// Wait for the client's next `VolumeChanged`; returns `(volume, muted)`.
async fn expect_volume(client: &mut TestClient) -> (u16, bool) {
    expect_event(&mut client.events, 2000, |e| match e {
        ClientEvent::VolumeChanged { volume, muted } => Some((volume, muted)),
        _ => None,
    })
    .await
}

#[tokio::test]
async fn set_group_stream_reroutes_client() {
    let mut server = start_two_stream_server().await;
    let mut client = connect_client(server.port).await;

    let client_id = expect_client_connected(&mut server.events).await;
    expect_stream_started(&mut client).await;

    // The first stream is the default.
    let (group_id, stream_id) = client_group(&server.cmd, &client_id).await;
    assert_eq!(stream_id, "stream_a");

    server
        .cmd
        .send(ServerCommand::SetGroupStream {
            group_id: group_id.clone(),
            stream_id: "stream_b".into(),
        })
        .await
        .unwrap();
    expect_server_event(&mut server.events, 2000, |e| match e {
        ServerEvent::GroupStreamChanged {
            group_id: g,
            stream_id: s,
        } if g == group_id && s == "stream_b" => Some(()),
        _ => None,
    })
    .await;
    let (_, new_stream) = client_group(&server.cmd, &client_id).await;
    assert_eq!(new_stream, "stream_b");

    // The client gets the new stream's codec header and then its audio.
    expect_stream_started(&mut client).await;
    let mut ts = 1_000_000_000i64;
    for _ in 0..30 {
        server.stream_b.send(constant_frame(0.5, ts)).await.unwrap();
        ts += 10_000;
    }
    let frames = collect_audio(&mut client.audio_rx, 1, 5000).await;
    assert!(
        frames[0].samples.iter().all(|&s| (s - 0.5).abs() < 1e-3),
        "decoded audio is stream_b's"
    );
}

#[tokio::test]
async fn mute_and_unmute_reach_the_client() {
    let mut server = start_two_stream_server().await;
    let mut client = connect_client(server.port).await;

    let client_id = expect_client_connected(&mut server.events).await;
    // Settings sent in the handshake: full volume, not muted.
    let initial = expect_event(&mut client.events, 2000, |e| match e {
        ClientEvent::ServerSettings { volume, muted, .. } => Some((volume, muted)),
        _ => None,
    })
    .await;
    assert_eq!(initial, (100, false));

    for (volume, muted) in [(0, true), (80, false)] {
        server
            .cmd
            .send(ServerCommand::SetClientVolume {
                client_id: client_id.clone(),
                volume,
                muted,
            })
            .await
            .unwrap();
        expect_server_event(&mut server.events, 2000, |e| {
            matches!(e, ServerEvent::ClientVolumeChanged { muted: m, .. } if m == muted)
                .then_some(())
        })
        .await;
        assert_eq!(expect_volume(&mut client).await, (volume, muted));
    }
}
