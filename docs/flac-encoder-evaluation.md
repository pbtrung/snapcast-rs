# FLAC encoder evaluation: pure Rust vs. vendored C

**Status:** **Implemented, 2026-06-30.**
`crates/snapcast-server/src/encoder/flac.rs` now encodes FLAC with `flacenc`
(pure Rust). `libflac-sys`, CMake, and the C-toolchain build requirement are
gone, and `snapcast-server` is now free of `unsafe` entirely. This document
records why FLAC encoding *was* C, what changed on crates.io to make a
pure-Rust move viable, and why `flacenc` was chosen.

## TL;DR

**Migrated to [`flacenc`](https://crates.io/crates/flacenc).** Both `flacenc`
and
[`oxideav-flac`](https://crates.io/crates/oxideav-flac) pass a real
correctness spike — chunked streaming encode that decodes bit-exact via two
independent decoders, including the partial-block edge case. `flacenc` wins
on track record (4 years, 518K downloads, zero security advisories, a lean
dependency footprint once slimmed) without giving up anything on API fit.
`oxideav-flac` is a genuinely good, actively-developed fit too — its
push/pull API arguably matches our use case even more naturally, and it's
part of a serious multi-codec framework (the same `Encoder` trait is
exercised across cinepak/h264/tta/magicyuv) — it's just earlier in its
lifecycle (`0.0.x`, ~2.5 months old at evaluation time) than we'd want to
depend on today. **Worth revisiting once it reaches a more established
release.**

## Why FLAC encoding was C (history)

`snapcast-server`'s FLAC encoder had flipped implementations twice before
this, both times for protocol-correctness reasons, not lack of trying pure
Rust:

1. Originally `flac-bound` (C bindings).
2. Switched to [`flac-codec`](https://crates.io/crates/flac-codec) (pure
   Rust, zero C dependency) — explicitly verified on Windows at the time.
3. Reverted to `flac-bound`, then to the current `libflac-sys`, the same
   day: `flac-codec` produced **complete standalone FLAC files** per
   `encode()` call, but the Snapcast wire protocol needs a **continuous
   stream of raw frames** (header once, then per-`WireChunk` frame bytes).
   Each call's file-level framing leaked into the frame stream, causing
   client decode errors (`frame header reserved bit`).

`libflac-sys` vendored the real libFLAC C source and built it via CMake at
compile time, uniformly on every platform (no system package needed). It
worked correctly. The only real cost was that it wasn't pure Rust: a
`flac`-feature build needed a C/C++ toolchain + CMake available, which is
otherwise unnecessary for this project — and the FLAC write-callback was the
last `unsafe` block in `snapcast-server`. Both are now gone.

## Why revisit now

A 2026-06-30 crates.io search for FLAC crates surfaced several actively
maintained pure-Rust encoders that didn't exist (or weren't relevant) when
the `flac-codec` attempt was reverted. Re-checking made sense rather than
assuming the landscape was still the same.

## Candidates surveyed

| Crate | Downloads | Verdict |
|---|---|---|
| **`flacenc`** | 518K | ✅ Selected — see below |
| **`oxideav-flac`** | 1.3K | ✅ Architecturally excellent fit, revisit later — see below |
| `libflac-rs` | 52 | ❌ Ruled out — see below |
| `flac-io` | 145 | ❌ Ruled out — see below |

**`libflac-rs`** ("bit-exact pure-Rust port of libFLAC") looked promising
from its naming, but a full read of its public API found no incremental
encode path anywhere in the crate. `Encoder::encode_frames(&self, ...)`
takes `&self` (immutable) and the struct holds only encoder *settings*, no
mutable encode state — it cannot remember a frame counter or prediction
history between calls. Its own doc comment gives away the actual design
target: *"the byte stream **MAME/CHD** embeds"* (CHD is the MAME emulator's
disc-image format) — built to encode one complete, known-in-advance track
in one shot, not to take live chunks over time. Same structural limitation
that broke `flac-codec`.

**`flac-io`**'s own crate-level doc comment states its purpose directly:
*"It exists for steganography, watermarking, forensic analysis, and any
audio work that needs the decoded sample plane with a guarantee that a
decode followed by an encode preserves the data exactly."* A decode →
re-encode-to-a-complete-file round-trip tool, by design. Its `encoder`
module exposes zero public items beyond a single top-level `encode(&audio)
-> Vec<u8>` — no lower-level escape hatch.

## The two real candidates

**`flacenc`** (Google, 2022–present): the convenience API
(`encode_with_fixed_block_size`) is whole-buffer, same shape as
`flac-codec`. But it also exports `encode_fixed_size_frame(config,
&framebuf, frame_number, &stream_info) -> Frame` at the crate root — a
lower-level per-block function we call ourselves, holding `frame_number` as
our own field. `Frame`, `StreamInfo`, and `MetadataBlock` each implement
`BitRepr` independently (`.write()` into any `BitSink`), so a frame and the
header serialize to bytes separately — exactly "header once, frame bytes
per chunk."

**`oxideav-flac`** (part of the `OxideAV` multi-codec framework — the same
`Encoder` trait used here is also implemented for cinepak, h264, tta, and
magicyuv): a first-class push/pull streaming API —
`send_frame(&Frame::Audio(..))` / `receive_packet() -> Packet` — the same
shape as FFmpeg's `avcodec_send_frame`/`avcodec_receive_packet`. The header
(STREAMINFO) is exposed separately via `encoder.output_params().extradata`,
populated immediately at construction with placeholder stats for the
fields a live stream can't know yet (total samples, min/max frame size,
MD5) — matching exactly how our current `libflac-sys` header already works.
`receive_packet()` only ever yields raw frame-data packets. Frame numbering
persists on the encoder's own internal state across the whole session — one
less invariant for an integration to track than `flacenc` requires.

## Spike: does it actually round-trip?

The API shape isn't enough on its own — `flac-codec` *looked* usable too,
right up until real decoders rejected its output. So before trusting either
candidate, both were run through a harness that mimics production exactly:
1152-frame blocks (`snapserver-rs`'s actual `FLAC_BLOCK_FRAMES`), 48 kHz /
16-bit / stereo, header emitted once with placeholder stats, frame bytes
concatenated per chunk — tested with both a block-aligned duration and one
with a 240-frame partial final block.

Verified two independent ways: the **real C reference libFLAC** (`flac -t`,
completely independent of both Rust candidates) and **`symphonia`** (the
exact decoder `snapcast-client` already uses in production).

| | `flac -t` (C reference) | `symphonia` (production's decoder) |
|---|---|---|
| **flacenc**, aligned | ✅ OK | ✅ bit-exact, 288,000/288,000 samples |
| **flacenc**, partial-tail | ✅ OK | ✅ bit-exact, 288,480/288,480 samples |
| **oxideav-flac**, aligned | ✅ OK | ✅ bit-exact, 288,000/288,000 samples |
| **oxideav-flac**, partial-tail | ✅ OK | ✅ bit-exact, 288,480/288,480 samples |

This directly disproves the failure mode that killed `flac-codec` — neither
candidate leaks header bytes into the frame stream, confirmed by an
independent decoder rather than internal self-consistency.

Tool: [`tools/flac-encoder-spike`](../tools/flac-encoder-spike). Run it
yourself with `cd tools/flac-encoder-spike && cargo run --release`.

**Not yet tested by the spike:** sustained throughput/CPU cost under real
load, other sample formats the project supports (24-bit), and actual
integration into `snapcast-server`'s `Encoder` trait.

## Comparison: integration ergonomics

Against the real trait we'd implement
(`crates/snapcast-server/src/encoder/mod.rs`): synchronous, one
`encode(&mut self, input: &AudioData) -> Result<EncodedChunk>` call per
chunk, `header() -> &[u8]` borrowed from a stored field. (That was the trait
at evaluation time; `encode` now returns `Result<Vec<EncodedPacket>>`, one
packet per completed FLAC frame, each sent as its own `WireChunk`.)

| | `flacenc` | `oxideav-flac` |
|---|---|---|
| `header()` | Build once via `Stream::write`, cache as a field | Build once via `FLAC_MAGIC ++ extradata`, cache as a field — equally easy |
| `encode()` per chunk | `FrameBuf::with_size` → `.fill_interleaved()` → `encode_fixed_size_frame()` → `frame.write()` | Build `AudioFrame` → `send_frame()` → drain `receive_packet()` loop |
| Frame numbering | We own and increment it ourselves | Encoder owns it internally |
| Tuning knobs | `config::Encoder`: stereo/subframe coding, block size, multithread — richer surface | `FlacEncoderOptions`: padding + a streamable-subset toggle — narrower today |
| Per-chunk allocation | Yes (`FrameBuf`) | Yes (LE-byte `Vec`) — comparable; neither is zero-alloc out of the box |

Roughly a wash. `oxideav-flac` tracks slightly less state on our side;
`flacenc` has more tuning surface if we ever expose a compression-level
knob.

## Comparison: maturity

Checked with `cargo audit` against the actual locked versions, each
crate's own test suite run directly, and the real (not assumed)
dependency tree:

| | `flacenc` | `oxideav-flac` |
|---|---|---|
| Downloads | 518K | 1.3K |
| Track record | 4 years, Google-origin, published comparison report vs. the reference encoder | 2.5 months, actively developed (9 releases), part of a larger multi-codec framework |
| Own test suite | 163 unit tests, all pass | 206 tests (incl. a dedicated `roundtrip.rs`), all pass |
| API stability | `0.5.x` | `0.0.x` — semver says anything can break |
| Security advisories | none | none |
| MSRV | 1.65 | 1.80 — both well under our `1.94.1` |
| Dependency footprint | With `default-features = false` (verified it still builds and the spike still passes): just `crc`/`heapless`/`md-5`/`num-traits` — no `crossbeam-channel`, no `serde` | `oxideav-core` + `oxideav-id3` pull `serde_json` + the full `syn`/`quote`/`proc-macro2` chain via `thiserror-impl` — heavier than slimmed `flacenc` |

## Decision

`flacenc` (with `default-features = false`). Both candidates are
bitstream-verified correct, so this wasn't a correctness call — `flacenc`'s
much longer track record, broader adoption, and leaner dependency footprint
once slimmed make it the safer long-term dependency for an encoder this
project's audio path relies on.

This isn't a rejection of `oxideav-flac` — its API is arguably the more
natural fit for our streaming use case, it passed every test we threw at
it, and it's part of an actively and seriously developed project. It's
simply earlier in its lifecycle than we'd want to depend on for this today.
**Worth a second look once it reaches a more established release** (a
`0.x` past the earliest `0.0` stage, a longer track record, broader
adoption).

## What the migration did

`crates/snapcast-server/src/encoder/flac.rs` was rewritten against `flacenc`:

- The encoder buffers incoming interleaved PCM and emits fixed 1152-sample
  FLAC frames via `flacenc::encode_fixed_size_frame`, with the `fLaC` marker +
  STREAMINFO header built once via `Stream::write`. This keeps the on-wire
  stream byte-structurally the same as the libFLAC version (same frame
  cadence, same fixed-block-size STREAMINFO, same "empty result while
  buffering" contract the server's stream loop already relies on), so every
  existing test stays green.
- `libflac-sys` and its `cmake` build dependency are dropped; the `flac`
  feature now pulls only `flacenc` (no system library, no C toolchain).
- The FLAC write-callback was the last `unsafe` in `snapcast-server`, so the
  crate is now entirely `unsafe`-free under its `#![deny(unsafe_code)]`.
- Validated three independent ways: the existing end-to-end round trip
  (server encode → `symphonia` client decode) in `snapcast-tests/tests/audio.rs`,
  unit tests for 16-bit, 24-bit, buffering cadence, and header/frame
  separation, and a manual `flac -t` (C reference) pass over the production
  buffering output.

The compression-level option (`0..=8`) is still accepted and validated for
CLI/API compatibility, but `flacenc` has no libFLAC-style preset ladder, so
the level is mapped to `flacenc`'s LPC search order rather than reproducing
libFLAC's exact presets. It is set via the codec string, e.g.
`codec = flac:5` or `--codec flac:5`; plain `flac` uses `flacenc`'s default
profile.

### Supported format envelope (the one behavioral narrowing)

`flacenc` 0.5.x verifies a narrower format envelope at construction than the
vendored libFLAC did:

| Parameter | `flacenc` (now) | libFLAC (before) |
|---|---|---|
| Bit depth | 8–24-bit | up to 32-bit |
| Sample rate | ≤ 96 kHz | up to ~655 kHz |
| Channels | 1–8 | 1–8 |

Every default and realistic Snapcast configuration (`48000:16:2` default,
plus 24-bit and ≤ 96 kHz) is unaffected — this matches the codec table in the
README, which already documents FLAC as 16/24-bit. A stream requesting 32-bit
depth, a rate above 96 kHz, or more than 8 channels now fails at encoder
construction. `FlacEncoder::new` checks these bounds up front and returns an
explicit, actionable error (e.g. *"FLAC (flacenc) supports up to 24-bit
samples, got 32-bit — use a 16- or 24-bit sample format, or a different
codec"*) rather than letting `flacenc`'s opaque `VerifyError` surface as a
generic init failure. Down-quantizing 32-bit to 24-bit was considered but
rejected: it would silently degrade resolution and risk a mismatch between the
STREAMINFO bit depth and the stream's negotiated sample format, so failing
loudly with a clear remedy is the safer default. These limits, and the
channel-interleaving alignment of the streaming buffer, are covered by unit
tests in `flac.rs`.
