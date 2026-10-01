//! Wave-3 property/fuzz test: codec-header parser robustness.
//!
//! For every default-reachable decoder (PCM, FLAC, Opus), feed arbitrary
//! `CodecHeader.payload` bytes — any length, truncated, garbage, or crafted to look
//! like a real header — into the untrusted codec-header parsing path
//! (RIFF/WAVE, fLaC/STREAMINFO, Opus pseudo-header / OpusHead).
//!
//! The single invariant under test: `create()` / `set_header()` must NEVER panic.
//! It must return `Ok` or `Err` for any input. A genuine panic/overflow here would be
//! a real robustness bug in production parsing code (these headers arrive over the
//! network from an untrusted server), so we deliberately do NOT wrap anything in
//! `should_panic` or narrow the input domain to dodge such a case.
//!
//! Uses DEFAULT features only. The `flac` and `opus` decoder modules are declared
//! unconditionally in `decoder/mod.rs`, so all three parsers are reachable.

use proptest::prelude::*;

use snapcast_client::decoder::{Decoder, PcmDecoder};
use snapcast_proto::message::codec_header::CodecHeader;

/// Arbitrary codec-header payload bytes.
///
/// Length spans empty up to a few KB — large enough to exceed every parser's minimum
/// header size and to hit multi-chunk / multi-segment walking paths, but capped so the
/// suite stays fast and never OOMs. `any::<u8>()` gives fully arbitrary bytes (garbage).
fn arb_payload() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..4096)
}

/// A payload that starts with a codec's magic prefix, then arbitrary bytes.
///
/// Pure random bytes almost never reproduce a 4-8 byte magic marker, so most random
/// inputs bail out at the magic check before exercising the deeper field-parsing logic
/// (chunk walking, bit-field extraction, segment tables, length arithmetic). Seeding a
/// valid prefix forces proptest past the magic gate and fuzzes the code that actually
/// does index math on attacker-controlled lengths/counts.
fn arb_payload_with_prefix(prefix: &'static [u8]) -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..4096).prop_map(move |mut rest| {
        let mut v = Vec::with_capacity(prefix.len() + rest.len());
        v.extend_from_slice(prefix);
        v.append(&mut rest);
        v
    })
}

fn header(codec: &str, payload: Vec<u8>) -> CodecHeader {
    CodecHeader {
        codec: codec.to_string(),
        payload,
    }
}

proptest! {
    // ---- PCM: RIFF/WAVE header via PcmDecoder::set_header ----

    #[test]
    fn prop_pcm_set_header_never_panics(payload in arb_payload()) {
        let h = header("pcm", payload);
        let mut dec = PcmDecoder::new();
        // Ok or Err both acceptable; the only failure mode is a panic (which aborts the test).
        let _ = dec.set_header(&h);
    }

    // "RIFF" + "WAVE" gate: force proptest past the magic check so the chunk-walking
    // loop (which does `chunk_size` arithmetic on arbitrary bytes) is exercised.
    #[test]
    fn prop_pcm_set_header_riff_prefixed_never_panics(payload in arb_payload_with_prefix(b"RIFF")) {
        let mut payload = payload;
        // Splice "WAVE" into offsets 8..12 when the buffer is long enough, so more inputs
        // clear both magic checks and reach chunk parsing.
        if payload.len() >= 12 {
            payload[8..12].copy_from_slice(b"WAVE");
        }
        let h = header("pcm", payload);
        let mut dec = PcmDecoder::new();
        let _ = dec.set_header(&h);
    }

    // ---- FLAC: fLaC + STREAMINFO via decoder::flac::create ----

    #[test]
    fn prop_flac_create_never_panics(payload in arb_payload()) {
        let h = header("flac", payload);
        let _ = snapcast_client::decoder::flac::create(&h);
    }

    #[test]
    fn prop_flac_create_magic_prefixed_never_panics(payload in arb_payload_with_prefix(b"fLaC")) {
        let h = header("flac", payload);
        let _ = snapcast_client::decoder::flac::create(&h);
    }

    // ---- Opus: Opus pseudo-header / OpusHead via decoder::opus::create ----

    #[test]
    fn prop_opus_create_never_panics(payload in arb_payload()) {
        let h = header("opus", payload);
        let _ = snapcast_client::decoder::opus::create(&h);
    }

    #[test]
    fn prop_opus_create_opushead_prefixed_never_panics(payload in arb_payload_with_prefix(b"OpusHead")) {
        let h = header("opus", payload);
        let _ = snapcast_client::decoder::opus::create(&h);
    }

    // Opus pseudo-header magic (0x4F505553, little-endian) prefix — reaches the
    // rate/bits/channels field parsing path rather than the OpusHead branch.
    #[test]
    fn prop_opus_create_pseudo_magic_prefixed_never_panics(
        payload in arb_payload_with_prefix(&[0x53, 0x55, 0x50, 0x4F])
    ) {
        let h = header("opus", payload);
        let _ = snapcast_client::decoder::opus::create(&h);
    }
}
