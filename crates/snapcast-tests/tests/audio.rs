use snapcast_client::ClientEvent;
use snapcast_tests::{
    FLAC_BLOCK_FRAMES, collect_audio, connect_client, expect_client_connected,
    expect_client_disconnected, expect_event, start_server, tone_frame,
};

#[tokio::test]
async fn server_sees_client_connect_and_disconnect() {
    let mut server = start_server().await;
    let client = connect_client(server.port).await;

    let client_id = expect_client_connected(&mut server.events).await;
    assert!(!client_id.is_empty());

    client
        .cmd
        .send(snapcast_client::ClientCommand::Stop)
        .await
        .ok();
    expect_client_disconnected(&mut server.events, &client_id).await;
}

#[tokio::test]
async fn audio_round_trip() {
    let server = start_server().await;
    let mut client = connect_client(server.port).await;

    // The test crate builds the server with `flac`, the default codec.
    let codec = expect_event(&mut client.events, 2000, |e| match e {
        ClientEvent::StreamStarted { codec, .. } => Some(codec),
        _ => None,
    })
    .await;
    assert_eq!(codec, snapcast_proto::CODEC_FLAC);

    // 20 frames of 10 ms: 200 ms, 9600 interleaved samples. Decoding does not
    // wait for time sync, so there is no need to wait for it here.
    let mut ts = 1_000_000_000; // Start at 1000 s to stay away from 0
    for _ in 0..20 {
        server.audio_tx.send(tone_frame(ts)).await.unwrap();
        ts += 10_000;
    }

    // The encoder keeps a partial FLAC block back, so expect whole blocks only.
    let frames = collect_audio(&mut client.audio_rx, 4 * FLAC_BLOCK_FRAMES * 2, 5000).await;
    for frame in &frames {
        assert_eq!(frame.sample_rate, 48000);
        assert_eq!(frame.channels, 2);
        assert!(
            frame.samples.iter().any(|&s| s.abs() > 0.1),
            "decoded audio is not silent"
        );
    }
}
