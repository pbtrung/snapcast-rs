//! Wave-2 integration test: decoded-audio CONTENT correctness.
//!
//! Unlike `audio.rs` (which only counts that *some* samples arrive), this test
//! feeds the server a KNOWN deterministic waveform, receives the decoded audio
//! on the client, and asserts the decoded samples match the input by CONTENT.
//!
//! We stay on the DEFAULT feature set, so the codec is FLAC — a LOSSLESS path.
//! The only transformation the content undergoes is a well-defined 16-bit
//! quantization on the server:
//!
//!   server:  q = (s.clamp(-1,1) * i16::MAX) as i16     // truncate toward zero
//!   flac:    encodes/decodes those i16 samples LOSSLESSLY (bit-exact)
//!   client:  symphonia decodes i16 -> f32 by dividing by 2^15 (32768)
//!
//! So the expected decoded value for an input sample `s` is:
//!
//!   expected(s) = ((s.clamp(-1,1) * 32767.0) as i16) as f32 / 32768.0
//!
//! We assert BOTH sample-by-sample (|decoded - expected(s)| within a tiny
//! tolerance) and RMS(decoded - expected) within a tiny tolerance, after
//! aligning the received stream to the input (the first received FLAC block may
//! sit at a non-zero offset, and the encoder buffers a trailing partial block,
//! so we compare a contiguous interior region).

use snapcast_client::ClientEvent;
use snapcast_tests::{connect_client, expect_event, start_server};

/// FLAC fixed block size in *frames* (per channel). Mirrors the server encoder's
/// `BLOCK_SIZE` constant; used only to size the pushed audio into whole blocks
/// and to reason about alignment. Not imported (it's private), duplicated here.
const FLAC_BLOCK_FRAMES: usize = 1152;

const CHANNELS: usize = 2;
const SAMPLE_RATE: u32 = 48_000;

/// Interleaved samples per FLAC block (both channels).
const BLOCK_SAMPLES: usize = FLAC_BLOCK_FRAMES * CHANNELS;

/// The server's exact 16-bit quantization + symphonia's i16->f32 decode.
/// This is the ground truth a LOSSLESS (flac) round-trip must reproduce.
fn expected_decoded(s: f32) -> f32 {
    let q = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16; // truncate toward zero
    q as f32 / 32768.0
}

/// Deterministic known waveform: a stereo signal where the left channel is a
/// sine and the right channel is a linear ramp. Both are non-trivial and
/// non-constant so the content check is meaningful. Amplitude 0.5 keeps us well
/// inside [-1, 1] (no clipping) so the quantization model is exact.
///
/// `frame_idx` is the per-channel sample index (0-based, monotonic across the
/// whole signal). Returns (left, right).
fn waveform(frame_idx: usize) -> (f32, f32) {
    // 480 Hz sine at 48 kHz -> period of exactly 100 frames (clean, periodic).
    let phase = (frame_idx as f32) * (2.0 * std::f32::consts::PI * 480.0 / SAMPLE_RATE as f32);
    let left = 0.5 * phase.sin();
    // Slow triangle-ish ramp in [-0.5, 0.5], period 500 frames.
    let t = (frame_idx % 500) as f32 / 500.0; // 0..1
    let right = t - 0.5;
    (left, right)
}

/// Build `frames` frames of interleaved stereo f32 samples for the known signal.
fn build_signal(frames: usize) -> Vec<f32> {
    let mut buf = Vec::with_capacity(frames * CHANNELS);
    for i in 0..frames {
        let (l, r) = waveform(i);
        buf.push(l);
        buf.push(r);
    }
    buf
}

/// Root-mean-square of a slice.
fn rms(xs: &[f32]) -> f32 {
    if xs.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = xs.iter().map(|&x| (x as f64) * (x as f64)).sum();
    ((sum_sq / xs.len() as f64).sqrt()) as f32
}

#[tokio::test]
async fn flac_roundtrip_preserves_waveform_content() {
    // Default features => FLAC, a lossless codec. Sample format 48000:16:2.
    let server = start_server().await;
    let mut client = connect_client(server.port).await;

    // Wait for the stream to start so the client's decoder is initialised and
    // (crucially) the server session is already subscribed to the chunk
    // broadcast. Pushing AFTER this point means no block prefix is dropped.
    let codec = expect_event(&mut client.events, 2000, |e| match e {
        ClientEvent::StreamStarted { codec, format } => {
            assert_eq!(format.rate(), SAMPLE_RATE, "unexpected sample rate");
            assert_eq!(format.channels(), CHANNELS as u16, "unexpected channels");
            Some(codec)
        }
        _ => None,
    })
    .await;
    // On default features this must be FLAC (lossless). If the crate were built
    // without flac it would fall back to pcm — still lossless — so we
    // only require a known lossless codec here rather than hard-coding "flac".
    assert!(
        codec == "flac" || codec == "pcm",
        "expected a lossless default codec, got {codec:?}"
    );

    // Push a known signal spanning several whole FLAC blocks. We push MORE
    // blocks than we later assert on, so a trailing partial block buffered in
    // the encoder never eats into the region we check. Realtime pacing means
    // ~ (frames / 48000) seconds of wall time, so keep it short.
    const PUSH_BLOCKS: usize = 8;
    let total_frames = FLAC_BLOCK_FRAMES * PUSH_BLOCKS; // 9216 frames (~192 ms)
    let signal = build_signal(total_frames);

    // Send as block-sized F32 frames with monotonic timestamps.
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

    // Collect decoded samples from the client. We want enough contiguous samples
    // to cover an interior comparison region even after alignment. Collect at
    // least 6 full blocks' worth; the encoder emits one WireChunk per block, so
    // received frames should each be exactly BLOCK_SAMPLES samples.
    const COLLECT_BLOCKS: usize = 6;
    let want_samples = BLOCK_SAMPLES * COLLECT_BLOCKS;

    let mut received: Vec<f32> = Vec::with_capacity(want_samples + BLOCK_SAMPLES);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while received.len() < want_samples {
        let frame = tokio::time::timeout_at(deadline, client.audio_rx.recv())
            .await
            .expect("timed out collecting decoded audio")
            .expect("audio channel closed");
        assert_eq!(frame.sample_rate, SAMPLE_RATE);
        assert_eq!(frame.channels, CHANNELS as u16);
        // Each decoded FLAC block should be exactly one full block of samples.
        assert_eq!(
            frame.samples.len(),
            BLOCK_SAMPLES,
            "decoded frame not a whole FLAC block"
        );
        received.extend_from_slice(&frame.samples);
    }

    // ---- Align received stream to the input signal ----
    // In practice the first received block corresponds to input offset 0, but
    // to be robust we search a small window of interleaved offsets (a multiple
    // of CHANNELS, so we don't swap L/R) for the best match against the
    // reference, then verify the aligned region rigorously.
    let build_expected = |offset: usize, len: usize| -> Vec<f32> {
        (0..len)
            .map(|k| {
                let interleaved = offset + k;
                let frame_idx = interleaved / CHANNELS;
                let (l, r) = waveform(frame_idx);
                let s = if interleaved.is_multiple_of(CHANNELS) {
                    l
                } else {
                    r
                };
                expected_decoded(s)
            })
            .collect()
    };

    // Region of the received stream we will compare (skip the first block to
    // avoid any edge effects, keep a solid interior chunk).
    let cmp_len = BLOCK_SAMPLES * 4;
    let recv_start = BLOCK_SAMPLES; // skip first received block
    assert!(
        received.len() >= recv_start + cmp_len,
        "not enough decoded audio collected: got {}, need {}",
        received.len(),
        recv_start + cmp_len
    );
    let recv_region = &received[recv_start..recv_start + cmp_len];

    // Find the input offset (in interleaved samples, stepping by CHANNELS) that
    // minimises RMS error. Search a generous window of whole blocks.
    let mut best_offset = 0usize;
    let mut best_err = f32::INFINITY;
    // Search up to a few blocks of possible offset; the true offset is small.
    let search_frames = FLAC_BLOCK_FRAMES * 3;
    for frame_off in 0..=search_frames {
        // build_expected is analytic in `frame_idx`, so it is valid for any
        // offset regardless of the pushed `signal` length.
        let offset = frame_off * CHANNELS;
        let expected = build_expected(offset, cmp_len);
        let diff: Vec<f32> = recv_region
            .iter()
            .zip(expected.iter())
            .map(|(a, b)| a - b)
            .collect();
        let err = rms(&diff);
        if err < best_err {
            best_err = err;
            best_offset = offset;
        }
        // Early exit: a perfect lossless match will be essentially zero.
        if best_err < 1e-6 {
            break;
        }
    }

    // ---- Assertions on the aligned region ----
    let expected = build_expected(best_offset, cmp_len);

    // 1) RMS of the content error must be tiny. The theoretical worst case is
    //    dominated by the sub-LSB scale mismatch (32767 vs 32768); empirically
    //    ~1e-5. A 1e-3 bound is a comfortable, non-flaky upper bound.
    let diff: Vec<f32> = recv_region
        .iter()
        .zip(expected.iter())
        .map(|(a, b)| a - b)
        .collect();
    let err_rms = rms(&diff);
    assert!(
        err_rms < 1e-3,
        "content RMS error too large: {err_rms} (alignment offset {best_offset})"
    );

    // 2) Sample-by-sample: every decoded sample matches the exact quantization
    //    model within one-ish LSB. i16 LSB in normalized units is 1/32768 ≈
    //    3.05e-5; allow a bit of slack for the 32767/32768 scale difference.
    let per_sample_tol = 1.5e-4_f32;
    let mut max_abs = 0.0_f32;
    for (i, (a, b)) in recv_region.iter().zip(expected.iter()).enumerate() {
        let d = (a - b).abs();
        if d > max_abs {
            max_abs = d;
        }
        assert!(
            d <= per_sample_tol,
            "sample {i} mismatch: decoded={a}, expected={b}, |diff|={d} > {per_sample_tol}"
        );
    }

    // 3) Sanity: the reference signal is non-trivial (not silence), so a bug that
    //    zeroed the audio would NOT pass the checks above. Assert the reference
    //    actually has energy.
    let ref_rms = rms(&expected);
    assert!(
        ref_rms > 0.1,
        "reference waveform unexpectedly quiet (rms={ref_rms}); test would be vacuous"
    );

    eprintln!(
        "content check ok: codec={codec}, aligned offset={best_offset}, \
         err_rms={err_rms:.3e}, max_abs_sample_err={max_abs:.3e}, ref_rms={ref_rms:.3}"
    );
}
