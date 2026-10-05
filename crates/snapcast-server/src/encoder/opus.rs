//! Opus encoder using the `opus` crate (system libopus).

use anyhow::{Result, bail};
use opus::{Application, Bitrate, Channels, Encoder as OpusEnc};
use snapcast_proto::SampleFormat;

use super::{EncodedPacket, Encoder};
use crate::AudioData;

/// Default bitrate in bits/second (matches C++ snapserver).
const DEFAULT_BITRATE: i32 = 192_000;

/// Encoder settings parsed from the codec options string.
#[derive(Debug, PartialEq)]
struct OpusOptions {
    bitrate: i32,
    complexity: Option<u8>,
}

/// Parse `BITRATE:<bps>,COMPLEXITY:<0-10>` (each key optional, any order).
fn parse_options(options: &str) -> Result<OpusOptions> {
    let mut parsed = OpusOptions {
        bitrate: DEFAULT_BITRATE,
        complexity: None,
    };
    for option in options.split(',').map(str::trim).filter(|o| !o.is_empty()) {
        let Some((key, value)) = option.split_once(':') else {
            bail!("invalid Opus option {option:?}, expected KEY:VALUE");
        };
        let value = value.trim();
        match key.trim().to_ascii_uppercase().as_str() {
            "BITRATE" => {
                let bitrate: i32 = value
                    .parse()
                    .map_err(|_| anyhow::anyhow!("invalid Opus bitrate: {value}"))?;
                if !(6_000..=512_000).contains(&bitrate) {
                    bail!("Opus bitrate must be 6000..=512000 bps, got {bitrate}");
                }
                parsed.bitrate = bitrate;
            }
            "COMPLEXITY" => {
                let complexity: u8 = value
                    .parse()
                    .map_err(|_| anyhow::anyhow!("invalid Opus complexity: {value}"))?;
                if complexity > 10 {
                    bail!("Opus complexity must be 0..=10, got {complexity}");
                }
                parsed.complexity = Some(complexity);
            }
            other => bail!("unknown Opus option {other:?} (supported: BITRATE, COMPLEXITY)"),
        }
    }
    Ok(parsed)
}

/// Opus encoder wrapping libopus via the `opus` crate.
pub struct OpusEncoder {
    format: SampleFormat,
    encoder: OpusEnc,
    header: Vec<u8>,
    frame_size: usize,
    /// Interleaved 16-bit samples not yet filling a whole Opus frame.
    pending: Vec<i16>,
    warned: bool,
}

impl OpusEncoder {
    /// Create a new Opus encoder.
    ///
    /// Options: `BITRATE:<bps>` (6000–512000, default 192000) and
    /// `COMPLEXITY:<0-10>` (default: libopus), comma-separated.
    pub fn new(format: SampleFormat, options: &str) -> Result<Self> {
        let options = parse_options(options)?;
        let sample_rate = match format.rate() {
            r @ (8000 | 12000 | 16000 | 24000 | 48000) => r,
            r => {
                tracing::warn!(codec = "opus", sample_rate = r, "unsupported sample rate");
                bail!("Opus does not support sample rate {r}");
            }
        };
        let channels = match format.channels() {
            1 => Channels::Mono,
            2 => Channels::Stereo,
            c => {
                tracing::warn!(codec = "opus", channels = c, "unsupported channel count");
                bail!("Opus does not support {c} channels");
            }
        };

        let mut encoder = OpusEnc::new(sample_rate, channels, Application::Audio)?;
        encoder.set_bitrate(Bitrate::Bits(options.bitrate))?;
        if let Some(complexity) = options.complexity {
            encoder.set_complexity(i32::from(complexity))?;
        }
        tracing::info!(
            codec = "opus",
            bitrate = options.bitrate,
            complexity = ?options.complexity,
            "Opus encoder configured"
        );

        // C++ snapserver's pseudo header (see snapcast_proto::OPUS_HEADER_ID).
        // Samples are encoded from 16-bit PCM, so 16 bits are announced.
        let mut header = Vec::with_capacity(12);
        header.extend_from_slice(&snapcast_proto::OPUS_HEADER_ID.to_le_bytes());
        header.extend_from_slice(&format.rate().to_le_bytes());
        header.extend_from_slice(&16u16.to_le_bytes());
        header.extend_from_slice(&format.channels().to_le_bytes());

        // 20ms frame size
        let frame_size = format.rate() as usize / 50;

        Ok(Self {
            format,
            encoder,
            header,
            frame_size,
            pending: Vec::new(),
            warned: false,
        })
    }
}

impl Encoder for OpusEncoder {
    fn name(&self) -> &str {
        snapcast_proto::CODEC_OPUS
    }

    fn header(&self) -> &[u8] {
        &self.header
    }

    fn encode(&mut self, input: &AudioData) -> Result<Vec<EncodedPacket>> {
        let pcm = match input {
            AudioData::Pcm(data) if self.format.bits() == 16 => {
                std::borrow::Cow::Borrowed(data.as_slice())
            }
            AudioData::Pcm(data) => {
                if !self.warned {
                    self.warned = true;
                    tracing::warn!(
                        codec = "opus",
                        bits = self.format.bits(),
                        "PCM input requires quantization to 16-bit for Opus"
                    );
                }
                let samples = super::pcm_to_f32(data, self.format.bits());
                std::borrow::Cow::Owned(super::f32_to_pcm(&samples, 16))
            }
            AudioData::F32(samples) => {
                if !self.warned {
                    self.warned = true;
                    tracing::warn!(
                        codec = "opus",
                        "F32 input requires quantization to 16-bit — consider pcm for lossless path"
                    );
                }
                std::borrow::Cow::Owned(super::f32_to_pcm(samples, 16))
            }
        };

        let channels = self.format.channels() as usize;
        let frame_samples = self.frame_size * channels;
        // Frames carried from earlier input start before this input does.
        let carried_frames = (self.pending.len() / channels) as i64;
        // Whole inter-channel frames only, so interleaving never shifts.
        let aligned = pcm.len() - pcm.len() % (channels * 2);
        self.pending.extend(
            pcm[..aligned]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| i16::from_le_bytes(*b)),
        );
        tracing::trace!(
            codec = "opus",
            input_bytes = pcm.len(),
            pending_frames = self.pending.len() / channels,
            "encode"
        );

        let mut packets = Vec::new();
        let mut encode_buf = [0u8; 4096];
        let mut consumed = 0;
        while self.pending.len() - consumed >= frame_samples {
            let samples = &self.pending[consumed..consumed + frame_samples];
            let len = match self.encoder.encode(samples, &mut encode_buf) {
                Ok(len) => len,
                Err(e) => {
                    tracing::warn!(codec = "opus", error = %e, "encode failed");
                    self.pending.drain(..consumed + frame_samples);
                    bail!("Opus encode failed: {e}");
                }
            };
            packets.push(EncodedPacket {
                data: encode_buf[..len].to_vec(),
                offset_frames: (packets.len() * self.frame_size) as i64 - carried_frames,
            });
            consumed += frame_samples;
        }
        self.pending.drain(..consumed);

        Ok(packets)
    }

    fn reset(&mut self) {
        self.pending.clear();
        if let Err(e) = self.encoder.reset_state() {
            tracing::warn!(codec = "opus", error = %e, "reset failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_options_use_defaults() {
        assert_eq!(
            parse_options("").unwrap(),
            OpusOptions {
                bitrate: DEFAULT_BITRATE,
                complexity: None
            }
        );
    }

    #[test]
    fn parses_bitrate_and_complexity() {
        assert_eq!(
            parse_options("BITRATE:256000,COMPLEXITY:10").unwrap(),
            OpusOptions {
                bitrate: 256_000,
                complexity: Some(10)
            }
        );
    }

    #[test]
    fn rejects_out_of_range_and_unknown_options() {
        assert!(parse_options("BITRATE:1000").is_err());
        assert!(parse_options("COMPLEXITY:11").is_err());
        assert!(parse_options("FOO:1").is_err());
        assert!(parse_options("BITRATE").is_err());
    }

    #[test]
    fn header_is_cpp_pseudo_header() {
        let enc = OpusEncoder::new(SampleFormat::new(48000, 24, 2), "").unwrap();
        let h = enc.header();
        assert_eq!(h.len(), 12);
        assert_eq!(&h[..4], b"SUPO"); // 0x4F505553 little-endian
        assert_eq!(u32::from_le_bytes(h[4..8].try_into().unwrap()), 48000);
        assert_eq!(u16::from_le_bytes(h[8..10].try_into().unwrap()), 16);
        assert_eq!(u16::from_le_bytes(h[10..12].try_into().unwrap()), 2);
    }

    /// Interleaved 16-bit stereo PCM bytes for `frames` frames of a tone.
    fn tone(frames: usize) -> Vec<u8> {
        (0..frames * 2)
            .flat_map(|n| ((((n as f32) * 0.05).sin() * 8000.0) as i16).to_le_bytes())
            .collect()
    }

    #[test]
    fn multi_frame_input_yields_one_decodable_packet_per_frame() {
        let fmt = SampleFormat::new(48000, 16, 2);
        let mut enc = OpusEncoder::new(fmt, "").unwrap();
        // 300 frames stay pending, then 3.5 Opus frames complete three more.
        assert!(enc.encode(&AudioData::Pcm(tone(300))).unwrap().is_empty());
        let packets = enc.encode(&AudioData::Pcm(tone(960 * 3 + 480))).unwrap();
        assert_eq!(packets.len(), 3, "one packet per 20 ms frame");
        let offsets: Vec<i64> = packets.iter().map(|p| p.offset_frames).collect();
        assert_eq!(offsets, [-300, 960 - 300, 2 * 960 - 300]);

        let mut dec = opus::Decoder::new(48000, Channels::Stereo).unwrap();
        let mut out = vec![0i16; 5760 * 2];
        for p in &packets {
            let n = dec.decode(&p.data, &mut out, false).unwrap();
            assert_eq!(n, 960, "each packet decodes to exactly one frame");
        }
        assert_eq!(enc.pending.len(), (300 + 480) * 2, "remainder is carried");
    }

    #[test]
    fn reset_drops_partial_frame() {
        let mut enc = OpusEncoder::new(SampleFormat::new(48000, 16, 2), "").unwrap();
        assert!(enc.encode(&AudioData::Pcm(tone(500))).unwrap().is_empty());
        enc.reset();
        assert!(enc.pending.is_empty());
    }

    #[test]
    fn encoder_accepts_inline_codec_options() {
        let format = SampleFormat::new(48000, 16, 2);
        let enc = crate::encoder::create(&crate::encoder::EncoderConfig {
            codec: "opus:BITRATE:256000,COMPLEXITY:10".into(),
            format,
            options: String::new(),
        })
        .unwrap();
        assert_eq!(enc.name(), "opus");
    }
}
