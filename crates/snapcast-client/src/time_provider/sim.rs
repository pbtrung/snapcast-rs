//! Deterministic time-sync simulator for evaluating [`TimeProvider`] filters.
//!
//! Replays synthetic Time exchanges on the controller's schedule (50 quick
//! syncs at 100 ms, then one per second) through a modelled network with a
//! known server clock (offset and skew), symmetric per-direction jitter and
//! load-correlated queuing of server-to-client replies (a reply waiting
//! behind audio). The estimate is compared against the true clock difference
//! every 100 ms after a warm-up.
//!
//! `cargo test -p snapcast-client --lib time_provider::sim -- --ignored --nocapture`
//! prints a report for all scenarios.

use super::TimeProvider;
use crate::double_buffer::DoubleBuffer;

/// Exchanges done at [`QUICK_INTERVAL_USEC`] before switching to one per second.
const QUICK_SYNCS: u32 = 50;
const QUICK_INTERVAL_USEC: f64 = 100_000.0;
const SYNC_INTERVAL_USEC: f64 = 1_000_000.0;
/// Errors are sampled this often...
const EVAL_INTERVAL_USEC: f64 = 100_000.0;
/// ...once the quick syncs are long done.
const WARMUP_USEC: f64 = 10_000_000.0;
/// Server time spent between receiving a request and sending the reply.
const SERVER_TURNAROUND_USEC: f64 = 50.0;
/// Local clock reading at the start (a monotonic clock is rarely near 0).
const LOCAL_START_USEC: f64 = 1_000_000_000.0;

/// splitmix64: tiny, deterministic, good enough for jitter.
pub(super) struct Rng(u64);

impl Rng {
    pub(super) fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in [0, 1).
    pub(super) fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Exponentially distributed with the given mean.
    fn exp(&mut self, mean: f64) -> f64 {
        -mean * (1.0 - self.uniform()).ln()
    }
}

/// One simulated setup.
#[derive(Clone, Copy)]
pub(super) struct Scenario {
    pub name: &'static str,
    pub duration_s: f64,
    /// Server clock minus client clock at the start (µs).
    pub offset_usec: f64,
    /// Server clock rate relative to the client clock (ppm).
    pub skew_ppm: f64,
    /// Fixed one-way latency, each direction (µs).
    pub latency_usec: f64,
    /// Mean of the exponential jitter added independently per direction (µs).
    pub jitter_usec: f64,
    /// Probability that a reply is queued behind audio while loaded.
    pub queue_prob: f64,
    /// A queued reply waits uniformly up to this long (µs).
    pub queue_max_usec: f64,
    /// Load alternates on/off with this period (s); 0 = always loaded.
    pub load_period_s: f64,
    pub seed: u64,
}

impl Scenario {
    const fn base(name: &'static str) -> Self {
        Self {
            name,
            duration_s: 600.0,
            offset_usec: 12_345.0,
            skew_ppm: 0.0,
            latency_usec: 1_000.0,
            jitter_usec: 300.0,
            queue_prob: 0.0,
            queue_max_usec: 0.0,
            load_period_s: 0.0,
            seed: 1,
        }
    }

    /// True server minus client clock at local time `local` (µs).
    fn true_diff(&self, local: f64) -> f64 {
        self.offset_usec + (local - LOCAL_START_USEC) * self.skew_ppm * 1e-6
    }

    fn loaded(&self, local: f64) -> bool {
        if self.load_period_s <= 0.0 {
            return true;
        }
        let period = self.load_period_s * 1e6;
        (local - LOCAL_START_USEC) % period < period / 2.0
    }
}

/// The scenarios of the report.
pub(super) fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario::base("symmetric jitter 300us"),
        Scenario {
            jitter_usec: 2_000.0,
            ..Scenario::base("symmetric jitter 2ms")
        },
        Scenario {
            queue_prob: 0.5,
            queue_max_usec: 5_000.0,
            ..Scenario::base("s2c queuing 50% x U(0,5ms)")
        },
        Scenario {
            queue_prob: 0.8,
            queue_max_usec: 10_000.0,
            load_period_s: 40.0,
            ..Scenario::base("bursty load 80% x U(0,10ms)")
        },
        Scenario {
            skew_ppm: 50.0,
            ..Scenario::base("skew 50ppm")
        },
        Scenario {
            skew_ppm: -120.0,
            jitter_usec: 1_000.0,
            ..Scenario::base("skew -120ppm, jitter 1ms")
        },
        Scenario {
            skew_ppm: 50.0,
            queue_prob: 0.5,
            queue_max_usec: 5_000.0,
            ..Scenario::base("skew 50ppm + s2c queuing")
        },
    ]
}

/// Something that turns time exchanges into a clock difference estimate.
pub(super) trait Estimator {
    fn add(&mut self, local_usec: i64, c2s_usec: i64, s2c_usec: i64);
    fn diff_at(&self, local_usec: i64) -> i64;
}

impl Estimator for TimeProvider {
    fn add(&mut self, local_usec: i64, c2s_usec: i64, s2c_usec: i64) {
        self.add_sample_at(local_usec, c2s_usec, s2c_usec);
    }

    fn diff_at(&self, local_usec: i64) -> i64 {
        self.diff_to_server_usec_at(local_usec)
    }
}

/// The C++ estimator: plain median of the last 200 diffs (reference).
pub(super) struct PlainMedian(DoubleBuffer);

impl PlainMedian {
    pub(super) fn new() -> Self {
        Self(DoubleBuffer::new(200))
    }
}

impl Estimator for PlainMedian {
    fn add(&mut self, _local_usec: i64, c2s_usec: i64, s2c_usec: i64) {
        self.0.add((c2s_usec - s2c_usec) / 2);
    }

    fn diff_at(&self, _local_usec: i64) -> i64 {
        self.0.median_simple()
    }
}

/// Error statistics (µs).
#[derive(Debug, Clone, Copy)]
pub(super) struct Stats {
    /// Mean signed error (bias).
    pub mean: f64,
    /// Percentiles and maximum of the absolute error.
    pub p50: f64,
    pub p95: f64,
    pub max: f64,
}

impl Stats {
    pub(super) fn from_errors(errors: &[f64]) -> Self {
        assert!(!errors.is_empty());
        let mean = errors.iter().sum::<f64>() / errors.len() as f64;
        let mut abs: Vec<f64> = errors.iter().map(|e| e.abs()).collect();
        abs.sort_by(f64::total_cmp);
        let pct = |p: f64| abs[((abs.len() - 1) as f64 * p).round() as usize];
        Self {
            mean,
            p50: pct(0.5),
            p95: pct(0.95),
            max: abs[abs.len() - 1],
        }
    }
}

impl std::fmt::Display for Stats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "mean {:>8.1}  p50 {:>7.1}  p95 {:>7.1}  max {:>7.1}",
            self.mean, self.p50, self.p95, self.max
        )
    }
}

/// Run `scenario` through `est`, returning the estimation error statistics.
pub(super) fn run(scenario: &Scenario, est: &mut dyn Estimator) -> Stats {
    let mut rng = Rng::new(scenario.seed);
    let end = LOCAL_START_USEC + scenario.duration_s * 1e6;
    let server_clock = |t: f64| t + scenario.true_diff(t);

    let mut errors = Vec::new();
    let mut next_eval = LOCAL_START_USEC + WARMUP_USEC;
    let mut t1 = LOCAL_START_USEC;
    let mut exchanges = 0u32;
    // Replies are delivered in order, as on a TCP stream.
    let mut last_t4 = 0.0f64;
    while t1 < end {
        let up = scenario.latency_usec + rng.exp(scenario.jitter_usec);
        let arrive = t1 + up;
        let depart = arrive + SERVER_TURNAROUND_USEC;
        let mut down = scenario.latency_usec + rng.exp(scenario.jitter_usec);
        if scenario.loaded(depart) && rng.uniform() < scenario.queue_prob {
            down += rng.uniform() * scenario.queue_max_usec;
        }
        let t4 = (depart + down).max(last_t4);
        last_t4 = t4;

        // Evaluate the estimate up to the moment the reply lands.
        while next_eval < t4.min(end) {
            errors.push(est.diff_at(next_eval as i64) as f64 - scenario.true_diff(next_eval));
            next_eval += EVAL_INTERVAL_USEC;
        }

        // Wire values: whole microseconds.
        let c2s = (server_clock(arrive) - t1).round() as i64;
        let s2c = (t4 - server_clock(depart)).round() as i64;
        est.add(t4 as i64, c2s, s2c);

        exchanges += 1;
        t1 += if exchanges <= QUICK_SYNCS {
            QUICK_INTERVAL_USEC
        } else {
            SYNC_INTERVAL_USEC
        };
    }
    Stats::from_errors(&errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_is_deterministic() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        let u = a.uniform();
        assert!((0.0..1.0).contains(&u));
    }

    #[test]
    fn noiseless_link_is_exact() {
        let s = Scenario {
            jitter_usec: 0.0,
            duration_s: 60.0,
            ..Scenario::base("noiseless")
        };
        let stats = run(&s, &mut TimeProvider::new());
        assert!(stats.max <= 1.0, "{stats}");
    }

    #[test]
    fn symmetric_jitter_is_unbiased() {
        let s = Scenario::base("symmetric jitter 300us");
        let stats = run(&s, &mut TimeProvider::new());
        assert!(stats.mean.abs() < 100.0, "{stats}");
        assert!(stats.p95 < 200.0, "{stats}");
    }

    /// Prints the estimation error of the plain median (C++) and of
    /// `TimeProvider` for every scenario.
    #[test]
    #[ignore = "report; run with --ignored --nocapture"]
    fn report() {
        println!("time sync estimation error vs true clock difference (us)");
        for s in scenarios() {
            println!("{} ({} s)", s.name, s.duration_s);
            println!("  plain median : {}", run(&s, &mut PlainMedian::new()));
            println!("  TimeProvider : {}", run(&s, &mut TimeProvider::new()));
        }
    }
}
