//! Decoded-audio content correctness.
//!
//! Unlike `audio.rs`, this test feeds the server a known deterministic
//! waveform and asserts that the client decodes exactly the expected samples.
//!
//! The codec is FLAC (the default; the test crate enables the server's `flac`
//! feature), a lossless path. The only transformation the content undergoes
//! is a well-defined 16-bit quantization on the server:
//!
//!   server:  q = (s.clamp(-1,1) * i16::MAX) as i16     // truncate toward zero
//!   flac:    encodes/decodes those i16 samples losslessly (bit-exact)
//!   client:  symphonia decodes i16 -> f32 by dividing by 2^15 (32768)
//!
//! So every decoded sample must equal
//! `((s.clamp(-1,1) * 32767.0) as i16) as f32 / 32768.0` exactly, in order,
//! from the first pushed sample on: the server session subscribes to the
//! stream's chunks before the handshake, so audio pushed after
//! `StreamStarted` loses no prefix.

use snapcast_client::ClientEvent;
use snapcast_tests::{
    FLAC_BLOCK_FRAMES, collect_audio, connect_client, expect_event, start_server,
};

const CHANNELS: usize = 2;
const SAMPLE_RATE: u32 = 48_000;

/// Interleaved samples per FLAC block (both channels).
const BLOCK_SAMPLES: usize = FLAC_BLOCK_FRAMES * CHANNELS;

/// The server's 16-bit quantization followed by symphonia's i16 -> f32 decode.
fn expected_decoded(s: f32) -> f32 {
    let q = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16; // truncate toward zero
    q as f32 / 32768.0
}

/// Interleaved sample `n` of the test signal: the left channel is a 480 Hz
/// sine (period 100 frames), the right one a ramp with period 500 frames,
/// both within ±0.5 so nothing clips.
fn sample(n: usize) -> f32 {
    let frame = n / CHANNELS;
    if n.is_multiple_of(CHANNELS) {
        let phase = frame as f32 * (2.0 * std::f32::consts::PI * 480.0 / SAMPLE_RATE as f32);
        0.5 * phase.sin()
    } else {
        (frame % 500) as f32 / 500.0 - 0.5
    }
}

#[tokio::test]
async fn flac_roundtrip_preserves_waveform_content() {
    let server = start_server().await;
    let mut client = connect_client(server.port).await;

    let codec = expect_event(&mut client.events, 2000, |e| match e {
        ClientEvent::StreamStarted { codec, format } => {
            assert_eq!(format.rate(), SAMPLE_RATE, "unexpected sample rate");
            assert_eq!(format.channels(), CHANNELS as u16, "unexpected channels");
            Some(codec)
        }
        _ => None,
    })
    .await;
    assert_eq!(codec, snapcast_proto::CODEC_FLAC);

    // Whole FLAC blocks, each pushed as one F32 frame with its own timestamp.
    const BLOCKS: usize = 8;
    let signal: Vec<f32> = (0..BLOCKS * BLOCK_SAMPLES).map(sample).collect();
    let mut ts: i64 = 1_000_000_000;
    for block in signal.chunks(BLOCK_SAMPLES) {
        server
            .audio_tx
            .send(snapcast_server::AudioFrame {
                data: snapcast_server::AudioData::F32(block.to_vec()),
                timestamp_usec: ts,
            })
            .await
            .expect("audio_tx send failed");
        ts += (FLAC_BLOCK_FRAMES as i64 * 1_000_000) / SAMPLE_RATE as i64;
    }

    let mut received = Vec::with_capacity(signal.len());
    for frame in collect_audio(&mut client.audio_rx, signal.len(), 5000).await {
        assert_eq!(frame.sample_rate, SAMPLE_RATE);
        assert_eq!(frame.channels, CHANNELS as u16);
        assert_eq!(
            frame.samples.len(),
            BLOCK_SAMPLES,
            "decoded frame not a whole FLAC block"
        );
        received.extend_from_slice(&frame.samples);
    }
    assert_eq!(received.len(), signal.len());

    for (n, (&got, &input)) in received.iter().zip(&signal).enumerate() {
        let want = expected_decoded(input);
        assert!(
            got == want,
            "sample {n} (frame {}, channel {}): decoded {got}, expected {want}",
            n / CHANNELS,
            n % CHANNELS
        );
    }
    // The signal has energy, so a decoder emitting silence cannot pass.
    assert!(received.iter().any(|s| s.abs() > 0.4));
}
