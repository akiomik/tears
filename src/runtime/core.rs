//! The runtime's owned execution resources for one application run.
//!
//! [`RuntimeCore`] bundles the running application instance with the plumbing
//! the event loop drives it through: the message/quit channels, the input mux,
//! the [`SubscriptionManager`], and the set of running command tasks. It also
//! owns their lifecycle operations (construction, command spawning,
//! subscription initialization, rendering, and shutdown).

use std::panic::AssertUnwindSafe;

use color_eyre::eyre::Result;
use futures::FutureExt;
use futures::stream::StreamExt;
use ratatui::prelude::Backend;
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::application::Application;
use crate::command::{Action, RuntimeCommandParts};
use crate::subscription::SubscriptionManager;

use super::app_input::AppInputs;

/// The runtime's owned execution state for a single [`Application`] run.
///
/// Internal type that orchestrates TUI application execution following The Elm
/// Architecture. It holds the application instance, routes messages, executes
/// commands asynchronously, and coordinates subscriptions through
/// [`SubscriptionManager`]. [`Runtime`](super::Runtime) drives it; applications
/// should use `Runtime` directly instead of this type.
///
/// Fields and methods are `pub(super)` so [`Runtime`](super::Runtime) can drive
/// them directly. This is deliberate rather than a leak: the `run()` loop's
/// `select!` borrows `app_inputs` and `quit_rx` in separate branches, which
/// requires disjoint `&mut` borrows of individual fields; wrapping them in
/// whole-`self` methods would hold `&mut self` across the whole `select!` and
/// conflict with the branch handlers' `&mut self`. Direct field access is thus
/// the only workable shape, so the methods here express intent rather than
/// enforce encapsulation.
///
/// # Type Parameters
///
/// * `App` - The application type implementing [`Application`]
pub(super) struct RuntimeCore<App: Application> {
    /// The application instance
    pub(super) app: App,
    /// Sender for application messages
    pub(super) msg_tx: mpsc::UnboundedSender<App::Message>,
    /// Runtime-owned receiver for application inputs
    pub(super) app_inputs: AppInputs<App::Message>,
    /// Sender for quit signals
    pub(super) quit_tx: mpsc::UnboundedSender<()>,
    /// Receiver for quit signals
    pub(super) quit_rx: mpsc::UnboundedReceiver<()>,
    /// Manages subscription lifecycle
    pub(super) subscription_manager: SubscriptionManager<App::Message>,
    /// Running command tasks.
    ///
    /// Kept so command tasks can be aborted on shutdown, and — because a
    /// [`JoinSet`] aborts its tasks when dropped — also when the runtime is
    /// dropped while unwinding from a panic. Finished tasks are reaped on each
    /// enqueue so the set does not grow without bound.
    pub(super) command_tasks: JoinSet<()>,
}

impl<App: Application> RuntimeCore<App> {
    /// Creates the runtime core with the given initialization flags.
    ///
    /// Initializes the application by calling [`Application::new`] and automatically
    /// enqueues any returned initialization commands for execution.
    ///
    /// # Arguments
    ///
    /// * `flags` - Configuration data passed to [`Application::new`]
    #[must_use]
    pub(super) fn new(flags: App::Flags) -> Self {
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();
        let app_inputs = AppInputs::new(msg_rx);
        let (quit_tx, quit_rx) = mpsc::unbounded_channel();
        let subscription_manager = SubscriptionManager::new(msg_tx.clone());

        // Initialize the application with flags
        let (app, init_cmd) = App::new(flags);

        let mut core = Self {
            app,
            msg_tx,
            app_inputs,
            quit_tx,
            quit_rx,
            subscription_manager,
            command_tasks: JoinSet::new(),
        };

        // Enqueue the initial command
        core.enqueue_command(init_cmd.into_runtime_parts());

        core
    }

    /// Enqueues decomposed command parts for asynchronous execution.
    ///
    /// Spawns a tokio task that executes the command's action stream, if one exists.
    /// Messages are sent to the message channel, and quit signals are sent to the quit
    /// channel. The task terminates when the stream completes or a quit action is
    /// received.
    ///
    /// The task is tracked in [`command_tasks`](Self::command_tasks) so it can be aborted
    /// on shutdown (or when the runtime is dropped). If the command's stream panics, the
    /// panic is caught and logged rather than silently lost.
    ///
    /// Send failures are silently ignored as they only occur during application shutdown.
    pub(super) fn enqueue_command(&mut self, parts: RuntimeCommandParts<App::Message>) {
        if let Some(stream) = parts.into_stream() {
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
                                // when the RuntimeCore is dropped, which means the application is shutting
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
    pub(super) fn initialize_subscriptions(&mut self) {
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
    pub(super) fn render<B: Backend>(
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
    pub(super) fn shutdown(&mut self) {
        self.subscription_manager.shutdown();
        self.command_tasks.abort_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::future::pending;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use ratatui::backend::TestBackend;
    use ratatui::prelude::*;
    use tokio::sync::oneshot;
    use tokio::time::{Duration, timeout};

    use crate::command::Command;
    use crate::runtime::AppInput;
    use crate::subscription::Subscription;
    use crate::subscription::time::Timer;
    use crate::test_support::{TestApp, TestMessage, wait_until};

    #[test]
    fn test_new() {
        let core = RuntimeCore::<TestApp>::new(42);
        assert_eq!(core.app.counter, 42);
    }

    #[test]
    fn test_new_with_zero() {
        let core = RuntimeCore::<TestApp>::new(0);
        assert_eq!(core.app.counter, 0);
    }

    #[test]
    fn test_new_initializes_channels() {
        let core = RuntimeCore::<TestApp>::new(0);

        // RuntimeCore should have channels set up. We can't directly test the
        // private plumbing, but we can verify it was created.
        assert_eq!(core.app.counter, 0);
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
    async fn test_processes_init_command() {
        let mut core = RuntimeCore::<AppWithInitCommand>::new(());

        let input = timeout(Duration::from_secs(1), core.app_inputs.next())
            .await
            .expect("init command should send a message before the timeout");

        assert_eq!(input, Some(AppInput::Shared(true)));
        assert!(!core.app.initialized);
    }

    #[test]
    fn test_enqueue_command_none() {
        let mut core = RuntimeCore::<TestApp>::new(0);

        // Enqueue a none command (should not panic)
        core.enqueue_command(Command::none().into_runtime_parts());
    }

    #[tokio::test]
    async fn test_enqueue_command_with_message() {
        let mut core = RuntimeCore::<TestApp>::new(0);

        // Enqueue a command that sends a message
        let cmd = Command::future(async { TestMessage::Increment });
        core.enqueue_command(cmd.into_runtime_parts());

        let input = timeout(Duration::from_secs(1), core.app_inputs.next())
            .await
            .expect("command should send a message before the timeout");

        assert!(matches!(
            input,
            Some(AppInput::Shared(TestMessage::Increment))
        ));
    }

    #[tokio::test]
    async fn test_enqueue_command_with_quit() {
        let mut core = RuntimeCore::<TestApp>::new(0);

        // Enqueue a quit command
        let cmd = Command::effect(Action::Quit);
        core.enqueue_command(cmd.into_runtime_parts());

        timeout(Duration::from_secs(1), core.quit_rx.recv())
            .await
            .expect("quit command should send a quit signal before the timeout")
            .expect("quit channel should remain open");
    }

    // Test multiple cores can be created
    #[test]
    fn test_multiple_cores() {
        let core1 = RuntimeCore::<TestApp>::new(1);
        let core2 = RuntimeCore::<TestApp>::new(2);

        assert_eq!(core1.app.counter, 1);
        assert_eq!(core2.app.counter, 2);
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
    fn test_with_string_flags() {
        let core = RuntimeCore::<AppWithStringFlags>::new("test".to_string());
        assert_eq!(core.app.name, "test");
    }

    #[test]
    fn test_with_empty_string_flags() {
        let core = RuntimeCore::<AppWithStringFlags>::new(String::new());
        assert_eq!(core.app.name, "");
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
                vec![
                    Subscription::new(
                        Timer::try_new(100).expect("timer interval must be non-zero"),
                    )
                    .map(|_| ()),
                ]
            }
        }

        let mut core = RuntimeCore::<AppWithSubs>::new(());

        // Should not panic
        core.initialize_subscriptions();
    }

    #[test]
    fn test_initialize_subscriptions_empty() {
        let mut core = RuntimeCore::<TestApp>::new(0);

        // Should not panic with empty subscriptions
        core.initialize_subscriptions();
    }

    #[test]
    fn test_render() -> Result<()> {
        let core = RuntimeCore::<TestApp>::new(0);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend)?;

        // Should render without error
        assert!(core.render(&mut terminal).is_ok());

        Ok(())
    }

    #[test]
    fn test_render_multiple_times() -> Result<()> {
        let core = RuntimeCore::<TestApp>::new(0);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend)?;

        // Should be able to render multiple times
        assert!(core.render(&mut terminal).is_ok());
        assert!(core.render(&mut terminal).is_ok());
        assert!(core.render(&mut terminal).is_ok());

        Ok(())
    }

    #[test]
    fn test_shutdown() {
        let mut core = RuntimeCore::<TestApp>::new(0);

        // Should not panic
        core.shutdown();
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
                vec![
                    Subscription::new(
                        Timer::try_new(100).expect("timer interval must be non-zero"),
                    )
                    .map(|_| ()),
                ]
            }
        }

        let mut core = RuntimeCore::<AppWithSubs>::new(());
        core.initialize_subscriptions();

        // Should cancel subscriptions without panic
        core.shutdown();
    }

    #[tokio::test]
    async fn test_dropping_core_aborts_command_tasks() {
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
            let mut core = RuntimeCore::<TestApp>::new(0);
            let guard = AbortGuard(aborted.clone());
            let (started_tx, started_rx) = oneshot::channel();
            // A command that owns the guard and never completes, so its task
            // stays parked until aborted.
            core.enqueue_command(
                Command::future(async move {
                    let _guard = guard;
                    let _ = started_tx.send(());
                    pending::<TestMessage>().await
                })
                .into_runtime_parts(),
            );

            timeout(Duration::from_secs(1), started_rx)
                .await
                .expect("command task should start before the timeout")
                .expect("command task should signal that it started");
            assert!(
                !aborted.load(Ordering::SeqCst),
                "command task should still be running before the core is dropped"
            );
            // `core` (and its `command_tasks` JoinSet) is dropped here.
        }

        wait_until(
            || aborted.load(Ordering::SeqCst),
            "dropping the core should abort running command tasks",
        )
        .await;
    }
}
