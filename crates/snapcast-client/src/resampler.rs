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
    /// One resampler chunk of output, reused across calls.
    output: Vec<f32>,
    in_format: SampleFormat,
    in_encoding: SampleEncoding,
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
            output: Vec::new(),
            in_format,
            in_encoding,
            channels,
        }))
    }

    /// Resample interleaved sample data in-place, into interleaved
    /// little-endian `f32` (`data` keeps its allocation, so a buffer reused
    /// across calls stops allocating).
    ///
    /// Output may be shorter or longer than a rate-scaled copy of the input:
    /// frames that don't fill a whole resampler chunk are held back until the
    /// next call.
    pub fn process(&mut self, data: &mut Vec<u8>) -> Result<()> {
        let sample_size = self.in_format.sample_size() as usize;
        let frame_size = self.in_format.frame_size() as usize;
        if frame_size == 0 || sample_size == 0 {
            bail!("cannot resample zero-sized frames");
        }

        // Whole frames only: a partial one would shift the channels of
        // everything held back after it.
        let whole = data.len() - data.len() % frame_size;
        let (encoding, bits) = (self.in_encoding, self.in_format.bits());
        self.pending.extend(
            data[..whole]
                .chunks_exact(sample_size)
                .map(|b| encoding.sample_to_f32(b, bits)),
        );
        data.clear();

        let mut consumed = 0;
        loop {
            let need = self.resampler.input_frames_next();
            let available = (self.pending.len() - consumed) / self.channels;
            if available < need {
                break;
            }
            let input = InterleavedSlice::new(&self.pending[consumed..], self.channels, need)?;
            let out_capacity = self.resampler.output_frames_next();
            self.output.resize(out_capacity * self.channels, 0.0);
            let (in_frames, out_frames) = {
                let mut output_adapter =
                    InterleavedSlice::new_mut(&mut self.output, self.channels, out_capacity)?;
                self.resampler
                    .process_into_buffer(&input, &mut output_adapter, None)?
            };
            consumed += in_frames * self.channels;
            data.extend(
                self.output[..out_frames * self.channels]
                    .iter()
                    .flat_map(|s| s.to_le_bytes()),
            );
        }
        self.pending.drain(..consumed);
        Ok(())
    }

    /// Input frames to pass to [`process`](Self::process) so that it emits
    /// the next chunk of output (input already held back counts).
    pub fn input_frames_until_output(&self) -> usize {
        self.resampler
            .input_frames_next()
            .saturating_sub(self.pending.len() / self.channels)
    }

    /// Latency of the resampler in output frames.
    pub fn output_delay(&self) -> usize {
        self.resampler.output_delay()
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
        assert_eq!(r.input_frames_until_output(), frames - (500 - frames));
    }

    #[test]
    fn drops_a_trailing_partial_frame() {
        let in_fmt = SampleFormat::new(44100, 16, 2);
        let out_fmt = SampleFormat::new(48000, 16, 2);
        let mut r = Resampler::new_if_needed(in_fmt, out_fmt, SampleEncoding::PcmInt, 441)
            .unwrap()
            .unwrap();
        // 10 frames and one sample of the next.
        let mut data = vec![0u8; 10 * 4 + 2];
        r.process(&mut data).unwrap();
        assert_eq!(r.pending.len(), 10 * 2, "channels stay aligned");
    }
}
