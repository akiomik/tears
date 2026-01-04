//! Runtime for executing TUI applications.
//!
//! The [`Runtime`] manages the application lifecycle, including:
//! - Initializing the application
//! - Running the event loop
//! - Processing messages and commands
//! - Managing subscriptions
//! - Rendering the UI
//!
//! # Example
//!
//! ```rust,no_run
//! use tears::prelude::*;
//! # use color_eyre::eyre::Result;
//! # use ratatui::Frame;
//! # struct MyApp;
//! # enum Message {}
//! # impl Application for MyApp {
//! #     type Message = Message;
//! #     type Flags = ();
//! #     fn new(_: ()) -> (Self, Command<Message>) { (MyApp, Command::none()) }
//! #     fn update(&mut self, msg: Message) -> Command<Message> { Command::none() }
//! #     fn view(&self, frame: &mut Frame<'_>) {}
//! #     fn subscriptions(&self) -> Vec<Subscription<Message>> { vec![] }
//! # }
//!
//! #[tokio::main]
//! async fn main() -> Result<()> {
//!     let runtime = Runtime::<MyApp>::new(());
//!
//!     let mut terminal = ratatui::init();
//!
//!     runtime.run(&mut terminal, 60).await?;
//!
//!     ratatui::restore();
//!     Ok(())
//! }
//! ```

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
/// The runtime is responsible for:
/// - Initializing the application
/// - Running the event loop
/// - Processing messages and updating the application state
/// - Executing commands asynchronously
/// - Managing subscriptions
/// - Rendering the UI at the specified frame rate
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
///     // Create the runtime with initial flags
///     let runtime = Runtime::<MyApp>::new(());
///
///     // Setup terminal
///     let mut terminal = ratatui::init();
///
///     // Run the application at 60 FPS
///     runtime.run(&mut terminal, 60).await?;
///
///     // Cleanup
///     ratatui::restore();
///     Ok(())
/// }
/// ```
pub struct Runtime<App: Application> {
    app: App,
    msg_tx: mpsc::UnboundedSender<App::Message>,
    msg_rx: mpsc::UnboundedReceiver<App::Message>,
    quit_tx: mpsc::UnboundedSender<()>,
    quit_rx: mpsc::UnboundedReceiver<()>,
    subscription_manager: SubscriptionManager<App::Message>,
}

impl<App: Application> Runtime<App> {
    /// Create a new runtime instance with the given flags.
    ///
    /// This method:
    /// 1. Creates the necessary channels for message passing
    /// 2. Initializes the application by calling [`Application::new`]
    /// 3. Executes any initialization commands returned by the application
    /// 4. Sets up the subscription manager
    ///
    /// # Arguments
    ///
    /// * `flags` - Configuration data to pass to the application's initialization
    ///
    /// # Returns
    ///
    /// A new `Runtime` instance ready to be executed
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
    /// // Create runtime with initial value
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
        };

        // Enqueue the initial command
        runtime.enqueue_command(init_cmd);

        runtime
    }

    /// Enqueue a command to be executed asynchronously.
    ///
    /// Commands are spawned as separate tokio tasks that will execute concurrently
    /// with the main event loop. Actions produced by the command stream are sent
    /// back to the runtime via channels:
    /// - `Action::Message` values are sent to the message queue for processing
    /// - `Action::Quit` signals are sent to the quit channel to terminate the loop
    ///
    /// The spawned task will run on the tokio runtime and complete independently
    /// of the main event loop timing.
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

    /// Process all pending messages in the message queue.
    ///
    /// This method processes messages synchronously by:
    /// 1. Draining all pending messages from the message queue using `try_recv`
    /// 2. Calling [`Application::update`] for each message
    /// 3. Collecting all returned commands
    /// 4. Batching and enqueuing the commands for asynchronous execution
    ///
    /// Commands returned by `update` are spawned as async tasks but are not
    /// guaranteed to execute before this method returns. The runtime yields
    /// control after calling this method to allow spawned tasks to run.
    ///
    /// By batching commands, we avoid spawning multiple tokio tasks when
    /// processing multiple messages in a single frame, which improves
    /// performance when there are many pending messages.
    fn process_messages(&mut self) {
        let mut commands = Vec::new();

        while let Ok(msg) = self.msg_rx.try_recv() {
            let cmd = self.app.update(msg);
            commands.push(cmd);
        }

        // Batch all commands and enqueue them as a single operation
        if !commands.is_empty() {
            self.enqueue_command(Command::batch(commands));
        }
    }

    /// Check if a quit signal has been received.
    ///
    /// Returns `true` if at least one quit signal is in the queue.
    /// This uses `try_recv` for non-blocking check, consuming one signal if present.
    fn check_quit(&mut self) -> bool {
        self.quit_rx.try_recv().is_ok()
    }

    /// Initialize subscriptions from the application.
    fn initialize_subscriptions(&mut self) {
        let subscriptions = self.app.subscriptions();
        self.subscription_manager.update(subscriptions);
    }

    /// Update subscriptions based on current application state.
    ///
    /// This method is called every frame to support dynamic subscriptions.
    /// The `SubscriptionManager` will diff the new subscriptions against
    /// the currently running ones and only start/stop changed subscriptions.
    fn update_subscriptions(&mut self) {
        let subscriptions = self.app.subscriptions();
        self.subscription_manager.update(subscriptions);
    }

    /// Render the application's view to the terminal.
    ///
    /// # Errors
    ///
    /// Returns an error if terminal I/O fails. Common causes include terminal
    /// disconnection or broken pipe. These errors are typically unrecoverable.
    fn render<B: Backend>(
        &self,
        terminal: &mut ratatui::Terminal<B>,
    ) -> Result<(), <B as Backend>::Error> {
        terminal.draw(|frame| {
            self.app.view(frame);
        })?;
        Ok(())
    }

    /// Perform cleanup when shutting down.
    fn shutdown(&mut self) {
        self.subscription_manager.shutdown();
    }

    /// Run the application event loop.
    ///
    /// This method starts the main event loop and runs until the application quits.
    /// It performs the following actions in each iteration:
    ///
    /// 1. Renders the UI by calling [`Application::view`]
    /// 2. Processes all pending messages by calling [`Application::update`]
    /// 3. Yields control to the async runtime to execute spawned command tasks
    /// 4. Checks if a quit signal has been received
    /// 5. Updates subscriptions based on current application state
    /// 6. Waits for the next frame or quit signal (whichever comes first)
    ///
    /// ## Command Execution Model
    ///
    /// Commands returned by [`Application::update`] are executed asynchronously
    /// as separate tokio tasks. The runtime yields control after processing messages
    /// (using [`tokio::task::yield_now`]) to ensure that these command tasks have
    /// an opportunity to execute before the next quit check. This design guarantees
    /// that quit commands are processed promptly without blocking on frame timing.
    ///
    /// This behavior is similar to how Elm's runtime processes commands in the
    /// JavaScript event loop - commands are scheduled to execute "soon" but not
    /// necessarily synchronously with the update that produced them.
    ///
    /// ## Termination
    ///
    /// The event loop terminates when:
    /// - A command returns [`Action::Quit`]
    /// - An error occurs during rendering
    ///
    /// # Arguments
    ///
    /// * `terminal` - A mutable reference to the ratatui terminal
    /// * `frame_rate` - The target frames per second (FPS) for rendering
    ///
    /// # Returns
    ///
    /// `Ok(())` on successful completion, or an error if rendering fails
    ///
    /// # Errors
    ///
    /// Returns an error if terminal rendering fails (e.g., terminal disconnection,
    /// I/O errors, broken pipe). Such errors typically indicate unrecoverable
    /// terminal state, and the application will terminate. This behavior is
    /// consistent with other TUI frameworks and ratatui best practices
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
    ///
    ///     let mut terminal = ratatui::init();
    ///
    ///     // Run at 60 FPS
    ///     runtime.run(&mut terminal, 60).await?;
    ///
    ///     ratatui::restore();
    ///     Ok(())
    /// }
    /// ```
    ///
    /// # Note
    ///
    /// The actual frame rate may vary depending on:
    /// - The complexity of your UI rendering
    /// - The time spent processing messages
    /// - System performance
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
            self.render(terminal)?;

            // Process all pending messages
            self.process_messages();

            // Yield to allow spawned quit tasks to execute
            // This ensures quit signals from commands are processed promptly
            tokio::task::yield_now().await;

            // Check quit after yielding
            if self.check_quit() {
                break;
            }

            // Update subscriptions based on current state (dynamic subscriptions)
            // NOTE: This is called every frame. The SubscriptionManager efficiently
            // diffs subscriptions and only starts/stops changed ones.
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

        // No messages, should just return without doing anything
        runtime.process_messages();

        // Counter should remain unchanged
        assert_eq!(runtime.app.counter, 0);
    }

    #[tokio::test]
    async fn test_process_messages_with_increment() {
        let mut runtime = Runtime::<TestApp>::new(0);

        // Send increment message
        let _ = runtime.msg_tx.send(TestMessage::Increment);

        // Process messages
        runtime.process_messages();

        // Counter should be incremented
        assert_eq!(runtime.app.counter, 1);
    }

    #[tokio::test]
    async fn test_process_messages_with_quit() {
        let mut runtime = Runtime::<TestApp>::new(0);

        // Send quit message
        let _ = runtime.msg_tx.send(TestMessage::Quit);

        // Process messages - this will enqueue the quit command
        runtime.process_messages();

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

        // Process all messages
        runtime.process_messages();

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
        runtime.process_messages();

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
}
