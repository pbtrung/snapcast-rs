//! Time utilities — server timestamp generation using the monotonic clock.
//!
//! The clock itself is the single source of truth in [`snapcast_proto::time`]
//! so the server and client cannot drift onto different clock domains.

/// Current monotonic time in microseconds.
pub fn now_usec() -> i64 {
    snapcast_proto::time::now_usec()
}

/// Generates evenly-spaced timestamps based on sample count.
/// This matches the C++ server behavior: timestamps reflect when audio
/// *should* be played, not when it was read from the source.
pub struct ChunkTimestamper {
    start_usec: i64,
    samples_written: u64,
    rate: u32,
}

impl ChunkTimestamper {
    /// Create a new timestamper anchored at the current time.
    pub fn new(rate: u32) -> Self {
        Self {
            start_usec: now_usec(),
            samples_written: 0,
            rate,
        }
    }

    /// Get the timestamp for the next chunk of `frames` frames.
    pub fn next(&mut self, frames: u32) -> i64 {
        let ts = self.start_usec + (self.samples_written as i64 * 1_000_000) / self.rate as i64;
        self.samples_written += frames as u64;
        ts
    }

    /// Re-anchor at the current time if the next timestamp is more than
    /// `max_lag_usec` in the past, i.e. the source delivered slower than
    /// realtime (it started late, or stalled). Returns the lag in µs that was
    /// dropped, if any.
    ///
    /// Without this, a source whose first audio arrives seconds after it was
    /// opened (for example ffmpeg with a lookahead filter) would stamp every
    /// chunk seconds in the past, and clients would discard them all as too
    /// late to play.
    pub fn resync_if_behind(&mut self, max_lag_usec: i64) -> Option<i64> {
        let next = self.start_usec + (self.samples_written as i64 * 1_000_000) / self.rate as i64;
        let lag = now_usec() - next;
        if lag > max_lag_usec {
            self.reset();
            Some(lag)
        } else {
            None
        }
    }

    /// Reset the timestamper (e.g. on stream restart).
    pub fn reset(&mut self) {
        self.start_usec = now_usec();
        self.samples_written = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resync_if_behind_reanchors_a_late_source() {
        let mut ts = ChunkTimestamper::new(48000);
        // On time: nothing to do.
        assert_eq!(ts.resync_if_behind(200_000), None);
        // Pretend the timestamper was anchored 15 s ago with no audio since.
        ts.start_usec -= 15_000_000;
        let lag = ts.resync_if_behind(200_000).expect("15 s behind");
        assert!(lag >= 15_000_000);
        // The next chunk is stamped now, not 15 s in the past.
        assert!((now_usec() - ts.next(960)).abs() < 100_000);
    }

    #[test]
    fn now_usec_is_positive_and_nondecreasing() {
        let a = now_usec();
        let b = now_usec();
        assert!(a > 0);
        assert!(b >= a);
    }

    #[test]
    fn first_chunk_is_anchored_at_start() {
        let mut ts = ChunkTimestamper::new(48_000);
        let start = ts.start_usec;
        assert_eq!(ts.next(1024), start);
    }

    #[test]
    fn timestamps_advance_by_exact_sample_duration() {
        let mut ts = ChunkTimestamper::new(48_000);
        let t0 = ts.next(48_000);
        let t1 = ts.next(48_000);
        // 48000 frames at 48 kHz is exactly one second (1,000,000 µs) apart.
        assert_eq!(t1 - t0, 1_000_000);
    }

    #[test]
    fn reset_rewinds_the_sample_counter() {
        let mut ts = ChunkTimestamper::new(44_100);
        ts.next(44_100);
        ts.next(44_100);
        ts.reset();
        // After reset the next chunk is anchored at the (new) start again.
        assert_eq!(ts.next(0), ts.start_usec);
    }
}
