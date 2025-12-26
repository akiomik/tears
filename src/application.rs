use ratatui::Frame;

use crate::{command::Command, subscription::Subscription};

/// The main trait that defines a TUI application following the Elm Architecture.
///
/// This trait encapsulates all the core concepts of the Elm Architecture:
/// initialization, updates, view rendering, and subscriptions.
///
/// # Type Parameters
///
/// * `Message` - The type of messages that your application handles. Must be `Send + 'static`.
/// * `Flags` - Configuration data passed at initialization. Must be `Clone + Send`.
///
/// # Example
///
/// ```
/// use ratatui::Frame;
/// use tears::{application::Application, command::Command, subscription::Subscription};
///
/// #[derive(Debug, Clone)]
/// enum Message {
///     Increment,
///     Decrement,
/// }
///
/// struct Counter {
///     value: i32,
/// }
///
/// impl Application for Counter {
///     type Message = Message;
///     type Flags = i32; // Initial value
///
///     fn new(initial: i32) -> (Self, Command<Message>) {
///         (Counter { value: initial }, Command::none())
///     }
///
///     fn update(&mut self, msg: Message) -> Command<Message> {
///         match msg {
///             Message::Increment => self.value += 1,
///             Message::Decrement => self.value -= 1,
///         }
///         Command::none()
///     }
///
///     fn view(&self, frame: &mut Frame<'_>) {
///         // Render UI here
///     }
///
///     fn subscriptions(&self) -> Vec<Subscription<Message>> {
///         vec![]
///     }
/// }
/// ```
pub trait Application: Sized {
    /// The type of messages your application processes.
    ///
    /// Messages represent all possible events that can occur in your application.
    /// They are produced by user interactions, subscriptions, or commands.
    type Message: Send + 'static;

    /// Configuration data for initializing your application.
    ///
    /// Flags allow you to pass initial configuration when creating the application.
    /// Use `()` if no configuration is needed.
    type Flags: Clone + Send;

    /// Initialize the application with the given flags.
    ///
    /// This method is called once when the application starts. It returns
    /// the initial state and an optional command to run at startup.
    ///
    /// # Returns
    ///
    /// A tuple of `(Self, Command<Self::Message>)` containing:
    /// - The initial application state
    /// - An optional command to execute (use `Command::none()` if not needed)
    ///
    /// # Examples
    ///
    /// ```
    /// # use tears::{application::Application, command::Command};
    /// # use ratatui::Frame;
    /// # use tears::subscription::Subscription;
    /// # struct MyApp { initialized: bool }
    /// # #[derive(Clone)] enum Message { InitComplete }
    /// # impl Application for MyApp {
    /// #     type Message = Message;
    /// #     type Flags = ();
    /// fn new(_flags: ()) -> (Self, Command<Message>) {
    ///     let app = MyApp { initialized: false };
    ///     let cmd = Command::perform(
    ///         async { /* initialization work */ },
    ///         |_| Message::InitComplete
    ///     );
    ///     (app, cmd)
    /// }
    /// #     fn update(&mut self, msg: Message) -> Command<Message> { Command::none() }
    /// #     fn view(&self, frame: &mut Frame<'_>) {}
    /// #     fn subscriptions(&self) -> Vec<Subscription<Message>> { vec![] }
    /// # }
    /// ```
    fn new(flags: Self::Flags) -> (Self, Command<Self::Message>);

    /// Process a message and update the application state.
    ///
    /// This is the heart of the Elm Architecture. All state changes happen here
    /// in response to messages. The method can return a command to perform
    /// asynchronous operations.
    ///
    /// # Arguments
    ///
    /// * `msg` - The message to process
    ///
    /// # Returns
    ///
    /// A `Command` to execute after the update (use `Command::none()` if not needed)
    ///
    /// # Examples
    ///
    /// ```
    /// # use tears::{application::Application, command::{Command, Action}};
    /// # use ratatui::Frame;
    /// # use tears::subscription::Subscription;
    /// # struct MyApp;
    /// # enum Message { Save, Quit }
    /// # impl Application for MyApp {
    /// #     type Message = Message;
    /// #     type Flags = ();
    /// #     fn new(_: ()) -> (Self, Command<Message>) { (MyApp, Command::none()) }
    /// fn update(&mut self, msg: Message) -> Command<Message> {
    ///     match msg {
    ///         Message::Save => {
    ///             // Update state and perform async save
    ///             Command::perform(async { /* save */ }, |_| Message::Quit)
    ///         }
    ///         Message::Quit => {
    ///             Command::effect(Action::Quit)
    ///         }
    ///     }
    /// }
    /// #     fn view(&self, frame: &mut Frame<'_>) {}
    /// #     fn subscriptions(&self) -> Vec<Subscription<Message>> { vec![] }
    /// # }
    /// ```
    fn update(&mut self, msg: Self::Message) -> Command<Self::Message>;

    /// Render the application's user interface.
    ///
    /// This method is called every frame to draw the UI. It receives a mutable
    /// reference to the ratatui frame, which you use to render widgets.
    ///
    /// # Arguments
    ///
    /// * `frame` - The ratatui frame to render into
    ///
    /// # Note
    ///
    /// This method should be pure - it should only read from `self` and not
    /// modify state. All state changes should happen in `update()`.
    fn view(&self, frame: &mut Frame<'_>);

    /// Define subscriptions for external event sources.
    ///
    /// Subscriptions represent ongoing sources of messages, such as:
    /// - Keyboard/mouse input
    /// - Timers
    /// - WebSocket connections
    /// - File watchers
    ///
    /// This method is called during initialization to set up subscriptions.
    ///
    /// # Returns
    ///
    /// A vector of subscriptions that the application wants to listen to
    ///
    /// # Examples
    ///
    /// ```
    /// # use tears::{application::Application, command::Command, subscription::Subscription};
    /// # use ratatui::Frame;
    /// # struct MyApp;
    /// # enum Message { Tick, Input }
    /// # impl Application for MyApp {
    /// #     type Message = Message;
    /// #     type Flags = ();
    /// #     fn new(_: ()) -> (Self, Command<Message>) { (MyApp, Command::none()) }
    /// #     fn update(&mut self, msg: Message) -> Command<Message> { Command::none() }
    /// #     fn view(&self, frame: &mut Frame<'_>) {}
    /// fn subscriptions(&self) -> Vec<Subscription<Message>> {
    ///     use tears::subscription::time::{TimeSub, Message as TimeMsg};
    ///     use tears::subscription::terminal::TerminalSub;
    ///
    ///     vec![
    ///         Subscription::new(TimeSub::new(1000)).map(|_| Message::Tick),
    ///         Subscription::new(TerminalSub::new()).map(|_| Message::Input),
    ///     ]
    /// }
    /// # }
    /// ```
    fn subscriptions(&self) -> Vec<Subscription<Self::Message>>;
}
