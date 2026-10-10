# Time sync audit

Audit of the client/server time synchronization and playout path, with
proposed fixes. Date: 2026-10-10 (at 0.999.15).

## Summary

- The clock-offset estimator (`TimeProvider`) is strong: the simulator puts
  it at about 10–50 µs in steady state (p95 about 200 µs with 1 ms jitter).
- The playout controller (`Stream` soft sync) is the bottleneck: playout
  through it is 80–290 µs off in the same simulations.
- One confirmed bug: after reconnecting to a rebooted or different server,
  the estimate stays wrong for about 55 s.
- One suspected bug: the cpal player plays one audio period early.
- None of the fixes needs a protocol change.

## Components reviewed

| Component | File |
|---|---|
| Wire format (`Time`, `Timeval`) | `crates/snapcast-proto/src/message/time.rs`, `types.rs` |
| Clock source | `crates/snapcast-proto/src/time.rs` |
| Client timestamping, sync schedule | `crates/snapcast-client/src/connection/mod.rs`, `controller.rs` |
| Server time replies | `crates/snapcast-server/src/session.rs` |
| Offset/skew estimator | `crates/snapcast-client/src/time_provider.rs` |
| Playout / soft sync | `crates/snapcast-client/src/stream.rs` |
| Output delay | `crates/snapclient-rs/src/player/mod.rs` |

## What works well

- The four-timestamp exchange (as in NTP): `c2s = t2 − t1`, `s2c = t4 − t3`.
  The server's turnaround time is excluded from the round trip.
- Receive times are taken when the read returns, not when the frame is
  parsed. The server re-stamps the reply's `sent` right before each write
  attempt.
- The server sets `TCP_NOTSENT_LOWAT` (4 KiB), so time replies don't queue
  behind a large audio backlog in the kernel, and its biased `select!`
  puts time replies ahead of audio.
- The client randomizes the sync interval (75–125 %) so requests don't keep
  landing at the same phase as the server's audio writes.
- `TimeProvider` weights exchanges by round-trip excess, fits clock skew
  with a robust Theil-Sen slope, shrinks insignificant skew toward 0, and
  limits how fast the estimate moves once settled.

## Findings

### 1. Stale history after reconnecting to a rebooted or different server (confirmed)

`TimeProvider` is shared across reconnects and never reset. History is only
cleared after 60 s without a sync, but reconnects take 1–N seconds.

Reproduced with a temporary test: 10 minutes at a 5 s offset, then a
reconnect where the offset is −3 s. The estimate stayed 8 s off for 100
samples (about 55 s). During that time it also drifted at the 500 ppm
skew clamp, because the slope fit mixed the two server clocks. Playback is
broken for the whole period.

**Fix (client only):**

- On a new connection, put the first ~5 samples on probation. If their
  median agrees with the current estimate within `JUMP_USEC` + rtt/2, keep
  the history (same server clock). Otherwise clear it and refit, which
  converges in about 0.5 s.
- Run the same check all the time: if K consecutive low-round-trip samples
  disagree with the estimate by more than `JUMP_USEC` + rtt/2, clear the
  history.

Always resetting on reconnect would also be correct, but it throws away a
good skew estimate after every brief network drop.

### 2. Suspend/resume on a Linux client is not detected

`CLOCK_MONOTONIC` does not advance during suspend, so the 60 s gap check
never triggers. The result is the same stale history as in #1. The
always-on check from #1 covers it.

### 3. Output delay includes one extra period (suspected, needs measuring)

`snapclient-rs/src/player/mod.rs:155`:

```rust
let buffer_dac_usec = (playback - callback) + num_frames * 1_000_000 / device_rate;
```

In cpal 0.18's ALSA backend, `playback = callback + avail_delay`, which is
already when the first sample of this buffer reaches the DAC. Adding
`num_frames` makes the client play one period early (often 5–20 ms). This
cancels out between clients with the same period size, but not across
different devices, backends, or C++ clients. Other cpal backends were not
checked.

**Fix:** drop the `num_frames` term after confirming with a loopback
measurement of two clients that use different buffer sizes.

### 4. Playback starts on the first time sample

For the first 50 exchanges (5 s) the estimate follows the raw fit, but the
initial hard sync uses whatever it is at that moment, possibly a single
reply that waited behind audio. A later long-term median over 2 ms then
forces an audible re-sync.

**Fix:** hold the first hard sync until about 10 samples (1 s of quick
syncs) are in.

### 5. Client receive timestamps include decode time

The controller decodes FLAC/Opus inside its receive loop. A time reply that
arrives during a decode gets its receive time only after the decode
finishes. That inflates only `s2c`, which biases the offset. Round-trip
weighting hides most of it.

**Fix:** read the socket in a separate task, as the server already does.

### 6. Minor

- `(c2s − s2c) / 2` uses integer division: up to 0.5 µs bias.
- On non-Unix systems the clock falls back to `SystemTime`, which jumps
  when NTP steps it.
- The comment in `snapcast-proto/src/time.rs` that both ends must use the
  same clock domain is too strong. Any constant offset is estimated; what
  matters is that each end is consistent with itself.

## Playout controller (largest accuracy gain)

The soft sync in `stream.rs` is ported from C++:

- No correction until `|short_median|` exceeds 100 µs.
- Proportional only, so constant drift between the sound card and the
  server clock leaves a permanent offset.
- `median` and `short_median` are only refreshed once a second, inside the
  stats-logging block.
- Corrections insert or drop whole frames (about 21 µs at 48 kHz).

**Proposed:**

- Every callback, compute `error` as a filtered age (for example the
  median of the last 20).
- `rate = 1 − (Kp·error + Ki·Σerror)`, clamped to ±500 ppm. The integral
  term learns the sound card's drift.
- Apply the rate fractionally: through the `resampler` feature, or with a
  fractional accumulator that inserts or drops one frame each time the
  accumulated correction reaches a whole frame.
- Keep hard sync for large errors (2 ms long-term median, 500 ms age).
- Clear the integral on a hard sync or format change, so a jump isn't
  learned as drift. Keep it across underruns.

**Expected:** playout error falls from 80–290 µs toward the estimator's
10–50 µs. Verify with the "playout through Stream" report in
`time_provider/sim.rs`, plus a new scenario where the sound card drifts
±50 ppm against the server.

## Simulator baseline

`cargo test -p snapcast-client --lib time_provider::sim -- --ignored --nocapture`,
steady state (from 120 s), absolute error in µs:

| Scenario | Estimator p50 / p95 | Playout through `Stream` p50 / p95 |
|---|---|---|
| Bursty load 80 % × U(0, 10 ms) | 10 / 21 | 83 / 83 |
| Skew 50 ppm | 11 / 26 | 118 / 157 |
| Skew −120 ppm, jitter 1 ms | 46 / 207 | 271 / 443 |
| Skew 50 ppm + s2c queuing | 18 / 64 | 135 / 187 |

## Further options

These need no protocol change either, but are more work:

- **Burst syncs:** send 3–4 requests back to back each second and keep the
  one with the fastest round trip. Client only; works with C++ servers.
- **Kernel receive timestamps** (`SO_TIMESTAMPING`, Linux) on both ends.
  They arrive in `CLOCK_REALTIME` and need converting to the monotonic
  clock.

Options that would change the protocol (C++ compatibility not required):

- A server clock ID in `ServerSettings`, so the client knows when to reset
  (superseded by the client-side check in #1).
- A UDP sync channel, so replies never queue behind audio in TCP. TCP stays
  as the fallback; WebSocket clients can't use UDP.

## PTP / NTP (upstream issue #1478)

Typical offset between machines:

| Clock source | Offset |
|---|---|
| Internet NTP | 1–10 ms |
| snapcast's own LAN sync | about 10–50 µs |
| LAN NTP (chrony), software timestamps | about 10–100 µs |
| LAN chrony or PTP with NIC hardware timestamps | under 1 µs to a few µs |

Internet NTP is worse than snapcast's own sync. Only hardware-timestamped
PTP or chrony beats it, and only on wired links: Wi-Fi chips don't
timestamp PTP in hardware.

**Setup:**

- NICs with hardware timestamping. Check with `ethtool -T eth0` for
  `hardware-transmit`, `hardware-receive` and a PTP Hardware Clock. Pi 5
  and CM4 have it; Pi 3 and Pi 4 don't.
- A switch: lightly loaded unmanaged works; a PTP-aware (transparent clock)
  switch is best.
- linuxptp: `ptp4l -i eth0` (server as grandmaster via a lower
  `priority1`), then `phc2sys -s eth0 -w` to steer the system clock. The
  simpler alternative is chrony with `hwtimestamp eth0`, with the server
  host as the LAN NTP server.

**snapcast-rs support:** a "trust the system clock" flag on both ends that
timestamps with `CLOCK_REALTIME` or `CLOCK_TAI`, fixes the offset at 0,
and keeps the time exchanges only as a sanity check (warn above about
1 ms). It needs a hard sync after a clock step. The wire format is
unchanged, but the timestamps mean something different, so it only works
between Rust peers that both have the flag on.

A perfect clock doesn't help until the playout fixes are in: the sound
card runs on its own crystal, which PTP doesn't discipline, so a resampler
still has to follow it.

## Suggested order

1. Reconnect/suspend detection (#1, #2).
2. Measure, then fix the output delay (#3).
3. PI playout controller with fractional rate.
4. Delay the first hard sync (#4); separate client reader task (#5).
5. Burst syncs.
6. Kernel timestamps, or the system-clock mode for PTP users, if needed.
