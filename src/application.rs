//! Application trait and core types.
//!
//! This module contains the [`Application`] trait, which is the main interface
//! for defining TUI applications using the Elm Architecture pattern.

use ratatui::Frame;

use crate::{command::Command, subscription::Subscription};

/// The main trait for defining TUI applications following The Elm Architecture.
///
/// # Type Parameters
///
/// * `Message` - Messages that your application handles
/// * `Flags` - Configuration data for initialization
///
/// # Example
///
/// ```
/// use ratatui::Frame;
/// use tears::prelude::*;
///
/// #[derive(Debug, Clone)]
/// enum Message { Increment, Decrement }
///
/// struct Counter { value: i32 }
///
/// impl Application for Counter {
///     type Message = Message;
///     type Flags = i32;
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
///         // Render UI
///     }
///
///     fn subscriptions(&self) -> Vec<Subscription<Message>> {
///         vec![]
///     }
/// }
/// ```
pub trait Application: Sized {
    /// The type of messages your application processes.
    type Message: Send + 'static;

    /// Configuration data for initializing your application.
    type Flags;

    /// Initialize the application with the given flags.
    ///
    /// Returns the initial state and an optional initialization command.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tears::prelude::*;
    /// # use ratatui::Frame;
    /// # struct MyApp;
    /// # enum Message { Init }
    /// # impl Application for MyApp {
    /// #     type Message = Message;
    /// #     type Flags = String;
    /// fn new(config: String) -> (Self, Command<Message>) {
    ///     let cmd = Command::perform(async { /* async init */ }, |_| Message::Init);
    ///     (MyApp, cmd)
    /// }
    /// #     fn update(&mut self, msg: Message) -> Command<Message> { Command::none() }
    /// #     fn view(&self, frame: &mut Frame<'_>) {}
    /// #     fn subscriptions(&self) -> Vec<Subscription<Message>> { vec![] }
    /// # }
    /// ```
    fn new(flags: Self::Flags) -> (Self, Command<Self::Message>);

    /// Process a message and update the application state.
    ///
    /// All state changes happen here in response to messages. Can return a command
    /// to perform asynchronous operations.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tears::prelude::*;
    /// # use ratatui::Frame;
    /// # struct MyApp;
    /// # enum Message { Save, Quit }
    /// # impl Application for MyApp {
    /// #     type Message = Message;
    /// #     type Flags = ();
    /// #     fn new(_: ()) -> (Self, Command<Message>) { (MyApp, Command::none()) }
    /// fn update(&mut self, msg: Message) -> Command<Message> {
    ///     match msg {
    ///         Message::Save => Command::perform(async { /* save */ }, |_| Message::Quit),
    ///         Message::Quit => Command::effect(Action::Quit),
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
    /// Subscriptions represent ongoing sources of messages, such as timers,
    /// keyboard input, WebSocket connections, or file watchers.
    ///
    /// This method can return different subscriptions based on application state,
    /// enabling dynamic subscription management.
    ///
    /// # Examples
    ///
    /// ```
    /// # use tears::prelude::*;
    /// # use ratatui::Frame;
    /// # struct MyApp { enabled: bool }
    /// # enum Message { Tick }
    /// # impl Application for MyApp {
    /// #     type Message = Message;
    /// #     type Flags = ();
    /// #     fn new(_: ()) -> (Self, Command<Message>) { (MyApp { enabled: true }, Command::none()) }
    /// #     fn update(&mut self, msg: Message) -> Command<Message> { Command::none() }
    /// #     fn view(&self, frame: &mut Frame<'_>) {}
    /// fn subscriptions(&self) -> Vec<Subscription<Message>> {
    ///     if self.enabled {
    ///         use tears::subscription::time::Timer;
    ///         vec![Subscription::new(Timer::new(1000)).map(|_| Message::Tick)]
    ///     } else {
    ///         vec![]
    ///     }
    /// }
    /// # }
    /// ```
    fn subscriptions(&self) -> Vec<Subscription<Self::Message>>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::Action;
    use futures::StreamExt;
    use ratatui::Frame;

    // Test application implementation
    #[derive(Debug)]
    struct TestApp {
        counter: i32,
        initialized: bool,
    }

    #[derive(Debug, Clone, PartialEq)]
    enum TestMessage {
        Increment,
        Decrement,
        Reset,
    }

    impl Application for TestApp {
        type Message = TestMessage;
        type Flags = i32;

        fn new(initial_value: i32) -> (Self, Command<Self::Message>) {
            let app = Self {
                counter: initial_value,
                initialized: true,
            };
            (app, Command::none())
        }

        fn update(&mut self, msg: Self::Message) -> Command<Self::Message> {
            match msg {
                TestMessage::Increment => {
                    self.counter += 1;
                }
                TestMessage::Decrement => {
                    self.counter -= 1;
                }
                TestMessage::Reset => {
                    self.counter = 0;
                }
            }
            Command::none()
        }

        fn view(&self, _frame: &mut Frame<'_>) {
            // No-op for testing
        }

        fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
            vec![]
        }
    }

    #[test]
    fn test_application_new() {
        let (app, cmd) = TestApp::new(42);
        assert_eq!(app.counter, 42);
        assert!(app.initialized);
        assert!(cmd.stream.is_none()); // Command::none()
    }

    #[test]
    fn test_application_new_with_zero() {
        let (app, _) = TestApp::new(0);
        assert_eq!(app.counter, 0);
    }

    #[test]
    fn test_application_update_increment() {
        let (mut app, _) = TestApp::new(0);
        app.update(TestMessage::Increment);
        assert_eq!(app.counter, 1);
    }

    #[test]
    fn test_application_update_decrement() {
        let (mut app, _) = TestApp::new(10);
        app.update(TestMessage::Decrement);
        assert_eq!(app.counter, 9);
    }

    #[test]
    fn test_application_update_reset() {
        let (mut app, _) = TestApp::new(42);
        app.update(TestMessage::Reset);
        assert_eq!(app.counter, 0);
    }

    #[test]
    fn test_application_update_multiple() {
        let (mut app, _) = TestApp::new(0);
        app.update(TestMessage::Increment);
        app.update(TestMessage::Increment);
        app.update(TestMessage::Increment);
        assert_eq!(app.counter, 3);
    }

    #[test]
    fn test_application_update_mixed_operations() {
        let (mut app, _) = TestApp::new(5);
        app.update(TestMessage::Increment); // 6
        app.update(TestMessage::Decrement); // 5
        app.update(TestMessage::Increment); // 6
        app.update(TestMessage::Increment); // 7
        assert_eq!(app.counter, 7);
    }

    #[test]
    fn test_application_subscriptions() {
        let (app, _) = TestApp::new(0);
        let subs = app.subscriptions();
        assert!(subs.is_empty());
    }

    // Test application with command
    struct AppWithCommand;

    impl Application for AppWithCommand {
        type Message = String;
        type Flags = ();

        fn new(_flags: ()) -> (Self, Command<Self::Message>) {
            let cmd = Command::future(async { "initialized".to_string() });
            (Self, cmd)
        }

        fn update(&mut self, _msg: Self::Message) -> Command<Self::Message> {
            Command::none()
        }

        fn view(&self, _frame: &mut Frame<'_>) {}

        fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
            vec![]
        }
    }

    #[tokio::test]
    async fn test_application_new_with_command() {
        let (_, cmd) = AppWithCommand::new(());
        assert!(cmd.stream.is_some());

        // Verify the command produces the expected message
        if let Some(mut stream) = cmd.stream {
            if let Some(action) = stream.next().await {
                assert!(matches!(action, Action::Message(msg) if msg == "initialized"));
            }
        }
    }

    // Test message traits
    #[test]
    fn test_message_debug() {
        let msg = TestMessage::Increment;
        let debug_str = format!("{msg:?}");
        assert!(debug_str.contains("Increment"));
    }

    #[test]
    fn test_message_clone() {
        let msg1 = TestMessage::Increment;
        let msg2 = msg1.clone();
        assert_eq!(msg1, msg2);
    }

    #[test]
    fn test_message_equality() {
        assert_eq!(TestMessage::Increment, TestMessage::Increment);
        assert_ne!(TestMessage::Increment, TestMessage::Decrement);
    }
}
