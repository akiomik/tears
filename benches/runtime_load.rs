//! Full-loop runtime load harness for the S4 runtime load control RFC.
//!
//! Unlike `benches/subscription.rs`, which isolates the reconciliation hot
//! path, this harness drives the real [`Runtime`] end-to-end through the
//! public API: a subscription floods the shared message channel at a
//! configured rate while the application simulates per-message update work
//! and per-frame render work. It observes the load characteristics that
//! throughput numbers alone hide:
//!
//! - **Queue depth**: pending messages (produced - processed), sampled while
//!   the scenario runs; all runtime channels are unbounded today, so this is
//!   the direct driver of memory growth under overload.
//! - **Update latency**: message emission to `Application::update`.
//! - **Render latency**: message emission to the first `Application::view`
//!   call that observes it (input-to-screen staleness).
//! - **Keyed delivery latency**: emission to update for outputs of a
//!   cancellable (keyed) command while the shared channel is loaded, to
//!   expose ordering bias between the shared and keyed input paths.
//! - **Memory**: peak RSS delta per scenario plus an estimate of the backlog
//!   footprint from the maximum observed queue depth.
//!
//! Run all scenarios, or name a subset:
//!
//! ```bash
//! cargo bench --bench runtime_load
//! cargo bench --bench runtime_load -- overload keyed_overload
//! ```
//!
//! Peak RSS is process-wide and monotone, so for clean memory numbers run a
//! single scenario per invocation.

// Metric reporting converts counters and nanosecond values to floating point
// for human-readable output; precision loss there is irrelevant.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::num::NonZeroU32;
use std::time::{Duration, Instant};
use std::{env, hint, mem};

use futures::stream::{self, StreamExt};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tears::command::CommandId;
use tears::prelude::*;
use tears::{BoxStream, SubscriptionSource};
use tokio::runtime::Builder;
use tokio::time::{MissedTickBehavior, interval, timeout};
use tokio_stream::wrappers::IntervalStream;

/// Message rate for scenarios that emit their whole load in one burst.
const BURST: u64 = 0;

#[derive(Clone)]
struct ScenarioCfg {
    name: &'static str,
    /// Messages per second, or [`BURST`] to emit everything at once.
    rate: u64,
    /// Total number of flood messages to emit.
    total: u64,
    /// Simulated CPU cost per `update` call.
    update_cost: Duration,
    /// Simulated CPU cost per `view` call.
    render_cost: Duration,
    /// Whether to run a keyed command emitting probe messages every 25ms.
    keyed_probe: bool,
    /// Wall-clock guard; the scenario is aborted and reported as timed out.
    max_wall: Duration,
}

fn scenarios() -> Vec<ScenarioCfg> {
    vec![
        // Paced load well below consumer capacity: the baseline contract.
        ScenarioCfg {
            name: "steady_20k",
            rate: 20_000,
            total: 100_000,
            update_cost: Duration::from_micros(2),
            render_cost: Duration::from_micros(500),
            keyed_probe: false,
            max_wall: Duration::from_secs(30),
        },
        // Paced load near consumer capacity.
        ScenarioCfg {
            name: "steady_200k",
            rate: 200_000,
            total: 1_000_000,
            update_cost: Duration::from_micros(2),
            render_cost: Duration::from_micros(500),
            keyed_probe: false,
            max_wall: Duration::from_secs(30),
        },
        // One burst dumped into the unbounded channel at t=0; measures drain
        // behavior and worst-case backlog for a spike.
        ScenarioCfg {
            name: "burst_200k",
            rate: BURST,
            total: 200_000,
            update_cost: Duration::from_micros(2),
            render_cost: Duration::from_micros(500),
            keyed_probe: false,
            max_wall: Duration::from_secs(30),
        },
        // Sustained producer faster than the consumer: queue depth and
        // latency must grow without bound while the producer runs.
        ScenarioCfg {
            name: "overload",
            rate: 100_000,
            total: 500_000,
            update_cost: Duration::from_micros(25),
            render_cost: Duration::from_micros(500),
            keyed_probe: false,
            max_wall: Duration::from_secs(60),
        },
        // Control: keyed command outputs while the shared channel is lightly
        // loaded.
        ScenarioCfg {
            name: "keyed_steady",
            rate: 20_000,
            total: 100_000,
            update_cost: Duration::from_micros(2),
            render_cost: Duration::from_micros(500),
            keyed_probe: true,
            max_wall: Duration::from_secs(30),
        },
        // Keyed command outputs while the shared channel is overloaded; the
        // input mux polls the shared channel first, so this measures how much
        // a shared-channel backlog delays keyed delivery.
        ScenarioCfg {
            name: "keyed_overload",
            rate: 100_000,
            total: 500_000,
            update_cost: Duration::from_micros(25),
            render_cost: Duration::from_micros(500),
            keyed_probe: true,
            max_wall: Duration::from_secs(60),
        },
    ]
}

struct Metrics {
    start: Instant,
    produced: AtomicU64,
    processed: AtomicU64,
    /// Nanoseconds from `start` at which the producer emitted its last
    /// message; 0 while still producing.
    producer_done_ns: AtomicU64,
    frames: AtomicU64,
    /// `seq + 1` of the newest flood message observed by `view`; 0 if none.
    rendered_marker: AtomicU64,
    update_lat_ns: Mutex<Vec<u64>>,
    render_lat_ns: Mutex<Vec<u64>>,
    keyed_lat_ns: Mutex<Vec<u64>>,
}

impl Metrics {
    fn new() -> Self {
        Self {
            start: Instant::now(),
            produced: AtomicU64::new(0),
            processed: AtomicU64::new(0),
            producer_done_ns: AtomicU64::new(0),
            frames: AtomicU64::new(0),
            rendered_marker: AtomicU64::new(0),
            update_lat_ns: Mutex::new(Vec::new()),
            render_lat_ns: Mutex::new(Vec::new()),
            keyed_lat_ns: Mutex::new(Vec::new()),
        }
    }

    fn queue_depth(&self) -> u64 {
        let produced = self.produced.load(Ordering::Relaxed);
        let processed = self.processed.load(Ordering::Relaxed);
        produced.saturating_sub(processed)
    }

    fn push_latency(bucket: &Mutex<Vec<u64>>, sent_at: Instant) {
        let nanos = u64::try_from(sent_at.elapsed().as_nanos()).unwrap_or(u64::MAX);
        bucket.lock().expect("latency bucket poisoned").push(nanos);
    }
}

enum Msg {
    Load { seq: u64, sent_at: Instant },
    KeyedProbe { sent_at: Instant },
}

/// Emits `total` flood messages, paced by `rate`, then stays pending forever
/// so the subscription task is never reaped and restarted by reconciliation.
struct FloodSource {
    cfg: ScenarioCfg,
    metrics: Arc<Metrics>,
}

impl SubscriptionSource for FloodSource {
    type Output = Msg;
    type Key = &'static str;

    fn stream(&self) -> BoxStream<'static, Msg> {
        let metrics = Arc::clone(&self.metrics);
        let total = self.cfg.total;
        let per_tick = if self.cfg.rate == BURST {
            usize::try_from(total).expect("total fits in usize")
        } else {
            // 1ms ticks; keep at least one message per tick.
            usize::try_from((self.cfg.rate / 1_000).max(1)).expect("per-tick fits in usize")
        };

        let mut ticker = interval(Duration::from_millis(1));
        // Catch up after missed ticks so the configured rate holds on average.
        ticker.set_missed_tick_behavior(MissedTickBehavior::Burst);

        IntervalStream::new(ticker)
            .flat_map(move |_| {
                let metrics = Arc::clone(&metrics);
                stream::iter((0..per_tick).map(move |_| {
                    let seq = metrics.produced.fetch_add(1, Ordering::Relaxed);
                    if seq + 1 == total {
                        let elapsed =
                            u64::try_from(metrics.start.elapsed().as_nanos()).unwrap_or(u64::MAX);
                        metrics.producer_done_ns.store(elapsed, Ordering::Relaxed);
                    }
                    Msg::Load {
                        seq,
                        sent_at: Instant::now(),
                    }
                }))
            })
            .take(usize::try_from(total).expect("total fits in usize"))
            .chain(stream::pending())
            .boxed()
    }

    fn key(&self) -> Self::Key {
        "flood"
    }
}

struct LoadApp {
    cfg: ScenarioCfg,
    metrics: Arc<Metrics>,
    /// Newest flood message applied to state, for render staleness.
    last_processed: Option<(u64, Instant)>,
    processed: u64,
    /// Record every Nth update latency to bound sample memory.
    sample_every: u64,
}

impl Application for LoadApp {
    type Message = Msg;
    type Flags = (ScenarioCfg, Arc<Metrics>);

    fn new((cfg, metrics): Self::Flags) -> (Self, Command<Msg>) {
        let cmd = if cfg.keyed_probe {
            let mut ticker = interval(Duration::from_millis(25));
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            let probes = IntervalStream::new(ticker).map(|_| Msg::KeyedProbe {
                sent_at: Instant::now(),
            });
            Command::stream(probes).cancellable(CommandId::new("keyed-probe"))
        } else {
            Command::none()
        };

        let sample_every = (cfg.total / 100_000).max(1);
        (
            Self {
                cfg,
                metrics,
                last_processed: None,
                processed: 0,
                sample_every,
            },
            cmd,
        )
    }

    fn update(&mut self, msg: Msg) -> Command<Msg> {
        match msg {
            Msg::Load { seq, sent_at } => {
                spin(self.cfg.update_cost);
                if seq % self.sample_every == 0 {
                    Metrics::push_latency(&self.metrics.update_lat_ns, sent_at);
                }
                self.last_processed = Some((seq, sent_at));
                self.processed += 1;
                self.metrics.processed.store(self.processed, Ordering::Relaxed);
                if self.processed == self.cfg.total {
                    return Command::quit();
                }
                Command::none()
            }
            Msg::KeyedProbe { sent_at } => {
                Metrics::push_latency(&self.metrics.keyed_lat_ns, sent_at);
                Command::none()
            }
        }
    }

    fn view(&self, _frame: &mut ratatui::Frame<'_>) {
        spin(self.cfg.render_cost);
        self.metrics.frames.fetch_add(1, Ordering::Relaxed);
        if let Some((seq, sent_at)) = self.last_processed {
            let marker = seq + 1;
            // Only the runtime task calls `view`, so load-then-store is safe.
            if self.metrics.rendered_marker.load(Ordering::Relaxed) < marker {
                self.metrics.rendered_marker.store(marker, Ordering::Relaxed);
                Metrics::push_latency(&self.metrics.render_lat_ns, sent_at);
            }
        }
    }

    fn subscriptions(&self) -> Vec<Subscription<Msg>> {
        vec![Subscription::new(FloodSource {
            cfg: self.cfg.clone(),
            metrics: Arc::clone(&self.metrics),
        })]
    }
}

/// Busy-waits for `duration` to simulate CPU-bound work on the runtime task.
fn spin(duration: Duration) {
    if duration.is_zero() {
        return;
    }
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        hint::spin_loop();
    }
}

struct Report {
    cfg: ScenarioCfg,
    wall: Duration,
    timed_out: bool,
    produced: u64,
    processed: u64,
    frames: u64,
    max_depth: u64,
    depth_at_producer_done: Option<u64>,
    producer_done: Option<Duration>,
    update_lat_ns: Vec<u64>,
    render_lat_ns: Vec<u64>,
    keyed_lat_ns: Vec<u64>,
    peak_rss_delta: Option<u64>,
}

async fn run_scenario(cfg: ScenarioCfg) -> Report {
    let rss_before = peak_rss_bytes();
    let metrics = Arc::new(Metrics::new());

    // Queue depth sampler; runs until the scenario finishes.
    let stop = Arc::new(AtomicBool::new(false));
    let sampler = {
        let metrics = Arc::clone(&metrics);
        let stop = Arc::clone(&stop);
        tokio::spawn(async move {
            let mut samples: Vec<(Duration, u64)> = Vec::new();
            let mut ticker = interval(Duration::from_millis(5));
            while !stop.load(Ordering::Relaxed) {
                ticker.tick().await;
                samples.push((metrics.start.elapsed(), metrics.queue_depth()));
            }
            samples
        })
    };

    let frame_rate = FrameRate::new(NonZeroU32::new(60).expect("non-zero fps"))
        .expect("60 FPS is a valid frame rate");
    let runtime = Runtime::<LoadApp>::new((cfg.clone(), Arc::clone(&metrics)), frame_rate);
    let mut terminal =
        Terminal::new(TestBackend::new(120, 40)).expect("test backend terminal creation");

    let started = Instant::now();
    let timed_out = timeout(cfg.max_wall, runtime.run(&mut terminal))
        .await
        .is_err();
    let wall = started.elapsed();

    stop.store(true, Ordering::Relaxed);
    let samples = sampler.await.expect("sampler task");

    let producer_done_ns = metrics.producer_done_ns.load(Ordering::Relaxed);
    let producer_done =
        (producer_done_ns > 0).then(|| Duration::from_nanos(producer_done_ns));
    let depth_at_producer_done = producer_done.and_then(|done| {
        samples
            .iter()
            .find(|(at, _)| *at >= done)
            .map(|(_, depth)| *depth)
    });

    let take_sorted = |bucket: &Mutex<Vec<u64>>| {
        let mut values = bucket.lock().expect("latency bucket poisoned").clone();
        values.sort_unstable();
        values
    };

    Report {
        wall,
        timed_out,
        produced: metrics.produced.load(Ordering::Relaxed),
        processed: metrics.processed.load(Ordering::Relaxed),
        frames: metrics.frames.load(Ordering::Relaxed),
        max_depth: samples.iter().map(|(_, depth)| *depth).max().unwrap_or(0),
        depth_at_producer_done,
        producer_done,
        update_lat_ns: take_sorted(&metrics.update_lat_ns),
        render_lat_ns: take_sorted(&metrics.render_lat_ns),
        keyed_lat_ns: take_sorted(&metrics.keyed_lat_ns),
        peak_rss_delta: match (rss_before, peak_rss_bytes()) {
            (Some(before), Some(after)) => Some(after.saturating_sub(before)),
            _ => None,
        },
        cfg,
    }
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = ((sorted.len() as f64) * p).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

fn format_lat(sorted: &[u64]) -> String {
    if sorted.is_empty() {
        return "n/a".to_owned();
    }
    let ms = |ns: u64| ns as f64 / 1_000_000.0;
    format!(
        "p50={:.3}ms p95={:.3}ms p99={:.3}ms max={:.3}ms (n={})",
        ms(percentile(sorted, 0.50)),
        ms(percentile(sorted, 0.95)),
        ms(percentile(sorted, 0.99)),
        ms(*sorted.last().expect("non-empty")),
        sorted.len(),
    )
}

fn print_report(report: &Report) {
    let cfg = &report.cfg;
    println!("## {}", cfg.name);
    let rate = if cfg.rate == BURST {
        "burst".to_owned()
    } else {
        format!("{}/s", cfg.rate)
    };
    println!(
        "   load: rate={rate} total={} update_cost={:?} render_cost={:?} keyed_probe={}",
        cfg.total, cfg.update_cost, cfg.render_cost, cfg.keyed_probe
    );
    let status = if report.timed_out { "TIMED OUT" } else { "ok" };
    println!(
        "   run: {status} wall={:.2}s produced={} processed={} throughput={:.0}/s",
        report.wall.as_secs_f64(),
        report.produced,
        report.processed,
        report.processed as f64 / report.wall.as_secs_f64(),
    );
    println!(
        "   frames: {} ({:.1} fps effective)",
        report.frames,
        report.frames as f64 / report.wall.as_secs_f64(),
    );
    let backlog_bytes = report.max_depth * mem::size_of::<Msg>() as u64;
    println!(
        "   queue: max_depth={} (~{:.1} MiB backlog)",
        report.max_depth,
        backlog_bytes as f64 / (1024.0 * 1024.0),
    );
    if let (Some(done), Some(depth)) = (report.producer_done, report.depth_at_producer_done) {
        let drain = report.wall.saturating_sub(done);
        println!(
            "   producer done at {:.2}s: depth={depth} drain={:.2}s",
            done.as_secs_f64(),
            drain.as_secs_f64(),
        );
    }
    println!("   update latency: {}", format_lat(&report.update_lat_ns));
    println!("   render latency: {}", format_lat(&report.render_lat_ns));
    if cfg.keyed_probe {
        println!("   keyed latency:  {}", format_lat(&report.keyed_lat_ns));
    }
    if let Some(delta) = report.peak_rss_delta {
        println!(
            "   peak RSS delta: {:.1} MiB (process-wide, monotone across scenarios)",
            delta as f64 / (1024.0 * 1024.0),
        );
    }
    println!();
}

#[cfg(unix)]
fn peak_rss_bytes() -> Option<u64> {
    let mut usage = mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `usage` points to writable memory of the correct type, and
    // `getrusage` only writes into it.
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    // SAFETY: `getrusage` returned 0, so the struct is initialized.
    let usage = unsafe { usage.assume_init() };
    let raw = u64::try_from(usage.ru_maxrss).ok()?;
    // macOS reports bytes; Linux reports kilobytes.
    if cfg!(target_os = "macos") {
        Some(raw)
    } else {
        Some(raw * 1024)
    }
}

#[cfg(not(unix))]
fn peak_rss_bytes() -> Option<u64> {
    None
}

fn main() {
    // Positional arguments select scenarios by name; flags (e.g. the
    // `--bench` cargo passes) are ignored.
    let selected: Vec<String> = env::args()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .collect();

    let to_run: Vec<ScenarioCfg> = scenarios()
        .into_iter()
        .filter(|cfg| selected.is_empty() || selected.iter().any(|name| name == cfg.name))
        .collect();
    if to_run.is_empty() {
        let names: Vec<&str> = scenarios().iter().map(|cfg| cfg.name).collect();
        println!("no matching scenario; available: {}", names.join(", "));
        return;
    }

    let runtime = Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    println!("# tears runtime load harness\n");
    for cfg in to_run {
        let report = runtime.block_on(run_scenario(cfg));
        print_report(&report);
    }
}
