use std::{
    error::Error,
    fmt::{self, Display, Formatter},
    future::Future,
    num::NonZeroUsize,
    time::Duration,
};
use tokio::time::sleep;

/// Configuration for a retrying command.
///
/// `max_attempts` includes the first execution. The default backoff performs
/// the next attempt immediately.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetryPolicy {
    max_attempts: NonZeroUsize,
    backoff: RetryBackoff,
}

impl RetryPolicy {
    /// Create a retry policy with no delay between attempts.
    #[must_use]
    pub const fn new(max_attempts: NonZeroUsize) -> Self {
        Self {
            max_attempts,
            backoff: RetryBackoff::None,
        }
    }

    /// Return the maximum number of executions, including the first attempt.
    #[must_use]
    pub const fn max_attempts(&self) -> NonZeroUsize {
        self.max_attempts
    }

    /// Return the delay strategy used between retriable failures.
    #[must_use]
    pub const fn backoff(&self) -> &RetryBackoff {
        &self.backoff
    }

    /// Replace the delay strategy.
    #[must_use = "with_backoff consumes the policy and returns the modified value"]
    pub const fn with_backoff(mut self, backoff: RetryBackoff) -> Self {
        self.backoff = backoff;
        self
    }

    /// Use the same delay before every subsequent attempt.
    #[must_use = "with_fixed_backoff consumes the policy and returns the modified value"]
    pub const fn with_fixed_backoff(self, delay: Duration) -> Self {
        self.with_backoff(RetryBackoff::fixed(delay))
    }
}

/// Delay strategy applied between retry attempts.
///
/// This enum and its field-bearing variants are non-exhaustive. Downstream
/// matches must include `..` in variant patterns and a wildcard arm for future
/// strategies.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RetryBackoff {
    /// Start the next attempt without sleeping.
    None,

    /// Wait the same duration after every retriable failure.
    #[non_exhaustive]
    Fixed {
        /// Delay before the next attempt.
        delay: Duration,
    },
}

impl RetryBackoff {
    /// Create a strategy with no delay between attempts.
    #[must_use]
    pub const fn none() -> Self {
        Self::None
    }

    /// Create a fixed-delay strategy.
    #[must_use]
    pub const fn fixed(delay: Duration) -> Self {
        Self::Fixed { delay }
    }
}

/// Information about the currently executing retry attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct RetryContext {
    attempt: NonZeroUsize,
}

impl RetryContext {
    pub(crate) const fn new(attempt: NonZeroUsize) -> Self {
        Self { attempt }
    }

    /// Return the 1-based attempt number.
    #[must_use]
    pub const fn attempt(&self) -> NonZeroUsize {
        self.attempt
    }
}

/// The final failure of a retrying command.
#[derive(Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct RetryError<E> {
    attempts: NonZeroUsize,
    last_error: E,
    reason: RetryStopReason,
}

impl<E> RetryError<E> {
    /// Return the number of attempts that completed.
    #[must_use]
    pub const fn attempts(&self) -> NonZeroUsize {
        self.attempts
    }

    /// Borrow the error returned by the final attempt.
    #[must_use]
    pub const fn last_error(&self) -> &E {
        &self.last_error
    }

    /// Return why no further attempt was started.
    #[must_use]
    pub const fn reason(&self) -> RetryStopReason {
        self.reason
    }

    /// Consume this retry error and return the final operation error.
    #[must_use]
    pub fn into_last_error(self) -> E {
        self.last_error
    }
}

impl<E: Display> Display for RetryError<E> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let reason = match self.reason {
            RetryStopReason::Exhausted => "retry policy exhausted",
            RetryStopReason::StoppedByPredicate => "retry stopped by predicate",
        };

        write!(
            f,
            "{reason} after {} attempt(s): {}",
            self.attempts, self.last_error
        )
    }
}

impl<E: Error + 'static> Error for RetryError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.last_error)
    }
}

/// Why a failed retrying command did not start another attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RetryStopReason {
    /// The final allowed attempt failed.
    Exhausted,

    /// The retry predicate rejected an error while attempts remained.
    StoppedByPredicate,
}

pub(super) async fn run_retry<A, E, Fut, Op, P>(
    policy: RetryPolicy,
    mut operation: Op,
    mut should_retry: P,
) -> Result<A, RetryError<E>>
where
    Fut: Future<Output = Result<A, E>>,
    Op: FnMut(RetryContext) -> Fut,
    P: FnMut(&E, RetryContext) -> bool,
{
    let mut attempt = NonZeroUsize::MIN;

    loop {
        let context = RetryContext::new(attempt);

        match operation(context).await {
            Ok(value) => return Ok(value),
            Err(error) if attempt == policy.max_attempts => {
                return Err(RetryError {
                    attempts: attempt,
                    last_error: error,
                    reason: RetryStopReason::Exhausted,
                });
            }
            Err(error) if !should_retry(&error, context) => {
                return Err(RetryError {
                    attempts: attempt,
                    last_error: error,
                    reason: RetryStopReason::StoppedByPredicate,
                });
            }
            Err(_) => {
                match policy.backoff() {
                    RetryBackoff::None => {}
                    RetryBackoff::Fixed { delay, .. } => sleep(*delay).await,
                }

                // The final-attempt branch above guarantees `attempt < max`,
                // so incrementing cannot overflow and cannot produce zero.
                attempt = NonZeroUsize::new(attempt.get() + 1)
                    .expect("attempt remains non-zero and within max_attempts");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use futures::{StreamExt, poll};
    use tokio::time::advance;

    use crate::command::{Action, Command};

    fn policy(max_attempts: usize) -> RetryPolicy {
        RetryPolicy::new(NonZeroUsize::new(max_attempts).expect("max_attempts must be non-zero"))
    }

    async fn final_result<A, E>(
        command: Command<Result<A, RetryError<E>>>,
    ) -> Result<A, RetryError<E>>
    where
        A: Send + 'static,
        E: Send + 'static,
    {
        let mut stream = command.into_stream().expect("stream should exist");
        let action = stream.next().await.expect("retry should emit one message");
        assert!(stream.next().await.is_none());

        match action {
            Action::Message(result) => result,
            Action::Quit => unreachable!("retry commands never emit Quit"),
        }
    }

    #[tokio::test]
    async fn retry_succeeds_on_the_first_attempt_and_emits_once() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_by_operation = Arc::clone(&attempts);
        let command = Command::retry(
            policy(3),
            move |context| {
                let attempts = Arc::clone(&attempts_by_operation);
                async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(context.attempt(), NonZeroUsize::MIN);
                    Ok::<_, &'static str>(42)
                }
            },
            |result| result,
        );

        assert_eq!(final_result(command.into()).await, Ok(42));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retry_recreates_the_operation_until_success() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_by_operation = Arc::clone(&attempts);
        let command = Command::retry(
            policy(3),
            move |context| {
                let attempt = attempts_by_operation.fetch_add(1, Ordering::SeqCst) + 1;
                async move {
                    assert_eq!(context.attempt().get(), attempt);
                    if attempt == 2 {
                        Ok(42)
                    } else {
                        Err("transient")
                    }
                }
            },
            |result| result,
        );

        assert_eq!(final_result(command.into()).await, Ok(42));
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn final_attempt_failure_is_exhausted_without_calling_predicate() {
        let predicate_calls = Arc::new(AtomicUsize::new(0));
        let calls_by_predicate = Arc::clone(&predicate_calls);
        let command = Command::retry_if(
            policy(1),
            |_| async { Err::<i32, _>("failed") },
            move |_, _| {
                calls_by_predicate.fetch_add(1, Ordering::SeqCst);
                true
            },
            |result| result,
        );

        let error = final_result(command.into())
            .await
            .expect_err("attempt should fail");
        assert_eq!(error.attempts().get(), 1);
        assert_eq!(error.reason(), RetryStopReason::Exhausted);
        assert_eq!(predicate_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn predicate_stop_occurs_only_while_attempts_remain() {
        let command = Command::retry_if(
            policy(3),
            |_| async { Err::<i32, _>("permanent") },
            |_, context| {
                assert_eq!(context.attempt().get(), 1);
                false
            },
            |result| result,
        );

        let error = final_result(command.into())
            .await
            .expect_err("attempt should fail");
        assert_eq!(error.attempts().get(), 1);
        assert_eq!(error.reason(), RetryStopReason::StoppedByPredicate);
    }

    #[tokio::test(start_paused = true)]
    async fn no_backoff_starts_the_next_attempt_without_sleeping() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_by_operation = Arc::clone(&attempts);
        let command = Command::retry(
            policy(2),
            move |_| {
                let attempt = attempts_by_operation.fetch_add(1, Ordering::SeqCst) + 1;
                async move {
                    if attempt == 2 {
                        Ok(42)
                    } else {
                        Err("transient")
                    }
                }
            },
            |result| result,
        );

        assert_eq!(final_result(command.into()).await, Ok(42));
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn fixed_backoff_waits_before_every_next_attempt() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let attempts_by_operation = Arc::clone(&attempts);
        let command = Command::retry(
            policy(3).with_fixed_backoff(Duration::from_secs(2)),
            move |_| {
                let attempt = attempts_by_operation.fetch_add(1, Ordering::SeqCst) + 1;
                async move {
                    if attempt == 3 {
                        Ok(42)
                    } else {
                        Err("transient")
                    }
                }
            },
            |result| result,
        );
        let mut stream = command
            .into_command()
            .into_stream()
            .expect("stream should exist");

        assert!(poll!(stream.next()).is_pending());
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

        advance(Duration::from_secs(2)).await;
        assert!(poll!(stream.next()).is_pending());
        assert_eq!(attempts.load(Ordering::SeqCst), 2);

        advance(Duration::from_secs(2)).await;
        assert!(matches!(stream.next().await, Some(Action::Message(Ok(42)))));
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        assert!(stream.next().await.is_none());
    }

    #[test]
    fn retry_policy_preserves_max_attempts_in_builders() {
        let max_attempts = NonZeroUsize::new(3).expect("non-zero");
        let policy = RetryPolicy::new(max_attempts);
        assert_eq!(policy.max_attempts(), max_attempts);
        assert_eq!(policy.backoff(), &RetryBackoff::None);

        let policy = policy.with_fixed_backoff(Duration::from_millis(50));
        assert_eq!(policy.max_attempts(), max_attempts);
        assert_eq!(
            policy.backoff(),
            &RetryBackoff::Fixed {
                delay: Duration::from_millis(50)
            }
        );
        assert_eq!(RetryBackoff::none(), RetryBackoff::None);
        assert_eq!(
            RetryBackoff::fixed(Duration::from_secs(1)),
            RetryBackoff::Fixed {
                delay: Duration::from_secs(1)
            }
        );
    }

    #[test]
    fn retry_error_exposes_last_error_through_display_and_source() {
        #[derive(Debug, Eq, PartialEq)]
        struct TestError(&'static str);

        impl Display for TestError {
            fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
                f.write_str(self.0)
            }
        }

        impl Error for TestError {}

        let error = RetryError {
            attempts: NonZeroUsize::new(2).expect("non-zero"),
            last_error: TestError("boom"),
            reason: RetryStopReason::StoppedByPredicate,
        };

        assert_eq!(error.attempts().get(), 2);
        assert_eq!(error.last_error(), &TestError("boom"));
        assert_eq!(error.reason(), RetryStopReason::StoppedByPredicate);
        assert!(error.to_string().contains("boom"));
        assert_eq!(
            error.source().map(ToString::to_string).as_deref(),
            Some("boom")
        );
        assert_eq!(error.into_last_error(), TestError("boom"));
    }

    #[tokio::test]
    async fn retry_composes_as_a_single_leaf_command() {
        let retry = Command::retry(
            policy(1),
            |_| async { Ok::<_, &'static str>(20) },
            |result| result.expect("operation should succeed"),
        )
        .without_redraw()
        .map(|value| value * 2);

        let retry = retry.into_command();
        assert_eq!(retry.effect.leaf_count(), 1);
        assert!(!retry.requests_redraw());

        let command = Command::batch([retry, Command::message(1).without_redraw().into()]);
        assert_eq!(command.effect.leaf_count(), 2);
        assert!(!command.requests_redraw());

        let mut stream = command.into_stream().expect("stream should exist");
        let mut messages = Vec::new();
        let mut quit = false;
        while let Some(action) = stream.next().await {
            match action {
                Action::Message(message) => messages.push(message),
                Action::Quit => quit = true,
            }
        }
        messages.sort_unstable();
        assert_eq!(messages, vec![1, 40]);
        assert!(!quit);
    }

    #[tokio::test]
    async fn retry_predicate_may_retain_mutable_local_state() {
        let mut predicate_calls = 0;
        let command = Command::retry_if(
            policy(4),
            |_| async { Err::<i32, _>("failed") },
            move |_, _| {
                predicate_calls += 1;
                predicate_calls < 2
            },
            |result| result,
        );

        let error = final_result(command.into())
            .await
            .expect_err("attempt should fail");
        assert_eq!(error.attempts().get(), 2);
        assert_eq!(error.reason(), RetryStopReason::StoppedByPredicate);
    }
}
