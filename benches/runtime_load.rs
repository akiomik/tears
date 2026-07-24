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
//!   the scenario runs; in the default (unbounded) mode this is the direct
//!   driver of memory growth under overload. `produced` counts a message when
//!   the source stream yields it and `processed` counts it when `update` begins
//!   (once it has left the channel), so under the bounded configuration the
//!   observable bound is `capacity + producers`: each producer blocked in
//!   `send` holds one in-flight message outside the channel, while the message
//!   currently inside `update` is already excluded (RFC 0006 section 5.1).
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
//! - **quit -> delivered**: quit request to the event loop's quit branch
//!   observing it. Delivery is not observable through the public API, so a
//!   process-global tracing subscriber timestamps the runtime's own
//!   `quit signal received` / `keyed quit signal received` debug events
//!   (target `tears::runtime`) — the delivery instant itself, suitable as
//!   an INV-L4 acceptance measurement.
//! - **quit -> exit**: quit request to `Runtime::run` returning, which
//!   additionally includes shutdown and backlog deallocation; the latter
//!   scales with queue depth and must not be misread as delivery latency.
//!
//! The keyed variant sends the quit through a cancellable command's private
//! channel instead of the dedicated quit channel, quantifying the INV-14
//! shared-first bias for in-band quit (RFC 0006 open question 7).
//!
//! Scenarios come in the default (unbounded) mode and, for the rows RFC 0007
//! §5 defines, bounded re-runs under the §5.1 configuration (the `*_bounded`
//! scenarios and the `quit_blocked_*` / `quit_keyed_bounded` rows), built with
//! `Runtime::with_config`. The bounded quit rows check a valid-trial predicate
//! (a `blocked`-gauge or capacity-wait-churn reading) at the quit instant and
//! retry predicate misses up to an attempt cap (RFC 0007 §5.2).
//!
//! Run all scenarios, name a subset, or run the CI smoke profile (RFC 0007 §6):
//!
//! ```bash
//! cargo bench --bench runtime_load
//! cargo bench --bench runtime_load -- overload keyed_overload
//! cargo bench --bench runtime_load -- quit_blocked_1 quit_keyed_bounded
//! cargo bench --bench runtime_load -- --smoke   # or: just bench-smoke
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

use std::fmt::Debug;
use std::num::{NonZeroU32, NonZeroUsize};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use std::{env, hint, iter, mem};

use futures::stream::{self, StreamExt};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tears::command::CommandId;
use tears::prelude::*;
use tears::{BoxStream, RuntimeConfig, SubscriptionSource};
use tokio::runtime::{Builder, Runtime as TokioRuntime};
use tokio::task::yield_now;
use tokio::time::{MissedTickBehavior, interval, timeout};
use tokio_stream::wrappers::IntervalStream;
use tracing::field::{Field, Visit};
use tracing::level_filters::LevelFilter;
use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::set_global_default;
use tracing::{Event, Level, Metadata, Subscriber};

/// Message rate for scenarios that emit their whole load in one burst.
const BURST: u64 = 0;

/// The 60 FPS frame rate every scenario runs at (RFC 0006 §2 harness target).
fn frame_rate() -> FrameRate {
    FrameRate::new(NonZeroU32::new(60).expect("non-zero fps"))
        .expect("60 FPS is a valid frame rate")
}

/// RFC 0007 §5.1 bounded capacities. Shared by [`bounded_config`] and the
/// `keyed_isolation` gates so a retune moves the runtime config and the gate
/// expectations (`yields == keyed_cap + 1`, `concurrent_shared_depth ==
/// app_cap + 1`) together rather than leaving one stale.
const APP_CHANNEL_CAPACITY: usize = 1024;
const KEYED_CHANNEL_CAPACITY: usize = 16;

/// The RFC 0007 §5.1 bounded configuration used for every bounded row:
/// `app_channel_capacity = 1024`, `keyed_channel_capacity = 16`,
/// `batch_max_messages = None`, at 60 FPS.
fn bounded_config() -> RuntimeConfig {
    RuntimeConfig::new(frame_rate())
        .app_channel_capacity(NonZeroUsize::new(APP_CHANNEL_CAPACITY).expect("non-zero"))
        .keyed_channel_capacity(NonZeroUsize::new(KEYED_CHANNEL_CAPACITY).expect("non-zero"))
}

/// Delivery mode a scenario's [`Runtime`] is built in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// `Runtime::new` — the load-control-unset (unbounded) default path.
    Default,
    /// `Runtime::with_config` under the RFC 0007 §5.1 bounded configuration.
    Bounded,
}

impl Mode {
    fn build_runtime(self, flags: (ScenarioCfg, Arc<Metrics>)) -> Runtime<LoadApp> {
        match self {
            Self::Default => Runtime::new(flags, frame_rate()),
            Self::Bounded => Runtime::with_config(flags, bounded_config()),
        }
    }
}

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
    /// Delivery mode the runtime is built in (default/unbounded vs the §5.1
    /// bounded configuration).
    mode: Mode,
    /// Number of concurrent flood subscriptions. Several producers all flood
    /// the shared channel, so in bounded mode several block on `send` at once
    /// (the `quit_blocked_*` scenarios' contention, RFC 0007 §5.2).
    producers: u32,
    /// Wall-clock guard; the scenario is aborted and reported as timed out.
    max_wall: Duration,
}

/// The valid-trial predicate a bounded quit scenario checks at the quit
/// instant (RFC 0007 §5.2). Only a predicate *miss* is retried; a quit-contract
/// failure (timeout or missing delivery) fails the row outright.
#[derive(Clone, Copy)]
enum ValidTrial {
    /// `none` — every completed attempt is a valid trial (`quit_idle`).
    Always,
    /// The `blocked` producer gauge reads exactly this at the quit instant
    /// (`quit_blocked_1` = 1, `quit_blocked_64` = 64).
    BlockedEq(u64),
    /// The `blocked` gauge reads at least this (`quit_keyed_bounded` = 1).
    BlockedAtLeast(u64),
    /// At least two shared-channel capacity-wait events in the 5ms preceding
    /// the quit — `quit_overload`'s churn predicate.
    Churn,
}

impl ValidTrial {
    fn holds(self, metrics: &Metrics) -> bool {
        match self {
            Self::Always => true,
            Self::BlockedEq(n) => metrics.blocked_at_quit.load(Ordering::Relaxed) == n,
            Self::BlockedAtLeast(n) => metrics.blocked_at_quit.load(Ordering::Relaxed) >= n,
            Self::Churn => metrics.capacity_waits_before_quit.load(Ordering::Relaxed) >= 2,
        }
    }
}

/// A quit-latency scenario: `base` is run until `trials` *valid* trials are
/// collected (predicate misses are retried up to the attempt cap), and
/// per-trial quit latencies are aggregated into one report.
struct QuitScenarioCfg {
    base: ScenarioCfg,
    trials: u32,
    /// Valid-trial predicate; `Always` needs no attempt cap.
    valid_trial: ValidTrial,
}

#[allow(clippy::too_many_lines, reason = "a flat table of scenario literals")]
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
            producers: 1,
            mode: Mode::Default,
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
            producers: 1,
            mode: Mode::Default,
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
            producers: 1,
            mode: Mode::Default,
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
            producers: 1,
            mode: Mode::Default,
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
            producers: 1,
            mode: Mode::Default,
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
            producers: 1,
            mode: Mode::Default,
            max_wall: Duration::from_secs(60),
        },
        // Bounded re-runs under the §5.1 configuration (RFC 0007 §5.3): the
        // same RFC 0006 §2 load parameters, but built with `Runtime::with_config`
        // so the shared channel is bounded to 1024 and each keyed channel to 16.
        // Compared cell-by-cell against the unbounded rows above.
        ScenarioCfg {
            name: "burst_200k_bounded",
            rate: BURST,
            total: 200_000,
            update_cost: Duration::from_micros(2),
            render_cost: Duration::from_micros(500),
            keyed_probe: false,
            quit_at_seq: None,
            keyed_quit: false,
            producers: 1,
            mode: Mode::Bounded,
            max_wall: Duration::from_secs(30),
        },
        ScenarioCfg {
            name: "overload_bounded",
            rate: 100_000,
            total: 500_000,
            update_cost: Duration::from_micros(25),
            render_cost: Duration::from_micros(500),
            keyed_probe: false,
            quit_at_seq: None,
            keyed_quit: false,
            producers: 1,
            mode: Mode::Bounded,
            max_wall: Duration::from_secs(60),
        },
        ScenarioCfg {
            name: "keyed_overload_bounded",
            rate: 100_000,
            total: 500_000,
            update_cost: Duration::from_micros(25),
            render_cost: Duration::from_micros(500),
            keyed_probe: true,
            quit_at_seq: None,
            keyed_quit: false,
            producers: 1,
            mode: Mode::Bounded,
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

#[allow(clippy::too_many_lines, reason = "a flat table of scenario literals")]
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
        producers: 1,
        mode: Mode::Default,
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
            valid_trial: ValidTrial::Always,
        },
        // Quit while ~50k messages are still queued.
        QuitScenarioCfg {
            base: ScenarioCfg {
                name: "quit_backlog_50k",
                total: 55_000,
                ..base.clone()
            },
            trials: QUIT_TRIALS,
            valid_trial: ValidTrial::Always,
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
            valid_trial: ValidTrial::Always,
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
            valid_trial: ValidTrial::Always,
        },
        // Keyed quit under the 50k backlog: delivered through the command's
        // private channel, so INV-14 shared-first pull defers it until the
        // shared backlog drains (expected latency ~ full drain time).
        QuitScenarioCfg {
            base: ScenarioCfg {
                name: "quit_keyed_backlog_50k",
                total: 55_000,
                keyed_quit: true,
                ..base.clone()
            },
            trials: KEYED_QUIT_TRIALS,
            valid_trial: ValidTrial::Always,
        },
        // Bounded quit rows (RFC 0007 §5.2). Under the §5.1 configuration the
        // shared channel caps at 1024, so these vary the blocked-producer count
        // and channel-full churn instead of raw depth. A large `total` keeps the
        // burst producers blocked well past the quit at seq 5000; the valid-trial
        // predicate is checked at the quit instant and misses are retried.
        //
        // The bounded `quit_idle` baseline: INV-L4 covers both delivery modes,
        // so quit→delivered is measured bounded as well as unbounded. Its `none`
        // predicate needs no blocked producers, so it keeps the empty-queue load.
        QuitScenarioCfg {
            base: ScenarioCfg {
                name: "quit_idle_bounded",
                total: 1,
                quit_at_seq: Some(0),
                mode: Mode::Bounded,
                ..base.clone()
            },
            trials: QUIT_TRIALS,
            valid_trial: ValidTrial::Always,
        },
        QuitScenarioCfg {
            base: ScenarioCfg {
                name: "quit_blocked_1",
                total: 500_000,
                mode: Mode::Bounded,
                ..base.clone()
            },
            trials: QUIT_TRIALS,
            valid_trial: ValidTrial::BlockedEq(1),
        },
        QuitScenarioCfg {
            base: ScenarioCfg {
                name: "quit_blocked_64",
                total: 500_000,
                producers: 64,
                mode: Mode::Bounded,
                ..base.clone()
            },
            trials: QUIT_TRIALS,
            valid_trial: ValidTrial::BlockedEq(64),
        },
        QuitScenarioCfg {
            base: ScenarioCfg {
                name: "quit_overload_bounded",
                rate: 100_000,
                total: 500_000,
                mode: Mode::Bounded,
                ..base.clone()
            },
            trials: QUIT_TRIALS,
            valid_trial: ValidTrial::Churn,
        },
        QuitScenarioCfg {
            base: ScenarioCfg {
                name: "quit_keyed_bounded",
                total: 500_000,
                keyed_quit: true,
                mode: Mode::Bounded,
                ..base
            },
            trials: KEYED_QUIT_TRIALS,
            valid_trial: ValidTrial::BlockedAtLeast(1),
        },
    ]
}

/// The quit trial whose [`Metrics`] receive the next quit-delivery event;
/// `None` outside quit trials, so load scenarios' completion quits are
/// ignored. Trials run sequentially, so a single slot suffices.
static TRIAL_METRICS: Mutex<Option<Arc<Metrics>>> = Mutex::new(None);

/// The current producer-gauge sum
/// (`subscriptions + unkeyed_commands + keyed_commands + blocked`), updated by
/// [`QuitDeliverySubscriber`] from the greatest-`seq` gauge event regardless of
/// the trial slot. Reaches 0 only when the *current* runtime has fully torn its
/// producers down; scenarios run one runtime at a time, so [`await_quiescence`]
/// can wait on it as a common teardown barrier before the next runtime starts,
/// keeping a late gauge/capacity event from one scenario out of the next
/// scenario's slot.
static LIVE_PRODUCERS: AtomicU64 = AtomicU64::new(0);

/// Newest producer-gauge `seq` applied for the current runtime (RFC 0006 §4.4).
/// A gauge event's values reach [`LIVE_PRODUCERS`] and [`Metrics::blocked_live`]
/// only when its `seq` advances this high-water mark, so a reordered stale gauge
/// event never supersedes the current value — the schema orders gauge events by
/// `seq`, not by arrival. It is a `Mutex`, not an atomic, so the advance and the
/// value stores it guards happen as one step even if a future runtime dispatches
/// gauge events off several producer threads at once. [`await_quiescence`] resets
/// it to 0 per runtime, since `seq` restarts at 0 with each new runtime's
/// `LoadObserver`.
static GAUGE_SEQ_SEEN: Mutex<u64> = Mutex::new(0);

/// Waits until the current runtime's producers have fully torn down (the gauge
/// sum returns to 0) before the caller starts the next runtime — the teardown
/// barrier shared by every runtime scenario. A teardown that never quiesces is
/// a harness fault: this aborts the whole run rather than proceeding on a slot
/// a straggler event could still corrupt.
async fn await_quiescence() {
    let quiesced = timeout(Duration::from_secs(5), async {
        while LIVE_PRODUCERS.load(Ordering::Relaxed) != 0 {
            yield_now().await;
        }
    })
    .await;
    assert!(
        quiesced.is_ok(),
        "harness fault: a scenario's producers did not quiesce within 5s; \
         refusing to reuse the trial slot with a teardown still in flight",
    );
    // The next runtime starts a fresh `LoadObserver` whose gauge `seq` restarts
    // at 0, so clear the high-water mark; the barrier above guarantees the
    // drained runtime emits no further gauge event that this reset could let a
    // stale reading through.
    *GAUGE_SEQ_SEEN
        .lock()
        .expect("gauge seq high-water mark poisoned") = 0;
}

/// Records the instant the event loop's quit branch fires.
///
/// The runtime logs `debug!(target: "tears::runtime", "quit signal
/// received")` (dedicated-channel quit) or `"keyed quit signal received"`
/// (keyed quit surfacing through the input mux) at the moment its `select!`
/// observes the quit — the delivery instant INV-L4 is about, which is not
/// observable through the public API. This subscriber is installed once as
/// the process-global tracing subscriber and timestamps that event into the
/// current trial's metrics. If the runtime's message strings ever change,
/// quit trials fail loudly as "no delivery event" instead of silently
/// reporting skewed numbers.
struct QuitDeliverySubscriber;

impl Subscriber for QuitDeliverySubscriber {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        // The quit-delivery message (`tears::runtime`) and the load gauges /
        // capacity-wait events (`tears::runtime::load`) are all DEBUG; the
        // batch event (TRACE) is not needed here.
        metadata.is_event()
            && *metadata.level() == Level::DEBUG
            && matches!(metadata.target(), "tears::runtime" | "tears::runtime::load")
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(LevelFilter::DEBUG)
    }

    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    // The gauge high-water guard (`seen`) is deliberately held across the value
    // stores below: releasing it after `*seen = seq` — the nursery lint's
    // suggestion — would let a concurrent stale gauge event interleave its store
    // between the check and the apply, the exact reorder the `seq` ordering
    // exists to defeat (RFC 0006 §4.4).
    #[allow(clippy::significant_drop_tightening)]
    fn event(&self, event: &Event<'_>) {
        let is_load = event.metadata().target() == "tears::runtime::load";
        let mut visitor = LoadVisitor::default();
        event.record(&mut visitor);

        // A producer-gauge event is a load-target event carrying `seq` (and
        // `blocked`). Match on the target too, not on `seq` alone: were a `seq`
        // field ever added to a `tears::runtime` DEBUG event, matching on `seq`
        // alone would swallow it here and skip the quit-delivery match below.
        // Order these by `seq`, not by arrival: apply the event's values only
        // when its `seq` advances the high-water mark, so a reordered stale
        // gauge event never supersedes the current value (RFC 0006 §4.4).
        // Holding the high-water lock across the value stores makes "advance and
        // apply" one step, so concurrent dispatch cannot interleave a stale
        // store between the check and the apply. This gates both the
        // slot-independent teardown barrier and the per-trial `blocked` reading.
        // The trial slot is cloned out first, before the high-water lock, so the
        // two locks are never held at once.
        if is_load && let Some(seq) = visitor.seq {
            let slot = TRIAL_METRICS
                .lock()
                .expect("trial metrics slot poisoned")
                .clone();
            let mut seen = GAUGE_SEQ_SEEN
                .lock()
                .expect("gauge seq high-water mark poisoned");
            if seq <= *seen {
                return;
            }
            *seen = seq;
            LIVE_PRODUCERS.store(visitor.gauge_sum(), Ordering::Relaxed);
            if let (Some(metrics), Some(blocked)) = (slot, visitor.blocked) {
                metrics.blocked_live.store(blocked, Ordering::Relaxed);
            }
            return;
        }

        let Some(metrics) = TRIAL_METRICS
            .lock()
            .expect("trial metrics slot poisoned")
            .clone()
        else {
            return;
        };

        if is_load {
            // Capacity-wait event (per-occurrence, not seq-ordered): log shared
            // capacity-wait instants for the churn window while a slot is active.
            if visitor.channel.as_deref() == Some("shared") {
                metrics
                    .capacity_wait_shared_ns
                    .lock()
                    .expect("capacity-wait log poisoned")
                    .push(metrics.elapsed_ns());
            }
        } else if visitor.matched_quit {
            metrics
                .quit_delivered_ns
                .store(metrics.elapsed_ns(), Ordering::Relaxed);
        }
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

#[derive(Default)]
struct LoadVisitor {
    matched_quit: bool,
    /// Present only on a producer-gauge event: its monotone ordering counter
    /// (RFC 0006 §4.4). Its presence identifies the event as a gauge event, and
    /// its value orders it against the other gauge events.
    seq: Option<u64>,
    subscriptions: Option<u64>,
    unkeyed_commands: Option<u64>,
    keyed_commands: Option<u64>,
    blocked: Option<u64>,
    channel: Option<String>,
}

impl LoadVisitor {
    /// Sum of the four producer gauges on this event (all present together on a
    /// gauge event); 0 for non-gauge events.
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

    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        if field.name() == "message" {
            let text = format!("{value:?}");
            if text == "quit signal received" || text == "keyed quit signal received" {
                self.matched_quit = true;
            }
        }
    }
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
    /// Nanoseconds from `start` at which the event loop's quit branch
    /// observed the quit, recorded by [`QuitDeliverySubscriber`] from the
    /// runtime's own tracing events; 0 while undelivered.
    quit_delivered_ns: AtomicU64,
    /// Next flood seq `update` expects to process. A single flood producer
    /// feeds the shared FIFO in seq order, so a lossless in-order drain sees
    /// `0, 1, …`; the smoke profile's draining scenarios assert this exactly
    /// (RFC 0007 §6). Also the count of processed flood messages.
    seq_next: AtomicU64,
    /// Set if a processed flood seq ever differs from the expected next seq —
    /// a drop, duplicate, reorder, or lost tail (RFC 0007 §6).
    seq_broken: AtomicBool,
    /// The `blocked` producer-gauge value observed at the instant `update`
    /// requested the quit, captured by [`QuitDeliverySubscriber`] from the
    /// live gauge; the bounded quit scenarios' valid-trial predicate reads it
    /// (RFC 0007 §5.2).
    blocked_at_quit: AtomicU64,
    /// Count of shared-channel capacity-wait events in the 5ms preceding the
    /// quit request — `quit_overload`'s churn predicate (RFC 0007 §5.2).
    capacity_waits_before_quit: AtomicU64,
    /// The most recent `blocked` producer-gauge value, updated live by
    /// [`QuitDeliverySubscriber`] from each `tears::runtime::load` gauge event.
    blocked_live: AtomicU64,
    /// Nanoseconds from `start` of each shared-channel capacity-wait event,
    /// logged live by [`QuitDeliverySubscriber`]; the churn predicate counts
    /// those inside its window.
    capacity_wait_shared_ns: Mutex<Vec<u64>>,
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
            quit_delivered_ns: AtomicU64::new(0),
            seq_next: AtomicU64::new(0),
            seq_broken: AtomicBool::new(false),
            blocked_at_quit: AtomicU64::new(0),
            capacity_waits_before_quit: AtomicU64::new(0),
            blocked_live: AtomicU64::new(0),
            capacity_wait_shared_ns: Mutex::new(Vec::new()),
        }
    }

    /// Pending messages as `produced - processed`. `produced` counts a
    /// message at source emission, not channel admission; `processed` counts
    /// it when `update` begins, once it has left the channel. Under a future
    /// bounded configuration a producer blocked in `send` therefore
    /// contributes the one in-flight message it holds outside the channel,
    /// while the message being processed contributes nothing: the observable
    /// bound is `capacity + producers`, not raw channel occupancy (RFC 0006
    /// section 5.1).
    fn queue_depth(&self) -> u64 {
        let produced = self.produced.load(Ordering::Relaxed);
        let processed = self.processed.load(Ordering::Relaxed);
        produced.saturating_sub(processed)
    }

    fn elapsed_ns(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_nanos()).unwrap_or(u64::MAX)
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
    /// Distinct per producer so several flood subscriptions coexist (the
    /// subscription manager dedupes by key). Seqs stay globally ordered via the
    /// shared `produced` counter regardless of producer count.
    index: u32,
}

impl SubscriptionSource for FloodSource {
    type Output = Msg;
    type Key = u32;

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
        self.index
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
                // Counted when `update` begins, i.e. once the message has
                // left the runtime's channel: queue depth then excludes the
                // message being processed, keeping the bounded-mode
                // acceptance bound at `capacity + producers` instead of
                // adding a consumer in-flight slot (RFC 0006 section 5.1).
                self.processed += 1;
                self.metrics
                    .processed
                    .store(self.processed, Ordering::Relaxed);
                // Seq-integrity: a single flood producer feeds the shared FIFO
                // in seq order, so a lossless in-order drain sees 0, 1, …. Any
                // mismatch is a drop/duplicate/reorder/lost-tail (RFC 0007 §6).
                let expected = self.metrics.seq_next.fetch_add(1, Ordering::Relaxed);
                if seq != expected {
                    self.metrics.seq_broken.store(true, Ordering::Relaxed);
                }
                spin(self.cfg.update_cost);
                if seq % self.sample_every == 0 {
                    Metrics::push_latency(&self.metrics.update_lat_ns, sent_at);
                }
                self.last_processed = Some((seq, sent_at));
                let request_quit = match self.cfg.quit_at_seq {
                    Some(quit_seq) => seq == quit_seq,
                    None => self.processed == self.cfg.total,
                };
                if request_quit {
                    self.metrics
                        .depth_at_quit
                        .store(self.metrics.queue_depth(), Ordering::Relaxed);
                    // Snapshot the predicate input and the latency reference in
                    // O(1), as the last thing before returning the quit, so both
                    // reflect the actual quit instant (RFC 0007 §5.2): the live
                    // `blocked` gauge, then `quit_requested_ns` last. The churn
                    // predicate's window count is computed post-run from the
                    // logged capacity-wait timestamps, off this hot path, so no
                    // history scan sits between the reference reads and the send.
                    self.metrics.blocked_at_quit.store(
                        self.metrics.blocked_live.load(Ordering::Relaxed),
                        Ordering::Relaxed,
                    );
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
    /// A processed flood seq differed from the expected contiguous run — a
    /// drop/duplicate/reorder/lost-tail (RFC 0007 §6 smoke seq-integrity).
    seq_broken: bool,
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

    let runtime = cfg.mode.build_runtime((cfg.clone(), Arc::clone(&metrics)));
    let mut terminal =
        Terminal::new(TestBackend::new(120, 40)).expect("test backend terminal creation");

    let started = Instant::now();
    let timed_out = timeout(cfg.max_wall, runtime.run(&mut terminal))
        .await
        .is_err();
    let wall = started.elapsed();

    // Load scenarios also wait on the teardown barrier: this scenario's flood
    // producer stays alive (its stream chains `pending()`) until shutdown aborts
    // it, so its gauge decrements could otherwise attribute to the next
    // scenario's trial slot (e.g. a quit trial that immediately follows in the
    // smoke profile).
    await_quiescence().await;

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
        seq_broken: metrics.seq_broken.load(Ordering::Relaxed),
        cfg,
    }
}

/// One successful quit trial: queue depth at the quit request plus the two
/// latencies described in the module docs (delivery, and exit including
/// teardown).
struct QuitTrialSample {
    depth: u64,
    to_delivered_ns: u64,
    to_exit_ns: u64,
}

enum QuitTrialFailure {
    /// Quit-contract failure: the run timed out. Fails the row outright, never
    /// retried (RFC 0007 §5.2).
    TimedOut,
    /// Quit-contract failure: the runtime never emitted its quit-delivery
    /// tracing event (see [`QuitDeliverySubscriber`]). Fails the row outright.
    NoDeliveryEvent,
    /// Predicate miss: the quit was delivered, but the valid-trial predicate did
    /// not hold at the quit instant. The *only* retryable outcome (RFC 0007
    /// §5.2).
    PredicateMiss,
}

struct QuitReport {
    cfg: ScenarioCfg,
    /// Required valid trials (the row's configured count).
    trials: u32,
    /// Total attempts made (valid trials + predicate misses + the failing one).
    attempts: u32,
    timeouts: u32,
    missing_delivery: u32,
    predicate_misses: u32,
    /// The attempt cap was reached before enough valid trials were collected.
    cap_exhausted: bool,
    /// Sorted per-trial values (valid trials only).
    depths: Vec<u64>,
    to_delivered_ns: Vec<u64>,
    to_exit_ns: Vec<u64>,
}

impl QuitReport {
    /// The row failed: a quit-contract failure, attempt-cap exhaustion, or an
    /// incomplete sample. Feeds the harness's non-zero exit (RFC 0007 §5.2/§6).
    const fn failed(&self) -> bool {
        self.timeouts > 0
            || self.missing_delivery > 0
            || self.cap_exhausted
            || (self.depths.len() as u32) < self.trials
    }
}

async fn run_quit_trial(
    cfg: ScenarioCfg,
    valid_trial: ValidTrial,
) -> Result<QuitTrialSample, QuitTrialFailure> {
    let metrics = Arc::new(Metrics::new());
    let runtime = cfg.mode.build_runtime((cfg.clone(), Arc::clone(&metrics)));
    let mut terminal =
        Terminal::new(TestBackend::new(120, 40)).expect("test backend terminal creation");

    *TRIAL_METRICS.lock().expect("trial metrics slot poisoned") = Some(Arc::clone(&metrics));
    let timed_out = timeout(cfg.max_wall, runtime.run(&mut terminal))
        .await
        .is_err();
    let exit_ns = metrics.elapsed_ns();

    // Freeze this trial's slot, then wait on the shared teardown barrier so a
    // late gauge/capacity event from this runtime cannot land in the next
    // trial's slot and skew its predicate (RFC 0007 §5.2). Clearing the slot
    // first stops late events from writing this trial's `blocked_live`/log; the
    // barrier then confirms the producers are actually gone — and hard-fails the
    // whole run if they are not, rather than releasing the slot to the next
    // trial with a teardown still in flight.
    *TRIAL_METRICS.lock().expect("trial metrics slot poisoned") = None;
    await_quiescence().await;

    if timed_out {
        return Err(QuitTrialFailure::TimedOut);
    }
    let quit_ns = metrics.quit_requested_ns.load(Ordering::Relaxed);
    let delivered_ns = metrics.quit_delivered_ns.load(Ordering::Relaxed);
    if quit_ns == 0 || delivered_ns == 0 {
        return Err(QuitTrialFailure::NoDeliveryEvent);
    }
    // Churn predicate: count shared capacity-wait events in the 5ms preceding
    // the quit, from the logged timestamps. Computed here, off `update`'s hot
    // path, so no history scan sits between the quit-instant snapshot and the
    // quit send (RFC 0007 §5.2).
    let window_start = quit_ns.saturating_sub(5_000_000);
    let churn = metrics
        .capacity_wait_shared_ns
        .lock()
        .expect("capacity-wait log poisoned")
        .iter()
        .filter(|&&at| at >= window_start && at <= quit_ns)
        .count();
    metrics
        .capacity_waits_before_quit
        .store(u64::try_from(churn).unwrap_or(u64::MAX), Ordering::Relaxed);

    // Predicate checked at the quit instant, from the values `update` snapshot
    // when it requested the quit plus the churn count just computed (RFC 0007
    // §5.2).
    if !valid_trial.holds(&metrics) {
        return Err(QuitTrialFailure::PredicateMiss);
    }
    Ok(QuitTrialSample {
        depth: metrics.depth_at_quit.load(Ordering::Relaxed),
        to_delivered_ns: delivered_ns.saturating_sub(quit_ns),
        to_exit_ns: exit_ns.saturating_sub(quit_ns),
    })
}

async fn run_quit_scenario(scenario: &QuitScenarioCfg) -> QuitReport {
    // Only predicate misses are retried; a quit-contract failure fails the row
    // immediately, and the attempt cap (10 × trials) bounds retries so a rarely
    // held predicate terminates instead of looping forever (RFC 0007 §5.2). The
    // `Always` predicate never misses, so it needs no cap.
    let attempt_cap = match scenario.valid_trial {
        ValidTrial::Always => u32::MAX,
        _ => scenario.trials.saturating_mul(10),
    };

    let mut attempts = 0u32;
    let mut timeouts = 0;
    let mut missing_delivery = 0;
    let mut predicate_misses = 0;
    let mut cap_exhausted = false;
    let mut depths = Vec::new();
    let mut to_delivered_ns = Vec::new();
    let mut to_exit_ns = Vec::new();

    while (depths.len() as u32) < scenario.trials {
        if attempts >= attempt_cap {
            cap_exhausted = true;
            break;
        }
        attempts += 1;
        match run_quit_trial(scenario.base.clone(), scenario.valid_trial).await {
            Ok(sample) => {
                depths.push(sample.depth);
                to_delivered_ns.push(sample.to_delivered_ns);
                to_exit_ns.push(sample.to_exit_ns);
            }
            Err(QuitTrialFailure::PredicateMiss) => predicate_misses += 1,
            // Quit-contract failures fail the row outright — never retried.
            Err(QuitTrialFailure::TimedOut) => {
                timeouts += 1;
                break;
            }
            Err(QuitTrialFailure::NoDeliveryEvent) => {
                missing_delivery += 1;
                break;
            }
        }
    }

    depths.sort_unstable();
    to_delivered_ns.sort_unstable();
    to_exit_ns.sort_unstable();

    QuitReport {
        cfg: scenario.base.clone(),
        trials: scenario.trials,
        attempts,
        timeouts,
        missing_delivery,
        predicate_misses,
        cap_exhausted,
        depths,
        to_delivered_ns,
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
        "   trials: {} valid / {} required ({} attempts, {} predicate misses, \
         {} timed out, {} missing delivery){}",
        report.depths.len(),
        report.trials,
        report.attempts,
        report.predicate_misses,
        report.timeouts,
        report.missing_delivery,
        if report.cap_exhausted {
            ", ATTEMPT-CAP EXHAUSTED"
        } else {
            ""
        },
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
        "   quit -> delivered: {}",
        format_lat(&report.to_delivered_ns)
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

/// Looks up a row of the canonical [`scenarios`] table by name so a smoke row
/// can derive from it with `..` struct update. Panics if the name is absent —
/// a rename in the canonical table must update the smoke profile too, and a
/// silent fallback would let it drift back to a stale literal.
fn scenario_named(name: &str) -> ScenarioCfg {
    scenarios()
        .into_iter()
        .find(|cfg| cfg.name == name)
        .expect("scenario name present in the canonical table")
}

/// Looks up a row of the canonical [`quit_scenarios`] table by name; see
/// [`scenario_named`].
fn quit_scenario_named(name: &str) -> QuitScenarioCfg {
    quit_scenarios()
        .into_iter()
        .find(|cfg| cfg.base.name == name)
        .expect("quit scenario name present in the canonical table")
}

/// The RFC 0007 §6 smoke profile's draining scenarios: `steady_20k` under the
/// default configuration, shortened to ~0.5s of load, and a 20k-message bounded
/// burst under the §5.1 configuration. Derived from the canonical [`scenarios`]
/// table by name lookup + struct update, so a retune of the shared fields
/// (costs, mode, ...) carries over automatically; only the fields the smoke
/// profile intentionally shortens are overridden here.
fn smoke_load_scenarios() -> Vec<ScenarioCfg> {
    vec![
        ScenarioCfg {
            total: 10_000,
            ..scenario_named("steady_20k")
        },
        // Shortened from `burst_200k_bounded`; the smoke profile uses its own
        // name since `total` (and so the scenario) differs from the full row.
        ScenarioCfg {
            name: "burst_20k_bounded",
            total: 20_000,
            ..scenario_named("burst_200k_bounded")
        },
    ]
}

/// The RFC 0007 §6 smoke profile's quit scenarios: `quit_idle_bounded` and
/// `quit_blocked_1` at 5 valid trials each (the attempt cap scales to 50). The
/// row name matches the full quit table's bounded row (the full table also has
/// a separate Default-mode `quit_idle`), so smoke and full report the same
/// configuration under the same name. Derived from the canonical
/// [`quit_scenarios`] table by name lookup + struct update; only `trials`
/// differs from the full row.
fn smoke_quit_scenarios() -> Vec<QuitScenarioCfg> {
    vec![
        QuitScenarioCfg {
            trials: 5,
            ..quit_scenario_named("quit_idle_bounded")
        },
        QuitScenarioCfg {
            trials: 5,
            ..quit_scenario_named("quit_blocked_1")
        },
    ]
}

/// Runs the smoke profile (RFC 0007 §6); returns whether it passed. Draining
/// scenarios must complete with their exact scripted sequence `0..total`; quit
/// scenarios must complete their required valid trials within the attempt cap.
/// No latency is asserted.
fn run_smoke(runtime: &TokioRuntime) -> bool {
    println!("# tears runtime load harness — smoke profile\n");
    let mut ok = true;

    for cfg in smoke_load_scenarios() {
        let report = runtime.block_on(run_scenario(cfg));
        print_report(&report);
        if report.timed_out {
            eprintln!("smoke: draining scenario `{}` timed out", report.cfg.name);
            ok = false;
        }
        // Seq-integrity: every scripted `Msg::Load` seq in `0..total`, once and
        // in order — refutes any drop, duplicate, reorder, or lost tail. A
        // total-only check would pass a drop-plus-duplicate (RFC 0007 §6).
        if report.seq_broken || report.processed != report.cfg.total {
            eprintln!(
                "smoke: draining scenario `{}` did not deliver the exact sequence \
                 0..{} (processed={}, seq_broken={})",
                report.cfg.name, report.cfg.total, report.processed, report.seq_broken,
            );
            ok = false;
        }
    }

    for scenario in smoke_quit_scenarios() {
        let report = runtime.block_on(run_quit_scenario(&scenario));
        print_quit_report(&report);
        // Quit scenarios assert completion only: at the harness's observation
        // point a legal shutdown discard is indistinguishable from an illegal
        // drop, so no seq-integrity gate here (RFC 0007 §6).
        if report.failed() {
            eprintln!("smoke: quit scenario `{}` failed", report.cfg.name);
            ok = false;
        }
    }

    ok
}

// ---- keyed_isolation scenario (RFC 0007 §5.3, INV-L9) ----------------------
//
// Under the §5.1 bounded configuration, eight keyed channels are held saturated
// (each with `keyed_channel_capacity` admitted and its next send blocked, so
// the key's stream has yielded `capacity + 1`), and only *then* is a ninth,
// previously idle probe key started. The probe admitting its own full capacity
// while the eight others hold 128 messages is the keyed→keyed isolation check.
// A shared flood keeps the shared channel full so the event loop stays
// shared-first and never drains any keyed channel; its own admission — a peak
// occupancy of `app_channel_capacity + 1` (the full channel plus one blocked
// send) — is the keyed→shared side, verified while the keyed channels are held.
// This is a behavioral regression check on INV-L9's structural pool-absence
// proof: a shared pool sized near per-channel capacity could not admit all
// `9 × capacity` keyed messages plus the shared channel's full capacity at once.
// Compiled into the harness but not part of the §6 smoke profile.
//
// Delivery is excluded and gated at zero: the keyed `StreamMap` cannot drain a
// chosen key selectively (no keyed-delivery bound exists, RFC 0006 §4.7), so a
// drained keyed message would inflate a key's yield count past `capacity + 1`
// and read as spurious extra admission — the run asserts no keyed message ever
// reaches `update` and that every raw yield is exactly `capacity + 1`.
//
// One deviation from RFC 0007 §5.3's letter, noted for reconciliation: its
// shared probe is worded as a producer started *after* the keyed saturation,
// admitting its first `app_channel_capacity` sends into an empty shared channel.
// That is not realizable here — the shared channel must stay full throughout to
// keep the keyed channels from draining (shared-first), so it cannot be empty at
// a later probe start. The shared producer therefore runs throughout as both the
// saturation enabler and the shared probe, and its full-capacity admission is
// verified concurrently with the held keyed saturation, which carries the same
// isolation evidence.

/// Saturated keyed channels held before the probe starts.
const ISO_SATURATED_KEYS: usize = 8;
/// Total keyed channels: the saturated set plus the probe.
const ISO_KEYS: usize = ISO_SATURATED_KEYS + 1;

struct IsoMetrics {
    /// Yields of each keyed command's stream; a saturated key reads exactly
    /// `keyed_channel_capacity + 1` (capacity admitted, the next send blocked).
    /// A value above that means the channel was drained — an isolation failure.
    /// Each is its own `Arc` so a `'static` keyed stream can own a clone.
    key_yields: Vec<Arc<AtomicU64>>,
    /// Keyed outputs delivered to `update`; must stay 0 (no keyed delivery
    /// during the measurement), else a key's yield count is not admission alone.
    keyed_delivered: AtomicU64,
    shared_produced: AtomicU64,
    shared_processed: AtomicU64,
    /// Max shared occupancy (`produced - processed`) over the whole run — a
    /// display-only signal. Not the isolation gate: a global pool could reach it
    /// before the keyed channels start and then shed capacity to hold them, so
    /// a historical max says nothing about *simultaneous* occupancy.
    max_shared_depth: AtomicU64,
    /// Max shared occupancy sampled *only while all keyed channels are
    /// concurrently saturated* — the isolation gate. A shared pool cannot hold
    /// the full shared channel and all `9 × capacity` keyed messages at once, so
    /// its concurrent shared occupancy stays below `app_channel_capacity + 1`.
    concurrent_shared_depth: AtomicU64,
}

impl IsoMetrics {
    fn new() -> Self {
        Self {
            key_yields: (0..ISO_KEYS).map(|_| Arc::new(AtomicU64::new(0))).collect(),
            keyed_delivered: AtomicU64::new(0),
            shared_produced: AtomicU64::new(0),
            shared_processed: AtomicU64::new(0),
            max_shared_depth: AtomicU64::new(0),
            concurrent_shared_depth: AtomicU64::new(0),
        }
    }

    fn shared_depth(&self) -> u64 {
        self.shared_produced
            .load(Ordering::Relaxed)
            .saturating_sub(self.shared_processed.load(Ordering::Relaxed))
    }

    /// A keyed channel is saturated once its stream has yielded more than
    /// `capacity` — i.e. `capacity + 1`, capacity admitted with the next send
    /// blocked (the final gate checks the count is *exactly* that).
    fn saturated(&self, key: usize, keyed_cap: u64) -> bool {
        self.key_yields[key].load(Ordering::Relaxed) > keyed_cap
    }
}

/// Shared flood messages carry a seq; keyed outputs are a distinct variant so a
/// keyed message reaching `update` (a delivery that must not happen) is caught.
#[derive(Clone)]
enum IsoMsg {
    Flood(u64),
    KeyedOut,
}

/// Infinite shared flood keeping the shared channel saturated.
struct IsoFloodSource {
    iso: Arc<IsoMetrics>,
}

impl SubscriptionSource for IsoFloodSource {
    type Output = IsoMsg;
    type Key = u32;

    fn stream(&self) -> BoxStream<'static, IsoMsg> {
        let iso = Arc::clone(&self.iso);
        stream::repeat(())
            .map(move |()| IsoMsg::Flood(iso.shared_produced.fetch_add(1, Ordering::Relaxed)))
            .boxed()
    }

    fn key(&self) -> Self::Key {
        0
    }
}

/// Infinite keyed-command stream: increments its yield counter per item, so its
/// channel fills to capacity and the next send blocks (the counter then reads
/// `capacity + 1`). Yields the `KeyedOut` variant so any delivery to `update`
/// is detectable.
fn iso_keyed_stream(
    counter: Arc<AtomicU64>,
) -> impl futures::Stream<Item = IsoMsg> + Send + 'static {
    stream::repeat(()).map(move |()| {
        counter.fetch_add(1, Ordering::Relaxed);
        IsoMsg::KeyedOut
    })
}

struct KeyedIsolationApp {
    iso: Arc<IsoMetrics>,
    keyed_cap: u64,
    app_cap: u64,
    /// How many of the eight saturating keys have been started.
    saturators_started: usize,
    /// The probe key has been started (only after all saturators saturated).
    probe_started: bool,
}

impl KeyedIsolationApp {
    fn spawn_key(&self, key: usize) -> Command<IsoMsg> {
        let counter = Arc::clone(&self.iso.key_yields[key]);
        Command::stream(iso_keyed_stream(counter)).cancellable(CommandId::new(key as u64))
    }
}

impl Application for KeyedIsolationApp {
    type Message = IsoMsg;
    type Flags = (Arc<IsoMetrics>, u64, u64);

    fn new((iso, keyed_cap, app_cap): Self::Flags) -> (Self, Command<IsoMsg>) {
        (
            Self {
                iso,
                keyed_cap,
                app_cap,
                saturators_started: 0,
                probe_started: false,
            },
            Command::none(),
        )
    }

    fn update(&mut self, msg: IsoMsg) -> Command<IsoMsg> {
        let seq = match msg {
            // A keyed output reached `update`: keyed delivery occurred, which the
            // scenario forbids during the measurement. Record it; the run fails.
            IsoMsg::KeyedOut => {
                self.iso.keyed_delivered.fetch_add(1, Ordering::Relaxed);
                return Command::none();
            }
            IsoMsg::Flood(seq) => seq,
        };
        let _ = seq;
        // Keep the shared channel full so the event loop stays shared-first and
        // never drains the keyed channels; they must hold saturated.
        spin(Duration::from_micros(10));
        self.iso.shared_processed.fetch_add(1, Ordering::Relaxed);
        // Sample the shared occupancy peak on every shared message.
        self.iso
            .max_shared_depth
            .fetch_max(self.iso.shared_depth(), Ordering::Relaxed);

        // Stage 1: start the eight saturating keys, one per shared message.
        if self.saturators_started < ISO_SATURATED_KEYS {
            let key = self.saturators_started;
            self.saturators_started += 1;
            return self.spawn_key(key);
        }

        // Stage 2: only once all eight are saturated, start the previously idle
        // probe key (index ISO_SATURATED_KEYS).
        let saturators_saturated =
            (0..ISO_SATURATED_KEYS).all(|k| self.iso.saturated(k, self.keyed_cap));
        if !self.probe_started {
            if saturators_saturated {
                self.probe_started = true;
                return self.spawn_key(ISO_SATURATED_KEYS);
            }
            return Command::none();
        }

        // Stage 3: sample the shared depth *only while every keyed channel is
        // concurrently saturated*, and quit once that simultaneous value reaches
        // `app_cap + 1` — the shared channel full at the same instant the nine
        // keyed channels hold their `9 × capacity` messages. A shared pool can
        // reach either alone but not both at once, so its concurrent shared
        // depth never gets there and the scenario times out instead of passing.
        let all_saturated = (0..ISO_KEYS).all(|k| self.iso.saturated(k, self.keyed_cap));
        if all_saturated {
            self.iso
                .concurrent_shared_depth
                .fetch_max(self.iso.shared_depth(), Ordering::Relaxed);
            if self.iso.concurrent_shared_depth.load(Ordering::Relaxed) > self.app_cap {
                return Command::quit();
            }
        }
        Command::none()
    }

    fn view(&self, _frame: &mut ratatui::Frame<'_>) {}

    fn subscriptions(&self) -> Vec<Subscription<IsoMsg>> {
        vec![Subscription::new(IsoFloodSource {
            iso: Arc::clone(&self.iso),
        })]
    }
}

struct IsoReport {
    keyed_cap: u64,
    app_cap: u64,
    timed_out: bool,
    /// Raw yield count per key (exactly `keyed_cap + 1` when cleanly saturated).
    yields: Vec<u64>,
    keyed_delivered: u64,
    /// Whole-run peak shared occupancy — display only.
    max_shared_depth: u64,
    /// Peak shared occupancy while all keyed channels were concurrently
    /// saturated — the isolation gate.
    concurrent_shared_depth: u64,
}

impl IsoReport {
    /// Isolation held, with every gate exact (RFC 0007 §5.3): the run did not
    /// time out, every keyed channel yielded exactly `capacity + 1` (its full
    /// capacity admitted and no drain past it), no keyed message was delivered,
    /// and — the load-bearing gate — the shared channel reached exactly
    /// `app_channel_capacity + 1` *while every keyed channel was concurrently
    /// saturated*, which a shared pool cannot do (it lacks the permits for both
    /// at once).
    fn isolated(&self) -> bool {
        !self.timed_out
            && self.keyed_delivered == 0
            && self.yields.iter().all(|&y| y == self.keyed_cap + 1)
            && self.concurrent_shared_depth == self.app_cap + 1
    }
}

async fn run_keyed_isolation() -> IsoReport {
    let keyed_cap = KEYED_CHANNEL_CAPACITY as u64;
    let app_cap = APP_CHANNEL_CAPACITY as u64;
    let iso = Arc::new(IsoMetrics::new());
    let runtime = Runtime::<KeyedIsolationApp>::with_config(
        (Arc::clone(&iso), keyed_cap, app_cap),
        bounded_config(),
    );
    let mut terminal =
        Terminal::new(TestBackend::new(120, 40)).expect("test backend terminal creation");

    let timed_out = timeout(Duration::from_secs(10), runtime.run(&mut terminal))
        .await
        .is_err();
    await_quiescence().await;

    let yields = iso
        .key_yields
        .iter()
        .map(|counter| counter.load(Ordering::Relaxed))
        .collect();
    IsoReport {
        keyed_cap,
        app_cap,
        timed_out,
        yields,
        keyed_delivered: iso.keyed_delivered.load(Ordering::Relaxed),
        max_shared_depth: iso.max_shared_depth.load(Ordering::Relaxed),
        concurrent_shared_depth: iso.concurrent_shared_depth.load(Ordering::Relaxed),
    }
}

fn print_iso_report(report: &IsoReport) {
    println!("## keyed_isolation");
    println!(
        "   {} keyed channels ({} saturated + 1 probe), capacity {}",
        report.yields.len(),
        ISO_SATURATED_KEYS,
        report.keyed_cap,
    );
    println!(
        "   status: {}",
        if report.timed_out {
            "TIMED OUT — full saturation never coincided with a full shared channel \
             (possible shared pool)"
        } else {
            "ok"
        },
    );
    println!(
        "   per-key raw yields: {:?} (each must equal capacity + 1 = {})",
        report.yields,
        report.keyed_cap + 1,
    );
    println!(
        "   keyed delivered to update: {} (must be 0)",
        report.keyed_delivered,
    );
    println!(
        "   concurrent shared depth: {} (must equal app_channel_capacity + 1 = {}; \
         whole-run max {})",
        report.concurrent_shared_depth,
        report.app_cap + 1,
        report.max_shared_depth,
    );
    println!("   isolation: {}", report.isolated());
    println!();
}

fn main() -> ExitCode {
    // `--smoke` runs the reduced CI profile; otherwise positional arguments
    // select full scenarios by name (other flags, e.g. cargo's `--bench`, are
    // ignored).
    let args: Vec<String> = env::args().skip(1).collect();
    let smoke = args.iter().any(|arg| arg == "--smoke");
    let selected: Vec<String> = args
        .into_iter()
        .filter(|arg| !arg.starts_with('-'))
        .collect();

    // Quit trials read delivery instants and the `blocked` gauge from the
    // runtime's tracing events; installed unconditionally because its filter
    // rejects everything but rare debug events on the runtime targets.
    set_global_default(QuitDeliverySubscriber)
        .expect("no other global tracing subscriber is installed");

    let runtime = Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    if smoke {
        return if run_smoke(&runtime) {
            ExitCode::SUCCESS
        } else {
            eprintln!("error: smoke profile failed");
            ExitCode::FAILURE
        };
    }

    let matches = |name: &str| selected.is_empty() || selected.iter().any(|s| s == name);
    let load_to_run: Vec<ScenarioCfg> = scenarios()
        .into_iter()
        .filter(|cfg| matches(cfg.name))
        .collect();
    let quit_to_run: Vec<QuitScenarioCfg> = quit_scenarios()
        .into_iter()
        .filter(|scenario| matches(scenario.base.name))
        .collect();
    let run_iso = matches("keyed_isolation");
    if load_to_run.is_empty() && quit_to_run.is_empty() && !run_iso {
        let names: Vec<&str> = scenarios()
            .into_iter()
            .map(|cfg| cfg.name)
            .chain(
                quit_scenarios()
                    .into_iter()
                    .map(|scenario| scenario.base.name),
            )
            .chain(iter::once("keyed_isolation"))
            .collect();
        println!("no matching scenario; available: {}", names.join(", "));
        return ExitCode::FAILURE;
    }

    println!("# tears runtime load harness\n");
    for cfg in load_to_run {
        let report = runtime.block_on(run_scenario(cfg));
        print_report(&report);
    }
    let mut any_failed = false;
    for scenario in quit_to_run {
        let report = runtime.block_on(run_quit_scenario(&scenario));
        let failed = report.failed();
        print_quit_report(&report);
        any_failed |= failed;
    }
    if run_iso {
        let report = runtime.block_on(run_keyed_isolation());
        let failed = !report.isolated();
        print_iso_report(&report);
        any_failed |= failed;
    }
    // Quit-trial statistics and the keyed_isolation regression check feed RFC
    // 0006/0007 acceptance criteria, so a row that failed its contract,
    // exhausted its attempt cap, collected a partial sample, or lost isolation
    // must fail the run (and the CI Benchmarks check).
    if any_failed {
        eprintln!("error: one or more scenarios failed");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
