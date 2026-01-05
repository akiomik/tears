//! Commands for performing asynchronous side effects.
//!
//! Commands represent asynchronous operations that produce messages or actions.
//! They are the primary way to perform side effects in the Elm Architecture,
//! such as HTTP requests, file I/O, or any other async operation.
//!
//! # Examples
//!
//! ```
//! use tears::prelude::*;
//!
//! enum Message {
//!     DataLoaded(String),
//! }
//!
//! async fn load_data() -> String {
//!     // Perform async operation
//!     "data".to_string()
//! }
//!
//! // Create a command that performs an async operation
//! let cmd = Command::perform(load_data(), Message::DataLoaded);
//! ```

use futures::{
    FutureExt, Stream, StreamExt,
    stream::{self, BoxStream, select_all},
};

/// An action that can be performed by a command.
///
/// Actions represent side effects that can be executed as a result of processing commands.
/// They are emitted by command streams and processed by the runtime.
pub enum Action<Msg> {
    /// Send a message to the application's update function.
    ///
    /// This is the most common action, used to communicate results of
    /// asynchronous operations back to the application.
    Message(Msg),

    /// Request the application to quit.
    ///
    /// When this action is emitted, the runtime will terminate the event loop
    /// and shut down the application gracefully.
    Quit,
}

/// A command that can be executed to perform side effects.
///
/// Commands represent asynchronous operations that produce messages or actions.
/// They are the primary way to perform side effects in the Elm architecture,
/// such as:
/// - Running async tasks (HTTP requests, file I/O, etc.)
/// - Subscribing to event sources
/// - Performing computations in the background
///
/// Commands are returned from `Application::new` and `Application::update`,
/// and are executed by the runtime.
///
/// # Examples
///
/// ```
/// use tears::prelude::*;
///
/// enum Message {
///     GotResult(i32),
/// }
///
/// // Create a command that performs an async operation
/// let cmd = Command::perform(
///     async { 42 },
///     |result| Message::GotResult(result)
/// );
/// ```
pub struct Command<Msg: Send + 'static> {
    pub(super) stream: Option<BoxStream<'static, Action<Msg>>>,
}

impl<Msg: Send + 'static> Command<Msg> {
    /// Create a command that does nothing.
    ///
    /// This is useful when you need to return a command but have no side effects to perform.
    ///
    /// # Examples
    ///
    /// ```
    /// use tears::prelude::*;
    ///
    /// let cmd: Command<i32> = Command::none();
    /// ```
    #[must_use]
    pub const fn none() -> Self {
        Self { stream: None }
    }

    /// Perform an asynchronous operation and convert its result to a message.
    ///
    /// This is one of the most common ways to create commands. It runs a future
    /// and applies a function to convert the result into a message.
    ///
    /// # Arguments
    ///
    /// * `future` - The async operation to perform
    /// * `f` - Function to convert the result into a message
    ///
    /// # Examples
    ///
    /// ```
    /// use tears::prelude::*;
    ///
    /// async fn fetch_data() -> String {
    ///     "data".to_string()
    /// }
    ///
    /// enum Message {
    ///     DataReceived(String),
    /// }
    ///
    /// let cmd = Command::perform(fetch_data(), Message::DataReceived);
    /// ```
    #[must_use]
    pub fn perform<A>(
        future: impl Future<Output = A> + Send + 'static,
        f: impl FnOnce(A) -> Msg + Send + 'static,
    ) -> Self {
        Self::future(future.map(f))
    }

    /// Create a command from a future that produces a message.
    ///
    /// Unlike `perform`, this method expects the future to directly produce a message
    /// without any conversion function.
    ///
    /// # Examples
    ///
    /// ```
    /// use tears::prelude::*;
    ///
    /// let cmd = Command::future(async { 42 });
    /// ```
    #[must_use]
    pub fn future(future: impl Future<Output = Msg> + Send + 'static) -> Self {
        Self {
            stream: Some(future.into_stream().map(Action::Message).boxed()),
        }
    }

    /// Send a message to the application immediately.
    ///
    /// This is a tears-specific feature that allows immediate message dispatch
    /// within the update cycle. It's useful for state transitions and converting
    /// input events to messages.
    ///
    /// # Why `message` and not `send` or `dispatch`?
    ///
    /// We chose `message` to be explicit about what this method does: it sends
    /// a message to the application's update function. This keeps the API surface
    /// clear for future extensions like `send()` for stream operations or
    /// `dispatch()` for task management.
    ///
    /// # Examples
    ///
    /// ```
    /// use tears::prelude::*;
    ///
    /// enum Message {
    ///     GoToMenu,
    ///     Refresh,
    /// }
    ///
    /// // Send a message immediately
    /// let cmd = Command::message(Message::Refresh);
    ///
    /// // State transition
    /// let cmd = Command::message(Message::GoToMenu);
    /// ```
    ///
    /// # Comparison with iced
    ///
    /// Unlike iced, tears supports self-messaging through commands. In iced,
    /// such cases would typically be handled directly in the update function.
    #[must_use]
    pub fn message(msg: Msg) -> Self {
        Self::effect(Action::Message(msg))
    }

    /// Create a command that performs a single action immediately.
    ///
    /// This is the fundamental way to execute actions in tears, compatible with
    /// iced's `effect` API design (v0.14.0). For sending messages specifically,
    /// consider using [`Command::message`] for better ergonomics.
    ///
    /// # Examples
    ///
    /// ```
    /// use tears::prelude::*;
    ///
    /// // Quit the application
    /// let cmd: Command<i32> = Command::effect(Action::Quit);
    ///
    /// // Send a message immediately (prefer Command::message for this)
    /// let cmd = Command::effect(Action::Message(42));
    /// ```
    ///
    /// # Comparison with iced
    ///
    /// This is similar to iced's `Task::effect` (v0.14.0), which accepts
    /// actions without output. However, tears allows `Action::Message` for
    /// consistency with its self-messaging feature.
    #[must_use]
    pub fn effect(action: Action<Msg>) -> Self {
        Self {
            stream: Some(stream::once(async move { action }).boxed()),
        }
    }

    /// Batch multiple commands into a single command.
    ///
    /// All commands will be executed concurrently. The order in which
    /// messages arrive is not guaranteed. Commands that are `Command::none()`
    /// are automatically filtered out.
    ///
    /// # Examples
    ///
    /// ```
    /// use tears::prelude::*;
    ///
    /// enum Message {
    ///     First(i32),
    ///     Second(String),
    /// }
    ///
    /// let cmd = Command::batch(vec![
    ///     Command::perform(async { 1 }, Message::First),
    ///     Command::perform(async { "data".to_string() }, Message::Second),
    ///     Command::none(), // This will be filtered out
    /// ]);
    /// ```
    #[must_use]
    pub fn batch(commands: impl IntoIterator<Item = Self>) -> Self {
        let streams: Vec<_> = commands.into_iter().filter_map(|cmd| cmd.stream).collect();

        if streams.is_empty() {
            Self::none()
        } else {
            Self {
                stream: Some(select_all(streams).boxed()),
            }
        }
    }

    /// Create a command from a stream of messages.
    ///
    /// The stream will be consumed and each item will be sent as a message
    /// to the application's update function.
    ///
    /// # Examples
    ///
    /// ```
    /// use tears::prelude::*;
    /// use futures::stream;
    ///
    /// let messages = stream::iter(vec![1, 2, 3]);
    /// let cmd = Command::stream(messages);
    /// ```
    #[must_use]
    pub fn stream(stream: impl Stream<Item = Msg> + Send + 'static) -> Self {
        Self {
            stream: Some(stream.map(Action::Message).boxed()),
        }
    }

    /// Run a stream and convert each item to a message.
    ///
    /// This is useful for subscribing to external event sources where you need
    /// to transform the stream's items into your application's message type.
    ///
    /// # Arguments
    ///
    /// * `stream` - The stream to consume
    /// * `f` - Function to convert each stream item into a message
    ///
    /// # Examples
    ///
    /// ```
    /// use tears::prelude::*;
    /// use futures::stream;
    ///
    /// enum Message {
    ///     NumberReceived(i32),
    /// }
    ///
    /// let numbers = stream::iter(vec![1, 2, 3]);
    /// let cmd = Command::run(numbers, |n| Message::NumberReceived(n * 2));
    /// ```
    #[must_use]
    pub fn run<A>(
        stream: impl Stream<Item = A> + Send + 'static,
        f: impl Fn(A) -> Msg + Send + 'static,
    ) -> Self
    where
        Msg: 'static,
    {
        Self::stream(stream.map(f))
    }

    /// Transform the message type of this command.
    ///
    /// This allows you to adapt a command that produces messages of one type
    /// to produce messages of another type. This is particularly useful when
    /// composing commands from different parts of your application or when
    /// working with generic operations like HTTP mutations.
    ///
    /// # Arguments
    ///
    /// * `f` - Function to convert messages from type `Msg` to type `T`
    ///
    /// # Examples
    ///
    /// ```
    /// use tears::prelude::*;
    ///
    /// enum Message {
    ///     DataLoaded(Result<String, String>),
    ///     Error(String),
    /// }
    ///
    /// // Create a command that produces Result<String, String>
    /// let cmd: Command<Result<String, String>> = Command::future(async {
    ///     Ok("data".to_string())
    /// });
    ///
    /// // Map it to your application's message type
    /// let cmd = cmd.map(Message::DataLoaded);
    /// ```
    ///
    /// # Advanced Example with Mutation
    ///
    /// ```rust,ignore
    /// use tears::subscription::http::Mutation;
    ///
    /// enum Message {
    ///     UserUpdated(User),
    ///     UpdateFailed(String),
    /// }
    ///
    /// // Mutation returns Command<Result<User, Error>>
    /// let cmd = Mutation::mutate(user_data, update_user_api)
    ///     .map(|result| match result {
    ///         Ok(user) => Message::UserUpdated(user),
    ///         Err(e) => Message::UpdateFailed(e.to_string()),
    ///     });
    /// ```
    #[must_use]
    pub fn map<T>(self, f: impl Fn(Msg) -> T + Send + 'static) -> Command<T>
    where
        T: Send + 'static,
    {
        self.stream.map_or_else(Command::none, |stream| {
            let mapped = stream.map(move |action| match action {
                Action::Message(msg) => Action::Message(f(msg)),
                Action::Quit => Action::Quit,
            });
            Command {
                stream: Some(mapped.boxed()),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use futures::stream;
    use tokio::time::{Duration, sleep};

    #[tokio::test]
    async fn test_batch_empty() {
        let cmd: Command<i32> = Command::batch(vec![]);
        assert!(cmd.stream.is_none());
    }

    #[tokio::test]
    async fn test_batch_single_command() {
        let cmd1 = Command::future(async { 1 });
        let cmd = Command::batch(vec![cmd1]);

        let mut stream = cmd.stream.expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(msg) if msg == 1));
    }

    #[tokio::test]
    async fn test_batch_multiple_commands() {
        let cmd1 = Command::future(async { 1 });
        let cmd2 = Command::future(async { 2 });
        let cmd3 = Command::future(async { 3 });

        let cmd = Command::batch(vec![cmd1, cmd2, cmd3]);

        let mut stream = cmd.stream.expect("stream should exist");
        let mut results = vec![];

        while let Some(action) = stream.next().await {
            match action {
                Action::Message(msg) => results.push(msg),
                Action::Quit => break,
            }
        }

        // All messages should be received (order may vary due to concurrent execution)
        results.sort_unstable();
        assert_eq!(results, vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn test_batch_with_none_commands() {
        let cmd1 = Command::future(async { 1 });
        let cmd2 = Command::<i32>::none();
        let cmd3 = Command::future(async { 3 });

        let cmd = Command::batch(vec![cmd1, cmd2, cmd3]);

        let mut stream = cmd.stream.expect("stream should exist");
        let mut results = vec![];

        while let Some(action) = stream.next().await {
            match action {
                Action::Message(msg) => results.push(msg),
                Action::Quit => break,
            }
        }

        // Only non-none commands should produce messages
        results.sort_unstable();
        assert_eq!(results, vec![1, 3]);
    }

    #[tokio::test]
    async fn test_batch_all_none() {
        let cmd1 = Command::<i32>::none();
        let cmd2 = Command::<i32>::none();

        let cmd = Command::batch(vec![cmd1, cmd2]);
        assert!(cmd.stream.is_none());
    }

    #[tokio::test]
    async fn test_batch_with_quit_action() {
        let cmd1 = Command::future(async { 1 });
        let cmd2 = Command::effect(Action::Quit);
        let cmd3 = Command::future(async { 3 });

        let cmd = Command::batch(vec![cmd1, cmd2, cmd3]);

        let mut stream = cmd.stream.expect("stream should exist");
        let mut has_quit = false;
        let mut messages = vec![];

        while let Some(action) = stream.next().await {
            match action {
                Action::Message(msg) => messages.push(msg),
                Action::Quit => {
                    has_quit = true;
                    break;
                }
            }
        }

        assert!(has_quit, "should receive quit action");
        assert!(!messages.is_empty());
    }

    #[tokio::test]
    async fn test_stream() {
        let input_stream = stream::iter(vec![1, 2, 3]);
        let cmd = Command::stream(input_stream);

        let mut stream = cmd.stream.expect("stream should exist");
        let mut results = vec![];

        while let Some(action) = stream.next().await {
            match action {
                Action::Message(msg) => results.push(msg),
                Action::Quit => break,
            }
        }

        assert_eq!(results, vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn test_run() {
        let input_stream = stream::iter(vec![1, 2, 3]);
        let cmd = Command::run(input_stream, |x| x * 2);

        let mut stream = cmd.stream.expect("stream should exist");
        let mut results = vec![];

        while let Some(action) = stream.next().await {
            match action {
                Action::Message(msg) => results.push(msg),
                Action::Quit => break,
            }
        }

        assert_eq!(results, vec![2, 4, 6]);
    }

    #[tokio::test]
    async fn test_run_with_conversion() {
        #[derive(Debug, PartialEq)]
        enum Message {
            Number(i32),
        }

        let input_stream = stream::iter(vec![1, 2, 3]);
        let cmd = Command::run(input_stream, |x| Message::Number(x * 10));

        let mut stream = cmd.stream.expect("stream should exist");
        let mut results = vec![];

        while let Some(action) = stream.next().await {
            match action {
                Action::Message(msg) => results.push(msg),
                Action::Quit => break,
            }
        }

        assert_eq!(
            results,
            vec![
                Message::Number(10),
                Message::Number(20),
                Message::Number(30)
            ]
        );
    }

    #[tokio::test]
    async fn test_run_with_empty_stream() {
        let input_stream = stream::iter(Vec::<i32>::new());
        let cmd = Command::run(input_stream, |x| x * 2);

        let mut stream = cmd.stream.expect("stream should exist");
        let result = stream.next().await;

        assert!(result.is_none(), "empty stream should produce no messages");
    }

    #[tokio::test]
    async fn test_none() {
        let cmd: Command<i32> = Command::none();
        assert!(cmd.stream.is_none());
    }

    #[tokio::test]
    async fn test_future() {
        let cmd = Command::future(async { 42 });

        let mut stream = cmd.stream.expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(msg) if msg == 42));

        // Stream should be exhausted
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_perform() {
        #[allow(clippy::unused_async)]
        async fn fetch_value() -> i32 {
            42
        }

        let cmd = Command::perform(fetch_value(), |x| x * 2);

        let mut stream = cmd.stream.expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(msg) if msg == 84));
    }

    #[tokio::test]
    async fn test_perform_with_result() {
        #[allow(clippy::unused_async)]
        async fn fallible_operation() -> Result<String, String> {
            Ok("success".to_string())
        }

        let cmd = Command::perform(fallible_operation(), |result| match result {
            Ok(s) => format!("Got: {s}"),
            Err(e) => format!("Error: {e}"),
        });

        let mut stream = cmd.stream.expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(msg) if msg == "Got: success"));
    }

    #[tokio::test]
    async fn test_message() {
        let cmd = Command::message(42);

        let mut stream = cmd.stream.expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(msg) if msg == 42));

        // Stream should be exhausted
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_message_with_string() {
        let cmd = Command::message("hello".to_string());

        let mut stream = cmd.stream.expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(msg) if msg == "hello"));
    }

    #[tokio::test]
    async fn test_effect_with_message() {
        let cmd = Command::effect(Action::Message(100));

        let mut stream = cmd.stream.expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(msg) if msg == 100));
    }

    #[tokio::test]
    async fn test_effect_with_quit() {
        let cmd: Command<i32> = Command::effect(Action::Quit);

        let mut stream = cmd.stream.expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Quit));
    }

    #[tokio::test]
    async fn test_stream_empty() {
        let input_stream = stream::iter(Vec::<i32>::new());
        let cmd = Command::stream(input_stream);

        let mut stream = cmd.stream.expect("stream should exist");
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_batch_nested() {
        // Test batching commands that are themselves batches
        let cmd1 = Command::future(async { 1 });
        let cmd2 = Command::future(async { 2 });
        let batch1 = Command::batch(vec![cmd1, cmd2]);

        let cmd3 = Command::future(async { 3 });
        let cmd4 = Command::future(async { 4 });
        let batch2 = Command::batch(vec![cmd3, cmd4]);

        let final_batch = Command::batch(vec![batch1, batch2]);

        let mut stream = final_batch.stream.expect("stream should exist");
        let mut results = vec![];

        while let Some(action) = stream.next().await {
            match action {
                Action::Message(msg) => results.push(msg),
                Action::Quit => break,
            }
        }

        results.sort_unstable();
        assert_eq!(results, vec![1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn test_future_with_delay() {
        let cmd = Command::future(async {
            sleep(Duration::from_millis(10)).await;
            "delayed".to_string()
        });

        let mut stream = cmd.stream.expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(msg) if msg == "delayed"));
    }

    #[tokio::test]
    async fn test_perform_with_error_handling() {
        #[allow(clippy::unused_async)]
        async fn may_fail(should_fail: bool) -> Result<i32, &'static str> {
            if should_fail {
                Err("operation failed")
            } else {
                Ok(42)
            }
        }

        // Test success case
        let cmd = Command::perform(may_fail(false), |result| result.unwrap_or(-1));

        let mut stream = cmd.stream.expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(msg) if msg == 42));

        // Test error case
        let cmd = Command::perform(may_fail(true), |result| result.unwrap_or(-1));

        let mut stream = cmd.stream.expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(msg) if msg == -1));
    }

    #[tokio::test]
    async fn test_batch_execution_order_independence() {
        // Commands with different delays to test concurrent execution
        let cmd1 = Command::future(async {
            sleep(Duration::from_millis(30)).await;
            1
        });
        let cmd2 = Command::future(async {
            sleep(Duration::from_millis(10)).await;
            2
        });
        let cmd3 = Command::future(async {
            sleep(Duration::from_millis(20)).await;
            3
        });

        let cmd = Command::batch(vec![cmd1, cmd2, cmd3]);

        let mut stream = cmd.stream.expect("stream should exist");
        let mut results = vec![];

        while let Some(action) = stream.next().await {
            match action {
                Action::Message(msg) => results.push(msg),
                Action::Quit => break,
            }
        }

        // Results should be received in order of completion (2, 3, 1)
        // but we just verify all were received
        results.sort_unstable();
        assert_eq!(results, vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn test_map() {
        let cmd = Command::future(async { 42 });
        let mapped = cmd.map(|x| x * 2);

        let mut stream = mapped.stream.expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(msg) if msg == 84));
    }

    #[tokio::test]
    async fn test_map_with_type_conversion() {
        #[derive(Debug, PartialEq)]
        enum Message {
            Number(i32),
        }

        let cmd: Command<i32> = Command::future(async { 42 });
        let mapped = cmd.map(Message::Number);

        let mut stream = mapped.stream.expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(Message::Number(42))));
    }

    #[tokio::test]
    async fn test_map_with_result() {
        #[derive(Debug, PartialEq)]
        enum Message {
            Success(String),
            Error(String),
        }

        let cmd: Command<Result<String, String>> =
            Command::future(async { Ok("data".to_string()) });

        let mapped = cmd.map(|result| match result {
            Ok(s) => Message::Success(s),
            Err(e) => Message::Error(e),
        });

        let mut stream = mapped.stream.expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(Message::Success(ref s)) if s == "data"));
    }

    #[tokio::test]
    async fn test_map_none() {
        let cmd: Command<i32> = Command::none();
        let mapped = cmd.map(|x| x * 2);

        assert!(mapped.stream.is_none());
    }

    #[tokio::test]
    async fn test_map_preserves_quit() {
        let cmd: Command<i32> = Command::effect(Action::Quit);
        let mapped = cmd.map(|x| x * 2);

        let mut stream = mapped.stream.expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Quit));
    }
}
