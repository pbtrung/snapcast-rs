//! Opus encoder using the `opus` crate (system libopus).

use anyhow::{Result, bail};
use opus::{Application, Bitrate, Channels, Encoder as OpusEnc};
use snapcast_proto::SampleFormat;

use super::{EncodedChunk, Encoder};
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

        // Build OpusHead identification header
        let mut header = Vec::with_capacity(19);
        header.extend_from_slice(b"OpusHead");
        header.push(1); // version
        header.push(format.channels() as u8);
        header.extend_from_slice(&0u16.to_le_bytes()); // pre-skip
        header.extend_from_slice(&format.rate().to_le_bytes());
        header.extend_from_slice(&0u16.to_le_bytes()); // output gain
        header.push(0); // channel mapping family

        // 20ms frame size
        let frame_size = format.rate() as usize / 50;

        Ok(Self {
            format,
            encoder,
            header,
            frame_size,
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

    fn encode(&mut self, input: &AudioData) -> Result<EncodedChunk> {
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
        let frame_bytes = frame_samples * 2; // 16-bit samples
        let total_frames = pcm.len() / (channels * 2);
        tracing::trace!(
            codec = "opus",
            input_bytes = pcm.len(),
            total_frames,
            "encode"
        );

        let mut output = Vec::new();
        let mut encode_buf = [0u8; 4096];

        for chunk in pcm.chunks(frame_bytes) {
            if chunk.len() < frame_bytes {
                break;
            }
            let samples: Vec<i16> = chunk
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| i16::from_le_bytes([b[0], b[1]]))
                .collect();

            match self.encoder.encode(&samples, &mut encode_buf) {
                Ok(len) => output.extend_from_slice(&encode_buf[..len]),
                Err(e) => {
                    tracing::warn!(codec = "opus", error = %e, "encode failed");
                    bail!("Opus encode failed: {e}");
                }
            }
        }

        Ok(EncodedChunk { data: output })
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
