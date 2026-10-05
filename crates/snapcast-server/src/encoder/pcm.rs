//! PCM encoder — passthrough with WAV header.

use anyhow::Result;
use snapcast_proto::SampleFormat;

use super::{EncodedPacket, Encoder};
use crate::AudioData;

/// PCM passthrough encoder. Header is a 44-byte WAV header.
pub struct PcmEncoder {
    format: SampleFormat,
    header: Vec<u8>,
    warned: bool,
}

impl PcmEncoder {
    /// Create a new PCM encoder for the given sample format.
    pub fn new(format: SampleFormat) -> Self {
        let header = build_wav_header(format);
        Self {
            format,
            header,
            warned: false,
        }
    }
}

impl Encoder for PcmEncoder {
    fn name(&self) -> &str {
        snapcast_proto::CODEC_PCM
    }

    fn header(&self) -> &[u8] {
        &self.header
    }

    fn encode(&mut self, input: &AudioData) -> Result<Vec<EncodedPacket>> {
        let data = match input {
            AudioData::Pcm(pcm) => pcm.clone(),
            AudioData::F32(samples) => {
                if !self.warned {
                    self.warned = true;
                    tracing::warn!(
                        codec = "pcm",
                        bits = self.format.bits(),
                        "F32 input → {}-bit PCM quantization",
                        self.format.bits()
                    );
                }
                super::f32_to_pcm(samples, self.format.bits())
            }
        };
        if data.is_empty() {
            return Ok(Vec::new());
        }
        Ok(vec![EncodedPacket {
            data,
            offset_frames: 0,
        }])
    }
}

fn build_wav_header(fmt: SampleFormat) -> Vec<u8> {
    let mut h = vec![0u8; 44];
    let channels = fmt.channels();
    let rate = fmt.rate();
    let bits = fmt.bits();
    let block_align = channels * fmt.sample_size();
    let byte_rate = rate * block_align as u32;

    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&36u32.to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&channels.to_le_bytes());
    h[24..28].copy_from_slice(&rate.to_le_bytes());
    h[28..32].copy_from_slice(&byte_rate.to_le_bytes());
    h[32..34].copy_from_slice(&block_align.to_le_bytes());
    h[34..36].copy_from_slice(&bits.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&0u32.to_le_bytes());
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_passthrough() {
        let fmt = SampleFormat::new(48000, 16, 2);
        let mut enc = PcmEncoder::new(fmt);
        assert_eq!(enc.name(), "pcm");
        assert_eq!(enc.header().len(), 44);
        assert_eq!(&enc.header()[0..4], b"RIFF");

        let pcm = vec![0u8; 960 * 4];
        let result = enc.encode(&AudioData::Pcm(pcm.clone())).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].data.len(), pcm.len());
        assert_eq!(result[0].offset_frames, 0);
    }

    #[test]
    fn f32_converts_to_pcm() {
        let fmt = SampleFormat::new(48000, 16, 2);
        let mut enc = PcmEncoder::new(fmt);
        let samples = vec![0.0f32; 960];
        let result = enc.encode(&AudioData::F32(samples)).unwrap();
        assert_eq!(result[0].data.len(), 960 * 2); // 16-bit = 2 bytes/sample
    }

    #[test]
    fn wav_header_uses_padded_24_bit_alignment() {
        let fmt = SampleFormat::new(48000, 24, 2);
        let enc = PcmEncoder::new(fmt);
        let block_align = u16::from_le_bytes([enc.header()[32], enc.header()[33]]);
        let byte_rate = u32::from_le_bytes([
            enc.header()[28],
            enc.header()[29],
            enc.header()[30],
            enc.header()[31],
        ]);
        assert_eq!(block_align, 8);
        assert_eq!(byte_rate, 48000 * 8);
    }
}
