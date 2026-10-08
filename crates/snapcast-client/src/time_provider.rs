//! Server time synchronization.
//!
//! Port of the C++ `TimeProvider`. Receives client-to-server and server-to-client
//! latency pairs, computes the clock difference via a median buffer, and provides
//! `server_now()` — the estimated current server time.

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

use snapcast_proto::Timeval;

use crate::double_buffer::DoubleBuffer;

/// Provides the estimated server time based on time sync messages.
pub struct TimeProvider {
    diff_buffer: DoubleBuffer,
    diff_to_server_usec: AtomicI64,
    last_sync: Option<Instant>,
}

impl Default for TimeProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl TimeProvider {
    /// Create a new time provider with default settings.
    pub fn new() -> Self {
        Self {
            diff_buffer: DoubleBuffer::new(200),
            diff_to_server_usec: AtomicI64::new(0),
            last_sync: None,
        }
    }

    /// Set the time diff from a c2s/s2c latency pair (as received in Time messages).
    ///
    /// The diff is computed as `(c2s - s2c) / 2` which cancels out the symmetric
    /// network latency, leaving only the clock difference.
    pub fn set_diff(&mut self, c2s: &Timeval, s2c: &Timeval) {
        let diff_ms = (f64::from(c2s.sec) / 2.0 - f64::from(s2c.sec) / 2.0) * 1000.0
            + (f64::from(c2s.usec) / 2.0 - f64::from(s2c.usec) / 2.0) / 1000.0;
        tracing::trace!(diff_ms, "set_diff");
        self.set_diff_ms(diff_ms);
    }

    /// Set the time diff directly in milliseconds.
    pub fn set_diff_ms(&mut self, ms: f64) {
        let now = Instant::now();

        // Clear buffer if last sync was more than 60 seconds ago
        if let Some(last) = self.last_sync
            && now.duration_since(last) > Duration::from_secs(60)
        {
            self.diff_buffer.clear();
        }
        self.last_sync = Some(now);

        self.diff_buffer.add((ms * 1000.0) as i64);
        // Plain median, as C++ `TimeProvider::setDiffToServer`.
        let median = self.diff_buffer.median_simple();
        self.diff_to_server_usec.store(median, Ordering::Relaxed);
    }

    /// Get the current diff to server in microseconds.
    pub fn diff_to_server_usec(&self) -> i64 {
        self.diff_to_server_usec.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_diff_is_zero() {
        let tp = TimeProvider::new();
        assert_eq!(tp.diff_to_server_usec(), 0);
    }

    #[test]
    fn set_diff_from_latency_pair() {
        let mut tp = TimeProvider::new();

        // c2s = 10ms, s2c = 8ms → diff = (10 - 8) / 2 = 1ms = 1000 usec
        let c2s = Timeval {
            sec: 0,
            usec: 10_000,
        };
        let s2c = Timeval {
            sec: 0,
            usec: 8_000,
        };
        tp.set_diff(&c2s, &s2c);
        assert_eq!(tp.diff_to_server_usec(), 1000);
    }

    #[test]
    fn median_stabilizes() {
        let mut tp = TimeProvider::new();

        // Feed several values, one outlier
        for _ in 0..10 {
            tp.set_diff_ms(5.0); // 5ms = 5000 usec
        }
        tp.set_diff_ms(100.0); // outlier

        // Median should still be close to 5000 usec
        assert_eq!(tp.diff_to_server_usec(), 5000);
    }

    #[test]
    fn negative_diff() {
        let mut tp = TimeProvider::new();
        tp.set_diff_ms(-3.5);
        assert_eq!(tp.diff_to_server_usec(), -3500);
    }

    #[test]
    fn set_diff_symmetric_latency_cancels() {
        let mut tp = TimeProvider::new();

        // If c2s == s2c, the diff should be 0 (symmetric network)
        let c2s = Timeval { sec: 0, usec: 5000 };
        let s2c = Timeval { sec: 0, usec: 5000 };
        tp.set_diff(&c2s, &s2c);
        assert_eq!(tp.diff_to_server_usec(), 0);
    }
}
