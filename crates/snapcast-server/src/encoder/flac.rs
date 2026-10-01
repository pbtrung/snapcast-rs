//! FLAC encoder using [`flacenc`] — pure Rust, no C dependency.
//!
//! The Snapcast wire protocol carries FLAC as a header (the `fLaC` marker plus
//! a STREAMINFO metadata block, sent once via [`Encoder::header`]) followed by
//! a continuous run of raw FLAC frames — one or more per `WireChunk`, never a
//! per-chunk file. To produce that, this encoder buffers incoming interleaved
//! PCM and emits fixed-size FLAC frames of [`BLOCK_SIZE`] samples each,
//! independent of how many frames the caller hands us per `encode` call. That
//! matches the C++ Snapcast server (and the prior libFLAC-backed
//! implementation): callers feed 1152-frame chunks for `pipe`/`tcp` sources
//! and 20 ms chunks for f32 sources, and either way the stream on the wire is a
//! compliant fixed-block-size FLAC stream.
//!
//! `encode` returns an empty chunk while it is still buffering toward a full
//! block; the server's stream loop treats an empty result as "still buffering"
//! and sends no `WireChunk`.

use anyhow::{Result, anyhow, bail};
use flacenc::bitsink::MemSink;
use flacenc::component::{BitRepr, Stream, StreamInfo};
use flacenc::config;
use flacenc::error::{Verified, Verify};
use flacenc::source::{Fill, FrameBuf};
use snapcast_proto::SampleFormat;

use super::{EncodedChunk, Encoder};
use crate::AudioData;

/// FLAC frame block size, in inter-channel samples. 1152 matches the C++
/// Snapcast server's default and the prior libFLAC build, so the produced
/// stream keeps the same frame cadence and the fixed-block-size STREAMINFO
/// declaration stays accurate for every emitted frame.
const BLOCK_SIZE: usize = 1152;

/// Streaming FLAC encoder (pure Rust via `flacenc`).
pub struct FlacEncoder {
    format: SampleFormat,
    config: Verified<config::Encoder>,
    /// Stream parameters passed to per-frame encoding (sample rate / channels /
    /// bit depth). Block-size fields are irrelevant here — each frame's block
    /// size comes from the `FrameBuf` we hand to `encode_fixed_size_frame`.
    stream_info: StreamInfo,
    /// `fLaC` marker + STREAMINFO metadata block, built once at construction.
    header: Vec<u8>,
    /// Interleaved i32 samples accumulated but not yet emitted as a full frame.
    pending: Vec<i32>,
    /// Monotonic frame counter for fixed-block-size frame headers.
    frame_number: usize,
    /// Whether we've already warned about F32 → integer-PCM quantization.
    warned: bool,
}

impl FlacEncoder {
    /// Create a new FLAC encoder.
    ///
    /// `options` is an optional compression level `0..=8`, accepted for
    /// compatibility with the previous libFLAC encoder and the C++ server's CLI.
    /// `flacenc` does not expose libFLAC's exact 0–8 preset ladder, so the level
    /// is mapped to `flacenc`'s LPC search order (higher level → higher order →
    /// better compression, more CPU); an empty string selects `flacenc`'s
    /// default profile.
    pub fn new(format: SampleFormat, options: &str) -> Result<Self> {
        let level: Option<u32> = if options.is_empty() {
            None
        } else {
            let n: u32 = options
                .parse()
                .map_err(|_| anyhow!("invalid FLAC compression level: {options}"))?;
            if n > 8 {
                bail!("FLAC compression level must be 0-8, got {n}");
            }
            Some(n)
        };

        let mut enc_cfg = config::Encoder::default();
        if let Some(level) = level {
            if level == 0 {
                // Fastest: fixed predictors only, no LPC search.
                enc_cfg.subframe_coding.use_lpc = false;
            } else {
                // Monotonic LPC-order ramp toward flacenc's max (24).
                enc_cfg.subframe_coding.qlpc.lpc_order = (level as usize * 3).clamp(1, 24);
            }
        }
        let config = enc_cfg
            .into_verified()
            .map_err(|(_, e)| anyhow!("invalid FLAC encoder config: {e}"))?;

        let rate = format.rate() as usize;
        let channels = format.channels() as usize;
        let bits = format.bits() as usize;

        // flacenc verifies a narrower envelope than libFLAC accepted: up to
        // 24-bit samples, sample rate <= 96 kHz, and 1..=8 channels (enforced by
        // its `StreamInfo::verify`). Reject anything outside it up front with an
        // actionable message instead of letting flacenc's opaque `VerifyError`
        // surface as a generic "FLAC stream init failed". This is the one
        // behavioral narrowing versus the prior libFLAC build (which accepted
        // 32-bit and rates to ~655 kHz); make it loud rather than silent.
        if bits > 24 {
            bail!(
                "FLAC (flacenc) supports up to 24-bit samples, got {bits}-bit — \
                 use a 16- or 24-bit sample format, or a different codec"
            );
        }
        if rate > 96_000 {
            bail!(
                "FLAC (flacenc) supports sample rates up to 96 kHz, got {rate} Hz — \
                 use a rate <= 96 kHz, or a different codec"
            );
        }
        if !(1..=8).contains(&channels) {
            bail!("FLAC (flacenc) supports 1-8 channels, got {channels}");
        }

        // Header: `fLaC` marker + STREAMINFO, no frames. Declare the fixed block
        // size up front (a live stream's total-samples / MD5 / frame-size stats
        // stay at their "unknown" sentinels, exactly as the C++ server emits).
        let mut header_stream = Stream::new(rate, channels, bits)
            .map_err(|e| anyhow!("FLAC stream init failed: {e}"))?;
        header_stream
            .stream_info_mut()
            .set_block_sizes(BLOCK_SIZE, BLOCK_SIZE)
            .map_err(|e| anyhow!("FLAC block size config failed: {e}"))?;
        let mut header_sink = MemSink::<u8>::new();
        header_stream
            .write(&mut header_sink)
            .map_err(|e| anyhow!("FLAC header serialization failed: {e}"))?;
        let header = header_sink.into_inner();

        let stream_info =
            StreamInfo::new(rate, channels, bits).map_err(|e| anyhow!("FLAC stream info: {e}"))?;

        tracing::info!(
            rate,
            channels,
            bits,
            block_size = BLOCK_SIZE,
            header_bytes = header.len(),
            "FLAC streaming encoder initialized (pure Rust, flacenc)"
        );

        Ok(Self {
            format,
            config,
            stream_info,
            header,
            pending: Vec::new(),
            frame_number: 0,
            warned: false,
        })
    }

    /// Decode a chunk of interleaved little-endian PCM into i32 samples.
    ///
    /// Only whole inter-channel frames are decoded; a trailing partial frame
    /// (possible only from a malformed/truncated chunk — production sources
    /// always hand us frame-aligned chunks) is dropped rather than carried, so
    /// the persistent `pending` buffer never loses channel-interleaving
    /// alignment. This matches the prior libFLAC encoder, which computed
    /// `frames = samples / channels` per call and ignored any sub-frame
    /// remainder.
    fn pcm_to_i32(&self, pcm: &[u8]) -> Result<Vec<i32>> {
        let sample_size = self.format.sample_size() as usize;
        let frame_size = sample_size * self.format.channels() as usize;
        let aligned = pcm.len() - pcm.len() % frame_size.max(1);
        let pcm = &pcm[..aligned];
        let mut out = Vec::with_capacity(pcm.len() / sample_size);
        match sample_size {
            2 => {
                for c in pcm.as_chunks::<2>().0 {
                    out.push(i16::from_le_bytes([c[0], c[1]]) as i32);
                }
            }
            4 => {
                for c in pcm.as_chunks::<4>().0 {
                    out.push(i32::from_le_bytes([c[0], c[1], c[2], c[3]]));
                }
            }
            other => bail!("unsupported sample size: {other}"),
        }
        Ok(out)
    }
}

impl Encoder for FlacEncoder {
    fn name(&self) -> &str {
        snapcast_proto::CODEC_FLAC
    }

    fn header(&self) -> &[u8] {
        &self.header
    }

    fn encode(&mut self, input: &AudioData) -> Result<EncodedChunk> {
        let pcm = match input {
            AudioData::Pcm(data) => std::borrow::Cow::Borrowed(data.as_slice()),
            AudioData::F32(samples) => {
                if !self.warned {
                    self.warned = true;
                    tracing::warn!(
                        codec = "flac",
                        bits = self.format.bits(),
                        "F32 input requires quantization — consider pcm for lossless path"
                    );
                }
                std::borrow::Cow::Owned(super::f32_to_pcm(samples, self.format.bits()))
            }
        };

        self.pending.extend(self.pcm_to_i32(&pcm)?);

        let channels = self.format.channels() as usize;
        let block_samples = BLOCK_SIZE * channels;
        let mut out = Vec::new();

        while self.pending.len() >= block_samples {
            let block: Vec<i32> = self.pending.drain(..block_samples).collect();

            let mut framebuf = FrameBuf::with_size(channels, BLOCK_SIZE)
                .map_err(|e| anyhow!("FLAC framebuf: {e}"))?;
            framebuf
                .fill_interleaved(&block)
                .map_err(|e| anyhow!("FLAC fill: {e}"))?;

            let frame_number = self.frame_number;
            let frame = flacenc::encode_fixed_size_frame(
                &self.config,
                &framebuf,
                frame_number,
                &self.stream_info,
            )
            .map_err(|e| anyhow!("FLAC encode: {e}"))?;

            let mut sink = MemSink::<u8>::new();
            frame
                .write(&mut sink)
                .map_err(|e| anyhow!("FLAC frame serialization: {e}"))?;
            out.extend_from_slice(&sink.into_inner());

            self.frame_number += 1;
        }

        Ok(EncodedChunk { data: out })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_starts_with_flac() {
        let fmt = SampleFormat::new(48000, 16, 2);
        let enc = FlacEncoder::new(fmt, "").unwrap();
        assert!(!enc.header().is_empty());
        assert_eq!(&enc.header()[..4], b"fLaC");
    }

    #[test]
    fn invalid_compression_level_rejected() {
        let fmt = SampleFormat::new(48000, 16, 2);
        assert!(FlacEncoder::new(fmt, "9").is_err());
        assert!(FlacEncoder::new(fmt, "abc").is_err());
        // Valid levels and empty are accepted.
        assert!(FlacEncoder::new(fmt, "0").is_ok());
        assert!(FlacEncoder::new(fmt, "8").is_ok());
        assert!(FlacEncoder::new(fmt, "").is_ok());
    }

    #[test]
    fn rejects_formats_outside_flacenc_envelope() {
        // 32-bit depth, >96 kHz, and >8 channels were accepted by libFLAC but
        // not by flacenc; reject them up front with a clear error rather than an
        // opaque init failure.
        assert!(FlacEncoder::new(SampleFormat::new(48000, 32, 2), "").is_err());
        assert!(FlacEncoder::new(SampleFormat::new(192000, 24, 2), "").is_err());
        assert!(FlacEncoder::new(SampleFormat::new(48000, 16, 9), "").is_err());
        // Common/realistic formats still construct (16/24-bit, <=96 kHz, <=8 ch).
        assert!(FlacEncoder::new(SampleFormat::new(48000, 16, 2), "").is_ok());
        assert!(FlacEncoder::new(SampleFormat::new(96000, 24, 8), "").is_ok());
    }

    #[test]
    fn misaligned_pcm_chunk_stays_frame_aligned() {
        // A chunk whose byte length isn't a whole number of frames must not
        // shift channel interleaving for subsequent chunks: the trailing partial
        // frame is dropped, never carried into the persistent `pending` buffer.
        let fmt = SampleFormat::new(48000, 16, 2); // 16-bit stereo => 4-byte frame
        let mut enc = FlacEncoder::new(fmt, "").unwrap();
        // 100 whole frames + 2 stray bytes (one extra sample).
        let stray = vec![0u8; 4 * 100 + 2];
        enc.encode(&AudioData::Pcm(stray)).unwrap();
        assert_eq!(
            enc.pending.len() % 2,
            0,
            "pending must hold a whole number of stereo frames"
        );
        assert_eq!(
            enc.pending.len(),
            200,
            "the stray sub-frame sample is dropped"
        );
    }

    #[test]
    fn encode_produces_frames() {
        let fmt = SampleFormat::new(48000, 16, 2);
        let mut enc = FlacEncoder::new(fmt, "").unwrap();
        let mut total = 0;
        let mut saw_frame = false;
        for _ in 0..10 {
            // 960 frames per chunk; buffers to 1152-frame FLAC frames.
            let pcm = vec![0u8; 960 * 4];
            let result = enc.encode(&AudioData::Pcm(pcm)).unwrap();
            if !result.data.is_empty() {
                saw_frame = true;
                // FLAC frame sync: 0xFF then 0xF8 (fixed) / 0xF9 (variable).
                assert_eq!(result.data[0], 0xFF);
                assert!(result.data[1] == 0xF8 || result.data[1] == 0xF9);
            }
            total += result.data.len();
        }
        assert!(saw_frame, "expected at least one FLAC frame");
        assert!(total > 0, "expected FLAC output");
    }

    #[test]
    fn frames_never_contain_the_header() {
        let fmt = SampleFormat::new(48000, 16, 2);
        let mut enc = FlacEncoder::new(fmt, "").unwrap();
        for i in 0..100 {
            // Non-constant data so frames aren't trivially all-constant subframes.
            let pcm: Vec<u8> = (0..960 * 4).map(|n| ((n + i) % 251) as u8).collect();
            let result = enc.encode(&AudioData::Pcm(pcm)).unwrap();
            if result.data.len() >= 4 {
                assert_ne!(&result.data[..4], b"fLaC", "header leaked into frame data");
            }
        }
    }

    #[test]
    fn encodes_24bit() {
        // 24-bit is carried as 4 bytes/sample (i32) with values in 24-bit range.
        let fmt = SampleFormat::new(48000, 24, 2);
        assert_eq!(fmt.sample_size(), 4);
        let mut enc = FlacEncoder::new(fmt, "").unwrap();
        assert_eq!(&enc.header()[..4], b"fLaC");
        let mut produced = false;
        for k in 0..6 {
            let mut pcm = Vec::with_capacity(1152 * 2 * 4);
            for n in 0..1152 * 2 {
                // 24-bit-range signed value, sign-extended into i32.
                let v: i32 = (((n as i64 + k as i64) * 4099) % 8_000_000 - 4_000_000) as i32;
                pcm.extend_from_slice(&v.to_le_bytes());
            }
            let out = enc.encode(&AudioData::Pcm(pcm)).unwrap();
            if !out.data.is_empty() {
                produced = true;
                assert_eq!(out.data[0], 0xFF);
            }
        }
        assert!(produced, "expected 24-bit FLAC frames");
    }

    #[test]
    fn block_size_buffering_emits_at_expected_cadence() {
        // 1152-frame chunks should each emit exactly one frame (1:1).
        let fmt = SampleFormat::new(48000, 16, 2);
        let mut enc = FlacEncoder::new(fmt, "").unwrap();
        let mut frames = 0;
        for _ in 0..4 {
            let pcm = vec![7u8; BLOCK_SIZE * 2 /*ch*/ * 2 /*bytes*/];
            let out = enc.encode(&AudioData::Pcm(pcm)).unwrap();
            if !out.data.is_empty() {
                frames += 1;
            }
        }
        assert_eq!(frames, 4, "1152-frame chunks should map 1:1 to frames");
    }
}
