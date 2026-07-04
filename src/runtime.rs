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
//! - **Subscription Caching**: Subscriptions are only re-evaluated after a message is
//!   processed (idle frames are skipped), and the subscription manager is only updated
//!   when their IDs change, using hash-based comparison for efficiency
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

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::panic::AssertUnwindSafe;
use std::time::{Duration, Instant};

use color_eyre::eyre::Result;
use futures::FutureExt;
use futures::stream::StreamExt;
use ratatui::prelude::Backend;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::{Interval, MissedTickBehavior, interval};

use crate::{
    application::Application,
    command::{Action, Command},
    frame_rate::{FrameRate, FrameRateError},
    subscription::SubscriptionManager,
};

/// Application state that manages the application lifecycle.
///
/// Internal type that orchestrates TUI application execution following The Elm Architecture.
/// It manages the application instance, routes messages, executes commands asynchronously,
/// and coordinates subscriptions through [`SubscriptionManager`].
///
/// Applications should use [`Runtime`] directly instead of this type.
///
/// # Type Parameters
///
/// * `App` - The application type implementing [`Application`]
struct ApplicationState<App: Application> {
    /// The application instance
    app: App,
    /// Sender for application messages
    msg_tx: mpsc::UnboundedSender<App::Message>,
    /// Receiver for application messages
    msg_rx: mpsc::UnboundedReceiver<App::Message>,
    /// Sender for quit signals
    quit_tx: mpsc::UnboundedSender<()>,
    /// Receiver for quit signals
    quit_rx: mpsc::UnboundedReceiver<()>,
    /// Manages subscription lifecycle
    subscription_manager: SubscriptionManager<App::Message>,
    /// Running command tasks.
    ///
    /// Kept so command tasks can be aborted on shutdown, and — because a
    /// [`JoinSet`] aborts its tasks when dropped — also when the runtime is
    /// dropped while unwinding from a panic. Finished tasks are reaped on each
    /// enqueue so the set does not grow without bound.
    command_tasks: JoinSet<()>,
}

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
/// - **Subscription Caching**: Re-evaluates subscriptions only after a message is processed,
///   and updates the subscription manager only when their IDs change
///
/// # Type Parameters
///
/// * `App` - The application type implementing [`Application`]
///
/// # Examples
///
/// See the [module-level documentation](self) for a complete example.
pub struct Runtime<App: Application> {
    /// Application state and message routing
    state: ApplicationState<App>,
    /// Timer for frame rate regulation
    frame_interval: Interval,
    /// Whether the UI needs to be re-rendered (conditional rendering optimization)
    needs_redraw: bool,
    /// Whether subscriptions may have changed and should be re-evaluated.
    ///
    /// `subscriptions()` is a pure function of the application state, which only
    /// changes when a message is processed. This flag is set after each message
    /// batch so that subscriptions are re-evaluated only when the state may have
    /// changed, rather than on every frame.
    subscriptions_dirty: bool,
    /// Cached hash of subscription IDs from previous frame (subscription caching optimization)
    subscription_ids_hash: Option<u64>,
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
        let state = ApplicationState::new(flags);

        // Divide a one-second `Duration` directly so the period is exact (e.g.
        // 60 FPS -> 16.667ms) instead of truncating to whole milliseconds (the
        // old `1000 / frame_rate` gave 16ms, an effective ~62.5 FPS).
        let frame_duration = frame_rate.frame_duration();
        let mut frame_interval = interval(frame_duration);
        // Skip missed frames rather than trying to catch up
        frame_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        Self {
            state,
            frame_interval,
            needs_redraw: true, // Initial draw is always needed
            // Initial subscriptions are started by `initialize_subscriptions`,
            // so no re-evaluation is needed until a message is processed.
            subscriptions_dirty: false,
            subscription_ids_hash: None, // No subscriptions cached yet
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

    /// Processes a batch of messages that arrive in quick succession (micro-batching).
    ///
    /// Processes the first message immediately, then attempts to process additional messages
    /// that arrived within 100μs. This reduces overhead by batching rapid message sequences
    /// (e.g., keyboard input, rapid timers) while maintaining responsiveness.
    ///
    /// Sets `needs_redraw` if any processed command requests a redraw, and
    /// always sets `subscriptions_dirty` after processing.
    fn process_message_batch(&mut self, first_msg: App::Message) {
        // Process the first message
        let cmd = self.state.app.update(first_msg);
        self.dispatch_update_command(cmd);
        let mut processed = 1usize;

        // Micro-batching: process additional messages that arrived during a short window
        let batch_deadline = Instant::now() + Duration::from_micros(100);
        while Instant::now() < batch_deadline {
            match self.state.msg_rx.try_recv() {
                Ok(msg) => {
                    let cmd = self.state.app.update(msg);
                    self.dispatch_update_command(cmd);
                    processed += 1;
                }
                Err(_) => break, // No more messages available
            }
        }

        tracing::trace!(target: "tears::runtime", messages = processed, "processed message batch");

        // Mark that subscriptions may have changed even when redraw is suppressed.
        self.subscriptions_dirty = true;
    }

    fn dispatch_update_command(&mut self, cmd: Command<App::Message>) {
        self.needs_redraw |= cmd.requests_redraw();
        self.state.enqueue_command(cmd);
    }

    /// Whether the frame tick has pending work (a redraw or a subscription
    /// re-evaluation) to do.
    ///
    /// Used to gate the frame branch of the event loop's `select!`: when the
    /// application is idle (no message has been processed since the last frame),
    /// both flags are false and there is nothing for a frame tick to do. Skipping
    /// the tick entirely means the loop no longer wakes up at the frame rate while
    /// idle, turning the loop event-driven and saving CPU/battery.
    ///
    /// Must use `||` (not `&&`): the initial state has `needs_redraw == true` and
    /// `subscriptions_dirty == false`, and the first frame must still render.
    const fn should_process_frame(&self) -> bool {
        self.needs_redraw || self.subscriptions_dirty
    }

    /// Processes a frame tick: renders if needed and updates subscriptions.
    ///
    /// Only renders when `needs_redraw` is true (conditional rendering optimization).
    /// Only re-evaluates subscriptions when `subscriptions_dirty` is true, i.e. when
    /// a message has been processed since the last evaluation.
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
        if self.needs_redraw {
            self.state.render(terminal)?;
            self.needs_redraw = false;
            tracing::trace!(target: "tears::runtime", "frame rendered");
        }

        // Re-evaluate subscriptions only when the state may have changed.
        // Since `subscriptions()` is a pure function of the application state,
        // an idle frame (no messages processed) cannot change the subscription
        // set, so we skip the (potentially costly) evaluation entirely.
        if self.subscriptions_dirty {
            // NOTE: Uses hash-based caching to skip the manager update when the
            // subscription IDs haven't actually changed.
            self.update_subscriptions();
            self.subscriptions_dirty = false;
        }

        Ok(())
    }

    /// Updates subscriptions if they have changed (uses hash-based caching).
    ///
    /// Computes a hash of subscription IDs and only calls [`SubscriptionManager::update`]
    /// if the hash differs from the previous frame. This optimization avoids unnecessary
    /// subscription updates when the application's subscription set hasn't changed.
    fn update_subscriptions(&mut self) {
        let subscriptions = self.state.app.subscriptions();

        // Compute hash of subscription IDs
        let mut hasher = DefaultHasher::new();
        for sub in &subscriptions {
            sub.id.hash(&mut hasher);
        }
        let current_hash = hasher.finish();

        // Only update if subscriptions have changed
        if self.subscription_ids_hash != Some(current_hash) {
            let count = subscriptions.len();
            self.state.subscription_manager.update(subscriptions);
            self.subscription_ids_hash = Some(current_hash);
            tracing::debug!(target: "tears::runtime", count, "subscriptions updated");
        }
    }

    /// Runs the runtime until the application quits.
    ///
    /// This is the main entry point for executing the application. It starts the event loop
    /// that processes messages, renders the UI, and manages subscriptions. The loop continues
    /// until the application sends a quit signal via [`Action::Quit`].
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
        self.state.initialize_subscriptions();
        tracing::debug!(target: "tears::runtime", "runtime started");

        loop {
            tokio::select! {
                // Message received: batch process messages that arrive in quick succession
                Some(msg) = self.state.msg_rx.recv() => {
                    self.process_message_batch(msg);
                }

                // Frame tick: render if needed and update subscriptions.
                //
                // Gated on pending work so an idle loop does not wake at the frame
                // rate just to do nothing. When a message re-enables the branch, the
                // timer deadline has already elapsed, so `tick()` is ready on the
                // next poll and adds no render latency. `MissedTickBehavior::Skip`
                // only controls where the following tick lands (no catch-up burst).
                _ = self.frame_interval.tick(), if self.should_process_frame() => {
                    self.process_frame_tick(terminal)?;
                }

                // Quit signal received
                _ = self.state.quit_rx.recv() => {
                    tracing::debug!(target: "tears::runtime", "quit signal received");
                    break;
                }
            }
        }

        tracing::debug!(target: "tears::runtime", "runtime shutting down");
        self.state.shutdown();

        Ok(())
    }
}

impl<App: Application> ApplicationState<App> {
    /// Creates a new application state with the given initialization flags.
    ///
    /// Initializes the application by calling [`Application::new`] and automatically
    /// enqueues any returned initialization commands for execution.
    ///
    /// # Arguments
    ///
    /// * `flags` - Configuration data passed to [`Application::new`]
    #[must_use]
    fn new(flags: App::Flags) -> Self {
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();
        let (quit_tx, quit_rx) = mpsc::unbounded_channel();
        let subscription_manager = SubscriptionManager::new(msg_tx.clone());

        // Initialize the application with flags
        let (app, init_cmd) = App::new(flags);

        let mut runtime = Self {
            app,
            msg_tx,
            msg_rx,
            quit_tx,
            quit_rx,
            subscription_manager,
            command_tasks: JoinSet::new(),
        };

        // Enqueue the initial command
        runtime.enqueue_command(init_cmd);

        runtime
    }

    /// Enqueues a command for asynchronous execution.
    ///
    /// Spawns a tokio task that executes the command's action stream. Messages are sent
    /// to the message channel, and quit signals are sent to the quit channel. The task
    /// terminates when the stream completes or a quit action is received.
    ///
    /// The task is tracked in [`command_tasks`](Self::command_tasks) so it can be aborted
    /// on shutdown (or when the runtime is dropped). If the command's stream panics, the
    /// panic is caught and logged rather than silently lost.
    ///
    /// Send failures are silently ignored as they only occur during application shutdown.
    fn enqueue_command(&mut self, cmd: Command<App::Message>) {
        if let Some(stream) = cmd.stream {
            let msg_tx = self.msg_tx.clone();
            let quit_tx = self.quit_tx.clone();

            // Reap finished command tasks so the set stays bounded to the
            // commands that are actually still running.
            while self.command_tasks.try_join_next().is_some() {}

            tracing::trace!(target: "tears::runtime", "command spawned");

            self.command_tasks.spawn(async move {
                // Catch panics in the command's stream so a bug in a fetcher or
                // effect is logged instead of vanishing into a detached task.
                let result = AssertUnwindSafe(async move {
                    futures::pin_mut!(stream);
                    while let Some(action) = stream.next().await {
                        match action {
                            Action::Message(msg) => {
                                // NOTE: Send errors are silently ignored. The channel is closed only
                                // when the ApplicationState is dropped, which means the application is shutting
                                // down. In this case, dropping messages is the expected behavior.
                                // This follows the same approach as iced and other Elm-like frameworks.
                                let _ = msg_tx.send(msg);
                            }
                            Action::Quit => {
                                // NOTE: Same reasoning as above. If the quit channel is closed,
                                // the application is already shutting down.
                                let _ = quit_tx.send(());
                                break;
                            }
                        }
                    }
                })
                .catch_unwind()
                .await;

                if result.is_err() {
                    tracing::error!(target: "tears::runtime", "command task panicked");
                }
            });
        }
    }

    /// Initializes subscriptions from the application.
    ///
    /// Called once before the event loop starts. Gets the initial subscription set
    /// from [`Application::subscriptions`] and registers them with the subscription manager.
    fn initialize_subscriptions(&mut self) {
        let subscriptions = self.app.subscriptions();
        self.subscription_manager.update(subscriptions);
    }

    /// Renders the application to the terminal.
    ///
    /// Calls [`Application::view`] within a ratatui draw context to render the UI.
    ///
    /// # Errors
    ///
    /// Returns an error if the terminal backend fails (e.g., I/O error).
    fn render<B: Backend>(
        &self,
        terminal: &mut ratatui::Terminal<B>,
    ) -> Result<(), <B as Backend>::Error> {
        terminal.draw(|frame| {
            self.app.view(frame);
        })?;
        Ok(())
    }

    /// Cleans up resources on shutdown.
    ///
    /// Shuts down the subscription manager, which cancels all active subscriptions,
    /// and aborts any still-running command tasks.
    fn shutdown(&mut self) {
        self.subscription_manager.shutdown();
        self.command_tasks.abort_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::Application;
    use crate::command::{Action, Command};
    use crate::subscription::Subscription;
    use crate::subscription::time::Timer;
    use ratatui::backend::TestBackend;
    use ratatui::prelude::*;
    use tokio::time::{Duration, sleep};

    fn frame_rate(value: u32) -> FrameRate {
        FrameRate::try_new(value).expect("frame rate must be valid")
    }

    // Simple test application
    #[derive(Debug)]
    struct TestApp {
        counter: i32,
        should_quit: bool,
    }

    #[derive(Debug, Clone)]
    #[allow(dead_code)]
    enum TestMessage {
        Increment,
        Quit,
    }

    impl Application for TestApp {
        type Message = TestMessage;
        type Flags = i32;

        fn new(initial: i32) -> (Self, Command<Self::Message>) {
            (
                Self {
                    counter: initial,
                    should_quit: false,
                },
                Command::none(),
            )
        }

        fn update(&mut self, msg: Self::Message) -> Command<Self::Message> {
            match msg {
                TestMessage::Increment => {
                    self.counter += 1;
                    Command::none()
                }
                TestMessage::Quit => {
                    self.should_quit = true;
                    Command::effect(Action::Quit)
                }
            }
        }

        fn view(&self, _frame: &mut Frame<'_>) {
            // No-op for testing
        }

        fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
            vec![]
        }
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

    #[test]
    fn test_runtime_new() {
        let runtime = ApplicationState::<TestApp>::new(42);
        assert_eq!(runtime.app.counter, 42);
    }

    #[test]
    fn test_runtime_new_with_zero() {
        let runtime = ApplicationState::<TestApp>::new(0);
        assert_eq!(runtime.app.counter, 0);
    }

    #[test]
    fn test_runtime_new_initializes_channels() {
        let runtime = ApplicationState::<TestApp>::new(0);

        // ApplicationState should have channels set up
        // We can't directly test private fields, but we can verify the runtime was created
        assert_eq!(runtime.app.counter, 0);
    }

    // Test application with init command
    struct AppWithInitCommand {
        initialized: bool,
    }

    impl Application for AppWithInitCommand {
        type Message = bool;
        type Flags = ();

        fn new(_flags: ()) -> (Self, Command<Self::Message>) {
            let cmd = Command::future(async { true });
            (Self { initialized: false }, cmd)
        }

        fn update(&mut self, msg: Self::Message) -> Command<Self::Message> {
            self.initialized = msg;
            Command::none()
        }

        fn view(&self, _frame: &mut Frame<'_>) {}

        fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
            vec![]
        }
    }

    #[tokio::test]
    async fn test_runtime_processes_init_command() {
        let runtime = ApplicationState::<AppWithInitCommand>::new(());

        // Give time for init command to be processed
        sleep(Duration::from_millis(50)).await;

        // The application state should have enqueued the init command
        // We can't directly verify it was processed without running the event loop
        // but we can verify the runtime was created successfully
        assert!(!runtime.app.initialized);
    }

    #[test]
    fn test_runtime_enqueue_command_none() {
        let mut runtime = ApplicationState::<TestApp>::new(0);

        // Enqueue a none command (should not panic)
        runtime.enqueue_command(Command::none());
    }

    #[tokio::test]
    async fn test_runtime_enqueue_command_with_message() {
        let mut runtime = ApplicationState::<TestApp>::new(0);

        // Enqueue a command that sends a message
        let cmd = Command::future(async { TestMessage::Increment });
        runtime.enqueue_command(cmd);

        // Give time for message to be sent
        sleep(Duration::from_millis(50)).await;

        // We can't directly verify the message was received without running the full event loop
    }

    #[tokio::test]
    async fn test_runtime_enqueue_command_with_quit() {
        let mut runtime = ApplicationState::<TestApp>::new(0);

        // Enqueue a quit command
        let cmd = Command::effect(Action::Quit);
        runtime.enqueue_command(cmd);

        // Give time for quit signal to be sent
        sleep(Duration::from_millis(50)).await;

        // The quit signal should have been sent to the quit channel
    }

    // Test multiple application states can be created
    #[test]
    fn test_multiple_runtimes() {
        let runtime1 = ApplicationState::<TestApp>::new(1);
        let runtime2 = ApplicationState::<TestApp>::new(2);

        assert_eq!(runtime1.app.counter, 1);
        assert_eq!(runtime2.app.counter, 2);
    }

    // Test with different flag types
    struct AppWithStringFlags {
        name: String,
    }

    impl Application for AppWithStringFlags {
        type Message = ();
        type Flags = String;

        fn new(name: String) -> (Self, Command<Self::Message>) {
            (Self { name }, Command::none())
        }

        fn update(&mut self, _msg: Self::Message) -> Command<Self::Message> {
            Command::none()
        }

        fn view(&self, _frame: &mut Frame<'_>) {}

        fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
            vec![]
        }
    }

    #[test]
    fn test_runtime_with_string_flags() {
        let runtime = ApplicationState::<AppWithStringFlags>::new("test".to_string());
        assert_eq!(runtime.app.name, "test");
    }

    #[test]
    fn test_runtime_with_empty_string_flags() {
        let runtime = ApplicationState::<AppWithStringFlags>::new(String::new());
        assert_eq!(runtime.app.name, "");
    }

    // Unit tests for extracted methods

    #[tokio::test]
    async fn test_initialize_subscriptions() {
        struct AppWithSubs;

        impl Application for AppWithSubs {
            type Message = ();
            type Flags = ();

            fn new((): ()) -> (Self, Command<()>) {
                (Self, Command::none())
            }

            fn update(&mut self, (): ()) -> Command<()> {
                Command::none()
            }

            fn view(&self, _frame: &mut Frame<'_>) {}

            fn subscriptions(&self) -> Vec<Subscription<()>> {
                vec![
                    Subscription::new(
                        Timer::try_new(100).expect("timer interval must be non-zero"),
                    )
                    .map(|_| ()),
                ]
            }
        }

        let mut runtime = ApplicationState::<AppWithSubs>::new(());

        // Should not panic
        runtime.initialize_subscriptions();
    }

    #[test]
    fn test_initialize_subscriptions_empty() {
        let mut runtime = ApplicationState::<TestApp>::new(0);

        // Should not panic with empty subscriptions
        runtime.initialize_subscriptions();
    }

    #[test]
    fn test_render() -> Result<()> {
        let runtime = ApplicationState::<TestApp>::new(0);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend)?;

        // Should render without error
        assert!(runtime.render(&mut terminal).is_ok());

        Ok(())
    }

    #[test]
    fn test_render_multiple_times() -> Result<()> {
        let runtime = ApplicationState::<TestApp>::new(0);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend)?;

        // Should be able to render multiple times
        assert!(runtime.render(&mut terminal).is_ok());
        assert!(runtime.render(&mut terminal).is_ok());
        assert!(runtime.render(&mut terminal).is_ok());

        Ok(())
    }

    #[test]
    fn test_shutdown() {
        let mut runtime = ApplicationState::<TestApp>::new(0);

        // Should not panic
        runtime.shutdown();
    }

    #[tokio::test]
    async fn test_shutdown_after_initialize_subscriptions() {
        struct AppWithSubs;

        impl Application for AppWithSubs {
            type Message = ();
            type Flags = ();

            fn new((): ()) -> (Self, Command<()>) {
                (Self, Command::none())
            }

            fn update(&mut self, (): ()) -> Command<()> {
                Command::none()
            }

            fn view(&self, _frame: &mut Frame<'_>) {}

            fn subscriptions(&self) -> Vec<Subscription<()>> {
                use crate::subscription::time::Timer;
                vec![
                    Subscription::new(
                        Timer::try_new(100).expect("timer interval must be non-zero"),
                    )
                    .map(|_| ()),
                ]
            }
        }

        let mut runtime = ApplicationState::<AppWithSubs>::new(());
        runtime.initialize_subscriptions();

        // Should cancel subscriptions without panic
        runtime.shutdown();
    }

    // Runtime tests

    #[tokio::test]
    async fn test_event_loop_new() {
        let runtime = Runtime::<TestApp>::new(0, frame_rate(60));

        // Runtime should be created successfully
        assert_eq!(runtime.state.app.counter, 0);
    }

    #[tokio::test]
    async fn test_event_loop_new_with_different_frame_rates() {
        let _runtime1 = Runtime::<TestApp>::new(0, frame_rate(30));
        let _runtime2 = Runtime::<TestApp>::new(0, frame_rate(144));

        // Should handle different frame rates without panic
    }

    #[tokio::test]
    async fn test_runtime_frame_interval_period_is_accurate() {
        // 60 FPS should yield a ~16.667ms period. The previous integer
        // millisecond division (1000 / 60 = 16ms) truncated this, producing
        // an effective ~62.5 FPS.
        let runtime = Runtime::<TestApp>::new(0, frame_rate(60));
        let period = runtime.frame_interval.period();
        assert!(
            period >= Duration::from_micros(16_600) && period <= Duration::from_micros(16_700),
            "60 FPS period should be ~16.667ms, got {period:?}",
        );

        // 144 FPS should yield a ~6.944ms period (not the truncated 6ms).
        let runtime = Runtime::<TestApp>::new(0, frame_rate(144));
        let period = runtime.frame_interval.period();
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
        assert_eq!(runtime.state.app.counter, 1);

        // Redraw should be needed
        assert!(runtime.needs_redraw);
    }

    #[tokio::test]
    async fn test_process_message_batch_without_redraw_leaves_redraw_unchanged() {
        let mut runtime = Runtime::<RedrawControlApp>::new((), frame_rate(60));
        runtime.needs_redraw = false;
        runtime.subscriptions_dirty = false;

        runtime.process_message_batch(RedrawControlMessage::Skip);

        assert!(!runtime.needs_redraw);
        assert!(
            runtime.subscriptions_dirty,
            "redraw suppression must not suppress subscription re-evaluation"
        );
    }

    #[tokio::test]
    async fn test_process_message_batch_mixed_commands_redraws() {
        let mut runtime = Runtime::<RedrawControlApp>::new((), frame_rate(60));
        runtime.needs_redraw = false;
        runtime.subscriptions_dirty = false;

        let _ = runtime.state.msg_tx.send(RedrawControlMessage::Redraw);
        runtime.process_message_batch(RedrawControlMessage::Skip);

        assert!(runtime.needs_redraw);
        assert!(runtime.subscriptions_dirty);
    }

    #[tokio::test]
    async fn test_process_message_batch_redraw_recovers_after_suppression() {
        let mut runtime = Runtime::<RedrawControlApp>::new((), frame_rate(60));
        runtime.needs_redraw = false;

        runtime.process_message_batch(RedrawControlMessage::Skip);
        assert!(!runtime.needs_redraw);

        runtime.subscriptions_dirty = false;
        runtime.process_message_batch(RedrawControlMessage::Redraw);

        assert!(runtime.needs_redraw);
        assert!(runtime.subscriptions_dirty);
    }

    // A minimal `tracing::Subscriber` that counts emitted events (total, and
    // those at ERROR level), used to assert that instrumentation actually fires.
    #[derive(Clone, Default)]
    struct EventCounter {
        events: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        errors: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl tracing::Subscriber for EventCounter {
        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            use std::sync::atomic::Ordering;
            self.events.fetch_add(1, Ordering::SeqCst);
            if *event.metadata().level() == tracing::Level::ERROR {
                self.errors.fetch_add(1, Ordering::SeqCst);
            }
        }
        fn enter(&self, _span: &tracing::span::Id) {}
        fn exit(&self, _span: &tracing::span::Id) {}
    }

    #[tokio::test]
    async fn test_process_message_batch_emits_tracing_event() {
        use std::sync::atomic::Ordering;

        let counter = EventCounter::default();
        let events = counter.events.clone();

        tracing::subscriber::with_default(counter, || {
            let mut runtime = Runtime::<TestApp>::new(0, frame_rate(60));
            runtime.process_message_batch(TestMessage::Increment);
        });

        assert!(
            events.load(Ordering::SeqCst) >= 1,
            "processing a message batch should emit a tracing event"
        );
    }

    #[test]
    #[allow(
        clippy::panic,
        reason = "the test intentionally panics inside a command"
    )]
    fn test_command_task_panic_is_logged() {
        use std::sync::atomic::Ordering;

        // Serialize against other tests that observe the global panic hook, so
        // this test's caught panic cannot pollute their recorded hook activity.
        let _hook_guard = crate::test_support::PANIC_HOOK_GUARD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let counter = EventCounter::default();
        let errors = counter.errors.clone();

        // Drive the panicking command task to completion on the current thread,
        // inside the subscriber scope, so its `catch_unwind` error event is seen.
        //
        // The panic is caught by `catch_unwind`, so the default hook only prints
        // the panic message to stderr — harmless test noise. We deliberately do
        // not swap the panic hook here: it is process-global, so a no-op hook
        // would suppress panics in other tests running in parallel and could
        // leak if this test panicked before restoring it.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime should build");

        tracing::subscriber::with_default(counter, || {
            rt.block_on(async {
                let mut state = ApplicationState::<TestApp>::new(0);
                state.enqueue_command(Command::future(async {
                    panic!("boom");
                    #[allow(unreachable_code)]
                    TestMessage::Increment
                }));
                // Run the command task to completion (it panics and is caught).
                state.command_tasks.join_next().await;
            });
        });

        assert!(
            errors.load(Ordering::SeqCst) >= 1,
            "a panicking command task should log an error event"
        );
    }

    #[tokio::test]
    async fn test_dropping_state_aborts_command_tasks() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        // A guard that records, via its `Drop`, that the command task's future
        // was dropped — which only happens if the task is aborted rather than
        // detached.
        struct AbortGuard(Arc<AtomicBool>);
        impl Drop for AbortGuard {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let aborted = Arc::new(AtomicBool::new(false));

        {
            let mut state = ApplicationState::<TestApp>::new(0);
            let guard = AbortGuard(aborted.clone());
            // A command that owns the guard and never completes, so its task
            // stays parked until aborted.
            state.enqueue_command(Command::future(async move {
                let _guard = guard;
                std::future::pending::<TestMessage>().await
            }));

            // Let the task start and park.
            sleep(Duration::from_millis(10)).await;
            assert!(
                !aborted.load(Ordering::SeqCst),
                "command task should still be running before the state is dropped"
            );
            // `state` (and its `command_tasks` JoinSet) is dropped here.
        }

        // Give the runtime a chance to process the abort and drop the future.
        sleep(Duration::from_millis(10)).await;
        assert!(
            aborted.load(Ordering::SeqCst),
            "dropping the state should abort running command tasks"
        );
    }

    #[tokio::test]
    async fn test_event_loop_process_message_batch_with_batching() {
        let mut runtime = Runtime::<TestApp>::new(0, frame_rate(60));

        // Send multiple messages to the queue
        let _ = runtime.state.msg_tx.send(TestMessage::Increment);
        let _ = runtime.state.msg_tx.send(TestMessage::Increment);
        let _ = runtime.state.msg_tx.send(TestMessage::Increment);

        // Give messages time to arrive
        sleep(Duration::from_millis(10)).await;

        // Process first message (should batch the others within the deadline)
        runtime.process_message_batch(TestMessage::Increment);

        // All messages should be processed (1 direct + 3 batched = 4 total)
        assert_eq!(runtime.state.app.counter, 4);
    }

    #[tokio::test]
    async fn test_event_loop_process_frame_tick_renders_when_needed() -> Result<()> {
        let mut runtime = Runtime::<TestApp>::new(0, frame_rate(60));

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend)?;

        // Initially needs_redraw is true
        assert!(runtime.needs_redraw);

        // Process frame tick
        runtime.process_frame_tick(&mut terminal)?;

        // Redraw flag should be cleared
        assert!(!runtime.needs_redraw);

        Ok(())
    }

    #[tokio::test]
    async fn test_event_loop_process_frame_tick_skips_render_when_not_needed() -> Result<()> {
        let mut runtime = Runtime::<TestApp>::new(0, frame_rate(60));

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend)?;

        // Clear the needs_redraw flag
        runtime.needs_redraw = false;

        // Process frame tick
        runtime.process_frame_tick(&mut terminal)?;

        // Redraw flag should still be false
        assert!(!runtime.needs_redraw);

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
        let _ = runtime.state.msg_tx.send(TestMessage::Quit);

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
    // has rendered and the frame branch is gated off by `should_process_frame()`.
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
        let quit_tx = runtime.state.quit_tx.clone();
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
    // The event loop gates its frame branch on `should_process_frame()` so an
    // idle loop stops waking at the frame rate. These unit tests cover the gate
    // predicate in isolation; the end-to-end behavior (zero idle wake-ups, and
    // an immediate render once a message re-enables the branch) is covered by
    // `tests/idle_wakeup.rs`.

    #[tokio::test]
    async fn test_should_process_frame_initial_state_allows_first_frame() {
        // Fresh runtime: `needs_redraw == true`, so the first frame must render.
        let runtime = Runtime::<TestApp>::new(0, frame_rate(60));
        assert!(runtime.should_process_frame());
    }

    #[tokio::test]
    async fn test_should_process_frame_is_gated_off_when_idle() -> Result<()> {
        let mut runtime = Runtime::<TestApp>::new(0, frame_rate(60));
        let mut terminal = Terminal::new(TestBackend::new(80, 24))?;

        // Draining the initial pending work leaves both flags false: idle.
        runtime.process_frame_tick(&mut terminal)?;
        assert!(!runtime.should_process_frame());

        Ok(())
    }

    #[tokio::test]
    async fn test_should_process_frame_reenabled_after_message() -> Result<()> {
        let mut runtime = Runtime::<TestApp>::new(0, frame_rate(60));
        let mut terminal = Terminal::new(TestBackend::new(80, 24))?;

        runtime.process_frame_tick(&mut terminal)?;
        assert!(!runtime.should_process_frame());

        // A processed message re-enables the frame branch.
        runtime.process_message_batch(TestMessage::Increment);
        assert!(runtime.should_process_frame());

        Ok(())
    }

    #[tokio::test]
    async fn test_event_loop_process_frame_tick_updates_subscriptions() -> Result<()> {
        // App with dynamic subscriptions
        struct DynamicApp {
            enabled: bool,
        }

        impl Application for DynamicApp {
            type Message = ();
            type Flags = bool;

            fn new(enabled: bool) -> (Self, Command<Self::Message>) {
                (Self { enabled }, Command::none())
            }

            fn update(&mut self, (): ()) -> Command<Self::Message> {
                Command::none()
            }

            fn view(&self, _frame: &mut Frame<'_>) {}

            fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
                if self.enabled {
                    vec![
                        Subscription::new(
                            Timer::try_new(100).expect("timer interval must be non-zero"),
                        )
                        .map(|_| ()),
                    ]
                } else {
                    vec![]
                }
            }
        }

        let mut runtime = Runtime::<DynamicApp>::new(true, frame_rate(60));

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend)?;

        // Clear needs_redraw
        runtime.needs_redraw = false;

        // Initially no hash
        assert_eq!(runtime.subscription_ids_hash, None);

        // Subscriptions are only re-evaluated when marked dirty (after a
        // message). Simulate that here.
        runtime.subscriptions_dirty = true;

        // Process frame tick (should update subscriptions)
        runtime.process_frame_tick(&mut terminal)?;

        // Hash should be set
        assert!(runtime.subscription_ids_hash.is_some());

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
}
