//! F32 LZ4 decoder — lossless compressed f32 audio.
//!
//! Wire format:
//! - Header: "F32L" + sample_rate(u32) + channels(u16) + bits(u16)
//! - Chunks: LZ4-compressed f32 samples
//!
//! Output: raw f32 bytes (4 bytes per sample, little-endian).
//! The audio_pump converts these to AudioFrame without i16 quantization.

use anyhow::{Result, bail};
use snapcast_proto::SampleFormat;
use snapcast_proto::f32lz4::{F32LZ4_HEADER_LEN, F32LZ4_MAGIC};
use snapcast_proto::message::codec_header::CodecHeader;

use crate::decoder::Decoder;
use crate::stream::SampleEncoding;

/// F32 LZ4 decoder.
pub struct F32Lz4Decoder {
    sample_format: SampleFormat,
}

impl Decoder for F32Lz4Decoder {
    fn set_header(&mut self, header: &CodecHeader) -> Result<SampleFormat> {
        if header.payload.len() < F32LZ4_HEADER_LEN {
            bail!("F32LZ4 header too small");
        }
        if &header.payload[..F32LZ4_MAGIC.len()] != F32LZ4_MAGIC {
            bail!("not an F32LZ4 header");
        }
        let rate = u32::from_le_bytes(header.payload[4..8].try_into().unwrap());
        let channels = u16::from_le_bytes(header.payload[8..10].try_into().unwrap());
        let bits = u16::from_le_bytes(header.payload[10..12].try_into().unwrap());
        self.sample_format = SampleFormat::new(rate, bits, channels);

        tracing::info!(rate, channels, bits, "F32LZ4 decoder initialized");
        Ok(self.sample_format)
    }

    fn decode(&mut self, data: &mut Vec<u8>) -> Result<bool> {
        if data.is_empty() {
            return Ok(false);
        }

        match lz4_flex::decompress_size_prepended(data) {
            Ok(decompressed) => {
                tracing::trace!(
                    compressed = data.len(),
                    decompressed = decompressed.len(),
                    "F32LZ4 decoded"
                );
                *data = decompressed;
                Ok(true)
            }
            Err(e) => {
                tracing::warn!(error = %e, "F32LZ4 decompress failed");
                Ok(false)
            }
        }
    }

    fn output_encoding(&self) -> SampleEncoding {
        SampleEncoding::Float32
    }
}

/// Create an F32Lz4Decoder.
pub fn create() -> F32Lz4Decoder {
    F32Lz4Decoder {
        sample_format: SampleFormat::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let _fmt = SampleFormat::new(48000, 16, 2);

        // Encode
        let f32_samples: Vec<f32> = (0..960).map(|i| (i as f32 / 960.0) * 2.0 - 1.0).collect();
        let f32_bytes: Vec<u8> = f32_samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let compressed = lz4_flex::compress_prepend_size(&f32_bytes);

        // Decode
        let mut dec = create();
        let header = CodecHeader {
            codec: "f32lz4".into(),
            payload: {
                let mut h = Vec::new();
                h.extend_from_slice(F32LZ4_MAGIC);
                h.extend_from_slice(&48000u32.to_le_bytes());
                h.extend_from_slice(&2u16.to_le_bytes());
                h.extend_from_slice(&32u16.to_le_bytes());
                h
            },
        };
        let sf = dec.set_header(&header).unwrap();
        assert_eq!(sf.rate(), 48000);

        let mut data = compressed;
        assert!(dec.decode(&mut data).unwrap());
        assert_eq!(data, f32_bytes);
    }
}
