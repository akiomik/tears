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
/// use tears::command::Command;
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
    /// use tears::command::Command;
    ///
    /// let cmd: Command<i32> = Command::none();
    /// ```
    pub fn none() -> Self {
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
    /// use tears::command::Command;
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
    /// use tears::command::Command;
    ///
    /// let cmd = Command::future(async { 42 });
    /// ```
    pub fn future(future: impl Future<Output = Msg> + Send + 'static) -> Self {
        Self {
            stream: Some(future.into_stream().map(Action::Message).boxed()),
        }
    }

    /// Create a command that performs a single action immediately.
    ///
    /// This is useful for operations that need to happen synchronously,
    /// such as quitting the application.
    ///
    /// # Examples
    ///
    /// ```
    /// use tears::command::{Action, Command};
    ///
    /// // Quit the application
    /// let cmd: Command<i32> = Command::effect(Action::Quit);
    ///
    /// // Send a message immediately
    /// let cmd = Command::effect(Action::Message(42));
    /// ```
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
    /// use tears::command::Command;
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
    pub fn batch(commands: impl IntoIterator<Item = Command<Msg>>) -> Self {
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
    /// use tears::command::Command;
    /// use futures::stream;
    ///
    /// let messages = stream::iter(vec![1, 2, 3]);
    /// let cmd = Command::stream(messages);
    /// ```
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
    /// use tears::command::Command;
    /// use futures::stream;
    ///
    /// enum Message {
    ///     NumberReceived(i32),
    /// }
    ///
    /// let numbers = stream::iter(vec![1, 2, 3]);
    /// let cmd = Command::run(numbers, |n| Message::NumberReceived(n * 2));
    /// ```
    pub fn run<A>(
        stream: impl Stream<Item = A> + Send + 'static,
        f: impl Fn(A) -> Msg + Send + 'static,
    ) -> Self
    where
        Msg: 'static,
    {
        Self::stream(stream.map(f))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

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

        match action {
            Action::Message(msg) => assert_eq!(msg, 1),
            Action::Quit => panic!("unexpected quit"),
        }
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
        results.sort();
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
        results.sort();
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
    }

    #[tokio::test]
    async fn test_stream() {
        use futures::stream;

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
        use futures::stream;

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
        use futures::stream;

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
        use futures::stream;

        let input_stream = stream::iter(Vec::<i32>::new());
        let cmd = Command::run(input_stream, |x| x * 2);

        let mut stream = cmd.stream.expect("stream should exist");
        let result = stream.next().await;

        assert!(result.is_none(), "empty stream should produce no messages");
    }
}
