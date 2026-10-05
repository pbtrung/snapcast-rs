//! Audio sample format description.
//!
//! Matches the C++ `SampleFormat` class. Parses from the string format
//! `"rate:bits:channels"` (e.g. `"48000:16:2"`). A `*` or `0` in any
//! position means "unspecified / same as source".

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// Errors when parsing a [`SampleFormat`] from a string.
#[derive(Debug, Error)]
pub enum SampleFormatError {
    /// The string does not have the expected `rate:bits:channels` format.
    #[error("sample format must be <rate>:<bits>:<channels>, got {0:?}")]
    InvalidFormat(String),
    /// A numeric field could not be parsed.
    #[error("invalid number in sample format: {0}")]
    InvalidNumber(#[from] std::num::ParseIntError),
    /// The format is not concrete PCM that sources, encoders and decoders
    /// can process (see [`SampleFormat::validate_concrete_pcm`]).
    #[error("unsupported sample format {format}: {reason}")]
    Unsupported {
        /// The rejected format.
        format: SampleFormat,
        /// Why it was rejected.
        reason: &'static str,
    },
}

/// Highest sample rate accepted by [`SampleFormat::validate_concrete_pcm`].
pub const MAX_SAMPLE_RATE: u32 = 768_000;
/// Highest channel count accepted by [`SampleFormat::validate_concrete_pcm`].
pub const MAX_CHANNELS: u16 = 256;

/// Audio sample format: rate, bit depth, and channel count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleFormat {
    rate: u32,
    bits: u16,
    channels: u16,
}

impl SampleFormat {
    /// Create a new sample format.
    pub const fn new(rate: u32, bits: u16, channels: u16) -> Self {
        Self {
            rate,
            bits,
            channels,
        }
    }

    /// Sample rate in Hz.
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Bit depth per sample.
    pub fn bits(&self) -> u16 {
        self.bits
    }

    /// Number of audio channels.
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Size in bytes of a single mono sample.
    ///
    /// Note: 24-bit samples are padded to 4 bytes (matching C++ behavior).
    pub fn sample_size(&self) -> u16 {
        if self.bits == 24 { 4 } else { self.bits / 8 }
    }

    /// Size in bytes of one frame (all channels combined).
    pub fn frame_size(&self) -> u16 {
        self.channels * self.sample_size()
    }

    /// Sample rate in frames per millisecond.
    pub fn ms_rate(&self) -> f64 {
        f64::from(self.rate) / 1000.0
    }

    /// Convert a frame count to duration in milliseconds.
    pub fn frames_to_ms(&self, frames: usize) -> f64 {
        frames as f64 * 1000.0 / f64::from(self.rate)
    }

    /// Whether any field has been set (non-zero).
    pub fn is_initialized(&self) -> bool {
        self.rate != 0 || self.bits != 0 || self.channels != 0
    }

    /// Check that this is a concrete PCM format that audio can actually be
    /// read, encoded or played in.
    ///
    /// Parsing accepts wildcards (`*` → 0) and arbitrary widths because a
    /// format string may describe a still-unresolved format; anything that
    /// frames, paces or converts samples must call this first. Requires a
    /// rate of 1..=[`MAX_SAMPLE_RATE`] Hz, 1..=[`MAX_CHANNELS`] channels and
    /// 16-, 24- (padded to 4 bytes) or 32-bit samples, which keeps frame and
    /// byte-rate arithmetic free of overflow.
    pub fn validate_concrete_pcm(&self) -> Result<(), SampleFormatError> {
        let reason = if self.rate == 0 {
            "sample rate must be nonzero"
        } else if self.rate > MAX_SAMPLE_RATE {
            "sample rate is above 768000 Hz"
        } else if !matches!(self.bits, 16 | 24 | 32) {
            "bit depth must be 16, 24 or 32"
        } else if self.channels == 0 {
            "channel count must be nonzero"
        } else if self.channels > MAX_CHANNELS {
            "channel count is above 256"
        } else {
            return Ok(());
        };
        Err(SampleFormatError::Unsupported {
            format: *self,
            reason,
        })
    }
}

impl FromStr for SampleFormat {
    type Err = SampleFormatError;

    /// Parse from `"rate:bits:channels"` string. `*` means 0 (unspecified).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() != 3 {
            return Err(SampleFormatError::InvalidFormat(s.to_string()));
        }
        let parse_u32 = |p: &str| -> Result<u32, SampleFormatError> {
            if p == "*" { Ok(0) } else { Ok(p.parse()?) }
        };
        // Reject out-of-range bits/channels rather than silently wrapping via
        // `as u16` (e.g. "65536" previously became 0).
        let parse_u16 = |p: &str| -> Result<u16, SampleFormatError> {
            if p == "*" {
                Ok(0)
            } else {
                p.parse::<u32>()?
                    .try_into()
                    .map_err(|_| SampleFormatError::InvalidFormat(s.to_string()))
            }
        };
        let rate = parse_u32(parts[0])?;
        let bits = parse_u16(parts[1])?;
        let channels = parse_u16(parts[2])?;
        Ok(Self::new(rate, bits, channels))
    }
}

impl Default for SampleFormat {
    fn default() -> Self {
        Self::new(0, 0, 0)
    }
}

impl fmt::Display for SampleFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.rate, self.bits, self.channels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_concrete_pcm_accepts_supported_formats() {
        for s in ["48000:16:2", "44100:24:2", "96000:32:8", "8000:16:1"] {
            let sf: SampleFormat = s.parse().unwrap();
            assert!(sf.validate_concrete_pcm().is_ok(), "{s}");
        }
    }

    #[test]
    fn validate_concrete_pcm_rejects_wildcards_and_unsupported_widths() {
        for s in [
            "*:16:2",
            "0:16:2",
            "48000:*:2",
            "48000:1:2",
            "48000:8:2",
            "48000:20:2",
            "48000:16:0",
            "48000:16:*",
            "48000:16:257",
            "800000:16:2",
        ] {
            let sf: SampleFormat = s.parse().unwrap();
            let err = sf.validate_concrete_pcm().unwrap_err();
            assert!(
                err.to_string().starts_with("unsupported sample format"),
                "{s}: {err}"
            );
        }
    }

    #[test]
    fn parse_standard_format() {
        let sf: SampleFormat = "48000:16:2".parse().unwrap();
        assert_eq!(sf.rate(), 48000);
        assert_eq!(sf.bits(), 16);
        assert_eq!(sf.channels(), 2);
    }

    #[test]
    fn parse_wildcard_channels() {
        let sf: SampleFormat = "44100:16:*".parse().unwrap();
        assert_eq!(sf.rate(), 44100);
        assert_eq!(sf.bits(), 16);
        assert_eq!(sf.channels(), 0);
    }

    #[test]
    fn parse_all_wildcards() {
        let sf: SampleFormat = "*:*:*".parse().unwrap();
        assert_eq!(sf, SampleFormat::default());
        assert!(!sf.is_initialized());
    }

    #[test]
    fn parse_invalid_format() {
        assert!("48000:16".parse::<SampleFormat>().is_err());
        assert!("48000:16:2:1".parse::<SampleFormat>().is_err());
        assert!("".parse::<SampleFormat>().is_err());
    }

    #[test]
    fn parse_rejects_out_of_range_fields() {
        // bits/channels above u16::MAX must error, not silently wrap via `as u16`.
        assert!("48000:65536:2".parse::<SampleFormat>().is_err());
        assert!("48000:16:70000".parse::<SampleFormat>().is_err());
        // Realistic values still parse.
        assert!("48000:16:2".parse::<SampleFormat>().is_ok());
        assert_eq!("48000:24:2".parse::<SampleFormat>().unwrap().bits(), 24);
    }

    #[test]
    fn sample_size_16bit() {
        let sf = SampleFormat::new(48000, 16, 2);
        assert_eq!(sf.sample_size(), 2);
    }

    #[test]
    fn sample_size_24bit_padded_to_4() {
        // C++ behavior: 24-bit samples are padded to 4 bytes
        let sf = SampleFormat::new(48000, 24, 2);
        assert_eq!(sf.sample_size(), 4);
    }

    #[test]
    fn sample_size_32bit() {
        let sf = SampleFormat::new(48000, 32, 2);
        assert_eq!(sf.sample_size(), 4);
    }

    #[test]
    fn frame_size_stereo_16bit() {
        // 2 channels * 2 bytes = 4 bytes per frame
        let sf = SampleFormat::new(48000, 16, 2);
        assert_eq!(sf.frame_size(), 4);
    }

    #[test]
    fn frame_size_stereo_24bit() {
        // 2 channels * 4 bytes (padded) = 8 bytes per frame
        let sf = SampleFormat::new(48000, 24, 2);
        assert_eq!(sf.frame_size(), 8);
    }

    #[test]
    fn ms_rate() {
        let sf = SampleFormat::new(48000, 16, 2);
        assert!((sf.ms_rate() - 48.0).abs() < f64::EPSILON);
    }

    #[test]
    fn display_format() {
        let sf = SampleFormat::new(48000, 16, 2);
        assert_eq!(sf.to_string(), "48000:16:2");
    }

    #[test]
    fn round_trip_string() {
        let original = SampleFormat::new(44100, 24, 6);
        let s = original.to_string();
        let parsed: SampleFormat = s.parse().unwrap();
        assert_eq!(original, parsed);
    }
}
