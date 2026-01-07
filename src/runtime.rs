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
//! 1. **Initialization**: Create a [`Runtime`] with initial flags and frame rate
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
//! - **Subscription Caching**: Subscriptions are only updated when their IDs change,
//!   using hash-based comparison for efficiency
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
//!     let runtime = Runtime::<CounterApp>::new((), 60);
//!     let mut terminal = ratatui::init();
//!     runtime.run(&mut terminal).await?;
//!     ratatui::restore();
//!     Ok(())
//! }
//! ```

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use color_eyre::eyre::Result;
use futures::stream::StreamExt;
use ratatui::prelude::Backend;
use tokio::sync::mpsc;
use tokio::time::{Interval, MissedTickBehavior, interval};

use crate::{
    application::Application,
    command::{Action, Command},
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
/// - **Subscription Caching**: Updates subscriptions only when their IDs change
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
    /// * `frame_rate` - Target frames per second (typical values: 30, 60, 120, 144)
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
    /// let runtime = Runtime::<MyApp>::new((), 60);
    /// ```
    pub fn new(flags: App::Flags, frame_rate: u32) -> Self {
        let state = ApplicationState::new(flags);

        let frame_duration = Duration::from_millis(1000 / u64::from(frame_rate));
        let mut frame_interval = interval(frame_duration);
        // Skip missed frames rather than trying to catch up
        frame_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        Self {
            state,
            frame_interval,
            needs_redraw: true,          // Initial draw is always needed
            subscription_ids_hash: None, // No subscriptions cached yet
        }
    }

    /// Processes a batch of messages that arrive in quick succession (micro-batching).
    ///
    /// Processes the first message immediately, then attempts to process additional messages
    /// that arrived within 100μs. This reduces overhead by batching rapid message sequences
    /// (e.g., keyboard input, rapid timers) while maintaining responsiveness.
    ///
    /// Always sets `needs_redraw` to true after processing.
    fn process_message_batch(&mut self, first_msg: App::Message) {
        // Process the first message
        let cmd = self.state.app.update(first_msg);
        self.state.enqueue_command(cmd);

        // Micro-batching: process additional messages that arrived during a short window
        let batch_deadline = Instant::now() + Duration::from_micros(100);
        while Instant::now() < batch_deadline {
            match self.state.msg_rx.try_recv() {
                Ok(msg) => {
                    let cmd = self.state.app.update(msg);
                    self.state.enqueue_command(cmd);
                }
                Err(_) => break, // No more messages available
            }
        }

        // Mark that a redraw is needed
        self.needs_redraw = true;
    }

    /// Processes a frame tick: renders if needed and updates subscriptions.
    ///
    /// Only renders when `needs_redraw` is true (conditional rendering optimization).
    /// Always updates subscriptions using hash-based caching to detect changes.
    ///
    /// # Returns
    ///
    /// `true` if a quit signal was received, `false` otherwise.
    fn process_frame_tick<B: Backend>(
        &mut self,
        terminal: &mut ratatui::Terminal<B>,
    ) -> Result<bool, <B as Backend>::Error> {
        // Render only if state has changed
        if self.needs_redraw {
            self.state.render(terminal)?;
            self.needs_redraw = false;
        }

        // Update subscriptions based on current state (dynamic subscriptions)
        // NOTE: Uses hash-based caching to skip updates when subscriptions
        // haven't changed, improving performance.
        self.update_subscriptions();

        // Check for quit signal (non-blocking)
        Ok(self.state.check_quit())
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
            self.state.subscription_manager.update(subscriptions);
            self.subscription_ids_hash = Some(current_hash);
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
    ///    and updates subscriptions at the specified frame rate.
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
    ///
    ///     let runtime = Runtime::<MyApp>::new((), 60);
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

        loop {
            tokio::select! {
                // Message received: batch process messages that arrive in quick succession
                Some(msg) = self.state.msg_rx.recv() => {
                    self.process_message_batch(msg);
                }

                // Frame tick: render if needed and update subscriptions
                _ = self.frame_interval.tick() => {
                    if self.process_frame_tick(terminal)? {
                        break;
                    }
                }

                // Quit signal received
                _ = self.state.quit_rx.recv() => {
                    break;
                }
            }
        }

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

        let runtime = Self {
            app,
            msg_tx,
            msg_rx,
            quit_tx,
            quit_rx,
            subscription_manager,
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
    /// Send failures are silently ignored as they only occur during application shutdown.
    fn enqueue_command(&self, cmd: Command<App::Message>) {
        if let Some(stream) = cmd.stream {
            let msg_tx = self.msg_tx.clone();
            let quit_tx = self.quit_tx.clone();

            tokio::spawn(async move {
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
            });
        }
    }

    /// Checks if a quit signal has been received (non-blocking).
    ///
    /// # Returns
    ///
    /// `true` if a quit signal is available, `false` otherwise.
    fn check_quit(&mut self) -> bool {
        self.quit_rx.try_recv().is_ok()
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
    /// Shuts down the subscription manager, which cancels all active subscriptions
    /// and cleans up their resources.
    fn shutdown(&mut self) {
        self.subscription_manager.shutdown();
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
        let runtime = ApplicationState::<TestApp>::new(0);

        // Enqueue a none command (should not panic)
        runtime.enqueue_command(Command::none());
    }

    #[tokio::test]
    async fn test_runtime_enqueue_command_with_message() {
        let runtime = ApplicationState::<TestApp>::new(0);

        // Enqueue a command that sends a message
        let cmd = Command::future(async { TestMessage::Increment });
        runtime.enqueue_command(cmd);

        // Give time for message to be sent
        sleep(Duration::from_millis(50)).await;

        // We can't directly verify the message was received without running the full event loop
    }

    #[tokio::test]
    async fn test_runtime_enqueue_command_with_quit() {
        let runtime = ApplicationState::<TestApp>::new(0);

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

    #[test]
    fn test_check_quit_no_signal() {
        let mut runtime = ApplicationState::<TestApp>::new(0);
        assert!(!runtime.check_quit());
    }

    #[test]
    fn test_check_quit_with_signal() {
        let mut runtime = ApplicationState::<TestApp>::new(0);

        // Send quit signal
        let _ = runtime.quit_tx.send(());

        assert!(runtime.check_quit());
    }

    #[test]
    fn test_check_quit_multiple_signals() {
        let mut runtime = ApplicationState::<TestApp>::new(0);

        // Send multiple quit signals
        let _ = runtime.quit_tx.send(());
        let _ = runtime.quit_tx.send(());

        // First check should return true
        assert!(runtime.check_quit());
        // Second check should also return true (signal still in queue)
        assert!(runtime.check_quit());
        // Third check should return false (no more signals)
        assert!(!runtime.check_quit());
    }

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
                vec![Subscription::new(Timer::new(100)).map(|_| ())]
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
                vec![Subscription::new(Timer::new(100)).map(|_| ())]
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
        let runtime = Runtime::<TestApp>::new(0, 60);

        // Runtime should be created successfully
        assert_eq!(runtime.state.app.counter, 0);
    }

    #[tokio::test]
    async fn test_event_loop_new_with_different_frame_rates() {
        let _runtime1 = Runtime::<TestApp>::new(0, 30);
        let _runtime2 = Runtime::<TestApp>::new(0, 144);

        // Should handle different frame rates without panic
    }

    #[tokio::test]
    async fn test_event_loop_process_message_batch_single_message() {
        let mut runtime = Runtime::<TestApp>::new(0, 60);

        // Initially no redraw needed (well, actually true for initial state)
        // Process a message
        runtime.process_message_batch(TestMessage::Increment);

        // Counter should be incremented
        assert_eq!(runtime.state.app.counter, 1);

        // Redraw should be needed
        assert!(runtime.needs_redraw);
    }

    #[tokio::test]
    async fn test_event_loop_process_message_batch_with_batching() {
        let mut runtime = Runtime::<TestApp>::new(0, 60);

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
        let mut runtime = Runtime::<TestApp>::new(0, 60);

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend)?;

        // Initially needs_redraw is true
        assert!(runtime.needs_redraw);

        // Process frame tick
        let should_quit = runtime.process_frame_tick(&mut terminal)?;

        // Should not quit
        assert!(!should_quit);

        // Redraw flag should be cleared
        assert!(!runtime.needs_redraw);

        Ok(())
    }

    #[tokio::test]
    async fn test_event_loop_process_frame_tick_skips_render_when_not_needed() -> Result<()> {
        let mut runtime = Runtime::<TestApp>::new(0, 60);

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend)?;

        // Clear the needs_redraw flag
        runtime.needs_redraw = false;

        // Process frame tick
        let should_quit = runtime.process_frame_tick(&mut terminal)?;

        // Should not quit
        assert!(!should_quit);

        // Redraw flag should still be false
        assert!(!runtime.needs_redraw);

        Ok(())
    }

    #[tokio::test]
    async fn test_event_loop_process_frame_tick_detects_quit() -> Result<()> {
        let mut runtime = Runtime::<TestApp>::new(0, 60);

        // Send quit signal
        let _ = runtime.state.quit_tx.send(());

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend)?;

        // Clear needs_redraw to test quit detection
        runtime.needs_redraw = false;

        // Process frame tick
        let should_quit = runtime.process_frame_tick(&mut terminal)?;

        // Should detect quit
        assert!(should_quit);

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
                    vec![Subscription::new(Timer::new(100)).map(|_| ())]
                } else {
                    vec![]
                }
            }
        }

        let mut runtime = Runtime::<DynamicApp>::new(true, 60);

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend)?;

        // Clear needs_redraw
        runtime.needs_redraw = false;

        // Initially no hash
        assert_eq!(runtime.subscription_ids_hash, None);

        // Process frame tick (should update subscriptions)
        runtime.process_frame_tick(&mut terminal)?;

        // Hash should be set
        assert!(runtime.subscription_ids_hash.is_some());

        Ok(())
    }
}
