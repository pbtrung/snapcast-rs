//! Time sync and playout alignment, measured with the sync harness.
//!
//! The fast test checks a clean link against loose bounds. The ignored
//! `sync_report` runs the impaired-link scenarios and prints statistics:
//! `cargo test -p snapcast-tests --test sync_accuracy -- --ignored --nocapture`

use std::time::Duration;

use snapcast_tests::sync::{Link, LinkProfile, SyncRun, run_sync};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_clients_play_in_sync_on_a_clean_link() {
    let link = LinkProfile::symmetric(Link::delay(500, 200));
    let report = run_sync(SyncRun {
        links: vec![link, link],
        duration: Duration::from_secs(7),
        warmup: Duration::from_secs(3),
        buffer_ms: 1000,
        seed: 1,
        probe: false,
        drop_every: 0,
    })
    .await;
    println!("{report}");
    for c in &report.clients {
        assert!(c.ticks > 300);
        assert!(c.playout.count > c.ticks / 2, "clients play audio");
        assert!(c.diff.p95 < 2_000.0, "{}", c.diff);
        assert!(c.playout.p95 < 3_000.0, "{}", c.playout);
    }
    assert!(report.alignment[0].p95 < 3_000.0, "{}", report.alignment[0]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropped_chunks_keep_clients_aligned() {
    // One 20 ms chunk missing every second: clients must play the rest at
    // its own time (silence in the gap), not 20 ms early.
    let link = LinkProfile::symmetric(Link::delay(500, 200));
    let report = run_sync(SyncRun {
        links: vec![link, link],
        duration: Duration::from_secs(8),
        warmup: Duration::from_secs(3),
        buffer_ms: 1000,
        seed: 3,
        probe: false,
        drop_every: 50,
    })
    .await;
    println!("{report}");
    for c in &report.clients {
        assert!(c.playout.count > c.ticks / 2, "clients play audio");
        assert!(c.playout.p95 < 3_000.0, "{}", c.playout);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "report; ~1 minute; run with --ignored --nocapture"]
async fn dropped_chunks_report() {
    let clean = LinkProfile::symmetric(Link::delay(500, 200));
    let report = run_sync(SyncRun {
        links: vec![clean, clean],
        duration: Duration::from_secs(45),
        warmup: Duration::from_secs(10),
        buffer_ms: 1000,
        seed: 7,
        probe: false,
        drop_every: 250,
    })
    .await;
    println!("one 20 ms chunk dropped every 5 s, clean links\n{report}");
}

/// Scenarios of the report: client 0 always has a clean link, client 1 the
/// impaired one.
fn scenarios() -> Vec<(&'static str, LinkProfile)> {
    let clean = Link::delay(500, 200);
    vec![
        ("clean 0.5ms +-0.2ms", LinkProfile::symmetric(clean)),
        (
            "jitter 3ms +-4ms",
            LinkProfile::symmetric(Link::delay(3_000, 4_000)),
        ),
        (
            "s2c 2.5 Mbit/s, 256 KiB queue (in-network queuing)",
            LinkProfile {
                c2s: clean,
                s2c: clean.with_bandwidth(2_500_000, 256 * 1024),
            },
        ),
        (
            "s2c 1.8 Mbit/s, 8 KiB queue (backlog in server socket)",
            LinkProfile {
                c2s: clean,
                s2c: clean.with_bandwidth(1_800_000, 8 * 1024),
            },
        ),
        (
            "s2c fading 4 <-> 1.2 Mbit/s every 4 s, 4 KiB queue (bursty backlog in server socket)",
            LinkProfile {
                c2s: clean,
                s2c: clean
                    .with_bandwidth(4_000_000, 4 * 1024)
                    .with_fade(1_200_000, Duration::from_secs(4)),
            },
        ),
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "report; ~4 minutes; run with --ignored --nocapture"]
async fn sync_report() {
    let clean = LinkProfile::symmetric(Link::delay(500, 200));
    // SYNC_SCENARIO=<substring> runs only the matching scenarios.
    let filter = std::env::var("SYNC_SCENARIO").unwrap_or_default();
    for (name, link) in scenarios() {
        if !name.contains(&filter) {
            continue;
        }
        let report = run_sync(SyncRun {
            links: vec![clean, link],
            duration: Duration::from_secs(45),
            warmup: Duration::from_secs(10),
            buffer_ms: 1000,
            seed: 7,
            probe: true,
            drop_every: 0,
        })
        .await;
        println!("{name}\n{report}");
    }
}
