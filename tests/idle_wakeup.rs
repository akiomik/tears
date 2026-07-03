// Integration tests for the idle frame wake-up elision.
// These verify end-to-end that the event loop stops waking at the frame rate
// while idle, and that re-enabling the frame branch after idle renders without
// added latency. The gate predicate itself is unit-tested in src/runtime.rs.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use color_eyre::eyre::Result;
use ratatui::Frame;
use tears::prelude::*;
use tokio::time::{Duration, Instant, sleep};

// Idle window: one second is ~60 frame periods at 60 FPS, so an ungated loop
// would wake ~60 times before the message arrives.
const IDLE_WINDOW: Duration = Duration::from_secs(1);

// 60 FPS frame period, computed the same way the runtime does (dividing a
// one-second `Duration`), so the latency assertion stays exact.
const FRAME_PERIOD: Duration = Duration::from_nanos(1_000_000_000 / 60);

// A `tracing::Subscriber` that counts only frame-tick wake-ups, identified by
// the dedicated `tears::runtime::frame` target. Filtering in `enabled` means
// `event` is called for those events only.
#[derive(Clone, Default)]
struct FrameTickCounter {
    ticks: Arc<AtomicUsize>,
}

impl tracing::Subscriber for FrameTickCounter {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.target() == "tears::runtime::frame"
    }
    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
    fn event(&self, _event: &tracing::Event<'_>) {
        self.ticks.fetch_add(1, Ordering::SeqCst);
    }
    fn enter(&self, _span: &tracing::span::Id) {}
    fn exit(&self, _span: &tracing::span::Id) {}
}

// App for both tests: its init command idles for `IDLE_WINDOW`, then emits a
// message whose `update` quits. `view` records the virtual instant of every
// render so latency can be asserted.
struct IdleThenQuitApp {
    renders: Arc<Mutex<Vec<Instant>>>,
}

#[derive(Clone)]
struct Wake;

impl Application for IdleThenQuitApp {
    type Message = Wake;
    type Flags = Arc<Mutex<Vec<Instant>>>;

    fn new(renders: Self::Flags) -> (Self, Command<Self::Message>) {
        let cmd = Command::future(async {
            sleep(IDLE_WINDOW).await;
            Wake
        });
        (Self { renders }, cmd)
    }

    fn update(&mut self, _msg: Wake) -> Command<Self::Message> {
        Command::effect(Action::Quit)
    }

    fn view(&self, _frame: &mut Frame<'_>) {
        self.renders
            .lock()
            .expect("render log mutex should not be poisoned")
            .push(Instant::now());
    }

    fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
        vec![]
    }
}

// With paused virtual time, an ungated loop would auto-advance in frame-sized
// steps and wake ~60 times during the idle window; the gated loop has no armed
// frame timer while idle, so time jumps straight to the message.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn idle_loop_does_not_wake_at_frame_rate() -> Result<()> {
    let counter = FrameTickCounter::default();
    let ticks = counter.ticks.clone();
    // `set_default` is thread-local; on a current-thread runtime the spawned
    // tasks run on this same thread and so observe the subscriber.
    let _guard = tracing::subscriber::set_default(counter);

    let renders = Arc::new(Mutex::new(Vec::new()));
    let mut terminal = common::test_terminal()?;
    let runtime = Runtime::<IdleThenQuitApp>::try_new(renders, 60)?;
    runtime.run(&mut terminal).await?;

    // Only the initial render and the single post-message render wake the frame
    // branch. A count near the frame rate means the gate is missing.
    let count = ticks.load(Ordering::SeqCst);
    assert!(
        (1..=2).contains(&count),
        "idle loop should wake at most twice (initial + post-message render), \
         but woke {count} times; a value near the frame rate means the frame \
         branch is not gated on pending work",
    );

    Ok(())
}

// The message arrives at `IDLE_WINDOW`. Re-enabling the frame branch polls an
// interval whose deadline has already elapsed, so `tick()` is ready at once and
// the render happens at that same virtual instant rather than on the next frame
// boundary. (The immediate readiness comes from polling a past deadline;
// `MissedTickBehavior::Skip` only re-aligns the *following* tick.)
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn message_after_idle_renders_without_extra_frame_delay() -> Result<()> {
    // Install a subscriber here too so no test thread ever emits the
    // `tears::runtime::frame` callsite under `NoSubscriber`, which would poison
    // its cached interest for the parallel wake-up test.
    let _guard = tracing::subscriber::set_default(FrameTickCounter::default());

    let start = Instant::now();

    let renders = Arc::new(Mutex::new(Vec::new()));
    let mut terminal = common::test_terminal()?;
    let runtime = Runtime::<IdleThenQuitApp>::try_new(renders.clone(), 60)?;
    runtime.run(&mut terminal).await?;

    let times: Vec<Duration> = renders
        .lock()
        .expect("render log mutex should not be poisoned")
        .iter()
        .map(|instant| *instant - start)
        .collect();

    // Initial render at startup, then one render for the post-idle message.
    assert_eq!(
        times.len(),
        2,
        "expected an initial render and one post-message render, got {times:?}",
    );
    assert_eq!(
        times[0],
        Duration::ZERO,
        "initial render should be at startup"
    );

    let post = times[1];
    assert!(
        post >= IDLE_WINDOW && post < IDLE_WINDOW + FRAME_PERIOD,
        "a message arriving after idle should render at ~{IDLE_WINDOW:?} (got \
         {post:?}); a delay of a full frame period would mean the loop waited for \
         the next grid tick instead of the already-elapsed one",
    );

    Ok(())
}
