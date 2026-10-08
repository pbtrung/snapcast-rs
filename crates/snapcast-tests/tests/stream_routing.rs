//! Single-client stream routing: `SetGroupStream` and mute/unmute commands
//! take effect in the server state while audio flows on both streams.
//!
//! These tests check the commands are applied and that pushing audio around
//! them does not fail; they do not inspect what the client decodes.
//! `multi_client.rs` asserts on decoded audio per client.

use snapcast_client::ClientEvent;
use snapcast_server::{AudioData, AudioFrame, ServerCommand, ServerEvent};
use snapcast_tests::{
    client_group, connect_client, expect_event, expect_server_event, start_two_stream_server,
};

fn silence_frame() -> AudioFrame {
    AudioFrame {
        data: AudioData::F32(vec![0.0; 960]),
        timestamp_usec: 0,
    }
}

fn tone_frame() -> AudioFrame {
    AudioFrame {
        data: AudioData::F32((0..960).map(|i| (i as f32 / 960.0) * 2.0 - 1.0).collect()),
        timestamp_usec: 0,
    }
}

/// Wait for the server's `ClientConnected` event and return the client id.
async fn wait_for_connect(server: &mut snapcast_tests::TwoStreamServer) -> String {
    expect_server_event(&mut server.events, 2000, |e| match e {
        ServerEvent::ClientConnected { id, .. } => Some(id),
        _ => None,
    })
    .await
}

#[tokio::test]
async fn set_group_stream_reroutes_client_while_audio_flows() {
    let mut server = start_two_stream_server().await;
    let mut client = connect_client(server.port).await;

    // Wait for client to connect and stream to start
    let client_id = wait_for_connect(&mut server).await;
    expect_event(&mut client.events, 2000, |e| match e {
        ClientEvent::StreamStarted { .. } => Some(()),
        _ => None,
    })
    .await;

    // Client is in group assigned to stream_a (first stream = default)
    let (group_id, stream_id) = client_group(&server.cmd, &client_id).await;
    assert_eq!(stream_id, "stream_a");

    // Feed both streams: tone on the assigned one, silence on the other.
    for _ in 0..5 {
        server.stream_a.send(tone_frame()).await.unwrap();
        server.stream_b.send(silence_frame()).await.unwrap();
    }

    // Switch client's group to stream_b and wait for the routing change to apply.
    server
        .cmd
        .send(ServerCommand::SetGroupStream {
            group_id: group_id.clone(),
            stream_id: "stream_b".into(),
        })
        .await
        .unwrap();
    expect_server_event(&mut server.events, 2000, |e| {
        matches!(e, ServerEvent::GroupStreamChanged { .. }).then_some(())
    })
    .await;

    // Verify the switch happened
    let (_, new_stream) = client_group(&server.cmd, &client_id).await;
    assert_eq!(new_stream, "stream_b");

    // Keep both streams fed after the switch; the server must accept audio on
    // both regardless of routing.
    for _ in 0..5 {
        server.stream_b.send(tone_frame()).await.unwrap();
        server.stream_a.send(silence_frame()).await.unwrap();
    }
}

#[tokio::test]
async fn mute_and_unmute_apply_while_audio_flows() {
    let mut server = start_two_stream_server().await;
    let mut client = connect_client(server.port).await;

    let client_id = wait_for_connect(&mut server).await;
    expect_event(&mut client.events, 2000, |e| match e {
        ClientEvent::StreamStarted { .. } => Some(()),
        _ => None,
    })
    .await;

    // Mute the client
    server
        .cmd
        .send(ServerCommand::SetClientVolume {
            client_id: client_id.clone(),
            volume: 0,
            muted: true,
        })
        .await
        .unwrap();
    expect_server_event(&mut server.events, 2000, |e| {
        matches!(e, ServerEvent::ClientVolumeChanged { muted: true, .. }).then_some(())
    })
    .await;

    // Send audio while muted (the server skips sending chunks to muted clients;
    // multi_client.rs asserts that on the decoded output).
    for _ in 0..5 {
        server.stream_a.send(tone_frame()).await.unwrap();
    }

    // Unmute
    server
        .cmd
        .send(ServerCommand::SetClientVolume {
            client_id: client_id.clone(),
            volume: 100,
            muted: false,
        })
        .await
        .unwrap();
    expect_server_event(&mut server.events, 2000, |e| {
        matches!(e, ServerEvent::ClientVolumeChanged { muted: false, .. }).then_some(())
    })
    .await;

    // Keep audio flowing after unmuting.
    for _ in 0..5 {
        server.stream_a.send(tone_frame()).await.unwrap();
    }
}
