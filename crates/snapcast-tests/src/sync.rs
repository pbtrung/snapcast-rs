//! Sync accuracy harness.
//!
//! Runs an in-process server and several `SnapClient`s, each connected
//! through its own TCP proxy that emulates a network link: per-direction
//! latency and jitter, and an optional bandwidth limit with a bounded queue
//! (so audio can queue in front of time replies, in the proxy and, once the
//! proxy stops reading, in the server's socket).
//!
//! The server streams PCM whose samples carry a frame counter, so every
//! played frame identifies its server timestamp. A simulated DAC drives each
//! client's `Stream` in real time from one thread; at every tick all clients
//! render the same instant, which gives
//!
//! - the time sync estimation error: client and server share one clock, so
//!   the true clock difference is 0 and any estimated diff is error;
//! - the playout error: the server time of the frame each client plays
//!   minus the server time it should play (`now - buffer`);
//! - the alignment between clients: playout error relative to client 0.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use snapcast_client::{ClientCommand, ClientConfig, SnapClient};
use snapcast_proto::time::now_usec;
use snapcast_server::{AudioData, AudioFrame, ServerConfig, SnapServer};

const RATE: u32 = 48_000;
/// Frames per input chunk pushed to the server (20 ms).
const CHUNK_FRAMES: u32 = 960;
/// Frames rendered per simulated DAC tick (10 ms).
const DAC_FRAMES: u32 = 480;
/// 48000:16:2
const FRAME_SIZE: usize = 4;
/// Largest segment the emulated link transmits at once (bytes).
const SEGMENT: usize = 1448;

// ── Link emulation ────────────────────────────────────────────

/// One direction of an emulated link.
#[derive(Debug, Clone, Copy)]
pub struct Link {
    /// Fixed one-way delay.
    pub latency: Duration,
    /// Extra delay, uniform in `[0, jitter]` per segment (kept in order).
    pub jitter: Duration,
    /// Bottleneck rate in bits per second; `None` = unlimited.
    pub bandwidth_bps: Option<u64>,
    /// Fading: for the second half of every period the bottleneck drops to
    /// this rate (bits per second), as on a congested wireless link.
    pub fade: Option<(u64, Duration)>,
    /// The proxy stops reading while this many bytes wait for the link, so
    /// any further backlog stays in the sender's socket.
    pub queue_limit: usize,
}

impl Link {
    /// A link with fixed delay and jitter, in microseconds.
    pub fn delay(latency_usec: u64, jitter_usec: u64) -> Self {
        Self {
            latency: Duration::from_micros(latency_usec),
            jitter: Duration::from_micros(jitter_usec),
            bandwidth_bps: None,
            fade: None,
            queue_limit: 1 << 20,
        }
    }

    /// Limit the link to `bps`, queueing at most `queue_limit` bytes.
    pub fn with_bandwidth(self, bps: u64, queue_limit: usize) -> Self {
        Self {
            bandwidth_bps: Some(bps),
            queue_limit,
            ..self
        }
    }

    /// Drop the bandwidth to `low_bps` for half of every `period`.
    pub fn with_fade(self, low_bps: u64, period: Duration) -> Self {
        Self {
            fade: Some((low_bps, period)),
            ..self
        }
    }

    /// Bottleneck rate at `elapsed` into the connection.
    fn rate_at(&self, elapsed: Duration) -> Option<u64> {
        match self.fade {
            Some((low, period))
                if elapsed.as_nanos() % period.as_nanos() >= period.as_nanos() / 2 =>
            {
                Some(low)
            }
            _ => self.bandwidth_bps,
        }
    }
}

/// Both directions of a client's link.
#[derive(Debug, Clone, Copy)]
pub struct LinkProfile {
    /// Client to server.
    pub c2s: Link,
    /// Server to client (audio and time replies).
    pub s2c: Link,
}

impl LinkProfile {
    /// The same link in both directions.
    pub fn symmetric(link: Link) -> Self {
        Self {
            c2s: link,
            s2c: link,
        }
    }
}

/// Start a proxy to `127.0.0.1:upstream_port` emulating `profile`, returning
/// its port. Every accepted connection gets its own link. The proxy threads
/// end when either side closes.
pub fn spawn_proxy(upstream_port: u16, profile: LinkProfile, seed: u64) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for (n, client) in listener.incoming().enumerate() {
            let Ok(client) = client else { return };
            let Ok(server) = connect_upstream(upstream_port, profile.s2c.queue_limit) else {
                return;
            };
            client.set_nodelay(true).ok();
            server.set_nodelay(true).ok();
            let seed = seed.wrapping_add(n as u64 * 2);
            let (c_read, s_write) = (client.try_clone().unwrap(), server.try_clone().unwrap());
            std::thread::spawn(move || pump(c_read, s_write, profile.c2s, seed));
            std::thread::spawn(move || pump(server, client, profile.s2c, seed + 1));
        }
    });
    port
}

/// Connect to the server with a receive buffer near `queue_limit`, so that a
/// stalled proxy makes the server's socket fill instead of the proxy's.
fn connect_upstream(port: u16, queue_limit: usize) -> std::io::Result<TcpStream> {
    use socket2::{Domain, Socket, Type};
    let addr: std::net::SocketAddr = ([127, 0, 0, 1], port).into();
    let socket = Socket::new(Domain::IPV4, Type::STREAM, None)?;
    socket.set_recv_buffer_size(queue_limit.max(4096))?;
    socket.connect(&addr.into())?;
    Ok(socket.into())
}

/// Bytes accepted from the source and not yet written to the destination.
type Pending = Arc<(Mutex<usize>, Condvar)>;

/// Forward `src` to `dst` through `link`.
fn pump(mut src: TcpStream, mut dst: TcpStream, link: Link, seed: u64) {
    let (tx, rx) = std::sync::mpsc::channel::<(Instant, Vec<u8>)>();
    let pending: Pending = Arc::new((Mutex::new(0), Condvar::new()));
    let writer = {
        let pending = Arc::clone(&pending);
        std::thread::spawn(move || {
            for (at, bytes) in rx {
                sleep_until(at);
                if dst.write_all(&bytes).is_err() {
                    break;
                }
                let (lock, cvar) = &*pending;
                *lock.lock().unwrap() -= bytes.len();
                cvar.notify_all();
            }
            dst.shutdown(std::net::Shutdown::Both).ok();
        })
    };

    let mut rng = Rng(seed);
    let mut buf = vec![0u8; 64 * 1024];
    let opened = Instant::now();
    let mut link_free = opened;
    let mut last_delivery = opened;
    loop {
        {
            let (lock, cvar) = &*pending;
            let mut queued = lock.lock().unwrap();
            while *queued >= link.queue_limit {
                queued = cvar.wait(queued).unwrap();
            }
        }
        let n = match src.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let now = Instant::now();
        for segment in buf[..n].chunks(SEGMENT) {
            // Serialized on the bottleneck, then delayed.
            let start = link_free.max(now);
            link_free = start
                + link.rate_at(start - opened).map_or(Duration::ZERO, |bps| {
                    Duration::from_nanos(segment.len() as u64 * 8 * 1_000_000_000 / bps)
                });
            let jitter = link.jitter.mul_f64(rng.uniform());
            let at = (link_free + link.latency + jitter).max(last_delivery);
            last_delivery = at;
            *pending.0.lock().unwrap() += segment.len();
            if tx.send((at, segment.to_vec())).is_err() {
                break;
            }
        }
    }
    drop(tx);
    writer.join().ok();
    src.shutdown(std::net::Shutdown::Both).ok();
}

/// Sleep until `at` with sub-100 µs accuracy (sleep, then spin).
fn sleep_until(at: Instant) {
    loop {
        let now = Instant::now();
        if now >= at {
            return;
        }
        let left = at - now;
        if left > Duration::from_micros(300) {
            std::thread::sleep(left - Duration::from_micros(200));
        } else {
            std::hint::spin_loop();
        }
    }
}

/// splitmix64, for reproducible jitter.
struct Rng(u64);

impl Rng {
    fn uniform(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
    }
}

// ── Statistics ────────────────────────────────────────────────

/// Error statistics in microseconds.
#[derive(Debug, Clone, Copy, Default)]
pub struct Stats {
    /// Number of samples.
    pub count: usize,
    /// Mean signed error (bias).
    pub mean: f64,
    /// Median absolute error.
    pub p50: f64,
    /// 95th percentile of the absolute error.
    pub p95: f64,
    /// Largest absolute error.
    pub max: f64,
}

impl Stats {
    /// Statistics over signed errors; all zero when empty.
    pub fn from_errors(errors: &[f64]) -> Self {
        if errors.is_empty() {
            return Self::default();
        }
        let mean = errors.iter().sum::<f64>() / errors.len() as f64;
        let mut abs: Vec<f64> = errors.iter().map(|e| e.abs()).collect();
        abs.sort_by(f64::total_cmp);
        let pct = |p: f64| abs[((abs.len() - 1) as f64 * p).round() as usize];
        Self {
            count: errors.len(),
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
            "mean {:>8.1}  p50 {:>7.1}  p95 {:>7.1}  max {:>8.1}  (n={})",
            self.mean, self.p50, self.p95, self.max, self.count
        )
    }
}

// ── Sync run ──────────────────────────────────────────────────

/// What to run.
#[derive(Debug, Clone)]
pub struct SyncRun {
    /// One link per client.
    pub links: Vec<LinkProfile>,
    /// Total run time.
    pub duration: Duration,
    /// Samples before this are discarded (time sync and hard sync settle).
    pub warmup: Duration,
    /// Server buffer.
    pub buffer_ms: u32,
    /// Seed for the links' jitter.
    pub seed: u64,
    /// Also probe raw time exchanges over a copy of the last client's link.
    pub probe: bool,
    /// Leave out every n-th 20 ms chunk at the source, timestamps going on
    /// as when the server drops chunks (0 = never).
    pub drop_every: u32,
}

/// Results of one client (µs).
#[derive(Debug, Clone, Copy)]
pub struct ClientReport {
    /// Estimated clock difference (the true one is 0).
    pub diff: Stats,
    /// Server time of the played frame minus the ideal one.
    pub playout: Stats,
    /// DAC ticks after warm-up.
    pub ticks: usize,
}

/// Results of a [`SyncRun`].
#[derive(Debug, Clone)]
pub struct SyncReport {
    /// Per client, in [`SyncRun::links`] order.
    pub clients: Vec<ClientReport>,
    /// Playout error of client `i` minus that of client 0, for `i >= 1`.
    pub alignment: Vec<Stats>,
    /// Raw exchanges of the probe, if run.
    pub probe: Option<ProbeReport>,
}

/// Unfiltered time exchanges (µs), as `TimeProvider` receives them.
#[derive(Debug, Clone, Copy)]
pub struct ProbeReport {
    /// Per-exchange diff `(c2s - s2c) / 2`; the true diff is 0.
    pub diff: Stats,
    /// Per-exchange round trip `c2s + s2c`.
    pub rtt: Stats,
    /// Diff of the fifth of the exchanges with the lowest round trips.
    pub low_rtt_diff: Stats,
}

impl std::fmt::Display for SyncReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, c) in self.clients.iter().enumerate() {
            writeln!(f, "  client {i} time diff : {}", c.diff)?;
            writeln!(f, "  client {i} playout   : {}", c.playout)?;
        }
        for (i, a) in self.alignment.iter().enumerate() {
            writeln!(f, "  client {} vs 0 align: {a}", i + 1)?;
        }
        if let Some(p) = &self.probe {
            writeln!(f, "  raw exchange diff  : {}", p.diff)?;
            writeln!(f, "  raw exchange rtt   : {}", p.rtt)?;
            writeln!(f, "  raw low-rtt diff   : {}", p.low_rtt_diff)?;
        }
        Ok(())
    }
}

/// One frame of the counter signal: the 32-bit value `n + 1` split over the
/// two 16-bit channels (0 stays reserved for silence).
fn counter_frame(n: u32) -> [u8; FRAME_SIZE] {
    (n + 1).to_le_bytes()
}

fn decode_counter(frame: &[u8]) -> Option<u32> {
    let v = u32::from_le_bytes(frame[..FRAME_SIZE].try_into().unwrap());
    v.checked_sub(1)
}

/// Run the server, the clients and the simulated DAC for `run.duration`.
pub async fn run_sync(run: SyncRun) -> SyncReport {
    let config = ServerConfig {
        codec: snapcast_proto::CODEC_PCM.into(),
        buffer_ms: run.buffer_ms,
        ..ServerConfig::default()
    };
    let (mut server, _events) = SnapServer::new(config);
    let audio_tx = server.add_stream("default");
    let port = crate::spawn_serving(server).await;

    // Counter-signal source, paced in real time and stamped like the server's
    // own timestamper: chunk k plays at start + k * 20 ms (+ buffer).
    let start_usec = now_usec();
    let start = tokio::time::Instant::now();
    let drop_every = run.drop_every;
    let feeder = tokio::spawn(async move {
        for k in 0u32.. {
            if drop_every > 0 && k > 0 && k % drop_every == 0 {
                let next = Duration::from_micros(u64::from(k + 1) * 20_000);
                tokio::time::sleep_until(start + next).await;
                continue;
            }
            let mut pcm = Vec::with_capacity(CHUNK_FRAMES as usize * FRAME_SIZE);
            for i in 0..CHUNK_FRAMES {
                pcm.extend_from_slice(&counter_frame(k * CHUNK_FRAMES + i));
            }
            let frame = AudioFrame {
                data: AudioData::Pcm(pcm),
                timestamp_usec: start_usec + i64::from(k * CHUNK_FRAMES) * 1_000_000 / 48_000,
            };
            if audio_tx.send(frame).await.is_err() {
                return;
            }
            let next = Duration::from_micros(u64::from(k + 1) * 20_000);
            tokio::time::sleep_until(start + next).await;
        }
    });

    let mut clients = Vec::new();
    for (i, link) in run.links.iter().enumerate() {
        let proxy_port = spawn_proxy(port, *link, run.seed.wrapping_add(i as u64 * 1000));
        let config = ClientConfig {
            host: "127.0.0.1".into(),
            port: proxy_port,
            host_id: format!("sync-{i}"),
            ..ClientConfig::default()
        };
        // Dropping the receivers: nothing reads events or decoded audio.
        let (mut client, _events, _audio) = SnapClient::new(config);
        let handles = (
            Arc::clone(&client.time_provider),
            Arc::clone(&client.stream),
            client.command_sender(),
        );
        tokio::spawn(async move {
            client.run().await.ok();
        });
        clients.push(handles);
    }

    let probe = match (run.probe, run.links.last()) {
        (true, Some(link)) => {
            let proxy_port = spawn_proxy(port, *link, run.seed.wrapping_add(999));
            Some(tokio::spawn(probe(proxy_port, run.duration, run.warmup)))
        }
        _ => None,
    };

    let dac_clients: Vec<_> = clients
        .iter()
        .map(|(tp, stream, _)| (Arc::clone(tp), Arc::clone(stream)))
        .collect();
    let buffer_usec = i64::from(run.buffer_ms) * 1000;
    let (duration, warmup) = (run.duration, run.warmup);
    let samples = tokio::task::spawn_blocking(move || {
        dac_loop(&dac_clients, duration, warmup, buffer_usec, start_usec)
    })
    .await
    .unwrap();

    for (_, _, cmd) in &clients {
        cmd.send(ClientCommand::Stop).await.ok();
    }
    let probe = match probe {
        Some(handle) => Some(handle.await.unwrap()),
        None => None,
    };
    feeder.abort();

    let mut alignment = Vec::new();
    for i in 1..samples.len() {
        let errors: Vec<f64> = samples[0]
            .playout
            .iter()
            .zip(&samples[i].playout)
            .filter_map(|(a, b)| Some(b.as_ref()? - a.as_ref()?))
            .collect();
        alignment.push(Stats::from_errors(&errors));
    }
    let clients = samples
        .iter()
        .map(|s| ClientReport {
            diff: Stats::from_errors(&s.diff),
            playout: Stats::from_errors(&s.playout.iter().flatten().copied().collect::<Vec<_>>()),
            ticks: s.diff.len(),
        })
        .collect();
    SyncReport {
        clients,
        alignment,
        probe,
    }
}

/// A bare client sending a Time request about every 100 ms and recording the raw
/// exchanges. Reads run in their own task and stamp arrival when the read
/// returns, as the client does.
async fn probe(port: u16, duration: Duration, warmup: Duration) -> ProbeReport {
    use snapcast_proto::message::factory::{self, MessagePayload};
    use snapcast_proto::message::hello::Hello;
    use snapcast_proto::message::time::Time;
    use snapcast_proto::{BaseMessage, MessageType, Timeval};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    stream.set_nodelay(true).unwrap();
    let (mut rd, mut wr) = stream.into_split();
    let frame = |msg_type, payload: &MessagePayload| {
        let mut base = BaseMessage {
            msg_type,
            id: 1,
            refers_to: 0,
            sent: Timeval::from_usec(now_usec()),
            received: Timeval::default(),
            size: 0,
        };
        factory::serialize(&mut base, payload).unwrap()
    };
    let hello = Hello {
        mac: "00:00:00:00:00:00".into(),
        host_name: "probe".into(),
        version: "0.0.0".into(),
        client_name: "probe".into(),
        os: "test".into(),
        arch: "test".into(),
        instance: 1,
        id: "probe".into(),
        snap_stream_protocol_version: snapcast_proto::PROTOCOL_VERSION,
        auth: None,
    };
    wr.write_all(&frame(MessageType::Hello, &MessagePayload::Hello(hello)))
        .await
        .unwrap();

    let begin = tokio::time::Instant::now();
    let samples = Arc::new(Mutex::new(Vec::new()));
    let reader = tokio::spawn({
        let samples = Arc::clone(&samples);
        async move {
            let mut buf = Vec::new();
            let mut read_at = 0;
            loop {
                while let Some(msg) = factory::take_frame(&mut buf).unwrap() {
                    if let MessagePayload::Time(t) = msg.payload
                        && begin.elapsed() > warmup
                    {
                        let c2s = t.latency.to_usec();
                        let s2c = read_at - msg.base.sent.to_usec();
                        let sample = ((c2s - s2c) as f64 / 2.0, (c2s + s2c) as f64);
                        samples.lock().unwrap().push(sample);
                    }
                }
                buf.reserve(64 * 1024);
                match rd.read_buf(&mut buf).await {
                    Ok(n) if n > 0 => read_at = now_usec(),
                    _ => return,
                }
            }
        }
    });
    // Randomized like the client's schedule (75-125 ms), so requests don't
    // phase-lock to the audio chunks.
    let mut rng = Rng(0x5eed);
    while begin.elapsed() < duration {
        let pause = Duration::from_millis(75).mul_f64(1.0 + rng.uniform() * 2.0 / 3.0);
        tokio::time::sleep(pause).await;
        let req = frame(MessageType::Time, &MessagePayload::Time(Time::new()));
        if wr.write_all(&req).await.is_err() {
            break;
        }
    }
    drop(wr);
    reader.abort();
    let mut samples = samples.lock().unwrap().clone();
    let (diffs, rtts): (Vec<f64>, Vec<f64>) = samples.iter().copied().unzip();
    samples.sort_by(|a, b| a.1.total_cmp(&b.1));
    samples.truncate(samples.len().div_ceil(5));
    let low_rtt: Vec<f64> = samples.iter().map(|s| s.0).collect();
    ProbeReport {
        diff: Stats::from_errors(&diffs),
        rtt: Stats::from_errors(&rtts),
        low_rtt_diff: Stats::from_errors(&low_rtt),
    }
}

/// Per-client samples, one per DAC tick after warm-up.
#[derive(Default)]
struct Samples {
    diff: Vec<f64>,
    /// `None` when the tick played no counter frame (silence, underrun).
    playout: Vec<Option<f64>>,
}

type ClientHandles = (
    Arc<std::sync::Mutex<snapcast_client::time_provider::TimeProvider>>,
    Arc<std::sync::Mutex<snapcast_client::stream::Stream>>,
);

/// Render every client every 10 ms of real time, all at the same instant.
fn dac_loop(
    clients: &[ClientHandles],
    duration: Duration,
    warmup: Duration,
    buffer_usec: i64,
    start_usec: i64,
) -> Vec<Samples> {
    let mut samples: Vec<Samples> = clients.iter().map(|_| Samples::default()).collect();
    let mut buf = vec![0u8; DAC_FRAMES as usize * FRAME_SIZE];
    // The DAC's sample clock: tick n plays at begin + n * 10 ms, however
    // late the thread wakes up.
    let begin = Instant::now();
    let begin_usec = now_usec();
    let tick = Duration::from_micros(u64::from(DAC_FRAMES) * 1_000_000 / u64::from(RATE));
    for n in 1u32.. {
        let at = begin + tick * n;
        if at - begin > duration {
            break;
        }
        sleep_until(at);
        let now = begin_usec + (tick * n).as_micros() as i64;
        let record = at - begin > warmup;
        for ((tp, stream), out) in clients.iter().zip(&mut samples) {
            let diff = tp.lock().unwrap().diff_to_server_usec();
            buf.fill(0);
            let played =
                stream
                    .lock()
                    .unwrap()
                    .get_player_chunk(now + diff, 0, &mut buf, DAC_FRAMES);
            if !record {
                continue;
            }
            out.diff.push(diff as f64);
            let ideal = now - buffer_usec - start_usec;
            out.playout.push(
                decode_counter(&buf)
                    .filter(|_| played)
                    .map(|c| c as f64 * 1e6 / f64::from(RATE) - ideal as f64),
            );
        }
    }
    samples
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_round_trips() {
        for n in [0, 1, 65_535, 65_536, 1 << 30] {
            assert_eq!(decode_counter(&counter_frame(n)), Some(n));
        }
        assert_eq!(decode_counter(&[0; 4]), None);
    }

    #[test]
    fn stats_percentiles() {
        let s = Stats::from_errors(&[-4.0, 1.0, 2.0, 3.0]);
        assert_eq!(s.mean, 0.5);
        assert_eq!(s.max, 4.0);
        assert_eq!(Stats::from_errors(&[]).count, 0);
    }

    #[test]
    fn proxy_delays_and_forwards() {
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let up_port = upstream.local_addr().unwrap().port();
        let port = spawn_proxy(up_port, LinkProfile::symmetric(Link::delay(20_000, 0)), 1);
        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (mut server, _) = upstream.accept().unwrap();
        let t = Instant::now();
        client.write_all(b"ping").unwrap();
        let mut got = [0u8; 4];
        server.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"ping");
        assert!(t.elapsed() >= Duration::from_millis(20));
    }
}
