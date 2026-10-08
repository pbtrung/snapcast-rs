//! Server time synchronization.
//!
//! Port of the C++ `TimeProvider`. Receives client-to-server and server-to-client
//! latency pairs, estimates the clock difference from them, and provides
//! `server_now()` — the estimated current server time.
//!
//! Unlike C++ (a plain median of the last 200 diffs), the estimate only uses
//! the exchanges with the lowest round trip. A reply that waited behind audio
//! (in the server's socket or a congested link) has a longer server-to-client
//! leg, which skews its diff by half the wait; its round trip is longer by
//! the same wait, so selecting low round trips discards it. With symmetric
//! jitter the selected diffs stay centered on the true difference.

use std::collections::VecDeque;

use snapcast_proto::Timeval;

#[cfg(test)]
mod sim;

/// Samples older than this are discarded before a new one is added (µs).
const RESET_AFTER_USEC: i64 = 60_000_000;
/// Exchanges kept (as the C++ median buffer: 200 s at one sync per second).
const WINDOW: usize = 200;
/// The estimate is the median diff of the `1 / LOW_RTT_DIVISOR` of the
/// window with the lowest round trips.
const LOW_RTT_DIVISOR: usize = 5;

/// One time exchange.
#[derive(Debug, Clone, Copy)]
struct Sample {
    /// `c2s + s2c`: the round trip minus the server's turnaround.
    rtt_usec: i64,
    /// `(c2s - s2c) / 2`: the clock difference plus half the path asymmetry.
    diff_usec: i64,
}

/// Provides the estimated server time based on time sync messages.
pub struct TimeProvider {
    /// Most recent exchanges, oldest first.
    samples: VecDeque<Sample>,
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
            samples: VecDeque::with_capacity(WINDOW),
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
        let sample = Sample {
            rtt_usec: c2s_usec + s2c_usec,
            diff_usec: (c2s_usec - s2c_usec) / 2,
        };
        tracing::trace!(
            diff_usec = sample.diff_usec,
            rtt_usec = sample.rtt_usec,
            "time sample"
        );

        // Clear buffer if last sync was more than 60 seconds ago
        if let Some(last) = self.last_sync_usec
            && local_usec - last > RESET_AFTER_USEC
        {
            self.samples.clear();
        }
        self.last_sync_usec = Some(local_usec);

        if self.samples.len() == WINDOW {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
        self.diff_to_server_usec = self.estimate();
    }

    /// Median diff of the exchanges with the lowest round trips; among equal
    /// round trips the newer exchanges are preferred.
    fn estimate(&self) -> i64 {
        let mut by_rtt: Vec<(usize, Sample)> = self.samples.iter().copied().enumerate().collect();
        by_rtt.sort_by_key(|&(age, s)| (s.rtt_usec, std::cmp::Reverse(age)));
        let keep = self.samples.len().div_ceil(LOW_RTT_DIVISOR);
        let mut diffs: Vec<i64> = by_rtt[..keep].iter().map(|(_, s)| s.diff_usec).collect();
        diffs.sort_unstable();
        diffs.get(diffs.len() / 2).copied().unwrap_or(0)
    }

    /// Set the time diff directly in milliseconds.
    pub fn set_diff_ms(&mut self, ms: f64) {
        let usec = (ms * 1000.0) as i64;
        // A zero round trip: c2s - s2c = 2 * diff, c2s + s2c = 0.
        self.add_sample_at(snapcast_proto::time::now_usec(), usec, -usec);
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
    fn low_round_trips_outweigh_queued_replies() {
        let mut tp = TimeProvider::new();
        // True diff 1 ms, 2 ms each way. Most replies waited 2..10 ms behind
        // audio, which shifts their diff by half the wait.
        for i in 0..100 {
            let wait = if i % 4 == 0 {
                0
            } else {
                2_000 + (i % 9) * 1_000
            };
            tp.add_sample_at(i * 1_000_000, 2_000 + 1_000, 2_000 + wait - 1_000);
        }
        assert_eq!(tp.diff_to_server_usec_at(100_000_000), 1_000);
    }

    #[test]
    fn samples_reset_after_a_minute_without_sync() {
        let mut tp = TimeProvider::new();
        for i in 0..10 {
            tp.add_sample_at(i * 1_000_000, 5_000, 1_000);
        }
        assert_eq!(tp.diff_to_server_usec_at(10_000_000), 2_000);
        // 61 s later, a slower link: the old samples must not win.
        tp.add_sample_at(71_000_000, 9_000, 5_000);
        assert_eq!(tp.diff_to_server_usec_at(71_000_000), 2_000);
        tp.add_sample_at(72_000_000, 10_000, 4_000);
        assert_eq!(tp.samples.len(), 2);
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
