//! One input frame that spans several codec packets must reach the client as
//! several `WireChunk`s, so every packet is decoded rather than only the
//! first one in a combined payload.

use snapcast_client::ClientEvent;
use snapcast_tests::{connect_client, expect_event, start_server};

/// FLAC block size in frames (mirrors the server encoder's private constant).
const FLAC_BLOCK_FRAMES: usize = 1152;
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

    let mut timestamps = Vec::new();
    let mut decoded = 0;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while decoded < FLAC_BLOCK_FRAMES * BLOCKS * CHANNELS {
        let frame = tokio::time::timeout_at(deadline, client.audio_rx.recv())
            .await
            .expect("timed out: not every FLAC block was decoded")
            .expect("audio channel closed");
        assert_eq!(frame.samples.len(), FLAC_BLOCK_FRAMES * CHANNELS);
        decoded += frame.samples.len();
        timestamps.push(frame.timestamp_usec);
    }
    assert_eq!(timestamps.len(), BLOCKS);
    // 1152 frames at 48 kHz = 24 ms between consecutive blocks.
    for pair in timestamps.windows(2) {
        assert_eq!(
            pair[1] - pair[0],
            24_000,
            "blocks are timestamped by offset"
        );
    }
}
