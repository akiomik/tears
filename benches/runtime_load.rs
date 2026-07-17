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
//! # Quit-latency scenarios (`quit_*`)
//!
//! The `quit_*` scenarios measure quit responsiveness under a shared-channel
//! backlog (RFC 0006 INV-L4 / open question 8). Each one runs many short
//! trials: a trial floods the shared channel, then `update` returns
//! [`Command::quit`] while the backlog is still deep, mirroring how a real
//! key press quits an application. Because the event loop's `select!` is
//! unbiased, a single run says nothing about the tail; the report therefore
//! aggregates per-trial values into percentiles across trials:
//!
//! - **quit -> last work**: quit request to the end of the last `update` or
//!   `view` call — a lower bound on quit *delivery* (the loop breaks right
//!   after the last work item, before teardown).
//! - **quit -> exit**: quit request to `Runtime::run` returning — an upper
//!   bound that additionally includes shutdown and backlog deallocation,
//!   which scales with queue depth and must not be misread as delivery
//!   latency.
//!
//! The keyed variant sends the quit through a cancellable command's private
//! channel instead of the dedicated quit channel, quantifying the INV-14
//! shared-first bias for in-band quit (RFC 0006 open question 7).
//!
//! Run all scenarios, or name a subset:
//!
//! ```bash
//! cargo bench --bench runtime_load
//! cargo bench --bench runtime_load -- overload keyed_overload
//! cargo bench --bench runtime_load -- quit_idle quit_backlog_300k
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

use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    /// Have `update` return [`Command::quit`] when it processes the flood
    /// message with this seq, instead of quitting at `total`. The flood keeps
    /// the backlog deep past this point, so the quit races a loaded loop.
    quit_at_seq: Option<u64>,
    /// Route the triggered quit through a keyed (cancellable) command, i.e.
    /// the command's private channel, instead of an unkeyed command's direct
    /// send to the dedicated quit channel.
    keyed_quit: bool,
    /// Wall-clock guard; the scenario is aborted and reported as timed out.
    max_wall: Duration,
}

/// A quit-latency scenario: `base` is run `trials` times and per-trial quit
/// latencies are aggregated into one report.
struct QuitScenarioCfg {
    base: ScenarioCfg,
    trials: u32,
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
            quit_at_seq: None,
            keyed_quit: false,
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
            quit_at_seq: None,
            keyed_quit: false,
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
            quit_at_seq: None,
            keyed_quit: false,
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
            quit_at_seq: None,
            keyed_quit: false,
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
            quit_at_seq: None,
            keyed_quit: false,
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
            quit_at_seq: None,
            keyed_quit: false,
            max_wall: Duration::from_secs(60),
        },
    ]
}

/// Trials for unkeyed quit scenarios. INV-L4's unbiased-select tie-break
/// makes quit latency a distribution, so the tail needs many short trials.
const QUIT_TRIALS: u32 = 200;

/// Trials for the keyed quit scenario. Each trial drains the whole backlog
/// before the quit is delivered, so trials are long and the expected effect
/// (latency ~ drain time) is orders of magnitude above trial noise.
const KEYED_QUIT_TRIALS: u32 = 20;

fn quit_scenarios() -> Vec<QuitScenarioCfg> {
    // All quit scenarios use the overload update cost (25µs) so the backlog
    // drains slowly (~38k msg/s on the reference machine) and the depth at
    // the quit request is dominated by `total - quit_at_seq`.
    let base = ScenarioCfg {
        name: "",
        rate: BURST,
        total: 0,
        update_cost: Duration::from_micros(25),
        render_cost: Duration::from_micros(500),
        keyed_probe: false,
        quit_at_seq: Some(5_000),
        keyed_quit: false,
        max_wall: Duration::from_secs(30),
    };
    vec![
        // Control: quit with an empty queue; baseline delivery latency.
        QuitScenarioCfg {
            base: ScenarioCfg {
                name: "quit_idle",
                total: 1,
                quit_at_seq: Some(0),
                ..base.clone()
            },
            trials: QUIT_TRIALS,
        },
        // Quit while ~50k messages are still queued.
        QuitScenarioCfg {
            base: ScenarioCfg {
                name: "quit_backlog_50k",
                total: 55_000,
                ..base.clone()
            },
            trials: QUIT_TRIALS,
        },
        // Quit while ~300k messages are still queued; if quit latency were
        // backlog-dependent, it must separate from the 50k scenario here.
        QuitScenarioCfg {
            base: ScenarioCfg {
                name: "quit_backlog_300k",
                total: 305_000,
                ..base.clone()
            },
            trials: QUIT_TRIALS,
        },
        // Quit while the producer is still actively refilling the shared
        // channel (sustained overload rather than a draining burst).
        QuitScenarioCfg {
            base: ScenarioCfg {
                name: "quit_overload",
                rate: 100_000,
                total: 500_000,
                ..base.clone()
            },
            trials: QUIT_TRIALS,
        },
        // Keyed quit under the 50k backlog: delivered through the command's
        // private channel, so INV-14 shared-first pull defers it until the
        // shared backlog drains (expected latency ~ full drain time).
        QuitScenarioCfg {
            base: ScenarioCfg {
                name: "quit_keyed_backlog_50k",
                total: 55_000,
                keyed_quit: true,
                ..base
            },
            trials: KEYED_QUIT_TRIALS,
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
    /// Nanoseconds from `start` at which `update` requested the quit; 0 while
    /// no quit has been requested.
    quit_requested_ns: AtomicU64,
    /// Queue depth at the moment the quit was requested.
    depth_at_quit: AtomicU64,
    /// Nanoseconds from `start` at which the most recent `update` or `view`
    /// call finished; the loop breaks right after the last one, so this
    /// lower-bounds the quit delivery instant without runtime instrumentation.
    last_work_ns: AtomicU64,
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
            quit_requested_ns: AtomicU64::new(0),
            depth_at_quit: AtomicU64::new(0),
            last_work_ns: AtomicU64::new(0),
        }
    }

    fn queue_depth(&self) -> u64 {
        let produced = self.produced.load(Ordering::Relaxed);
        let processed = self.processed.load(Ordering::Relaxed);
        produced.saturating_sub(processed)
    }

    fn elapsed_ns(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    /// Records that an `update` or `view` call just finished.
    fn mark_work(&self) {
        self.last_work_ns
            .store(self.elapsed_ns(), Ordering::Relaxed);
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
                self.metrics
                    .processed
                    .store(self.processed, Ordering::Relaxed);
                self.metrics.mark_work();
                let request_quit = match self.cfg.quit_at_seq {
                    Some(quit_seq) => seq == quit_seq,
                    None => self.processed == self.cfg.total,
                };
                if request_quit {
                    self.metrics
                        .depth_at_quit
                        .store(self.metrics.queue_depth(), Ordering::Relaxed);
                    self.metrics
                        .quit_requested_ns
                        .store(self.metrics.elapsed_ns(), Ordering::Relaxed);
                    // The unkeyed quit is sent by its command task straight to
                    // the dedicated quit channel; the keyed variant travels the
                    // command's private channel instead (RFC 0006 section 4.2).
                    return if self.cfg.keyed_quit {
                        Command::quit().cancellable(CommandId::new("quit"))
                    } else {
                        Command::quit()
                    };
                }
                Command::none()
            }
            Msg::KeyedProbe { sent_at } => {
                Metrics::push_latency(&self.metrics.keyed_lat_ns, sent_at);
                self.metrics.mark_work();
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
                self.metrics
                    .rendered_marker
                    .store(marker, Ordering::Relaxed);
                Metrics::push_latency(&self.metrics.render_lat_ns, sent_at);
            }
        }
        self.metrics.mark_work();
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
    let producer_done = (producer_done_ns > 0).then(|| Duration::from_nanos(producer_done_ns));
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

/// One successful quit trial: queue depth at the quit request plus the two
/// latency bounds bracketing quit delivery (see the module docs).
struct QuitTrialSample {
    depth: u64,
    to_last_work_ns: u64,
    to_exit_ns: u64,
}

struct QuitReport {
    cfg: ScenarioCfg,
    trials: u32,
    timeouts: u32,
    /// Sorted per-trial values.
    depths: Vec<u64>,
    to_last_work_ns: Vec<u64>,
    to_exit_ns: Vec<u64>,
}

async fn run_quit_trial(cfg: ScenarioCfg) -> Option<QuitTrialSample> {
    let metrics = Arc::new(Metrics::new());
    let frame_rate = FrameRate::new(NonZeroU32::new(60).expect("non-zero fps"))
        .expect("60 FPS is a valid frame rate");
    let runtime = Runtime::<LoadApp>::new((cfg.clone(), Arc::clone(&metrics)), frame_rate);
    let mut terminal =
        Terminal::new(TestBackend::new(120, 40)).expect("test backend terminal creation");

    let timed_out = timeout(cfg.max_wall, runtime.run(&mut terminal))
        .await
        .is_err();
    let exit_ns = metrics.elapsed_ns();

    let quit_ns = metrics.quit_requested_ns.load(Ordering::Relaxed);
    if timed_out || quit_ns == 0 {
        return None;
    }
    Some(QuitTrialSample {
        depth: metrics.depth_at_quit.load(Ordering::Relaxed),
        to_last_work_ns: metrics
            .last_work_ns
            .load(Ordering::Relaxed)
            .saturating_sub(quit_ns),
        to_exit_ns: exit_ns.saturating_sub(quit_ns),
    })
}

async fn run_quit_scenario(scenario: &QuitScenarioCfg) -> QuitReport {
    let mut timeouts = 0;
    let mut depths = Vec::new();
    let mut to_last_work_ns = Vec::new();
    let mut to_exit_ns = Vec::new();

    for _ in 0..scenario.trials {
        match run_quit_trial(scenario.base.clone()).await {
            Some(sample) => {
                depths.push(sample.depth);
                to_last_work_ns.push(sample.to_last_work_ns);
                to_exit_ns.push(sample.to_exit_ns);
            }
            None => timeouts += 1,
        }
    }

    depths.sort_unstable();
    to_last_work_ns.sort_unstable();
    to_exit_ns.sort_unstable();

    QuitReport {
        cfg: scenario.base.clone(),
        trials: scenario.trials,
        timeouts,
        depths,
        to_last_work_ns,
        to_exit_ns,
    }
}

fn print_quit_report(report: &QuitReport) {
    let cfg = &report.cfg;
    println!("## {}", cfg.name);
    let rate = if cfg.rate == BURST {
        "burst".to_owned()
    } else {
        format!("{}/s", cfg.rate)
    };
    println!(
        "   load: rate={rate} total={} update_cost={:?} quit_at_seq={} keyed_quit={}",
        cfg.total,
        cfg.update_cost,
        cfg.quit_at_seq.expect("quit scenarios set quit_at_seq"),
        cfg.keyed_quit,
    );
    println!(
        "   trials: {} ok, {} timed out",
        report.trials - report.timeouts,
        report.timeouts,
    );
    if report.depths.is_empty() {
        return;
    }
    println!(
        "   depth at quit: min={} p50={} max={}",
        report.depths.first().expect("non-empty"),
        percentile(&report.depths, 0.50),
        report.depths.last().expect("non-empty"),
    );
    println!(
        "   quit -> last work: {}",
        format_lat(&report.to_last_work_ns)
    );
    println!("   quit -> exit:      {}", format_lat(&report.to_exit_ns));
    println!();
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

    let matches = |name: &str| selected.is_empty() || selected.iter().any(|s| s == name);
    let load_to_run: Vec<ScenarioCfg> = scenarios()
        .into_iter()
        .filter(|cfg| matches(cfg.name))
        .collect();
    let quit_to_run: Vec<QuitScenarioCfg> = quit_scenarios()
        .into_iter()
        .filter(|scenario| matches(scenario.base.name))
        .collect();
    if load_to_run.is_empty() && quit_to_run.is_empty() {
        let names: Vec<&str> = scenarios()
            .into_iter()
            .map(|cfg| cfg.name)
            .chain(
                quit_scenarios()
                    .into_iter()
                    .map(|scenario| scenario.base.name),
            )
            .collect();
        println!("no matching scenario; available: {}", names.join(", "));
        return;
    }

    let runtime = Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    println!("# tears runtime load harness\n");
    for cfg in load_to_run {
        let report = runtime.block_on(run_scenario(cfg));
        print_report(&report);
    }
    for scenario in quit_to_run {
        let report = runtime.block_on(run_quit_scenario(&scenario));
        print_quit_report(&report);
    }
}
