//! Load harness for the reducer-first kernel — the re-derivation half of
//! RFC 0014 §13.5, which RFC 0006 §5.2 names as INV-L4's prerequisite.
//!
//! `benches/runtime_load.rs` measures the superseded topology and is left
//! untouched: it is the *before* column. This harness runs the same shapes of
//! load against the kernel's own driving loop (`Kernel::drive`, reached
//! through the bench-only `BenchKernel` handle), so the two columns differ in
//! the topology under test and in nothing else the harness controls.
//!
//! # What the new topology changes about the measurement
//!
//! - **The quit route split.** RFC 0006's `quit_*` scenarios have `update`
//!   return `Command::quit()`, which in the old topology became an effect
//!   stream item travelling the dedicated quit channel — the channel INV-L4 is
//!   about. On the kernel that spelling is the **synchronous** route, applied
//!   at its own dispatch with no lane involved (RFC 0014 §3.3). The lane route
//!   INV-L4's property carries to is the **producer-originated** quit, so
//!   every quit row here is run twice: `…_sync` (update-returned) and
//!   `…_control` (a spawned run emitting one quit on the control lane).
//! - **Where the measured interval ends.** The old harness timestamps the
//!   runtime's `quit signal received` tracing event, emitted from inside the
//!   `select!` arm before any shutdown work. The kernel emits no counterpart —
//!   its control drain applies the quit directly — so what this harness can
//!   observe is `Kernel::drive` returning, which is the quit's application
//!   **plus its immediate postcondition** (receivers dropped, buffers cleared,
//!   every run revoked and aborted; RFC 0011 §4.4). The reported
//!   `quit->applied` is therefore an **upper bound** on delivery, not the
//!   delivery instant: it contains a term that scales with the data lane's
//!   residual occupancy, which is why every row also reports the depth at the
//!   quit instant. In the bounded rows that residue is capped at
//!   `capacity + producers`, so the bound is tight there; in the unbounded
//!   deep-backlog rows it is not, and the number must be read as a bound.
//!   Whether the successor's acceptance instrument stays this bound or gains a
//!   delivery-instant event is a contract question for RFC 0006, not a
//!   harness choice — this file only reports what it can see.
//! - **INV-L9 has no counterpart.** The `keyed_isolation` scenario quantifies
//!   over the per-`CommandId` private channels the kernel removes (RFC 0006
//!   §5.2 records the property loss), so it is absent here rather than
//!   re-measured.
//! - **No frame pacing.** The kernel's stage 4 renders once per pass with a
//!   redraw pending and reads no clock (RFC 0014 §3.5, §6.3), so `FrameRate`
//!   is not an input. `RuntimeConfig::new` still requires one to construct;
//!   the kernel never reads it (`RuntimeConfig::kernel_controls`).
//!
//! # Rows
//!
//! - **`overload`, `burst_200k`, `steady_20k`** — INV-L1's depth bound,
//!   INV-L2's losslessness, and INV-L3's update latency, unbounded and under
//!   the RFC 0007 §5.1 bounded capacity.
//! - **`quit_*`** — INV-L4's statistical rows, both routes and both modes,
//!   `QUIT_TRIALS` trials each.
//!
//! Run all rows, or name a subset:
//!
//! ```bash
//! cargo bench --bench kernel_load --features bench-internals
//! cargo bench --bench kernel_load --features bench-internals -- overload
//! ```

// Metric reporting converts counters and nanosecond values to floating point
// for human-readable output; precision loss there is irrelevant.
#![expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "metric reporting casts counters and nanosecond values to floating point for human-readable output; precision loss there is irrelevant"
)]

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::fmt::Debug;
use std::num::{NonZeroU32, NonZeroUsize};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use std::{env, hint};

use futures::stream::{self, StreamExt};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tears::prelude::*;
use tears::{BenchKernel, BoxStream, FrameRate, RuntimeConfig, SubscriptionSource, producer_quit};
use tokio::runtime::{Builder, Runtime as TokioRuntime};
use tokio::task::yield_now;
use tokio::time::{MissedTickBehavior, interval, timeout};
use tokio_stream::wrappers::IntervalStream;
use tracing::field::{Field, Visit};
use tracing::level_filters::LevelFilter;
use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::set_global_default;
use tracing::{Event, Level, Metadata, Subscriber};

/// Message rate for rows that emit their whole load in one burst.
const BURST: u64 = 0;

/// RFC 0007 §5.1's bounded capacity, carried over unchanged so the bounded
/// column compares against the old harness's.
const DATA_LANE_CAPACITY: usize = 1024;

/// Trials per quit row — RFC 0006 INV-L4's "≥ 200 trials per scenario".
const QUIT_TRIALS: u32 = 200;

/// Attempts a quit row may spend collecting `QUIT_TRIALS` valid trials.
const QUIT_ATTEMPT_CAP: u32 = 4 * QUIT_TRIALS;

/// Terminal size every row renders into.
const SCREEN: (u16, u16) = (80, 24);

/// The frame rate `RuntimeConfig::new` requires and the kernel never reads.
fn unread_frame_rate() -> FrameRate {
    FrameRate::new(NonZeroU32::new(60).expect("non-zero fps")).expect("60 FPS is valid")
}

/// Which of RFC 0014 §3.3's two quit routes a row exercises.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum QuitRoute {
    /// `update` returns `Command::quit()`: applied synchronously at that
    /// dispatch, no lane.
    Sync,
    /// `update` returns a command whose spawned run emits one quit on the
    /// control lane — the route INV-L4's backlog independence carries to.
    Control,
}

/// The data lane's mode.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// `data_lane_capacity` unset: unbounded lane.
    Unbounded,
    /// The RFC 0007 §5.1 capacity.
    Bounded,
}

impl Mode {
    fn config(self) -> RuntimeConfig {
        let config = RuntimeConfig::new(unread_frame_rate());
        match self {
            Self::Unbounded => config,
            Self::Bounded => config
                .app_channel_capacity(NonZeroUsize::new(DATA_LANE_CAPACITY).expect("non-zero")),
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Unbounded => "unbounded",
            Self::Bounded => "bounded",
        }
    }
}

#[derive(Clone)]
struct Cfg {
    name: &'static str,
    /// Messages per second per producer, or [`BURST`].
    rate: u64,
    /// Total flood messages across all producers.
    total: u64,
    update_cost: Duration,
    render_cost: Duration,
    /// Quit when this flood seq reaches `update`; `None` quits at `total`.
    quit_at_seq: Option<u64>,
    quit_route: QuitRoute,
    producers: u32,
    mode: Mode,
    /// `batch_max_messages`; `None` leaves the kernel's own finite default
    /// (`DEFAULT_BATCH_MAX_MESSAGES`, 1024). Only the decomposition rows set
    /// it, to shrink the in-progress batch's remainder to nothing.
    batch_cap: Option<usize>,
    max_wall: Duration,
}

/// The configuration a row runs under: the lane mode plus an optional batch
/// cap.
fn config_for(cfg: &Cfg) -> RuntimeConfig {
    let config = cfg.mode.config();
    match cfg.batch_cap {
        None => config,
        Some(cap) => config.batch_max_messages(NonZeroUsize::new(cap).expect("non-zero cap")),
    }
}

/// The valid-trial predicate a bounded quit row checks at the quit instant
/// (RFC 0007 §5.2, carried over).
#[derive(Clone, Copy)]
enum ValidTrial {
    /// Every completed attempt counts.
    Always,
    /// The `blocked` producer gauge reads exactly this at the quit instant.
    BlockedEq(u64),
    /// At least two data-lane capacity-wait events in the 5ms before the quit.
    Churn,
}

impl ValidTrial {
    fn holds(self, metrics: &Metrics) -> bool {
        match self {
            Self::Always => true,
            Self::BlockedEq(n) => metrics.blocked_at_quit.load(Ordering::Relaxed) == n,
            Self::Churn => metrics.capacity_waits_before_quit.load(Ordering::Relaxed) >= 2,
        }
    }
}

struct QuitCfg {
    base: Cfg,
    trials: u32,
    valid_trial: ValidTrial,
}

fn load_scenarios() -> Vec<Cfg> {
    let base = Cfg {
        name: "",
        rate: BURST,
        total: 0,
        update_cost: Duration::from_micros(25),
        render_cost: Duration::from_micros(500),
        quit_at_seq: None,
        quit_route: QuitRoute::Sync,
        producers: 1,
        mode: Mode::Unbounded,
        batch_cap: None,
        max_wall: Duration::from_secs(60),
    };
    vec![
        // INV-L3 / INV-L1: sustained overload, producer faster than the drain.
        Cfg {
            name: "overload",
            rate: 100_000,
            total: 500_000,
            ..base.clone()
        },
        Cfg {
            name: "overload_bounded",
            rate: 100_000,
            total: 500_000,
            mode: Mode::Bounded,
            ..base.clone()
        },
        // INV-L2 / INV-L1: one burst, then the drain.
        Cfg {
            name: "burst_200k",
            total: 200_000,
            update_cost: Duration::from_micros(2),
            ..base.clone()
        },
        Cfg {
            name: "burst_200k_bounded",
            total: 200_000,
            update_cost: Duration::from_micros(2),
            mode: Mode::Bounded,
            ..base.clone()
        },
        // Paced load well below the loop's capacity: the baseline row.
        Cfg {
            name: "steady_20k",
            rate: 20_000,
            total: 100_000,
            update_cost: Duration::from_micros(2),
            ..base.clone()
        },
        Cfg {
            name: "steady_20k_bounded",
            rate: 20_000,
            total: 100_000,
            update_cost: Duration::from_micros(2),
            mode: Mode::Bounded,
            ..base.clone()
        },
        // Paced load near the loop's capacity.
        Cfg {
            name: "steady_200k",
            rate: 200_000,
            total: 1_000_000,
            update_cost: Duration::from_micros(2),
            ..base
        },
    ]
}

#[expect(clippy::too_many_lines, reason = "a flat table of scenario literals")]
fn quit_scenarios() -> Vec<QuitCfg> {
    // Same 25µs update cost as the old harness's quit rows, so the backlog
    // drains at a comparable rate and the depth at the quit request is
    // dominated by `total - quit_at_seq`.
    let base = Cfg {
        name: "",
        rate: BURST,
        total: 0,
        update_cost: Duration::from_micros(25),
        render_cost: Duration::from_micros(500),
        quit_at_seq: Some(5_000),
        quit_route: QuitRoute::Sync,
        producers: 1,
        mode: Mode::Unbounded,
        batch_cap: None,
        max_wall: Duration::from_secs(30),
    };
    let mut rows = Vec::new();
    // The unbounded depth rows, both routes: the empty-lane control and the
    // two depths F6's depth-independence was read from.
    for (name, total, quit_at_seq, rate) in [
        ("quit_idle", 1_u64, Some(0_u64), BURST),
        ("quit_backlog_50k", 55_000, Some(5_000), BURST),
        ("quit_backlog_300k", 305_000, Some(5_000), BURST),
        ("quit_overload", 500_000, Some(5_000), 100_000),
    ] {
        for route in [QuitRoute::Sync, QuitRoute::Control] {
            rows.push(QuitCfg {
                base: Cfg {
                    name: leak_name(name, route),
                    total,
                    quit_at_seq,
                    rate,
                    quit_route: route,
                    ..base.clone()
                },
                trials: QUIT_TRIALS,
                valid_trial: ValidTrial::Always,
            });
        }
    }
    // The bounded rows. Depth caps at `capacity + producers` there, so these
    // vary blocked-producer count and channel-full churn instead — RFC 0006
    // §5.1's reproducibility rule, and the shape RFC 0007 §5.2 pinned.
    for route in [QuitRoute::Sync, QuitRoute::Control] {
        rows.push(QuitCfg {
            base: Cfg {
                name: leak_name("quit_idle_bounded", route),
                total: 1,
                quit_at_seq: Some(0),
                mode: Mode::Bounded,
                quit_route: route,
                ..base.clone()
            },
            trials: QUIT_TRIALS,
            valid_trial: ValidTrial::Always,
        });
        rows.push(QuitCfg {
            base: Cfg {
                name: leak_name("quit_blocked_1", route),
                total: 500_000,
                mode: Mode::Bounded,
                quit_route: route,
                ..base.clone()
            },
            trials: QUIT_TRIALS,
            valid_trial: ValidTrial::BlockedEq(1),
        });
        rows.push(QuitCfg {
            base: Cfg {
                name: leak_name("quit_blocked_64", route),
                total: 500_000,
                producers: 64,
                mode: Mode::Bounded,
                quit_route: route,
                ..base.clone()
            },
            trials: QUIT_TRIALS,
            valid_trial: ValidTrial::BlockedEq(64),
        });
        rows.push(QuitCfg {
            base: Cfg {
                name: leak_name("quit_overload_bounded", route),
                rate: 100_000,
                total: 500_000,
                mode: Mode::Bounded,
                quit_route: route,
                ..base.clone()
            },
            trials: QUIT_TRIALS,
            valid_trial: ValidTrial::Churn,
        });
    }
    // Decomposition rows (control route only; the synchronous route's own
    // interval is already at the harness's resolution). RFC 0014 §3.3 states
    // the control-lane bound as "the in-progress batch's remainder, bounded by
    // the batch cap", and §3.5 puts the frame stage after the drain of the
    // *next* pass. These three rows vary exactly those two terms — batch cap
    // 1 removes the remainder, `render_cost` 0 removes the frame's spin — so
    // the depth-scaling term and the two fixed terms are separated by
    // measurement rather than by inference.
    for (name, total, batch_cap, render_cost) in [
        (
            "decomp_50k_batch1",
            55_000_u64,
            Some(1_usize),
            Duration::from_micros(500),
        ),
        (
            "decomp_50k_batch1_norender",
            55_000,
            Some(1),
            Duration::ZERO,
        ),
        (
            "decomp_300k_batch1_norender",
            305_000,
            Some(1),
            Duration::ZERO,
        ),
    ] {
        rows.push(QuitCfg {
            base: Cfg {
                name,
                total,
                batch_cap,
                render_cost,
                quit_route: QuitRoute::Control,
                ..base.clone()
            },
            trials: QUIT_TRIALS,
            valid_trial: ValidTrial::Always,
        });
    }
    rows
}

/// Row names are `&'static str` for the same reason the old harness's are —
/// they are compared against CLI arguments and printed — and the route suffix
/// is built at table-construction time, once per row.
fn leak_name(stem: &str, route: QuitRoute) -> &'static str {
    let suffix = match route {
        QuitRoute::Sync => "_sync",
        QuitRoute::Control => "_control",
    };
    Box::leak(format!("{stem}{suffix}").into_boxed_str())
}

/// The trial whose [`Metrics`] receive the next gauge / capacity-wait events.
/// Rows run one kernel at a time, so a single slot suffices.
static TRIAL_METRICS: Mutex<Option<Arc<Metrics>>> = Mutex::new(None);

/// The current producer-gauge sum, from the greatest-`seq` gauge event of each
/// `runtime_id` partition. Reaches 0 only once the current kernel has torn its
/// producers down, so it doubles as the between-row teardown barrier.
static LIVE_PRODUCERS: AtomicU64 = AtomicU64::new(0);

/// Per-`runtime_id` high-water marks of the newest applied `seq` (RFC 0006
/// §4.4): gauge events are ordered by `seq` within a partition, never by
/// arrival.
static GAUGE_PARTITION_SEEN: Mutex<BTreeMap<u64, u64>> = Mutex::new(BTreeMap::new());

/// Waits for the previous kernel's producers to quiesce before the next row
/// starts, so a straggler gauge event cannot land in the next row's slot.
async fn await_quiescence() {
    let quiesced = timeout(Duration::from_secs(5), async {
        while LIVE_PRODUCERS.load(Ordering::Relaxed) != 0 {
            yield_now().await;
        }
    })
    .await;
    assert!(
        quiesced.is_ok(),
        "producers did not quiesce within 5s; a later row's gauge slot would be corrupted"
    );
}

/// Reads the `blocked` producer gauge and the data-lane capacity-wait events
/// from `tears::runtime::load` — the same schema the old harness reads, under
/// RFC 0014 §9 row 9's vocabulary (`channel` now takes the single value
/// `"data"`).
///
/// Unlike the old harness's subscriber this one has no quit-delivery event to
/// match: the kernel emits none (see this file's header).
struct LoadSubscriber;

impl Subscriber for LoadSubscriber {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.is_event()
            && *metadata.level() == Level::DEBUG
            && metadata.target() == "tears::runtime::load"
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(LevelFilter::DEBUG)
    }

    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    // The partition guard is held across the value stores on purpose: releasing
    // it after the high-water advance would let a stale concurrent gauge event
    // interleave a store between the check and the apply, the reorder the `seq`
    // ordering exists to defeat (RFC 0006 §4.4).
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the gauge partition guard is deliberately held across the value stores so \"advance and apply\" is one step (RFC 0006 §4.4)"
    )]
    fn event(&self, event: &Event<'_>) {
        let mut visitor = LoadVisitor::default();
        event.record(&mut visitor);

        if let (Some(runtime_id), Some(seq)) = (visitor.runtime_id, visitor.seq) {
            let slot = TRIAL_METRICS
                .lock()
                .expect("trial metrics slot poisoned")
                .clone();
            let mut seen = GAUGE_PARTITION_SEEN
                .lock()
                .expect("gauge partition high-water mark poisoned");
            match seen.entry(runtime_id) {
                Entry::Vacant(vacant) => {
                    vacant.insert(seq);
                }
                Entry::Occupied(mut mark) if seq > *mark.get() => {
                    mark.insert(seq);
                }
                Entry::Occupied(_) => return,
            }
            LIVE_PRODUCERS.store(visitor.gauge_sum(), Ordering::Relaxed);
            if let (Some(metrics), Some(blocked)) = (slot, visitor.blocked) {
                metrics.blocked_live.store(blocked, Ordering::Relaxed);
            }
            return;
        }

        // A capacity-wait event: one lane, so `channel` has one value.
        if visitor.channel.as_deref() == Some("data")
            && let Some(metrics) = TRIAL_METRICS
                .lock()
                .expect("trial metrics slot poisoned")
                .clone()
        {
            metrics
                .capacity_wait_ns
                .lock()
                .expect("capacity-wait log poisoned")
                .push(metrics.elapsed_ns());
        }
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

#[derive(Default)]
struct LoadVisitor {
    runtime_id: Option<u64>,
    seq: Option<u64>,
    subscriptions: Option<u64>,
    unkeyed_commands: Option<u64>,
    keyed_commands: Option<u64>,
    blocked: Option<u64>,
    channel: Option<String>,
}

impl LoadVisitor {
    fn gauge_sum(&self) -> u64 {
        self.subscriptions.unwrap_or(0)
            + self.unkeyed_commands.unwrap_or(0)
            + self.keyed_commands.unwrap_or(0)
            + self.blocked.unwrap_or(0)
    }
}

impl Visit for LoadVisitor {
    fn record_u64(&mut self, field: &Field, value: u64) {
        match field.name() {
            "runtime_id" => self.runtime_id = Some(value),
            "seq" => self.seq = Some(value),
            "subscriptions" => self.subscriptions = Some(value),
            "unkeyed_commands" => self.unkeyed_commands = Some(value),
            "keyed_commands" => self.keyed_commands = Some(value),
            "blocked" => self.blocked = Some(value),
            _ => {}
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "channel" {
            self.channel = Some(value.to_owned());
        }
    }

    fn record_debug(&mut self, _field: &Field, _value: &dyn Debug) {}
}

struct Metrics {
    start: Instant,
    produced: AtomicU64,
    processed: AtomicU64,
    frames: AtomicU64,
    update_lat_ns: Mutex<Vec<u64>>,
    max_depth: AtomicU64,
    /// Nanoseconds from `start` at which the quit was requested — inside
    /// `update` for the sync route, on the producer task for the control
    /// route, in both cases as the last act before the quit leaves.
    quit_requested_ns: AtomicU64,
    /// Data-lane residue at the quit request (`produced - processed`).
    depth_at_quit: AtomicU64,
    /// Whether the flood seqs arrived in an unbroken `0, 1, …` run.
    seq_next: AtomicU64,
    seq_broken: AtomicBool,
    blocked_at_quit: AtomicU64,
    blocked_live: AtomicU64,
    capacity_waits_before_quit: AtomicU64,
    capacity_wait_ns: Mutex<Vec<u64>>,
}

impl Metrics {
    // Real wall-clock reads: RFC 0006's acceptance criteria are defined on real
    // time, the sanctioned exception to the single-time-source rule
    // (RFC 0009 §3.1).
    #[expect(
        clippy::disallowed_methods,
        reason = "stamps the real wall-clock baseline; RFC 0006's acceptance criteria are defined on real time, the sanctioned single-time-source exception (RFC 0009 §3.1)"
    )]
    fn new() -> Self {
        Self {
            start: Instant::now(),
            produced: AtomicU64::new(0),
            processed: AtomicU64::new(0),
            frames: AtomicU64::new(0),
            update_lat_ns: Mutex::new(Vec::new()),
            max_depth: AtomicU64::new(0),
            quit_requested_ns: AtomicU64::new(0),
            depth_at_quit: AtomicU64::new(0),
            seq_next: AtomicU64::new(0),
            seq_broken: AtomicBool::new(false),
            blocked_at_quit: AtomicU64::new(0),
            blocked_live: AtomicU64::new(0),
            capacity_waits_before_quit: AtomicU64::new(0),
            capacity_wait_ns: Mutex::new(Vec::new()),
        }
    }

    /// `produced - processed`: `produced` counts a message when the source
    /// yields it, `processed` when `reduce` begins. The observable bounded
    /// bound is therefore `capacity + producers` (RFC 0006 §5.1's depth
    /// accounting, unchanged — the one lane replaces the shared channel).
    fn queue_depth(&self) -> u64 {
        self.produced
            .load(Ordering::Relaxed)
            .saturating_sub(self.processed.load(Ordering::Relaxed))
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "reads real wall-clock elapsed time; RFC 0006's acceptance criteria are defined on real time, the sanctioned single-time-source exception (RFC 0009 §3.1)"
    )]
    fn elapsed_ns(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "measures real wall-clock message latency; RFC 0006's acceptance criteria are defined on real time, the sanctioned single-time-source exception (RFC 0009 §3.1)"
    )]
    fn push_latency(bucket: &Mutex<Vec<u64>>, sent_at: Instant) {
        let nanos = u64::try_from(sent_at.elapsed().as_nanos()).unwrap_or(u64::MAX);
        bucket.lock().expect("latency bucket poisoned").push(nanos);
    }
}

enum Msg {
    Load { seq: u64, sent_at: Instant },
}

/// Emits `total / producers` flood messages, paced by `rate`, then stays
/// pending so the run is never reaped and re-admitted by a re-evaluation.
struct FloodSource {
    cfg: Cfg,
    metrics: Arc<Metrics>,
    index: u32,
}

impl SubscriptionSource for FloodSource {
    type Output = Msg;
    type Key = u32;

    #[expect(
        clippy::disallowed_methods,
        reason = "stamps each message's send time, a real wall-clock read; RFC 0006's acceptance criteria are defined on real time, the sanctioned single-time-source exception (RFC 0009 §3.1)"
    )]
    fn stream(&self) -> BoxStream<'static, Msg> {
        let metrics = Arc::clone(&self.metrics);
        let total = self.cfg.total;
        let share = usize::try_from(total.div_ceil(u64::from(self.cfg.producers)))
            .expect("share fits in usize");
        let per_tick = if self.cfg.rate == BURST {
            share
        } else {
            usize::try_from((self.cfg.rate / 1_000).max(1)).expect("per-tick fits in usize")
        };

        let mut ticker = interval(Duration::from_millis(1));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Burst);

        IntervalStream::new(ticker)
            .flat_map(move |_| {
                let metrics = Arc::clone(&metrics);
                stream::iter((0..per_tick).map(move |_| Msg::Load {
                    seq: metrics.produced.fetch_add(1, Ordering::Relaxed),
                    sent_at: Instant::now(),
                }))
            })
            .take(share)
            .chain(stream::pending())
            .boxed()
    }

    fn key(&self) -> Self::Key {
        self.index
    }
}

struct LoadApp {
    cfg: Cfg,
    metrics: Arc<Metrics>,
    processed: u64,
    /// Record every Nth update latency to bound sample memory.
    sample_every: u64,
}

impl Application for LoadApp {
    type Message = Msg;
    type Flags = (Cfg, Arc<Metrics>);

    fn new((cfg, metrics): Self::Flags) -> (Self, Command<Msg>) {
        let sample_every = (cfg.total / 100_000).max(1);
        (
            Self {
                cfg,
                metrics,
                processed: 0,
                sample_every,
            },
            Command::none(),
        )
    }

    fn update(&mut self, msg: Msg) -> Command<Msg> {
        let Msg::Load { seq, sent_at } = msg;
        self.processed += 1;
        self.metrics
            .processed
            .store(self.processed, Ordering::Relaxed);
        let expected = self.metrics.seq_next.fetch_add(1, Ordering::Relaxed);
        if seq != expected {
            self.metrics.seq_broken.store(true, Ordering::Relaxed);
        }
        let depth = self.metrics.queue_depth();
        self.metrics.max_depth.fetch_max(depth, Ordering::Relaxed);
        spin(self.cfg.update_cost);
        if seq % self.sample_every == 0 {
            Metrics::push_latency(&self.metrics.update_lat_ns, sent_at);
        }

        let request_quit = match self.cfg.quit_at_seq {
            Some(quit_seq) => seq == quit_seq,
            None => self.processed == self.cfg.total,
        };
        if !request_quit {
            return Command::none();
        }
        self.metrics.depth_at_quit.store(depth, Ordering::Relaxed);
        self.metrics.blocked_at_quit.store(
            self.metrics.blocked_live.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
        match self.cfg.quit_route {
            QuitRoute::Sync => {
                // Stamped last, so it is the instant the synchronous route is
                // requested; lowering applies it at this dispatch's completion.
                self.metrics
                    .quit_requested_ns
                    .store(self.metrics.elapsed_ns(), Ordering::Relaxed);
                Command::quit()
            }
            QuitRoute::Control => {
                // Stamped on the producer task, in the poll that yields the
                // quit — immediately before it enters the control lane.
                let metrics = Arc::clone(&self.metrics);
                producer_quit(move || {
                    metrics
                        .quit_requested_ns
                        .store(metrics.elapsed_ns(), Ordering::Relaxed);
                })
            }
        }
    }

    fn view(&self, _frame: &mut ratatui::Frame<'_>) {
        spin(self.cfg.render_cost);
        self.metrics.frames.fetch_add(1, Ordering::Relaxed);
    }

    fn subscriptions(&self) -> Vec<Subscription<Msg>> {
        (0..self.cfg.producers)
            .map(|index| {
                Subscription::new(FloodSource {
                    cfg: self.cfg.clone(),
                    metrics: Arc::clone(&self.metrics),
                    index,
                })
            })
            .collect()
    }
}

/// Busy-waits for `duration`, simulating CPU-bound work on the driving task.
#[expect(
    clippy::disallowed_methods,
    reason = "busy-waits against a real wall-clock deadline while simulating CPU-bound work; RFC 0006's acceptance criteria are defined on real time, the sanctioned single-time-source exception (RFC 0009 §3.1)"
)]
fn spin(duration: Duration) {
    if duration.is_zero() {
        return;
    }
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        hint::spin_loop();
    }
}

/// Runs one kernel to termination, returning the metrics plus the two
/// wall-clock instants the quit rows read.
///
/// `applied_ns` is `Kernel::drive` returning — the quit applied *and* its
/// immediate postcondition run. `exit_ns` adds the quiescent postcondition.
async fn run_kernel(cfg: Cfg, metrics: &Arc<Metrics>) -> Option<(u64, u64, usize)> {
    let mut terminal =
        Terminal::new(TestBackend::new(SCREEN.0, SCREEN.1)).expect("test terminal builds");
    let config = config_for(&cfg);
    let max_wall = cfg.max_wall;
    let mut kernel = BenchKernel::<LoadApp>::new((cfg, Arc::clone(metrics)), &config);

    let driven = timeout(max_wall, kernel.drive(&mut terminal)).await;
    let applied_ns = metrics.elapsed_ns();
    let Ok(outcome) = driven else {
        return None;
    };
    outcome.expect("the test backend never fails a render");
    let joined = kernel.settle().await;
    let exit_ns = metrics.elapsed_ns();
    Some((applied_ns, exit_ns, joined))
}

struct Report {
    cfg: Cfg,
    timed_out: bool,
    wall_ns: u64,
    max_depth: u64,
    produced: u64,
    processed: u64,
    seq_broken: bool,
    frames: u64,
    joined: usize,
    update_lat: Vec<u64>,
}

async fn run_load_scenario(cfg: Cfg) -> Report {
    await_quiescence().await;
    let metrics = Arc::new(Metrics::new());
    *TRIAL_METRICS.lock().expect("trial metrics slot poisoned") = Some(Arc::clone(&metrics));
    let outcome = run_kernel(cfg.clone(), &metrics).await;
    *TRIAL_METRICS.lock().expect("trial metrics slot poisoned") = None;

    let mut update_lat = metrics
        .update_lat_ns
        .lock()
        .expect("latency bucket poisoned")
        .clone();
    update_lat.sort_unstable();
    Report {
        cfg,
        timed_out: outcome.is_none(),
        wall_ns: outcome.map_or(0, |(_, exit, _)| exit),
        max_depth: metrics.max_depth.load(Ordering::Relaxed),
        produced: metrics.produced.load(Ordering::Relaxed),
        processed: metrics.processed.load(Ordering::Relaxed),
        seq_broken: metrics.seq_broken.load(Ordering::Relaxed),
        frames: metrics.frames.load(Ordering::Relaxed),
        joined: outcome.map_or(0, |(_, _, joined)| joined),
        update_lat,
    }
}

struct QuitSample {
    applied_ns: u64,
    exit_ns: u64,
    depth_at_quit: u64,
    valid: bool,
}

async fn run_quit_trial(cfg: Cfg, valid_trial: ValidTrial) -> Option<QuitSample> {
    await_quiescence().await;
    let metrics = Arc::new(Metrics::new());
    *TRIAL_METRICS.lock().expect("trial metrics slot poisoned") = Some(Arc::clone(&metrics));
    let outcome = run_kernel(cfg, &metrics).await;
    // The churn predicate's window is counted off the hot path, from the
    // logged capacity-wait instants.
    let requested = metrics.quit_requested_ns.load(Ordering::Relaxed);
    let waits = metrics
        .capacity_wait_ns
        .lock()
        .expect("capacity-wait log poisoned")
        .iter()
        .filter(|at| **at <= requested && requested.saturating_sub(**at) <= 5_000_000)
        .count();
    metrics
        .capacity_waits_before_quit
        .store(waits as u64, Ordering::Relaxed);
    *TRIAL_METRICS.lock().expect("trial metrics slot poisoned") = None;

    let (applied_ns, exit_ns, _joined) = outcome?;
    if requested == 0 || applied_ns < requested {
        return None;
    }
    Some(QuitSample {
        applied_ns: applied_ns - requested,
        exit_ns: exit_ns - requested,
        depth_at_quit: metrics.depth_at_quit.load(Ordering::Relaxed),
        valid: valid_trial.holds(&metrics),
    })
}

struct QuitReport {
    cfg: Cfg,
    trials: u32,
    attempts: u32,
    failures: u32,
    applied: Vec<u64>,
    exit: Vec<u64>,
    depths: Vec<u64>,
}

impl QuitReport {
    const fn incomplete(&self) -> bool {
        self.applied.len() < self.trials as usize
    }
}

async fn run_quit_scenario(scenario: &QuitCfg) -> QuitReport {
    let mut applied = Vec::new();
    let mut exit = Vec::new();
    let mut depths = Vec::new();
    let mut attempts = 0;
    let mut failures = 0;
    while (applied.len() as u32) < scenario.trials && attempts < QUIT_ATTEMPT_CAP {
        attempts += 1;
        match run_quit_trial(scenario.base.clone(), scenario.valid_trial).await {
            Some(sample) if sample.valid => {
                applied.push(sample.applied_ns);
                exit.push(sample.exit_ns);
                depths.push(sample.depth_at_quit);
            }
            Some(_) => {}
            None => failures += 1,
        }
    }
    applied.sort_unstable();
    exit.sort_unstable();
    depths.sort_unstable();
    QuitReport {
        cfg: scenario.base.clone(),
        trials: scenario.trials,
        attempts,
        failures,
        applied,
        exit,
        depths,
    }
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let index = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[index]
}

fn format_ms(sorted: &[u64]) -> String {
    if sorted.is_empty() {
        return "n/a".to_owned();
    }
    format!(
        "p50 {:.3}ms  p95 {:.3}ms  p99 {:.3}ms  max {:.3}ms",
        percentile(sorted, 0.50) as f64 / 1e6,
        percentile(sorted, 0.95) as f64 / 1e6,
        percentile(sorted, 0.99) as f64 / 1e6,
        sorted[sorted.len() - 1] as f64 / 1e6,
    )
}

fn print_load_report(report: &Report) {
    println!("## {} ({})", report.cfg.name, report.cfg.mode.label());
    if report.timed_out {
        println!("  TIMED OUT after {:?}\n", report.cfg.max_wall);
        return;
    }
    println!("  wall              {:.3}s", report.wall_ns as f64 / 1e9);
    println!(
        "  produced/processed {} / {}   lossless={}  in-order={}",
        report.produced,
        report.processed,
        report.produced == report.processed,
        !report.seq_broken
    );
    println!("  max queue depth   {}", report.max_depth);
    println!("  update latency    {}", format_ms(&report.update_lat));
    println!(
        "  frames            {}   settle joined {}",
        report.frames, report.joined
    );
    println!();
}

fn print_quit_report(report: &QuitReport) {
    println!(
        "## {} ({}, route={:?})",
        report.cfg.name,
        report.cfg.mode.label(),
        report.cfg.quit_route
    );
    println!(
        "  trials            {} valid / {} attempts / {} failures",
        report.applied.len(),
        report.attempts,
        report.failures
    );
    println!("  quit->applied     {}", format_ms(&report.applied));
    println!("  quit->exit        {}", format_ms(&report.exit));
    if report.depths.is_empty() {
        println!("  depth at quit     n/a");
    } else {
        println!(
            "  depth at quit     p50 {}  max {}",
            percentile(&report.depths, 0.50),
            report.depths[report.depths.len() - 1]
        );
    }
    println!();
}

fn main() -> ExitCode {
    let selected: Vec<String> = env::args()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .collect();

    set_global_default(LoadSubscriber).expect("no other global tracing subscriber is installed");

    let executor: TokioRuntime = Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    let matches = |name: &str| selected.is_empty() || selected.iter().any(|arg| arg == name);
    let load_rows: Vec<Cfg> = load_scenarios()
        .into_iter()
        .filter(|cfg| matches(cfg.name))
        .collect();
    let quit_rows: Vec<QuitCfg> = quit_scenarios()
        .into_iter()
        .filter(|row| matches(row.base.name))
        .collect();
    if load_rows.is_empty() && quit_rows.is_empty() {
        let names: Vec<&str> = load_scenarios()
            .into_iter()
            .map(|cfg| cfg.name)
            .chain(quit_scenarios().into_iter().map(|row| row.base.name))
            .collect();
        println!("no matching row; available: {}", names.join(", "));
        return ExitCode::FAILURE;
    }

    println!("# tears kernel load harness (RFC 0014 §13.5)\n");
    let mut incomplete = false;
    for cfg in load_rows {
        let report = executor.block_on(run_load_scenario(cfg));
        incomplete |= report.timed_out;
        print_load_report(&report);
    }
    for row in quit_rows {
        let report = executor.block_on(run_quit_scenario(&row));
        incomplete |= report.incomplete();
        print_quit_report(&report);
    }
    // A row that could not collect its sample is not a measurement, so it
    // fails the run rather than being reported as one.
    if incomplete {
        eprintln!("error: one or more rows did not collect a complete sample");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
