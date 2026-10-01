//! F32 LZ4 encoder — lossless compressed f32 audio.
//!
//! Skips PCM conversion entirely. The wire format is:
//! - Header: "F32L" magic + sample_rate(u32) + channels(u16) + bits(u16) = 12 bytes
//! - Chunks: LZ4-compressed f32 samples (lz4_flex prepend_size format)

use anyhow::Result;
use snapcast_proto::SampleFormat;
use snapcast_proto::f32lz4::{F32LZ4_HEADER_LEN, F32LZ4_MAGIC};

use super::{EncodedChunk, Encoder};
use crate::AudioData;

/// F32 LZ4 encoder — compresses f32 audio with LZ4.
pub struct F32Lz4Encoder {
    format: SampleFormat,
    header: Vec<u8>,
    warned: bool,
}

impl F32Lz4Encoder {
    /// Create a new F32 LZ4 encoder.
    pub fn new(format: SampleFormat) -> Self {
        tracing::info!(
            rate = format.rate(),
            channels = format.channels(),
            "F32LZ4 encoder initialized"
        );
        let mut header = Vec::with_capacity(F32LZ4_HEADER_LEN);
        header.extend_from_slice(F32LZ4_MAGIC);
        header.extend_from_slice(&format.rate().to_le_bytes());
        header.extend_from_slice(&format.channels().to_le_bytes());
        header.extend_from_slice(&32u16.to_le_bytes()); // bits = 32 (f32)
        Self {
            format,
            header,
            warned: false,
        }
    }
}

impl Encoder for F32Lz4Encoder {
    fn name(&self) -> &str {
        snapcast_proto::CODEC_F32LZ4
    }

    fn header(&self) -> &[u8] {
        &self.header
    }

    fn encode(&mut self, input: &AudioData) -> Result<EncodedChunk> {
        let channels = self.format.channels() as usize;

        // f32lz4 compresses f32 bytes directly
        let f32_bytes: Vec<u8> = match input {
            AudioData::F32(samples) => {
                // Zero conversion — reinterpret f32 as bytes
                samples.iter().flat_map(|s| s.to_le_bytes()).collect()
            }
            AudioData::Pcm(pcm) => {
                // Convert integer PCM → f32 → bytes
                if !self.warned {
                    self.warned = true;
                    tracing::warn!(
                        codec = "f32lz4",
                        bits = self.format.bits(),
                        "PCM input requires conversion to f32 — consider sending F32 directly"
                    );
                }
                let f32_samples = super::pcm_to_f32(pcm, self.format.bits());
                f32_samples.iter().flat_map(|s| s.to_le_bytes()).collect()
            }
        };

        let frames = f32_bytes.len() / (4 * channels);
        tracing::trace!(input_bytes = f32_bytes.len(), frames, "F32LZ4 encoding");
        let data = lz4_flex::compress_prepend_size(&f32_bytes);

        Ok(EncodedChunk { data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_format() {
        let fmt = SampleFormat::new(48000, 16, 2);
        let enc = F32Lz4Encoder::new(fmt);
        assert_eq!(&enc.header()[..4], F32LZ4_MAGIC.as_slice());
        assert_eq!(enc.header().len(), F32LZ4_HEADER_LEN);
    }

    #[test]
    fn encode_compresses_f32() {
        let fmt = SampleFormat::new(48000, 32, 2);
        let mut enc = F32Lz4Encoder::new(fmt);
        let samples = vec![0.0f32; 960 * 2]; // 960 frames, stereo
        let result = enc.encode(&AudioData::F32(samples)).unwrap();
        assert!(!result.data.is_empty());
    }

    #[test]
    fn encode_compresses_pcm() {
        let fmt = SampleFormat::new(48000, 16, 2);
        let mut enc = F32Lz4Encoder::new(fmt);
        let pcm = vec![0u8; 960 * 4]; // 960 frames, 16-bit stereo
        let result = enc.encode(&AudioData::Pcm(pcm)).unwrap();
        assert!(!result.data.is_empty());
    }
}
