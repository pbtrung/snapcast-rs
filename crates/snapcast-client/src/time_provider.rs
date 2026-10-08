//! Server time synchronization.
//!
//! Port of the C++ `TimeProvider`. Receives client-to-server and server-to-client
//! latency pairs, computes the clock difference via a median buffer, and provides
//! `server_now()` — the estimated current server time.

use snapcast_proto::Timeval;

use crate::double_buffer::DoubleBuffer;

#[cfg(test)]
mod sim;

/// Samples older than this are discarded before a new one is added (µs).
const RESET_AFTER_USEC: i64 = 60_000_000;

/// Provides the estimated server time based on time sync messages.
pub struct TimeProvider {
    diff_buffer: DoubleBuffer,
    diff_to_server_usec: i64,
    /// Local time of the last sample, on the [`now_usec`] clock.
    ///
    /// [`now_usec`]: snapcast_proto::time::now_usec
    last_sync_usec: Option<i64>,
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
            diff_to_server_usec: 0,
            last_sync_usec: None,
        }
    }

    /// Set the time diff from a c2s/s2c latency pair (as received in Time messages).
    ///
    /// The diff is computed as `(c2s - s2c) / 2` which cancels out the symmetric
    /// network latency, leaving only the clock difference.
    pub fn set_diff(&mut self, c2s: &Timeval, s2c: &Timeval) {
        self.add_sample_at(
            snapcast_proto::time::now_usec(),
            c2s.to_usec(),
            s2c.to_usec(),
        );
    }

    /// Add one time exchange measured at local time `local_usec`.
    ///
    /// `c2s_usec` is the server receive time minus the client send time and
    /// `s2c_usec` the client receive time minus the server send time, each
    /// mixing one-way latency with the clock difference. [`set_diff`]
    /// calls this with the current time; tests and simulations pass their own.
    ///
    /// [`set_diff`]: Self::set_diff
    pub fn add_sample_at(&mut self, local_usec: i64, c2s_usec: i64, s2c_usec: i64) {
        let diff_usec = (c2s_usec - s2c_usec) / 2;
        tracing::trace!(diff_usec, c2s_usec, s2c_usec, "time sample");

        // Clear buffer if last sync was more than 60 seconds ago
        if let Some(last) = self.last_sync_usec
            && local_usec - last > RESET_AFTER_USEC
        {
            self.diff_buffer.clear();
        }
        self.last_sync_usec = Some(local_usec);

        self.diff_buffer.add(diff_usec);
        // Plain median, as C++ `TimeProvider::setDiffToServer`.
        self.diff_to_server_usec = self.diff_buffer.median_simple();
    }

    /// Set the time diff directly in milliseconds.
    pub fn set_diff_ms(&mut self, ms: f64) {
        let usec = (ms * 1000.0) as i64;
        // c2s - s2c = 2 * diff
        self.add_sample_at(snapcast_proto::time::now_usec(), 2 * usec, 0);
    }

    /// Get the current diff to server in microseconds.
    pub fn diff_to_server_usec(&self) -> i64 {
        self.diff_to_server_usec_at(snapcast_proto::time::now_usec())
    }

    /// The diff to server in microseconds at local time `local_usec`.
    pub fn diff_to_server_usec_at(&self, _local_usec: i64) -> i64 {
        self.diff_to_server_usec
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
