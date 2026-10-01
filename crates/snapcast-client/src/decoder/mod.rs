//! Audio decoder trait and implementations.

pub mod flac;
pub mod opus;
pub mod vorbis;

use anyhow::{Result, bail};
use snapcast_proto::SampleFormat;
use snapcast_proto::message::codec_header::CodecHeader;

use crate::stream::SampleEncoding;

/// Audio decoder trait — matches the C++ `Decoder` interface.
pub trait Decoder: Send {
    /// Initialize the decoder from a codec header. Returns the sample format.
    fn set_header(&mut self, header: &CodecHeader) -> Result<SampleFormat>;

    /// Decode audio data in-place. Returns true if successful.
    /// For PCM this is a no-op (passthrough).
    fn decode(&mut self, data: &mut Vec<u8>) -> Result<bool>;

    /// Encoding produced by [`decode`](Self::decode).
    fn output_encoding(&self) -> SampleEncoding {
        SampleEncoding::PcmInt
    }
}

/// PCM decoder — passthrough. Parses the RIFF/WAV header to extract sample format.
#[derive(Default)]
pub struct PcmDecoder;

impl PcmDecoder {
    /// Create a new PCM passthrough decoder.
    pub fn new() -> Self {
        Self
    }
}

/// Parse a RIFF/WAV header and return the sample format.
fn parse_riff_header(payload: &[u8]) -> Result<SampleFormat> {
    if payload.len() < 44 {
        bail!("PCM header too small ({} bytes)", payload.len());
    }

    // RIFF header: "RIFF" + size + "WAVE"
    if &payload[0..4] != b"RIFF" || &payload[8..12] != b"WAVE" {
        bail!("not a RIFF/WAVE header");
    }

    let mut pos = 12;
    let mut sample_rate: u32 = 0;
    let mut bits_per_sample: u16 = 0;
    let mut num_channels: u16 = 0;

    // Walk chunks until we find "fmt " and "data"
    while pos + 8 <= payload.len() {
        let chunk_id = &payload[pos..pos + 4];
        let chunk_size = u32::from_le_bytes(payload[pos + 4..pos + 8].try_into().unwrap()) as usize;
        pos += 8;

        if chunk_id == b"fmt " {
            if pos + 16 > payload.len() {
                bail!("fmt chunk too small");
            }
            // audio_format: u16 at +0 (skip)
            num_channels = u16::from_le_bytes(payload[pos + 2..pos + 4].try_into().unwrap());
            sample_rate = u32::from_le_bytes(payload[pos + 4..pos + 8].try_into().unwrap());
            // byte_rate: u32 at +8 (skip)
            // block_align: u16 at +12 (skip)
            bits_per_sample = u16::from_le_bytes(payload[pos + 14..pos + 16].try_into().unwrap());
            pos += chunk_size;
        } else if chunk_id == b"data" {
            break;
        } else {
            pos += chunk_size;
        }
    }

    if sample_rate == 0 {
        bail!("sample format not found in RIFF header");
    }

    Ok(SampleFormat::new(
        sample_rate,
        bits_per_sample,
        num_channels,
    ))
}

impl Decoder for PcmDecoder {
    fn set_header(&mut self, header: &CodecHeader) -> Result<SampleFormat> {
        tracing::trace!(
            codec = "pcm",
            payload_len = header.payload.len(),
            "set_header"
        );
        parse_riff_header(&header.payload)
    }

    fn decode(&mut self, _data: &mut Vec<u8>) -> Result<bool> {
        // PCM is passthrough — no decoding needed
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal valid 44-byte WAV header for 48000:16:2
    fn wav_header_48000_16_2() -> Vec<u8> {
        let mut h = Vec::new();
        h.extend_from_slice(b"RIFF");
        h.extend_from_slice(&0u32.to_le_bytes()); // file size (don't care)
        h.extend_from_slice(b"WAVE");
        // fmt chunk
        h.extend_from_slice(b"fmt ");
        h.extend_from_slice(&16u32.to_le_bytes()); // chunk size
        h.extend_from_slice(&1u16.to_le_bytes()); // PCM format
        h.extend_from_slice(&2u16.to_le_bytes()); // channels
        h.extend_from_slice(&48000u32.to_le_bytes()); // sample rate
        h.extend_from_slice(&192000u32.to_le_bytes()); // byte rate
        h.extend_from_slice(&4u16.to_le_bytes()); // block align
        h.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        // data chunk
        h.extend_from_slice(b"data");
        h.extend_from_slice(&0u32.to_le_bytes()); // data size
        h
    }

    #[test]
    fn parse_wav_header() {
        let header = CodecHeader {
            codec: "pcm".into(),
            payload: wav_header_48000_16_2(),
        };
        let mut dec = PcmDecoder::new();
        let sf = dec.set_header(&header).unwrap();
        assert_eq!(sf.rate(), 48000);
        assert_eq!(sf.bits(), 16);
        assert_eq!(sf.channels(), 2);
    }

    #[test]
    fn parse_wav_header_44100_24_2() {
        let mut h = Vec::new();
        h.extend_from_slice(b"RIFF");
        h.extend_from_slice(&0u32.to_le_bytes());
        h.extend_from_slice(b"WAVE");
        h.extend_from_slice(b"fmt ");
        h.extend_from_slice(&16u32.to_le_bytes());
        h.extend_from_slice(&1u16.to_le_bytes());
        h.extend_from_slice(&2u16.to_le_bytes());
        h.extend_from_slice(&44100u32.to_le_bytes());
        h.extend_from_slice(&264600u32.to_le_bytes());
        h.extend_from_slice(&6u16.to_le_bytes());
        h.extend_from_slice(&24u16.to_le_bytes());
        h.extend_from_slice(b"data");
        h.extend_from_slice(&0u32.to_le_bytes());

        let header = CodecHeader {
            codec: "pcm".into(),
            payload: h,
        };
        let mut dec = PcmDecoder::new();
        let sf = dec.set_header(&header).unwrap();
        assert_eq!(sf.rate(), 44100);
        assert_eq!(sf.bits(), 24);
        assert_eq!(sf.channels(), 2);
    }

    #[test]
    fn too_small_header_fails() {
        let header = CodecHeader {
            codec: "pcm".into(),
            payload: vec![0; 10],
        };
        let mut dec = PcmDecoder::new();
        assert!(dec.set_header(&header).is_err());
    }

    #[test]
    fn not_riff_fails() {
        let mut h = vec![0u8; 44];
        h[0..4].copy_from_slice(b"NOPE");
        let header = CodecHeader {
            codec: "pcm".into(),
            payload: h,
        };
        let mut dec = PcmDecoder::new();
        assert!(dec.set_header(&header).is_err());
    }

    #[test]
    fn decode_is_passthrough() {
        let mut dec = PcmDecoder::new();
        let mut data = vec![0xAA, 0xBB, 0xCC];
        assert!(dec.decode(&mut data).unwrap());
        assert_eq!(data, vec![0xAA, 0xBB, 0xCC]);
    }
}
