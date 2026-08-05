//! Integration tests for RFC 0011's runtime lifecycle contract at the layer
//! its invariants place them: the controlled and abrupt termination routes and
//! their two-stage postconditions (INV-LC5/INV-LC6/INV-LC7), panic containment
//! for runtime-owned producer tasks (INV-LC8), and the construction-inertness
//! contract whose behavior change lands with the 0.11.0 lifecycle work
//! (INV-LC3, RFC 0011 §3.4).
//!
//! The steady-state phase-order invariants (INV-LC1/INV-LC2) and the
//! first-render eligibility half of INV-LC4 are white-box and live in
//! `src/runtime.rs`. INV-LC9, the ordering half of INV-LC4, and the synchrony
//! half of INV-LC6 are structural checks (RFC 0011 §8): they have no behavioral
//! seam a test can anchor on.

mod common;
#[path = "common/panic_hook.rs"]
mod panic_hook;
#[path = "common/trace_recorder.rs"]
mod trace_recorder;

use std::convert::Infallible;
use std::future::pending;
use std::io;
use std::num::{NonZeroU32, NonZeroU64};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use color_eyre::eyre::Result;
use futures::FutureExt;
use futures::stream::{self, StreamExt};
use ratatui::Terminal;
use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use ratatui::prelude::Frame;
use tears::command::CommandId;
use tears::prelude::*;
use tears::subscription::time::Timer;
use tears::{BoxStream, SubscriptionSource};
use tokio::task::yield_now;
use tokio::time::{Duration, timeout};
use trace_recorder::TraceRecorder;

fn frame_rate(value: u32) -> FrameRate {
    FrameRate::new(NonZeroU32::new(value).expect("frame rate must be non-zero"))
        .expect("frame rate must be valid")
}

fn timer_subscription<Msg: Send + 'static>(make: fn() -> Msg) -> Subscription<Msg> {
    Subscription::new(Timer::new(NonZeroU64::new(10).expect("non-zero"))).map(move |_| make())
}

// --- Shared observation surfaces --------------------------------------------

/// Counts every application transition the runtime invokes, so a test can
/// assert that none of them runs again after a terminating operation (RFC 0011
/// §4.4's immediate postcondition, item 1).
#[derive(Clone, Default)]
struct Transitions {
    updates: Arc<AtomicUsize>,
    views: Arc<AtomicUsize>,
    subscriptions: Arc<AtomicUsize>,
}

impl Transitions {
    fn snapshot(&self) -> [usize; 3] {
        [
            self.updates.load(Ordering::SeqCst),
            self.views.load(Ordering::SeqCst),
            self.subscriptions.load(Ordering::SeqCst),
        ]
    }
}

/// Flips its flag when dropped. A runtime-owned task's future is dropped only
/// once the executor has processed the abort request, so this witnesses the
/// quiescent stage rather than merely the request — and, for a subscription
/// source, proves the stream can never be polled again.
struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// The producer gauges RFC 0006 §4.4 defines and RFC 0011 INV-LC7 reads.
const PRODUCER_GAUGES: [&str; 3] = ["subscriptions", "unkeyed_commands", "keyed_commands"];

fn producer_gauges_are_zero(recorder: &TraceRecorder) -> bool {
    PRODUCER_GAUGES
        .iter()
        .all(|field| matches!(recorder.u64_values(field).last(), None | Some(&0)))
}

fn no_producer_gauge_event_fired(recorder: &TraceRecorder) -> bool {
    PRODUCER_GAUGES
        .iter()
        .all(|field| recorder.u64_values(field).is_empty())
}

/// INV-LC7's bounded settle loop, doubling as INV-LC6's re-checked
/// no-further-transition assertion.
///
/// The transition counters are compared on every iteration — immediately after
/// the terminating operation and again across each of the settle loop's yields
/// — because a settle-only check would pass an implementation that defers its
/// cancellation requests by a scheduler pass. The quiescent half never asserts a
/// fixed pass count: abort is a request, and the aborted task's future (with the
/// RAII state it holds) is dropped on a later executor poll (RFC 0011 §4.4).
///
/// `dismantled` names the task futures whose drop flags must all have flipped by
/// the time the loop exits. They are checked alongside the gauges because the
/// keyed gauge is count-based and publishes zero at the owner's drop, ahead of
/// the executor dismantling the task futures themselves.
async fn assert_two_stage_postconditions(
    recorder: &TraceRecorder,
    transitions: &Transitions,
    immediate: [usize; 3],
    dismantled: &[(&str, &Arc<AtomicBool>)],
) {
    for _ in 0..1_000 {
        assert_eq!(
            transitions.snapshot(),
            immediate,
            "no update/view/subscriptions call may run after the terminating operation"
        );
        if producer_gauges_are_zero(recorder)
            && dismantled
                .iter()
                .all(|(_, flag)| flag.load(Ordering::SeqCst))
        {
            return;
        }
        yield_now().await;
    }

    assert!(
        producer_gauges_are_zero(recorder),
        "every producer gauge must settle to zero: subscriptions={:?} unkeyed={:?} keyed={:?}",
        recorder.u64_values("subscriptions"),
        recorder.u64_values("unkeyed_commands"),
        recorder.u64_values("keyed_commands"),
    );
    for (name, flag) in dismantled {
        assert!(
            flag.load(Ordering::SeqCst),
            "the executor must dismantle {name} once the requested cancellations are processed"
        );
    }
}

// --- INV-LC5: controlled termination ----------------------------------------

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QuitRoute {
    Unkeyed,
    Keyed,
}

#[derive(Clone, Copy, Debug)]
enum QuitMessage {
    Tick,
}

/// Keeps all three producer kinds running until the quit arrives: a keyed
/// parked effect from `new`, an unkeyed parked effect from the first processed
/// message, and a `Timer` subscription that supplies the messages.
struct QuitApp {
    transitions: Transitions,
    route: QuitRoute,
    ticks: usize,
}

impl Application for QuitApp {
    type Message = QuitMessage;
    type Flags = (Transitions, QuitRoute);

    fn new((transitions, route): Self::Flags) -> (Self, Command<Self::Message>) {
        (
            Self {
                transitions,
                route,
                ticks: 0,
            },
            Command::future(pending::<QuitMessage>()).cancellable(CommandId::new("parked-keyed")),
        )
    }

    fn update(&mut self, _msg: Self::Message) -> Command<Self::Message> {
        self.transitions.updates.fetch_add(1, Ordering::SeqCst);
        self.ticks += 1;
        if self.ticks == 1 {
            return Command::future(pending::<QuitMessage>());
        }

        match self.route {
            QuitRoute::Unkeyed => Command::quit(),
            QuitRoute::Keyed => Command::quit().cancellable(CommandId::new("keyed-quit")),
        }
    }

    fn view(&self, _frame: &mut Frame<'_>) {
        self.transitions.views.fetch_add(1, Ordering::SeqCst);
    }

    fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
        self.transitions
            .subscriptions
            .fetch_add(1, Ordering::SeqCst);
        vec![timer_subscription(|| QuitMessage::Tick)]
    }
}

async fn assert_quit_route_terminates(route: QuitRoute) -> Result<()> {
    let recorder = TraceRecorder::new().with_target("tears::runtime::load");
    let _guard = recorder.set_default();

    let transitions = Transitions::default();
    let mut terminal = common::test_terminal()?;
    let runtime = Runtime::<QuitApp>::new((transitions.clone(), route), frame_rate(60));

    let outcome = timeout(Duration::from_secs(5), runtime.run(&mut terminal))
        .await
        .expect("the quit should exit the loop before the timeout");

    assert!(
        outcome.is_ok(),
        "a {route:?} quit classifies the run as Ok(())"
    );
    let immediate = transitions.snapshot();
    assert!(
        immediate[0] >= 2,
        "the run should reach the quitting message: {immediate:?}"
    );

    assert_two_stage_postconditions(&recorder, &transitions, immediate, &[]).await;

    Ok(())
}

// INV-LC5: an unkeyed quit under running producers exits the loop, returns
// `Ok(())`, and invokes no further transition; INV-LC7's settle loop then
// witnesses the producers winding down.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn unkeyed_quit_returns_ok_and_reaches_both_postconditions() -> Result<()> {
    assert_quit_route_terminates(QuitRoute::Unkeyed).await
}

// INV-LC5: a keyed quit — delivered in band through its run's private channel —
// reaches the same postconditions with the same `Ok(())` classification.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn keyed_quit_returns_ok_and_reaches_both_postconditions() -> Result<()> {
    assert_quit_route_terminates(QuitRoute::Keyed).await
}

/// A backend whose every draw fails, so the runtime's render step takes the
/// render-error controlled termination route (RFC 0011 §4.1). Everything else
/// delegates to a `TestBackend`, whose error type is `Infallible`.
struct FailingBackend {
    inner: TestBackend,
}

impl FailingBackend {
    fn new() -> Self {
        Self {
            inner: TestBackend::new(80, 24),
        }
    }
}

/// Widens a `TestBackend` result into this backend's error type. `TestBackend`'s
/// error type is uninhabited, so the `Err` arm cannot be constructed.
fn lift<T>(result: Result<T, Infallible>) -> Result<T, io::Error> {
    result.map_err(|never| match never {})
}

impl Backend for FailingBackend {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, _content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        Err(io::Error::other("injected render failure"))
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        lift(self.inner.hide_cursor())
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        lift(self.inner.show_cursor())
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        lift(self.inner.get_cursor_position())
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        lift(self.inner.set_cursor_position(position))
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        lift(self.inner.clear())
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        lift(self.inner.clear_region(clear_type))
    }

    fn size(&self) -> Result<Size, Self::Error> {
        lift(self.inner.size())
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        lift(self.inner.window_size())
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        lift(self.inner.flush())
    }
}

// INV-LC5: a failing render step is a controlled cause too — the loop exits, the
// return value classifies the reason as `Err`, no further transition runs, and
// the producers wind down through the same settle loop as the quit rows even
// though this exit bypasses the explicit shutdown routine (RFC 0011 §4.2).
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn render_error_returns_err_and_reaches_both_postconditions() -> Result<()> {
    let recorder = TraceRecorder::new().with_target("tears::runtime::load");
    let _guard = recorder.set_default();

    let transitions = Transitions::default();
    let mut terminal = Terminal::new(FailingBackend::new())?;
    let runtime =
        Runtime::<QuitApp>::new((transitions.clone(), QuitRoute::Unkeyed), frame_rate(60));

    let outcome = timeout(Duration::from_secs(5), runtime.run(&mut terminal))
        .await
        .expect("the render error should exit the loop before the timeout");

    assert!(
        outcome.is_err(),
        "a render error classifies the run as Err: {outcome:?}"
    );
    let immediate = transitions.snapshot();
    assert!(
        immediate[1] >= 1,
        "the failing render step invoked view before the backend rejected the draw: {immediate:?}"
    );

    assert_two_stage_postconditions(&recorder, &transitions, immediate, &[]).await;

    Ok(())
}

// --- INV-LC6: abrupt termination --------------------------------------------

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PanicSite {
    None,
    Update,
    View,
    SubscriptionsBootstrap,
    SubscriptionsSteady,
    SourceConstructor,
}

#[derive(Clone, Copy, Debug)]
enum AbruptMessage {
    Tick,
}

#[derive(Clone)]
struct AbruptFlags {
    transitions: Transitions,
    site: PanicSite,
    keyed_effect_dropped: Arc<AtomicBool>,
    source_dropped: Arc<AtomicBool>,
}

impl AbruptFlags {
    fn new(site: PanicSite) -> Self {
        Self {
            transitions: Transitions::default(),
            site,
            keyed_effect_dropped: Arc::new(AtomicBool::new(false)),
            source_dropped: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// A subscription source that emits one message and then parks forever while
/// holding a drop flag, so the forwarder task's dismantling is observable.
/// Optionally panics inside `stream()` — the lazy source constructor, which the
/// reconcile calls on the driving task.
struct ProbeSource {
    dropped: Arc<AtomicBool>,
    panic_in_constructor: bool,
}

impl SubscriptionSource for ProbeSource {
    type Output = AbruptMessage;
    type Key = ();

    fn stream(&self) -> BoxStream<'static, Self::Output> {
        assert!(
            !self.panic_in_constructor,
            "deliberate panic: a subscription's lazy source constructor, on the driving task"
        );

        let guard = DropFlag(Arc::clone(&self.dropped));
        stream::once(async { AbruptMessage::Tick })
            .chain(stream::once(async move {
                let _guard = guard;
                pending::<AbruptMessage>().await
            }))
            .boxed()
    }

    fn key(&self) -> Self::Key {}
}

struct AbruptApp {
    flags: AbruptFlags,
}

#[expect(
    clippy::panic,
    reason = "`subscriptions` panics deliberately to exercise INV-LC6's two subscriptions rows"
)]
impl Application for AbruptApp {
    type Message = AbruptMessage;
    type Flags = AbruptFlags;

    fn new(flags: AbruptFlags) -> (Self, Command<Self::Message>) {
        let dropped = Arc::clone(&flags.keyed_effect_dropped);
        let command = Command::future(async move {
            let _guard = DropFlag(dropped);
            pending::<AbruptMessage>().await
        })
        .cancellable(CommandId::new("parked-keyed"));

        (Self { flags }, command)
    }

    fn update(&mut self, _msg: Self::Message) -> Command<Self::Message> {
        self.flags
            .transitions
            .updates
            .fetch_add(1, Ordering::SeqCst);
        assert!(
            self.flags.site != PanicSite::Update,
            "deliberate panic: update, on the driving task"
        );
        Command::none()
    }

    fn view(&self, _frame: &mut Frame<'_>) {
        self.flags.transitions.views.fetch_add(1, Ordering::SeqCst);
        assert!(
            self.flags.site != PanicSite::View,
            "deliberate panic: view, on the driving task"
        );
    }

    fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
        let previous = self
            .flags
            .transitions
            .subscriptions
            .fetch_add(1, Ordering::SeqCst);
        match self.flags.site {
            PanicSite::SubscriptionsBootstrap => {
                panic!("deliberate panic: subscriptions, at the bootstrap call site");
            }
            // The bootstrap call is the first one; a later call can only come
            // from a re-evaluation after a processed message.
            PanicSite::SubscriptionsSteady if previous >= 1 => {
                panic!("deliberate panic: subscriptions, at the steady call site");
            }
            _ => {}
        }

        vec![Subscription::new(ProbeSource {
            dropped: Arc::clone(&self.flags.source_dropped),
            panic_in_constructor: self.flags.site == PanicSite::SourceConstructor,
        })]
    }
}

/// Drives one INV-LC6 panic row: the unwind must propagate to `run()`'s caller,
/// and from the moment it completes no transition may run — checked immediately
/// and re-checked across the settle loop — while the runtime-owned tasks are
/// wound down.
///
/// `source_starts` says whether the row got far enough to start the
/// subscription's stream; the bootstrap-`subscriptions` and source-constructor
/// rows panic before any stream exists.
async fn assert_transition_panic_tears_down(site: PanicSite, source_starts: bool) -> Result<()> {
    let recorder = TraceRecorder::new().with_target("tears::runtime::load");
    let _guard = recorder.set_default();

    let flags = AbruptFlags::new(site);
    let mut terminal = common::test_terminal()?;
    let runtime = Runtime::<AbruptApp>::new(flags.clone(), frame_rate(60));

    let outcome = panic_hook::with_silent_panic_hook(
        AssertUnwindSafe(timeout(Duration::from_secs(5), runtime.run(&mut terminal)))
            .catch_unwind(),
    )
    .await;

    assert!(
        outcome.is_err(),
        "the {site:?} panic must propagate to run()'s caller"
    );

    let mut dismantled = vec![("the keyed init effect", &flags.keyed_effect_dropped)];
    if source_starts {
        // Once the stream is dropped it can never be polled again — a stronger
        // witness than counting polls over a finite window.
        dismantled.push(("the subscription's stream", &flags.source_dropped));
    }

    let immediate = flags.transitions.snapshot();
    assert_two_stage_postconditions(&recorder, &flags.transitions, immediate, &dismantled).await;

    Ok(())
}

// INV-LC6: dropping the `run` future mid-run (a caller's `select!`/timeout)
// performs the ownership teardown and the cancellation requests itself; no
// transition runs afterwards, and both producer kinds are dismantled.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn dropping_the_run_future_reaches_both_postconditions() -> Result<()> {
    let recorder = TraceRecorder::new().with_target("tears::runtime::load");
    let _guard = recorder.set_default();

    let flags = AbruptFlags::new(PanicSite::None);
    let mut terminal = common::test_terminal()?;
    let runtime = Runtime::<AbruptApp>::new(flags.clone(), frame_rate(60));

    let mut run = Box::pin(runtime.run(&mut terminal));
    let cancelled = timeout(Duration::from_millis(50), &mut run).await;
    assert!(
        cancelled.is_err(),
        "the run must still be in progress when the caller cancels it"
    );

    // The terminating operation.
    drop(run);

    let immediate = flags.transitions.snapshot();
    assert_two_stage_postconditions(
        &recorder,
        &flags.transitions,
        immediate,
        &[
            ("the keyed init effect", &flags.keyed_effect_dropped),
            ("the subscription's stream", &flags.source_dropped),
        ],
    )
    .await;

    Ok(())
}

// INV-LC6: a panic in `update` — application code on the driving task — stays
// fail-fast and propagates, and the unwind still performs the teardown.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_panic_in_update_propagates_and_reaches_both_postconditions() -> Result<()> {
    assert_transition_panic_tears_down(PanicSite::Update, true).await
}

// INV-LC6: same for a panic in `view`.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_panic_in_view_propagates_and_reaches_both_postconditions() -> Result<()> {
    assert_transition_panic_tears_down(PanicSite::View, true).await
}

// INV-LC6: same for a panic in `subscriptions` at the bootstrap call site,
// raised on its first call, before the loop.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_bootstrap_subscriptions_panic_propagates_and_reaches_both_postconditions() -> Result<()>
{
    assert_transition_panic_tears_down(PanicSite::SubscriptionsBootstrap, false).await
}

// INV-LC6: same for a panic in `subscriptions` at the steady call site, raised
// only on a re-evaluation after a processed message.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_steady_subscriptions_panic_propagates_and_reaches_both_postconditions() -> Result<()> {
    assert_transition_panic_tears_down(PanicSite::SubscriptionsSteady, true).await
}

// INV-LC6: same for a panic in a declared subscription's lazy source
// constructor, which runs inside the reconcile on the driving task — distinct
// from INV-LC8's forwarder-task row.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_source_constructor_panic_propagates_and_reaches_both_postconditions() -> Result<()> {
    assert_transition_panic_tears_down(PanicSite::SourceConstructor, false).await
}

// --- INV-LC3 and the never-run-drop row of INV-LC6 --------------------------

#[derive(Clone, Default)]
struct InertProbe {
    effect_started: Arc<AtomicBool>,
    source_started: Arc<AtomicBool>,
}

#[derive(Clone, Copy, Debug)]
enum InertMessage {
    #[expect(
        dead_code,
        reason = "an inert runtime never delivers a message, so this variant only types the application"
    )]
    Never,
}

struct InertSource {
    started: Arc<AtomicBool>,
}

impl SubscriptionSource for InertSource {
    type Output = InertMessage;
    type Key = ();

    fn stream(&self) -> BoxStream<'static, Self::Output> {
        self.started.store(true, Ordering::SeqCst);
        stream::pending().boxed()
    }

    fn key(&self) -> Self::Key {}
}

/// Records whether the init command's effect was ever polled and whether a
/// subscription source was ever started.
struct InertApp {
    probe: InertProbe,
}

impl Application for InertApp {
    type Message = InertMessage;
    type Flags = InertProbe;

    fn new(probe: InertProbe) -> (Self, Command<Self::Message>) {
        let started = Arc::clone(&probe.effect_started);
        let command = Command::future(async move {
            started.store(true, Ordering::SeqCst);
            pending::<InertMessage>().await
        });

        (Self { probe }, command)
    }

    fn update(&mut self, _msg: Self::Message) -> Command<Self::Message> {
        Command::none()
    }

    fn view(&self, _frame: &mut Frame<'_>) {}

    fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
        vec![Subscription::new(InertSource {
            started: Arc::clone(&self.probe.source_started),
        })]
    }
}

/// Gives the executor every chance to poll a task that a conforming
/// construction never spawned.
async fn drain_executor() {
    for _ in 0..32 {
        yield_now().await;
    }
}

// INV-LC3: constructing a `Runtime` spawns no runtime-owned task, polls no
// command effect, and starts no subscription source — construction is inert
// (RFC 0011 §3.1). Today the constructor dispatches the init command itself, so
// this asserts the post-conformance contract of the §3.4 deliverable.
#[tokio::test(flavor = "current_thread", start_paused = true)]
#[ignore = "RFC 0011 §3.4 conformance lands with the 0.11.0 lifecycle change"]
async fn constructing_a_runtime_starts_no_effect_and_no_subscription_source() {
    let recorder = TraceRecorder::new().with_target("tears::runtime::load");
    let _guard = recorder.set_default();

    let probe = InertProbe::default();
    let runtime = Runtime::<InertApp>::new(probe.clone(), frame_rate(60));

    drain_executor().await;

    assert!(
        !probe.effect_started.load(Ordering::SeqCst),
        "construction must not poll the init command's effect"
    );
    assert!(
        !probe.source_started.load(Ordering::SeqCst),
        "construction must not start a subscription source"
    );
    assert!(
        no_producer_gauge_event_fired(&recorder),
        "construction must fire no producer-gauge event"
    );

    drop(runtime);
}

// INV-LC6 (never-run-drop row): with the §3.4 change landed there is nothing to
// wind down when a constructed-but-never-run runtime is dropped, and this row
// asserts exactly that, reusing INV-LC3's recorder setup (RFC 0011 §8).
#[tokio::test(flavor = "current_thread", start_paused = true)]
#[ignore = "RFC 0011 §3.4 conformance lands with the 0.11.0 lifecycle change"]
async fn dropping_a_never_run_runtime_winds_down_nothing() {
    let recorder = TraceRecorder::new().with_target("tears::runtime::load");
    let _guard = recorder.set_default();

    let probe = InertProbe::default();
    drop(Runtime::<InertApp>::new(probe.clone(), frame_rate(60)));

    drain_executor().await;

    assert!(
        !probe.effect_started.load(Ordering::SeqCst),
        "a never-run runtime must have executed no effect"
    );
    assert!(
        !probe.source_started.load(Ordering::SeqCst),
        "a never-run runtime must have started no subscription source"
    );
    assert!(
        no_producer_gauge_event_fired(&recorder),
        "a never-run runtime's lifetime must fire no producer-gauge event"
    );
}

// --- INV-LC8: panic containment for runtime-owned producer tasks ------------

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PanickingProducer {
    UnkeyedCommand,
    KeyedCommand,
    SubscriptionForwarder,
}

#[derive(Clone, Copy, Debug)]
enum ContainmentMessage {
    Tick,
}

#[expect(
    clippy::panic,
    reason = "the effect panics deliberately to exercise INV-LC8's command-task rows"
)]
fn panicking_effect() -> Command<ContainmentMessage> {
    Command::future(async {
        panic!("runtime-owned command task panicked");
        #[expect(
            unreachable_code,
            reason = "the preceding panic! diverges, so this trailing expression that types the async block is unreachable"
        )]
        ContainmentMessage::Tick
    })
}

/// A source whose constructor succeeds and whose stream panics while the
/// forwarder task polls it — the forwarder row, distinct from INV-LC6's
/// constructor-panic row, which unwinds on the driving task.
struct PanickingStreamSource;

impl SubscriptionSource for PanickingStreamSource {
    type Output = ContainmentMessage;
    type Key = ();

    #[expect(
        clippy::panic,
        reason = "the stream panics deliberately to exercise INV-LC8's forwarder row"
    )]
    fn stream(&self) -> BoxStream<'static, Self::Output> {
        stream::once(async {
            panic!("subscription stream panicked inside the forwarder task");
            #[expect(
                unreachable_code,
                reason = "the preceding panic! diverges, so this trailing expression that types the async block is unreachable"
            )]
            ContainmentMessage::Tick
        })
        .boxed()
    }

    fn key(&self) -> Self::Key {}
}

/// Runs one panicking producer alongside a surviving `Timer` subscription whose
/// later messages must still reach `update`, then quits normally.
struct ContainmentApp {
    kind: PanickingProducer,
    transitions: Transitions,
    ticks: usize,
}

const SURVIVING_TICKS: usize = 3;

impl Application for ContainmentApp {
    type Message = ContainmentMessage;
    type Flags = (PanickingProducer, Transitions);

    fn new((kind, transitions): Self::Flags) -> (Self, Command<Self::Message>) {
        let command = match kind {
            PanickingProducer::UnkeyedCommand => panicking_effect(),
            PanickingProducer::KeyedCommand => {
                panicking_effect().cancellable(CommandId::new("panicking-keyed"))
            }
            PanickingProducer::SubscriptionForwarder => Command::none(),
        };

        (
            Self {
                kind,
                transitions,
                ticks: 0,
            },
            command,
        )
    }

    fn update(&mut self, _msg: Self::Message) -> Command<Self::Message> {
        self.transitions.updates.fetch_add(1, Ordering::SeqCst);
        self.ticks += 1;
        if self.ticks >= SURVIVING_TICKS {
            Command::quit()
        } else {
            Command::none()
        }
    }

    fn view(&self, _frame: &mut Frame<'_>) {
        self.transitions.views.fetch_add(1, Ordering::SeqCst);
    }

    fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
        self.transitions
            .subscriptions
            .fetch_add(1, Ordering::SeqCst);
        let surviving = timer_subscription(|| ContainmentMessage::Tick);
        if self.kind == PanickingProducer::SubscriptionForwarder {
            vec![Subscription::new(PanickingStreamSource), surviving]
        } else {
            vec![surviving]
        }
    }
}

async fn assert_producer_panic_is_contained(kind: PanickingProducer) -> Result<()> {
    let transitions = Transitions::default();
    let mut terminal = common::test_terminal()?;
    let runtime = Runtime::<ContainmentApp>::new((kind, transitions.clone()), frame_rate(60));

    let outcome = panic_hook::with_silent_panic_hook(timeout(
        Duration::from_secs(5),
        runtime.run(&mut terminal),
    ))
    .await
    .expect("a contained producer panic must leave the run able to quit before the timeout");

    assert!(
        outcome.is_ok(),
        "a {kind:?} panic must not terminate the application: {outcome:?}"
    );
    assert!(
        transitions.updates.load(Ordering::SeqCst) >= SURVIVING_TICKS,
        "the surviving producer's later messages must still reach update after the {kind:?} panic"
    );

    Ok(())
}

// INV-LC8: a panic in an unkeyed command task does not terminate the
// application — the loop keeps running and a surviving producer's later
// messages still arrive at `update`.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_panicking_unkeyed_command_task_does_not_terminate_the_application() -> Result<()> {
    assert_producer_panic_is_contained(PanickingProducer::UnkeyedCommand).await
}

// INV-LC8: same for a keyed command task. RFC 0003 §7.3 requires only that the
// panic is logged; the continuation property is this invariant's, so this row is
// not redundant with the keyed logging test.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_panicking_keyed_command_task_does_not_terminate_the_application() -> Result<()> {
    assert_producer_panic_is_contained(PanickingProducer::KeyedCommand).await
}

// INV-LC8: same for a subscription forwarder whose source constructor succeeds
// and whose stream panics while the forwarder task polls it.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_panicking_subscription_forwarder_does_not_terminate_the_application() -> Result<()> {
    assert_producer_panic_is_contained(PanickingProducer::SubscriptionForwarder).await
}
