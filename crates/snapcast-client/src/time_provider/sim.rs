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
/// Start of the steady state (the skew is known), for [`run_from`].
pub(super) const STEADY_USEC: f64 = 120_000_000.0;
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
    /// Mean of the jitter added independently per direction (µs).
    pub jitter_usec: f64,
    /// Jitter is uniform in `[0, 2 * jitter_usec]` instead of exponential.
    pub uniform_jitter: bool,
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
            uniform_jitter: false,
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

    fn jitter(&self, rng: &mut Rng) -> f64 {
        if self.uniform_jitter {
            2.0 * self.jitter_usec * rng.uniform()
        } else {
            rng.exp(self.jitter_usec)
        }
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
            jitter_usec: 2_000.0,
            uniform_jitter: true,
            ..Scenario::base("uniform jitter U(0,4ms)")
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
    /// Largest change of the error between two evaluations 100 ms apart:
    /// how far the estimate jumps when it updates.
    pub step: f64,
}

impl Stats {
    pub(super) fn from_errors(errors: &[f64]) -> Self {
        assert!(!errors.is_empty());
        let mean = errors.iter().sum::<f64>() / errors.len() as f64;
        let mut abs: Vec<f64> = errors.iter().map(|e| e.abs()).collect();
        abs.sort_by(f64::total_cmp);
        let pct = |p: f64| abs[((abs.len() - 1) as f64 * p).round() as usize];
        let step = errors
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f64::max);
        Self {
            mean,
            p50: pct(0.5),
            p95: pct(0.95),
            max: abs[abs.len() - 1],
            step,
        }
    }
}

impl std::fmt::Display for Stats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "mean {:>8.1}  p50 {:>7.1}  p95 {:>7.1}  max {:>7.1}  step {:>6.1}",
            self.mean, self.p50, self.p95, self.max, self.step
        )
    }
}

/// Run `scenario` through `est`, returning the estimation error statistics.
pub(super) fn run(scenario: &Scenario, est: &mut dyn Estimator) -> Stats {
    run_from(scenario, est, WARMUP_USEC)
}

/// [`run`], sampling the error from `warmup_usec` into the run.
pub(super) fn run_from(scenario: &Scenario, est: &mut dyn Estimator, warmup_usec: f64) -> Stats {
    let mut rng = Rng::new(scenario.seed);
    let end = LOCAL_START_USEC + scenario.duration_s * 1e6;
    let server_clock = |t: f64| t + scenario.true_diff(t);

    let mut errors = Vec::new();
    let mut next_eval = LOCAL_START_USEC + warmup_usec;
    let mut t1 = LOCAL_START_USEC;
    let mut exchanges = 0u32;
    // Replies are delivered in order, as on a TCP stream.
    let mut last_t4 = 0.0f64;
    while t1 < end {
        let up = scenario.latency_usec + scenario.jitter(&mut rng);
        let arrive = t1 + up;
        let depart = arrive + SERVER_TURNAROUND_USEC;
        let mut down = scenario.latency_usec + scenario.jitter(&mut rng);
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

/// Result of [`run_playout`] (µs).
pub(super) struct Playout {
    /// Server time of the played frame minus the ideal one.
    pub error: Stats,
    /// Hard syncs after the initial one.
    pub hard_syncs: u32,
}

/// Play `scenario` through a [`Stream`](crate::stream::Stream) driven by a
/// DAC on the client clock (480 frames every 10 ms) and `est` as the time
/// provider, the server stamping 20 ms chunks on its own clock. Every frame
/// carries its index, so each played frame identifies its server time.
pub(super) fn run_playout(scenario: &Scenario, est: &mut dyn Estimator) -> Playout {
    use crate::stream::{PcmChunk, Stream};
    use snapcast_proto::SampleFormat;

    const RATE: i64 = 48_000;
    const CHUNK_FRAMES: i64 = 960;
    const DAC_FRAMES: u32 = 480;
    const BUFFER_MS: i64 = 1000;
    let format = SampleFormat::new(RATE as u32, 16, 2);
    let server_clock = |t: f64| t + scenario.true_diff(t);
    // Server time of the first chunk; chunk k covers k * 20 ms from there.
    let first_chunk = server_clock(LOCAL_START_USEC);

    let mut rng = Rng::new(scenario.seed);
    let mut stream = Stream::new(format);
    stream.set_buffer_ms(BUFFER_MS);
    let mut next_chunk = 0i64;
    let mut next_sync = LOCAL_START_USEC;
    let mut exchanges = 0u32;
    let mut buf = vec![0u8; DAC_FRAMES as usize * 4];
    let mut errors = Vec::new();
    let mut hard_syncs = 0;
    let mut was_hard_syncing = true;
    let mut t = LOCAL_START_USEC;
    let end = LOCAL_START_USEC + scenario.duration_s * 1e6;
    while t < end {
        // Chunks the server has produced by now (1 ms later on the wire).
        while first_chunk + ((next_chunk * CHUNK_FRAMES * 1_000_000) / RATE) as f64
            <= server_clock(t - 1_000.0)
        {
            let ts = first_chunk as i64 + next_chunk * CHUNK_FRAMES * 1_000_000 / RATE;
            let data: Vec<u8> = (0..CHUNK_FRAMES)
                .flat_map(|i| ((next_chunk * CHUNK_FRAMES + i + 1) as u32).to_le_bytes())
                .collect();
            stream.add_chunk(PcmChunk::new(
                snapcast_proto::Timeval::from_usec(ts),
                data,
                format,
            ));
            next_chunk += 1;
        }
        // Time exchanges on the controller's schedule; the reply lands
        // within this DAC tick.
        while next_sync <= t {
            let up = scenario.latency_usec + scenario.jitter(&mut rng);
            let mut down = scenario.latency_usec + scenario.jitter(&mut rng);
            if scenario.loaded(next_sync) && rng.uniform() < scenario.queue_prob {
                down += rng.uniform() * scenario.queue_max_usec;
            }
            let arrive = next_sync + up;
            let c2s = (server_clock(arrive) - next_sync).round() as i64;
            let s2c = (arrive + down - server_clock(arrive)).round() as i64;
            est.add((arrive + down) as i64, c2s, s2c);
            exchanges += 1;
            next_sync += if exchanges <= QUICK_SYNCS {
                QUICK_INTERVAL_USEC
            } else {
                SYNC_INTERVAL_USEC
            };
        }

        let server_now = t as i64 + est.diff_at(t as i64);
        buf.fill(0);
        let played = stream.get_player_chunk(server_now, 0, &mut buf, DAC_FRAMES);
        let hard_syncing = stream.is_hard_syncing();
        if hard_syncing && !was_hard_syncing {
            hard_syncs += 1;
        }
        was_hard_syncing = hard_syncing;
        let index = u32::from_le_bytes(buf[..4].try_into().unwrap());
        if played && index > 0 && t >= LOCAL_START_USEC + STEADY_USEC {
            let ts = first_chunk + f64::from(index - 1) * 1e6 / RATE as f64;
            let ideal = server_clock(t) - (BUFFER_MS * 1000) as f64;
            errors.push(ts - ideal);
        }
        t += f64::from(DAC_FRAMES) * 1e6 / RATE as f64;
    }
    Playout {
        error: Stats::from_errors(&errors),
        hard_syncs,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "timing; run with --release --ignored --nocapture"]
    fn update_cost_with_full_window() {
        let mut tp = TimeProvider::new();
        let mut rng = Rng::new(3);
        for i in 0..super::super::SKEW_WINDOW as i64 {
            tp.add_sample_at(i * 1_000_000, 1_000 + (rng.uniform() * 500.0) as i64, 1_000);
        }
        let start = std::time::Instant::now();
        for i in 0..100 {
            tp.add_sample_at((1_200 + i) * 1_000_000, 1_200, 1_000);
        }
        println!("add_sample_at: {:?} per update", start.elapsed() / 100);
    }

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

    fn scenario(name: &str) -> Scenario {
        scenarios()
            .into_iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no scenario {name}"))
    }

    #[test]
    fn symmetric_jitter_no_worse_than_plain_median() {
        for name in [
            "symmetric jitter 300us",
            "symmetric jitter 2ms",
            "uniform jitter U(0,4ms)",
        ] {
            let s = scenario(name);
            let plain = run(&s, &mut PlainMedian::new());
            let tp = run(&s, &mut TimeProvider::new());
            assert!(tp.p95 <= plain.p95 * 1.1, "{name}: {tp} vs {plain}");
        }
    }

    #[test]
    fn queued_replies_do_not_bias_the_estimate() {
        for name in ["s2c queuing 50% x U(0,5ms)", "bursty load 80% x U(0,10ms)"] {
            let stats = run(&scenario(name), &mut TimeProvider::new());
            assert!(
                stats.mean.abs() < 50.0 && stats.p95 < 100.0,
                "{name}: {stats}"
            );
        }
    }

    #[test]
    fn skew_does_not_lag() {
        for name in ["skew 50ppm", "skew 50ppm + s2c queuing"] {
            let s = scenario(name);
            let plain = run_from(&s, &mut PlainMedian::new(), STEADY_USEC);
            let tp = run_from(&s, &mut TimeProvider::new(), STEADY_USEC);
            assert!(plain.p50 > 4_000.0, "{name}: {plain}");
            assert!(tp.mean.abs() < 30.0 && tp.p95 < 100.0, "{name}: {tp}");
        }
    }

    #[test]
    fn estimate_never_jumps() {
        // One exchange per second may move it by at most 300 µs (plus the
        // jitter of the evaluation grid against the exchanges).
        for s in scenarios() {
            let stats = run(&s, &mut TimeProvider::new());
            assert!(stats.step < 350.0, "{}: {stats}", s.name);
        }
    }

    #[test]
    fn stream_follows_a_skewed_server_without_hard_syncs() {
        let s = Scenario {
            duration_s: 300.0,
            ..scenario("skew 50ppm")
        };
        let playout = run_playout(&s, &mut TimeProvider::new());
        assert_eq!(playout.hard_syncs, 0);
        // The stream lets up to ~100 µs build up before soft sync corrects.
        assert!(playout.error.p95 < 300.0, "{}", playout.error);
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
            let steady = |est: &mut dyn Estimator| run_from(&s, est, STEADY_USEC);
            println!("  steady state (from {} s):", STEADY_USEC / 1e6);
            println!("  plain median : {}", steady(&mut PlainMedian::new()));
            println!("  TimeProvider : {}", steady(&mut TimeProvider::new()));
            println!("  playout through Stream (from {} s):", STEADY_USEC / 1e6);
            for (name, est) in [
                (
                    "plain median",
                    &mut PlainMedian::new() as &mut dyn Estimator,
                ),
                ("TimeProvider", &mut TimeProvider::new()),
            ] {
                let p = run_playout(&s, est);
                println!("  {name} : {}  hard syncs {}", p.error, p.hard_syncs);
            }
        }
    }
}
