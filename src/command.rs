//! Commands for performing asynchronous side effects and returning runtime
//! directives.
//!
//! Commands represent asynchronous operations that produce messages or actions,
//! plus attributes that tell the runtime how to handle the update that returned
//! them. They are the primary way to perform side effects in the Elm
//! Architecture, such as HTTP requests, file I/O, or any other async operation.
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

use std::time::Duration;

mod effect;
mod retry;
mod runtime_directives;
mod runtime_parts;

use effect::Effect;
#[cfg(test)]
use futures::stream::BoxStream;
use futures::{FutureExt, Stream, StreamExt};
pub use retry::{RetryBackoff, RetryContext, RetryError, RetryPolicy, RetryStopReason};
use runtime_directives::RuntimeDirectives;
pub(crate) use runtime_parts::RuntimeCommandParts;

/// An internal runtime directive produced by a command's effect stream.
///
/// This type is not part of the public API. Use [`Command::message`] and
/// [`Command::quit`] to construct the corresponding commands.
pub(crate) enum Action<Msg> {
    /// Send a message to the application's update function.
    Message(Msg),

    /// Request the application to quit.
    Quit,
}

/// A command that can be executed to perform side effects and carry runtime
/// directives.
///
/// Commands represent asynchronous operations that produce messages,
/// such as HTTP requests, file I/O, or background computations. They may also
/// carry runtime attributes, such as [`Command::without_redraw`], independently
/// of whether they have a side-effect stream.
///
/// # Examples
///
/// ```
/// use tears::prelude::*;
///
/// enum Message { GotResult(i32) }
///
/// let cmd = Command::perform(async { 42 }, Message::GotResult);
/// ```
#[must_use = "Commands represent side effects and runtime directives in the Elm Architecture and must be handled by the runtime."]
pub struct Command<Msg: Send + 'static> {
    effect: Effect<Msg>,
    directives: RuntimeDirectives,
}

impl<Msg: Send + 'static> Command<Msg> {
    const fn with_effect(effect: Effect<Msg>) -> Self {
        Self {
            effect,
            directives: RuntimeDirectives::DEFAULT,
        }
    }

    /// Create a command with no side-effect stream.
    ///
    /// A stream-less command may still carry runtime attributes after applying
    /// modifiers such as [`Command::without_redraw`], so it should not be
    /// treated as having no effect on runtime behavior.
    ///
    /// # Examples
    ///
    /// ```
    /// use tears::prelude::*;
    ///
    /// let cmd: Command<i32> = Command::none();
    /// ```
    pub const fn none() -> Self {
        Self {
            effect: Effect::none(),
            directives: RuntimeDirectives::DEFAULT,
        }
    }

    /// Returns `true` if the command has no side-effect stream.
    ///
    /// This only reflects whether the runtime has a stream to spawn. A command
    /// with no stream may still carry runtime attributes such as
    /// [`Command::without_redraw`].
    ///
    /// # Examples
    ///
    /// ```
    /// use tears::prelude::*;
    ///
    /// let cmd: Command<i32> = Command::none();
    /// assert!(cmd.is_none());
    ///
    /// let cmd = Command::perform(async { 42 }, |x| x);
    /// assert!(!cmd.is_none());
    /// ```
    #[must_use]
    pub const fn is_none(&self) -> bool {
        self.effect.is_none()
    }

    /// Returns `true` if the command has a side-effect stream.
    ///
    /// This is the inverse of [`Command::is_none`] and does not inspect runtime
    /// attributes such as [`Command::without_redraw`].
    ///
    /// # Examples
    ///
    /// ```
    /// use tears::prelude::*;
    ///
    /// let cmd = Command::perform(async { 42 }, |x| x);
    /// assert!(cmd.is_some());
    ///
    /// let cmd: Command<i32> = Command::none();
    /// assert!(!cmd.is_some());
    /// ```
    #[must_use]
    pub const fn is_some(&self) -> bool {
        self.effect.is_some()
    }

    #[cfg(test)]
    pub(super) const fn requests_redraw(&self) -> bool {
        self.directives.requests_redraw()
    }

    #[cfg(test)]
    pub(super) fn into_stream(self) -> Option<BoxStream<'static, Action<Msg>>> {
        self.effect.into_stream()
    }

    /// Decompose this command into the runtime-facing parts that must be
    /// observed together.
    pub(super) fn into_runtime_parts(self) -> RuntimeCommandParts<Msg> {
        RuntimeCommandParts::new(self.directives, self.effect.into_stream())
    }

    /// Declare that the update returning this command did not change the
    /// visible view, so the runtime may skip the redraw it would otherwise
    /// perform.
    ///
    /// Side effects still run. This is an optimization hint rather than a
    /// guarantee: the runtime may still redraw for other reasons, such as an
    /// initial frame or another message in the same batch.
    #[must_use = "without_redraw returns a modified command and does not mutate in place"]
    pub const fn without_redraw(mut self) -> Self {
        self.directives = self.directives.without_redraw();
        self
    }

    /// Add an overall deadline to every effect leaf in this command.
    ///
    /// The deadline for each leaf starts when that leaf is first polled. A
    /// single call emits at most one timeout message across all of the
    /// command's leaves, while messages produced before the deadline continue
    /// to flow normally. Applying a timeout to [`Command::none`] is inert.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::time::Duration;
    /// use tears::prelude::*;
    ///
    /// enum Message {
    ///     Loaded(String),
    ///     TimedOut,
    /// }
    ///
    /// let cmd = Command::perform(async { "data".to_string() }, Message::Loaded)
    ///     .timeout(Duration::from_secs(5), || Message::TimedOut);
    /// ```
    #[must_use = "timeout returns a modified command and does not mutate in place"]
    pub fn timeout(
        mut self,
        duration: Duration,
        on_timeout: impl FnOnce() -> Msg + Send + 'static,
    ) -> Self {
        self.effect = self.effect.timeout(duration, on_timeout);
        self
    }

    /// Retry an operation after every error while attempts remain.
    ///
    /// Arguments read as configuration → repeatable operation → message
    /// conversion. `policy.max_attempts()` includes the first execution, and
    /// the operation receives a 1-based [`RetryContext`] for every attempt.
    /// Processing emits one final message containing either the successful
    /// value or a [`RetryError`].
    ///
    /// # Repetition safety
    ///
    /// The operation may run up to `policy.max_attempts()` times. Callers must
    /// ensure repetition is safe: a non-idempotent external side effect can
    /// occur more than once, including when an attempt performs the side
    /// effect and later returns an error.
    ///
    /// # Examples
    ///
    /// ```
    /// use tears::Command;
    /// use tears::command::{RetryError, RetryPolicy};
    ///
    /// enum Message {
    ///     Loaded(Result<String, RetryError<&'static str>>),
    /// }
    ///
    /// let policy = RetryPolicy::try_new(3).expect("attempt count is non-zero");
    /// let command = Command::retry(
    ///     policy,
    ///     |_| async { Ok::<_, &'static str>("data".to_string()) },
    ///     Message::Loaded,
    /// );
    /// ```
    pub fn retry<A, E, Fut, Op, F>(policy: RetryPolicy, operation: Op, f: F) -> Self
    where
        A: Send + 'static,
        E: Send + 'static,
        Fut: Future<Output = Result<A, E>> + Send + 'static,
        Op: FnMut(RetryContext) -> Fut + Send + 'static,
        F: FnOnce(Result<A, RetryError<E>>) -> Msg + Send + 'static,
    {
        Self::retry_if(policy, operation, |_, _| true, f)
    }

    /// Retry an operation when its error is accepted by a predicate.
    ///
    /// Arguments read as configuration → repeatable operation → retry
    /// predicate → message conversion. The predicate is called only when an
    /// error occurs while another attempt remains. Rejecting that error
    /// produces [`RetryStopReason::StoppedByPredicate`]; an error on the final
    /// attempt produces [`RetryStopReason::Exhausted`] without invoking the
    /// predicate.
    ///
    /// The operation may run up to `policy.max_attempts()` times, so callers
    /// are responsible for ensuring that repetition is safe.
    pub fn retry_if<A, E, Fut, Op, P, F>(
        policy: RetryPolicy,
        operation: Op,
        should_retry: P,
        f: F,
    ) -> Self
    where
        A: Send + 'static,
        E: Send + 'static,
        Fut: Future<Output = Result<A, E>> + Send + 'static,
        Op: FnMut(RetryContext) -> Fut + Send + 'static,
        P: FnMut(&E, RetryContext) -> bool + Send + 'static,
        F: FnOnce(Result<A, RetryError<E>>) -> Msg + Send + 'static,
    {
        Self::future(async move {
            let result = retry::run_retry(policy, operation, should_retry).await;
            f(result)
        })
    }

    /// Perform an asynchronous operation and convert its result to a message.
    ///
    /// # Examples
    ///
    /// ```
    /// use tears::prelude::*;
    ///
    /// enum Message { DataReceived(String) }
    ///
    /// async fn fetch_data() -> String { "data".to_string() }
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
    /// # Examples
    ///
    /// ```
    /// use tears::prelude::*;
    ///
    /// let cmd = Command::future(async { 42 });
    /// ```
    pub fn future(future: impl Future<Output = Msg> + Send + 'static) -> Self {
        Self::with_effect(Effect::future(future))
    }

    /// Send a message to the application immediately.
    ///
    /// Useful for state transitions and converting input events to messages.
    ///
    /// # Examples
    ///
    /// ```
    /// use tears::prelude::*;
    ///
    /// enum Message { GoToMenu, Refresh }
    ///
    /// let cmd = Command::message(Message::Refresh);
    /// ```
    pub fn message(msg: Msg) -> Self {
        Self::effect(Action::Message(msg))
    }

    /// Create a command that requests the application to quit immediately.
    ///
    /// # Examples
    ///
    /// ```
    /// use tears::prelude::*;
    ///
    /// let cmd: Command<i32> = Command::quit();
    /// ```
    pub fn quit() -> Self {
        Self::effect(Action::Quit)
    }

    fn effect(action: Action<Msg>) -> Self {
        Self::with_effect(Effect::action(action))
    }

    /// Batch multiple commands into a single command.
    ///
    /// All command streams execute concurrently. Commands with no side-effect
    /// stream do not contribute work to spawn, but their runtime attributes are
    /// still folded into the combined command. The combined command redraws if
    /// any child command redraws; only an empty input uses the default redraw
    /// behavior.
    ///
    /// # Examples
    ///
    /// ```
    /// use tears::prelude::*;
    ///
    /// enum Message { First(i32), Second(String) }
    ///
    /// let cmd = Command::batch(vec![
    ///     Command::perform(async { 1 }, Message::First),
    ///     Command::perform(async { "data".to_string() }, Message::Second),
    /// ]);
    /// ```
    pub fn batch(commands: impl IntoIterator<Item = Self>) -> Self {
        let mut directives = RuntimeDirectives::DEFAULT.without_redraw();
        let mut any_child = false;
        let mut effects = Vec::new();

        for cmd in commands {
            any_child = true;
            directives = directives.combine(cmd.directives);
            effects.push(cmd.effect);
        }

        if any_child {
            Self {
                effect: Effect::batch(effects),
                directives,
            }
        } else {
            Self::none()
        }
    }

    /// Create a command from a stream of messages.
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
    pub fn stream(stream: impl Stream<Item = Msg> + Send + 'static) -> Self {
        Self::with_effect(Effect::stream(stream))
    }

    /// Run a stream and convert each item to a message.
    ///
    /// # Examples
    ///
    /// ```
    /// use tears::prelude::*;
    /// use futures::stream;
    ///
    /// enum Message { NumberReceived(i32) }
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
    pub fn map<T>(self, f: impl Fn(Msg) -> T + Send + 'static) -> Command<T>
    where
        T: Send + 'static,
    {
        let directives = self.directives;
        let effect = self.effect.map(f);

        Command { effect, directives }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::{cell::Cell, future::pending};

    use futures::stream;
    use tokio::time::{advance, sleep};

    #[test]
    fn test_redraw_defaults_to_true_for_constructors() {
        assert!(Command::<i32>::none().requests_redraw());
        assert!(Command::message(1).requests_redraw());
        assert!(Command::future(async { 1 }).requests_redraw());
        assert!(Command::perform(async { 1 }, |value| value).requests_redraw());
        assert!(Command::<i32>::quit().requests_redraw());
        assert!(Command::stream(stream::iter(vec![1])).requests_redraw());
        assert!(Command::run(stream::iter(vec![1]), |value| value).requests_redraw());
        assert!(Command::batch(vec![Command::<i32>::none()]).requests_redraw());
        assert!(Command::<i32>::batch(vec![]).requests_redraw());
    }

    #[test]
    fn test_without_redraw_flips_redraw() {
        let cmd = Command::<i32>::none().without_redraw();
        assert!(!cmd.requests_redraw());
    }

    #[test]
    fn test_batch_redraw_is_or_over_children() {
        let cmd = Command::batch(vec![
            Command::none().without_redraw(),
            Command::future(async { 1 }),
        ]);
        assert!(cmd.requests_redraw());

        let cmd = Command::batch(vec![
            Command::future(async { 1 }).without_redraw(),
            Command::future(async { 2 }).without_redraw(),
        ]);
        assert!(!cmd.requests_redraw());
    }

    #[test]
    fn test_batch_all_opted_out_streamless_children_stays_opted_out() {
        let cmd = Command::batch(vec![Command::<i32>::none().without_redraw()]);

        assert!(cmd.is_none());
        assert!(!cmd.requests_redraw());
    }

    #[test]
    fn test_map_preserves_redraw_for_stream_command() {
        let cmd = Command::future(async { 1 })
            .without_redraw()
            .map(|value| value * 2);
        assert!(!cmd.requests_redraw());
    }

    #[test]
    fn test_map_preserves_redraw_for_streamless_command() {
        let cmd = Command::<i32>::none()
            .without_redraw()
            .map(|value| value * 2);

        assert!(cmd.is_none());
        assert!(!cmd.requests_redraw());
    }

    #[test]
    fn test_is_none_is_independent_of_redraw() {
        let cmd = Command::<i32>::none().without_redraw();

        assert!(cmd.is_none());
        assert!(!cmd.requests_redraw());
    }

    #[test]
    fn test_timeout_preserves_runtime_directives_and_streamless_shape() {
        let cmd = Command::<i32>::none()
            .without_redraw()
            .timeout(Duration::from_secs(1), || 99);

        assert!(cmd.is_none());
        assert!(!cmd.requests_redraw());
    }

    #[tokio::test(start_paused = true)]
    async fn test_timeout_composes_with_map_on_either_side() {
        let before = Command::future(pending::<i32>())
            .map(|value| value.to_string())
            .timeout(Duration::from_secs(1), || "before".to_string());
        let after = Command::future(pending::<i32>())
            .timeout(Duration::from_secs(1), || 99)
            .map(|value| value.to_string());
        let command = Command::batch([before, after]);
        let mut stream = command.into_stream().expect("stream should exist");

        assert!(futures::poll!(stream.next()).is_pending());
        advance(Duration::from_secs(1)).await;

        let mut messages = Vec::new();
        while let Some(action) = stream.next().await {
            if let Action::Message(message) = action {
                messages.push(message);
            }
        }
        messages.sort();
        assert_eq!(messages, vec!["99", "before"]);
    }

    #[test]
    fn test_into_runtime_parts_none_requests_redraw_without_stream() {
        let parts = Command::<i32>::none().into_runtime_parts();

        assert!(parts.requests_redraw());
        assert!(parts.into_stream().is_none());
    }

    #[test]
    fn test_into_runtime_parts_without_redraw_has_no_stream() {
        let parts = Command::<i32>::none().without_redraw().into_runtime_parts();

        assert!(!parts.requests_redraw());
        assert!(parts.into_stream().is_none());
    }

    #[tokio::test]
    async fn test_into_runtime_parts_message_requests_redraw_and_yields_message() {
        let parts = Command::message(42).into_runtime_parts();

        assert!(parts.requests_redraw());

        let mut stream = parts.into_stream().expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(msg) if msg == 42));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_into_runtime_parts_quit_yields_quit() {
        let parts = Command::<i32>::quit().into_runtime_parts();

        assert!(parts.requests_redraw());

        let mut stream = parts.into_stream().expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Quit));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_into_runtime_parts_batch_preserves_redraw_and_stream() {
        let cmd = Command::batch(vec![
            Command::message(2).without_redraw(),
            Command::none().without_redraw(),
            Command::message(1).without_redraw(),
        ]);
        let parts = cmd.into_runtime_parts();

        assert!(!parts.requests_redraw());

        let mut stream = parts.into_stream().expect("stream should exist");
        let mut results = vec![];

        while let Some(action) = stream.next().await {
            match action {
                Action::Message(msg) => results.push(msg),
                Action::Quit => break,
            }
        }

        results.sort_unstable();
        assert_eq!(results, vec![1, 2]);
    }

    #[tokio::test]
    async fn test_map_preserves_directives_through_runtime_parts() {
        let parts = Command::message(21)
            .without_redraw()
            .map(|value| value * 2)
            .into_runtime_parts();

        assert!(!parts.requests_redraw());

        let mut stream = parts.into_stream().expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(msg) if msg == 42));
    }

    #[tokio::test]
    async fn test_batch_empty() {
        let cmd: Command<i32> = Command::batch(vec![]);
        assert!(cmd.is_none());
    }

    #[tokio::test]
    async fn test_batch_single_command() {
        let cmd1 = Command::future(async { 1 });
        let cmd = Command::batch(vec![cmd1]);

        let mut stream = cmd.into_stream().expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(msg) if msg == 1));
    }

    #[tokio::test]
    async fn test_batch_multiple_commands() {
        let cmd1 = Command::future(async { 1 });
        let cmd2 = Command::future(async { 2 });
        let cmd3 = Command::future(async { 3 });

        let cmd = Command::batch(vec![cmd1, cmd2, cmd3]);

        let mut stream = cmd.into_stream().expect("stream should exist");
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

        let mut stream = cmd.into_stream().expect("stream should exist");
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
        assert!(cmd.is_none());
    }

    #[tokio::test]
    async fn test_batch_with_quit_action() {
        let cmd1 = Command::future(async { 1 });
        let cmd2 = Command::quit();
        let cmd3 = Command::future(async { 3 });

        let cmd = Command::batch(vec![cmd1, cmd2, cmd3]);

        let mut stream = cmd.into_stream().expect("stream should exist");
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

        let mut stream = cmd.into_stream().expect("stream should exist");
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

        let mut stream = cmd.into_stream().expect("stream should exist");
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

        let mut stream = cmd.into_stream().expect("stream should exist");
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

        let mut stream = cmd.into_stream().expect("stream should exist");
        let result = stream.next().await;

        assert!(result.is_none(), "empty stream should produce no messages");
    }

    #[tokio::test]
    async fn test_none() {
        let cmd: Command<i32> = Command::none();
        assert!(cmd.is_none());
    }

    #[tokio::test]
    async fn test_future() {
        let cmd = Command::future(async { 42 });

        let mut stream = cmd.into_stream().expect("stream should exist");
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

        let mut stream = cmd.into_stream().expect("stream should exist");
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

        let mut stream = cmd.into_stream().expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(msg) if msg == "Got: success"));
    }

    #[tokio::test]
    async fn test_message() {
        let cmd = Command::message(42);

        let mut stream = cmd.into_stream().expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(msg) if msg == 42));

        // Stream should be exhausted
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_message_with_string() {
        let cmd = Command::message("hello".to_string());

        let mut stream = cmd.into_stream().expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(msg) if msg == "hello"));
    }

    #[tokio::test]
    async fn test_effect_with_message() {
        let cmd = Command::effect(Action::Message(100));

        let mut stream = cmd.into_stream().expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(msg) if msg == 100));
    }

    #[tokio::test]
    async fn test_effect_with_quit() {
        let cmd: Command<i32> = Command::effect(Action::Quit);

        let mut stream = cmd.into_stream().expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Quit));
    }

    #[tokio::test]
    async fn test_stream_empty() {
        let input_stream = stream::iter(Vec::<i32>::new());
        let cmd = Command::stream(input_stream);

        let mut stream = cmd.into_stream().expect("stream should exist");
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

        let mut stream = final_batch.into_stream().expect("stream should exist");
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

        let mut stream = cmd.into_stream().expect("stream should exist");
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

        let mut stream = cmd.into_stream().expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(msg) if msg == 42));

        // Test error case
        let cmd = Command::perform(may_fail(true), |result| result.unwrap_or(-1));

        let mut stream = cmd.into_stream().expect("stream should exist");
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

        let mut stream = cmd.into_stream().expect("stream should exist");
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

        let mut stream = mapped.into_stream().expect("stream should exist");
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

        let mut stream = mapped.into_stream().expect("stream should exist");
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

        let mut stream = mapped.into_stream().expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(Message::Success(ref s)) if s == "data"));
    }

    #[tokio::test]
    async fn test_map_none() {
        let cmd: Command<i32> = Command::none();
        let mapped = cmd.map(|x| x * 2);

        assert!(mapped.is_none());
    }

    #[tokio::test]
    async fn test_map_preserves_quit() {
        let cmd: Command<i32> = Command::quit();
        let mapped = cmd.map(|x| x * 2);

        let mut stream = mapped.into_stream().expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Quit));
    }

    #[test]
    fn test_is_none() {
        let cmd: Command<i32> = Command::none();
        assert!(cmd.is_none());
        assert!(!cmd.is_some());
    }

    #[test]
    fn test_is_some() {
        let cmd = Command::perform(async { 42 }, |x| x);
        assert!(cmd.is_some());
        assert!(!cmd.is_none());
    }

    #[test]
    fn test_is_some_with_future() {
        let cmd = Command::future(async { 100 });
        assert!(cmd.is_some());
        assert!(!cmd.is_none());
    }

    #[test]
    fn test_is_some_with_message() {
        let cmd = Command::message("test");
        assert!(cmd.is_some());
        assert!(!cmd.is_none());
    }

    #[test]
    fn test_is_some_with_quit() {
        let cmd: Command<i32> = Command::quit();
        assert!(cmd.is_some());
        assert!(!cmd.is_none());
    }

    #[test]
    fn test_is_none_after_batch_empty() {
        let cmd: Command<i32> = Command::batch(vec![]);
        assert!(cmd.is_none());
        assert!(!cmd.is_some());
    }

    #[test]
    fn test_is_none_after_batch_all_none() {
        let cmd = Command::batch(vec![
            Command::<i32>::none(),
            Command::<i32>::none(),
            Command::<i32>::none(),
        ]);
        assert!(cmd.is_none());
        assert!(!cmd.is_some());
    }

    #[test]
    fn test_is_some_after_batch_with_some() {
        let cmd = Command::batch(vec![
            Command::<i32>::none(),
            Command::future(async { 42 }),
            Command::<i32>::none(),
        ]);
        assert!(cmd.is_some());
        assert!(!cmd.is_none());
    }

    #[test]
    fn test_is_none_after_map_none() {
        let cmd: Command<i32> = Command::none();
        let mapped = cmd.map(|x| x * 2);
        assert!(mapped.is_none());
        assert!(!mapped.is_some());
    }

    #[test]
    fn test_is_some_after_map_some() {
        let cmd = Command::future(async { 42 });
        let mapped = cmd.map(|x| x * 2);
        assert!(mapped.is_some());
        assert!(!mapped.is_none());
    }

    #[test]
    fn test_batch_flattens_nested_batches_into_flat_leaves() {
        let batch1 = Command::batch(vec![
            Command::future(async { 1 }),
            Command::future(async { 2 }),
        ]);
        let batch2 = Command::batch(vec![
            Command::future(async { 3 }),
            Command::future(async { 4 }),
        ]);
        let cmd = Command::batch(vec![batch1, batch2, Command::future(async { 5 })]);

        // batch(batch(a, b), batch(c, d), e) collapses to a single flat leaf
        // sequence [a, b, c, d, e] rather than a nested structure.
        assert_eq!(cmd.effect.leaf_count(), 5);
    }

    #[test]
    fn test_batch_drops_streamless_children_from_leaves() {
        let cmd = Command::batch(vec![
            Command::future(async { 1 }),
            Command::none(),
            Command::none().without_redraw(),
            Command::future(async { 2 }),
        ]);

        // Stream-less children contribute no leaves even though their redraw
        // directives are still folded in.
        assert_eq!(cmd.effect.leaf_count(), 2);
    }

    #[test]
    fn test_map_over_batch_preserves_leaf_count() {
        let cmd = Command::batch(vec![
            Command::future(async { 1 }),
            Command::future(async { 2 }),
            Command::future(async { 3 }),
        ])
        .map(|value| value * 10);

        assert_eq!(cmd.effect.leaf_count(), 3);
    }

    // `map` promises to accept any `Fn + Send` mapper without also requiring
    // `Sync`. On the multi-leaf batch path the mapper is shared across leaves,
    // and it is `Arc<Mutex<F>>` (not `Arc<F>`, which would demand `F: Sync`)
    // that upholds that contract. This test pins the contract down: a mapper
    // that is `Send` but not `Sync` (it captures a `Cell`) must still compile
    // and run through a batched `map`, so a regression to `Arc<F>` fails to
    // build.
    #[tokio::test]
    async fn test_batch_map_accepts_send_non_sync_mapper() {
        fn assert_send<F: Send>(_: &F) {}

        // `Cell<i32>` is `Send` but not `Sync`, so this closure is too.
        let offset = Cell::new(10);
        let mapper = move |value: i32| value + offset.get();
        assert_send(&mapper);

        let cmd = Command::batch(vec![
            Command::future(async { 1 }),
            Command::future(async { 2 }),
        ])
        .map(mapper);
        assert_eq!(cmd.effect.leaf_count(), 2);

        let mut stream = cmd.into_stream().expect("stream should exist");
        let mut results = vec![];
        while let Some(action) = stream.next().await {
            match action {
                Action::Message(msg) => results.push(msg),
                Action::Quit => break,
            }
        }

        results.sort_unstable();
        assert_eq!(results, vec![11, 12]);
    }

    // Directives (`without_redraw`) travel a path independent of the effect
    // leaves, so exercise the redraw flag across the matrix of effect presence
    // and `map` application to pin the two concerns apart.
    #[test]
    fn test_redraw_preserved_across_effect_and_map_matrix() {
        // Command with a stream: redraw survives map, without_redraw survives map.
        assert!(
            Command::future(async { 1 })
                .map(|v| v * 2)
                .requests_redraw()
        );
        assert!(
            !Command::future(async { 1 })
                .without_redraw()
                .map(|v| v * 2)
                .requests_redraw()
        );

        // stream-less command: same, and it stays stream-less.
        let cmd = Command::<i32>::none().map(|v| v * 2);
        assert!(cmd.is_none());
        assert!(cmd.requests_redraw());
        let cmd = Command::<i32>::none().without_redraw().map(|v| v * 2);
        assert!(cmd.is_none());
        assert!(!cmd.requests_redraw());
    }

    #[test]
    fn test_batch_redraw_matrix_over_effect_and_map() {
        // Mixed children: OR over redraw flags holds regardless of effects.
        let cmd = Command::batch(vec![
            Command::future(async { 1 }).without_redraw(),
            Command::none(),
        ]);
        assert!(cmd.is_some());
        assert!(cmd.requests_redraw());

        // All opted out, both with and without a stream: stays opted out.
        let cmd = Command::batch(vec![
            Command::future(async { 1 }).without_redraw(),
            Command::none().without_redraw(),
        ]);
        assert!(cmd.is_some());
        assert!(!cmd.requests_redraw());

        // Mapping the batch does not disturb the folded redraw directive.
        let cmd = Command::batch(vec![
            Command::future(async { 1 }).without_redraw(),
            Command::future(async { 2 }).without_redraw(),
        ])
        .map(|v| v * 2);
        assert!(!cmd.requests_redraw());

        let cmd = Command::batch(vec![
            Command::future(async { 1 }),
            Command::future(async { 2 }).without_redraw(),
        ])
        .map(|v| v * 2);
        assert!(cmd.requests_redraw());
    }
}
