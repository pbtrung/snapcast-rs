//! Audio decoder trait and implementations.

pub mod flac;
pub mod opus;

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

/// WAVE `audio_format` of integer PCM.
const WAVE_FORMAT_PCM: u16 = 1;
/// WAVE `audio_format` whose real format is the extension's sub-format.
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

/// Parse a RIFF/WAV header and return the sample format.
///
/// Only the layout passthrough PCM is played in is accepted: integer PCM
/// whose block alignment is the frame size (24-bit samples padded to four
/// bytes, as Snapcast servers send them). Anything else (float, compressed,
/// packed 24-bit) would be played as noise.
fn parse_riff_header(payload: &[u8]) -> Result<SampleFormat> {
    if payload.len() < 44 {
        bail!("PCM header too small ({} bytes)", payload.len());
    }

    // RIFF header: "RIFF" + size + "WAVE"
    if &payload[0..4] != b"RIFF" || &payload[8..12] != b"WAVE" {
        bail!("not a RIFF/WAVE header");
    }

    let u16_at = |b: &[u8], at: usize| u16::from_le_bytes([b[at], b[at + 1]]);
    let u32_at = |b: &[u8], at: usize| u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);

    // Walk the chunks on the remaining slice, so no offset can overflow.
    let mut rest = &payload[12..];
    let mut fmt = None;
    while rest.len() >= 8 {
        let chunk_id = &rest[0..4];
        let chunk_size = u32_at(rest, 4) as usize;
        rest = &rest[8..];
        if chunk_id == b"data" {
            break;
        }
        if chunk_size > rest.len() {
            bail!(
                "RIFF chunk {:?} exceeds the header",
                String::from_utf8_lossy(chunk_id)
            );
        }
        let body = &rest[..chunk_size];
        if chunk_id == b"fmt " {
            if chunk_size < 16 {
                bail!("fmt chunk too small ({chunk_size} bytes)");
            }
            fmt = Some(body);
        }
        // Chunks are padded to an even size.
        rest = &rest[(chunk_size + chunk_size % 2).min(rest.len())..];
    }

    let Some(fmt) = fmt else {
        bail!("sample format not found in RIFF header");
    };
    let mut audio_format = u16_at(fmt, 0);
    let num_channels = u16_at(fmt, 2);
    let sample_rate = u32_at(fmt, 4);
    let block_align = u16_at(fmt, 12);
    let bits_per_sample = u16_at(fmt, 14);
    if audio_format == WAVE_FORMAT_EXTENSIBLE && fmt.len() >= 26 {
        // The sub-format GUID starts with the format code.
        audio_format = u16_at(fmt, 24);
    }
    if audio_format != WAVE_FORMAT_PCM {
        bail!("unsupported WAVE format {audio_format:#06x} (integer PCM only)");
    }

    // Passthrough PCM is framed and converted as-is, so it must be a width
    // and layout the player handles.
    let format = SampleFormat::new(sample_rate, bits_per_sample, num_channels);
    format.validate_concrete_pcm()?;
    if block_align != format.frame_size() {
        bail!(
            "unsupported block alignment {block_align} for {format} (expected {}{})",
            format.frame_size(),
            if bits_per_sample == 24 {
                ": 24-bit samples must be padded to 4 bytes"
            } else {
                ""
            }
        );
    }
    Ok(format)
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
    fn wav_header_with_unsupported_format_fails() {
        let mut h = wav_header_48000_16_2();
        h[34..36].copy_from_slice(&8u16.to_le_bytes()); // 8-bit samples
        assert!(parse_riff_header(&h).is_err());
        let mut h = wav_header_48000_16_2();
        h[22..24].copy_from_slice(&0u16.to_le_bytes()); // no channels
        assert!(parse_riff_header(&h).is_err());
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
        h.extend_from_slice(&352800u32.to_le_bytes());
        h.extend_from_slice(&8u16.to_le_bytes()); // padded to 4 bytes per sample
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

    /// Regression: the format code and block alignment were ignored, so
    /// float or packed 24-bit audio was played as integer PCM.
    #[test]
    fn non_integer_or_packed_layouts_are_rejected() {
        let mut h = wav_header_48000_16_2();
        h[20..22].copy_from_slice(&3u16.to_le_bytes()); // IEEE float
        h[34..36].copy_from_slice(&32u16.to_le_bytes());
        h[32..34].copy_from_slice(&8u16.to_le_bytes());
        assert!(
            parse_riff_header(&h)
                .unwrap_err()
                .to_string()
                .contains("WAVE format")
        );

        let mut h = wav_header_48000_16_2();
        h[34..36].copy_from_slice(&24u16.to_le_bytes());
        h[32..34].copy_from_slice(&6u16.to_le_bytes()); // packed 24-bit
        assert!(
            parse_riff_header(&h)
                .unwrap_err()
                .to_string()
                .contains("padded")
        );

        let mut h = wav_header_48000_16_2();
        h[32..34].copy_from_slice(&3u16.to_le_bytes());
        assert!(parse_riff_header(&h).is_err());
    }

    #[test]
    fn extensible_integer_pcm_is_accepted() {
        let mut h = Vec::new();
        h.extend_from_slice(b"RIFF\0\0\0\0WAVEfmt ");
        h.extend_from_slice(&40u32.to_le_bytes());
        h.extend_from_slice(&WAVE_FORMAT_EXTENSIBLE.to_le_bytes());
        h.extend_from_slice(&2u16.to_le_bytes());
        h.extend_from_slice(&48000u32.to_le_bytes());
        h.extend_from_slice(&192000u32.to_le_bytes());
        h.extend_from_slice(&4u16.to_le_bytes());
        h.extend_from_slice(&16u16.to_le_bytes());
        h.extend_from_slice(&22u16.to_le_bytes()); // cbSize
        h.extend_from_slice(&16u16.to_le_bytes()); // valid bits
        h.extend_from_slice(&3u32.to_le_bytes()); // channel mask
        h.extend_from_slice(&WAVE_FORMAT_PCM.to_le_bytes()); // sub-format
        h.extend_from_slice(&[0; 14]);
        h.extend_from_slice(b"data\0\0\0\0");
        assert_eq!(
            parse_riff_header(&h).unwrap(),
            SampleFormat::new(48000, 16, 2)
        );
    }

    #[test]
    fn odd_sized_chunks_are_padded_and_truncated_chunks_rejected() {
        let tail = wav_header_48000_16_2().split_off(12);
        let mut h = b"RIFF\0\0\0\0WAVEodd \x03\0\0\0abc\0".to_vec();
        h.extend_from_slice(&tail);
        assert_eq!(
            parse_riff_header(&h).unwrap(),
            SampleFormat::new(48000, 16, 2)
        );

        // A fmt chunk declared shorter than the 16 bytes read from it.
        let mut h = wav_header_48000_16_2();
        h[16..20].copy_from_slice(&14u32.to_le_bytes());
        assert!(parse_riff_header(&h).is_err());

        // A chunk size past the end (or near usize::MAX) ends with an error.
        let mut h = b"RIFF\0\0\0\0WAVEjunk".to_vec();
        h.extend_from_slice(&u32::MAX.to_le_bytes());
        h.extend_from_slice(&tail);
        assert!(parse_riff_header(&h).is_err());
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
