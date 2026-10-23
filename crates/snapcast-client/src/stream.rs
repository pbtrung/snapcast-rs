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

impl SampleEncoding {
    /// Convert one sample, as stored in a [`PcmChunk`] (`bits` wide,
    /// little-endian, 24-bit padded to 4 bytes), to `f32` at full scale ±1.
    /// The padding byte of a 24-bit sample is ignored: the low 24 bits are
    /// sign-extended, as the server's encoders do. A width this encoding
    /// doesn't store, or too few bytes, is silence.
    pub fn sample_to_f32(self, bytes: &[u8], bits: u16) -> f32 {
        match (self, bits) {
            (Self::Float32, _) => bytes.first_chunk().map_or(0.0, |b| f32::from_le_bytes(*b)),
            (Self::PcmInt, 16) => bytes.first_chunk().map_or(0.0, |b| {
                f32::from(i16::from_le_bytes(*b)) / f32::from(i16::MAX)
            }),
            (Self::PcmInt, 24) => bytes.first_chunk().map_or(0.0, |b| {
                ((i32::from_le_bytes(*b) << 8) >> 8) as f32 / snapcast_proto::PCM_24BIT_MAX
            }),
            (Self::PcmInt, 32) => bytes
                .first_chunk()
                .map_or(0.0, |b| i32::from_le_bytes(*b) as f32 / i32::MAX as f32),
            _ => 0.0,
        }
    }
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
/// Largest playback rate correction (500 ppm, as C++).
const MAX_RATE_CORRECTION: f64 = 0.0005;
/// Proportional gain of the playout controller (per second): an error is
/// corrected with a time constant of `1 / KP` (2 s), 1 ms at the most
/// within it.
const KP: f64 = 0.5;
/// Integral gain (per second²), critically damped with [`KP`]. The integral
/// learns the rate difference between the sound card and the server clock,
/// which a proportional correction alone would leave as a constant error.
const KI: f64 = KP * KP / 4.0;
/// DoubleBuffer capacity for mini (fast) drift detection; its median is the
/// error the playout controller corrects.
const MINI_BUFFER_SIZE: usize = 20;
/// DoubleBuffer capacity for short-term drift detection.
const SHORT_BUFFER_SIZE: usize = 100;
/// DoubleBuffer capacity for long-term drift detection.
const BUFFER_SIZE: usize = 500;
/// Default buffer in milliseconds.
const DEFAULT_BUFFER_MS: i64 = 1000;
/// Consecutive chunks whose timestamps are further apart than this (µs)
/// are a discontinuity: a gap is filled with silence and an overlap is
/// skipped, so playback stays sample-aligned. Timestamps carry rounding of
/// a few µs.
const DISCONTINUITY_TOLERANCE_USEC: i64 = 500;
/// A discontinuity larger than this (µs) is resolved by a hard sync.
const DISCONTINUITY_HARD_SYNC_USEC: i64 = HARD_SYNC_AGE_USEC;
/// Audio queued beyond the playout buffer plus this (ms) is obsolete: it is
/// only there when nothing plays it (no output device, or playback waiting
/// for time sync) or the server sends faster than real time.
const QUEUE_SLACK_MS: i64 = 3000;
/// Upper bound on the bytes queued, whatever the format and buffer.
const MAX_QUEUE_BYTES: usize = 128 << 20;

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
/// 2. **Soft Sync**: Used for everything else. Instead of jumping, the
///    stream adjusts the playback rate (by at most 0.05%) by adding or
///    removing single frames, spread out evenly. A PI controller sets the
///    rate from the median age of the last 20 buffers: the proportional term
///    removes an error within a few seconds, the integral term learns the
///    rate difference between the sound card and the server clock. The rate
///    is applied fractionally, so even a correction of a few ppm is exact on
///    average. This is inaudible to most listeners.
///
/// ### Drift Detection
///
/// Synchronization is based on "age", which is the difference between when a sample
/// *should* have been played (server time) and when it *is* being played (now).
///
/// The stream maintains three `DoubleBuffer` instances to track drift over different timescales:
/// - **Mini Buffer** (20 samples): The error the soft sync corrects.
/// - **Short Buffer** (100 samples): Hard sync re-triggering.
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
    /// Sum of `data.len()` over `chunks`.
    queued_bytes: usize,
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
    /// Fraction of the playback rate removed (positive: frames are dropped,
    /// playing faster).
    rate_correction: f64,
    /// Integral of the error (µs·s).
    error_integral: f64,
    /// Frames owed to the rate correction, less than one either way.
    correction_frames: f64,
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
            queued_bytes: 0,
            current: None,
            buffer_ms: DEFAULT_BUFFER_MS,
            hard_sync: true,
            mini_buffer: DoubleBuffer::new(MINI_BUFFER_SIZE),
            short_buffer: DoubleBuffer::new(SHORT_BUFFER_SIZE),
            buffer: DoubleBuffer::new(BUFFER_SIZE),
            median: 0,
            short_median: 0,
            rate_correction: 0.0,
            error_integral: 0.0,
            correction_frames: 0.0,
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

    #[cfg(test)]
    pub(crate) fn is_hard_syncing(&self) -> bool {
        self.hard_sync
    }

    /// Enqueue a decoded PCM chunk.
    ///
    /// The queue is bounded to the playout buffer plus a few seconds (and
    /// [`MAX_QUEUE_BYTES`]): beyond that the oldest chunks are dropped, as
    /// they are past their playout time by then.
    pub fn add_chunk(&mut self, chunk: PcmChunk) {
        self.queued_bytes += chunk.data.len();
        self.chunks.push_back(chunk);
        let limit = self.max_queue_bytes();
        let mut dropped = 0usize;
        while self.queued_bytes > limit && self.chunks.len() > 1 {
            self.pop_front_chunk();
            dropped += 1;
        }
        if dropped > 0 {
            tracing::debug!(dropped, "Playout queue full, dropped the oldest chunks");
        }
    }

    /// Most bytes [`add_chunk`](Self::add_chunk) keeps queued.
    fn max_queue_bytes(&self) -> usize {
        let bytes_per_sec = u64::from(self.format.rate()) * u64::from(self.format.frame_size());
        let ms = (self.buffer_ms.max(0) + QUEUE_SLACK_MS) as u64;
        let bytes = bytes_per_sec.saturating_mul(ms) / 1000;
        usize::try_from(bytes).map_or(MAX_QUEUE_BYTES, |b| b.min(MAX_QUEUE_BYTES))
    }

    fn pop_front_chunk(&mut self) -> Option<PcmChunk> {
        let chunk = self.chunks.pop_front()?;
        self.queued_bytes = self.queued_bytes.saturating_sub(chunk.data.len());
        Some(chunk)
    }

    fn push_front_chunk(&mut self, chunk: PcmChunk) {
        self.queued_bytes += chunk.data.len();
        self.chunks.push_front(chunk);
    }

    /// Number of queued chunks.
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// Clear all queued chunks and reset sync state.
    pub fn clear(&mut self) {
        self.chunks.clear();
        self.queued_bytes = 0;
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

    /// Forget the soft sync state once a hard sync completes (whatever
    /// started it), so its jump isn't learned as drift. Kept across
    /// underruns: the drift hasn't changed.
    fn reset_soft_sync(&mut self) {
        self.rate_correction = 0.0;
        self.error_integral = 0.0;
        self.correction_frames = 0.0;
    }

    /// Set the rate correction from the filtered age `error_usec` after
    /// playing `frames` frames.
    fn update_soft_sync(&mut self, error_usec: f64, frames: u32) {
        let dt = f64::from(frames) / f64::from(self.format.rate());
        // Clamped so the integral term alone stays within the rate limit
        // (no windup).
        let max_integral = MAX_RATE_CORRECTION * 1e6 / KI;
        self.error_integral =
            (self.error_integral + error_usec * dt).clamp(-max_integral, max_integral);
        self.rate_correction = ((KP * error_usec + KI * self.error_integral) * 1e-6)
            .clamp(-MAX_RATE_CORRECTION, MAX_RATE_CORRECTION);
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
            self.current = match self.current.as_ref() {
                Some(ended) if !self.hard_sync => self.next_chunk(ended.end_usec()),
                _ => self.pop_front_chunk(),
            };
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
                    self.push_front_chunk(c);
                }
                while let Some(mut c) = self.pop_front_chunk() {
                    let a = playout_usec - c.start_usec();
                    if a <= 0 {
                        self.current = Some(c);
                        break;
                    }
                    if a < c.end_usec() - c.start_usec() {
                        // Rounded up: a seek short of the playout point
                        // leaves the chunk too old by a fraction of a frame,
                        // and the hard sync would fail again.
                        let rate = i64::from(self.format.rate());
                        let skip = (a * rate + 999_999) / 1_000_000;
                        c.seek(skip as u32);
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
                    // Completed before reading on: a timestamp jump the read
                    // runs into starts the next hard sync, which must stick.
                    self.hard_sync = false;
                    self.reset_buffers();
                    self.reset_soft_sync();
                    let offset = silent_frames as usize * frame_size;
                    self.read_next(&mut output[offset..], remaining);
                }
                return true;
            }
            return false;
        }

        // --- Normal playback with drift correction ---

        // Whole frames to drop (positive) or repeat at the current rate; the
        // fraction carries over.
        self.correction_frames += self.rate_correction * f64::from(frames);
        let frames_correction = self.correction_frames.trunc() as i32;
        self.correction_frames -= f64::from(frames_correction);

        // Read with correction (or plain read if correction == 0)
        let chunk_start = match self.read_with_correction(output, frames, frames_correction) {
            Some(ts) => ts,
            None => return false,
        };

        let age_usec = playout_usec - chunk_start;

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
            && self.mini_buffer.median().abs() > HARD_SYNC_MINI_MEDIAN_USEC
            && age_usec.abs() > HARD_SYNC_MIN_AGE_USEC
        {
            tracing::info!(
                age_usec,
                mini_median = self.mini_buffer.median(),
                "Hard sync: mini buffer full, |mini_median| > 50ms"
            );
            self.hard_sync = true;
        } else if age_usec.abs() > HARD_SYNC_AGE_USEC {
            tracing::info!(age_usec, "Hard sync: |age| > 500ms");
            self.hard_sync = true;
        }

        if !self.hard_sync {
            self.update_buffers(age_usec);
            if self.mini_buffer.full() {
                let error = self.mini_buffer.median();
                self.update_soft_sync(error as f64, frames);
            }
        }

        // The long medians only re-trigger hard syncs; once a second is
        // enough. Stats are logged with them.
        let now_sec = server_now_usec / 1_000_000;
        if now_sec != self.last_log_sec {
            self.last_log_sec = now_sec;
            self.median = self.buffer.median();
            self.short_median = self.short_buffer.median();
            tracing::debug!(
                target: "Stats",
                "Chunk: {}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.1}ppm",
                age_usec,
                self.mini_buffer.median(),
                self.short_median,
                self.median,
                self.buffer.len(),
                output_buffer_dac_time_usec / 1000,
                self.frame_delta,
                self.rate_correction * 1e6,
            );
            self.frame_delta = 0;
        }

        age_usec.abs() < HARD_SYNC_AGE_USEC
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
        let frame_size = self.format.frame_size() as usize;
        let ts = self.current.as_ref()?.start_usec();
        let mut read = 0u32;
        while read < frames {
            let offset = read as usize * frame_size;
            let chunk = self.current.as_mut()?;
            let n = chunk.read_frames(&mut output[offset..], frames - read);
            read += n;
            if read < frames && chunk.is_end() {
                let ended = chunk.end_usec();
                match self.next_chunk(ended) {
                    Some(next) => self.current = Some(next),
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

    /// Take the chunk to play after one that ended at server time
    /// `prev_end_usec`, keeping playback sample-aligned when the timestamps
    /// are not contiguous (the server dropped chunks, a chunk failed to
    /// decode, or the server's timestamps were re-anchored):
    ///
    /// - a gap is filled with exactly as much silence (a silent chunk is
    ///   returned and the next one stays queued);
    /// - an overlap is skipped, dropping chunks that were entirely played;
    /// - a jump beyond [`DISCONTINUITY_HARD_SYNC_USEC`] either way starts a
    ///   hard sync and returns `None`.
    fn next_chunk(&mut self, prev_end_usec: i64) -> Option<PcmChunk> {
        let rate = i64::from(self.format.rate());
        loop {
            let mut next = self.pop_front_chunk()?;
            let jump = next.start_usec() - prev_end_usec;
            if jump.abs() <= DISCONTINUITY_TOLERANCE_USEC || rate == 0 {
                return Some(next);
            }
            if jump.abs() > DISCONTINUITY_HARD_SYNC_USEC {
                tracing::info!(jump_usec = jump, "Hard sync: chunk timestamps jump");
                self.push_front_chunk(next);
                self.hard_sync = true;
                return None;
            }
            let frames = (jump.abs() * rate + 500_000) / 1_000_000;
            if jump > 0 {
                tracing::info!(
                    gap_usec = jump,
                    frames,
                    "Gap between chunks, inserting silence"
                );
                self.push_front_chunk(next);
                let bytes = frames as usize * self.format.frame_size() as usize;
                return Some(PcmChunk::new_with_encoding(
                    Timeval::from_usec(prev_end_usec),
                    vec![0; bytes],
                    self.format,
                    self.encoding,
                ));
            }
            let remaining = (next.data.len() - next.read_pos) as i64
                / i64::from(self.format.frame_size().max(1));
            tracing::info!(
                overlap_usec = -jump,
                frames = frames.min(remaining),
                "Chunks overlap, skipping audio already played"
            );
            if frames >= remaining {
                continue;
            }
            next.seek(frames as u32);
            return Some(next);
        }
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
    fn soft_sync_rate_is_proportional_and_clamped() {
        let mut stream = Stream::new(fmt());
        stream.update_soft_sync(100.0, 0);
        assert!((stream.rate_correction - KP * 100e-6).abs() < 1e-12);
        stream.update_soft_sync(-10_000.0, 0);
        assert_eq!(stream.rate_correction, -MAX_RATE_CORRECTION);
    }

    #[test]
    fn soft_sync_integral_learns_a_constant_drift() {
        let mut stream = Stream::new(fmt());
        // Error from a constant 30 ppm drift, integrated over a minute
        // (a 10 ms buffer each step) with the error held at 0: the integral
        // term alone keeps correcting.
        for _ in 0..6000 {
            stream.update_soft_sync(60.0, 480);
        }
        let learned = stream.rate_correction;
        stream.update_soft_sync(0.0, 480);
        assert!(stream.rate_correction > 0.0);
        assert!(stream.rate_correction <= learned);
        stream.reset_soft_sync();
        stream.update_soft_sync(0.0, 480);
        assert_eq!(stream.rate_correction, 0.0, "a hard sync forgets it");
    }

    #[test]
    fn fractional_rate_drops_frames_exactly() {
        // 1/7680 (130 ppm) is 1/16 frame per 480: one frame dropped every
        // 16th buffer, 64 (1.3 ms, short of a hard sync) over 1024.
        let f = fmt();
        let mut s = Stream::new(f);
        s.set_buffer_ms(1000);
        s.add_chunk(counter_chunk(99_000_000, 0, 48_000 * 20));
        let mut buf = vec![0u8; 480 * 4];
        assert!(s.get_player_chunk(100_000_000, 0, &mut buf, 480));
        let calls = 1024;
        for i in 1..=calls {
            // Held, instead of what the controller makes of the age.
            s.rate_correction = 1.0 / 7680.0;
            assert!(s.get_player_chunk(100_000_000 + i * 10_000, 0, &mut buf, 480));
        }
        assert!(!s.hard_sync);
        let last = *indices(&buf).last().unwrap();
        assert_eq!(last - 480 * (calls as u32 + 1), 64);
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

    // ---- sample conversion ----

    #[test]
    fn sample_to_f32_full_scale() {
        let pcm = SampleEncoding::PcmInt;
        assert_eq!(pcm.sample_to_f32(&i16::MAX.to_le_bytes(), 16), 1.0);
        assert_eq!(pcm.sample_to_f32(&(-i16::MAX).to_le_bytes(), 16), -1.0);
        assert_eq!(pcm.sample_to_f32(&8_388_607i32.to_le_bytes(), 24), 1.0);
        assert_eq!(pcm.sample_to_f32(&i32::MAX.to_le_bytes(), 32), 1.0);
        let f = SampleEncoding::Float32;
        assert_eq!(f.sample_to_f32(&0.5f32.to_le_bytes(), 32), 0.5);
    }

    /// Regression: the padding byte was read as signal, so a zero-padded
    /// -1 became about +2.0.
    #[test]
    fn sample_to_f32_24bit_ignores_padding_byte() {
        let pcm = SampleEncoding::PcmInt;
        let minus_one = -1.0 / snapcast_proto::PCM_24BIT_MAX;
        for padding in [0x00, 0xFF, 0x5A] {
            assert_eq!(
                pcm.sample_to_f32(&[0xFF, 0xFF, 0xFF, padding], 24),
                minus_one
            );
            assert_eq!(pcm.sample_to_f32(&[0xFF, 0xFF, 0x7F, padding], 24), 1.0);
            assert_eq!(
                pcm.sample_to_f32(&[0x00, 0x00, 0x80, padding], 24),
                -8_388_608.0 / snapcast_proto::PCM_24BIT_MAX
            );
        }
    }

    #[test]
    fn sample_to_f32_short_or_unsupported_is_silence() {
        let pcm = SampleEncoding::PcmInt;
        assert_eq!(pcm.sample_to_f32(&[0x7F], 16), 0.0);
        assert_eq!(pcm.sample_to_f32(&[0x7F, 0x7F], 8), 0.0);
        assert_eq!(SampleEncoding::Float32.sample_to_f32(&[1, 2], 32), 0.0);
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
    fn hard_sync_seek_off_the_frame_grid_completes_at_once() {
        // The playout point falls between two frames (20.83 µs apart at
        // 48 kHz): the seek must land on or after it, not leave the chunk a
        // fraction of a frame too old (which failed the hard sync on nearly
        // every call).
        for usec in [500_010, 500_001, 499_990] {
            let mut s = Stream::new(fmt());
            s.set_buffer_ms(1000);
            s.add_chunk(counter_chunk(98_000_000 + usec, 0, 48_000));
            let mut buf = vec![0u8; 480 * 4];
            assert!(s.get_player_chunk(100_000_000, 0, &mut buf, 480), "{usec}");
            assert!(!s.hard_sync, "{usec}");
        }
    }

    #[test]
    fn hard_sync_resets_the_soft_sync() {
        let f = fmt();
        let mut s = Stream::new(f);
        s.set_buffer_ms(1000);
        s.add_chunk(make_chunk(99, 0, 48_000, f));
        s.rate_correction = 1e-4;
        s.error_integral = 100.0;
        s.correction_frames = 0.5;
        let mut buf = vec![0u8; 480 * f.frame_size() as usize];
        assert!(s.get_player_chunk(100_000_000, 0, &mut buf, 480));
        assert!(!s.hard_sync);
        assert_eq!(
            (s.rate_correction, s.error_integral, s.correction_frames),
            (0.0, 0.0, 0.0)
        );
    }

    #[test]
    fn timestamp_jump_read_by_the_completing_hard_sync_starts_another() {
        // The hard sync aligns to the first chunk and reads on into the
        // next, whose timestamp jumps 10 s.
        let mut s = Stream::new(fmt());
        s.set_buffer_ms(1000);
        s.add_chunk(counter_chunk(99_000_000, 0, 240));
        s.add_chunk(counter_chunk(109_005_000, 240, 480));
        let mut buf = vec![0xAAu8; 480 * 4];
        assert!(s.get_player_chunk(100_000_000, 0, &mut buf, 480));
        assert!(s.hard_sync, "the jump starts the next hard sync");
        let got = indices(&buf);
        assert_eq!(got[239], 240);
        assert!(got[240..].iter().all(|&i| i == 0));
        assert_eq!(s.chunks.len(), 1, "the chunk is kept for the hard sync");
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
        // Contiguous: 10 frames at 48 kHz last 208 µs.
        s.add_chunk(make_chunk(100, 208, 10, f));
        s.current = s.chunks.pop_front();
        let mut out = vec![0u8; 15 * f.frame_size() as usize];
        assert!(s.read_next(&mut out, 15).is_some());
        assert_eq!(s.current.as_ref().unwrap().read_pos, 5 * 4);
    }

    // ---- discontinuities ----

    /// A chunk of `frames` frames starting at `ts_usec` whose frames hold
    /// their index from `first` on (0 is left for silence).
    fn counter_chunk(ts_usec: i64, first: u32, frames: u32) -> PcmChunk {
        let data = (first..first + frames)
            .flat_map(|i| (i + 1).to_le_bytes())
            .collect();
        PcmChunk::new(Timeval::from_usec(ts_usec), data, fmt())
    }

    fn indices(out: &[u8]) -> Vec<u32> {
        out.chunks(4)
            .map(|f| u32::from_le_bytes(f.try_into().unwrap()))
            .collect()
    }

    /// A stream positioned at the start of the first of `chunks`.
    fn stream_of(chunks: Vec<PcmChunk>) -> Stream {
        let mut s = Stream::new(fmt());
        for c in chunks {
            s.add_chunk(c);
        }
        s.current = s.chunks.pop_front();
        s.hard_sync = false;
        s
    }

    const T: i64 = 100_000_000;

    #[test]
    fn gap_between_chunks_is_filled_with_silence() {
        // 480 frames (10 ms), then the next chunk 5 ms (240 frames) late.
        let mut s = stream_of(vec![
            counter_chunk(T, 0, 480),
            counter_chunk(T + 15_000, 720, 480),
        ]);
        let mut out = vec![0xAAu8; 1440 * 4];
        s.read_next(&mut out, 1440);
        let got = indices(&out);
        assert_eq!(got[479], 480);
        assert!(
            got[480..720].iter().all(|&i| i == 0),
            "240 frames of silence"
        );
        assert_eq!(got[720], 721, "resumes at the late chunk's first frame");
        assert_eq!(got[1199], 1200);
        assert!(got[1200..].iter().all(|&i| i == 0), "underrun");
    }

    #[test]
    fn overlapping_chunk_skips_audio_already_played() {
        // The next chunk starts 5 ms (240 frames) before this one ends.
        let mut s = stream_of(vec![
            counter_chunk(T, 0, 480),
            counter_chunk(T + 5_000, 240, 480),
        ]);
        let mut out = vec![0u8; 720 * 4];
        s.read_next(&mut out, 720);
        let got = indices(&out);
        assert_eq!(&got[478..482], &[479, 480, 481, 482], "no frame repeated");
        assert_eq!(got[719], 720);
    }

    #[test]
    fn chunk_inside_played_audio_is_dropped() {
        let mut s = stream_of(vec![
            counter_chunk(T, 0, 960),
            counter_chunk(T + 5_000, 240, 240),
            counter_chunk(T + 20_000, 960, 480),
        ]);
        let mut out = vec![0u8; 1440 * 4];
        s.read_next(&mut out, 1440);
        let got = indices(&out);
        assert_eq!(&got[958..962], &[959, 960, 961, 962]);
        assert!(s.chunks.is_empty());
    }

    #[test]
    fn timestamp_jitter_within_tolerance_is_ignored() {
        let mut s = stream_of(vec![
            counter_chunk(T, 0, 480),
            counter_chunk(T + 10_100, 480, 480),
        ]);
        let mut out = vec![0u8; 960 * 4];
        s.read_next(&mut out, 960);
        let got = indices(&out);
        assert_eq!(&got[479..481], &[480, 481]);
    }

    #[test]
    fn large_timestamp_jump_starts_a_hard_sync() {
        for next in [T + 10_000_000, T - 10_000_000] {
            let mut s = stream_of(vec![
                counter_chunk(T, 0, 480),
                counter_chunk(next, 480, 480),
            ]);
            let mut out = vec![0xAAu8; 960 * 4];
            s.read_next(&mut out, 960);
            assert!(s.hard_sync, "jump to {next}");
            assert!(out[480 * 4..].iter().all(|&b| b == 0));
            assert_eq!(s.chunks.len(), 1, "the chunk is kept for the hard sync");
        }
    }

    #[test]
    fn gap_after_an_ended_chunk_is_filled_too() {
        // The current chunk ended exactly at a buffer boundary; the next
        // get_player_chunk call picks up the late chunk.
        let f = fmt();
        let mut s = Stream::new(f);
        s.set_buffer_ms(1000);
        s.add_chunk(counter_chunk(T, 0, 480));
        let mut out = vec![0u8; 480 * 4];
        assert!(s.get_player_chunk(T + 1_000_000, 0, &mut out, 480));
        assert!(!s.hard_sync);
        s.add_chunk(counter_chunk(T + 20_000, 960, 480));
        assert!(s.get_player_chunk(T + 1_010_000, 0, &mut out, 480));
        assert!(indices(&out).iter().all(|&i| i == 0), "10 ms of silence");
        assert!(s.get_player_chunk(T + 1_020_000, 0, &mut out, 480));
        assert_eq!(indices(&out)[0], 961);
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
