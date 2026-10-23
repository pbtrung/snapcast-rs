//! Audio encoders — PCM, FLAC, Opus.

#[cfg(feature = "flac")]
pub mod flac;
#[cfg(feature = "opus")]
pub mod opus;
pub mod pcm;

use anyhow::Result;
use snapcast_proto::SampleFormat;

use crate::AudioData;

/// One self-contained codec packet, sent to clients as one `WireChunk`.
///
/// Clients hand each `WireChunk` to their decoder as a single packet, so
/// packets must never be concatenated.
#[derive(Debug)]
pub(crate) struct EncodedPacket {
    /// Encoded audio data.
    pub data: Vec<u8>,
    /// Position of the packet's first sample relative to the first sample of
    /// the input passed to [`Encoder::encode`], in frames. Negative when the
    /// packet starts with samples buffered from earlier input.
    pub offset_frames: i64,
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

    /// Encode an audio chunk. Accepts F32 or Pcm input, by value so that a
    /// passthrough encoder can hand the buffer on without copying it.
    ///
    /// Returns every packet completed by this input, in order: none while
    /// the encoder is still buffering toward a full packet, several when the
    /// input spans more than one.
    fn encode(&mut self, input: AudioData) -> Result<Vec<EncodedPacket>>;

    /// Drop buffered input and codec state before encoding resumes after a
    /// gap, so the first output after the gap holds no audio from before it.
    fn reset(&mut self) {}

    /// Encode the input still buffered toward an incomplete packet, padded
    /// with silence to a whole packet (end of a track or of the source).
    /// `offset_frames` is relative to the end of the input so far, so it is
    /// negative.
    fn flush(&mut self) -> Result<Vec<EncodedPacket>> {
        Ok(Vec::new())
    }
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
}

/// Create an encoder from config.
pub(crate) fn create(config: &EncoderConfig) -> Result<Box<dyn Encoder>> {
    let EncoderConfig { codec, format } = config;
    let format = *format;
    format
        .validate_concrete_pcm()
        .map_err(|e| anyhow::anyhow!("{codec} encoder: {e}"))?;
    #[allow(unused_variables)]
    let (codec, options) = codec.split_once(':').unwrap_or((codec.as_str(), ""));
    match codec {
        snapcast_proto::CODEC_PCM => Ok(Box::new(pcm::PcmEncoder::new(format))),
        #[cfg(feature = "flac")]
        snapcast_proto::CODEC_FLAC => Ok(Box::new(flac::FlacEncoder::new(format, options)?)),
        #[cfg(feature = "opus")]
        snapcast_proto::CODEC_OPUS => Ok(Box::new(opus::OpusEncoder::new(format, options)?)),
        other => anyhow::bail!("unsupported codec: {other} (check enabled features)"),
    }
}

/// Quantize one f32 sample (nominal range -1.0..=1.0, clamped) to an
/// integer sample of `bits` bits (16, 24 or 32), as carried in PCM.
#[inline]
pub(crate) fn quantize(sample: f32, bits: u16) -> i32 {
    let scale = match bits {
        24 => snapcast_proto::PCM_24BIT_MAX,
        32 => i32::MAX as f32,
        _ => i16::MAX as f32,
    };
    // `as` saturates, so 1.0 at 32 bits (2^31 after rounding) stays in range.
    (sample.clamp(-1.0, 1.0) * scale) as i32
}

/// Convert f32 samples to PCM bytes at the given bit depth (16 bits for an
/// unsupported depth). Shared helper for encoders that need integer PCM input.
pub(crate) fn f32_to_pcm(samples: &[f32], bits: u16) -> Vec<u8> {
    if bits == 24 || bits == 32 {
        let mut buf = Vec::with_capacity(samples.len() * 4);
        for &s in samples {
            buf.extend_from_slice(&quantize(s, bits).to_le_bytes());
        }
        buf
    } else {
        let mut buf = Vec::with_capacity(samples.len() * 2);
        for &s in samples {
            buf.extend_from_slice(&(quantize(s, 16) as i16).to_le_bytes());
        }
        buf
    }
}

/// Append PCM bytes at the given bit depth (16 bits for an unsupported one)
/// to `out` as f32 samples, scaled by 2^(bits-1) as `opus_encode()` scales
/// 16-bit input. Of a 24-bit sample only the low 3 bytes count, as in
/// playback. Shared helper for encoders that need f32 input.
#[cfg(any(feature = "opus", test))]
pub(crate) fn extend_f32_from_pcm(out: &mut Vec<f32>, pcm: &[u8], bits: u16) {
    match bits {
        24 => out.extend(
            pcm.as_chunks::<4>()
                .0
                .iter()
                .map(|c| ((i32::from_le_bytes(*c) << 8) >> 8) as f32 / 8_388_608.0),
        ),
        32 => out.extend(
            pcm.as_chunks::<4>()
                .0
                .iter()
                .map(|c| i32::from_le_bytes(*c) as f32 / 2_147_483_648.0),
        ),
        _ => out.extend(
            pcm.as_chunks::<2>()
                .0
                .iter()
                .map(|c| f32::from(i16::from_le_bytes(*c)) / 32768.0),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f32_to_24_bit_pcm_uses_padded_samples() {
        let pcm = f32_to_pcm(&[0.0, 1.0, -1.0], 24);
        assert_eq!(pcm.len(), 12);
        let mut samples = Vec::new();
        extend_f32_from_pcm(&mut samples, &pcm, 24);
        assert_eq!(samples.len(), 3);
    }

    #[test]
    fn pcm_to_f32_ignores_the_24_bit_padding_byte() {
        let mut samples = Vec::new();
        // -1 in 24 bits, with a garbage top byte.
        extend_f32_from_pcm(&mut samples, &0x12FF_FFFFu32.to_le_bytes(), 24);
        assert_eq!(samples, [-1.0 / 8_388_608.0]);
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

    #[test]
    fn create_rejects_non_concrete_formats() {
        for format in [
            SampleFormat::new(0, 16, 2),
            SampleFormat::new(48000, 0, 2),
            SampleFormat::new(48000, 8, 2),
            SampleFormat::new(48000, 16, 0),
        ] {
            let config = EncoderConfig {
                codec: "pcm".into(),
                format,
            };
            let err = create(&config).err().expect("rejected").to_string();
            assert!(err.contains("unsupported sample format"), "{err}");
        }
    }
}
