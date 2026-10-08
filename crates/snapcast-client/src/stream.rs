//! Time-synchronized PCM audio stream buffer.

use std::collections::VecDeque;

use snapcast_proto::SampleFormat;
use snapcast_proto::types::Timeval;

use crate::double_buffer::DoubleBuffer;

/// In-memory representation of samples stored in a [`PcmChunk`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleEncoding {
    /// Signed little-endian integer PCM.
    PcmInt,
    /// Little-endian IEEE-754 `f32` samples.
    Float32,
}

/// A decoded PCM chunk with a server-time timestamp and a read cursor.
#[derive(Debug, Clone)]
pub struct PcmChunk {
    /// Server-time timestamp of this chunk.
    pub timestamp: Timeval,
    /// Raw PCM sample data.
    pub data: Vec<u8>,
    /// Sample format (rate, bits, channels).
    pub format: SampleFormat,
    /// Encoding of the sample bytes.
    pub encoding: SampleEncoding,
    read_pos: usize,
}

impl PcmChunk {
    /// Create a new PCM chunk.
    pub fn new(timestamp: Timeval, data: Vec<u8>, format: SampleFormat) -> Self {
        Self::new_with_encoding(timestamp, data, format, SampleEncoding::PcmInt)
    }

    /// Create a new chunk with an explicit sample encoding.
    pub fn new_with_encoding(
        timestamp: Timeval,
        data: Vec<u8>,
        format: SampleFormat,
        encoding: SampleEncoding,
    ) -> Self {
        Self {
            timestamp,
            data,
            format,
            encoding,
            read_pos: 0,
        }
    }

    /// Server time of the next unread frame in microseconds: the chunk's
    /// timestamp advanced by the frames already read or skipped (as C++
    /// `PcmChunk::start()`).
    pub fn start_usec(&self) -> i64 {
        let frame_size = self.format.frame_size() as i64;
        let rate = self.format.rate() as i64;
        if frame_size == 0 || rate == 0 {
            return self.timestamp.to_usec();
        }
        let consumed_frames = self.read_pos as i64 / frame_size;
        self.timestamp.to_usec() + consumed_frames * 1_000_000 / rate
    }

    /// Server time just past the last frame of this chunk in microseconds.
    fn end_usec(&self) -> i64 {
        self.timestamp.to_usec() + self.duration_usec()
    }

    /// Duration of this chunk in microseconds.
    pub fn duration_usec(&self) -> i64 {
        if self.format.frame_size() == 0 || self.format.rate() == 0 {
            return 0;
        }
        let frames = self.data.len() as i64 / self.format.frame_size() as i64;
        frames * 1_000_000 / self.format.rate() as i64
    }

    /// Read up to `frames` frames into `output`, returning the number read.
    pub fn read_frames(&mut self, output: &mut [u8], frames: u32) -> u32 {
        let frame_size = self.format.frame_size() as usize;
        if frame_size == 0 {
            return 0;
        }
        let available_bytes = self.data.len() - self.read_pos;
        let available_frames = available_bytes / frame_size;
        // Also bound by the output capacity: a chunk whose format differs from
        // the caller's frame size must never write past `output`. In the normal
        // matched-format path (`output` sized for `frames`) this cap is a no-op.
        let to_read = (frames as usize)
            .min(available_frames)
            .min(output.len() / frame_size);
        let bytes = to_read * frame_size;
        output[..bytes].copy_from_slice(&self.data[self.read_pos..self.read_pos + bytes]);
        self.read_pos += bytes;
        to_read as u32
    }

    /// Returns true if no whole frame is left to read.
    ///
    /// A trailing partial frame (payload length not a multiple of the frame
    /// size) counts as the end, so readers never wait on bytes that
    /// [`read_frames`](Self::read_frames) cannot return.
    pub fn is_end(&self) -> bool {
        let frame_size = self.format.frame_size() as usize;
        frame_size == 0 || self.data.len() - self.read_pos < frame_size
    }

    /// Skip forward by `frames` frames.
    pub fn seek(&mut self, frames: u32) {
        let bytes = frames as usize * self.format.frame_size() as usize;
        self.read_pos = (self.read_pos + bytes).min(self.data.len());
    }
}

/// Correction threshold — soft sync starts when |short_median| > 100µs
const CORRECTION_BEGIN_USEC: i64 = 100;
/// Hard sync: |median| exceeds this (µs).
const HARD_SYNC_MEDIAN_USEC: i64 = 2000;
/// Hard sync: |short_median| exceeds this (µs).
const HARD_SYNC_SHORT_MEDIAN_USEC: i64 = 5000;
/// Hard sync: |mini_median| exceeds this (µs).
const HARD_SYNC_MINI_MEDIAN_USEC: i64 = 50000;
/// Hard sync: |age| exceeds this (µs).
const HARD_SYNC_AGE_USEC: i64 = 500_000;
/// Minimum |age| for hard sync re-trigger (µs).
const HARD_SYNC_MIN_AGE_USEC: i64 = 500;
/// Minimum |mini_median| for soft sync (µs).
const SOFT_SYNC_MIN_USEC: i64 = 50;
/// Maximum playback rate correction factor.
const MAX_RATE_CORRECTION: f64 = 0.0005;
/// Rate correction scaling factor.
const RATE_CORRECTION_SCALE: f64 = 0.00005;
/// DoubleBuffer capacity for mini (fast) drift detection.
const MINI_BUFFER_SIZE: usize = 20;
/// DoubleBuffer capacity for short-term drift detection.
const SHORT_BUFFER_SIZE: usize = 100;
/// DoubleBuffer capacity for long-term drift detection.
const BUFFER_SIZE: usize = 500;
/// Default buffer in milliseconds.
const DEFAULT_BUFFER_MS: i64 = 1000;

/// Time-synchronized PCM stream buffer.
///
/// The `Stream` is responsible for buffering decoded PCM chunks and delivering them to the
/// audio player in a way that remains synchronized with the server's time. It implements
/// the same synchronization strategy as the original C++ Snapcast client.
///
/// ### Synchronization Strategy
///
/// There are two main modes of synchronization:
///
/// 1. **Hard Sync**: Used at startup and whenever the drift is too large to
///    correct smoothly (long-term median > 2ms, short-term median > 5ms, mini
///    median > 50ms, or a single age > 500ms). The stream skips forward in the
///    buffer or inserts silence to reach the desired target time exactly.
/// 2. **Soft Sync**: Used for fine-tuning when the short-term median drift
///    exceeds 100µs. Instead of jumping, the stream adjusts the playback rate
///    (by at most 0.05%) by adding or removing single frames at regular
///    intervals. This is inaudible to most listeners.
///
/// ### Drift Detection
///
/// Synchronization is based on "age", which is the difference between when a sample
/// *should* have been played (server time) and when it *is* being played (now).
///
/// The stream maintains three `DoubleBuffer` instances to track drift over different timescales:
/// - **Mini Buffer** (20 samples): Fast reaction to sudden network or system jitter.
/// - **Short Buffer** (100 samples): Used for calculating soft sync rate corrections.
/// - **Long Buffer** (500 samples): Long-term stability tracking and hard sync re-triggering.
///
/// The median value of these buffers is used to filter out outliers and ensure stable
/// synchronization even in unstable network conditions.
pub struct Stream {
    /// Nominal format of the incoming PCM data.
    format: SampleFormat,
    /// Encoding of samples stored in chunks.
    encoding: SampleEncoding,
    /// Queue of pending PCM chunks.
    chunks: VecDeque<PcmChunk>,
    /// The chunk currently being read from.
    current: Option<PcmChunk>,
    /// Target buffer size in milliseconds.
    buffer_ms: i64,
    /// Whether we are currently in hard sync mode.
    hard_sync: bool,

    // Drift detection buffers
    mini_buffer: DoubleBuffer,
    short_buffer: DoubleBuffer,
    buffer: DoubleBuffer,
    /// Long-term median drift in microseconds.
    median: i64,
    /// Short-term median drift in microseconds.
    short_median: i64,

    // Soft sync (rate correction) state
    /// Number of frames played at the current (corrected) rate.
    played_frames: u32,
    /// How many frames to play before adding/removing a single sample (0 if no correction).
    correct_after_x_frames: i32,
    /// Cumulative difference in frames caused by rate correction (for logging).
    frame_delta: i32,
    /// Internal buffer used for sample insertion/removal.
    read_buf: Vec<u8>,

    /// Last time (in server seconds) that stats were logged.
    last_log_sec: i64,
}

impl Stream {
    /// Create a new stream for the given sample format.
    pub fn new(format: SampleFormat) -> Self {
        Self::with_encoding(format, SampleEncoding::PcmInt)
    }

    /// Create a new stream for the given sample format and sample encoding.
    pub fn with_encoding(format: SampleFormat, encoding: SampleEncoding) -> Self {
        Self {
            format,
            encoding,
            chunks: VecDeque::new(),
            current: None,
            buffer_ms: DEFAULT_BUFFER_MS,
            hard_sync: true,
            mini_buffer: DoubleBuffer::new(MINI_BUFFER_SIZE),
            short_buffer: DoubleBuffer::new(SHORT_BUFFER_SIZE),
            buffer: DoubleBuffer::new(BUFFER_SIZE),
            median: 0,
            short_median: 0,
            played_frames: 0,
            correct_after_x_frames: 0,
            frame_delta: 0,
            read_buf: Vec::new(),
            last_log_sec: 0,
        }
    }

    /// Returns the sample format.
    pub fn format(&self) -> SampleFormat {
        self.format
    }

    /// Returns the sample byte encoding.
    pub fn encoding(&self) -> SampleEncoding {
        self.encoding
    }

    /// Set the target buffer size in milliseconds.
    pub fn set_buffer_ms(&mut self, ms: i64) {
        self.buffer_ms = ms;
    }

    #[cfg(test)]
    pub(crate) fn buffer_ms(&self) -> i64 {
        self.buffer_ms
    }

    /// Enqueue a decoded PCM chunk.
    pub fn add_chunk(&mut self, chunk: PcmChunk) {
        self.chunks.push_back(chunk);
    }

    /// Number of queued chunks.
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// Clear all queued chunks and reset sync state.
    pub fn clear(&mut self) {
        self.chunks.clear();
        self.current = None;
        self.hard_sync = true;
    }

    fn reset_buffers(&mut self) {
        self.buffer.clear();
        self.mini_buffer.clear();
        self.short_buffer.clear();
    }

    fn update_buffers(&mut self, age: i64) {
        self.buffer.add(age);
        self.mini_buffer.add(age);
        self.short_buffer.add(age);
    }

    fn set_real_sample_rate(&mut self, sample_rate: f64) {
        let nominal = self.format.rate() as f64;
        if (sample_rate - nominal).abs() < f64::EPSILON {
            self.correct_after_x_frames = 0;
        } else {
            let ratio = nominal / sample_rate;
            self.correct_after_x_frames = (ratio / (ratio - 1.0)).round() as i32;
        }
    }

    /// Fill `output` with time-synchronized PCM data. Returns false if no data available.
    pub fn get_player_chunk(
        &mut self,
        server_now_usec: i64,
        output_buffer_dac_time_usec: i64,
        output: &mut [u8],
        frames: u32,
    ) -> bool {
        let needs_new = self.current.as_ref().is_none_or(|c| c.is_end());
        if needs_new {
            self.current = self.chunks.pop_front();
        }
        if self.current.is_none() {
            return false;
        }

        // Server time whose sample should reach the DAC with this buffer;
        // the age of a frame is how far past its playout time it is.
        let playout_usec = server_now_usec - self.buffer_ms * 1000 + output_buffer_dac_time_usec;

        // --- Hard sync: initial alignment ---
        if self.hard_sync {
            let chunk = self.current.as_ref().unwrap();
            let req_duration_usec = (frames as i64 * 1_000_000) / self.format.rate() as i64;
            let age_usec = playout_usec - chunk.start_usec();

            if age_usec < -req_duration_usec {
                self.get_silence(output, frames);
                return true;
            }

            if age_usec > 0 {
                // Too old: drop frames until the playout point. The current
                // chunk is a candidate too (it may only be partly stale).
                if let Some(c) = self.current.take() {
                    self.chunks.push_front(c);
                }
                while let Some(mut c) = self.chunks.pop_front() {
                    let a = playout_usec - c.start_usec();
                    if a <= 0 {
                        self.current = Some(c);
                        break;
                    }
                    if a < c.end_usec() - c.start_usec() {
                        let skip = (self.format.rate() as f64 * a as f64 / 1_000_000.0) as u32;
                        c.seek(skip);
                        self.current = Some(c);
                        break;
                    }
                }
                if self.current.is_none() {
                    return false;
                }
            }

            let chunk = self.current.as_ref().unwrap();
            let age_usec = playout_usec - chunk.start_usec();

            if age_usec <= 0 {
                let silent_frames =
                    (self.format.rate() as f64 * (-age_usec) as f64 / 1_000_000.0) as u32;
                let silent_frames = silent_frames.min(frames);
                let frame_size = self.format.frame_size() as usize;

                if silent_frames > 0 {
                    output[..silent_frames as usize * frame_size].fill(0);
                }
                let remaining = frames - silent_frames;
                if remaining > 0 {
                    let offset = silent_frames as usize * frame_size;
                    self.read_next(&mut output[offset..], remaining);
                }
                if silent_frames < frames {
                    self.hard_sync = false;
                    self.reset_buffers();
                }
                return true;
            }
            return false;
        }

        // --- Normal playback with drift correction ---

        // Compute frames correction from current rate adjustment
        let mut frames_correction: i32 = 0;
        if self.correct_after_x_frames != 0 {
            self.played_frames += frames;
            if self.played_frames >= self.correct_after_x_frames.unsigned_abs() {
                frames_correction = self.played_frames as i32 / self.correct_after_x_frames;
                self.played_frames %= self.correct_after_x_frames.unsigned_abs();
            }
        }

        // Read with correction (or plain read if correction == 0)
        let chunk_start = match self.read_with_correction(output, frames, frames_correction) {
            Some(ts) => ts,
            None => return false,
        };

        let age_usec = playout_usec - chunk_start;

        // Reset sample rate to nominal, soft sync may override below
        self.set_real_sample_rate(self.format.rate() as f64);

        // Hard sync re-trigger thresholds (matching C++)
        if self.buffer.full()
            && self.median.abs() > HARD_SYNC_MEDIAN_USEC
            && age_usec.abs() > HARD_SYNC_MIN_AGE_USEC
        {
            tracing::info!(
                median = self.median,
                "Hard sync: buffer full, |median| > 2ms"
            );
            self.hard_sync = true;
        } else if self.short_buffer.full()
            && self.short_median.abs() > HARD_SYNC_SHORT_MEDIAN_USEC
            && age_usec.abs() > HARD_SYNC_MIN_AGE_USEC
        {
            tracing::info!(
                short_median = self.short_median,
                "Hard sync: short buffer full, |short_median| > 5ms"
            );
            self.hard_sync = true;
        } else if self.mini_buffer.full()
            && self.mini_buffer.median_simple().abs() > HARD_SYNC_MINI_MEDIAN_USEC
            && age_usec.abs() > HARD_SYNC_MIN_AGE_USEC
        {
            tracing::info!(
                age_usec,
                mini_median = self.mini_buffer.median_simple(),
                "Hard sync: mini buffer full, |mini_median| > 50ms"
            );
            self.hard_sync = true;
        } else if age_usec.abs() > HARD_SYNC_AGE_USEC {
            tracing::info!(age_usec, "Hard sync: |age| > 500ms");
            self.hard_sync = true;
        } else if self.short_buffer.full() {
            // Soft sync: adjust playback speed based on drift
            let mini_median = self.mini_buffer.median_simple();
            if self.short_median > CORRECTION_BEGIN_USEC
                && mini_median > SOFT_SYNC_MIN_USEC
                && age_usec > SOFT_SYNC_MIN_USEC
            {
                let rate = (self.short_median as f64 / 100.0) * RATE_CORRECTION_SCALE;
                let rate = 1.0 - rate.min(MAX_RATE_CORRECTION);
                self.set_real_sample_rate(self.format.rate() as f64 * rate);
            } else if self.short_median < -CORRECTION_BEGIN_USEC
                && mini_median < -SOFT_SYNC_MIN_USEC
                && age_usec < -SOFT_SYNC_MIN_USEC
            {
                let rate = (-self.short_median as f64 / 100.0) * RATE_CORRECTION_SCALE;
                let rate = 1.0 + rate.min(MAX_RATE_CORRECTION);
                self.set_real_sample_rate(self.format.rate() as f64 * rate);
            }
        }

        self.update_buffers(age_usec);

        // Stats logging (once per second)
        let now_sec = server_now_usec / 1_000_000;
        if now_sec != self.last_log_sec {
            self.last_log_sec = now_sec;
            self.median = self.buffer.median_simple();
            self.short_median = self.short_buffer.median_simple();
            tracing::debug!(
                target: "Stats",
                "Chunk: {}\t{}\t{}\t{}\t{}\t{}\t{}",
                age_usec,
                self.mini_buffer.median_simple(),
                self.short_median,
                self.median,
                self.buffer.len(),
                output_buffer_dac_time_usec / 1000,
                self.frame_delta,
            );
            self.frame_delta = 0;
        }

        age_usec.abs() < 500_000
    }

    /// Fill `output` with silence.
    pub fn get_silence(&self, output: &mut [u8], frames: u32) {
        let bytes = frames as usize * self.format.frame_size() as usize;
        let len = bytes.min(output.len());
        output[..len].fill(0);
    }

    /// Like [`get_player_chunk`](Self::get_player_chunk), but fills silence on failure.
    pub fn get_player_chunk_or_silence(
        &mut self,
        server_now_usec: i64,
        output_buffer_dac_time_usec: i64,
        output: &mut [u8],
        frames: u32,
    ) -> bool {
        let result =
            self.get_player_chunk(server_now_usec, output_buffer_dac_time_usec, output, frames);
        if !result {
            self.get_silence(output, frames);
        }
        result
    }

    /// Read `frames` frames, continuing into queued chunks as needed. Returns
    /// the server time of the first frame read. On underrun the unfilled tail
    /// of `output` is zeroed so stale buffer contents are never played.
    fn read_next(&mut self, output: &mut [u8], frames: u32) -> Option<i64> {
        let chunk = self.current.as_mut()?;
        let frame_size = self.format.frame_size() as usize;
        let ts = chunk.start_usec();
        let mut read = 0u32;
        while read < frames {
            let offset = read as usize * frame_size;
            let n = chunk.read_frames(&mut output[offset..], frames - read);
            read += n;
            if read < frames && chunk.is_end() {
                match self.chunks.pop_front() {
                    Some(next) => *chunk = next,
                    None => break,
                }
            } else if n == 0 {
                // `output` is full (shorter than `frames`): nothing more fits.
                break;
            }
        }
        let filled = (read as usize * frame_size).min(output.len());
        let wanted = (frames as usize * frame_size).min(output.len());
        if filled < wanted {
            output[filled..wanted].fill(0);
        }
        Some(ts)
    }

    fn read_with_correction(
        &mut self,
        output: &mut [u8],
        frames: u32,
        correction: i32,
    ) -> Option<i64> {
        if correction == 0 {
            return self.read_next(output, frames);
        }

        // Clamp correction to avoid underflow
        let correction = correction.max(-(frames as i32) + 1);

        self.frame_delta -= correction;
        let to_read = (frames as i32 + correction) as u32;
        let frame_size = self.format.frame_size() as usize;

        self.read_buf.resize(to_read as usize * frame_size, 0);
        let mut read_buf = std::mem::take(&mut self.read_buf);
        let ts = self.read_next(&mut read_buf, to_read);

        let max = if correction < 0 {
            frames as usize
        } else {
            to_read as usize
        };
        let slices = (correction.unsigned_abs() as usize + 1).min(max);
        let slice_size = max / slices;

        let mut pos = 0usize;
        for n in 0..slices {
            let size = if n + 1 == slices {
                max - pos
            } else {
                slice_size
            };

            if correction < 0 {
                let src_start = (pos - n) * frame_size;
                let dst_start = pos * frame_size;
                let len = size * frame_size;
                output[dst_start..dst_start + len]
                    .copy_from_slice(&read_buf[src_start..src_start + len]);
            } else {
                let src_start = pos * frame_size;
                let dst_start = (pos - n) * frame_size;
                let len = size * frame_size;
                output[dst_start..dst_start + len]
                    .copy_from_slice(&read_buf[src_start..src_start + len]);
            }
            pos += size;
        }

        self.read_buf = read_buf;
        ts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt() -> SampleFormat {
        SampleFormat::new(48000, 16, 2)
    }

    fn make_chunk(sec: i32, usec: i32, frames: u32, format: SampleFormat) -> PcmChunk {
        let bytes = frames as usize * format.frame_size() as usize;
        let data: Vec<u8> = (0..bytes).map(|i| (i % 256) as u8).collect();
        PcmChunk::new(Timeval { sec, usec }, data, format)
    }

    #[test]
    fn pcm_chunk_duration() {
        let f = fmt();
        let chunk = make_chunk(0, 0, 480, f);
        assert_eq!(chunk.duration_usec(), 10_000);
    }

    #[test]
    fn pcm_chunk_read_frames() {
        let f = fmt();
        let mut chunk = make_chunk(0, 0, 100, f);
        let mut buf = vec![0u8; 50 * f.frame_size() as usize];
        let read = chunk.read_frames(&mut buf, 50);
        assert_eq!(read, 50);
        assert!(!chunk.is_end());
        let read = chunk.read_frames(&mut buf, 50);
        assert_eq!(read, 50);
        assert!(chunk.is_end());
    }

    #[test]
    fn read_frames_bounded_by_output() {
        // A chunk with more data than the output can hold must fill only the
        // output (no panic), returning the number of whole frames written.
        let f = fmt();
        let mut chunk = make_chunk(0, 0, 100, f); // 100 frames available
        let mut buf = vec![0u8; 10 * f.frame_size() as usize]; // room for 10
        assert_eq!(chunk.read_frames(&mut buf, 100), 10);
    }

    #[test]
    fn pcm_chunk_seek() {
        let f = fmt();
        let mut chunk = make_chunk(0, 0, 100, f);
        chunk.seek(90);
        let mut buf = vec![0u8; 100 * f.frame_size() as usize];
        let read = chunk.read_frames(&mut buf, 100);
        assert_eq!(read, 10);
    }

    #[test]
    fn stream_add_and_count() {
        let f = fmt();
        let mut stream = Stream::new(f);
        assert_eq!(stream.chunk_count(), 0);
        stream.add_chunk(make_chunk(100, 0, 480, f));
        stream.add_chunk(make_chunk(100, 10_000, 480, f));
        assert_eq!(stream.chunk_count(), 2);
    }

    #[test]
    fn stream_clear() {
        let f = fmt();
        let mut stream = Stream::new(f);
        stream.add_chunk(make_chunk(100, 0, 480, f));
        stream.clear();
        assert_eq!(stream.chunk_count(), 0);
    }

    #[test]
    fn stream_silence_when_empty() {
        let f = fmt();
        let mut stream = Stream::new(f);
        let mut buf = vec![0xFFu8; 480 * f.frame_size() as usize];
        let result = stream.get_player_chunk(100_000_000, 0, &mut buf, 480);
        assert!(!result);
    }

    #[test]
    fn stream_hard_sync_plays_silence_when_too_early() {
        let f = fmt();
        let mut stream = Stream::new(f);
        stream.set_buffer_ms(1000);
        stream.add_chunk(make_chunk(100, 0, 4800, f));
        let server_now = 100_000_000i64;
        let mut buf = vec![0xFFu8; 480 * f.frame_size() as usize];
        let result = stream.get_player_chunk(server_now, 0, &mut buf, 480);
        assert!(result);
        assert!(buf.iter().all(|&b| b == 0));
    }

    #[test]
    fn stream_hard_sync_plays_data_when_aligned() {
        let f = fmt();
        let mut stream = Stream::new(f);
        stream.set_buffer_ms(1000);
        stream.add_chunk(make_chunk(99, 0, 4800, f));
        let server_now = 100_000_000i64;
        let mut buf = vec![0u8; 480 * f.frame_size() as usize];
        let result = stream.get_player_chunk(server_now, 0, &mut buf, 480);
        assert!(result);
        assert!(buf.iter().any(|&b| b != 0));
    }

    #[test]
    fn set_real_sample_rate_correction() {
        let f = fmt();
        let mut stream = Stream::new(f);
        stream.set_real_sample_rate(48000.0);
        assert_eq!(stream.correct_after_x_frames, 0);

        stream.set_real_sample_rate(47999.0);
        assert_ne!(stream.correct_after_x_frames, 0);
    }

    #[test]
    fn read_with_correction_remove_one_frame() {
        let f = fmt(); // 48000:16:2, frame_size=4
        let mut stream = Stream::new(f);

        let mut data = Vec::new();
        for i in 0..10u16 {
            data.extend_from_slice(&i.to_le_bytes());
            data.extend_from_slice(&(i + 100).to_le_bytes());
        }
        stream.add_chunk(make_chunk(100, 0, 10, f));
        stream.chunks.back_mut().unwrap().data = data;
        stream.current = stream.chunks.pop_front();

        let mut output = vec![0u8; 9 * f.frame_size() as usize];
        let ts = stream.read_with_correction(&mut output, 9, 1);
        assert!(ts.is_some());
        assert_eq!(output.len(), 36);
        for (i, chunk) in output.chunks(4).enumerate() {
            let left = u16::from_le_bytes([chunk[0], chunk[1]]);
            assert!(left <= 10, "frame {i}: left={left}");
        }
    }

    #[test]
    fn read_with_correction_zero_is_passthrough() {
        let f = fmt();
        let mut stream = Stream::new(f);
        stream.add_chunk(make_chunk(100, 0, 100, f));
        stream.current = stream.chunks.pop_front();

        let mut out1 = vec![0u8; 50 * f.frame_size() as usize];
        stream.read_with_correction(&mut out1, 50, 0);

        stream.add_chunk(make_chunk(100, 0, 100, f));
        stream.current = stream.chunks.pop_front();

        let mut out2 = vec![0u8; 50 * f.frame_size() as usize];
        stream.read_next(&mut out2, 50);

        assert_eq!(out1, out2);
    }

    // ---- PcmChunk edge cases ----

    #[test]
    fn pcm_chunk_duration_zero_rate() {
        // rate == 0 short-circuits to a 0 duration (guards a divide-by-zero).
        let f = SampleFormat::new(0, 16, 2);
        let chunk = PcmChunk::new(Timeval { sec: 0, usec: 0 }, vec![0u8; 16], f);
        assert_eq!(chunk.duration_usec(), 0);
    }

    #[test]
    fn pcm_chunk_zero_frame_size_is_safe() {
        // channels == 0 => frame_size 0 => read_frames and duration are no-ops.
        let f = SampleFormat::new(48000, 16, 0);
        let mut chunk = PcmChunk::new(Timeval { sec: 0, usec: 0 }, vec![0u8; 16], f);
        assert_eq!(chunk.duration_usec(), 0);
        let mut buf = vec![0u8; 16];
        assert_eq!(chunk.read_frames(&mut buf, 4), 0);
    }

    #[test]
    fn pcm_chunk_seek_past_end_clamps() {
        let f = fmt();
        let mut chunk = make_chunk(0, 0, 10, f);
        chunk.seek(1000); // far past the 10 available frames
        assert!(chunk.is_end());
        let mut buf = vec![0u8; f.frame_size() as usize];
        assert_eq!(chunk.read_frames(&mut buf, 1), 0);
    }

    #[test]
    fn pcm_chunk_is_end_lifecycle() {
        let f = fmt();
        let mut chunk = make_chunk(0, 0, 5, f);
        assert!(!chunk.is_end());
        let mut buf = vec![0u8; 5 * f.frame_size() as usize];
        chunk.read_frames(&mut buf, 5);
        assert!(chunk.is_end());
    }

    // ---- Stream accessors / encoding ----

    #[test]
    fn stream_accessors_report_format_and_encoding() {
        let s = Stream::new(SampleFormat::new(44100, 16, 1));
        assert_eq!(s.format().rate(), 44100);
        assert_eq!(s.format().channels(), 1);
        assert_eq!(s.encoding(), SampleEncoding::PcmInt);
    }

    #[test]
    fn stream_with_float32_encoding() {
        let s = Stream::with_encoding(fmt(), SampleEncoding::Float32);
        assert_eq!(s.encoding(), SampleEncoding::Float32);
    }

    // ---- silence helpers ----

    #[test]
    fn get_silence_zeroes_output() {
        let f = fmt();
        let s = Stream::new(f);
        let mut buf = vec![0xAAu8; 480 * f.frame_size() as usize];
        s.get_silence(&mut buf, 480);
        assert!(buf.iter().all(|&b| b == 0));
    }

    #[test]
    fn or_silence_fills_when_empty() {
        let f = fmt();
        let mut s = Stream::new(f);
        let mut buf = vec![0xAAu8; 480 * f.frame_size() as usize];
        let result = s.get_player_chunk_or_silence(100_000_000, 0, &mut buf, 480);
        assert!(!result, "empty stream underruns");
        assert!(
            buf.iter().all(|&b| b == 0),
            "silence is written on underrun"
        );
    }

    // ---- hard-sync chunk skipping (age > 0 path) ----

    #[test]
    fn hard_sync_skips_stale_chunk_and_aligns_to_next() {
        let f = fmt();
        let mut s = Stream::new(f);
        s.set_buffer_ms(1000);
        // Stale chunk (age far beyond its own duration) then an aligned one (age 0).
        s.add_chunk(make_chunk(90, 0, 4800, f));
        s.add_chunk(make_chunk(99, 0, 4800, f));
        let mut buf = vec![0u8; 480 * f.frame_size() as usize];
        let result = s.get_player_chunk(100_000_000, 0, &mut buf, 480);
        assert!(
            result,
            "should align to the good chunk after skipping the stale one"
        );
        assert!(
            buf.iter().any(|&b| b != 0),
            "expected real audio, not silence"
        );
    }

    #[test]
    fn hard_sync_seeks_into_partially_stale_chunk() {
        let f = fmt();
        let mut s = Stream::new(f);
        s.set_buffer_ms(1000);
        // One second of audio starting at 98.5 s: at server time 100 s with a
        // 1 s buffer, playout is 0.5 s into the chunk, so hard sync must seek
        // there and start playing instead of discarding the chunk.
        s.add_chunk(make_chunk(98, 500_000, 48000, f));
        let mut buf = vec![0u8; 480 * f.frame_size() as usize];
        assert!(s.get_player_chunk(100_000_000, 0, &mut buf, 480));
        assert!(!s.hard_sync, "hard sync completes after seeking");
        let pos = s.current.as_ref().unwrap().read_pos / f.frame_size() as usize;
        assert_eq!(pos, 24000 + 480, "seeked 0.5 s, then played one buffer");
    }

    #[test]
    fn read_continues_past_trailing_partial_frame() {
        // A payload that isn't a whole number of frames (malformed PCM) must
        // not stall the reader on the leftover bytes.
        let f = fmt();
        let mut s = Stream::new(f);
        let mut odd = make_chunk(100, 0, 10, f);
        odd.data.extend_from_slice(&[0xEE, 0xEE]);
        s.add_chunk(odd);
        s.add_chunk(make_chunk(100, 10_000, 10, f));
        s.current = s.chunks.pop_front();
        let mut out = vec![0u8; 15 * f.frame_size() as usize];
        assert!(s.read_next(&mut out, 15).is_some());
        assert_eq!(s.current.as_ref().unwrap().read_pos, 5 * 4);
    }

    #[test]
    fn underrun_zeroes_unfilled_output() {
        let f = fmt();
        let mut s = Stream::new(f);
        s.add_chunk(make_chunk(100, 0, 10, f));
        s.current = s.chunks.pop_front();
        let mut out = vec![0xAAu8; 20 * f.frame_size() as usize];
        s.read_next(&mut out, 20);
        assert!(out[40..].iter().all(|&b| b == 0), "no stale samples");
    }

    #[test]
    fn hard_sync_returns_false_when_all_chunks_stale() {
        let f = fmt();
        let mut s = Stream::new(f);
        s.set_buffer_ms(1000);
        // Every chunk is older than its own duration → skip loop exhausts, no alignment.
        s.add_chunk(make_chunk(90, 0, 480, f));
        s.add_chunk(make_chunk(91, 0, 480, f));
        let mut buf = vec![0u8; 480 * f.frame_size() as usize];
        assert!(!s.get_player_chunk(100_000_000, 0, &mut buf, 480));
    }

    // ---- normal playback (after hard sync completes) ----

    #[test]
    fn normal_playback_after_hard_sync() {
        let f = fmt();
        let mut s = Stream::new(f);
        s.set_buffer_ms(1000);
        s.add_chunk(make_chunk(99, 0, 48000, f)); // one second of audio, aligned at age 0
        let mut buf = vec![0u8; 480 * f.frame_size() as usize];
        assert!(s.get_player_chunk(100_000_000, 0, &mut buf, 480));
        assert!(
            !s.hard_sync,
            "hard sync completes on the first aligned call"
        );
        // Subsequent calls take the normal drift-correction playback path.
        let mut played = false;
        for i in 1..10i64 {
            let now = 100_000_000 + i * 10_000; // +10 ms == 480 frames @ 48 kHz
            if s.get_player_chunk(now, 0, &mut buf, 480) {
                played = true;
            }
        }
        assert!(played, "normal playback should deliver frames");
    }

    #[test]
    fn normal_playback_retriggers_hard_sync_on_large_age() {
        let f = fmt();
        let mut s = Stream::new(f);
        s.set_buffer_ms(1000);
        s.add_chunk(make_chunk(99, 0, 48000, f));
        let mut buf = vec![0u8; 480 * f.frame_size() as usize];
        assert!(s.get_player_chunk(100_000_000, 0, &mut buf, 480));
        assert!(!s.hard_sync);
        // Jump the clock >500 ms forward → age exceeds the hard-sync age threshold.
        let result = s.get_player_chunk(100_700_000, 0, &mut buf, 480);
        assert!(s.hard_sync, "a >500 ms age must re-trigger hard sync");
        assert!(!result, "an out-of-range age returns false");
    }

    #[test]
    fn out_of_order_timestamps_do_not_panic() {
        let f = fmt();
        let mut s = Stream::new(f);
        s.set_buffer_ms(1000);
        // Decreasing then increasing timestamps must never panic the sync loop.
        s.add_chunk(make_chunk(101, 0, 480, f));
        s.add_chunk(make_chunk(99, 0, 480, f));
        s.add_chunk(make_chunk(100, 0, 480, f));
        let mut buf = vec![0u8; 480 * f.frame_size() as usize];
        for i in 0..5i64 {
            let _ = s.get_player_chunk(100_000_000 + i * 10_000, 0, &mut buf, 480);
        }
    }
}
