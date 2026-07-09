//! Runtime for executing TUI applications.
//!
//! This module provides the [`Runtime`] type that manages the application lifecycle
//! based on The Elm Architecture (TEA). It coordinates message processing, command
//! execution, subscription management, and rendering.
//!
//! # Overview
//!
//! The runtime follows The Elm Architecture pattern:
//!
//! 1. **Initialization**: Create a [`Runtime`] with initial flags and [`FrameRate`]
//! 2. **Event Loop**: Process messages via [`Application::update`], render via [`Application::view`]
//! 3. **Commands**: Execute asynchronous operations that produce messages
//! 4. **Subscriptions**: Receive external events (timers, signals, etc.)
//! 5. **Termination**: Exit cleanly when quit is requested
//!
//! # Performance Optimizations
//!
//! The runtime includes built-in optimizations that are transparent to applications:
//!
//! - **Micro-batching**: Messages arriving in quick succession (within 100μs) are
//!   batched together for processing, reducing overhead and improving responsiveness
//! - **Conditional Rendering**: The UI is only re-rendered when the application state
//!   changes, skipping unnecessary draw operations
//! - **Subscription Re-evaluation Gating**: Subscriptions are only re-evaluated after a
//!   message is processed (idle frames are skipped), since `subscriptions()` is a pure
//!   function of the application state
//!
//! # Example
//!
//! ```rust,no_run
//! use color_eyre::eyre::Result;
//! use ratatui::Frame;
//! use tears::prelude::*;
//!
//! struct CounterApp {
//!     count: i32,
//! }
//!
//! enum Message {
//!     Increment,
//!     Quit,
//! }
//!
//! impl Application for CounterApp {
//!     type Message = Message;
//!     type Flags = ();
//!
//!     fn new(_flags: ()) -> (Self, Command<Message>) {
//!         (Self { count: 0 }, Command::none())
//!     }
//!
//!     fn update(&mut self, msg: Message) -> Command<Message> {
//!         match msg {
//!             Message::Increment => {
//!                 self.count += 1;
//!                 Command::none()
//!             }
//!             Message::Quit => Command::effect(Action::Quit),
//!         }
//!     }
//!
//!     fn view(&self, frame: &mut Frame<'_>) {
//!         // Render UI...
//!     }
//!
//!     fn subscriptions(&self) -> Vec<Subscription<Message>> {
//!         vec![]
//!     }
//! }
//!
//! #[tokio::main]
//! async fn main() -> Result<()> {
//!     let runtime = Runtime::<CounterApp>::try_new((), 60)?;
//!     let mut terminal = ratatui::init();
//!     // Restore the terminal if the application panics.
//!     tears::install_panic_hook();
//!     runtime.run(&mut terminal).await?;
//!     ratatui::restore();
//!     Ok(())
//! }
//! ```

use std::time::{Duration, Instant};

use color_eyre::eyre::Result;
use futures::stream::StreamExt;
use ratatui::prelude::Backend;

use crate::{application::Application, command::Command};

mod app_input;
mod core;
// `FrameRate` is a scheduling input, so it lives with the runtime. `pub(crate)`
// lets `lib.rs`/`prelude` re-export it without exposing a second public path.
pub(crate) mod frame_rate;
mod frame_scheduler;
mod pending_work;

use app_input::AppInput;
use frame_rate::{FrameRate, FrameRateError};
use frame_scheduler::FrameScheduler;
// `self::` disambiguates the submodule from the built-in `core` crate.
use self::core::RuntimeCore;

/// Runtime that schedules and executes TUI application operations.
///
/// The runtime manages the main event loop following The Elm Architecture pattern.
/// It coordinates message processing, UI rendering, and subscription management with
/// built-in performance optimizations.
///
/// # Performance Features
///
/// - **Frame Rate Control**: Regulates rendering at the specified FPS (e.g., 60 FPS)
/// - **Micro-batching**: Processes messages arriving within 100μs together, reducing overhead
/// - **Conditional Rendering**: Only renders when state changes, saving CPU cycles
/// - **Idle Wake-up Elision**: Skips frame ticks entirely while idle, so the loop is
///   event-driven and does not consume CPU at the frame rate with nothing to render
/// - **Subscription Re-evaluation Gating**: Re-evaluates subscriptions only after a message
///   is processed, since the subscription set is a pure function of application state
///
/// # Type Parameters
///
/// * `App` - The application type implementing [`Application`]
///
/// # Examples
///
/// See the [module-level documentation](self) for a complete example.
pub struct Runtime<App: Application> {
    /// The runtime's owned execution resources (app, channels, subscriptions, tasks)
    core: RuntimeCore<App>,
    /// Schedules frame work and gates idle wake-ups
    scheduler: FrameScheduler,
}

impl<App: Application> Runtime<App> {
    /// Creates a new runtime with the given initialization flags and frame rate.
    ///
    /// Initializes the application by calling [`Application::new`] with the provided flags.
    /// Any initialization commands returned are automatically enqueued for execution when
    /// [`run`](Self::run) is called.
    ///
    /// # Arguments
    ///
    /// * `flags` - Configuration data passed to [`Application::new`]
    /// * `frame_rate` - Validated target frames per second
    ///
    /// # Notes
    ///
    /// The actual frame rate may be lower if rendering or message processing takes longer
    /// than the frame duration. Missed frames are skipped rather than accumulated.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use tears::prelude::*;
    /// # use ratatui::Frame;
    /// #
    /// # struct MyApp;
    /// # enum Message {}
    /// # impl Application for MyApp {
    /// #     type Message = Message;
    /// #     type Flags = ();
    /// #     fn new(_: ()) -> (Self, Command<Message>) { (MyApp, Command::none()) }
    /// #     fn update(&mut self, _: Message) -> Command<Message> { Command::none() }
    /// #     fn view(&self, _: &mut Frame<'_>) {}
    /// #     fn subscriptions(&self) -> Vec<Subscription<Message>> { vec![] }
    /// # }
    ///
    /// // Create runtime with 60 FPS target
    /// let frame_rate = FrameRate::try_new(60).expect("frame rate must be valid");
    /// let runtime = Runtime::<MyApp>::new((), frame_rate);
    /// ```
    #[must_use]
    pub fn new(flags: App::Flags, frame_rate: FrameRate) -> Self {
        let core = RuntimeCore::new(flags);

        Self {
            core,
            scheduler: FrameScheduler::new(frame_rate),
        }
    }

    /// Tries to create a new runtime with the given initialization flags and FPS value.
    ///
    /// This is a convenience wrapper around [`FrameRate::try_new`] and
    /// [`Runtime::new`].
    ///
    /// # Errors
    ///
    /// Returns [`FrameRateError`] when `frame_rate` is zero or too high to
    /// produce a non-zero frame duration.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use tears::prelude::*;
    /// # use ratatui::Frame;
    /// #
    /// # struct MyApp;
    /// # enum Message {}
    /// # impl Application for MyApp {
    /// #     type Message = Message;
    /// #     type Flags = ();
    /// #     fn new(_: ()) -> (Self, Command<Message>) { (MyApp, Command::none()) }
    /// #     fn update(&mut self, _: Message) -> Command<Message> { Command::none() }
    /// #     fn view(&self, _: &mut Frame<'_>) {}
    /// #     fn subscriptions(&self) -> Vec<Subscription<Message>> { vec![] }
    /// # }
    /// let runtime = Runtime::<MyApp>::try_new((), 60).expect("frame rate must be valid");
    /// ```
    pub fn try_new(
        flags: App::Flags,
        frame_rate: u32,
    ) -> std::result::Result<Self, FrameRateError> {
        Ok(Self::new(flags, FrameRate::try_new(frame_rate)?))
    }

    #[cfg(test)]
    fn process_message_batch(&mut self, first_msg: App::Message) {
        self.process_input_batch(AppInput::Shared(first_msg));
    }

    /// Processes a batch of application inputs that arrive in quick succession
    /// (micro-batching).
    ///
    /// Processes the first input immediately, then attempts to process additional
    /// inputs that arrived within 100μs. This reduces overhead by batching rapid
    /// input sequences while maintaining responsiveness.
    ///
    /// Records a redraw request if any processed command wants one, and always
    /// marks subscriptions dirty after processing.
    fn process_input_batch(&mut self, first_input: AppInput<App::Message>) {
        self.process_app_input(first_input);
        let mut processed = 1usize;

        // Micro-batching: process additional messages that arrived during a short window
        let batch_deadline = Instant::now() + Duration::from_micros(100);
        while Instant::now() < batch_deadline {
            match self.core.app_inputs.try_next_ready() {
                Some(input) => {
                    self.process_app_input(input);
                    processed += 1;
                }
                None => break, // No more inputs available
            }
        }

        tracing::trace!(target: "tears::runtime", messages = processed, "processed message batch");

        // Mark that subscriptions may have changed even when redraw is suppressed.
        self.scheduler.mark_subscriptions_dirty();
    }

    fn process_app_input(&mut self, input: AppInput<App::Message>) {
        match input {
            AppInput::Shared(msg) => {
                let cmd = self.core.app.update(msg);
                self.dispatch_update_command(cmd);
            }
        }
    }

    fn dispatch_update_command(&mut self, cmd: Command<App::Message>) {
        let parts = cmd.into_runtime_parts();
        self.scheduler.record_redraw(parts.requests_redraw());
        self.core.enqueue_command(parts);
    }

    /// Processes a frame tick: renders if needed and updates subscriptions.
    ///
    /// Only renders when [`FrameScheduler`] has a pending redraw (conditional
    /// rendering optimization). Only re-evaluates subscriptions when it has marked
    /// them dirty, i.e. when a message has been processed since the last
    /// evaluation.
    ///
    /// Quit is not detected here: the event loop's `select!` has a dedicated
    /// `quit_rx.recv()` branch that handles it, so this method only renders and
    /// re-evaluates subscriptions.
    fn process_frame_tick<B: Backend>(
        &mut self,
        terminal: &mut ratatui::Terminal<B>,
    ) -> Result<(), <B as Backend>::Error> {
        // Emitted on every frame branch wake-up (before the redraw check), so the
        // event loop's idle behavior is observable via `tracing`. A dedicated
        // target lets subscribers count wake-ups without matching the message.
        tracing::trace!(target: "tears::runtime::frame", "frame tick");

        // Render only if state has changed
        if self.scheduler.take_redraw() {
            self.core.render(terminal)?;
            tracing::trace!(target: "tears::runtime", "frame rendered");
        }

        // Re-evaluate subscriptions only when the state may have changed.
        // Since `subscriptions()` is a pure function of the application state,
        // an idle frame (no messages processed) cannot change the subscription
        // set, so we skip the (potentially costly) evaluation entirely.
        if self.scheduler.take_subscriptions_dirty() {
            self.update_subscriptions();
        }

        Ok(())
    }

    /// Re-evaluates the application's subscriptions and applies them.
    ///
    /// Calls [`Application::subscriptions`] and hands the result to
    /// [`SubscriptionManager::update`], which diffs against the running set: it
    /// starts new subscriptions, cancels removed ones, and — crucially —
    /// restarts subscriptions whose tasks have finished but are still requested.
    ///
    /// The manager's diff is already ID-keyed and cheap, so this is called on
    /// every dirty frame without an additional caching layer. An earlier
    /// hash-of-IDs cache here skipped the manager update whenever the ID set was
    /// unchanged, which suppressed the restart of finished subscriptions and
    /// violated the manager's documented contract.
    fn update_subscriptions(&mut self) {
        let subscriptions = self.core.app.subscriptions();
        let count = subscriptions.len();
        self.core.subscription_manager.update(subscriptions);
        // Fires on every dirty frame regardless of whether the set actually
        // changed; the accurate change signals are the manager's
        // "subscription started"/"subscription stopped" events.
        tracing::debug!(target: "tears::runtime", count, "subscriptions re-evaluated");
    }

    /// Runs the runtime until the application quits.
    ///
    /// This is the main entry point for executing the application. It starts the event loop
    /// that processes messages, renders the UI, and manages subscriptions. The loop continues
    /// until the application sends a quit signal via [`Action::Quit`](crate::command::Action::Quit).
    ///
    /// # Event Loop
    ///
    /// The event loop operates on three concurrent channels using `tokio::select!`:
    ///
    /// 1. **Message Channel**: Processes messages through [`Application::update`]. Messages
    ///    arriving within 100μs are batched together for efficiency.
    /// 2. **Frame Timer**: Renders UI via [`Application::view`] (only when state changed)
    ///    and updates subscriptions at the specified frame rate. The frame branch is
    ///    skipped while the application is idle (no pending redraw or subscription
    ///    update), so the loop does not wake at the frame rate with nothing to do.
    /// 3. **Quit Channel**: Terminates the loop when quit signal is received.
    ///
    /// Commands returned from [`Application::update`] are executed asynchronously as
    /// tokio tasks, allowing multiple operations to run concurrently.
    ///
    /// # Arguments
    ///
    /// * `terminal` - Ratatui terminal instance for rendering
    ///
    /// # Errors
    ///
    /// Returns an error if terminal rendering fails, typically due to I/O errors or
    /// terminal disconnection. Such errors are usually unrecoverable.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use color_eyre::eyre::Result;
    /// # use ratatui::Frame;
    /// # use tears::prelude::*;
    /// #
    /// # struct MyApp;
    /// # enum Message { Quit }
    /// # impl Application for MyApp {
    /// #     type Message = Message;
    /// #     type Flags = ();
    /// #     fn new(_: ()) -> (Self, Command<Message>) { (MyApp, Command::none()) }
    /// #     fn update(&mut self, msg: Message) -> Command<Message> {
    /// #         match msg {
    /// #             Message::Quit => Command::effect(Action::Quit),
    /// #         }
    /// #     }
    /// #     fn view(&self, frame: &mut Frame<'_>) {}
    /// #     fn subscriptions(&self) -> Vec<Subscription<Message>> { vec![] }
    /// # }
    /// #
    /// #[tokio::main]
    /// async fn main() -> Result<()> {
    ///     let mut terminal = ratatui::init();
    ///     // Restore the terminal if the application panics.
    ///     tears::install_panic_hook();
    ///
    ///     let runtime = Runtime::<MyApp>::try_new((), 60)?;
    ///     runtime.run(&mut terminal).await?;
    ///
    ///     ratatui::restore();
    ///     Ok(())
    /// }
    /// ```
    pub async fn run<B: Backend>(
        mut self,
        terminal: &mut ratatui::Terminal<B>,
    ) -> Result<(), <B as Backend>::Error> {
        self.core.initialize_subscriptions();
        tracing::debug!(target: "tears::runtime", "runtime started");

        loop {
            tokio::select! {
                // Message received: batch process messages that arrive in quick succession
                Some(input) = self.core.app_inputs.next() => {
                    self.process_input_batch(input);
                }

                // Frame tick: render if needed and update subscriptions. The
                // scheduler parks while idle, so the loop does not wake at the
                // frame rate just to do nothing. When a message re-enables frame
                // work, the elapsed timer is ready on the next poll and adds no
                // render latency; `MissedTickBehavior::Skip` only controls where
                // the following tick lands (no catch-up burst).
                () = self.scheduler.next_work_frame() => {
                    self.process_frame_tick(terminal)?;
                }

                // Quit signal received
                _ = self.core.quit_rx.recv() => {
                    tracing::debug!(target: "tears::runtime", "quit signal received");
                    break;
                }
            }
        }

        tracing::debug!(target: "tears::runtime", "runtime shutting down");
        self.core.shutdown();

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::Application;
    use crate::command::Command;
    use crate::subscription::Subscription;
    use crate::test_support::{TestApp, TestMessage, wait_until};
    use ratatui::backend::TestBackend;
    use ratatui::prelude::*;
    use tokio::time::{Duration, sleep};

    fn frame_rate(value: u32) -> FrameRate {
        FrameRate::try_new(value).expect("frame rate must be valid")
    }

    struct RedrawControlApp;

    #[derive(Debug, Clone)]
    enum RedrawControlMessage {
        Redraw,
        Skip,
    }

    impl Application for RedrawControlApp {
        type Message = RedrawControlMessage;
        type Flags = ();

        fn new((): ()) -> (Self, Command<Self::Message>) {
            (Self, Command::none())
        }

        fn update(&mut self, msg: Self::Message) -> Command<Self::Message> {
            match msg {
                RedrawControlMessage::Redraw => Command::none(),
                RedrawControlMessage::Skip => Command::none().without_redraw(),
            }
        }

        fn view(&self, _frame: &mut Frame<'_>) {}

        fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
            vec![]
        }
    }

    // Runtime tests

    #[tokio::test]
    async fn test_event_loop_new() {
        let runtime = Runtime::<TestApp>::new(0, frame_rate(60));

        // Runtime should be created successfully
        assert_eq!(runtime.core.app.counter, 0);
    }

    #[tokio::test]
    async fn test_event_loop_new_with_different_frame_rates() {
        let _runtime1 = Runtime::<TestApp>::new(0, frame_rate(30));
        let _runtime2 = Runtime::<TestApp>::new(0, frame_rate(144));

        // Should handle different frame rates without panic
    }

    #[tokio::test]
    async fn test_runtime_frame_scheduler_period_is_accurate() {
        // 60 FPS should yield a ~16.667ms period. The previous integer
        // millisecond division (1000 / 60 = 16ms) truncated this, producing
        // an effective ~62.5 FPS.
        let runtime = Runtime::<TestApp>::new(0, frame_rate(60));
        let period = runtime.scheduler.frame_period();
        assert!(
            period >= Duration::from_micros(16_600) && period <= Duration::from_micros(16_700),
            "60 FPS period should be ~16.667ms, got {period:?}",
        );

        // 144 FPS should yield a ~6.944ms period (not the truncated 6ms).
        let runtime = Runtime::<TestApp>::new(0, frame_rate(144));
        let period = runtime.scheduler.frame_period();
        assert!(
            period >= Duration::from_micros(6_900) && period <= Duration::from_micros(6_950),
            "144 FPS period should be ~6.944ms, got {period:?}",
        );
    }

    #[tokio::test]
    async fn test_event_loop_process_message_batch_single_message() {
        let mut runtime = Runtime::<TestApp>::new(0, frame_rate(60));

        // Initially no redraw needed (well, actually true for initial state)
        // Process a message
        runtime.process_message_batch(TestMessage::Increment);

        // Counter should be incremented
        assert_eq!(runtime.core.app.counter, 1);

        // Redraw should be needed
        assert!(runtime.scheduler.pending.needs_redraw);
    }

    #[tokio::test]
    async fn test_process_message_batch_without_redraw_leaves_redraw_unchanged() {
        let mut runtime = Runtime::<RedrawControlApp>::new((), frame_rate(60));
        runtime.scheduler.pending.needs_redraw = false;
        runtime.scheduler.pending.subscriptions_dirty = false;

        runtime.process_message_batch(RedrawControlMessage::Skip);

        assert!(!runtime.scheduler.pending.needs_redraw);
        assert!(
            runtime.scheduler.pending.subscriptions_dirty,
            "redraw suppression must not suppress subscription re-evaluation"
        );
    }

    #[tokio::test]
    async fn test_process_message_batch_mixed_commands_redraws() {
        let mut runtime = Runtime::<RedrawControlApp>::new((), frame_rate(60));
        runtime.scheduler.pending.needs_redraw = false;
        runtime.scheduler.pending.subscriptions_dirty = false;

        let _ = runtime.core.msg_tx.send(RedrawControlMessage::Redraw);
        runtime.process_message_batch(RedrawControlMessage::Skip);

        assert!(runtime.scheduler.pending.needs_redraw);
        assert!(runtime.scheduler.pending.subscriptions_dirty);
    }

    #[tokio::test]
    async fn test_process_message_batch_redraw_recovers_after_suppression() {
        let mut runtime = Runtime::<RedrawControlApp>::new((), frame_rate(60));
        runtime.scheduler.pending.needs_redraw = false;

        runtime.process_message_batch(RedrawControlMessage::Skip);
        assert!(!runtime.scheduler.pending.needs_redraw);

        runtime.scheduler.pending.subscriptions_dirty = false;
        runtime.process_message_batch(RedrawControlMessage::Redraw);

        assert!(runtime.scheduler.pending.needs_redraw);
        assert!(runtime.scheduler.pending.subscriptions_dirty);
    }

    #[tokio::test]
    async fn test_process_message_batch_emits_tracing_event() {
        let recorder = crate::test_support::TraceRecorder::new().with_target("tears::runtime");
        let _guard = recorder.set_default();

        let mut runtime = Runtime::<TestApp>::new(0, frame_rate(60));
        runtime.process_message_batch(TestMessage::Increment);

        assert!(
            recorder.event_count() >= 1,
            "processing a message batch should emit a tracing event"
        );
    }

    #[tokio::test]
    async fn test_event_loop_process_message_batch_with_batching() {
        let mut runtime = Runtime::<TestApp>::new(0, frame_rate(60));

        // Send multiple messages to the queue
        let _ = runtime.core.msg_tx.send(TestMessage::Increment);
        let _ = runtime.core.msg_tx.send(TestMessage::Increment);
        let _ = runtime.core.msg_tx.send(TestMessage::Increment);

        // Process first message (should batch the others within the deadline)
        runtime.process_message_batch(TestMessage::Increment);

        // All messages should be processed (1 direct + 3 batched = 4 total)
        assert_eq!(runtime.core.app.counter, 4);
    }

    #[tokio::test]
    async fn test_process_input_batch_drains_shared_inputs_in_fifo_order() {
        struct BatchOrderApp {
            messages: Vec<i32>,
        }

        impl Application for BatchOrderApp {
            type Message = i32;
            type Flags = ();

            fn new((): ()) -> (Self, Command<Self::Message>) {
                (
                    Self {
                        messages: Vec::new(),
                    },
                    Command::none(),
                )
            }

            fn update(&mut self, msg: Self::Message) -> Command<Self::Message> {
                self.messages.push(msg);
                Command::none()
            }

            fn view(&self, _frame: &mut Frame<'_>) {}

            fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
                vec![]
            }
        }

        let mut runtime = Runtime::<BatchOrderApp>::new((), frame_rate(60));
        runtime.scheduler.pending.subscriptions_dirty = false;

        runtime
            .core
            .msg_tx
            .send(2)
            .expect("receiver should be open");
        runtime
            .core
            .msg_tx
            .send(3)
            .expect("receiver should be open");

        runtime.process_input_batch(AppInput::Shared(1));

        assert_eq!(runtime.core.app.messages, vec![1, 2, 3]);
        assert!(runtime.scheduler.pending.subscriptions_dirty);
    }

    #[tokio::test]
    async fn test_event_loop_process_frame_tick_renders_when_needed() -> Result<()> {
        let mut runtime = Runtime::<TestApp>::new(0, frame_rate(60));

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend)?;

        // Initially needs_redraw is true
        assert!(runtime.scheduler.pending.needs_redraw);

        // Process frame tick
        runtime.process_frame_tick(&mut terminal)?;

        // Redraw flag should be cleared
        assert!(!runtime.scheduler.pending.needs_redraw);

        Ok(())
    }

    #[tokio::test]
    async fn test_event_loop_process_frame_tick_skips_render_when_not_needed() -> Result<()> {
        let mut runtime = Runtime::<TestApp>::new(0, frame_rate(60));

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend)?;

        // Clear the needs_redraw flag
        runtime.scheduler.pending.needs_redraw = false;

        // Process frame tick
        runtime.process_frame_tick(&mut terminal)?;

        // Redraw flag should still be false
        assert!(!runtime.scheduler.pending.needs_redraw);

        Ok(())
    }

    // Quit detection lives solely in the event loop's `quit_rx.recv()` branch
    // (`process_frame_tick` no longer polls the quit channel), so it is covered
    // at the `run()` level: an `Action::Quit` must terminate the loop.
    #[tokio::test]
    async fn test_event_loop_run_quits_on_quit_action() -> Result<()> {
        let runtime = Runtime::<TestApp>::new(0, frame_rate(60));

        // A `Quit` message routes to `Action::Quit`, which the loop's dedicated
        // quit branch receives.
        let _ = runtime.core.msg_tx.send(TestMessage::Quit);

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend)?;

        // `run()` must return promptly; the timeout guards against a hang if the
        // quit path regresses.
        tokio::time::timeout(Duration::from_secs(5), runtime.run(&mut terminal))
            .await
            .expect("run() should quit before the timeout")?;

        Ok(())
    }

    // Quit must terminate the loop even while idle, i.e. after the initial frame
    // has rendered and the frame branch is gated off by `has_pending_work()`.
    // This is the exact safety property the frame-branch gating relies on: the
    // `quit_rx.recv()` branch is always armed, so a quit arriving with no pending
    // redraw or subscription work is still received.
    //
    // The quit is sent directly to `quit_tx` (bypassing the update/command
    // pipeline, so this isolates the quit branch) and, crucially, is *delayed*
    // until after startup. If it were sent before `run()`, the initial
    // `needs_redraw == true` would let the first frame tick run and consume it,
    // so the loop would never reach the idle state under test — and a regression
    // that moved quit handling back onto the frame branch would slip through.
    //
    // With paused virtual time on a current-thread runtime: the loop renders the
    // initial frame (clearing `needs_redraw`), goes idle with the frame branch
    // gated off, and the clock auto-advances to the spawned task's timer — the
    // only armed timer — which delivers the quit. If quit handling regressed onto
    // the (now gated-off) frame branch, nothing would receive it while idle and
    // the outer timeout would fire.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn test_event_loop_run_quits_while_idle() -> Result<()> {
        let runtime = Runtime::<TestApp>::new(0, frame_rate(60));

        // Deliver the quit only after the loop has rendered its first frame and
        // gone idle. A clone keeps `quit_tx` alive after `run()` takes ownership
        // of the runtime.
        let quit_tx = runtime.core.quit_tx.clone();
        tokio::spawn(async move {
            sleep(Duration::from_secs(1)).await;
            let _ = quit_tx.send(());
        });

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend)?;

        tokio::time::timeout(Duration::from_secs(5), runtime.run(&mut terminal))
            .await
            .expect("run() should quit while idle before the timeout")?;

        Ok(())
    }

    // --- Idle frame wake-up elision -----------------------------------------
    //
    // The scheduler gates its frame branch on `PendingWork::has_pending_work()`
    // so an idle loop stops waking at the frame rate. The predicate's pure logic
    // is unit-tested in `pending_work`; the scheduler's idle parking is unit-
    // tested in `frame_scheduler`; these tests cover the Runtime-level
    // integration — that a real frame tick drains the pending work and a processed
    // message re-arms the branch. The end-to-end behavior (zero idle wake-ups, and
    // an immediate render once a message re-enables the branch) is covered by
    // `tests/idle_wakeup.rs`.

    #[tokio::test]
    async fn test_frame_branch_gated_off_when_idle() -> Result<()> {
        let mut runtime = Runtime::<TestApp>::new(0, frame_rate(60));
        let mut terminal = Terminal::new(TestBackend::new(80, 24))?;

        // Draining the initial pending work leaves both flags false: idle.
        runtime.process_frame_tick(&mut terminal)?;
        assert!(!runtime.scheduler.pending.has_pending_work());

        Ok(())
    }

    #[tokio::test]
    async fn test_frame_branch_reenabled_after_message() -> Result<()> {
        let mut runtime = Runtime::<TestApp>::new(0, frame_rate(60));
        let mut terminal = Terminal::new(TestBackend::new(80, 24))?;

        runtime.process_frame_tick(&mut terminal)?;
        assert!(!runtime.scheduler.pending.has_pending_work());

        // A processed message re-enables the frame branch.
        runtime.process_message_batch(TestMessage::Increment);
        assert!(runtime.scheduler.pending.has_pending_work());

        Ok(())
    }

    #[tokio::test]
    async fn test_event_loop_process_frame_tick_updates_subscriptions() -> Result<()> {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        use futures::stream::{self, BoxStream};

        use crate::subscription::{SubscriptionId, SubscriptionSource};

        // A subscription whose stream stays parked (never yields), so once
        // started its task stays alive. Counts how many times it is spawned so
        // that "the frame tick actually applied the subscriptions" is
        // observable rather than merely inferred from the dirty flag.
        struct ParkedSource {
            spawns: Arc<AtomicUsize>,
        }

        impl SubscriptionSource for ParkedSource {
            type Output = ();

            fn stream(&self) -> BoxStream<'static, ()> {
                self.spawns.fetch_add(1, Ordering::SeqCst);
                stream::pending().boxed()
            }

            fn id(&self) -> SubscriptionId {
                SubscriptionId::of::<Self>(0)
            }
        }

        struct App {
            spawns: Arc<AtomicUsize>,
        }

        impl Application for App {
            type Message = ();
            type Flags = Arc<AtomicUsize>;

            fn new(spawns: Arc<AtomicUsize>) -> (Self, Command<Self::Message>) {
                (Self { spawns }, Command::none())
            }

            fn update(&mut self, (): ()) -> Command<Self::Message> {
                Command::none()
            }

            fn view(&self, _frame: &mut Frame<'_>) {}

            fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
                vec![Subscription::new(ParkedSource {
                    spawns: self.spawns.clone(),
                })]
            }
        }

        let spawns = Arc::new(AtomicUsize::new(0));
        let mut runtime = Runtime::<App>::new(spawns.clone(), frame_rate(60));

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend)?;

        // Clear needs_redraw so only the subscription path exercises the tick.
        runtime.scheduler.pending.needs_redraw = false;

        // Not yet applied: no frame tick has run.
        assert_eq!(spawns.load(Ordering::SeqCst), 0);

        // Subscriptions are only re-evaluated when marked dirty (after a
        // message). Simulate that here.
        runtime.scheduler.pending.subscriptions_dirty = true;

        // Process frame tick: it must re-evaluate and actually start the
        // subscription, and clear the dirty flag.
        runtime.process_frame_tick(&mut terminal)?;

        assert!(!runtime.scheduler.pending.subscriptions_dirty);
        wait_until(
            || spawns.load(Ordering::SeqCst) == 1,
            "a dirty frame tick must actually start the requested subscription",
        )
        .await;

        Ok(())
    }

    // App that counts how many times `subscriptions()` is evaluated.
    struct SubCountingApp {
        sub_calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Application for SubCountingApp {
        type Message = ();
        type Flags = std::sync::Arc<std::sync::atomic::AtomicUsize>;

        fn new(
            sub_calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        ) -> (Self, Command<Self::Message>) {
            (Self { sub_calls }, Command::none())
        }

        fn update(&mut self, (): ()) -> Command<Self::Message> {
            Command::none()
        }

        fn view(&self, _frame: &mut Frame<'_>) {}

        fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
            self.sub_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            vec![]
        }
    }

    #[tokio::test]
    async fn test_subscriptions_not_reevaluated_on_idle_frames() -> Result<()> {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let counter = Arc::new(AtomicUsize::new(0));
        let mut runtime = Runtime::<SubCountingApp>::new(counter.clone(), frame_rate(60));

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend)?;

        // Several frame ticks without any messages must not re-evaluate
        // subscriptions, since the app state cannot have changed.
        runtime.process_frame_tick(&mut terminal)?;
        runtime.process_frame_tick(&mut terminal)?;
        runtime.process_frame_tick(&mut terminal)?;

        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "subscriptions() should not be called on idle frames"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_subscriptions_reevaluated_after_message() -> Result<()> {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let counter = Arc::new(AtomicUsize::new(0));
        let mut runtime = Runtime::<SubCountingApp>::new(counter.clone(), frame_rate(60));

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend)?;

        // Idle frame: no evaluation.
        runtime.process_frame_tick(&mut terminal)?;
        assert_eq!(counter.load(Ordering::SeqCst), 0);

        // Processing a message marks subscriptions dirty.
        runtime.process_message_batch(());

        // The next frame tick re-evaluates subscriptions exactly once.
        runtime.process_frame_tick(&mut terminal)?;
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        // Subsequent idle frames do not re-evaluate again.
        runtime.process_frame_tick(&mut terminal)?;
        runtime.process_frame_tick(&mut terminal)?;
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        Ok(())
    }

    // Regression test: a finished subscription must be restarted on every
    // re-evaluation even when its ID is unchanged.
    //
    // `SubscriptionManager::update` documents that "subscriptions whose tasks
    // have finished will be restarted if still present". An earlier hash-of-IDs
    // cache in `update_subscriptions` skipped the manager update whenever the ID
    // set was unchanged. The skip only bit on the *second* re-evaluation: the
    // first cached the hash (and did restart), but every later re-evaluation
    // with the same ID set was elided, so a finite subscription that finished
    // again was never restarted. This test therefore drives two message-gated
    // re-evaluations and asserts the subscription restarts each time. Removing
    // the cache means every dirty frame calls `update`, restoring the restart.
    #[tokio::test]
    async fn test_finished_subscription_restarted_with_unchanged_id() -> Result<()> {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        use futures::stream::{self, BoxStream};

        use crate::subscription::{SubscriptionId, SubscriptionSource};

        #[derive(Clone)]
        struct RestartCounters {
            spawns: Arc<AtomicUsize>,
            completions: Arc<AtomicUsize>,
        }

        // A subscription with a fixed ID whose stream emits one value and then
        // ends. It counts spawns and completions so restarts are observable
        // without sleeping for the previous task to finish.
        struct OneshotSource {
            counters: RestartCounters,
        }

        impl SubscriptionSource for OneshotSource {
            type Output = ();

            fn stream(&self) -> BoxStream<'static, ()> {
                self.counters.spawns.fetch_add(1, Ordering::SeqCst);
                let completions = self.counters.completions.clone();
                stream::unfold(false, move |emitted| {
                    let completions = completions.clone();
                    async move {
                        if emitted {
                            completions.fetch_add(1, Ordering::SeqCst);
                            None
                        } else {
                            Some(((), true))
                        }
                    }
                })
                .boxed()
            }

            fn id(&self) -> SubscriptionId {
                // A constant ID: the set of IDs never changes across frames,
                // which is exactly the case the removed hash cache skipped.
                SubscriptionId::of::<Self>(0)
            }
        }

        struct RestartApp {
            counters: RestartCounters,
        }

        impl Application for RestartApp {
            type Message = ();
            type Flags = RestartCounters;

            fn new(counters: RestartCounters) -> (Self, Command<Self::Message>) {
                (Self { counters }, Command::none())
            }

            fn update(&mut self, (): ()) -> Command<Self::Message> {
                Command::none()
            }

            fn view(&self, _frame: &mut Frame<'_>) {}

            fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
                vec![Subscription::new(OneshotSource {
                    counters: self.counters.clone(),
                })]
            }
        }

        let counters = RestartCounters {
            spawns: Arc::new(AtomicUsize::new(0)),
            completions: Arc::new(AtomicUsize::new(0)),
        };
        let mut runtime = Runtime::<RestartApp>::new(counters.clone(), frame_rate(60));
        let mut terminal = Terminal::new(TestBackend::new(80, 24))?;

        // Start subscriptions the way `run()` does: the first spawn happens here.
        runtime.core.initialize_subscriptions();
        wait_until(
            || counters.spawns.load(Ordering::SeqCst) >= 1,
            "initial subscription spawn should start before the timeout",
        )
        .await;

        // Drive two message-gated re-evaluations. The ID never changes, so the
        // removed hash cache would have skipped the manager update from the
        // second round onward, leaving the finished subscription dead. Each
        // round must restart it, so the spawn count must keep climbing.
        for round in 2..=3 {
            wait_until(
                || counters.completions.load(Ordering::SeqCst) >= round - 1,
                "current one-shot subscription should finish before re-evaluation",
            )
            .await;

            // A message marks subscriptions dirty; the next frame re-evaluates.
            runtime.process_message_batch(());
            runtime.process_frame_tick(&mut terminal)?;

            wait_until(
                || counters.spawns.load(Ordering::SeqCst) >= round,
                "finished subscription should restart before the timeout",
            )
            .await;
        }

        Ok(())
    }
}
