//! Audio encoders — PCM, FLAC, Opus.

#[cfg(feature = "flac")]
pub mod flac;
#[cfg(feature = "opus")]
pub mod opus;
pub mod pcm;

use anyhow::Result;
use snapcast_proto::SampleFormat;

use crate::AudioData;

/// Result of encoding an audio chunk.
pub(crate) struct EncodedChunk {
    /// Encoded audio data.
    pub data: Vec<u8>,
}

/// Trait for audio encoders.
///
/// Each encoder accepts [`AudioData`] (F32 or Pcm) and handles conversion
/// internally. This keeps format-specific logic in the encoder, not the caller.
pub(crate) trait Encoder: Send {
    /// Codec name (e.g. "flac", "pcm", "opus").
    fn name(&self) -> &str;

    /// Codec header bytes sent to clients before audio data.
    fn header(&self) -> &[u8];

    /// Encode an audio chunk. Accepts F32 or Pcm input.
    fn encode(&mut self, input: &AudioData) -> Result<EncodedChunk>;

    /// Drop buffered input and codec state before encoding resumes after a
    /// gap, so the first output after the gap holds no audio from before it.
    fn reset(&mut self) {}
}

/// Configuration for creating an encoder.
#[derive(Debug, Clone)]
pub(crate) struct EncoderConfig {
    /// Codec name: "pcm", "flac", "opus". May carry inline options
    /// after the first `:` (e.g. `"opus:BITRATE:256000,COMPLEXITY:10"`), as
    /// in the C++ snapserver `codec` setting.
    pub codec: String,
    /// Audio sample format.
    pub format: SampleFormat,
    /// Codec-specific options (e.g. FLAC compression level).
    pub options: String,
}

/// Create an encoder from config.
pub(crate) fn create(config: &EncoderConfig) -> Result<Box<dyn Encoder>> {
    #[allow(unused_variables)]
    let EncoderConfig {
        codec,
        format,
        options,
        ..
    } = config;
    let format = *format;
    let (codec, options) = match codec.split_once(':') {
        Some((name, inline)) if options.is_empty() => (name, inline),
        _ => (codec.as_str(), options.as_str()),
    };
    match codec {
        snapcast_proto::CODEC_PCM => Ok(Box::new(pcm::PcmEncoder::new(format))),
        #[cfg(feature = "flac")]
        snapcast_proto::CODEC_FLAC => Ok(Box::new(flac::FlacEncoder::new(format, options)?)),
        #[cfg(feature = "opus")]
        snapcast_proto::CODEC_OPUS => Ok(Box::new(opus::OpusEncoder::new(format, options)?)),
        other => anyhow::bail!("unsupported codec: {other} (check enabled features)"),
    }
}

/// Convert f32 samples to PCM bytes at the given bit depth.
/// Shared helper for encoders that need integer PCM input.
pub(crate) fn f32_to_pcm(samples: &[f32], bits: u16) -> Vec<u8> {
    match bits {
        16 => {
            let mut buf = Vec::with_capacity(samples.len() * 2);
            for &s in samples {
                let i = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                buf.extend_from_slice(&i.to_le_bytes());
            }
            buf
        }
        24 => {
            let mut buf = Vec::with_capacity(samples.len() * 4);
            for &s in samples {
                let i = (s.clamp(-1.0, 1.0) * snapcast_proto::PCM_24BIT_MAX) as i32;
                buf.extend_from_slice(&i.to_le_bytes());
            }
            buf
        }
        32 => {
            let mut buf = Vec::with_capacity(samples.len() * 4);
            for &s in samples {
                let i = (s.clamp(-1.0, 1.0) * i32::MAX as f32) as i32;
                buf.extend_from_slice(&i.to_le_bytes());
            }
            buf
        }
        _ => f32_to_pcm(samples, 16),
    }
}

/// Convert PCM bytes to f32 samples at the given bit depth.
/// Shared helper for encoders that need f32 input.
#[cfg(any(feature = "opus", test))]
pub(crate) fn pcm_to_f32(pcm: &[u8], bits: u16) -> Vec<f32> {
    match bits {
        16 => pcm
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / i16::MAX as f32)
            .collect(),
        24 => pcm
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| {
                i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f32 / snapcast_proto::PCM_24BIT_MAX
            })
            .collect(),
        32 => pcm
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f32 / i32::MAX as f32)
            .collect(),
        _ => pcm_to_f32(pcm, 16),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f32_to_24_bit_pcm_uses_padded_samples() {
        let pcm = f32_to_pcm(&[0.0, 1.0, -1.0], 24);
        assert_eq!(pcm.len(), 12);
        assert_eq!(pcm_to_f32(&pcm, 24).len(), 3);
    }

    #[test]
    fn f32_to_16_bit_pcm_uses_two_byte_samples() {
        let pcm = f32_to_pcm(&[0.0, 1.0], 16);
        assert_eq!(pcm.len(), 4);
    }

    #[test]
    fn create_splits_inline_codec_options() {
        let format = SampleFormat::new(48000, 16, 2);
        let config = |codec: &str| EncoderConfig {
            codec: codec.into(),
            format,
            options: String::new(),
        };
        assert_eq!(create(&config("pcm")).unwrap().name(), "pcm");
        #[cfg(feature = "flac")]
        {
            assert_eq!(create(&config("flac:5")).unwrap().name(), "flac");
            assert!(create(&config("flac:99")).is_err());
        }
        let err = create(&config("nope:X:1")).err().unwrap().to_string();
        assert!(err.contains("unsupported codec: nope"), "{err}");
    }
}
