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
//! use std::num::NonZeroU32;
//!
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
//!             Message::Quit => Command::quit(),
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
//!     let frame_rate = FrameRate::new(NonZeroU32::new(60).expect("non-zero"))?;
//!     let runtime = Runtime::<CounterApp>::new((), frame_rate);
//!     let mut terminal = ratatui::init();
//!     // Restore the terminal if the application panics.
//!     tears::install_panic_hook();
//!     runtime.run(&mut terminal).await?;
//!     ratatui::restore();
//!     Ok(())
//! }
//! ```

use std::num::NonZeroUsize;

use color_eyre::eyre::Result;
use futures::stream::StreamExt;
use ratatui::prelude::Backend;
use tokio::time::{Duration, Instant};

use crate::{application::Application, command::Command};

mod app_input;
// `pub` (not `pub(crate)`) so `subscription`'s forwarding task can name the
// shared-channel `Sender` it feeds: `runtime` is already `pub(crate)`, so `pub`
// caps this submodule's effective reachability at the crate the same way, while
// avoiding the redundant-`pub(crate)` lint (see `frame_rate` below). The type
// carries no public API.
pub mod channel;
// `pub` for the same reason as `frame_rate` below: `runtime` is already
// `pub(crate)`, so `pub` only lets `lib.rs` re-export `RuntimeConfig` at the
// crate root; it does not widen effective reachability.
pub mod config;
mod core;
// `FrameRate` is a scheduling input, so it lives with the runtime. `pub`
// (not `pub(crate)`) because `runtime` itself is already `pub(crate)`,
// which caps this submodule's effective reachability the same way;
// `lib.rs`/`prelude` still need it nameable for their re-exports.
pub mod frame_rate;
mod frame_scheduler;
mod keyed_commands;
// `pub` for the same crate-capping reason as `channel` above: crate-internal
// load-observability types (`tears::runtime::load` schema, RFC 0006 §4.4) that
// `subscription` and the channel/keyed producers share.
pub mod load;
mod pending_work;

use app_input::AppInput;
use config::RuntimeConfig;
use frame_rate::FrameRate;
use frame_scheduler::FrameScheduler;
use keyed_commands::{CommandOutput, ReceiverEvent};
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
/// See the [crate-level documentation](crate) for a complete example.
pub struct Runtime<App: Application> {
    /// The runtime's owned execution resources (app, channels, subscriptions, tasks)
    core: RuntimeCore<App>,
    /// Schedules frame work and gates idle wake-ups
    scheduler: FrameScheduler,
    /// Count cap for one micro-batch window; `None` leaves the batch capped
    /// only by the 100µs time window (RFC 0006 INV-L12).
    batch_max_messages: Option<NonZeroUsize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BatchOutcome {
    Continue,
    Quit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InputOutcome {
    Updated,
    NoUpdate,
    Quit,
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
    /// # use std::num::NonZeroU32;
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
    /// let frame_rate = FrameRate::new(NonZeroU32::new(60).expect("non-zero"))
    ///     .expect("frame rate must be valid");
    /// let runtime = Runtime::<MyApp>::new((), frame_rate);
    /// ```
    #[must_use]
    pub fn new(flags: App::Flags, frame_rate: FrameRate) -> Self {
        // Literal delegation to `with_config` (RFC 0007 INV-C1): exactly one
        // construction path exists, and the load-control-unset configuration
        // selects RFC 0006's unchanged unbounded path within it.
        Self::with_config(flags, RuntimeConfig::new(frame_rate))
    }

    /// Creates a new runtime from a [`RuntimeConfig`], which carries the frame
    /// rate together with the opt-in load controls (RFC 0006).
    ///
    /// [`new`](Self::new) is equivalent to `with_config(flags,
    /// RuntimeConfig::new(frame_rate))`: a default (load-control-unset)
    /// configuration reproduces the unbounded delivery mode exactly. Use
    /// `with_config` to opt into bounded delivery by setting one or more of the
    /// [`RuntimeConfig`] capacities.
    ///
    /// # Arguments
    ///
    /// * `flags` - Configuration data passed to [`Application::new`]
    /// * `config` - Frame rate and opt-in load controls
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use std::num::{NonZeroU32, NonZeroUsize};
    /// # use tears::{FrameRate, Runtime, RuntimeConfig};
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
    /// let frame_rate = FrameRate::new(NonZeroU32::new(60).expect("non-zero"))
    ///     .expect("valid frame rate");
    /// let config = RuntimeConfig::new(frame_rate)
    ///     .app_channel_capacity(NonZeroUsize::new(1024).expect("non-zero"))
    ///     .keyed_channel_capacity(NonZeroUsize::new(16).expect("non-zero"));
    /// let runtime = Runtime::<MyApp>::with_config((), config);
    /// ```
    #[must_use]
    pub fn with_config(flags: App::Flags, config: RuntimeConfig) -> Self {
        // The single channel-construction path (RFC 0006 INV-L6): unset
        // capacities build the unchanged unbounded channels, `Some(n)` bound
        // them.
        let core = RuntimeCore::with_capacities(
            flags,
            config.app_channel_capacity,
            config.keyed_channel_capacity,
        );
        // INV-C5: the scheduler is paced by the frame rate the caller supplied
        // to `RuntimeConfig::new`, never a hardcoded one.
        let scheduler = FrameScheduler::new(config.frame_rate);

        Self {
            core,
            scheduler,
            batch_max_messages: config.batch_max_messages,
        }
    }

    #[cfg(test)]
    fn process_message_batch(&mut self, first_msg: App::Message) {
        let _ = self.process_input_batch(AppInput::Shared(first_msg));
    }

    /// Processes a batch of application inputs that arrive in quick succession
    /// (micro-batching).
    ///
    /// Processes the first input immediately, then attempts to process additional
    /// inputs that arrived within 100μs. This reduces overhead by batching rapid
    /// input sequences while maintaining responsiveness.
    ///
    /// Records a redraw request if any processed command wants one, and marks
    /// subscriptions dirty when at least one input invokes `update`.
    fn process_input_batch(&mut self, first_input: AppInput<App::Message>) -> BatchOutcome {
        let mut processed = match self.process_app_input(first_input) {
            InputOutcome::Updated => 1usize,
            InputOutcome::NoUpdate => 0usize,
            InputOutcome::Quit => return BatchOutcome::Quit,
        };

        // INV-L12: the opening input is the batch's first pulled input, so a
        // configured `batch_max_messages` of `Some(n)` admits at most `n - 1`
        // more. Every input the batch pulls counts — including inputs that do
        // not invoke `update` (a keyed `Closed`) — so this counter is distinct
        // from `processed`, which counts only `update` calls.
        let mut pulled = 1usize;

        // Micro-batching: process additional messages that arrived during a short window.
        // Uses tokio::time::Instant (not std) so tests can pause the clock and avoid
        // flaking under CI scheduling jitter around this tight deadline.
        let batch_deadline = Instant::now() + Duration::from_micros(100);
        while Instant::now() < batch_deadline {
            // Stop once the batch has pulled its configured maximum number of
            // inputs (INV-L12). Checked before the pull so `Some(1)` confines
            // the batch to just the opening input.
            if self
                .batch_max_messages
                .is_some_and(|max| pulled >= max.get())
            {
                break;
            }
            match self.core.app_inputs.try_next_ready() {
                Some(input) => {
                    pulled += 1;
                    match self.process_app_input(input) {
                        InputOutcome::Updated => processed += 1,
                        InputOutcome::NoUpdate => {}
                        InputOutcome::Quit => {
                            if processed > 0 {
                                self.scheduler.mark_subscriptions_dirty();
                            }
                            return BatchOutcome::Quit;
                        }
                    }
                }
                None => break, // No more inputs available
            }
        }

        // Batch event (RFC 0006 §4.4): `pulled` counts every input this batch
        // took (INV-L12's counted unit), `processed` those that invoked
        // `update`, and `shared_pending` the shared-channel occupancy now. A
        // quit-terminated batch returns above and never reaches here.
        load::batch(pulled, processed, self.core.app_inputs.shared_pending());

        // Mark that subscriptions may have changed even when redraw is suppressed.
        if processed > 0 {
            self.scheduler.mark_subscriptions_dirty();
        }
        BatchOutcome::Continue
    }

    fn process_app_input(&mut self, input: AppInput<App::Message>) -> InputOutcome {
        match input {
            AppInput::Shared(msg)
            | AppInput::Keyed(ReceiverEvent::Output(CommandOutput::Message(msg))) => {
                let cmd = self.core.app.update(msg);
                self.dispatch_update_command(cmd);
                InputOutcome::Updated
            }
            AppInput::Keyed(ReceiverEvent::Output(CommandOutput::Quit)) => InputOutcome::Quit,
            AppInput::Keyed(ReceiverEvent::Closed) => InputOutcome::NoUpdate,
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
    /// until the application sends a quit signal via [`Command::quit`](crate::Command::quit).
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
    /// # use std::num::NonZeroU32;
    /// #
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
    /// #             Message::Quit => Command::quit(),
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
    ///     let frame_rate = FrameRate::new(NonZeroU32::new(60).expect("non-zero"))?;
    ///     let runtime = Runtime::<MyApp>::new((), frame_rate);
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
                    if self.process_input_batch(input) == BatchOutcome::Quit {
                        tracing::debug!(target: "tears::runtime", "keyed quit signal received");
                        break;
                    }
                }

                // Frame tick: render if needed and update subscriptions. The
                // scheduler parks while idle, so the loop does not wake at the
                // frame rate just to do nothing. When a message re-enables frame
                // work, the elapsed deadline is ready on the next poll and adds
                // no render latency; missed frame deadlines are never replayed —
                // at most one immediate frame fires after a stall, and the
                // cadence resumes on the anchor's phase (the same non-catch-up
                // rule as `Timer`, RFC 0009 §4.2).
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

    use std::future::pending;
    use std::hash::{Hash, Hasher};
    use std::num::NonZeroU32;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use futures::stream::{self, BoxStream, iter};
    use ratatui::backend::TestBackend;
    use ratatui::prelude::*;
    use tokio::sync::oneshot;
    use tokio::time::{Duration, sleep, timeout};

    use crate::application::Application;
    use crate::command::{CancelPolicy, Command, CommandId};
    use crate::subscription::{Subscription, SubscriptionSource};
    use crate::test_support::{TestApp, TestMessage, TraceRecorder, wait_until};

    fn frame_rate(value: u32) -> FrameRate {
        FrameRate::new(NonZeroU32::new(value).expect("frame rate must be non-zero"))
            .expect("frame rate must be valid")
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

    // INV-C5: `with_config` builds the frame scheduler from `config.frame_rate`
    // — the value the caller supplied to `RuntimeConfig::new` — never a
    // hardcoded one. A direct, deterministic value comparison (no elapsed-time
    // observation) that fails an implementation which ignores `config.frame_rate`
    // in exactly the way INV-C1/INV-C2 cannot.
    #[tokio::test]
    async fn test_with_config_scheduler_uses_config_frame_rate() {
        for rate in [30, 144] {
            let config = RuntimeConfig::new(frame_rate(rate));
            let runtime = Runtime::<TestApp>::with_config(0, config);
            assert_eq!(
                runtime.scheduler.frame_period(),
                frame_rate(rate).frame_duration(),
                "with_config at {rate} FPS must pace the scheduler at that rate",
            );
        }
    }

    // INV-C1: `Runtime::new` is a literal delegation to `with_config` with a
    // load-control-unset configuration, so the two construct an equivalently
    // paced runtime. The behavioral witness of the single construction path.
    #[tokio::test]
    async fn test_new_delegates_to_with_config() {
        let via_new = Runtime::<TestApp>::new(0, frame_rate(60));
        let via_config = Runtime::<TestApp>::with_config(0, RuntimeConfig::new(frame_rate(60)));

        assert_eq!(
            via_new.scheduler.frame_period(),
            via_config.scheduler.frame_period(),
            "new(flags, fr) must construct the same runtime as with_config(flags, RuntimeConfig::new(fr))",
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

        let _ = runtime.core.msg_tx.try_send(RedrawControlMessage::Redraw);
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

    // Batch event (RFC 0006 §4.4, INV-L13) at the runtime batch layer: a batch
    // that pulls the opening input plus one queued input reports `pulled = 2`,
    // `updated = 2` (both invoked `update`), and `shared_pending = 0` (the
    // shared channel drained). `pulled` is unique to the batch event, so the
    // value assertions are unaffected by any gauge events on the same target.
    #[tokio::test(start_paused = true)]
    async fn batch_event_reports_pulled_updated_and_shared_pending() {
        let recorder = TraceRecorder::new().with_target("tears::runtime::load");
        let _guard = recorder.set_default();

        let mut runtime = Runtime::<TestApp>::new(0, frame_rate(60));
        runtime
            .core
            .msg_tx
            .try_send(TestMessage::Increment)
            .expect("receiver should be open");
        runtime.process_message_batch(TestMessage::Increment);

        assert_eq!(
            recorder.u64_values("pulled"),
            vec![2],
            "the opening input plus one batched input"
        );
        assert_eq!(
            recorder.u64_values("updated"),
            vec![2],
            "both pulled inputs invoked update"
        );
        assert_eq!(
            recorder.u64_values("shared_pending"),
            vec![0],
            "the shared channel drained by batch end"
        );
    }

    // Batch event differing-value case (RFC 0006 §4.4 DoD): a batch whose
    // opening input is a keyed `Closed` reports `pulled = 1`, `updated = 0` —
    // the pulled input did not invoke `update`. This is the deterministic
    // differ-value case the DoD places at the runtime batch layer; a queued
    // `Closed` is not deterministically constructible (see INV-L12).
    #[tokio::test]
    async fn batch_event_reports_pulled_without_updated_for_a_closed_input() {
        let recorder = TraceRecorder::new().with_target("tears::runtime::load");
        let _guard = recorder.set_default();

        let mut runtime = Runtime::<TestApp>::new(0, frame_rate(60));
        runtime.process_input_batch(AppInput::Keyed(ReceiverEvent::Closed));

        assert_eq!(
            recorder.u64_values("pulled"),
            vec![1],
            "the Closed input was pulled"
        );
        assert_eq!(
            recorder.u64_values("updated"),
            vec![0],
            "a Closed input does not invoke update"
        );
        assert_eq!(recorder.u64_values("shared_pending"), vec![0]);
    }

    // Batch event `shared_pending` against a scripted leftover (RFC 0006 §4.4
    // DoD): with `batch_max_messages = Some(n)` and `n + k` shared messages
    // queued under the paused clock, the capped batch reports
    // `shared_pending = k` — the inputs it left in the shared channel.
    #[tokio::test(start_paused = true)]
    async fn batch_event_reports_shared_pending_leftover_under_cap() {
        let recorder = TraceRecorder::new().with_target("tears::runtime::load");
        let _guard = recorder.set_default();

        // n = 2, k = 1: three queued shared inputs, capped at two per batch.
        let config = RuntimeConfig::new(frame_rate(60))
            .batch_max_messages(NonZeroUsize::new(2).expect("non-zero"));
        let mut runtime = Runtime::<TestApp>::with_config(0, config);
        for _ in 0..3 {
            runtime
                .core
                .msg_tx
                .try_send(TestMessage::Increment)
                .expect("receiver should be open");
        }

        let opener = runtime
            .core
            .app_inputs
            .next()
            .await
            .expect("the first input is ready");
        runtime.process_input_batch(opener);

        assert_eq!(recorder.u64_values("pulled"), vec![2], "the cap of two");
        assert_eq!(
            recorder.u64_values("shared_pending"),
            vec![1],
            "one input left queued after the capped batch"
        );
    }

    // INV-L13: a quit-terminated batch emits no batch event — the loop exits
    // instead (RFC 0006 §4.4). Opening `process_input_batch` on a keyed quit
    // returns `Quit` before the batch event would fire, so no `pulled` is
    // emitted.
    #[tokio::test]
    async fn quit_terminated_batch_emits_no_batch_event() {
        let recorder = TraceRecorder::new().with_target("tears::runtime::load");
        let _guard = recorder.set_default();

        let mut runtime = Runtime::<TestApp>::new(0, frame_rate(60));
        let outcome = runtime
            .process_input_batch(AppInput::Keyed(ReceiverEvent::Output(CommandOutput::Quit)));

        assert_eq!(outcome, BatchOutcome::Quit);
        assert!(
            recorder.u64_values("pulled").is_empty(),
            "a quit-terminated batch must emit no batch event"
        );
    }

    // The `keyed_commands` gauge is count-based, not guard-based, so a runtime
    // dropped without a clean `shutdown()` — e.g. a render error propagating out
    // of `run()` via `?` — must still reset it. `KeyedCommands`'s `Drop`
    // publishes zero on that path.
    #[tokio::test]
    async fn dropping_a_runtime_with_a_pending_keyed_command_resets_the_keyed_gauge() {
        let recorder = TraceRecorder::new().with_target("tears::runtime::load");
        let _guard = recorder.set_default();

        {
            let mut runtime = Runtime::<TestApp>::new(0, frame_rate(60));
            runtime.core.enqueue_command(
                Command::future(pending::<TestMessage>())
                    .cancellable(CommandId::new("keyed"))
                    .into_runtime_parts(),
            );
            assert_eq!(
                recorder.u64_values("keyed_commands").last(),
                Some(&1),
                "spawning a keyed command raises the gauge"
            );
            // `runtime` is dropped here — no `run()`, no `shutdown()`.
        }

        assert_eq!(
            recorder.u64_values("keyed_commands").last(),
            Some(&0),
            "dropping the runtime resets the keyed_commands gauge"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_event_loop_process_message_batch_with_batching() {
        let mut runtime = Runtime::<TestApp>::new(0, frame_rate(60));

        // Send multiple messages to the queue
        let _ = runtime.core.msg_tx.try_send(TestMessage::Increment);
        let _ = runtime.core.msg_tx.try_send(TestMessage::Increment);
        let _ = runtime.core.msg_tx.try_send(TestMessage::Increment);

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
            .try_send(2)
            .expect("receiver should be open");
        runtime
            .core
            .msg_tx
            .try_send(3)
            .expect("receiver should be open");

        runtime.process_input_batch(AppInput::Shared(1));

        assert_eq!(runtime.core.app.messages, vec![1, 2, 3]);
        assert!(runtime.scheduler.pending.subscriptions_dirty);
    }

    // App that records every message `update` receives, in order.
    struct RecordingApp {
        messages: Vec<i32>,
    }

    impl Application for RecordingApp {
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

    // INV-L12 (exact-n, the lower half of the off-by-one pair): with exactly `n`
    // inputs ready and `batch_max_messages = Some(n)`, one batch pulls all `n` —
    // the opening input plus `n - 1` more — and nothing is left over. The clock
    // is paused so the 100µs time cap never fires; the count cap alone bounds the
    // batch (with the clock frozen the loop would otherwise drain everything
    // ready). The opener is pulled from `app_inputs` exactly as `run()` does, so
    // it is a genuine first pulled input, not a synthesized one.
    #[tokio::test(start_paused = true)]
    async fn batch_pulls_all_inputs_when_exactly_the_cap_are_ready() {
        let config = RuntimeConfig::new(frame_rate(60))
            .batch_max_messages(NonZeroUsize::new(2).expect("non-zero"));
        let mut runtime = Runtime::<RecordingApp>::with_config((), config);

        // Exactly n = 2 ready inputs.
        for msg in [1, 2] {
            runtime
                .core
                .msg_tx
                .try_send(msg)
                .expect("receiver should be open");
        }
        let opener = runtime
            .core
            .app_inputs
            .next()
            .await
            .expect("the first input is ready");

        assert_eq!(runtime.process_input_batch(opener), BatchOutcome::Continue);

        // All n pulled in one batch; nothing left over.
        assert_eq!(runtime.core.app.messages, vec![1, 2]);
        assert_eq!(runtime.core.app_inputs.try_next_ready(), None);
    }

    // INV-L12 (exact-n+1, the upper half of the off-by-one pair): with `n + 1`
    // inputs ready and `batch_max_messages = Some(n)`, one batch pulls exactly
    // `n` and the single remaining input is delivered by the *next* batch. This
    // is the boundary that a cap of `n + 1` (or a missing cap) would fail by
    // draining the remainder into the first batch.
    #[tokio::test(start_paused = true)]
    async fn batch_pulls_exactly_the_cap_and_defers_the_remainder_to_the_next_batch() {
        let config = RuntimeConfig::new(frame_rate(60))
            .batch_max_messages(NonZeroUsize::new(2).expect("non-zero"));
        let mut runtime = Runtime::<RecordingApp>::with_config((), config);

        // n + 1 = 3 ready inputs.
        for msg in [1, 2, 3] {
            runtime
                .core
                .msg_tx
                .try_send(msg)
                .expect("receiver should be open");
        }

        let opener = runtime
            .core
            .app_inputs
            .next()
            .await
            .expect("the first input is ready");
        assert_eq!(runtime.process_input_batch(opener), BatchOutcome::Continue);

        // The first batch pulls exactly n; the remainder waits.
        assert_eq!(runtime.core.app.messages, vec![1, 2]);

        // The next batch delivers the leftover, in FIFO order.
        let next_opener = runtime
            .core
            .app_inputs
            .next()
            .await
            .expect("the leftover input is ready for the next batch");
        assert_eq!(
            runtime.process_input_batch(next_opener),
            BatchOutcome::Continue
        );
        assert_eq!(runtime.core.app.messages, vec![1, 2, 3]);
        assert_eq!(runtime.core.app_inputs.try_next_ready(), None);
    }

    // INV-L12 (the `Closed`-counts case): the count is over *pulled inputs*, not
    // over `update` calls — a keyed `Closed` is pulled but does not invoke
    // `update`, yet it still consumes a slot. Under `Some(1)` an opening `Closed`
    // ends the batch immediately, leaving a queued shared input untouched; if the
    // cap counted only `update` calls, `pulled` would stay 0 across the `Closed`
    // and the queued increment would be pulled into this batch (counter == 1).
    //
    // This uses `Closed` in the opening position deliberately. A `Closed` can
    // only ever be pulled at the *tail* of a batch: `AppInputs` pulls shared
    // inputs before keyed (shared-first, RFC §4.7), and a `Closed` is surfaced
    // only by an already-empty, sender-closed keyed receiver (a receiver that
    // still holds a buffered message is removed the moment that message is
    // pulled, before any `Closed`). So no real input is ever pull-ordered after a
    // `Closed` from a single source, and a second keyed source would make the
    // order non-deterministic (`StreamMap` fairness). The opening position is
    // therefore the only placement that deterministically distinguishes "counts
    // pulled inputs" from "counts `update` calls"; `Closed` surfacing itself is
    // locked separately in `keyed_commands` (`pending_keyed_poll_wakes_after_sender_closure`).
    #[tokio::test(start_paused = true)]
    async fn batch_max_of_one_stops_after_a_non_update_opening_closed() {
        let config = RuntimeConfig::new(frame_rate(60))
            .batch_max_messages(NonZeroUsize::new(1).expect("non-zero"));
        let mut runtime = Runtime::<TestApp>::with_config(0, config);

        // A shared input the batch must NOT pull under a cap of 1.
        runtime
            .core
            .msg_tx
            .try_send(TestMessage::Increment)
            .expect("receiver should be open");

        // Open the batch with a keyed `Closed` (a pulled input that does not
        // invoke `update`). With a cap of 1 it is the sole pulled input.
        let outcome = runtime.process_input_batch(AppInput::Keyed(ReceiverEvent::Closed));
        assert_eq!(outcome, BatchOutcome::Continue);

        // The queued increment was left for the next batch, not consumed.
        assert_eq!(runtime.core.app.counter, 0);
        assert!(matches!(
            runtime.core.app_inputs.try_next_ready(),
            Some(AppInput::Shared(TestMessage::Increment))
        ));
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
        let _ = runtime.core.msg_tx.try_send(TestMessage::Quit);

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend)?;

        // `run()` must return promptly; the timeout guards against a hang if the
        // quit path regresses.
        timeout(Duration::from_secs(5), runtime.run(&mut terminal))
            .await
            .expect("run() should quit before the timeout")?;

        Ok(())
    }

    #[tokio::test]
    async fn shared_cancel_drops_ready_keyed_output_before_batch_delivery() {
        enum Message {
            Leave,
            Result(i32),
        }

        struct CancellationApp {
            results: Vec<i32>,
        }

        impl Application for CancellationApp {
            type Message = Message;
            type Flags = ();

            fn new((): ()) -> (Self, Command<Self::Message>) {
                (
                    Self {
                        results: Vec::new(),
                    },
                    Command::none(),
                )
            }

            fn update(&mut self, message: Self::Message) -> Command<Self::Message> {
                match message {
                    Message::Leave => Command::cancel(CommandId::new("search")),
                    Message::Result(value) => {
                        self.results.push(value);
                        Command::none()
                    }
                }
            }

            fn view(&self, _frame: &mut Frame<'_>) {}

            fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
                Vec::new()
            }
        }

        let id = CommandId::new("search");
        let mut runtime = Runtime::<CancellationApp>::new((), frame_rate(60));
        runtime.core.enqueue_command(
            Command::stream(iter([Message::Result(1), Message::Result(2)]))
                .cancellable(id.clone())
                .into_runtime_parts(),
        );
        wait_until(
            || runtime.core.app_inputs.has_closed_buffered(&id),
            "keyed results should be buffered before cancellation",
        )
        .await;
        runtime
            .core
            .msg_tx
            .try_send(Message::Leave)
            .expect("message receiver should be open");

        let first = runtime
            .core
            .app_inputs
            .next()
            .await
            .expect("shared input should be ready");
        assert!(matches!(first, AppInput::Shared(Message::Leave)));
        assert_eq!(runtime.process_input_batch(first), BatchOutcome::Continue);

        assert!(runtime.core.app.results.is_empty());
        assert!(runtime.core.app_inputs.try_next_ready().is_none());
    }

    #[tokio::test]
    async fn keep_in_flight_applies_redraw_and_cancels_before_dropping_new_stream() {
        struct DropGuard(Arc<AtomicBool>);

        impl Drop for DropGuard {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let keep_id = CommandId::new("kept");
        let cancel_id = CommandId::new("cancelled");
        let kept_dropped = Arc::new(AtomicBool::new(false));
        let cancelled_dropped = Arc::new(AtomicBool::new(false));
        let arrival_dropped = Arc::new(AtomicBool::new(false));
        let (kept_started_tx, kept_started_rx) = oneshot::channel();
        let (cancelled_started_tx, cancelled_started_rx) = oneshot::channel();
        let mut runtime = Runtime::<TestApp>::new(0, frame_rate(60));

        let kept_guard = DropGuard(Arc::clone(&kept_dropped));
        runtime.core.enqueue_command(
            Command::future(async move {
                let _guard = kept_guard;
                let _ = kept_started_tx.send(());
                pending::<TestMessage>().await
            })
            .cancellable(keep_id.clone())
            .into_runtime_parts(),
        );
        let cancelled_guard = DropGuard(Arc::clone(&cancelled_dropped));
        runtime.core.enqueue_command(
            Command::future(async move {
                let _guard = cancelled_guard;
                let _ = cancelled_started_tx.send(());
                pending::<TestMessage>().await
            })
            .cancellable(cancel_id.clone())
            .into_runtime_parts(),
        );
        timeout(Duration::from_secs(1), kept_started_rx)
            .await
            .expect("kept command should start before the timeout")
            .expect("kept command should signal that it started");
        timeout(Duration::from_secs(1), cancelled_started_rx)
            .await
            .expect("cancelled command should start before the timeout")
            .expect("cancelled command should signal that it started");

        runtime.scheduler.pending.needs_redraw = false;
        let arrival_guard = DropGuard(Arc::clone(&arrival_dropped));
        let dropped_arrival = Command::future(async move {
            let _guard = arrival_guard;
            pending::<TestMessage>().await
        });
        let command = Command::batch([Command::cancel(cancel_id), dropped_arrival])
            .cancellable_with(keep_id, CancelPolicy::KeepInFlight);

        runtime.dispatch_update_command(command);

        assert!(runtime.scheduler.pending.needs_redraw);
        assert!(arrival_dropped.load(Ordering::SeqCst));
        assert!(!kept_dropped.load(Ordering::SeqCst));
        wait_until(
            || cancelled_dropped.load(Ordering::SeqCst),
            "explicit cancels should be applied before KeepInFlight drops the arrival",
        )
        .await;

        runtime.core.shutdown();
        wait_until(
            || kept_dropped.load(Ordering::SeqCst),
            "shutdown should clean up the kept command",
        )
        .await;
    }

    /// Derives the scoped `CommandId` that `.cancellable(CommandId::new(local)).scoped(scope)`
    /// would install as its keyed spawn id, without depending on `CommandId`'s
    /// internal shape.
    fn scoped_local_id<Scope>(local: &'static str, scope: Scope) -> CommandId
    where
        Scope: Eq + Hash + Send + Sync + 'static,
    {
        let (cancels, _, _) = Command::<TestMessage>::cancel(CommandId::new(local))
            .scoped(scope)
            .into_runtime_parts()
            .into_execution_parts();
        cancels
            .into_iter()
            .next()
            .expect("cancel id should be present")
    }

    /// A scope segment type whose `Hash` implementation always writes the
    /// same value, so its `PartialEq`/`Eq` (compared by `id`) is the only
    /// thing distinguishing two instances. Used to pin RFC 0005 section 6.4's
    /// requirement that constant-hash scope values still keep command
    /// replacement, `KeepInFlight` suppression, and explicit cancellation
    /// isolated per scope.
    #[derive(Clone, Copy, Eq, PartialEq)]
    struct CollidingScope(u8);

    impl Hash for CollidingScope {
        fn hash<H: Hasher>(&self, state: &mut H) {
            0_u8.hash(state);
        }
    }

    // RFC 0005 Phase B: `Command::scoped` must make the same local
    // `CommandId` independent across composition boundaries, so cancelling
    // one scope's slot cannot affect an equal local id under another scope.
    #[tokio::test]
    async fn scoped_keyed_commands_do_not_cross_cancel_or_replace() {
        let pane_a = scoped_local_id("search", "pane-a");
        let pane_b = scoped_local_id("search", "pane-b");
        let mut runtime = Runtime::<TestApp>::new(0, frame_rate(60));

        // Both panes reuse the same local id ("search"); if scoping failed to
        // qualify it, the second spawn would replace the first under
        // `CancelPolicy::CancelInFlight` instead of buffering independently.
        runtime.core.enqueue_command(
            Command::stream(iter([TestMessage::Increment]))
                .cancellable(CommandId::new("search"))
                .scoped("pane-a")
                .into_runtime_parts(),
        );
        runtime.core.enqueue_command(
            Command::stream(iter([TestMessage::Increment]))
                .cancellable(CommandId::new("search"))
                .scoped("pane-b")
                .into_runtime_parts(),
        );

        wait_until(
            || {
                runtime.core.app_inputs.has_closed_buffered(&pane_a)
                    && runtime.core.app_inputs.has_closed_buffered(&pane_b)
            },
            "both pane-scoped keyed results should buffer independently",
        )
        .await;

        runtime.core.app_inputs.cancel_keyed(&pane_a);

        assert!(!runtime.core.app_inputs.has_closed_buffered(&pane_a));
        assert!(
            runtime.core.app_inputs.has_closed_buffered(&pane_b),
            "cancelling one pane's scoped id must not affect the other pane"
        );
    }

    // RFC 0005 Phase B (INV-19): reusing the same local `CommandId` under
    // `CancelPolicy::KeepInFlight` in two different scopes must not let one
    // pane's occupancy suppress the other pane's stream.
    #[tokio::test]
    async fn scoped_keep_in_flight_does_not_suppress_a_different_pane() {
        struct DropGuard(Arc<AtomicBool>);

        impl Drop for DropGuard {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let pane_a_dropped = Arc::new(AtomicBool::new(false));
        let (pane_a_started_tx, pane_a_started_rx) = oneshot::channel();
        let mut runtime = Runtime::<TestApp>::new(0, frame_rate(60));

        let guard = DropGuard(Arc::clone(&pane_a_dropped));
        runtime.core.enqueue_command(
            Command::future(async move {
                let _guard = guard;
                let _ = pane_a_started_tx.send(());
                pending::<TestMessage>().await
            })
            .cancellable_with(CommandId::new("search"), CancelPolicy::KeepInFlight)
            .scoped("pane-a")
            .into_runtime_parts(),
        );
        timeout(Duration::from_secs(1), pane_a_started_rx)
            .await
            .expect("pane-a command should start before the timeout")
            .expect("pane-a command should signal that it started");

        // Same local id ("search") under pane-b, also `KeepInFlight`. If
        // scoping failed and both aliased to one slot, pane-a's occupancy
        // would silently discard this stream instead of delivering it.
        runtime.core.enqueue_command(
            Command::stream(iter([TestMessage::Increment]))
                .cancellable_with(CommandId::new("search"), CancelPolicy::KeepInFlight)
                .scoped("pane-b")
                .into_runtime_parts(),
        );

        let pane_b = scoped_local_id("search", "pane-b");
        wait_until(
            || runtime.core.app_inputs.has_closed_buffered(&pane_b),
            "pane-b's stream must deliver instead of being suppressed by pane-a's occupancy",
        )
        .await;

        assert!(
            !pane_a_dropped.load(Ordering::SeqCst),
            "pane-a's in-flight command must not be cancelled by pane-b"
        );

        runtime.core.shutdown();
    }

    // RFC 0005 section 6.4: "the same constant-hash scope values keep
    // command replacement, suppression, and explicit cancellation isolated."
    // The tests above already cover ordinary (non-colliding) scope values;
    // these three repeat that coverage under `CollidingScope`, whose `Hash`
    // always writes the same byte, to pin that scope equality — not merely
    // scope inequality — drives isolation.
    #[tokio::test]
    async fn scoped_explicit_cancel_is_isolated_under_hash_colliding_scopes() {
        let scope_a = scoped_local_id("search", CollidingScope(1));
        let scope_b = scoped_local_id("search", CollidingScope(2));
        let mut runtime = Runtime::<TestApp>::new(0, frame_rate(60));

        runtime.core.enqueue_command(
            Command::stream(iter([TestMessage::Increment]))
                .cancellable(CommandId::new("search"))
                .scoped(CollidingScope(1))
                .into_runtime_parts(),
        );
        runtime.core.enqueue_command(
            Command::stream(iter([TestMessage::Increment]))
                .cancellable(CommandId::new("search"))
                .scoped(CollidingScope(2))
                .into_runtime_parts(),
        );

        wait_until(
            || {
                runtime.core.app_inputs.has_closed_buffered(&scope_a)
                    && runtime.core.app_inputs.has_closed_buffered(&scope_b)
            },
            "both hash-colliding scopes' keyed results should buffer independently",
        )
        .await;

        runtime.core.app_inputs.cancel_keyed(&scope_a);

        assert!(!runtime.core.app_inputs.has_closed_buffered(&scope_a));
        assert!(
            runtime.core.app_inputs.has_closed_buffered(&scope_b),
            "cancelling one hash-colliding scope's id must not affect the other"
        );
    }

    #[tokio::test]
    async fn scoped_keep_in_flight_is_isolated_under_hash_colliding_scopes() {
        struct DropGuard(Arc<AtomicBool>);

        impl Drop for DropGuard {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let scope_a_dropped = Arc::new(AtomicBool::new(false));
        let (scope_a_started_tx, scope_a_started_rx) = oneshot::channel();
        let mut runtime = Runtime::<TestApp>::new(0, frame_rate(60));

        let guard = DropGuard(Arc::clone(&scope_a_dropped));
        runtime.core.enqueue_command(
            Command::future(async move {
                let _guard = guard;
                let _ = scope_a_started_tx.send(());
                pending::<TestMessage>().await
            })
            .cancellable_with(CommandId::new("search"), CancelPolicy::KeepInFlight)
            .scoped(CollidingScope(1))
            .into_runtime_parts(),
        );
        timeout(Duration::from_secs(1), scope_a_started_rx)
            .await
            .expect("scope-1 command should start before the timeout")
            .expect("scope-1 command should signal that it started");

        // Same local id ("search") under a different, hash-colliding scope,
        // also `KeepInFlight`. If scope equality fell back to the collided
        // hash, both would alias one slot and scope-1's occupancy would
        // silently discard this stream instead of delivering it.
        runtime.core.enqueue_command(
            Command::stream(iter([TestMessage::Increment]))
                .cancellable_with(CommandId::new("search"), CancelPolicy::KeepInFlight)
                .scoped(CollidingScope(2))
                .into_runtime_parts(),
        );

        let scope_b = scoped_local_id("search", CollidingScope(2));
        wait_until(
            || runtime.core.app_inputs.has_closed_buffered(&scope_b),
            "scope-2's stream must deliver instead of being suppressed by scope-1's occupancy",
        )
        .await;

        assert!(
            !scope_a_dropped.load(Ordering::SeqCst),
            "scope-1's in-flight command must not be cancelled by scope-2"
        );

        runtime.core.shutdown();
    }

    #[tokio::test]
    async fn scoped_replacement_is_isolated_under_hash_colliding_scopes() {
        struct DropGuard(Arc<AtomicBool>);

        impl Drop for DropGuard {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let scope_a_dropped = Arc::new(AtomicBool::new(false));
        let scope_b_dropped = Arc::new(AtomicBool::new(false));
        let (scope_a_started_tx, scope_a_started_rx) = oneshot::channel();
        let (scope_b_started_tx, scope_b_started_rx) = oneshot::channel();
        let mut runtime = Runtime::<TestApp>::new(0, frame_rate(60));

        // Both use the default `CancelPolicy::CancelInFlight`, so a same-scope
        // replacement is expected to cancel the prior occupant of that scope.
        let guard_a = DropGuard(Arc::clone(&scope_a_dropped));
        runtime.core.enqueue_command(
            Command::future(async move {
                let _guard = guard_a;
                let _ = scope_a_started_tx.send(());
                pending::<TestMessage>().await
            })
            .cancellable(CommandId::new("search"))
            .scoped(CollidingScope(1))
            .into_runtime_parts(),
        );
        let guard_b = DropGuard(Arc::clone(&scope_b_dropped));
        runtime.core.enqueue_command(
            Command::future(async move {
                let _guard = guard_b;
                let _ = scope_b_started_tx.send(());
                pending::<TestMessage>().await
            })
            .cancellable(CommandId::new("search"))
            .scoped(CollidingScope(2))
            .into_runtime_parts(),
        );
        timeout(Duration::from_secs(1), scope_a_started_rx)
            .await
            .expect("scope-1 command should start before the timeout")
            .expect("scope-1 command should signal that it started");
        timeout(Duration::from_secs(1), scope_b_started_rx)
            .await
            .expect("scope-2 command should start before the timeout")
            .expect("scope-2 command should signal that it started");

        // Replace the same local id under scope-1 again. If scope equality
        // fell back to the collided hash, this would also cancel scope-2.
        runtime.core.enqueue_command(
            Command::future(pending::<TestMessage>())
                .cancellable(CommandId::new("search"))
                .scoped(CollidingScope(1))
                .into_runtime_parts(),
        );

        wait_until(
            || scope_a_dropped.load(Ordering::SeqCst),
            "the replaced scope-1 command should be cancelled",
        )
        .await;
        assert!(
            !scope_b_dropped.load(Ordering::SeqCst),
            "replacing scope-1's occupant must not cancel scope-2's command"
        );

        runtime.core.shutdown();
    }

    #[tokio::test]
    async fn live_keyed_quit_exits_the_runtime() -> Result<()> {
        struct KeyedQuitApp;

        impl Application for KeyedQuitApp {
            type Message = ();
            type Flags = ();

            fn new((): ()) -> (Self, Command<Self::Message>) {
                (Self, Command::quit().cancellable(CommandId::new("quit")))
            }

            fn update(&mut self, (): ()) -> Command<Self::Message> {
                Command::none()
            }

            fn view(&self, _frame: &mut Frame<'_>) {}

            fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
                Vec::new()
            }
        }

        let runtime = Runtime::<KeyedQuitApp>::new((), frame_rate(60));
        let mut terminal = Terminal::new(TestBackend::new(80, 24))?;

        timeout(Duration::from_secs(1), runtime.run(&mut terminal))
            .await
            .expect("live keyed quit should stop the runtime")?;
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

        timeout(Duration::from_secs(5), runtime.run(&mut terminal))
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
        // A subscription whose stream stays parked (never yields), so once
        // started its task stays alive. Counts how many times it is spawned so
        // that "the frame tick actually applied the subscriptions" is
        // observable rather than merely inferred from the dirty flag.
        struct ParkedSource {
            spawns: Arc<AtomicUsize>,
        }

        impl SubscriptionSource for ParkedSource {
            type Output = ();
            type Key = ();

            fn stream(&self) -> BoxStream<'static, ()> {
                self.spawns.fetch_add(1, Ordering::SeqCst);
                stream::pending().boxed()
            }

            fn key(&self) -> Self::Key {}
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
        sub_calls: Arc<AtomicUsize>,
    }

    impl Application for SubCountingApp {
        type Message = ();
        type Flags = Arc<AtomicUsize>;

        fn new(sub_calls: Arc<AtomicUsize>) -> (Self, Command<Self::Message>) {
            (Self { sub_calls }, Command::none())
        }

        fn update(&mut self, (): ()) -> Command<Self::Message> {
            Command::none()
        }

        fn view(&self, _frame: &mut Frame<'_>) {}

        fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
            self.sub_calls.fetch_add(1, Ordering::SeqCst);
            vec![]
        }
    }

    #[tokio::test]
    async fn test_subscriptions_not_reevaluated_on_idle_frames() -> Result<()> {
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
            type Key = ();

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

            fn key(&self) -> Self::Key {
                // A constant ID: the set of IDs never changes across frames,
                // which is exactly the case the removed hash cache skipped.
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

    // --- RFC 0011 steady-state phase order ----------------------------------
    //
    // The white-box seam RFC 0011 §8 names for INV-LC1/INV-LC2: drive
    // `process_input_batch`/`process_frame_tick` directly with a recording
    // application whose `view` and `subscriptions` append the state they
    // observed to a shared log, so the phase sequence of a batch and of a frame
    // pass is asserted rather than inferred from the pending flags.

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Phase {
        View(i32),
        Subscriptions(i32),
    }

    #[derive(Clone, Default)]
    struct PhaseLog(Arc<Mutex<Vec<Phase>>>);

    impl PhaseLog {
        fn push(&self, phase: Phase) {
            self.0
                .lock()
                .expect("phase log mutex should not be poisoned")
                .push(phase);
        }

        fn entries(&self) -> Vec<Phase> {
            self.0
                .lock()
                .expect("phase log mutex should not be poisoned")
                .clone()
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum PhaseMessage {
        Bump,
        BumpWithoutRedraw,
    }

    struct PhaseApp {
        state: i32,
        log: PhaseLog,
    }

    impl Application for PhaseApp {
        type Message = PhaseMessage;
        type Flags = PhaseLog;

        fn new(log: PhaseLog) -> (Self, Command<Self::Message>) {
            // The init command opts out of a redraw deliberately: the runtime
            // never consults that directive (RFC 0011 §3.2), so the first
            // render still starts out pending — the eligibility half of
            // INV-LC4 asserted below.
            (Self { state: 0, log }, Command::none().without_redraw())
        }

        fn update(&mut self, msg: Self::Message) -> Command<Self::Message> {
            self.state += 1;
            match msg {
                PhaseMessage::Bump => Command::none(),
                PhaseMessage::BumpWithoutRedraw => Command::none().without_redraw(),
            }
        }

        fn view(&self, _frame: &mut Frame<'_>) {
            self.log.push(Phase::View(self.state));
        }

        fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
            self.log.push(Phase::Subscriptions(self.state));
            vec![]
        }
    }

    fn phase_runtime(log: &PhaseLog) -> Runtime<PhaseApp> {
        Runtime::<PhaseApp>::new(log.clone(), frame_rate(60))
    }

    fn phase_counts(entries: &[Phase]) -> (usize, usize) {
        let views = entries
            .iter()
            .filter(|phase| matches!(phase, Phase::View(_)))
            .count();
        let reevaluations = entries
            .iter()
            .filter(|phase| matches!(phase, Phase::Subscriptions(_)))
            .count();
        (views, reevaluations)
    }

    // INV-LC1: rendering and subscription re-evaluation are frame-phase
    // activities — neither runs inside an input batch, however many messages
    // the batch processes. The clock is paused so all three messages land in
    // one batch.
    #[tokio::test(start_paused = true)]
    async fn input_batch_neither_renders_nor_reevaluates_subscriptions() {
        let log = PhaseLog::default();
        let mut runtime = phase_runtime(&log);

        for _ in 0..2 {
            runtime
                .core
                .msg_tx
                .try_send(PhaseMessage::Bump)
                .expect("receiver should be open");
        }
        runtime.process_message_batch(PhaseMessage::Bump);

        assert_eq!(
            runtime.core.app.state, 3,
            "the batch should process all three messages"
        );
        assert_eq!(
            log.entries(),
            Vec::<Phase>::new(),
            "an input batch must perform no render and no subscription re-evaluation"
        );
    }

    // INV-LC1: one frame pass consumes the pending work a batch recorded
    // exactly once — at most one render and at most one re-evaluation — and a
    // following pass with nothing pending performs neither.
    #[tokio::test(start_paused = true)]
    async fn frame_pass_renders_once_and_reevaluates_once_per_pending_batch() -> Result<()> {
        let log = PhaseLog::default();
        let mut runtime = phase_runtime(&log);
        let mut terminal = Terminal::new(TestBackend::new(80, 24))?;

        for _ in 0..2 {
            runtime
                .core
                .msg_tx
                .try_send(PhaseMessage::Bump)
                .expect("receiver should be open");
        }
        runtime.process_message_batch(PhaseMessage::Bump);
        runtime.process_frame_tick(&mut terminal)?;

        let after_pass = log.entries();
        assert_eq!(
            phase_counts(&after_pass),
            (1, 1),
            "one frame pass performs at most one render and one re-evaluation: {after_pass:?}"
        );

        runtime.process_frame_tick(&mut terminal)?;
        assert_eq!(
            log.entries(),
            after_pass,
            "a frame pass with no pending work performs neither step"
        );

        Ok(())
    }

    // INV-LC2: within one frame pass the render step precedes subscription
    // re-evaluation, and both steps observe the pass's current state — so the
    // subscriptions this pass starts are those of a state it has just rendered.
    #[tokio::test(start_paused = true)]
    async fn frame_pass_renders_before_reevaluating_and_both_observe_the_same_state() -> Result<()>
    {
        let log = PhaseLog::default();
        let mut runtime = phase_runtime(&log);
        let mut terminal = Terminal::new(TestBackend::new(80, 24))?;

        runtime.process_message_batch(PhaseMessage::Bump);
        runtime.process_frame_tick(&mut terminal)?;

        assert_eq!(
            log.entries(),
            vec![Phase::View(1), Phase::Subscriptions(1)],
            "the render step must precede re-evaluation, both on the pass's current state"
        );

        Ok(())
    }

    // INV-LC2: pending work does not queue per state, so a state that requested
    // a redraw is not itself promised a render. A redraw-requesting batch
    // followed by a `without_redraw` batch leaves the redraw pending; the pass
    // renders the newer state once and the intermediate state is never drawn.
    #[tokio::test(start_paused = true)]
    async fn frame_pass_renders_only_the_latest_state_after_a_superseding_batch() -> Result<()> {
        let log = PhaseLog::default();
        let mut runtime = phase_runtime(&log);
        let mut terminal = Terminal::new(TestBackend::new(80, 24))?;

        // Clear the bootstrap redraw so only the batches below decide whether a
        // redraw is pending at the pass.
        runtime.scheduler.pending.needs_redraw = false;

        runtime.process_message_batch(PhaseMessage::Bump);
        runtime.process_message_batch(PhaseMessage::BumpWithoutRedraw);
        assert!(
            runtime.scheduler.pending.needs_redraw,
            "the suppressing batch must not clear the redraw the earlier batch requested"
        );

        runtime.process_frame_tick(&mut terminal)?;

        assert_eq!(
            log.entries(),
            vec![Phase::View(2), Phase::Subscriptions(2)],
            "exactly one render, of the latest state; the superseded state is never drawn"
        );

        Ok(())
    }

    // INV-LC2: a pass entered with no redraw pending re-evaluates subscriptions
    // with no preceding render — suppression suppresses the redraw, never the
    // re-evaluation (RFC 0002 non-negotiable B, seen from the lifecycle side).
    #[tokio::test(start_paused = true)]
    async fn frame_pass_with_no_redraw_pending_still_reevaluates_subscriptions() -> Result<()> {
        let log = PhaseLog::default();
        let mut runtime = phase_runtime(&log);
        let mut terminal = Terminal::new(TestBackend::new(80, 24))?;

        runtime.scheduler.pending.needs_redraw = false;
        runtime.process_message_batch(PhaseMessage::BumpWithoutRedraw);
        assert!(
            !runtime.scheduler.pending.needs_redraw,
            "the suppressing batch must leave no redraw pending"
        );

        runtime.process_frame_tick(&mut terminal)?;

        assert_eq!(
            log.entries(),
            vec![Phase::Subscriptions(1)],
            "re-evaluation must still run, with no preceding render"
        );

        Ok(())
    }

    // INV-LC4 (eligibility half): the first render starts out pending, so a
    // freshly constructed runtime's first frame pass renders even though no
    // message has been processed — unconditionally, and independently of the
    // init command's redraw directive, which `PhaseApp` sets to
    // `without_redraw` and the runtime never consults (RFC 0011 §3.2). The
    // ordering half of INV-LC4 is structural: production exposes no stable
    // observable phase between the init dispatch and its effect's first poll.
    #[tokio::test(start_paused = true)]
    async fn first_frame_pass_renders_without_any_message_processed() -> Result<()> {
        let log = PhaseLog::default();
        let mut runtime = phase_runtime(&log);
        let mut terminal = Terminal::new(TestBackend::new(80, 24))?;

        runtime.process_frame_tick(&mut terminal)?;

        assert_eq!(
            log.entries(),
            vec![Phase::View(0)],
            "the first pass renders the initial state and re-evaluates nothing"
        );

        Ok(())
    }
}
