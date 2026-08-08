//! Test-only stage-3 driving surface: scripted arbitration over the
//! production phase machine.
//!
//! This module is the adversarial spike for the common driving contract
//! (architecture-selection gate §H). It substitutes exactly one thing in the
//! production run — the arbitration of which ready kernel branch runs next
//! ([`ScriptedArbiter`] replacing the unbiased `SelectArbiter`) — while the
//! phase executors, task bookkeeping (`JoinSet`s, `SubscriptionManager`,
//! `KeyedCommands`), delivery channels, and termination all remain the single
//! production implementation in `Runtime::run_driven`.
//!
//! Determinism is scripted on two faces:
//!
//! - **kernel branch arbitration**: each [`Directive`] commits the kernel to
//!   one branch (input / frame / quit), and the seam's idle notification
//!   tells the script when the resulting phase has completed;
//! - **producer progress / enqueue arbitration**: real producer tasks (spawned
//!   through the production spawn paths) are gated inside their
//!   application-supplied effect and source bodies, and the script releases
//!   the gates in the scripted order on the current-thread test executor,
//!   settling between releases. This controls readiness supply only — no
//!   manual polling, no direct ingestion, no scheduler instrumentation.
//!
//! Tests never claim the scripted order as a production ordering guarantee:
//! production arbitration stays the unbiased select (RFC 0006 INV-L4) and is
//! negative space (RFC 0011 §2).

use std::convert::Infallible;
use std::future::pending;
use std::io;
use std::num::{NonZeroU32, NonZeroUsize};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;

use futures::FutureExt;
use futures::stream::{self, BoxStream, StreamExt};
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use tokio::sync::{mpsc, oneshot};
use tokio::task::yield_now;
use tokio::time::{Duration, timeout};

use crate::application::Application;
use crate::command::{Command, CommandId};
use crate::subscription::mock::MockSource;
use crate::subscription::{Subscription, SubscriptionSource};
use crate::test_support::{TraceRecorder, with_silent_panic_hook};

use super::config::RuntimeConfig;
use super::core::RuntimeCore;
use super::frame_rate::FrameRate;
use super::frame_scheduler::FrameScheduler;
use super::{KernelArbiter, KernelBranch, Runtime};

// ---------------------------------------------------------------------------
// The scripted arbiter (the seam's test-side implementation)
// ---------------------------------------------------------------------------

/// One scripted arbitration choice: which kernel branch the driver commits
/// the kernel to next (gate §H-2 face (ii)).
#[derive(Clone, Copy, Debug)]
enum Directive {
    /// Await the input branch. The pull itself is the production shared-first
    /// pull over the shared and keyed delivery channels.
    Input,
    /// Await the frame branch (parks until frame work is pending and due —
    /// the production `FrameScheduler` behavior, untouched).
    Frame,
    /// Await the dedicated quit channel.
    Quit,
}

/// Test-side arbitration: commits the kernel to the scripted branch.
///
/// Supplies arbitration only. The branch futures it awaits are the exact
/// production branch futures (`AppInputs::next`, `FrameScheduler::
/// next_work_frame`, `quit_rx.recv`), so input pulls, frame gating, and quit
/// delivery are not reimplemented — only the choice among them is.
struct ScriptedArbiter {
    directives: mpsc::UnboundedReceiver<Directive>,
    /// Signals each seam visit: the previous phase has completed and the
    /// kernel is idle at the arbitration point.
    idle: mpsc::UnboundedSender<()>,
}

impl<App: Application> KernelArbiter<App> for ScriptedArbiter {
    async fn next_branch(
        &mut self,
        core: &mut RuntimeCore<App>,
        scheduler: &mut FrameScheduler,
    ) -> KernelBranch<App::Message> {
        // Reaching the seam means the previous phase completed. A send
        // failure means the script side has finished (terminal directives
        // do not await further seam visits).
        let _ = self.idle.send(());
        let Some(directive) = self.directives.recv().await else {
            // The script ended without a terminal directive: this run is
            // being torn down by dropping the run future (the abrupt
            // termination series), so park instead of inventing a branch.
            loop {
                pending::<()>().await;
            }
        };
        match directive {
            Directive::Input => {
                let input = core
                    .app_inputs
                    .next()
                    .await
                    .expect("scripted Input directive requires an open input source");
                KernelBranch::Input(input)
            }
            Directive::Frame => {
                scheduler.next_work_frame().await;
                KernelBranch::Frame
            }
            Directive::Quit => {
                core.quit_rx.recv().await;
                KernelBranch::Quit
            }
        }
    }
}

/// The test-held end of a scripted run.
struct Script {
    directives: mpsc::UnboundedSender<Directive>,
    idle: mpsc::UnboundedReceiver<()>,
}

impl Script {
    /// Consumes the kernel's first seam visit: bootstrap (init-command
    /// dispatch at construction plus the initial subscription reconcile in
    /// `run_driven`) has completed.
    async fn synchronized(&mut self) {
        self.idle
            .recv()
            .await
            .expect("kernel should reach the arbitration seam");
    }

    /// Directs the kernel to run one branch and waits until the resulting
    /// phase has completed (the kernel is back at the seam).
    async fn step(&mut self, directive: Directive) {
        self.directives
            .send(directive)
            .expect("kernel should be awaiting directives");
        self.idle
            .recv()
            .await
            .expect("kernel should return to the arbitration seam");
    }

    /// Directs a terminal branch: the loop exits (or the run future errors)
    /// before revisiting the seam, so no completion is awaited.
    fn direct(&self, directive: Directive) {
        self.directives
            .send(directive)
            .expect("kernel should be awaiting directives");
    }

    /// Asserts the directed branch stays parked: no seam revisit within one
    /// paused-clock second. Deterministic under `start_paused`: when every
    /// task is idle the clock auto-advances straight to the timeout.
    async fn assert_parked(&mut self, why: &str) {
        let outcome = timeout(Duration::from_secs(1), self.idle.recv()).await;
        assert!(outcome.is_err(), "branch should stay parked: {why}");
    }
}

/// Creates a scripted arbiter and the script that drives it.
fn scripted() -> (ScriptedArbiter, Script) {
    let (directive_tx, directive_rx) = mpsc::unbounded_channel();
    let (idle_tx, idle_rx) = mpsc::unbounded_channel();
    (
        ScriptedArbiter {
            directives: directive_rx,
            idle: idle_tx,
        },
        Script {
            directives: directive_tx,
            idle: idle_rx,
        },
    )
}

// ---------------------------------------------------------------------------
// Test fixtures
// ---------------------------------------------------------------------------

fn frame_rate(value: u32) -> FrameRate {
    FrameRate::new(NonZeroU32::new(value).expect("frame rate must be non-zero"))
        .expect("frame rate must be valid")
}

/// The default driving configuration: production constructor, production
/// knobs only. `batch_max_messages = 1` confines each scripted input step to
/// one pulled input (the paused clock freezes the 100µs window, so the count
/// cap is what bounds a batch — RFC 0006 INV-L12's production semantics).
fn config() -> RuntimeConfig {
    RuntimeConfig::new(frame_rate(60)).batch_max_messages(NonZeroUsize::new(1).expect("non-zero"))
}

fn test_terminal() -> Terminal<TestBackend> {
    Terminal::new(TestBackend::new(80, 24)).expect("test terminal should build")
}

/// A shared, ordered observation log. Every observation point (update calls,
/// renders, producer enqueues) appends here, so one run yields one totally
/// ordered observation sequence — the unit H-2 compares across replays.
#[derive(Clone, Default)]
struct ObsLog(Arc<Mutex<Vec<String>>>);

impl ObsLog {
    fn push(&self, entry: impl Into<String>) {
        self.0
            .lock()
            .expect("observation log lock")
            .push(entry.into());
    }

    fn snapshot(&self) -> Vec<String> {
        self.0.lock().expect("observation log lock").clone()
    }

    fn contains(&self, entry: &str) -> bool {
        self.snapshot().iter().any(|e| e == entry)
    }

    fn count(&self, entry: &str) -> usize {
        self.snapshot().iter().filter(|e| *e == entry).count()
    }

    /// The `update:` entries only — the delivered-message subsequence.
    fn updates(&self) -> Vec<String> {
        self.snapshot()
            .into_iter()
            .filter(|e| e.starts_with("update:"))
            .collect()
    }
}

fn position(log: &[String], entry: &str) -> usize {
    log.iter()
        .position(|e| e == entry)
        .unwrap_or_else(|| unreachable!("observation {entry:?} should be present in {log:?}"))
}

/// Bounded, clock-free settle: yields to the executor until `condition`
/// holds. Success is always condition-observed; the iteration bound only
/// converts a would-be hang into a failed assertion (never a claim that a
/// fixed number of passes reaches quiescence).
async fn settle_until(mut condition: impl FnMut() -> bool, what: &str) {
    for _ in 0..10_000 {
        if condition() {
            return;
        }
        yield_now().await;
    }
    assert!(condition(), "bounded settle exhausted: {what}");
}

/// A fixed number of executor turns — enough for every already-ready
/// single-poll task to complete on the deterministic current-thread
/// scheduler. Used only where no external observation point exists (a keyed
/// quit's send has no app-visible probe); the assertion it supports does not
/// depend on the warm-up having been sufficient.
async fn drain_ready_tasks() {
    for _ in 0..32 {
        yield_now().await;
    }
}

/// Sets a drop flag when the producer's effect body is destroyed — the
/// task-reclamation probe for the termination series.
struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// An unkeyed command whose real producer task parks forever, holding a drop
/// flag: alive owned work for the termination series.
fn parked_command(dropped: Arc<AtomicBool>) -> Command<u32> {
    let guard = DropFlag(dropped);
    Command::stream(stream::poll_fn(move |_| {
        let _keep = &guard;
        Poll::<Option<u32>>::Pending
    }))
}

/// A gated one-message command: the real producer task parks on the gate,
/// and logs `enqueue:{tag}:{value}` in the same poll that sends the message
/// (unbounded sends complete synchronously), so the log order is the enqueue
/// order.
fn gated_message(
    gate: oneshot::Receiver<()>,
    value: u32,
    log: ObsLog,
    tag: &'static str,
) -> Command<u32> {
    Command::stream(
        stream::once(async move {
            let _ = gate.await;
            value
        })
        .map(move |v| {
            log.push(format!("enqueue:{tag}:{v}"));
            v
        }),
    )
}

/// A subscription source whose stream parks forever while tracking, at
/// stream-construction (admission) time, how many of its streams are alive
/// concurrently. `peak` records the maximum concurrency ever observed — the
/// safe-window probe for the stop/restart series.
struct GuardedPendingSource {
    key: u32,
    alive: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

struct AliveGuard(Arc<AtomicUsize>);

impl Drop for AliveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl SubscriptionSource for GuardedPendingSource {
    type Output = u32;
    type Key = u32;

    fn stream(&self) -> BoxStream<'static, u32> {
        let count = self.alive.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(count, Ordering::SeqCst);
        let guard = AliveGuard(Arc::clone(&self.alive));
        Box::pin(stream::poll_fn(move |_| {
            let _keep = &guard;
            Poll::<Option<u32>>::Pending
        }))
    }

    fn key(&self) -> u32 {
        self.key
    }
}

/// A subscription source that emits one message and finishes, logging each
/// admission (`admit:fin`) and each emission (`enqueue:fin`) — the
/// finished-but-still-desired restart probe for the idle-wake series.
struct FiniteSource {
    log: ObsLog,
}

impl SubscriptionSource for FiniteSource {
    type Output = u32;
    type Key = u32;

    fn stream(&self) -> BoxStream<'static, u32> {
        self.log.push("admit:fin");
        let log = self.log.clone();
        Box::pin(stream::iter([5u32]).map(move |v| {
            log.push("enqueue:fin");
            v
        }))
    }

    fn key(&self) -> u32 {
        0
    }
}

/// The external-input subscription every driven app uses: a real
/// `MockSource` (B-2's injection surface) whose forwarder is a real
/// runtime-owned subscription task. The map probe runs inside the forwarder
/// poll, so `enqueue:sub:{v}` marks the shared-channel enqueue.
fn probed_mock(mock: &MockSource<u32>, log: &ObsLog) -> Subscription<u32> {
    let log = log.clone();
    Subscription::new(mock.clone()).map(move |v| {
        log.push(format!("enqueue:sub:{v}"));
        v
    })
}

fn last_gauge(recorder: &TraceRecorder, field: &str) -> Option<u64> {
    recorder.u64_values(field).last().copied()
}

/// The quiescent-postcondition probe: every producer gauge of the (single)
/// runtime under test reads zero in the latest gauge snapshot. Gauge events
/// carry the full field set, so the last event has every current value.
fn producer_gauges_zero(recorder: &TraceRecorder) -> bool {
    [
        "subscriptions",
        "unkeyed_commands",
        "keyed_commands",
        "blocked",
    ]
    .iter()
    .all(|field| last_gauge(recorder, field) == Some(0))
}

/// A backend whose `draw` always fails: the render-error termination route.
/// Everything else delegates to `TestBackend`.
struct FailingBackend(TestBackend);

fn from_infallible<T>(result: Result<T, Infallible>) -> T {
    result.unwrap_or_else(|never| match never {})
}

impl Backend for FailingBackend {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, _content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        Err(io::Error::other("scripted render failure"))
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        from_infallible(self.0.hide_cursor());
        Ok(())
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        from_infallible(self.0.show_cursor());
        Ok(())
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        Ok(from_infallible(self.0.get_cursor_position()))
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        from_infallible(self.0.set_cursor_position(position));
        Ok(())
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        from_infallible(self.0.clear());
        Ok(())
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        from_infallible(self.0.clear_region(clear_type));
        Ok(())
    }

    fn size(&self) -> Result<Size, Self::Error> {
        Ok(from_infallible(self.0.size()))
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        Ok(from_infallible(self.0.window_size()))
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        from_infallible(self.0.flush());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Series 1 — cancel vs buffered output
// ---------------------------------------------------------------------------

/// Message codes: 200 spawn keyed values, 201 arm keyed quit, 202 cancel
/// both, 99 unkeyed quit; anything else is a plain logged update.
struct CancelApp {
    log: ObsLog,
    mock: MockSource<u32>,
}

impl Application for CancelApp {
    type Message = u32;
    type Flags = (ObsLog, MockSource<u32>);

    fn new((log, mock): Self::Flags) -> (Self, Command<u32>) {
        (Self { log, mock }, Command::none())
    }

    fn update(&mut self, msg: u32) -> Command<u32> {
        self.log.push(format!("update:{msg}"));
        match msg {
            200 => {
                let log = self.log.clone();
                Command::stream(stream::iter([1u32, 2]).map(move |v| {
                    log.push(format!("enqueue:kv:{v}"));
                    v
                }))
                .cancellable(CommandId::new("search"))
            }
            201 => Command::quit().cancellable(CommandId::new("kquit")),
            202 => Command::batch([
                Command::cancel(CommandId::new("search")),
                Command::cancel(CommandId::new("kquit")),
            ]),
            99 => Command::quit(),
            _ => Command::none(),
        }
    }

    fn view(&self, _frame: &mut Frame<'_>) {}

    fn subscriptions(&self) -> Vec<Subscription<u32>> {
        vec![probed_mock(&self.mock, &self.log)]
    }
}

// H-5 series "cancel vs buffered output": after the cancel is applied, the
// canceled command's already-buffered messages and its buffered keyed quit
// are not delivered, and the runtime keeps running.
#[tokio::test(start_paused = true)]
async fn s1_cancel_beats_buffered_keyed_output_and_keyed_quit() {
    let log = ObsLog::default();
    let mock = MockSource::new();
    let runtime = Runtime::<CancelApp>::with_config((log.clone(), mock.clone()), config());
    let mut terminal = test_terminal();
    let (arbiter, mut script) = scripted();
    let run = runtime.run_driven(&mut terminal, arbiter);

    let drive = async {
        script.synchronized().await;
        settle_until(|| mock.receiver_count() > 0, "mock forwarder admitted").await;

        // Spawn the keyed value command and let its real task buffer both
        // outputs in its private channel (the enqueue probe logs in the same
        // poll as each unbounded send).
        mock.emit(200).expect("forwarder should be subscribed");
        settle_until(|| log.contains("enqueue:sub:200"), "spawn trigger enqueued").await;
        script.step(Directive::Input).await;
        settle_until(
            || log.contains("enqueue:kv:1") && log.contains("enqueue:kv:2"),
            "keyed outputs buffered before cancellation",
        )
        .await;

        // Arm the keyed quit and give the ready single-poll task its turns so
        // the quit is buffered too (no app-visible probe exists for it; the
        // final assertion does not depend on this warm-up sufficing).
        mock.emit(201).expect("forwarder should be subscribed");
        settle_until(
            || log.contains("enqueue:sub:201"),
            "keyed-quit trigger enqueued",
        )
        .await;
        script.step(Directive::Input).await;
        drain_ready_tasks().await;

        // Cancel both while their outputs sit buffered.
        mock.emit(202).expect("forwarder should be subscribed");
        settle_until(
            || log.contains("enqueue:sub:202"),
            "cancel trigger enqueued",
        )
        .await;
        script.step(Directive::Input).await;

        // The next delivered input is the sentinel — not a canceled value,
        // not the canceled keyed quit.
        mock.emit(55).expect("forwarder should be subscribed");
        settle_until(|| log.contains("enqueue:sub:55"), "sentinel enqueued").await;
        script.step(Directive::Input).await;

        mock.emit(99).expect("forwarder should be subscribed");
        settle_until(|| log.contains("enqueue:sub:99"), "quit trigger enqueued").await;
        script.step(Directive::Input).await;
        script.direct(Directive::Quit);
    };

    let (run_result, ()) = tokio::join!(run, drive);
    run_result.expect("scripted run should exit cleanly");

    // The buffered keyed outputs (values 1 and 2) and the buffered keyed
    // quit were never delivered: the update sequence is exactly the shared
    // inputs, and the run ended by the unkeyed quit, not the canceled one.
    assert_eq!(
        log.updates(),
        vec![
            "update:200",
            "update:201",
            "update:202",
            "update:55",
            "update:99"
        ],
        "cancel must beat already-buffered keyed outputs and the buffered keyed quit"
    );
}

// ---------------------------------------------------------------------------
// Series 2 — subscription stop / restart (safe window)
// ---------------------------------------------------------------------------

/// Message codes: 77 bumps the subscription key (stop + replace), 99 quits.
struct SubSwapApp {
    version: u32,
    mock: MockSource<u32>,
    log: ObsLog,
    alive: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
}

impl Application for SubSwapApp {
    type Message = u32;
    type Flags = (ObsLog, MockSource<u32>, Arc<AtomicUsize>, Arc<AtomicUsize>);

    fn new((log, mock, alive, peak): Self::Flags) -> (Self, Command<u32>) {
        (
            Self {
                version: 1,
                mock,
                log,
                alive,
                peak,
            },
            Command::none(),
        )
    }

    fn update(&mut self, msg: u32) -> Command<u32> {
        self.log.push(format!("update:{msg}"));
        match msg {
            77 => {
                self.version += 1;
                Command::none()
            }
            99 => Command::quit(),
            _ => Command::none(),
        }
    }

    fn view(&self, _frame: &mut Frame<'_>) {}

    fn subscriptions(&self) -> Vec<Subscription<u32>> {
        vec![
            probed_mock(&self.mock, &self.log),
            Subscription::new(GuardedPendingSource {
                key: self.version,
                alive: Arc::clone(&self.alive),
                peak: Arc::clone(&self.peak),
            }),
        ]
    }
}

/// Drives one stop-and-replace cycle and returns the peak number of
/// concurrently alive source streams.
async fn drive_sub_swap() -> (Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let log = ObsLog::default();
    let mock = MockSource::new();
    let alive = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let runtime = Runtime::<SubSwapApp>::with_config(
        (
            log.clone(),
            mock.clone(),
            Arc::clone(&alive),
            Arc::clone(&peak),
        ),
        config(),
    );
    let mut terminal = test_terminal();
    let (arbiter, mut script) = scripted();
    let run = runtime.run_driven(&mut terminal, arbiter);

    let drive = async {
        script.synchronized().await;
        // Bootstrap admitted generation 1.
        assert_eq!(
            alive.load(Ordering::SeqCst),
            1,
            "generation 1 should be admitted"
        );
        settle_until(|| mock.receiver_count() > 0, "mock forwarder admitted").await;

        // Bump the key: the next re-evaluation stops generation 1 and admits
        // generation 2 (a replacement) in the same reconcile pass.
        mock.emit(77).expect("forwarder should be subscribed");
        settle_until(|| log.contains("enqueue:sub:77"), "bump trigger enqueued").await;
        script.step(Directive::Input).await;
        script.step(Directive::Frame).await;

        // The stopped generation eventually quiesces.
        settle_until(
            || alive.load(Ordering::SeqCst) == 1,
            "the stopped generation should quiesce",
        )
        .await;

        mock.emit(99).expect("forwarder should be subscribed");
        settle_until(|| log.contains("enqueue:sub:99"), "quit trigger enqueued").await;
        script.step(Directive::Input).await;
        script.direct(Directive::Quit);
    };

    let (run_result, ()) = tokio::join!(run, drive);
    run_result.expect("scripted run should exit cleanly");
    (alive, peak)
}

// Evidence row (current behavior, passes today): the successor generation is
// admitted while the stopped generation's task has not yet quiesced — the
// replacement's stream is constructed synchronously in the same reconcile
// pass that aborts its predecessor, so both streams are alive at once
// (peak = 2). This is the deterministic witness that RFC 0012 §4's
// quiescence barrier (Accepted, unimplemented) does not hold on current
// code; the driving surface exposes it without forking any phase.
#[tokio::test(start_paused = true)]
async fn s2_replacement_admission_overlaps_stop_quiescence_evidence() {
    let (alive, peak) = drive_sub_swap().await;

    assert_eq!(
        peak.load(Ordering::SeqCst),
        2,
        "current code admits the successor before the stopped task quiesces"
    );
    settle_until(
        || alive.load(Ordering::SeqCst) == 0,
        "teardown should reclaim all streams",
    )
    .await;
}

// H-5 series "subscription stop / restart" — the contract expectation
// (B-2 safe window: no successor admission before the stopped task's
// quiescence). Fails on current code; see the evidence test above.
#[tokio::test(start_paused = true)]
#[ignore = "RFC 0012 §4 quiescence barrier is Accepted but unimplemented: replacement admission currently overlaps the stopped task's quiescence (see s2_replacement_admission_overlaps_stop_quiescence_evidence)"]
async fn s2_safe_window_holds_expected() {
    let (_alive, peak) = drive_sub_swap().await;

    assert!(
        peak.load(Ordering::SeqCst) <= 1,
        "B-2 safe window: a successor must not be admitted before the stopped task quiesced"
    );
}

// ---------------------------------------------------------------------------
// Series 3 — simultaneous readiness (both determinism faces)
// ---------------------------------------------------------------------------

/// Message codes: 100/101 spawn gated producer A/B, 99 quits; anything else
/// is a plain logged update. `view` logs `render`.
struct ArbApp {
    log: ObsLog,
    mock: MockSource<u32>,
    gate_a: Option<oneshot::Receiver<()>>,
    gate_b: Option<oneshot::Receiver<()>>,
}

impl Application for ArbApp {
    type Message = u32;
    type Flags = (
        ObsLog,
        MockSource<u32>,
        oneshot::Receiver<()>,
        oneshot::Receiver<()>,
    );

    fn new((log, mock, gate_a, gate_b): Self::Flags) -> (Self, Command<u32>) {
        (
            Self {
                log,
                mock,
                gate_a: Some(gate_a),
                gate_b: Some(gate_b),
            },
            Command::none(),
        )
    }

    fn update(&mut self, msg: u32) -> Command<u32> {
        self.log.push(format!("update:{msg}"));
        match msg {
            100 => gated_message(
                self.gate_a.take().expect("gate A armed once"),
                1,
                self.log.clone(),
                "cmd",
            ),
            101 => gated_message(
                self.gate_b.take().expect("gate B armed once"),
                2,
                self.log.clone(),
                "cmd",
            ),
            99 => Command::quit(),
            _ => Command::none(),
        }
    }

    fn view(&self, _frame: &mut Frame<'_>) {
        self.log.push("render");
    }

    fn subscriptions(&self) -> Vec<Subscription<u32>> {
        vec![probed_mock(&self.mock, &self.log)]
    }
}

/// One scripted run: two real producer tasks made ready in scripted order
/// (face i), then the input and frame branches both ready and taken in
/// scripted order (face ii). Returns the full observation sequence.
async fn s3_run(a_first: bool, frame_first: bool) -> Vec<String> {
    let log = ObsLog::default();
    let mock = MockSource::new();
    let (gate_a_tx, gate_a_rx) = oneshot::channel();
    let (gate_b_tx, gate_b_rx) = oneshot::channel();
    let runtime =
        Runtime::<ArbApp>::with_config((log.clone(), mock.clone(), gate_a_rx, gate_b_rx), config());
    let mut terminal = test_terminal();
    let (arbiter, mut script) = scripted();
    let run = runtime.run_driven(&mut terminal, arbiter);

    let drive = async {
        script.synchronized().await;
        settle_until(|| mock.receiver_count() > 0, "mock forwarder admitted").await;

        // Spawn both real producer tasks; they park on their gates.
        for trigger in [100u32, 101] {
            mock.emit(trigger).expect("forwarder should be subscribed");
            settle_until(
                || log.contains(&format!("enqueue:sub:{trigger}")),
                "spawn trigger enqueued",
            )
            .await;
            script.step(Directive::Input).await;
        }

        // Face (i): both producers are ready to send to the same shared
        // channel; the script chooses the enqueue order by releasing the
        // gates one at a time and settling in between.
        let (first_gate, first_probe, second_gate, second_probe) = if a_first {
            (gate_a_tx, "enqueue:cmd:1", gate_b_tx, "enqueue:cmd:2")
        } else {
            (gate_b_tx, "enqueue:cmd:2", gate_a_tx, "enqueue:cmd:1")
        };
        first_gate
            .send(())
            .expect("first producer should be parked on its gate");
        settle_until(|| log.contains(first_probe), "first scripted enqueue").await;
        second_gate
            .send(())
            .expect("second producer should be parked on its gate");
        settle_until(|| log.contains(second_probe), "second scripted enqueue").await;

        mock.emit(55).expect("forwarder should be subscribed");
        settle_until(|| log.contains("enqueue:sub:55"), "plain message enqueued").await;

        // Face (ii): the input branch (three queued messages) and the frame
        // branch (redraw pending since the spawn updates) are both ready;
        // the script chooses which runs first.
        if frame_first {
            script.step(Directive::Frame).await;
            for _ in 0..3 {
                script.step(Directive::Input).await;
            }
        } else {
            for _ in 0..3 {
                script.step(Directive::Input).await;
            }
            script.step(Directive::Frame).await;
        }

        mock.emit(99).expect("forwarder should be subscribed");
        settle_until(|| log.contains("enqueue:sub:99"), "quit trigger enqueued").await;
        script.step(Directive::Input).await;
        script.direct(Directive::Quit);
    };

    let (run_result, ()) = tokio::join!(run, drive);
    run_result.expect("scripted run should exit cleanly");
    log.snapshot()
}

// H-5 series "simultaneous readiness": the script chooses both the producer
// enqueue order and the kernel branch order, the observations follow the
// script, and replaying the same script reproduces the same observation
// sequence — H-2's two determinism faces on one run.
#[tokio::test(start_paused = true)]
async fn s3_scripted_readiness_and_arbitration_replay_identically() {
    let ab_frame = s3_run(true, true).await;
    let ab_frame_replay = s3_run(true, true).await;
    assert_eq!(
        ab_frame, ab_frame_replay,
        "the same script must reproduce the same observation sequence"
    );

    let ba_frame = s3_run(false, true).await;
    let ba_frame_replay = s3_run(false, true).await;
    assert_eq!(
        ba_frame, ba_frame_replay,
        "the flipped producer script must also replay identically"
    );

    let ab_input = s3_run(true, false).await;
    let ab_input_replay = s3_run(true, false).await;
    assert_eq!(
        ab_input, ab_input_replay,
        "the flipped kernel script must also replay identically"
    );

    // Face (i): the enqueue order and the delivered order follow the
    // producer script.
    assert!(
        position(&ab_frame, "enqueue:cmd:1") < position(&ab_frame, "enqueue:cmd:2"),
        "producer A must enqueue first when scripted first"
    );
    assert!(
        position(&ba_frame, "enqueue:cmd:2") < position(&ba_frame, "enqueue:cmd:1"),
        "producer B must enqueue first when scripted first"
    );
    assert!(
        position(&ab_frame, "update:1") < position(&ab_frame, "update:2"),
        "delivery must follow the scripted enqueue order"
    );
    assert!(
        position(&ba_frame, "update:2") < position(&ba_frame, "update:1"),
        "delivery must follow the flipped scripted enqueue order"
    );

    // Face (ii): the frame/input interleaving follows the kernel script.
    assert!(
        position(&ab_frame, "render") < position(&ab_frame, "update:1"),
        "frame-first script must render before delivering the queued inputs"
    );
    assert!(
        position(&ab_input, "render") > position(&ab_input, "update:55"),
        "input-first script must deliver the queued inputs before rendering"
    );
}

// ---------------------------------------------------------------------------
// Series 4 — quit (two semantics)
// ---------------------------------------------------------------------------

/// Init sends an unkeyed quit; updates only log.
struct QuitBacklogApp {
    log: ObsLog,
    mock: MockSource<u32>,
}

impl Application for QuitBacklogApp {
    type Message = u32;
    type Flags = (ObsLog, MockSource<u32>);

    fn new((log, mock): Self::Flags) -> (Self, Command<u32>) {
        (Self { log, mock }, Command::quit())
    }

    fn update(&mut self, msg: u32) -> Command<u32> {
        self.log.push(format!("update:{msg}"));
        Command::none()
    }

    fn view(&self, _frame: &mut Frame<'_>) {}

    fn subscriptions(&self) -> Vec<Subscription<u32>> {
        vec![probed_mock(&self.mock, &self.log)]
    }
}

// H-5 series "quit (2 semantics)", immediate half: an unkeyed quit in the
// dedicated channel terminates the run even while the input branch holds a
// ready backlog, and the backlog is discarded, not delivered.
#[tokio::test(start_paused = true)]
async fn s4_unkeyed_quit_is_observed_independently_of_backlog() {
    let log = ObsLog::default();
    let mock = MockSource::new();
    let runtime = Runtime::<QuitBacklogApp>::with_config((log.clone(), mock.clone()), config());
    let mut terminal = test_terminal();
    let (arbiter, mut script) = scripted();
    let run = runtime.run_driven(&mut terminal, arbiter);

    let drive = async {
        script.synchronized().await;
        settle_until(|| mock.receiver_count() > 0, "mock forwarder admitted").await;

        // Build a ready backlog on the shared channel through the real
        // forwarder while the quit signal sits in the dedicated channel.
        for value in [1u32, 2, 3] {
            mock.emit(value).expect("forwarder should be subscribed");
        }
        settle_until(
            || log.contains("enqueue:sub:3"),
            "backlog enqueued on the shared channel",
        )
        .await;

        // Take the quit branch: it must be deliverable without draining the
        // backlog first.
        script.direct(Directive::Quit);
    };

    let (run_result, ()) = tokio::join!(run, drive);
    run_result.expect("quit-terminated run should exit cleanly");

    assert!(
        log.updates().is_empty(),
        "the ready backlog must be discarded, not delivered, on the immediate quit route"
    );
}

/// Init arms a keyed (in-band) quit; updates only log.
struct KeyedQuitApp {
    log: ObsLog,
}

impl Application for KeyedQuitApp {
    type Message = u32;
    type Flags = ObsLog;

    fn new(log: Self::Flags) -> (Self, Command<u32>) {
        (
            Self { log },
            Command::quit().cancellable(CommandId::new("kq")),
        )
    }

    fn update(&mut self, msg: u32) -> Command<u32> {
        self.log.push(format!("update:{msg}"));
        Command::none()
    }

    fn view(&self, _frame: &mut Frame<'_>) {}

    fn subscriptions(&self) -> Vec<Subscription<u32>> {
        vec![]
    }
}

// H-5 series "quit (2 semantics)", in-band half: a keyed quit travels its
// private delivery channel and terminates the run when the input branch
// pulls it. (Its cancellability before delivery is series 1.)
#[tokio::test(start_paused = true)]
async fn s4_keyed_quit_is_delivered_in_band() {
    let log = ObsLog::default();
    let runtime = Runtime::<KeyedQuitApp>::with_config(log.clone(), config());
    let mut terminal = test_terminal();
    let (arbiter, mut script) = scripted();
    let run = runtime.run_driven(&mut terminal, arbiter);

    let drive = async {
        script.synchronized().await;
        // The input branch parks until the real keyed task delivers the quit
        // through its private channel, then the batch maps it to loop exit.
        script.direct(Directive::Input);
    };

    let (run_result, ()) = tokio::join!(run, drive);
    run_result.expect("keyed-quit-terminated run should exit cleanly");

    assert!(
        log.updates().is_empty(),
        "the keyed quit run should terminate without any update"
    );
}

// ---------------------------------------------------------------------------
// Series 5 — panic (two classes)
// ---------------------------------------------------------------------------

/// Message codes: 300 spawns the gated healthy producer, 99 quits.
struct PanicApp {
    log: ObsLog,
    mock: MockSource<u32>,
    healthy_gate: Option<oneshot::Receiver<()>>,
}

impl Application for PanicApp {
    type Message = u32;
    type Flags = (
        ObsLog,
        MockSource<u32>,
        oneshot::Receiver<()>,
        oneshot::Receiver<()>,
    );

    #[expect(
        clippy::panic,
        reason = "the init producer must genuinely panic inside its effect body to drive the containment series"
    )]
    fn new((log, mock, panic_gate, healthy_gate): Self::Flags) -> (Self, Command<u32>) {
        let init = Command::stream(
            stream::once(async move {
                let _ = panic_gate.await;
            })
            .map(|()| -> u32 { panic!("scripted producer panic") }),
        );
        (
            Self {
                log,
                mock,
                healthy_gate: Some(healthy_gate),
            },
            init,
        )
    }

    fn update(&mut self, msg: u32) -> Command<u32> {
        self.log.push(format!("update:{msg}"));
        match msg {
            300 => gated_message(
                self.healthy_gate.take().expect("healthy gate armed once"),
                5,
                self.log.clone(),
                "healthy",
            ),
            99 => Command::quit(),
            _ => Command::none(),
        }
    }

    fn view(&self, _frame: &mut Frame<'_>) {}

    fn subscriptions(&self) -> Vec<Subscription<u32>> {
        vec![probed_mock(&self.mock, &self.log)]
    }
}

// H-5 series "panic (2 kinds)", containment half: a producer task panic is
// contained — the event loop keeps running, the other producers are not
// canceled, and their subsequent outputs are delivered through the normal
// delivery contract.
#[tokio::test(start_paused = true)]
async fn s5_producer_panic_is_contained_and_other_producers_deliver() {
    with_silent_panic_hook(async {
        let recorder = TraceRecorder::new().with_target("tears::runtime::load");
        let _guard = recorder.set_default();
        let log = ObsLog::default();
        let mock = MockSource::new();
        let (panic_gate_tx, panic_gate_rx) = oneshot::channel();
        let (healthy_gate_tx, healthy_gate_rx) = oneshot::channel();
        let runtime = Runtime::<PanicApp>::with_config(
            (log.clone(), mock.clone(), panic_gate_rx, healthy_gate_rx),
            config(),
        );
        let mut terminal = test_terminal();
        let (arbiter, mut script) = scripted();
        let run = runtime.run_driven(&mut terminal, arbiter);

        let drive = async {
            script.synchronized().await;
            settle_until(|| mock.receiver_count() > 0, "mock forwarder admitted").await;
            settle_until(
                || last_gauge(&recorder, "unkeyed_commands") == Some(1),
                "the doomed producer is running",
            )
            .await;

            // Spawn the healthy producer, then release the panic.
            mock.emit(300).expect("forwarder should be subscribed");
            settle_until(|| log.contains("enqueue:sub:300"), "spawn trigger enqueued").await;
            script.step(Directive::Input).await;
            settle_until(
                || last_gauge(&recorder, "unkeyed_commands") == Some(2),
                "both producers are running",
            )
            .await;

            panic_gate_tx
                .send(())
                .expect("doomed producer should be parked on its gate");
            settle_until(
                || last_gauge(&recorder, "unkeyed_commands") == Some(1),
                "the panicked producer's resources are released like a normal exit",
            )
            .await;

            // The surviving producer's subsequent output is delivered.
            healthy_gate_tx
                .send(())
                .expect("healthy producer should be parked on its gate");
            settle_until(|| log.contains("enqueue:healthy:5"), "healthy output enqueued").await;
            script.step(Directive::Input).await;

            mock.emit(99).expect("forwarder should be subscribed");
            settle_until(|| log.contains("enqueue:sub:99"), "quit trigger enqueued").await;
            script.step(Directive::Input).await;
            script.direct(Directive::Quit);
        };

        let (run_result, ()) = tokio::join!(run, drive);
        run_result.expect("the event loop must survive a producer panic");

        assert_eq!(
            log.updates(),
            vec!["update:300", "update:5", "update:99"],
            "the surviving producer's output and the mock's outputs must be delivered after the panic"
        );
    })
    .await;
}

/// Update panics on message 13; init parks an owned producer.
struct FailUpdateApp {
    log: ObsLog,
    mock: MockSource<u32>,
}

impl Application for FailUpdateApp {
    type Message = u32;
    type Flags = (ObsLog, MockSource<u32>, Arc<AtomicBool>);

    fn new((log, mock, parked_dropped): Self::Flags) -> (Self, Command<u32>) {
        (Self { log, mock }, parked_command(parked_dropped))
    }

    fn update(&mut self, msg: u32) -> Command<u32> {
        // The driving-task application panic under test (fail-fast, B-1).
        assert!(msg != 13, "scripted update panic");
        self.log.push(format!("update:{msg}"));
        Command::none()
    }

    fn view(&self, _frame: &mut Frame<'_>) {}

    fn subscriptions(&self) -> Vec<Subscription<u32>> {
        vec![probed_mock(&self.mock, &self.log)]
    }
}

// H-5 series "panic (2 kinds)", fail-fast half: an application panic on the
// driving task propagates out of the run future unchanged (no conversion
// into a continuing run), and unwinding reclaims the owned producers.
#[tokio::test(start_paused = true)]
async fn s5_driving_application_panic_fails_fast_and_reclaims_producers() {
    with_silent_panic_hook(async {
        let recorder = TraceRecorder::new().with_target("tears::runtime::load");
        let _guard = recorder.set_default();
        let log = ObsLog::default();
        let mock = MockSource::new();
        let parked_dropped = Arc::new(AtomicBool::new(false));
        let runtime = Runtime::<FailUpdateApp>::with_config(
            (log.clone(), mock.clone(), Arc::clone(&parked_dropped)),
            config(),
        );
        let mut terminal = test_terminal();
        let (arbiter, mut script) = scripted();
        let run = AssertUnwindSafe(runtime.run_driven(&mut terminal, arbiter)).catch_unwind();

        let drive = async {
            script.synchronized().await;
            settle_until(|| mock.receiver_count() > 0, "mock forwarder admitted").await;
            mock.emit(13).expect("forwarder should be subscribed");
            settle_until(|| log.contains("enqueue:sub:13"), "panic trigger enqueued").await;
            script.direct(Directive::Input);
        };

        let (run_result, ()) = tokio::join!(run, drive);
        let payload = run_result.expect_err("an update panic must propagate out of the run future");
        let message = payload
            .downcast_ref::<&str>()
            .copied()
            .unwrap_or("<non-str payload>");
        assert!(
            message.contains("scripted update panic"),
            "the propagated panic must be the application's own: {message}"
        );

        // Unwinding dropped the runtime: owned producers are reclaimed and
        // the gauges settle to zero.
        settle_until(
            || parked_dropped.load(Ordering::SeqCst) && producer_gauges_zero(&recorder),
            "fail-fast unwind should reclaim every owned producer",
        )
        .await;
    })
    .await;
}

// ---------------------------------------------------------------------------
// Series 6 — send failure / blocked-send reclamation
// ---------------------------------------------------------------------------

/// Init spawns a keyed producer that overfills its bounded private channel;
/// the mock forwarder overfills the bounded shared channel.
struct BlockedApp {
    log: ObsLog,
    mock: MockSource<u32>,
}

impl Application for BlockedApp {
    type Message = u32;
    type Flags = (ObsLog, MockSource<u32>);

    fn new((log, mock): Self::Flags) -> (Self, Command<u32>) {
        let probe = log.clone();
        let init = Command::stream(stream::iter([1u32, 2]).map(move |v| {
            probe.push(format!("enqueue:kfill:{v}"));
            v
        }))
        .cancellable(CommandId::new("kfill"));
        (Self { log, mock }, init)
    }

    fn update(&mut self, msg: u32) -> Command<u32> {
        self.log.push(format!("update:{msg}"));
        Command::none()
    }

    fn view(&self, _frame: &mut Frame<'_>) {}

    fn subscriptions(&self) -> Vec<Subscription<u32>> {
        vec![probed_mock(&self.mock, &self.log)]
    }
}

// H-5 series "send failure": producers whose sends cannot complete (bounded
// channels at capacity, receivers about to disappear) are reclaimed with
// their bookkeeping on teardown — tasks end, the blocked counters fall to
// zero, and nothing leaks. Note (spike report §S6): every receiver-drop site
// in the current topology also aborts its producer, so the literal
// `send(..).is_err()` arm is not deterministically reachable; the
// reclamation property is driven through blocked sends interrupted by
// teardown, which is the same recovery obligation.
#[tokio::test(start_paused = true)]
async fn s6_blocked_sends_and_bookkeeping_are_reclaimed_without_leaks() {
    let recorder = TraceRecorder::new().with_target("tears::runtime::load");
    let _guard = recorder.set_default();
    let log = ObsLog::default();
    let mock = MockSource::new();
    let one = NonZeroUsize::new(1).expect("non-zero");
    let runtime_config = RuntimeConfig::new(frame_rate(60))
        .app_channel_capacity(one)
        .keyed_channel_capacity(one);
    let runtime = Runtime::<BlockedApp>::with_config((log.clone(), mock.clone()), runtime_config);
    let mut terminal = test_terminal();
    let (arbiter, mut script) = scripted();
    let run = runtime.run_driven(&mut terminal, arbiter);

    let drive = async {
        script.synchronized().await;
        settle_until(|| mock.receiver_count() > 0, "mock forwarder admitted").await;

        // Jam both bounded channels: the keyed producer blocks on its second
        // private-channel send, the forwarder on its second shared send.
        mock.emit(10).expect("forwarder should be subscribed");
        mock.emit(11).expect("forwarder should be subscribed");
        settle_until(
            || {
                log.contains("enqueue:kfill:2")
                    && log.contains("enqueue:sub:11")
                    && last_gauge(&recorder, "blocked") == Some(2)
            },
            "both producers should be parked mid-send at capacity",
        )
        .await;
        script
    };

    // Drop the run future while both producers are parked mid-send: the
    // abrupt-termination route must reclaim them and their bookkeeping.
    tokio::select! {
        result = run => panic!("the kernel must not finish before the scripted drop: {result:?}"),
        _script = drive => {}
    }

    settle_until(
        || producer_gauges_zero(&recorder),
        "blocked producers and their bookkeeping should be reclaimed after the drop",
    )
    .await;
    assert!(
        log.updates().is_empty(),
        "no blocked output may be delivered after termination"
    );
}

// ---------------------------------------------------------------------------
// Series 7 — idle wake
// ---------------------------------------------------------------------------

/// Subscriptions: the finite source (emits once, finishes, stays desired)
/// plus the mock. Updates only log.
struct IdleApp {
    log: ObsLog,
    mock: MockSource<u32>,
}

impl Application for IdleApp {
    type Message = u32;
    type Flags = (ObsLog, MockSource<u32>);

    fn new((log, mock): Self::Flags) -> (Self, Command<u32>) {
        (Self { log, mock }, Command::none())
    }

    fn update(&mut self, msg: u32) -> Command<u32> {
        self.log.push(format!("update:{msg}"));
        Command::none()
    }

    fn view(&self, _frame: &mut Frame<'_>) {}

    fn subscriptions(&self) -> Vec<Subscription<u32>> {
        vec![
            probed_mock(&self.mock, &self.log),
            Subscription::new(FiniteSource {
                log: self.log.clone(),
            }),
        ]
    }
}

/// Drives the finished-but-still-desired cycle up to the point where the
/// second quiescence has happened and no message is pending, then hands the
/// script back for the wake probe.
async fn drive_idle_to_second_quiescence(
    script: &mut Script,
    log: &ObsLog,
    mock: &MockSource<u32>,
    recorder: &TraceRecorder,
) {
    script.synchronized().await;
    settle_until(|| mock.receiver_count() > 0, "mock forwarder admitted").await;
    settle_until(
        || log.count("enqueue:fin") == 1 && last_gauge(recorder, "subscriptions") == Some(1),
        "the finite subscription should emit once and quiesce",
    )
    .await;

    // Deliver its message: the batch marks subscriptions dirty, and the next
    // frame pass restarts the finished, still-desired subscription (B-2
    // restart semantics — driven through the real forwarder lifecycle).
    script.step(Directive::Input).await;
    script.step(Directive::Frame).await;
    settle_until(
        || log.count("admit:fin") == 2 && last_gauge(recorder, "subscriptions") == Some(1),
        "the restarted subscription should emit again and quiesce again",
    )
    .await;
}

// Evidence row (current behavior, passes today): after the restarted
// subscription finishes again, its quiescence does not mark frame work —
// the frame branch parks indefinitely even though a finished,
// still-desired subscription is waiting for a restart. The
// message-independent re-evaluation trigger (RFC 0012 §4.3, Accepted)
// is not implemented, so H-4's idle-wake path does not exist to drive.
#[tokio::test(start_paused = true)]
async fn s7_no_message_independent_wake_exists_evidence() {
    let recorder = TraceRecorder::new().with_target("tears::runtime::load");
    let _guard = recorder.set_default();
    let log = ObsLog::default();
    let mock = MockSource::new();
    let runtime = Runtime::<IdleApp>::with_config((log.clone(), mock.clone()), config());
    let mut terminal = test_terminal();
    let (arbiter, mut script) = scripted();
    let run = runtime.run_driven(&mut terminal, arbiter);

    let drive = async {
        drive_idle_to_second_quiescence(&mut script, &log, &mock, &recorder).await;

        // The second emission is still queued as a message; the parked-loop
        // claim is about the *frame* branch: no frame work is pending and no
        // message-independent source can create any, so the frame branch
        // stays parked.
        script.direct(Directive::Frame);
        script
            .assert_parked(
                "no message-independent trigger marks frame work after a subscription quiesces",
            )
            .await;
        script
    };

    tokio::select! {
        result = run => panic!("the kernel must not finish before the scripted drop: {result:?}"),
        _script = drive => {}
    }

    settle_until(
        || producer_gauges_zero(&recorder),
        "teardown should reclaim the forwarders after the drop",
    )
    .await;
    assert_eq!(
        log.updates(),
        vec!["update:5"],
        "only the first emission was delivered; the second was pending at the drop"
    );
}

// H-5 series "idle wake" — the contract expectation (H-4: a
// message-independent re-evaluation trigger wakes the parked loop and the
// next re-evaluation runs). Undrivable on current code; see the evidence
// test above.
#[tokio::test(start_paused = true)]
#[ignore = "RFC 0012 §4.3 message-independent re-evaluation trigger is Accepted but unimplemented: no wake source exists for the parked loop without a message (see s7_no_message_independent_wake_exists_evidence)"]
async fn s7_idle_wake_restarts_quiesced_subscription_expected() {
    let recorder = TraceRecorder::new().with_target("tears::runtime::load");
    let _guard = recorder.set_default();
    let log = ObsLog::default();
    let mock = MockSource::new();
    let runtime = Runtime::<IdleApp>::with_config((log.clone(), mock.clone()), config());
    let mut terminal = test_terminal();
    let (arbiter, mut script) = scripted();
    let run = runtime.run_driven(&mut terminal, arbiter);

    let drive = async {
        drive_idle_to_second_quiescence(&mut script, &log, &mock, &recorder).await;

        // Expected under RFC 0012 §4.3: the second quiescence itself marks
        // subscriptions dirty, so the frame branch has work and completes,
        // and the re-evaluation restarts the finished subscription.
        script.direct(Directive::Frame);
        let woke = timeout(Duration::from_secs(1), script.idle.recv()).await;
        assert!(
            woke.is_ok(),
            "H-4 idle wake: a quiesced still-desired subscription must wake the parked loop"
        );
        settle_until(
            || log.count("admit:fin") == 3,
            "the message-independent re-evaluation should restart the subscription",
        )
        .await;
        script.direct(Directive::Quit);
    };

    let (run_result, ()) = tokio::join!(run, drive);
    run_result.expect("scripted run should exit cleanly");
}

// ---------------------------------------------------------------------------
// Series 8 — termination under owned work
// ---------------------------------------------------------------------------

/// Owned work: a parked unkeyed producer (init), a parked keyed producer
/// (code 7), a parked subscription source, and the mock forwarder. Codes:
/// 9 unkeyed quit, 11 keyed quit.
struct OwnedWorkApp {
    log: ObsLog,
    mock: MockSource<u32>,
    keyed_dropped: Arc<AtomicBool>,
    sub_alive: Arc<AtomicUsize>,
    sub_peak: Arc<AtomicUsize>,
}

struct OwnedWorkFlags {
    log: ObsLog,
    mock: MockSource<u32>,
    unkeyed_dropped: Arc<AtomicBool>,
    keyed_dropped: Arc<AtomicBool>,
    sub_alive: Arc<AtomicUsize>,
}

impl Application for OwnedWorkApp {
    type Message = u32;
    type Flags = OwnedWorkFlags;

    fn new(flags: OwnedWorkFlags) -> (Self, Command<u32>) {
        let init = parked_command(Arc::clone(&flags.unkeyed_dropped));
        (
            Self {
                log: flags.log,
                mock: flags.mock,
                keyed_dropped: flags.keyed_dropped,
                sub_alive: flags.sub_alive,
                sub_peak: Arc::new(AtomicUsize::new(0)),
            },
            init,
        )
    }

    fn update(&mut self, msg: u32) -> Command<u32> {
        self.log.push(format!("update:{msg}"));
        match msg {
            7 => parked_command(Arc::clone(&self.keyed_dropped)).cancellable(CommandId::new("k")),
            9 => Command::quit(),
            11 => Command::quit().cancellable(CommandId::new("kq")),
            _ => Command::none(),
        }
    }

    fn view(&self, _frame: &mut Frame<'_>) {}

    fn subscriptions(&self) -> Vec<Subscription<u32>> {
        vec![
            probed_mock(&self.mock, &self.log),
            Subscription::new(GuardedPendingSource {
                key: 0,
                alive: Arc::clone(&self.sub_alive),
                peak: Arc::clone(&self.sub_peak),
            }),
        ]
    }
}

struct OwnedWorkRun {
    recorder: TraceRecorder,
    log: ObsLog,
    mock: MockSource<u32>,
    unkeyed_dropped: Arc<AtomicBool>,
    keyed_dropped: Arc<AtomicBool>,
    sub_alive: Arc<AtomicUsize>,
}

impl OwnedWorkRun {
    fn new() -> Self {
        Self {
            recorder: TraceRecorder::new().with_target("tears::runtime::load"),
            log: ObsLog::default(),
            mock: MockSource::new(),
            unkeyed_dropped: Arc::new(AtomicBool::new(false)),
            keyed_dropped: Arc::new(AtomicBool::new(false)),
            sub_alive: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn flags(&self) -> OwnedWorkFlags {
        OwnedWorkFlags {
            log: self.log.clone(),
            mock: self.mock.clone(),
            unkeyed_dropped: Arc::clone(&self.unkeyed_dropped),
            keyed_dropped: Arc::clone(&self.keyed_dropped),
            sub_alive: Arc::clone(&self.sub_alive),
        }
    }

    /// Establishes the owned work: mock forwarder admitted, parked
    /// subscription admitted, parked unkeyed producer running, parked keyed
    /// producer running (via code 7).
    async fn establish(&self, script: &mut Script) {
        script.synchronized().await;
        settle_until(|| self.mock.receiver_count() > 0, "mock forwarder admitted").await;
        self.mock.emit(7).expect("forwarder should be subscribed");
        settle_until(
            || self.log.contains("enqueue:sub:7"),
            "keyed spawn trigger enqueued",
        )
        .await;
        script.step(Directive::Input).await;
        settle_until(
            || last_gauge(&self.recorder, "keyed_commands") == Some(1),
            "the parked keyed producer is registered",
        )
        .await;
        assert!(
            !self.unkeyed_dropped.load(Ordering::SeqCst),
            "the parked unkeyed producer should still be alive"
        );
        assert_eq!(
            self.sub_alive.load(Ordering::SeqCst),
            1,
            "the parked subscription should be admitted"
        );
    }

    /// Asserts B-1's two-stage postcondition: all owned producers reclaimed
    /// (quiescent — task bodies dropped) and every producer gauge zero.
    async fn assert_reclaimed(&self) {
        settle_until(
            || {
                self.unkeyed_dropped.load(Ordering::SeqCst)
                    && self.keyed_dropped.load(Ordering::SeqCst)
                    && self.sub_alive.load(Ordering::SeqCst) == 0
                    && producer_gauges_zero(&self.recorder)
            },
            "every owned producer and its bookkeeping should be reclaimed",
        )
        .await;
    }
}

// H-5 series "termination under owned work", unkeyed-quit cause: with live
// owned work and a ready backlog, the controlled quit route terminates the
// run, the backlog is discarded (immediate postcondition), and all owned
// tasks and gauges are reclaimed (quiescent postcondition).
#[tokio::test(start_paused = true)]
async fn s8_unkeyed_quit_reclaims_owned_work_and_discards_backlog() {
    let fixture = OwnedWorkRun::new();
    let _guard = fixture.recorder.set_default();
    let runtime = Runtime::<OwnedWorkApp>::with_config(fixture.flags(), config());
    let mut terminal = test_terminal();
    let (arbiter, mut script) = scripted();
    let run = runtime.run_driven(&mut terminal, arbiter);

    let drive = async {
        fixture.establish(&mut script).await;
        fixture
            .mock
            .emit(9)
            .expect("forwarder should be subscribed");
        settle_until(
            || fixture.log.contains("enqueue:sub:9"),
            "quit trigger enqueued",
        )
        .await;
        script.step(Directive::Input).await;
        // Backlog enqueued after the quit command dispatched; it must never
        // be delivered.
        fixture
            .mock
            .emit(1)
            .expect("forwarder should be subscribed");
        fixture
            .mock
            .emit(2)
            .expect("forwarder should be subscribed");
        settle_until(
            || fixture.log.contains("enqueue:sub:2"),
            "backlog enqueued on the shared channel",
        )
        .await;
        script.direct(Directive::Quit);
    };

    let (run_result, ()) = tokio::join!(run, drive);
    run_result.expect("quit-terminated run should exit cleanly");

    assert_eq!(
        fixture.log.updates(),
        vec!["update:7", "update:9"],
        "the post-quit backlog must be discarded, not delivered"
    );
    fixture.assert_reclaimed().await;
}

// H-5 series "termination under owned work", keyed-quit cause: same
// two-stage postcondition through the same termination implementation.
#[tokio::test(start_paused = true)]
async fn s8_keyed_quit_reclaims_owned_work() {
    let fixture = OwnedWorkRun::new();
    let _guard = fixture.recorder.set_default();
    let runtime = Runtime::<OwnedWorkApp>::with_config(fixture.flags(), config());
    let mut terminal = test_terminal();
    let (arbiter, mut script) = scripted();
    let run = runtime.run_driven(&mut terminal, arbiter);

    let drive = async {
        fixture.establish(&mut script).await;
        fixture
            .mock
            .emit(11)
            .expect("forwarder should be subscribed");
        settle_until(
            || fixture.log.contains("enqueue:sub:11"),
            "keyed-quit trigger enqueued",
        )
        .await;
        script.step(Directive::Input).await;
        // The keyed quit is now buffered (or about to be); the input branch
        // delivers it in-band and the loop exits.
        script.direct(Directive::Input);
    };

    let (run_result, ()) = tokio::join!(run, drive);
    run_result.expect("keyed-quit-terminated run should exit cleanly");

    assert_eq!(fixture.log.updates(), vec!["update:7", "update:11"]);
    fixture.assert_reclaimed().await;
}

// H-5 series "termination under owned work", render-error cause: the frame
// pass's render failure propagates out of `run()` and the same two-stage
// postcondition holds (via the drop half of the termination contract).
#[tokio::test(start_paused = true)]
async fn s8_render_error_reclaims_owned_work() {
    let fixture = OwnedWorkRun::new();
    let _guard = fixture.recorder.set_default();
    let runtime = Runtime::<OwnedWorkApp>::with_config(fixture.flags(), config());
    let mut terminal =
        Terminal::new(FailingBackend(TestBackend::new(80, 24))).expect("failing terminal");
    let (arbiter, mut script) = scripted();
    let run = runtime.run_driven(&mut terminal, arbiter);

    let drive = async {
        fixture.establish(&mut script).await;
        // Redraw is pending (bootstrap + the update), so the frame branch
        // renders — and the backend fails the draw.
        script.direct(Directive::Frame);
    };

    let (run_result, ()) = tokio::join!(run, drive);
    let error = run_result.expect_err("a render error must propagate out of the run");
    assert!(
        error.to_string().contains("scripted render failure"),
        "the propagated error should be the backend's: {error}"
    );

    fixture.assert_reclaimed().await;
}

// H-5 series "termination under owned work", run-future-drop cause: the
// abrupt route (drop without shutdown) converges to the same two-stage
// postcondition.
#[tokio::test(start_paused = true)]
async fn s8_run_future_drop_reclaims_owned_work() {
    let fixture = OwnedWorkRun::new();
    let _guard = fixture.recorder.set_default();
    let runtime = Runtime::<OwnedWorkApp>::with_config(fixture.flags(), config());
    let mut terminal = test_terminal();
    let (arbiter, mut script) = scripted();
    let run = runtime.run_driven(&mut terminal, arbiter);

    let drive = async {
        fixture.establish(&mut script).await;
        script
    };

    tokio::select! {
        result = run => panic!("the kernel must not finish before the scripted drop: {result:?}"),
        _script = drive => {}
    }

    fixture.assert_reclaimed().await;
    assert_eq!(
        fixture.log.updates(),
        vec!["update:7"],
        "nothing may be delivered after the run future is dropped"
    );
}
