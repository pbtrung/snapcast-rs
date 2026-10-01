//! Sample rate conversion using rubato.
//!
//! Activated when the server sample rate differs from the player sample rate.

use anyhow::{Result, bail};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler as RubatoResampler};
use snapcast_proto::SampleFormat;

use crate::stream::SampleEncoding;

/// Resampler that converts between sample rates.
///
/// Input arrives in arbitrary frame counts while the FFT resampler consumes
/// fixed-size chunks, so leftover input frames are kept until the next call.
pub struct Resampler {
    resampler: Fft<f32>,
    /// Interleaved f32 input frames not yet consumed by the resampler.
    pending: Vec<f32>,
    in_format: SampleFormat,
    in_encoding: SampleEncoding,
    out_format: SampleFormat,
    channels: usize,
}

impl Resampler {
    /// Create a new resampler. Returns `None` if no resampling is needed.
    pub fn new_if_needed(
        in_format: SampleFormat,
        out_format: SampleFormat,
        in_encoding: SampleEncoding,
        chunk_frames: usize,
    ) -> Result<Option<Self>> {
        if out_format.rate() == 0 || out_format.rate() == in_format.rate() {
            return Ok(None);
        }

        let channels = in_format.channels() as usize;
        if channels == 0 {
            bail!("cannot resample 0 channels");
        }

        let resampler = Fft::new(
            in_format.rate() as usize,
            out_format.rate() as usize,
            chunk_frames,
            channels,
            FixedSync::Input,
        )?;

        Ok(Some(Self {
            resampler,
            pending: Vec::new(),
            in_format,
            in_encoding,
            out_format,
            channels,
        }))
    }

    /// Resample interleaved sample data in-place.
    ///
    /// Output may be shorter or longer than a rate-scaled copy of the input:
    /// frames that don't fill a whole resampler chunk are held back until the
    /// next call.
    pub fn process(&mut self, data: &mut Vec<u8>) -> Result<()> {
        let sample_size = self.in_format.sample_size() as usize;
        if self.in_format.frame_size() == 0 || sample_size == 0 {
            bail!("cannot resample zero-sized frames");
        }

        for sample_bytes in data.chunks_exact(sample_size) {
            let sample = match sample_size {
                2 => {
                    i16::from_le_bytes([sample_bytes[0], sample_bytes[1]]) as f32 / i16::MAX as f32
                }
                4 => {
                    let bytes = [
                        sample_bytes[0],
                        sample_bytes[1],
                        sample_bytes[2],
                        sample_bytes[3],
                    ];
                    if self.in_encoding == SampleEncoding::Float32 {
                        f32::from_le_bytes(bytes)
                    } else if self.in_format.bits() == 24 {
                        i32::from_le_bytes(bytes) as f32 / snapcast_proto::PCM_24BIT_MAX
                    } else {
                        i32::from_le_bytes(bytes) as f32 / i32::MAX as f32
                    }
                }
                _ => 0.0,
            };
            self.pending.push(sample);
        }

        let mut out = Vec::new();
        let mut consumed = 0;
        loop {
            let need = self.resampler.input_frames_next();
            let available = (self.pending.len() - consumed) / self.channels;
            if available < need {
                break;
            }
            let input = InterleavedSlice::new(&self.pending[consumed..], self.channels, need)?;
            let mut output = vec![0.0f32; self.resampler.output_frames_next() * self.channels];
            let out_capacity = output.len() / self.channels;
            let (in_frames, out_frames) = {
                let mut output_adapter =
                    InterleavedSlice::new_mut(&mut output, self.channels, out_capacity)?;
                self.resampler
                    .process_into_buffer(&input, &mut output_adapter, None)?
            };
            consumed += in_frames * self.channels;
            for s in &output[..out_frames * self.channels] {
                out.extend_from_slice(&s.to_le_bytes());
            }
        }
        self.pending.drain(..consumed);

        *data = out;
        Ok(())
    }

    /// Output encoding is always f32.
    pub fn output_encoding(&self) -> SampleEncoding {
        SampleEncoding::Float32
    }

    /// Output format: same channels as input, 32-bit, at the target rate.
    pub fn output_format(&self) -> SampleFormat {
        SampleFormat::new(self.out_format.rate(), 32, self.in_format.channels())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_resampler_when_same_rate() {
        let fmt = SampleFormat::new(48000, 16, 2);
        let r = Resampler::new_if_needed(fmt, fmt, SampleEncoding::PcmInt, 480).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn no_resampler_when_out_rate_zero() {
        let in_fmt = SampleFormat::new(48000, 16, 2);
        let out_fmt = SampleFormat::new(0, 16, 2);
        let r = Resampler::new_if_needed(in_fmt, out_fmt, SampleEncoding::PcmInt, 480).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn creates_resampler_for_different_rates() {
        let in_fmt = SampleFormat::new(44100, 16, 2);
        let out_fmt = SampleFormat::new(48000, 16, 2);
        let r = Resampler::new_if_needed(in_fmt, out_fmt, SampleEncoding::PcmInt, 441).unwrap();
        assert!(r.is_some());
    }

    #[test]
    fn resample_changes_length() {
        let in_fmt = SampleFormat::new(44100, 16, 2);
        let out_fmt = SampleFormat::new(48000, 16, 2);
        let frames = 441; // 10ms at 44100
        let mut r = Resampler::new_if_needed(in_fmt, out_fmt, SampleEncoding::PcmInt, frames)
            .unwrap()
            .unwrap();

        let in_bytes = frames * in_fmt.frame_size() as usize;
        let mut data = vec![0u8; in_bytes];
        // Fill with a simple pattern
        for (i, chunk) in data.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            let sample = ((i as f64 * 0.1).sin() * 10000.0) as i16;
            chunk.copy_from_slice(&sample.to_le_bytes());
        }

        r.process(&mut data).unwrap();

        // Output is non-empty and differs from input length
        // (rubato FFT resampler has latency, first call produces fewer frames)
        assert!(!data.is_empty());
        assert_ne!(data.len(), in_bytes);
    }

    #[test]
    fn buffers_partial_chunks_across_calls() {
        let in_fmt = SampleFormat::new(44100, 16, 2);
        let out_fmt = SampleFormat::new(48000, 16, 2);
        let frames = 441;
        let mut r = Resampler::new_if_needed(in_fmt, out_fmt, SampleEncoding::PcmInt, frames)
            .unwrap()
            .unwrap();
        let frame_bytes = in_fmt.frame_size() as usize;

        // Less than one chunk: held back, nothing emitted.
        let mut data = vec![0u8; 200 * frame_bytes];
        r.process(&mut data).unwrap();
        assert!(data.is_empty());

        // Completing the chunk (plus extra) emits one chunk of output.
        let mut data = vec![0u8; 300 * frame_bytes];
        r.process(&mut data).unwrap();
        assert!(!data.is_empty());
        assert_eq!(r.pending.len(), (500 - frames) * 2);
    }
}
