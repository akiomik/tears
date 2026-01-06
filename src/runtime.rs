//! Runtime for executing TUI applications.
//!
//! The [`Runtime`] manages the application lifecycle based on The Elm Architecture,
//! coordinating message processing, command execution, subscription management, and rendering.
//!
//! See [`Runtime`] for detailed documentation and examples.

use std::time::Duration;

use color_eyre::eyre::Result;
use futures::stream::StreamExt;
use ratatui::prelude::Backend;
use tokio::sync::mpsc;
use tokio::time::{MissedTickBehavior, interval};

use crate::{
    application::Application,
    command::{Action, Command},
    subscription::SubscriptionManager,
};

/// The runtime manages the application lifecycle and event loop.
///
/// # Responsibility: Scheduling ("When to Execute")
///
/// The Runtime's core responsibility is **scheduling** - controlling **when** operations
/// are executed. This is distinct from other components:
///
/// - **`Application`**: Declares **what** to display/subscribe (declarative)
/// - **`Runtime`**: Controls **when** to execute (scheduling, optimization)
/// - **`SubscriptionManager`**: Handles **how** subscriptions work (implementation)
///
/// The Runtime uses several optimizations to improve performance:
/// - Skip rendering when UI hasn't changed (`needs_redraw`)
/// - Skip subscription updates when unchanged (`subscription_ids_hash`)
/// - Regulate timing via frame rate control
///
/// These are transparent to the Application, maintaining separation of concerns.
///
/// # Type Parameters
///
/// * `App` - The application type that implements the [`Application`] trait
///
/// # Example
///
/// ```rust,no_run
/// use color_eyre::eyre::Result;
/// use ratatui::Frame;
/// use tears::prelude::*;
///
/// # struct MyApp;
/// # enum Message {}
/// # impl Application for MyApp {
/// #     type Message = Message;
/// #     type Flags = ();
/// #     fn new(_: ()) -> (Self, Command<Message>) { (MyApp, Command::none()) }
/// #     fn update(&mut self, msg: Message) -> Command<Message> { Command::none() }
/// #     fn view(&self, frame: &mut Frame<'_>) {}
/// #     fn subscriptions(&self) -> Vec<Subscription<Message>> { vec![] }
/// # }
/// #[tokio::main]
/// async fn main() -> Result<()> {
///     let runtime = Runtime::<MyApp>::new(());
///     let mut terminal = ratatui::init();
///     runtime.run(&mut terminal, 60).await?;
///     ratatui::restore();
///     Ok(())
/// }
/// ```
pub struct Runtime<App: Application> {
    /// The application instance
    app: App,
    /// Channel sender for application messages
    msg_tx: mpsc::UnboundedSender<App::Message>,
    /// Channel receiver for application messages
    msg_rx: mpsc::UnboundedReceiver<App::Message>,
    /// Channel sender for quit signals
    quit_tx: mpsc::UnboundedSender<()>,
    /// Channel receiver for quit signals
    quit_rx: mpsc::UnboundedReceiver<()>,
    /// Manages subscription lifecycle
    subscription_manager: SubscriptionManager<App::Message>,

    /// Scheduling optimization: tracks whether rendering is needed.
    ///
    /// Set to `true` when messages are processed (state may have changed).
    /// Reset to `false` after rendering. This allows the Runtime to skip
    /// unnecessary render calls when the UI hasn't changed.
    needs_redraw: bool,

    /// Scheduling optimization: cache of subscription IDs hash from the previous frame.
    ///
    /// Used to determine when subscription updates are needed. When the hash matches
    /// the previous frame, the expensive `SubscriptionManager::update()` call is skipped.
    /// This provides significant performance benefits for applications with static or
    /// infrequently-changing subscriptions.
    subscription_ids_hash: Option<u64>,
}

impl<App: Application> Runtime<App> {
    /// Create a new runtime instance with the given flags.
    ///
    /// Initializes the application by calling [`Application::new`] and executes
    /// any initialization commands returned.
    ///
    /// # Arguments
    ///
    /// * `flags` - Configuration data for application initialization
    ///
    /// # Examples
    ///
    /// ```
    /// use tears::prelude::*;
    /// # use ratatui::Frame;
    /// # struct MyApp;
    /// # enum Message {}
    /// # impl Application for MyApp {
    /// #     type Message = Message;
    /// #     type Flags = i32;
    /// #     fn new(_: i32) -> (Self, Command<Message>) { (MyApp, Command::none()) }
    /// #     fn update(&mut self, msg: Message) -> Command<Message> { Command::none() }
    /// #     fn view(&self, frame: &mut Frame<'_>) {}
    /// #     fn subscriptions(&self) -> Vec<Subscription<Message>> { vec![] }
    /// # }
    ///
    /// let runtime = Runtime::<MyApp>::new(42);
    /// ```
    #[must_use]
    pub fn new(flags: App::Flags) -> Self {
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
            needs_redraw: true,          // Initial draw is always needed
            subscription_ids_hash: None, // No subscriptions cached yet
        };

        // Enqueue the initial command
        runtime.enqueue_command(init_cmd);

        runtime
    }

    /// Enqueue a command to be executed asynchronously.
    ///
    /// Commands are spawned as tokio tasks that execute concurrently with the event loop.
    /// Actions are sent back via channels (Message → message queue, Quit → quit channel).
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
                            // when the Runtime is dropped, which means the application is shutting
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

    /// Process all pending messages in the queue.
    ///
    /// Drains the message queue, calls [`Application::update`] for each message,
    /// and batches the resulting commands for execution.
    ///
    /// Returns `true` if any messages were processed.
    fn process_messages(&mut self) -> bool {
        let mut has_messages = false;
        let mut commands = Vec::new();

        while let Ok(msg) = self.msg_rx.try_recv() {
            has_messages = true;
            let cmd = self.app.update(msg);
            commands.push(cmd);
        }

        // Batch all commands and enqueue them as a single operation
        if !commands.is_empty() {
            self.enqueue_command(Command::batch(commands));
        }

        has_messages
    }

    /// Check if a quit signal has been received.
    fn check_quit(&mut self) -> bool {
        self.quit_rx.try_recv().is_ok()
    }

    /// Initialize subscriptions from the application.
    fn initialize_subscriptions(&mut self) {
        let subscriptions = self.app.subscriptions();
        self.subscription_manager.update(subscriptions);
    }

    /// Update subscriptions if they have changed.
    ///
    /// Uses hash-based caching to skip updates when subscriptions are unchanged.
    fn update_subscriptions(&mut self) {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let subscriptions = self.app.subscriptions();

        // Compute hash of subscription IDs
        let mut hasher = DefaultHasher::new();
        for sub in &subscriptions {
            sub.id.hash(&mut hasher);
        }
        let current_hash = hasher.finish();

        // Only update if subscriptions have changed
        if self.subscription_ids_hash != Some(current_hash) {
            self.subscription_manager.update(subscriptions);
            self.subscription_ids_hash = Some(current_hash);
        }
    }

    /// Render the application to the terminal.
    ///
    /// # Errors
    ///
    /// Returns an error if terminal I/O fails.
    fn render<B: Backend>(
        &self,
        terminal: &mut ratatui::Terminal<B>,
    ) -> Result<(), <B as Backend>::Error> {
        terminal.draw(|frame| {
            self.app.view(frame);
        })?;
        Ok(())
    }

    /// Clean up resources on shutdown.
    fn shutdown(&mut self) {
        self.subscription_manager.shutdown();
    }

    /// Run the application event loop.
    ///
    /// Runs the main event loop until the application quits. Each iteration:
    /// 1. Processes pending messages via [`Application::update`]
    /// 2. Renders the UI if needed via [`Application::view`]
    /// 3. Updates subscriptions based on current state
    /// 4. Waits for the next frame or quit signal
    ///
    /// # Command Execution
    ///
    /// Commands from [`Application::update`] execute asynchronously as tokio tasks.
    /// The runtime yields control to ensure quit commands are processed promptly.
    ///
    /// # Termination
    ///
    /// The loop terminates when a quit signal is received (via [`Action::Quit`])
    /// or when rendering fails.
    ///
    /// # Arguments
    ///
    /// * `terminal` - Ratatui terminal instance
    /// * `frame_rate` - Target frames per second (actual rate may vary based on load)
    ///
    /// # Errors
    ///
    /// Returns an error if terminal rendering fails (e.g., disconnection, I/O error).
    /// Such errors are typically unrecoverable.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use color_eyre::eyre::Result;
    /// # use ratatui::Frame;
    /// # use tears::prelude::*;
    /// # struct MyApp;
    /// # enum Message {}
    /// # impl Application for MyApp {
    /// #     type Message = Message;
    /// #     type Flags = ();
    /// #     fn new(_: ()) -> (Self, Command<Message>) { (MyApp, Command::none()) }
    /// #     fn update(&mut self, msg: Message) -> Command<Message> { Command::none() }
    /// #     fn view(&self, frame: &mut Frame<'_>) {}
    /// #     fn subscriptions(&self) -> Vec<Subscription<Message>> { vec![] }
    /// # }
    /// #[tokio::main]
    /// async fn main() -> Result<()> {
    ///     let runtime = Runtime::<MyApp>::new(());
    ///     let mut terminal = ratatui::init();
    ///     runtime.run(&mut terminal, 60).await?;
    ///     ratatui::restore();
    ///     Ok(())
    /// }
    /// ```
    pub async fn run<B: Backend>(
        mut self,
        terminal: &mut ratatui::Terminal<B>,
        frame_rate: u32,
    ) -> Result<(), <B as Backend>::Error> {
        let frame_duration = Duration::from_millis(1000 / u64::from(frame_rate));

        // Use interval instead of sleep for more accurate frame timing with drift correction
        let mut frame_interval = interval(frame_duration);
        // Skip missed frames rather than trying to catch up
        // This maintains consistent frame rate even if rendering takes longer than expected
        frame_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        self.initialize_subscriptions();

        loop {
            // Process all pending messages and check if redraw is needed
            let has_messages = self.process_messages();
            if has_messages {
                self.needs_redraw = true;
            }

            // Render only if needed
            if self.needs_redraw {
                self.render(terminal)?;
                self.needs_redraw = false;
            }

            // Yield to allow spawned quit tasks to execute
            // This ensures quit signals from commands are processed promptly
            tokio::task::yield_now().await;

            // Check quit after yielding
            if self.check_quit() {
                break;
            }

            // Update subscriptions based on current state (dynamic subscriptions)
            // NOTE: This is called every frame, but uses hash-based caching to skip
            // updates when subscriptions haven't changed, improving performance.
            self.update_subscriptions();

            // Wait for next frame or quit signal (whichever comes first)
            tokio::select! {
                _ = frame_interval.tick() => {
                    // Continue to next frame
                }
                _ = self.quit_rx.recv() => {
                    // Quit signal received during frame wait
                    break;
                }
            }
        }

        self.shutdown();

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::Application;
    use crate::command::{Action, Command};
    use crate::subscription::Subscription;
    use ratatui::Frame;
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
        let runtime = Runtime::<TestApp>::new(42);
        assert_eq!(runtime.app.counter, 42);
    }

    #[test]
    fn test_runtime_new_with_zero() {
        let runtime = Runtime::<TestApp>::new(0);
        assert_eq!(runtime.app.counter, 0);
    }

    #[test]
    fn test_runtime_new_initializes_channels() {
        let runtime = Runtime::<TestApp>::new(0);

        // Runtime should have channels set up
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
        let runtime = Runtime::<AppWithInitCommand>::new(());

        // Give time for init command to be processed
        sleep(Duration::from_millis(50)).await;

        // The runtime should have enqueued the init command
        // We can't directly verify it was processed without running the event loop
        // but we can verify the runtime was created successfully
        assert!(!runtime.app.initialized);
    }

    #[test]
    fn test_runtime_enqueue_command_none() {
        let runtime = Runtime::<TestApp>::new(0);

        // Enqueue a none command (should not panic)
        runtime.enqueue_command(Command::none());
    }

    #[tokio::test]
    async fn test_runtime_enqueue_command_with_message() {
        let runtime = Runtime::<TestApp>::new(0);

        // Enqueue a command that sends a message
        let cmd = Command::future(async { TestMessage::Increment });
        runtime.enqueue_command(cmd);

        // Give time for message to be sent
        sleep(Duration::from_millis(50)).await;

        // We can't directly verify the message was received without running the full event loop
    }

    #[tokio::test]
    async fn test_runtime_enqueue_command_with_quit() {
        let runtime = Runtime::<TestApp>::new(0);

        // Enqueue a quit command
        let cmd = Command::effect(Action::Quit);
        runtime.enqueue_command(cmd);

        // Give time for quit signal to be sent
        sleep(Duration::from_millis(50)).await;

        // The quit signal should have been sent to the quit channel
    }

    // Test multiple runtimes can be created
    #[test]
    fn test_multiple_runtimes() {
        let runtime1 = Runtime::<TestApp>::new(1);
        let runtime2 = Runtime::<TestApp>::new(2);

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
        let runtime = Runtime::<AppWithStringFlags>::new("test".to_string());
        assert_eq!(runtime.app.name, "test");
    }

    #[test]
    fn test_runtime_with_empty_string_flags() {
        let runtime = Runtime::<AppWithStringFlags>::new(String::new());
        assert_eq!(runtime.app.name, "");
    }

    // Unit tests for extracted methods

    #[test]
    fn test_check_quit_no_signal() {
        let mut runtime = Runtime::<TestApp>::new(0);
        assert!(!runtime.check_quit());
    }

    #[test]
    fn test_check_quit_with_signal() {
        let mut runtime = Runtime::<TestApp>::new(0);

        // Send quit signal
        let _ = runtime.quit_tx.send(());

        assert!(runtime.check_quit());
    }

    #[test]
    fn test_check_quit_multiple_signals() {
        let mut runtime = Runtime::<TestApp>::new(0);

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
    async fn test_process_messages_no_messages() {
        let mut runtime = Runtime::<TestApp>::new(0);

        // No messages, should return false
        let has_messages = runtime.process_messages();

        assert!(!has_messages);
        // Counter should remain unchanged
        assert_eq!(runtime.app.counter, 0);
    }

    #[tokio::test]
    async fn test_process_messages_with_increment() {
        let mut runtime = Runtime::<TestApp>::new(0);

        // Send increment message
        let _ = runtime.msg_tx.send(TestMessage::Increment);

        // Process messages - should return true since we processed a message
        let has_messages = runtime.process_messages();

        assert!(has_messages);
        // Counter should be incremented
        assert_eq!(runtime.app.counter, 1);
    }

    #[tokio::test]
    async fn test_process_messages_with_quit() {
        let mut runtime = Runtime::<TestApp>::new(0);

        // Send quit message
        let _ = runtime.msg_tx.send(TestMessage::Quit);

        // Process messages - this will enqueue the quit command
        let has_messages = runtime.process_messages();

        assert!(has_messages);

        // Give time for the async command to send quit signal
        sleep(Duration::from_millis(50)).await;

        // Now check_quit should return true
        assert!(runtime.check_quit());
    }

    #[tokio::test]
    async fn test_process_messages_multiple_messages() {
        let mut runtime = Runtime::<TestApp>::new(0);

        // Send multiple increment messages
        let _ = runtime.msg_tx.send(TestMessage::Increment);
        let _ = runtime.msg_tx.send(TestMessage::Increment);
        let _ = runtime.msg_tx.send(TestMessage::Increment);

        // Process all messages - should return true
        let has_messages = runtime.process_messages();

        assert!(has_messages);
        // Counter should be incremented 3 times
        assert_eq!(runtime.app.counter, 3);
    }

    #[tokio::test]
    async fn test_process_messages_processes_all_messages() {
        let mut runtime = Runtime::<TestApp>::new(0);

        // Manually send quit signal first
        let _ = runtime.quit_tx.send(());

        // Send increment messages
        let _ = runtime.msg_tx.send(TestMessage::Increment);
        let _ = runtime.msg_tx.send(TestMessage::Increment);

        // Process messages - should process all messages regardless of quit signal
        let has_messages = runtime.process_messages();

        assert!(has_messages);
        // Both increments should be processed (quit doesn't stop message processing)
        assert_eq!(runtime.app.counter, 2);

        // Quit signal should still be available
        assert!(runtime.check_quit());
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
                use crate::subscription::time::Timer;
                vec![Subscription::new(Timer::new(100)).map(|_| ())]
            }
        }

        let mut runtime = Runtime::<AppWithSubs>::new(());

        // Should not panic
        runtime.initialize_subscriptions();
    }

    #[test]
    fn test_initialize_subscriptions_empty() {
        let mut runtime = Runtime::<TestApp>::new(0);

        // Should not panic with empty subscriptions
        runtime.initialize_subscriptions();
    }

    #[test]
    fn test_render() -> Result<()> {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let runtime = Runtime::<TestApp>::new(0);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend)?;

        // Should render without error
        assert!(runtime.render(&mut terminal).is_ok());

        Ok(())
    }

    #[test]
    fn test_render_multiple_times() -> Result<()> {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let runtime = Runtime::<TestApp>::new(0);
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
        let mut runtime = Runtime::<TestApp>::new(0);

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

        let mut runtime = Runtime::<AppWithSubs>::new(());
        runtime.initialize_subscriptions();

        // Should cancel subscriptions without panic
        runtime.shutdown();
    }

    // Tests for needs_redraw flag

    #[test]
    fn test_runtime_new_needs_redraw_is_true() {
        let runtime = Runtime::<TestApp>::new(0);

        // Initial redraw should always be needed
        assert!(runtime.needs_redraw);
    }

    #[tokio::test]
    async fn test_process_messages_returns_false_when_no_messages() {
        let mut runtime = Runtime::<TestApp>::new(0);

        // Process with no messages
        let has_messages = runtime.process_messages();

        assert!(!has_messages);
    }

    #[tokio::test]
    async fn test_process_messages_returns_true_when_messages_exist() {
        let mut runtime = Runtime::<TestApp>::new(0);

        // Send a message
        let _ = runtime.msg_tx.send(TestMessage::Increment);

        // Process messages
        let has_messages = runtime.process_messages();

        assert!(has_messages);
    }

    #[tokio::test]
    async fn test_process_messages_returns_true_for_multiple_messages() {
        let mut runtime = Runtime::<TestApp>::new(0);

        // Send multiple messages
        let _ = runtime.msg_tx.send(TestMessage::Increment);
        let _ = runtime.msg_tx.send(TestMessage::Increment);
        let _ = runtime.msg_tx.send(TestMessage::Increment);

        // Process messages - should return true even with multiple messages
        let has_messages = runtime.process_messages();

        assert!(has_messages);
        assert_eq!(runtime.app.counter, 3);
    }

    #[tokio::test]
    async fn test_process_messages_consecutive_calls() {
        let mut runtime = Runtime::<TestApp>::new(0);

        // First call with no messages
        let has_messages = runtime.process_messages();
        assert!(!has_messages);

        // Send a message
        let _ = runtime.msg_tx.send(TestMessage::Increment);

        // Second call with message
        let has_messages = runtime.process_messages();
        assert!(has_messages);
        assert_eq!(runtime.app.counter, 1);

        // Third call with no new messages
        let has_messages = runtime.process_messages();
        assert!(!has_messages);
        assert_eq!(runtime.app.counter, 1);
    }

    // Tests for subscription hash caching

    #[test]
    fn test_subscription_hash_initially_none() {
        let runtime = Runtime::<TestApp>::new(0);
        assert_eq!(runtime.subscription_ids_hash, None);
    }

    #[tokio::test]
    async fn test_update_subscriptions_caches_hash() {
        let mut runtime = Runtime::<TestApp>::new(0);

        // First update should set the hash
        runtime.update_subscriptions();
        assert!(runtime.subscription_ids_hash.is_some());

        let first_hash = runtime.subscription_ids_hash;

        // Second update with same subscriptions should keep the same hash
        runtime.update_subscriptions();
        assert_eq!(runtime.subscription_ids_hash, first_hash);
    }

    #[tokio::test]
    async fn test_update_subscriptions_with_dynamic_subscriptions() {
        // Test app with subscriptions that change based on state
        struct DynamicSubApp {
            enabled: bool,
        }

        impl Application for DynamicSubApp {
            type Message = ();
            type Flags = bool;

            fn new(enabled: bool) -> (Self, Command<Self::Message>) {
                (Self { enabled }, Command::none())
            }

            fn update(&mut self, (): ()) -> Command<Self::Message> {
                self.enabled = !self.enabled;
                Command::none()
            }

            fn view(&self, _frame: &mut Frame<'_>) {}

            fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
                if self.enabled {
                    use crate::subscription::time::Timer;
                    vec![Subscription::new(Timer::new(100)).map(|_| ())]
                } else {
                    vec![]
                }
            }
        }

        let mut runtime = Runtime::<DynamicSubApp>::new(true);

        // First update with enabled=true
        runtime.update_subscriptions();
        let hash_enabled = runtime.subscription_ids_hash;
        assert!(hash_enabled.is_some());

        // Toggle state
        runtime.app.enabled = false;

        // Second update with enabled=false should have different hash
        runtime.update_subscriptions();
        let hash_disabled = runtime.subscription_ids_hash;
        assert!(hash_disabled.is_some());
        assert_ne!(hash_enabled, hash_disabled);

        // Toggle back
        runtime.app.enabled = true;

        // Third update should match first hash
        runtime.update_subscriptions();
        assert_eq!(runtime.subscription_ids_hash, hash_enabled);
    }

    #[tokio::test]
    async fn test_update_subscriptions_skips_manager_when_unchanged() {
        use std::sync::{Arc, Mutex};

        // Track how many times subscriptions() is called
        struct CountingApp {
            call_count: Arc<Mutex<usize>>,
        }

        impl Application for CountingApp {
            type Message = ();
            type Flags = Arc<Mutex<usize>>;

            fn new(call_count: Arc<Mutex<usize>>) -> (Self, Command<Self::Message>) {
                (Self { call_count }, Command::none())
            }

            fn update(&mut self, (): ()) -> Command<Self::Message> {
                Command::none()
            }

            fn view(&self, _frame: &mut Frame<'_>) {}

            fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
                *self.call_count.lock().expect("lock poisoned") += 1;
                vec![]
            }
        }

        let call_count = Arc::new(Mutex::new(0));
        let mut runtime = Runtime::<CountingApp>::new(call_count.clone());

        // Reset counter after initialization
        *call_count.lock().expect("lock poisoned") = 0;

        // First call
        runtime.update_subscriptions();
        assert_eq!(*call_count.lock().expect("lock poisoned"), 1);

        // Second call - subscriptions() is still called but manager update is skipped
        runtime.update_subscriptions();
        assert_eq!(*call_count.lock().expect("lock poisoned"), 2);

        // Third call
        runtime.update_subscriptions();
        assert_eq!(*call_count.lock().expect("lock poisoned"), 3);
    }
}
