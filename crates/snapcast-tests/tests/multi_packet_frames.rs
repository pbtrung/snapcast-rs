//! One input frame that spans several codec packets must reach the client as
//! several `WireChunk`s, so every packet is decoded rather than only the
//! first one in a combined payload.

use snapcast_client::ClientEvent;
use snapcast_tests::{
    FLAC_BLOCK_FRAMES, collect_audio, connect_client, expect_event, start_server,
};
const CHANNELS: usize = 2;

#[tokio::test]
async fn flac_frame_spanning_several_blocks_decodes_completely() {
    let server = start_server().await;
    let mut client = connect_client(server.port).await;
    expect_event(&mut client.events, 2000, |e| {
        matches!(e, ClientEvent::StreamStarted { .. }).then_some(())
    })
    .await;

    const BLOCKS: usize = 4;
    let samples: Vec<f32> = (0..FLAC_BLOCK_FRAMES * BLOCKS * CHANNELS)
        .map(|n| ((n as f32) * 0.01).sin() * 0.5)
        .collect();
    server
        .audio_tx
        .send(snapcast_server::AudioFrame {
            data: snapcast_server::AudioData::F32(samples),
            timestamp_usec: 1_000_000_000,
        })
        .await
        .unwrap();

    let frames = collect_audio(
        &mut client.audio_rx,
        FLAC_BLOCK_FRAMES * BLOCKS * CHANNELS,
        5000,
    )
    .await;
    assert_eq!(frames.len(), BLOCKS, "one decoded frame per FLAC block");
    for frame in &frames {
        assert_eq!(frame.samples.len(), FLAC_BLOCK_FRAMES * CHANNELS);
    }
    let timestamps: Vec<i64> = frames.iter().map(|f| f.timestamp_usec).collect();
    assert_eq!(
        timestamps[0], 1_000_000_000,
        "first block keeps the input timestamp"
    );
    // 1152 frames at 48 kHz = 24 ms between consecutive blocks.
    for pair in timestamps.windows(2) {
        assert_eq!(
            pair[1] - pair[0],
            24_000,
            "blocks are timestamped by offset"
        );
    }
}
