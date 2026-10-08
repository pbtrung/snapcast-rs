//! Server time synchronization.
//!
//! Port of the C++ `TimeProvider`. Receives client-to-server and server-to-client
//! latency pairs, estimates the clock difference from them, and provides
//! `server_now()` — the estimated current server time.
//!
//! Unlike C++ (a plain median of the last 200 diffs), exchanges are
//! weighted by their round trip. A reply that waited behind audio (in the
//! server's socket or a congested link) has a longer server-to-client leg,
//! which skews its diff by half the wait; its round trip is longer by the
//! same wait, so a diff is off by at most half its round trip's excess over
//! the fastest exchange. Weighting by that excess discards queued replies;
//! with symmetric jitter the weighted diffs stay centered on the true
//! difference.
//!
//! Client and server clocks also run at slightly different rates (tens of
//! ppm), which a median over a 200 s window trails by half the window
//! (5 ms at 50 ppm). The estimate is therefore a line, offset + skew,
//! evaluated at the current time. The skew is the robust (Theil-Sen) slope
//! of the low round trip exchanges of the last 20 minutes, shrunk towards 0
//! unless clearly above its noise: a slope from the offset window alone
//! would add more noise at the window's end than a zero-skew median has.
//! The offset is the weighted median diff of the last 200 exchanges, each
//! moved to the current time along the skew.

use std::collections::VecDeque;

use snapcast_proto::Timeval;

#[cfg(test)]
mod sim;

/// Samples older than this are discarded before a new one is added (µs).
const RESET_AFTER_USEC: i64 = 60_000_000;
/// Exchanges the offset is taken from (as the C++ median buffer: 200 s at
/// one sync per second).
const WINDOW: usize = 200;
/// Exchanges the skew is taken from (20 minutes).
const SKEW_WINDOW: usize = 1200;
/// Only the `1 / LOW_RTT_DIVISOR` of a window with the lowest round trips
/// is used.
const LOW_RTT_DIVISOR: usize = 5;
/// Largest clock skew the model accepts (ppm); crystals are within ±100.
const MAX_SKEW_PPM: f64 = 500.0;
/// The skew is only fitted once the selected exchanges span this long...
const MIN_SKEW_SPAN_USEC: i64 = 20_000_000;
/// ...and number at least this many; until then it is taken as 0.
const MIN_SKEW_SAMPLES: usize = 16;
/// The selected exchanges are reduced to this many medians of consecutive
/// groups before the pairwise slopes, which keeps the fit cheap (it runs
/// under the lock the audio callback takes).
const SKEW_BINS: usize = 16;
/// Groups closer than this don't vote on the skew: their slope is mostly
/// jitter.
const MIN_PAIR_SPAN_USEC: i64 = 5_000_000;
/// The fitted skew `s` is scaled by `s² / (s² + SKEW_SHRINK * σ²)`, `σ`
/// being its standard error: a skew not clearly above the noise would add
/// more error, extrapolated to the current time, than it removes.
const SKEW_SHRINK: f64 = 16.0;
/// After this many exchanges (the controller's quick syncs), an update
/// moves the estimate by at most `MAX_STEP_USEC` plus `MAX_SLEW` times the
/// time since the previous exchange, so the stream sees no jumps (its soft
/// sync starts at 100 µs and corrects up to 500 ppm) when the selection
/// changes or the skew becomes known...
const SETTLED_SAMPLES: usize = 50;
const MAX_STEP_USEC: f64 = 50.0;
const MAX_SLEW: f64 = 250e-6;
/// ...unless the fit is further off than this: a real change (another
/// server), followed at once (it takes a hard sync anyway).
const JUMP_USEC: f64 = 5_000.0;

/// The clock difference as a line: `offset + skew * (t - anchor)`.
#[derive(Debug, Clone, Copy, Default)]
struct Line {
    anchor_usec: i64,
    offset_usec: f64,
    skew: f64,
}

impl Line {
    fn at(&self, local_usec: i64) -> f64 {
        self.offset_usec + self.skew * (local_usec - self.anchor_usec) as f64
    }
}

/// One time exchange.
#[derive(Debug, Clone, Copy)]
struct Sample {
    /// Local time the reply arrived.
    local_usec: i64,
    /// `c2s + s2c`: the round trip minus the server's turnaround.
    rtt_usec: i64,
    /// `(c2s - s2c) / 2`: the clock difference plus half the path asymmetry.
    diff_usec: i64,
}

/// Provides the estimated server time based on time sync messages.
pub struct TimeProvider {
    /// The last [`SKEW_WINDOW`] exchanges, oldest first.
    samples: VecDeque<Sample>,
    /// The estimate handed out, following the fit of `samples`.
    model: Line,
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
            samples: VecDeque::with_capacity(SKEW_WINDOW),
            model: Line::default(),
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
            local_usec,
            rtt_usec: c2s_usec + s2c_usec,
            diff_usec: (c2s_usec - s2c_usec) / 2,
        };
        tracing::trace!(
            diff_usec = sample.diff_usec,
            rtt_usec = sample.rtt_usec,
            "time sample"
        );

        // Clear buffer if last sync was more than 60 seconds ago
        let since_last = self.last_sync_usec.map_or(0, |last| local_usec - last);
        if since_last > RESET_AFTER_USEC {
            self.samples.clear();
        }
        self.last_sync_usec = Some(local_usec);

        if self.samples.len() == SKEW_WINDOW {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);

        let fit = self.fit(local_usec);
        self.model = if self.samples.len() < SETTLED_SAMPLES {
            fit
        } else {
            let current = self.model.at(local_usec);
            let delta = fit.at(local_usec) - current;
            let max_step = MAX_STEP_USEC + MAX_SLEW * since_last as f64;
            if delta.abs() > JUMP_USEC {
                fit
            } else {
                Line {
                    anchor_usec: local_usec,
                    offset_usec: current + delta.clamp(-max_step, max_step),
                    skew: fit.skew,
                }
            }
        };
    }

    /// Fit the line through the low round trip exchanges, anchored at
    /// `now`.
    fn fit(&self, now: i64) -> Line {
        let recent = self.samples.len().saturating_sub(WINDOW);
        let skew = skew_of(&lowest_rtt(self.samples.iter())).unwrap_or(0.0);
        let window: Vec<Sample> = self.samples.range(recent..).copied().collect();
        let mut offsets: Vec<(f64, f64)> = rtt_weights(&window)
            .into_iter()
            .zip(&window)
            .map(|(w, s)| (s.diff_usec as f64 - skew * (s.local_usec - now) as f64, w))
            .collect();
        Line {
            anchor_usec: now,
            offset_usec: weighted_median(&mut offsets).unwrap_or(0.0),
            skew,
        }
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
    pub fn diff_to_server_usec_at(&self, local_usec: i64) -> i64 {
        self.model.at(local_usec).round() as i64
    }
}

/// The `1 / LOW_RTT_DIVISOR` of `samples` with the lowest round trips
/// (among equal round trips the newer), in time order.
fn lowest_rtt<'a>(samples: impl Iterator<Item = &'a Sample>) -> Vec<Sample> {
    let mut by_rtt: Vec<(usize, Sample)> = samples.copied().enumerate().collect();
    by_rtt.sort_by_key(|&(age, s)| (s.rtt_usec, std::cmp::Reverse(age)));
    by_rtt.truncate(by_rtt.len().div_ceil(LOW_RTT_DIVISOR));
    by_rtt.sort_by_key(|&(age, _)| age);
    by_rtt.into_iter().map(|(_, s)| s).collect()
}

/// Skew (µs per µs) of `samples` (in time order): the median slope between
/// the medians of [`SKEW_BINS`] consecutive groups, over all pairs at least
/// [`MIN_PAIR_SPAN_USEC`] apart, shrunk by its significance
/// ([`SKEW_SHRINK`]) and clamped to [`MAX_SKEW_PPM`]. `None` while the
/// samples are too few or too close together.
fn skew_of(samples: &[Sample]) -> Option<f64> {
    let (first, last) = (samples.first()?, samples.last()?);
    if samples.len() < MIN_SKEW_SAMPLES || last.local_usec - first.local_usec < MIN_SKEW_SPAN_USEC {
        return None;
    }
    let points: Vec<(i64, f64)> = samples
        .chunks(samples.len().div_ceil(SKEW_BINS))
        .filter_map(|group| {
            let mut diffs: Vec<f64> = group.iter().map(|s| s.diff_usec as f64).collect();
            Some((group[group.len() / 2].local_usec, median(&mut diffs)?))
        })
        .collect();
    let mut slopes = Vec::new();
    for (i, &(ta, da)) in points.iter().enumerate() {
        for &(tb, db) in &points[i + 1..] {
            if tb - ta >= MIN_PAIR_SPAN_USEC {
                slopes.push((db - da) / (tb - ta) as f64);
            }
        }
    }
    let slope = median(&mut slopes)?;
    if slope == 0.0 {
        return Some(0.0);
    }

    // Standard error of a least-squares slope, from the robust (MAD)
    // scatter of the samples around the fitted line.
    let t0 = first.local_usec;
    let mean_t = samples
        .iter()
        .map(|s| (s.local_usec - t0) as f64)
        .sum::<f64>()
        / samples.len() as f64;
    let mut residuals: Vec<f64> = samples
        .iter()
        .map(|s| s.diff_usec as f64 - slope * (s.local_usec - t0) as f64)
        .collect();
    let center = median(&mut residuals)?;
    let mut deviations: Vec<f64> = residuals.iter().map(|r| (r - center).abs()).collect();
    let sigma = 1.4826 * median(&mut deviations)?;
    let sxx: f64 = samples
        .iter()
        .map(|s| ((s.local_usec - t0) as f64 - mean_t).powi(2))
        .sum();
    let variance = sigma * sigma / sxx;

    let shrunk = slope * slope * slope / (slope * slope + SKEW_SHRINK * variance);
    let max = MAX_SKEW_PPM * 1e-6;
    Some(shrunk.clamp(-max, max))
}

/// Weights for `samples` by round trip: a diff is off by at most half its
/// round trip's excess `e` over the fastest one, so each gets
/// `1 / (e² + e0²)`, `e0` being the excess at the [`LOW_RTT_DIVISOR`]
/// quantile. Queued replies (large `e`) count for next to nothing, while a
/// link with bounded jitter still uses most of its exchanges.
fn rtt_weights(samples: &[Sample]) -> Vec<f64> {
    let mut rtts: Vec<i64> = samples.iter().map(|s| s.rtt_usec).collect();
    rtts.sort_unstable();
    let Some(&min) = rtts.first() else {
        return Vec::new();
    };
    let quantile = rtts[(rtts.len() - 1) / LOW_RTT_DIVISOR];
    // At least 1 µs, so equal round trips weigh the same.
    let e0 = ((quantile - min) as f64).max(1.0);
    samples
        .iter()
        .map(|s| {
            let e = (s.rtt_usec - min) as f64;
            1.0 / (e * e + e0 * e0)
        })
        .collect()
}

/// Weighted (upper) median of `(value, weight)` pairs.
fn weighted_median(values: &mut [(f64, f64)]) -> Option<f64> {
    values.sort_by(|a, b| a.0.total_cmp(&b.0));
    let half = values.iter().map(|v| v.1).sum::<f64>() / 2.0;
    let mut acc = 0.0;
    for &(value, weight) in values.iter() {
        acc += weight;
        if acc > half {
            return Some(value);
        }
    }
    values.last().map(|v| v.0)
}

/// Upper median (as [`DoubleBuffer`](crate::double_buffer::DoubleBuffer)).
fn median(values: &mut [f64]) -> Option<f64> {
    values.sort_by(f64::total_cmp);
    values.get(values.len() / 2).copied()
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
    fn skew_is_tracked_without_lag() {
        let mut tp = TimeProvider::new();
        // Server clock runs 50 ppm fast: the diff grows 50 µs per second.
        for i in 0..200 {
            let t = i * 1_000_000;
            let diff = 7_000 + 50 * i;
            tp.add_sample_at(t, 1_000 + diff, 1_000 - diff);
        }
        let now = 200_000_000;
        assert!((tp.diff_to_server_usec_at(now) - (7_000 + 50 * 200)).abs() <= 2);
        assert!((tp.model.skew * 1e6 - 50.0).abs() < 0.1);
    }

    #[test]
    fn skew_is_clamped() {
        let mut tp = TimeProvider::new();
        for i in 0..200 {
            let diff = 2_000 * i; // 2000 ppm
            tp.add_sample_at(i * 1_000_000, 1_000 + diff, 1_000 - diff);
        }
        assert!((tp.model.skew - MAX_SKEW_PPM * 1e-6).abs() < 1e-12);
    }

    #[test]
    fn settled_estimate_moves_in_small_steps() {
        let mut tp = TimeProvider::new();
        for i in 0..50 {
            tp.add_sample_at(i * 1_000_000, 1_500, 500);
        }
        assert_eq!(tp.diff_to_server_usec_at(50_000_000), 500);
        // A faster route with another asymmetry: the diff is now 1.5 ms.
        // The estimate walks there in bounded steps (one exchange per
        // second)...
        let max_step = (MAX_STEP_USEC + MAX_SLEW * 1e6) as i64;
        let mut last = 500;
        let mut steps = 0;
        for i in 50..100 {
            tp.add_sample_at(i * 1_000_000, 2_400, -600);
            let now = tp.diff_to_server_usec_at(i * 1_000_000);
            assert!((now - last).abs() <= max_step);
            steps += i32::from(now != last);
            last = now;
        }
        assert!(steps > 1, "walked, not jumped");
        assert_eq!(last, 1_500);
        // ...but follows a change beyond JUMP_USEC at once.
        for i in 100..300 {
            tp.add_sample_at(i * 1_000_000, 11_500, -8_500);
        }
        assert_eq!(tp.diff_to_server_usec_at(300_000_000), 10_000);
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
