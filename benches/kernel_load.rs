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
//! - **Where each row's quit state is sampled.** A quit row reports three
//!   things about the request — the instant, the data lane's residual
//!   occupancy, and the `blocked` producer gauge — and all three are taken
//!   together, at the requesting route's own site, because the two routes
//!   request from different threads. On the **sync** route that site is the
//!   end of the `update` returning `Command::quit()`, the dispatch whose
//!   completion applies it; the depth is re-read there rather than reused
//!   from the top of `update`, so it is not stale by that update's own
//!   cost. On the **control** route it is the producer task's poll that
//!   yields the quit, immediately before it enters the lane — not the
//!   `update` that returned the run, which merely asks for it and then goes
//!   on batching while the producer is scheduled. The distinction is
//!   load-bearing twice over: `quit->applied` is read against the depth,
//!   and the `blocked` reading decides whether a bounded trial counts
//!   toward its row at all.
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
//! # The two run modes, and which one produces numbers
//!
//! **The full run is the only one whose numbers mean anything.** Every figure
//! this file reports — the percentiles, the depths, the decomposition — comes
//! from a full run at the stated row counts, on a quiet machine, and it is
//! those numbers RFC 0006 §5.2 consumes.
//!
//! **`--smoke` is a regression detector, not a measurement.** It runs a
//! reduced set of rows with the message counts and trial counts cut down, so
//! it finishes in CI time; it asserts *completion and integrity* — the
//! draining rows deliver their exact scripted sequence, the quit rows collect
//! their trials — and asserts **no latency at all**. Its percentiles are
//! printed for the operator's eye and are not comparable with a full run's:
//! shorter rows mean shallower backlogs, and the depth term is precisely what
//! the `quit->applied` bound scales with. Reading a smoke number as an
//! acceptance number would be reading a different measurement.
//!
//! **The blocked-producer sweep is neither.** `probe_blocked_*` varies the
//! producer count at a held lane capacity and backlog, to see what the
//! reclamation of N blocked producers costs. It is **informative** — no
//! acceptance row, no threshold, fewer trials than INV-L4 asks for — and it
//! is reachable only by naming its rows, so a bare run cannot grow to
//! include it. Every quit row additionally reports `applied->exit`, the
//! quiescent postcondition on its own, which is where that reclamation's
//! join drain lands (the abort requests are already inside `quit->applied`;
//! RFC 0011 §4.4's two stages sit either side of that boundary).
//!
//! **`--acceptance` guards the run; it does not choose the rows.** It is
//! independent of `--smoke` and of any row names. Under it the harness
//! enforces RFC 0006 §5.3's two-stage isolation *itself*, rather than
//! leaving it to whatever script starts the harness — a launcher can only
//! observe around a measurement, and what has to be observed is the
//! measurement:
//!
//! **Stage 1, pre-flight**, before any measurement exists: polls until no
//! `cargo`/`rustc` is running, load1 ≤ 2.5, and the largest working process
//! is ≤ 20% CPU — all three together, up to 10 minutes. Exceeding that is
//! not a degraded run but no run: non-zero exit, **nothing recorded**.
//!
//! **Stage 2, in-window**, sampled every 5 s for as long as measuring
//! continues:
//!
//! | In-window condition | Voids the run when |
//! | --- | --- |
//! | `cargo` or `rustc` process | present in any sample |
//! | non-bench working process above 20% CPU | in **3 consecutive** samples (≈15 s) |
//! | non-bench working process above 100% CPU | in **any single** sample |
//! | one-minute load average, bench included | above **5.0** in any sample |
//!
//! **Every sample is written to the record regardless**, including the
//! bursts that do not void, so what the window contained is inspectable
//! rather than summarised by whether it survived. Each line carries every
//! condition's input — load, build processes, top process — not only the one
//! that decided it, so the record answers "was `cargo` running at sample 9?"
//! from itself.
//!
//! Three properties of the implementation are load-bearing, and each is a
//! fail-**open** if got wrong, so each has a negative case in `--self-test`:
//!
//! - **Order.** The three immediate conditions are settled before
//!   persistence is consulted at all ([`decide`]), so a sample that has a
//!   build running *and* a process on a short streak cannot come back
//!   "elevated" and survive.
//! - **Fail-closed reading.** An unreadable load average yields `INFINITY`,
//!   never `NaN`: `NaN` compares false against every threshold and would
//!   sail through as quiet. Not observable is not quiet.
//! - **Sampling boundaries.** The first sample is taken at t=0 rather than
//!   after a cadence, so a run shorter than one cadence cannot succeed with
//!   zero samples; and the harness signals the monitor at the end, waits for
//!   a final sample, and joins before reporting — so "the window held"
//!   covers the whole measurement. A window with no samples is an error,
//!   not an empty success.
//!
//! The persistence requirement on the 20% condition is what that condition
//! is *for*: excluding a concurrent working session — a build, an editor,
//! another agent — which holds a core for as long as it runs. A daemon that
//! wakes, works for a few seconds and sleeps is not that, and three samples
//! is the smallest window that tells them apart at this cadence. The 100%
//! clause is the safety side: one full core is real work by any reading.
//!
//! The load ceiling is higher than pre-flight's because the bench is itself
//! the load once measurement starts, and it is the *aggregate* guard the
//! per-process bound cannot be — several processes at 15% each pass the
//! per-process condition on a machine that is not quiet. Voiding the run
//! rather than the row is deliberate: a disturbance that reached one row has
//! no reason to have spared the ones before it.
//!
//! ```bash
//! cargo bench --bench kernel_load --features bench-internals
//! cargo bench --bench kernel_load --features bench-internals -- overload
//! cargo bench --bench kernel_load --features bench-internals -- --smoke
//! cargo bench --bench kernel_load --features bench-internals -- --acceptance
//! # or, for both harnesses' smoke profiles at once: just bench-smoke
//! cargo bench --bench kernel_load --features bench-internals -- \
//!   probe_blocked_1_sync probe_blocked_8_sync probe_blocked_16_sync \
//!   probe_blocked_32_sync probe_blocked_64_sync probe_blocked_128_sync
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
use std::io::{self, Write};
use std::num::{NonZeroU32, NonZeroUsize};
use std::process::{Command as OsCommand, ExitCode, exit, id};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};
use std::{env, hint, thread};

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

/// The `batch_max_messages` every acceptance row states, pinned here rather
/// than inherited.
///
/// Equal to the kernel's own `DEFAULT_BATCH_MAX_MESSAGES` today, so stating
/// it moves no measured number. What it buys is that it *stays* a stated
/// parameter of the acceptance matrix: leaving the rows unset would let a
/// later change to that default silently re-run the matrix under a different
/// batch bound, and the batch bound is one of the two terms the `decomp_*`
/// rows separate out by varying it.
const BATCH_MAX_MESSAGES: usize = 1024;

/// The per-frame render cost every acceptance row spends, pinned for the
/// same reason as [`BATCH_MAX_MESSAGES`]: it is the other term the
/// `decomp_*` rows vary — those set it to zero to remove the frame's spin —
/// so it is a parameter of the matrix rather than an incidental literal.
const RENDER_COST: Duration = Duration::from_micros(500);

/// Trials per quit row — RFC 0006 INV-L4's "≥ 200 trials per scenario".
const QUIT_TRIALS: u32 = 200;

/// Trials per quit row in the smoke profile.
///
/// Far below INV-L4's minimum, and deliberately: the smoke profile makes no
/// statistical claim, so what this number has to be large enough for is
/// "the trial loop runs and terminates", not "the sample is a sample".
const SMOKE_TRIALS: u32 = 5;

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
    /// `batch_max_messages`, stated by every row rather than inherited from
    /// the kernel's default ([`BATCH_MAX_MESSAGES`], which is that default's
    /// current value). The decomposition rows depart from it, shrinking the
    /// in-progress batch's remainder to nothing.
    batch_cap: usize,
    max_wall: Duration,
}

/// The configuration a row runs under: the lane mode plus the batch cap.
///
/// The cap is not optional here, and that is the point — the acceptance
/// matrix has no unset-parameter state for a kernel default to fill in
/// later.
fn config_for(cfg: &Cfg) -> RuntimeConfig {
    cfg.mode
        .config()
        .batch_max_messages(NonZeroUsize::new(cfg.batch_cap).expect("non-zero cap"))
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
        render_cost: RENDER_COST,
        quit_at_seq: None,
        quit_route: QuitRoute::Sync,
        producers: 1,
        mode: Mode::Unbounded,
        batch_cap: BATCH_MAX_MESSAGES,
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
        render_cost: RENDER_COST,
        quit_at_seq: Some(5_000),
        quit_route: QuitRoute::Sync,
        producers: 1,
        mode: Mode::Unbounded,
        batch_cap: BATCH_MAX_MESSAGES,
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
        ("decomp_50k_batch1", 55_000_u64, 1_usize, RENDER_COST),
        ("decomp_50k_batch1_norender", 55_000, 1, Duration::ZERO),
        ("decomp_300k_batch1_norender", 305_000, 1, Duration::ZERO),
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

// ---- acceptance-mode isolation (RFC 0006 §5.3) -----------------------------
//
// Both stages live here rather than in whatever script starts the harness,
// and that placement is the point of the requirement: a launcher can only
// observe *around* a measurement, and what has to be observed is the
// measurement. Four runs were argued eligible after the fact or checked
// their conditions once at the start instant; a start instant says nothing
// about the minutes that follow.

/// Pre-flight: the machine must be idle before any measurement exists.
const PREFLIGHT_LOAD_MAX: f64 = 2.5;
/// Pre-flight and in-window: the largest single working process.
const PROCESS_CPU_MAX: f64 = 20.0;
/// In-window: a working process this far above the bound is real work by
/// any reading, and voids on sight without waiting for persistence.
const PROCESS_CPU_HARD: f64 = 100.0;
/// In-window: how many *consecutive* samples above [`PROCESS_CPU_MAX`] void
/// the run.
///
/// The 20% condition exists to exclude a concurrent working session — a
/// build, an editor, another agent — and those hold a core for as long as
/// they run. A daemon that wakes, works for a few seconds and sleeps is not
/// that, and three samples is the smallest window that tells them apart at
/// this cadence (≈15 s).
const PROCESS_CPU_PERSIST: u32 = 3;
/// In-window: the one-minute average **with the bench counted in**.
///
/// Higher than pre-flight's on purpose — the bench is itself the load once
/// measurement starts — and it is the aggregate guard the per-process bound
/// cannot be: a machine busy with several processes at 15% each passes the
/// per-process condition and is not quiet.
const WINDOW_LOAD_MAX: f64 = 5.0;
/// How long pre-flight waits for all three conditions to hold together.
const PREFLIGHT_BOUND: Duration = Duration::from_secs(600);
/// Pre-flight poll interval.
const PREFLIGHT_POLL: Duration = Duration::from_secs(20);
/// In-window sampling cadence.
///
/// **Cost**: a sample reads a load average (`getloadavg`, no subprocess) and
/// one `ps` snapshot — on the order of a millisecond. At this cadence a
/// five-and-a-half-minute run takes about 66 of them, so under a tenth of a
/// second of work spread across a run whose shortest row measures for far
/// longer. Negligible against what it protects.
const WINDOW_CADENCE: Duration = Duration::from_secs(5);

/// One observation of the machine, shared by both stages.
struct MachineSample {
    load1: f64,
    top_cpu: f64,
    top_comm: String,
    builders: Vec<String>,
}

impl MachineSample {
    /// Takes a snapshot, optionally excluding one pid from the per-process
    /// maximum (the bench itself, in window).
    fn take(exclude: Option<u32>) -> Self {
        let (top_cpu, top_comm, builders) = process_snapshot(exclude);
        Self {
            load1: load_average_1m(),
            top_cpu,
            top_comm,
            builders,
        }
    }

    /// Which conditions this sample fails, as human-readable clauses.
    fn violations(&self, load_max: f64) -> Vec<String> {
        let mut out = Vec::new();
        if !self.builders.is_empty() {
            out.push(format!(
                "build process present: {}",
                self.builders.join(", ")
            ));
        }
        if self.top_cpu > PROCESS_CPU_MAX {
            out.push(format!(
                "working process {:.1}% > {PROCESS_CPU_MAX:.0}% ({})",
                self.top_cpu, self.top_comm
            ));
        }
        if self.load1 > load_max {
            out.push(format!("load1 {:.2} > {load_max:.1}", self.load1));
        }
        out
    }
}

/// What one in-window sample means for the run.
enum Verdict {
    /// Nothing wrong; any elevated streak is broken.
    Quiet,
    /// Above [`PROCESS_CPU_MAX`] but not yet persistent, and not above the
    /// hard bound. Recorded, and counted toward the streak.
    Elevated(String),
    /// Voids the whole run.
    Void(Vec<String>),
}

impl MachineSample {
    /// Classifies this sample under the in-window rules (RFC 0006 §5.3).
    ///
    /// `streak` is how many immediately preceding samples were already
    /// elevated; it is what turns a burst into a violation.
    fn in_window(&self, streak: u32) -> Verdict {
        match decide(!self.builders.is_empty(), self.load1, self.top_cpu, streak) {
            Decision::Quiet => Verdict::Quiet,
            Decision::Elevated => Verdict::Elevated(format!(
                "{:.1}% ({}) — elevated {}/{PROCESS_CPU_PERSIST}",
                self.top_cpu,
                self.top_comm,
                streak + 1
            )),
            Decision::Void => {
                let mut why = Vec::new();
                if !self.builders.is_empty() {
                    why.push(format!(
                        "build process present: {}",
                        self.builders.join(", ")
                    ));
                }
                if self.load1 > WINDOW_LOAD_MAX {
                    why.push(format!("load1 {:.2} > {WINDOW_LOAD_MAX:.1}", self.load1));
                }
                if self.top_cpu > PROCESS_CPU_HARD {
                    why.push(format!(
                        "working process {:.1}% > {PROCESS_CPU_HARD:.0}% ({}) — voids on sight",
                        self.top_cpu, self.top_comm
                    ));
                } else if self.top_cpu > PROCESS_CPU_MAX {
                    why.push(format!(
                        "working process {:.1}% > {PROCESS_CPU_MAX:.0}% ({}) for {} consecutive samples",
                        self.top_cpu,
                        self.top_comm,
                        streak + 1
                    ));
                }
                Verdict::Void(why)
            }
        }
    }

    /// The line this sample contributes to the record.
    ///
    /// **Every condition's input is on the line, not only the one that
    /// decided it.** A record that printed the load only when the load was
    /// the problem could not answer "was `cargo` running at sample 9?" — and
    /// that is a question an acceptance record has to answer from itself.
    fn record_line(&self, n: u64, verdict: &Verdict) -> String {
        let builders = if self.builders.is_empty() {
            "none".to_owned()
        } else {
            self.builders.join(",")
        };
        let note = match verdict {
            Verdict::Quiet => String::new(),
            Verdict::Elevated(what) => format!("  ELEVATED {what}"),
            Verdict::Void(what) => format!("  VOID {}", what.join("; ")),
        };
        format!(
            "#  sample {n:>3} load1={:.2} cargo/rustc={builders} top={:.1}% ({}){note}",
            self.load1, self.top_cpu, self.top_comm
        )
    }
}

/// The in-window decision, stripped of its diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Decision {
    Quiet,
    Elevated,
    Void,
}

/// Decides one sample, **immediate conditions first**.
///
/// The ordering is the correctness property, not a matter of style. The three
/// immediate conditions — a build process, the load ceiling, a process over
/// the hard bound — are settled before persistence is consulted at all, so a
/// sample that has `cargo` running *and* a 30% process cannot return
/// "elevated" and survive on a short streak. Written as one function over
/// plain values precisely so that ordering lives in a single place and a test
/// can pin it (`--self-test`).
fn decide(builders: bool, load1: f64, top_cpu: f64, streak: u32) -> Decision {
    if builders || load1 > WINDOW_LOAD_MAX || top_cpu > PROCESS_CPU_HARD {
        return Decision::Void;
    }
    if top_cpu > PROCESS_CPU_MAX {
        if streak + 1 >= PROCESS_CPU_PERSIST {
            Decision::Void
        } else {
            Decision::Elevated
        }
    } else {
        Decision::Quiet
    }
}

/// The one-minute load average, without spawning anything.
fn load_average_1m() -> f64 {
    let mut avg = [0.0_f64; 3];
    // SAFETY: `getloadavg` writes at most the requested number of doubles
    // into the caller's buffer, and the buffer holds exactly three.
    let read = unsafe { libc::getloadavg(avg.as_mut_ptr(), 3) };
    // Fail **closed**. A `NaN` here would compare false against every
    // threshold and sail through as quiet, which is the opposite of what an
    // unreadable machine should mean: not observable is not quiet.
    if read >= 1 { avg[0] } else { f64::INFINITY }
}

/// One `ps` snapshot: the largest non-excluded process's CPU share and name,
/// plus any `cargo`/`rustc` found.
///
/// Both facts come from the same snapshot rather than a second `pgrep`, so a
/// sample costs one subprocess.
fn process_snapshot(exclude: Option<u32>) -> (f64, String, Vec<String>) {
    let observed = OsCommand::new("ps")
        .args(["axo", "pid=,pcpu=,comm="])
        .output()
        .ok()
        .and_then(|out| {
            parse_ps(
                out.status.success(),
                &String::from_utf8_lossy(&out.stdout),
                exclude,
            )
        });
    // Fail **closed** on every way the observation can fail — `ps` not
    // spawning, `ps` exiting non-zero, or output with no parsable row. The
    // last two used to arrive here as "top process 0.0%, no builders",
    // which is indistinguishable from a genuinely idle machine and is the
    // most dangerous of the three: a machine nobody could see read as quiet.
    observed.unwrap_or_else(|| (f64::INFINITY, "<ps unobservable>".to_owned(), Vec::new()))
}

/// Parses `ps axo pid=,pcpu=,comm=` output.
///
/// `None` means the machine could not be observed: `ps` failed, or its
/// output held no row this parser understood. A caller must treat that as a
/// failed condition, never as an absence of load.
fn parse_ps(ok: bool, stdout: &str, exclude: Option<u32>) -> Option<(f64, String, Vec<String>)> {
    if !ok {
        return None;
    }
    let (mut top_cpu, mut top_comm) = (0.0_f64, String::from("<none>"));
    let mut builders = Vec::new();
    let mut rows = 0_u32;
    for line in stdout.lines() {
        let mut parts = line.split_whitespace();
        let (Some(pid), Some(cpu)) = (parts.next(), parts.next()) else {
            continue;
        };
        let comm = parts.next().unwrap_or("");
        let (Ok(pid), Ok(cpu)) = (pid.parse::<u32>(), cpu.parse::<f64>()) else {
            continue;
        };
        rows += 1;
        let name = comm.rsplit('/').next().unwrap_or(comm);
        if name == "cargo" || name == "rustc" {
            builders.push(format!("{name}(pid {pid})"));
        }
        if exclude != Some(pid) && cpu > top_cpu {
            top_cpu = cpu;
            comm.clone_into(&mut top_comm);
        }
    }
    // No parsable row at all is an observation failure, not an idle machine:
    // this process is always running, so a working `ps` reports at least it.
    (rows > 0).then_some((top_cpu, top_comm, builders))
}

/// Stage 1. Polls until all three conditions hold **together**, stamps the
/// isolation record, and returns. Exceeding the bound means the run does not
/// exist — the caller exits non-zero with nothing recorded.
///
/// Real wall-clock and a real sleep: the barrier waits for the *machine* to
/// become quiet, which is a fact about the host rather than about this
/// process's executor, so the virtualizable clock RFC 0009 §3.1 mandates has
/// nothing to say about it — the same sanctioned exception the load
/// harnesses' measurement sites take.
#[expect(
    clippy::disallowed_methods,
    reason = "waits on real host quiescence, not executor time; the sanctioned single-time-source exception (RFC 0009 §3.1)"
)]
fn preflight() -> bool {
    println!("# acceptance mode: pre-flight barrier (RFC 0006 §5.3)");
    let started = Instant::now();
    let mut probe = 0_u32;
    loop {
        probe += 1;
        let sample = MachineSample::take(None);
        let failed = sample.violations(PREFLIGHT_LOAD_MAX);
        println!(
            "  probe {probe:>3} t+{:>4}s load1={:.2} top={:.1}% ({}) -> {}",
            started.elapsed().as_secs(),
            sample.load1,
            sample.top_cpu,
            sample.top_comm,
            if failed.is_empty() {
                "MET".to_owned()
            } else {
                failed.join("; ")
            }
        );
        flush_row();
        if failed.is_empty() {
            println!(
                "\n# barrier MET after {probe} probes / {}s\n#   load1 {:.2} (<= {PREFLIGHT_LOAD_MAX})\
                 \n#   largest working process {:.1}% (<= {PROCESS_CPU_MAX:.0}%) {}\
                 \n#   no cargo/rustc\n",
                started.elapsed().as_secs(),
                sample.load1,
                sample.top_cpu,
                sample.top_comm
            );
            flush_row();
            return true;
        }
        if started.elapsed() >= PREFLIGHT_BOUND {
            eprintln!(
                "error: pre-flight barrier not met within {}s ({probe} probes); \
                 last: {}. No measurement was started: this run does not exist.",
                PREFLIGHT_BOUND.as_secs(),
                failed.join("; ")
            );
            return false;
        }
        thread::sleep(PREFLIGHT_POLL);
    }
}

/// How finely the monitor checks for the stop signal while waiting out a
/// cadence, so termination is answered promptly rather than up to a full
/// cadence late.
const STOP_POLL: Duration = Duration::from_millis(100);

/// The running in-window monitor, and the handle that ends it.
struct WindowMonitor {
    stop: Arc<AtomicBool>,
    handle: thread::JoinHandle<()>,
    samples: Arc<AtomicU64>,
    bounds: Arc<Mutex<(f64, f64)>>,
}

impl WindowMonitor {
    /// Starts sampling, **with the first sample already taken and recorded
    /// when this returns**.
    ///
    /// That guarantee is the point, and spawning cannot provide it: a thread
    /// that samples first thing still has to be scheduled, and a short row
    /// can finish before it ever runs. The window would then be "covered" by
    /// a single sample taken *after* measurement ended — one sample, so the
    /// zero-sample check passes, and nothing about the window observed.
    /// So sample one happens here, on the caller's thread, before any
    /// measurement can begin.
    fn start() -> Self {
        let own_pid = id();
        Self::start_with(move || MachineSample::take(Some(own_pid)))
    }

    /// [`start`](Self::start) with an injectable sampler, so the guarantee
    /// above can be tested without depending on the machine being quiet.
    #[expect(
        clippy::disallowed_methods,
        reason = "samples real host state on a real cadence, not executor time (RFC 0009 §3.1)"
    )]
    fn start_with(sampler: impl Fn() -> MachineSample + Send + 'static) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let samples = Arc::new(AtomicU64::new(0));
        let bounds = Arc::new(Mutex::new((f64::INFINITY, f64::NEG_INFINITY)));

        // Sample one, synchronously. Only after this returns can the caller
        // begin measuring, so "first sample at t=0" is a fact rather than a
        // hope about the scheduler.
        let streak = observe(&sampler(), &samples, &bounds, 0);

        let (t_stop, t_samples, t_bounds) =
            (Arc::clone(&stop), Arc::clone(&samples), Arc::clone(&bounds));
        let handle = thread::spawn(move || {
            let mut streak = streak;
            loop {
                let mut waited = Duration::ZERO;
                while waited < WINDOW_CADENCE && !t_stop.load(Ordering::Acquire) {
                    thread::sleep(STOP_POLL);
                    waited += STOP_POLL;
                }
                // Sampled after the wait, so a stop that arrives mid-wait is
                // still followed by one final observation before leaving.
                streak = observe(&sampler(), &t_samples, &t_bounds, streak);
                if t_stop.load(Ordering::Acquire) {
                    break;
                }
            }
        });
        Self {
            stop,
            handle,
            samples,
            bounds,
        }
    }

    /// Signals the monitor, waits for its final sample, and reports the
    /// window.
    ///
    /// Joining before reporting is what makes "the window held" a statement
    /// about the whole measurement rather than about whatever had been
    /// sampled when the last row happened to finish. Zero samples is an
    /// error, not an empty success.
    fn finish(self) -> Result<(u64, f64, f64), String> {
        self.stop.store(true, Ordering::Release);
        if self.handle.join().is_err() {
            return Err("the in-window monitor panicked; the window is unverified".to_owned());
        }
        let n = self.samples.load(Ordering::Relaxed);
        if n == 0 {
            return Err("no in-window samples were taken; the window is unverified".to_owned());
        }
        let (lo, hi) = *self.bounds.lock().unwrap_or_else(PoisonError::into_inner);
        Ok((n, lo, hi))
    }
}

/// Records one sample and returns the updated elevated streak.
///
/// Shared by the synchronous first sample and the monitor thread, so both go
/// through exactly one classification and one recording path.
#[expect(
    clippy::exit,
    reason = "a violated window voids the run at the instant it is observed; returning would let a caller mistake partial output for acceptance evidence (RFC 0006 §5.3)"
)]
fn observe(
    sample: &MachineSample,
    samples: &AtomicU64,
    bounds: &Mutex<(f64, f64)>,
    streak: u32,
) -> u32 {
    let verdict = sample.in_window(streak);
    let n = samples.fetch_add(1, Ordering::Relaxed) + 1;
    if let Ok(mut b) = bounds.lock() {
        b.0 = b.0.min(sample.load1);
        b.1 = b.1.max(sample.load1);
    }
    println!("{}", sample.record_line(n, &verdict));
    flush_row();
    match verdict {
        Verdict::Quiet => 0,
        Verdict::Elevated(_) => streak + 1,
        Verdict::Void(why) => {
            println!("\n# RUN VOID — in-window sample {n}: {}", why.join("; "));
            flush_row();
            eprintln!(
                "error: acceptance window violated at sample {n} ({}); the whole run is void, \
                 not merely the row it landed in. Collected data is not acceptance evidence.",
                why.join("; ")
            );
            // Exits rather than returns: a violated window voids the run at
            // the instant it is observed, and letting the measurement
            // continue would leave partial output a caller could mistake for
            // acceptance evidence (RFC 0006 §5.3).
            exit(4);
        }
    }
}

/// Negative tests for the isolation logic, run by `--self-test`.
///
/// These exist because each one is a fail-**open** that a passing run cannot
/// reveal: a monitor that wrongly calls a disturbed window quiet produces a
/// clean-looking record, so only a deliberately disturbed input distinguishes
/// a working guard from a broken one. A bench with `harness = false` gets no
/// `cargo test` harness, hence a flag rather than `#[test]`.
///
/// # Panics
///
/// Panics on the first case that does not hold; that is the report.
fn self_test() {
    self_test_decision_rules();
    self_test_monitor_boundaries();
    self_test_observation_failure();
    println!("self-test: all isolation negative cases hold");
}

/// The in-window decision rules: ordering, fail-closed reading, persistence.
fn self_test_decision_rules() {
    // P1-1, the ordering bug this replaced: a build process present **and** a
    // process between the two CPU bounds, on a fresh streak. Persistence-first
    // returned Elevated and the run survived with `cargo` running.
    assert_eq!(
        decide(true, 1.0, 30.0, 0),
        Decision::Void,
        "a build process must void even when the streak is short"
    );
    assert_eq!(
        decide(false, WINDOW_LOAD_MAX + 0.1, 30.0, 0),
        Decision::Void,
        "an over-ceiling load must void even when the streak is short"
    );
    assert_eq!(
        decide(false, 1.0, PROCESS_CPU_HARD + 0.1, 0),
        Decision::Void,
        "a process over the hard bound voids on sight"
    );
    // P2-1: an unreadable load average must not read as quiet.
    assert_eq!(
        decide(false, f64::INFINITY, 1.0, 0),
        Decision::Void,
        "an unobservable load average must fail closed"
    );
    assert!(
        !f64::NAN.gt(&WINDOW_LOAD_MAX),
        "NaN compares false against every threshold — which is why the reader returns INFINITY"
    );
    // The persistence rule itself, both sides.
    assert_eq!(decide(false, 1.0, 30.0, 0), Decision::Elevated, "burst 1/3");
    assert_eq!(decide(false, 1.0, 30.0, 1), Decision::Elevated, "burst 2/3");
    assert_eq!(
        decide(false, 1.0, 30.0, PROCESS_CPU_PERSIST - 1),
        Decision::Void,
        "three consecutive elevated samples void"
    );
    assert_eq!(
        decide(false, 1.0, 1.0, 0),
        Decision::Quiet,
        "quiet is quiet"
    );
}

/// The monitor's start and end boundaries (P1-1, P1-2).
fn self_test_monitor_boundaries() {
    // P1-1: `start()` must return with sample one already recorded. The old
    // implementation spawned and returned, so on a slow schedule a short row
    // finished first and the only sample landed *after* measurement — one
    // sample, so the zero-sample check passed and nothing was observed.
    // A synthetic quiet sample keeps this independent of the real machine.
    let quiet = || MachineSample {
        load1: 0.5,
        top_cpu: 1.0,
        top_comm: "<self-test>".to_owned(),
        builders: Vec::new(),
    };
    let started = WindowMonitor::start_with(quiet);
    assert!(
        started.samples.load(Ordering::Relaxed) >= 1,
        "start() must return with the first sample already taken"
    );
    let (n, lo, hi) = started.finish().expect("a sampled window reports held");
    assert!(
        n >= 2,
        "finish() takes a final sample after the stop signal"
    );
    assert!(
        lo.is_finite() && hi.is_finite(),
        "load bounds must be real once any sample exists, never inf..-inf"
    );

    // P1-2: every guarded path ends through `close_window`, so a window that
    // cannot be verified overrides the measurement's own outcome.
    let unverifiable = WindowMonitor {
        stop: Arc::new(AtomicBool::new(true)),
        handle: thread::spawn(|| {}),
        samples: Arc::new(AtomicU64::new(0)),
        bounds: Arc::new(Mutex::new((f64::INFINITY, f64::NEG_INFINITY))),
    };
    let code = format!("{:?}", close_window(Some(unverifiable), ExitCode::SUCCESS));
    assert_eq!(
        code,
        format!("{:?}", ExitCode::from(5)),
        "an unverified window must override a successful measurement"
    );
    assert_eq!(
        format!("{:?}", close_window(None, ExitCode::SUCCESS)),
        format!("{:?}", ExitCode::SUCCESS),
        "an unguarded run passes its own outcome through unchanged"
    );
}

/// Observation failure must never read as an idle machine (P2).
fn self_test_observation_failure() {
    // P2: every way `ps` can fail is an observation failure, not an idle
    // machine. A non-zero exit used to arrive as "top 0.0%, no builders".
    assert!(
        parse_ps(false, "1 0.0 init\n", None).is_none(),
        "a non-zero `ps` exit is an observation failure"
    );
    assert!(
        parse_ps(true, "", None).is_none(),
        "empty `ps` output is an observation failure"
    );
    assert!(
        parse_ps(true, "not a process row\n", None).is_none(),
        "unparsable `ps` output is an observation failure"
    );
    let (cpu, comm, builders) = parse_ps(true, "1 3.5 launchd\n42 91.0 /usr/bin/cargo\n", None)
        .expect("well-formed ps output parses");
    assert!((cpu - 91.0).abs() < f64::EPSILON, "top CPU is the maximum");
    assert!(comm.contains("cargo"), "top process is named");
    assert_eq!(builders.len(), 1, "a build process is detected by name");
    let excluded = parse_ps(true, "1 3.5 launchd\n42 91.0 /usr/bin/cargo\n", Some(42))
        .expect("parses")
        .0;
    assert!(
        (excluded - 3.5).abs() < f64::EPSILON,
        "the excluded pid is left out of the maximum"
    );

    // A window that took no sample is an error, never an empty success.
    let empty = WindowMonitor {
        stop: Arc::new(AtomicBool::new(true)),
        handle: thread::spawn(|| {}),
        samples: Arc::new(AtomicU64::new(0)),
        bounds: Arc::new(Mutex::new((f64::INFINITY, f64::NEG_INFINITY))),
    };
    assert!(
        empty.finish().is_err(),
        "a zero-sample window must not report as held"
    );
}

/// The blocked-producer counts the probe series sweeps.
const PROBE_PRODUCERS: [u32; 6] = [1, 8, 16, 32, 64, 128];

/// Valid trials per probe row.
///
/// Below INV-L4's 200 because the probe makes no acceptance claim; what it
/// has to be large enough for is a readable tail, not a statistical one.
const PROBE_TRIALS: u32 = 100;

/// The blocked-producer sweep — **informative, and no part of the
/// acceptance matrix**.
///
/// Every row is `quit_blocked_1_sync` with the producer count varied, so
/// the lane capacity, the backlog, the update cost, the quit position and
/// the route are all held and `producers` is the only input that moves.
/// Each row keeps a `BlockedEq(N)` predicate, so a counted trial is one
/// where all N producers were in fact blocked at the quit instant — which
/// is what makes N the number of producers termination has to reclaim
/// rather than the number that merely exist.
///
/// The depth at quit is not perfectly held: bounded depth is
/// `capacity + producers`, so it moves 1,025 → 1,152 across the sweep. That
/// spread is reported beside the latencies rather than argued away.
///
/// **Opt-in by name**, and that is what keeps it out of the acceptance
/// matrix: a bare run reports the acceptance rows and nothing else, so this
/// series cannot drift into a table that is read as acceptance.
fn probe_scenarios() -> Vec<QuitCfg> {
    let template = quit_row_named("quit_blocked_1_sync");
    PROBE_PRODUCERS
        .into_iter()
        .map(|producers| QuitCfg {
            base: Cfg {
                name: leak_name(&format!("probe_blocked_{producers}"), QuitRoute::Sync),
                producers,
                ..template.base.clone()
            },
            trials: PROBE_TRIALS,
            valid_trial: ValidTrial::BlockedEq(u64::from(producers)),
        })
        .collect()
}

/// Looks up a row of the canonical [`load_scenarios`] table by name so a
/// smoke row can derive from it with `..` struct update.
///
/// Panics if the name is absent: a rename in the canonical table must update
/// the smoke profile too, and a silent fallback would let the profile drift
/// back to a stale literal while still reporting success.
fn row_named(name: &str) -> Cfg {
    load_scenarios()
        .into_iter()
        .find(|cfg| cfg.name == name)
        .expect("row name present in the canonical load table")
}

/// Looks up a row of the canonical [`quit_scenarios`] table by name; see
/// [`row_named`].
fn quit_row_named(name: &str) -> QuitCfg {
    quit_scenarios()
        .into_iter()
        .find(|row| row.base.name == name)
        .expect("row name present in the canonical quit table")
}

/// The smoke profile's draining rows: `steady_20k` shortened to ~0.5s of
/// load, and a bounded burst cut to a tenth.
///
/// Derived from the canonical table by name lookup plus struct update, so a
/// retune of the shared fields — costs, mode, capacity — carries over
/// automatically and only the shortened field is stated here. The bounded row
/// takes its own name because its `total` differs from the row it derives
/// from, and a name that reported a different configuration under the full
/// row's name would be worse than a new one.
fn smoke_load_rows() -> Vec<Cfg> {
    vec![
        Cfg {
            total: 10_000,
            ..row_named("steady_20k")
        },
        Cfg {
            name: "burst_20k_bounded",
            total: 20_000,
            ..row_named("burst_200k_bounded")
        },
    ]
}

/// The smoke profile's quit rows: one per **route**, at 5 valid trials each.
///
/// Both routes are here because the route split is what distinguishes this
/// harness from the old one (see the header): a smoke that exercised only the
/// synchronous route would leave the control lane — the route INV-L4's
/// property actually carries to — uncompiled and undriven. The bounded blocked
/// row keeps its `BlockedEq(1)` validity predicate, so the profile also drives
/// the gauge-reading path that decides whether a trial counts.
fn smoke_quit_rows() -> Vec<QuitCfg> {
    vec![
        QuitCfg {
            trials: SMOKE_TRIALS,
            ..quit_row_named("quit_idle_bounded_sync")
        },
        QuitCfg {
            trials: SMOKE_TRIALS,
            base: Cfg {
                total: 50_000,
                ..quit_row_named("quit_blocked_1_control").base
            },
            ..quit_row_named("quit_blocked_1_control")
        },
    ]
}

/// Runs the smoke profile; reports whether it passed.
///
/// The gates are completion and integrity, never latency. A draining row must
/// finish and deliver the exact scripted sequence `0..total` — a total-only
/// check would pass a drop paired with a duplicate — and a quit row must
/// collect its valid trials inside the attempt cap. Nothing here compares a
/// percentile against anything, because a shortened row's percentiles are not
/// the quantity the acceptance matrix is stated over.
fn run_smoke(executor: &TokioRuntime) -> bool {
    println!("# tears kernel load harness — smoke profile\n");
    println!(
        "Completion and integrity only; the percentiles below are shortened rows' and are not \
         acceptance numbers (RFC 0014 §13.5).\n"
    );
    let mut ok = true;

    for cfg in smoke_load_rows() {
        let report = executor.block_on(run_load_scenario(cfg));
        print_load_report(&report);
        if report.timed_out {
            eprintln!("smoke: draining row `{}` timed out", report.cfg.name);
            ok = false;
        }
        if report.seq_broken || report.processed != report.cfg.total {
            eprintln!(
                "smoke: draining row `{}` did not deliver the exact sequence 0..{} \
                 (processed={}, seq_broken={})",
                report.cfg.name, report.cfg.total, report.processed, report.seq_broken,
            );
            ok = false;
        }
    }

    for row in smoke_quit_rows() {
        let report = executor.block_on(run_quit_scenario(&row));
        print_quit_report(&report);
        // Completion only: at this harness's observation point a legal
        // shutdown discard is indistinguishable from an illegal drop, so
        // there is no sequence to gate a quit row on.
        if report.incomplete() {
            eprintln!(
                "smoke: quit row `{}` did not collect its trials",
                report.cfg.name
            );
            ok = false;
        }
    }

    ok
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
///
/// Then drops the previous partitions' high-water marks. Every trial builds
/// its own observer and so its own `runtime_id`, and the quit rows run
/// hundreds of trials each, so keeping them would grow the map for the whole
/// process with nothing ever reading an old entry again. Here is the one
/// point where dropping them is sound: the observer that could emit for
/// those partitions went with the kernel that owned it, its producers have
/// been joined by that kernel's settle, and the gauge sum reaching zero is
/// that partition's own last event — so no `runtime_id` in the map can
/// produce another event, and the monotone guard has nothing left to guard.
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
    GAUGE_PARTITION_SEEN
        .lock()
        .expect("gauge partition high-water mark poisoned")
        .clear();
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
    /// route, in both cases as the last act before the quit leaves
    /// ([`Metrics::snapshot_quit_state`]).
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

    /// Records the state the quit was requested in — the lane's residual
    /// occupancy, the `blocked` producer gauge, and the instant — as one
    /// act, so the three describe one moment.
    ///
    /// **Called at the requesting route's own instant, and that is the
    /// whole point of it being one call.** The two routes request a quit in
    /// different places on different threads (RFC 0014 §3.3), so a snapshot
    /// taken anywhere but the route's own request site describes a moment
    /// the row is not measuring: for the control route the request happens
    /// on a producer task, and between the reducer deciding to ask for one
    /// and the producer reaching its send, the driving thread carries on
    /// with its batch and the gauges move with it. The depth and the
    /// `blocked` reading are what `quit->applied` is interpreted against,
    /// and `blocked` additionally decides whether the trial counts at all
    /// ([`ValidTrial::BlockedEq`]), so a stale pair is not a cosmetic
    /// inaccuracy — it selects a different sample.
    ///
    /// The instant is stamped last, after the two readings, so it is the
    /// nearest of the three to the quit actually leaving.
    fn snapshot_quit_state(&self) {
        self.depth_at_quit
            .store(self.queue_depth(), Ordering::Relaxed);
        self.blocked_at_quit
            .store(self.blocked_live.load(Ordering::Relaxed), Ordering::Relaxed);
        self.quit_requested_ns
            .store(self.elapsed_ns(), Ordering::Relaxed);
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
        match self.cfg.quit_route {
            QuitRoute::Sync => {
                // Taken here, at the end of the `update` that asks for the
                // quit: the synchronous route's request instant is this
                // dispatch's completion, which is the next thing to happen
                // (RFC 0014 §3.3). Deliberately after `spin(update_cost)`
                // rather than beside the `depth` read above, so the depth
                // and the instant describe the same moment.
                self.metrics.snapshot_quit_state();
                Command::quit()
            }
            QuitRoute::Control => {
                // Taken on the producer task, in the poll that yields the
                // quit — immediately before it enters the control lane, and
                // *not* here. The reducer only asks for the run; the request
                // happens when that run reaches its send, and the driving
                // thread keeps batching in between.
                let metrics = Arc::clone(&self.metrics);
                producer_quit(move || {
                    metrics.snapshot_quit_state();
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
    /// Per trial, `exit - applied`: the quiescent postcondition alone.
    ///
    /// Kept as its own per-trial series rather than derived from the two
    /// percentile sets, because `p50(exit) - p50(applied)` pairs values from
    /// different trials and says nothing about either. This one is the
    /// interval between `Kernel::drive` returning and `Kernel::settle`
    /// finishing — that is, the join drain, with the abort requests already
    /// behind it in `applied` (RFC 0011 §4.4's two stages, one on each side
    /// of the boundary).
    settle: Vec<u64>,
    depths: Vec<u64>,
}

impl QuitReport {
    const fn incomplete(&self) -> bool {
        self.applied.len() < self.trials as usize
    }
}

async fn run_quit_scenario(scenario: &QuitCfg) -> QuitReport {
    // Ten attempts per requested trial — the form RFC 0007 §6 states
    // normatively, and stated per row rather than per harness so that a
    // reduced row's cap reduces with it: the smoke profile's 5-trial rows
    // fail after 50 attempts instead of spending a full row's budget before
    // anyone hears about it.
    //
    // Unlike the old harness, this cap binds an `Always` row too. That
    // predicate never misses, but an attempt can still yield no sample — a
    // trial whose kernel timed out, or one whose quit instant was never
    // recorded — and those are exactly the failures that would otherwise
    // retry forever instead of ending the row.
    let attempt_cap = scenario.trials.saturating_mul(10);
    let mut applied = Vec::new();
    let mut exit = Vec::new();
    let mut settle = Vec::new();
    let mut depths = Vec::new();
    let mut attempts = 0;
    let mut failures = 0;
    while (applied.len() as u32) < scenario.trials && attempts < attempt_cap {
        attempts += 1;
        match run_quit_trial(scenario.base.clone(), scenario.valid_trial).await {
            Some(sample) if sample.valid => {
                applied.push(sample.applied_ns);
                exit.push(sample.exit_ns);
                settle.push(sample.exit_ns.saturating_sub(sample.applied_ns));
                depths.push(sample.depth_at_quit);
            }
            Some(_) => {}
            None => failures += 1,
        }
    }
    applied.sort_unstable();
    exit.sort_unstable();
    settle.sort_unstable();
    depths.sort_unstable();
    QuitReport {
        cfg: scenario.base.clone(),
        trials: scenario.trials,
        attempts,
        failures,
        applied,
        exit,
        settle,
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
    flush_row();
}

/// Pushes a finished row's report out of the process's stdout buffer.
///
/// Rust block-buffers stdout when it is not a terminal, so a harness whose
/// output is redirected to a file holds finished rows in an 8 KiB buffer —
/// which is most of a whole run. A run killed part-way then looks as though
/// it produced nothing, when in fact it had measured most of its rows.
///
/// Flushing here rather than relying on a pty is what makes the incremental
/// output a property of the harness instead of a property of how it happened
/// to be invoked: `script(1)` cannot allocate a pty when stdin is a socket,
/// which is exactly the case for a detached launcher.
///
/// It sits on the reporting path, after a row's sample is complete, so it is
/// outside every measured interval. A failed flush is ignored because there
/// is nowhere left to report it to.
fn flush_row() {
    let _ = io::stdout().flush();
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
    println!("  applied->exit     {}", format_ms(&report.settle));
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
    flush_row();
}

fn main() -> ExitCode {
    // `--smoke` runs the reduced CI profile; otherwise positional arguments
    // select full rows by name (other flags, cargo's `--bench` among them,
    // are ignored).
    let args: Vec<String> = env::args().skip(1).collect();
    let smoke = args.iter().any(|arg| arg == "--smoke");
    // Independent of `--smoke`: this one decides whether the run is guarded,
    // not which rows it runs. CI passes only `--smoke` and is unaffected.
    let acceptance = args.iter().any(|arg| arg == "--acceptance");
    if args.iter().any(|arg| arg == "--self-test") {
        self_test();
        return ExitCode::SUCCESS;
    }
    let selected: Vec<String> = args
        .into_iter()
        .filter(|arg| !arg.starts_with('-'))
        .collect();

    set_global_default(LoadSubscriber).expect("no other global tracing subscriber is installed");

    // Stage 1 before anything is measured, stage 2 for as long as measuring
    // continues (RFC 0006 §5.3). Both are the harness's own.
    let monitor = if acceptance {
        if !preflight() {
            return ExitCode::from(3);
        }
        Some(WindowMonitor::start())
    } else {
        None
    };

    let executor: TokioRuntime = Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    if smoke {
        let code = if run_smoke(&executor) {
            ExitCode::SUCCESS
        } else {
            eprintln!("error: smoke profile failed");
            ExitCode::FAILURE
        };
        // Guarded runs end through `close_window` on **every** path, this one
        // included. `--acceptance` guards the run and `--smoke` chooses the
        // rows — the header states them as independent, so a combined
        // invocation has to mean "guard the smoke rows", not "guard nothing".
        // Returning straight from here skipped the final sample, the join and
        // the held check, and exited 0 with the window unverified.
        return close_window(monitor, code);
    }

    let matches = |name: &str| selected.is_empty() || selected.iter().any(|arg| arg == name);
    let load_rows: Vec<Cfg> = load_scenarios()
        .into_iter()
        .filter(|cfg| matches(cfg.name))
        .collect();
    let quit_rows: Vec<QuitCfg> = quit_scenarios()
        .into_iter()
        .filter(|row| matches(row.base.name))
        .collect();
    // Named only. A bare run is the acceptance matrix and nothing else, so
    // the informative sweep never joins it by default (`probe_scenarios`).
    let probe_rows: Vec<QuitCfg> = if selected.is_empty() {
        Vec::new()
    } else {
        probe_scenarios()
            .into_iter()
            .filter(|row| selected.iter().any(|arg| *arg == row.base.name))
            .collect()
    };
    if load_rows.is_empty() && quit_rows.is_empty() && probe_rows.is_empty() {
        let names: Vec<&str> = load_scenarios()
            .into_iter()
            .map(|cfg| cfg.name)
            .chain(quit_scenarios().into_iter().map(|row| row.base.name))
            .chain(probe_scenarios().into_iter().map(|row| row.base.name))
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
    if !probe_rows.is_empty() {
        println!("# blocked-producer sweep (informative; not acceptance rows)\n");
        for row in probe_rows {
            let report = executor.block_on(run_quit_scenario(&row));
            incomplete |= report.incomplete();
            print_quit_report(&report);
        }
    }
    // The window held for every sample taken — the monitor exits the process
    // itself on the first one that does not, so reaching here is the proof.
    // A row that could not collect its sample is not a measurement, so it
    // fails the run rather than being reported as one.
    let code = if incomplete {
        eprintln!("error: one or more rows did not collect a complete sample");
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    };
    close_window(monitor, code)
}

/// Ends a guarded run: final sample, join, and the held check — then the
/// outcome the measurement itself reached.
///
/// The single exit for every guarded path. A window that cannot be verified
/// overrides a successful measurement, because an unverified window is not
/// evidence whatever the rows say.
fn close_window(monitor: Option<WindowMonitor>, code: ExitCode) -> ExitCode {
    let Some(monitor) = monitor else {
        return code;
    };
    match monitor.finish() {
        Ok((n, lo, hi)) => {
            println!(
                "# acceptance window held: {n} samples at {}s cadence, load1 {lo:.2}..{hi:.2}",
                WINDOW_CADENCE.as_secs()
            );
            flush_row();
            code
        }
        Err(why) => {
            eprintln!("error: {why}");
            ExitCode::from(5)
        }
    }
}
