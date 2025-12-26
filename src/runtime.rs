use std::time::Duration;

use color_eyre::eyre::Result;
use futures::stream::StreamExt;
use ratatui::prelude::Backend;
use tokio::{sync::mpsc, time::sleep};

use crate::{
    application::Application,
    command::{Action, Command},
    subscription::SubscriptionManager,
};

/// Internal wrapper for the application instance.
#[repr(transparent)]
struct Instance<A: Application> {
    inner: A,
}

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
/// * `A` - The application type that implements the [`Application`] trait
///
/// # Example
///
/// ```rust,no_run
/// use tears::{application::Application, command::Command, runtime::Runtime, subscription::Subscription};
/// use ratatui::Frame;
/// use std::io;
/// use crossterm::terminal;
/// use ratatui::prelude::CrosstermBackend;
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
/// async fn main() -> color_eyre::eyre::Result<()> {
///     // Create the runtime with initial flags
///     let runtime = Runtime::<MyApp>::new(());
///
///     // Setup terminal
///     terminal::enable_raw_mode()?;
///     let mut stdout = io::stdout();
///     let backend = CrosstermBackend::new(stdout);
///     let mut terminal = ratatui::Terminal::new(backend)?;
///
///     // Run the application at 60 FPS
///     runtime.run(&mut terminal, 60).await?;
///
///     // Cleanup
///     terminal::disable_raw_mode()?;
///     Ok(())
/// }
/// ```
pub struct Runtime<A: Application> {
    app: Instance<A>,
    msg_tx: mpsc::UnboundedSender<A::Message>,
    msg_rx: mpsc::UnboundedReceiver<A::Message>,
    quit_tx: mpsc::UnboundedSender<()>,
    quit_rx: mpsc::UnboundedReceiver<()>,
    subscription_manager: SubscriptionManager<A::Message>,
}

impl<A: Application> Runtime<A> {
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
    /// use tears::runtime::Runtime;
    /// # use tears::{application::Application, command::Command, subscription::Subscription};
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
    pub fn new(flags: A::Flags) -> Self {
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();
        let (quit_tx, quit_rx) = mpsc::unbounded_channel();
        let subscription_manager = SubscriptionManager::new(msg_tx.clone());

        // Initialize the application with flags
        let (app, init_cmd) = A::new(flags);
        let instance = Instance { inner: app };

        let runtime = Self {
            app: instance,
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
    /// The command's actions will be sent to the message queue.
    fn enqueue_command(&self, cmd: Command<A::Message>) {
        if let Some(stream) = cmd.stream {
            let msg_tx = self.msg_tx.clone();
            let quit_tx = self.quit_tx.clone();
            tokio::spawn(async move {
                futures::pin_mut!(stream);
                while let Some(action) = stream.next().await {
                    match action {
                        Action::Message(msg) => {
                            // Send message to the queue
                            // NOTE: If the receiver is dropped, this will fail silently
                            let _ = msg_tx.send(msg);
                        }
                        Action::Quit => {
                            // Send quit signal
                            let _ = quit_tx.send(());
                            break;
                        }
                    }
                }
            });
        }
    }

    async fn process_messages(&mut self) {
        while let Ok(msg) = self.msg_rx.try_recv() {
            let cmd = self.app.inner.update(msg);

            // Enqueue the command for asynchronous execution
            self.enqueue_command(cmd);
        }

        // NOTE: Dynamic subscription updates are currently disabled due to the following issues:
        //
        // Problem 1: Infinite loop when updating subscriptions in message loop
        // - If we call subscription_manager.update() inside the while loop for each message,
        //   new messages can arrive from subscriptions during processing, causing an infinite loop.
        //
        // Problem 2: Blocking when updating after all messages are processed
        // - If we call subscription_manager.update() after the while loop only when has_messages is true,
        //   the screen doesn't update until the next message arrives.
        // - This is because subscriptions() creates new Subscription instances every time,
        //   and calling update() may interfere with the message flow.
        //
        // Current solution: Update subscriptions only at initialization (line 69-70 in run())
        // - This works for applications with static subscriptions
        // - But prevents dynamic subscription changes based on application state
        //
        // TODO: Implement proper subscription diffing to support dynamic subscriptions
        // - Cache the previous subscription set and compare with new one
        // - Only update when there's an actual difference
    }

    /// Run the application event loop.
    ///
    /// This method starts the main event loop and runs until the application quits.
    /// It performs the following actions in each iteration:
    ///
    /// 1. Renders the UI by calling [`Application::view`]
    /// 2. Processes all pending messages by calling [`Application::update`]
    /// 3. Executes any commands returned by the update function
    /// 4. Checks if a quit signal has been received
    /// 5. Waits for the next frame based on the frame rate
    ///
    /// The event loop terminates when:
    /// - A command returns [`Action::Quit`](crate::command::Action::Quit)
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
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use tears::{application::Application, command::Command, runtime::Runtime, subscription::Subscription};
    /// # use ratatui::Frame;
    /// # use std::io;
    /// # use crossterm::terminal;
    /// # use ratatui::prelude::CrosstermBackend;
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
    /// async fn main() -> color_eyre::eyre::Result<()> {
    ///     let runtime = Runtime::<MyApp>::new(());
    ///
    ///     terminal::enable_raw_mode()?;
    ///     let backend = CrosstermBackend::new(io::stdout());
    ///     let mut terminal = ratatui::Terminal::new(backend)?;
    ///
    ///     // Run at 60 FPS
    ///     runtime.run(&mut terminal, 60).await?;
    ///
    ///     terminal::disable_raw_mode()?;
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
    ) -> Result<()> {
        let frame_duration = Duration::from_millis(1000 / frame_rate as u64);

        let subscriptions = self.app.inner.subscriptions();
        self.subscription_manager.update(subscriptions);

        loop {
            terminal.draw(|frame| {
                self.app.inner.view(frame);
            })?;

            // Process messages
            self.process_messages().await;

            // Check if quit was requested
            if self.quit_rx.try_recv().is_ok() {
                break;
            }

            // Wait for next frame
            sleep(frame_duration).await;
        }

        // cancel all subscriptions
        self.subscription_manager.shutdown();

        Ok(())
    }
}
