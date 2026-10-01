//! Audio encoders — PCM, FLAC, Opus, Vorbis, F32LZ4.

#[cfg(feature = "f32lz4")]
pub mod f32lz4;
#[cfg(feature = "flac")]
pub mod flac;
#[cfg(feature = "opus")]
pub mod opus;
pub mod pcm;
#[cfg(feature = "vorbis")]
pub mod vorbis;

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
    /// Codec name (e.g. "flac", "pcm", "opus", "ogg", "f32lz4").
    fn name(&self) -> &str;

    /// Codec header bytes sent to clients before audio data.
    fn header(&self) -> &[u8];

    /// Encode an audio chunk. Accepts F32 or Pcm input.
    fn encode(&mut self, input: &AudioData) -> Result<EncodedChunk>;
}

/// Configuration for creating an encoder.
#[derive(Debug, Clone)]
pub(crate) struct EncoderConfig {
    /// Codec name: "pcm", "flac", "opus", "ogg", "f32lz4".
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
    match codec.as_str() {
        snapcast_proto::CODEC_PCM => Ok(Box::new(pcm::PcmEncoder::new(format))),
        #[cfg(feature = "flac")]
        snapcast_proto::CODEC_FLAC => Ok(Box::new(flac::FlacEncoder::new(format, options)?)),
        #[cfg(feature = "opus")]
        snapcast_proto::CODEC_OPUS => Ok(Box::new(opus::OpusEncoder::new(format, options)?)),
        #[cfg(feature = "vorbis")]
        snapcast_proto::CODEC_OGG => Ok(Box::new(vorbis::VorbisEncoder::new(format, options)?)),
        #[cfg(feature = "f32lz4")]
        snapcast_proto::CODEC_F32LZ4 => {
            let enc = f32lz4::F32Lz4Encoder::new(format);
            Ok(Box::new(enc))
        }
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
#[cfg(any(feature = "f32lz4", feature = "opus", test))]
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
}
