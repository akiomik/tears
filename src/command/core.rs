//! The `Command` type itself, split out from `command.rs` so the parent
//! module can stay `pub` (hosting opt-in vocabulary like `RetryPolicy`)
//! while closing the `command::Command` path. See `runtime::frame_rate` for
//! the same pattern applied to `Runtime`'s scheduling input.

use std::hash::Hash;
#[cfg(test)]
use std::hash::Hasher;
use std::time::Duration;

#[cfg(test)]
use futures::stream::BoxStream;
use futures::{FutureExt, Stream, StreamExt, stream};

use crate::structural_key::{ScopePath, StructuralKey};

use super::Action;
use super::cancellation::{CommandCancellation, CommandId};
use super::cleanup::CleanupRegistration;
use super::effect::{Effect, Leaf};
use super::effect_command::EffectCommand;
use super::retry::{self, RetryContext, RetryError, RetryPolicy};
use super::runtime_directives::RuntimeDirectives;
use super::runtime_parts::RuntimeCommandParts;

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
    // `pub(super)` so `command::retry`'s tests (a sibling of this module,
    // both under `command`) can assert on leaf shape via
    // `Effect::leaf_count()` without a dedicated accessor.
    pub(super) effect: Effect<Msg>,
    directives: RuntimeDirectives,
    cancellation: CommandCancellation,
    // Scope prefixes to tear down. A carrier, not a stream: teardown is
    // command metadata that applies in the cancel phase, so it travels
    // beside `cancellation` rather than as an effect leaf (RFC 0013 §3.3,
    // RFC 0014 §3.4).
    teardowns: Vec<ScopePath>,
    // Cleanup finalizers to arm. A carrier for the same reason teardown is
    // one, on the other side of the phase order: a registration applies in
    // the spawn phase and starts nothing there (RFC 0014 §3.4, §4.4).
    cleanups: Vec<CleanupRegistration>,
}

impl<Msg: Send + 'static> Command<Msg> {
    const fn with_effect(effect: Effect<Msg>) -> Self {
        Self {
            effect,
            directives: RuntimeDirectives::DEFAULT,
            cancellation: CommandCancellation {
                cancels: Vec::new(),
            },
            teardowns: Vec::new(),
            cleanups: Vec::new(),
        }
    }

    /// The command one [`EffectCommand`] carrier makes — the whole body of
    /// the one-way conversion between them.
    ///
    /// The key is written twice on purpose, and only until the switch. A
    /// carrier's key is the reading the kernel lowers from; the command-level
    /// `cancellation.key` is the one the superseded runtime reads, and it is
    /// still the authoritative production path. With exactly one carrier the
    /// two readings cannot disagree, which is what lets both consumers stand
    /// while only one of them is live. The command-level half goes when the
    /// runtime that reads it does.
    pub(super) fn from_carrier(leaf: Leaf<Msg>, directives: RuntimeDirectives) -> Self {
        Self {
            cancellation: CommandCancellation::default(),
            directives,
            effect: Effect::from_leaf(leaf),
            teardowns: Vec::new(),
            cleanups: Vec::new(),
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
        Self::with_effect(Effect::none())
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
    /// let cmd: Command<i32> = Command::perform(async { 42 }, |x| x).into();
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
    /// let cmd: Command<i32> = Command::perform(async { 42 }, |x| x).into();
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
    pub(crate) const fn requests_redraw(&self) -> bool {
        self.directives.requests_redraw()
    }

    #[cfg(test)]
    pub(crate) fn into_stream(self) -> Option<BoxStream<'static, Action<Msg>>> {
        self.effect.into_stream()
    }

    /// Decompose this command into the runtime-facing parts that must be
    /// observed together.
    pub(crate) fn into_runtime_parts(self) -> RuntimeCommandParts<Msg> {
        RuntimeCommandParts::new(
            self.directives,
            self.effect.into_leaves(),
            self.cancellation,
            self.teardowns,
            self.cleanups,
        )
    }

    /// Declare that the update returning this command did not change the
    /// visible view, so the runtime may skip the redraw it would otherwise
    /// perform.
    ///
    /// Side effects still run. This is an optimization hint rather than a
    /// guarantee: the runtime may still redraw for other reasons, such as an
    /// initial frame or another message in the same batch.
    #[must_use = "without_redraw consumes the command and returns the modified value"]
    pub const fn without_redraw(mut self) -> Self {
        self.directives = self.directives.without_redraw();
        self
    }

    /// Qualifies this command's lifecycle ids with one structural scope
    /// segment, expressing that this command belongs to a distinct child
    /// composition boundary (see RFC 0005 section 4.3).
    ///
    /// `scoped` prepends `scope` to every carrier's spawn key (attached by
    /// [`EffectCommand::cancellable`] or
    /// [`EffectCommand::cancellable_with`] before the carrier became part of
    /// this command) and to every explicit cancel id already present, for
    /// example from [`Command::cancel`] or a [`Command::batch`] child. It
    /// does not touch the effect stream, message mapping, redraw directive,
    /// timeout, retry wrapper, or output.
    ///
    /// [`Command::none().scoped(scope)`](Command::none) is lifecycle-inert:
    /// there is no spawn key or explicit cancel to qualify.
    ///
    /// # Ordering
    ///
    /// `scoped` is a boundary operation over lifecycle metadata already
    /// present at the call site, not a persistent mode inherited by later
    /// modifiers. The ordering examples live on
    /// [`EffectCommand::cancellable_with`], where the two keying modifiers
    /// are: `work.cancellable(id).scoped(scope)` scopes `id`, while
    /// `work.scoped(scope).cancellable(id)` attaches `id` as a new,
    /// unscoped, root-global key. No diagnostic is emitted for the second
    /// order, because a later root-global key can be an intentional
    /// composition (a pane-scoped effect participating in an
    /// application-wide slot), not only a mistake.
    ///
    /// # Examples
    ///
    /// ```
    /// use tears::prelude::*;
    /// use tears::command::CommandId;
    ///
    /// let cmd: Command<i32> = Command::cancel(CommandId::new("load")).scoped("pane-1");
    /// ```
    #[must_use = "scoped consumes the command and returns the modified value"]
    pub fn scoped<Scope>(mut self, scope: Scope) -> Self
    where
        Scope: Eq + Hash + Send + Sync + 'static,
    {
        // One erasure, shared by every carrier at this boundary, so the
        // scope type itself need not be `Clone` (RFC 0005 §8.1).
        let segment = StructuralKey::new(scope);

        self.cancellation.cancels = self
            .cancellation
            .cancels
            .into_iter()
            .map(|id| id.scoped_with(segment.clone()))
            .collect();

        self.teardowns = self
            .teardowns
            .into_iter()
            .map(|prefix| prefix.prefixed_key(segment.clone()))
            .collect();

        self.cleanups = self
            .cleanups
            .into_iter()
            .map(|registration| registration.scoped_with(segment.clone()))
            .collect();

        // Per-carrier keys and scopes. The key half reaches a batched
        // child's own key; the scope half is what an *unkeyed* carrier
        // gets, so a prefix teardown can select it (RFC 0014 INV-RC7).
        self.effect.apply_scope(&segment);

        self
    }

    /// Cancels the current deliverable command for `id` without replacing it.
    ///
    /// Cancellation is idempotent and drops buffered messages and quit requests
    /// in addition to aborting running work. Like other constructors, this
    /// requests a redraw unless followed by [`Command::without_redraw`].
    pub fn cancel(id: CommandId) -> Self {
        Self {
            cancellation: CommandCancellation { cancels: vec![id] },
            ..Self::none()
        }
    }

    /// Tears down every runtime-owned run under the structural scope
    /// prefix `scope`, and consumes that prefix's unfired cleanup
    /// registrations.
    ///
    /// This is the manual primitive behind RFC 0013's scope teardown: it
    /// carries no effect stream, so [`Command::is_none`] stays `true`, and
    /// it composes through [`Command::scoped`] exactly as an explicit
    /// cancel id does (RFC 0013 INV-ST2).
    ///
    /// Crate-private for now: the runtime that applies it is the kernel of
    /// RFC 0014, and a public constructor whose effect the current runtime
    /// would accept and ignore is precisely the silent mismatch RFC 0007
    /// INV-C5 prohibits.
    pub fn teardown<Scope>(scope: Scope) -> Self
    where
        Scope: Eq + Hash + Send + Sync + 'static,
    {
        Self {
            teardowns: vec![ScopePath::empty().prefixed(scope)],
            ..Self::none()
        }
    }

    /// Takes `other`'s teardown prefixes onto this command and **nothing
    /// else** — not its effect, not its directives, not its cancellation
    /// metadata, not its cleanup registrations.
    ///
    /// This is aggregation of an already-originated teardown, which RFC 0013
    /// §7.2's origination review names as a free transformation: the entry
    /// still comes from a [`Command::teardown`] call, and there is no route
    /// here from a raw prefix. A `debug_assert` holds `other` to that shape
    /// so this cannot quietly become a general-purpose merge.
    ///
    /// The one caller is a combinator's journal drain, which has to put a
    /// removal's teardown on a command the application returned.
    /// [`Command::batch`] would be wrong there twice over: it folds the
    /// redraw directive across its children, so an update that returned
    /// [`Command::without_redraw`] would silently regain its redraw, and it
    /// warns about a child spawn key for a command the boundary is only
    /// passing through. A boundary adds identity carriers and nothing else
    /// (RFC 0014 §2.5).
    pub(crate) fn merging_teardowns(mut self, other: Self) -> Self {
        debug_assert!(
            other.is_none()
                && other.directives == RuntimeDirectives::DEFAULT
                && other.cleanups.is_empty()
                && other.cancellation.cancels.is_empty(),
            "merging_teardowns aggregates teardown entries only; an effect, a redraw directive, a \
             cleanup registration, a spawn key, or an explicit cancel on `other` would be dropped \
             silently"
        );
        self.teardowns.extend(other.teardowns);
        self
    }

    /// Registers `finalizer` to run when a teardown selects the structural
    /// scope at this call boundary.
    ///
    /// The finalizer is an ordinary future under this type's existing effect
    /// bounds, with `Output = ()` rather than a message because a cleanup
    /// run produces none (RFC 0014 §4.4). It runs **at most once**, started
    /// at the application point of the teardown that consumes it, and is not
    /// re-fired by a later teardown of the same prefix. Termination is not a
    /// teardown: it discards whatever is still unfired and cancels whatever
    /// is running.
    ///
    /// Like [`Command::teardown`] this carries no effect stream, so
    /// [`Command::is_none`] stays `true`, and it composes through
    /// [`Command::scoped`] exactly as a teardown prefix does (RFC 0013
    /// INV-ST2, RFC 0005 INV-18's coverage). Its external side effects are
    /// its whole purpose and are not restricted; what is closed is the path
    /// back into the runtime.
    ///
    /// Crate-private for now, for the reason [`Command::teardown`] is: the
    /// runtime that starts a finalizer is the kernel of RFC 0014, and a
    /// public constructor the current runtime would accept and ignore is the
    /// silent mismatch RFC 0007 INV-C5 prohibits.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the kernel that starts a registered finalizer is here, but no non-test build \
                      can reach it until the entry point is switched over to it"
        )
    )]
    pub(crate) fn on_teardown(finalizer: impl Future<Output = ()> + Send + 'static) -> Self {
        Self {
            cleanups: vec![CleanupRegistration::new(finalizer)],
            ..Self::none()
        }
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
    #[must_use = "timeout consumes the command and returns the modified value"]
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
    /// use std::num::NonZeroUsize;
    /// use tears::Command;
    /// use tears::command::{RetryError, RetryPolicy};
    ///
    /// enum Message {
    ///     Loaded(Result<String, RetryError<&'static str>>),
    /// }
    ///
    /// let policy = RetryPolicy::new(NonZeroUsize::new(3).expect("non-zero"));
    /// let command = Command::retry(
    ///     policy,
    ///     |_| async { Ok::<_, &'static str>("data".to_string()) },
    ///     Message::Loaded,
    /// );
    /// ```
    pub fn retry<A, E, Fut, Op, F>(policy: RetryPolicy, operation: Op, f: F) -> EffectCommand<Msg>
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
    /// produces [`RetryStopReason::StoppedByPredicate`](crate::command::RetryStopReason::StoppedByPredicate);
    /// an error on the final attempt produces
    /// [`RetryStopReason::Exhausted`](crate::command::RetryStopReason::Exhausted)
    /// without invoking the predicate.
    ///
    /// The operation may run up to `policy.max_attempts()` times, so callers
    /// are responsible for ensuring that repetition is safe.
    pub fn retry_if<A, E, Fut, Op, P, F>(
        policy: RetryPolicy,
        operation: Op,
        should_retry: P,
        f: F,
    ) -> EffectCommand<Msg>
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
    ) -> EffectCommand<Msg> {
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
    pub fn future(future: impl Future<Output = Msg> + Send + 'static) -> EffectCommand<Msg> {
        EffectCommand::from_action_stream(future.into_stream().map(Action::Message).boxed())
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
    pub fn message(msg: Msg) -> EffectCommand<Msg> {
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
        // The same single-`Action::Quit` stream `Self::effect(Action::Quit)`
        // builds, tagged as the immediate-quit carrier so lowering can tell
        // an `update`-returned quit from an effect that happens to emit one
        // (RFC 0014 §3.3). Consumers that read streams alone see no change.
        Self::with_effect(Effect::immediate_quit())
    }

    fn effect(action: Action<Msg>) -> EffectCommand<Msg> {
        EffectCommand::from_action_stream(stream::once(async move { action }).boxed())
    }

    /// An ordinary effect carrier built from a stream of raw actions.
    ///
    /// This is the only route to a **producer-originated** quit (RFC 0014
    /// §3.3): [`Command::quit`] builds the immediate-quit carrier, which
    /// lowering applies synchronously at its dispatch and never spawns, and
    /// no other constructor here puts an [`Action::Quit`] into a carrier a
    /// run is spawned for. The kernel's conformance series need that route
    /// to script a producer quit at all, and the load harness needs it to
    /// measure the control lane at all (RFC 0014 §13.5), so it is
    /// crate-visible under `test` and under the bench-only feature.
    ///
    /// **It stays crate-visible.** The effect-constructor split that owns the
    /// public shape of keying has landed, so the deferral this comment used
    /// to make has somewhere to resolve. Publishing the constructor would
    /// carry [`Action`] into the public vocabulary with it, for a capability
    /// applications already have by other means: an effect emits a message
    /// and the `update` that observes it returns [`Command::quit`], which is
    /// the order RFC 0014 §3.3 recommends for "deliver then quit" anyway.
    /// What a public `actions` adds over that is backlog independence for the
    /// quit — a property of the control lane rather than of the constructor —
    /// and no part of the switch or of store parity needs it.
    ///
    /// It is an effect constructor, so it returns a carrier like the rest:
    /// `Command::actions(..).cancellable(id)` builds, because a
    /// producer-originated quit is keyed or anonymous like any other run.
    #[cfg(any(test, feature = "bench-internals"))]
    pub(crate) fn actions(
        stream: impl Stream<Item = Action<Msg>> + Send + 'static,
    ) -> EffectCommand<Msg> {
        EffectCommand::from_action_stream(stream.boxed())
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
    pub fn batch<C: Into<Self>>(commands: impl IntoIterator<Item = C>) -> Self {
        let mut directives = RuntimeDirectives::DEFAULT.without_redraw();
        let mut any_child = false;
        let mut effects = Vec::new();
        let mut cancels = Vec::new();
        let mut teardowns = Vec::new();
        let mut cleanups = Vec::new();

        for cmd in commands {
            let cmd = cmd.into();
            any_child = true;
            directives = directives.combine(cmd.directives);
            cancels.extend(cmd.cancellation.cancels);
            teardowns.extend(cmd.teardowns);
            cleanups.extend(cmd.cleanups);
            effects.push(cmd.effect);
        }

        if any_child {
            Self {
                effect: Effect::batch(effects),
                directives,
                cancellation: CommandCancellation { cancels },
                teardowns,
                cleanups,
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
    pub fn stream(stream: impl Stream<Item = Msg> + Send + 'static) -> EffectCommand<Msg> {
        EffectCommand::from_action_stream(stream.map(Action::Message).boxed())
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
    ) -> EffectCommand<Msg>
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
    /// })
    /// .into();
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
        let cancellation = self.cancellation;
        let teardowns = self.teardowns;
        // Carried across the message-type change untouched: a registration
        // holds no message type to map, its finalizer's `Output` being `()`
        // (RFC 0014 §4.4).
        let cleanups = self.cleanups;
        let effect = self.effect.map(f);

        Command {
            effect,
            directives,
            cancellation,
            teardowns,
            cleanups,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::{cell::Cell, future::pending};

    use futures::stream;
    use tokio::time::{advance, sleep};
    use tracing::Level;

    use crate::command::{CancelPolicy, KernelParts};
    use crate::test_support::TraceRecorder;

    #[test]
    fn test_redraw_defaults_to_true_for_constructors() {
        assert!(Command::<i32>::none().requests_redraw());
        assert!(Command::message(1).into_command().requests_redraw());
        assert!(
            Command::future(async { 1 })
                .into_command()
                .requests_redraw()
        );
        assert!(
            Command::perform(async { 1 }, |value| value)
                .into_command()
                .requests_redraw()
        );
        assert!(Command::<i32>::quit().requests_redraw());
        assert!(
            Command::stream(stream::iter(vec![1]))
                .into_command()
                .requests_redraw()
        );
        assert!(
            Command::run(stream::iter(vec![1]), |value| value)
                .into_command()
                .requests_redraw()
        );
        assert!(Command::batch(vec![Command::<i32>::none()]).requests_redraw());
        assert!(Command::<i32>::batch(Vec::<Command<i32>>::new()).requests_redraw());
    }

    #[test]
    fn test_without_redraw_flips_redraw() {
        let cmd = Command::<i32>::none().without_redraw();
        assert!(!cmd.requests_redraw());
    }

    #[test]
    fn test_cancellation_metadata_defaults_empty() {
        let command = Command::<i32>::none();

        assert!(command.cancellation.cancels.is_empty());
        assert!(kernel_parts(command).spawns.is_empty());
    }

    #[test]
    fn test_cancellable_is_last_call_wins() {
        let command = Command::message(1)
            .cancellable_with(CommandId::new("first"), CancelPolicy::KeepInFlight)
            .cancellable(CommandId::new("second"));
        let mut spawns = kernel_parts(command.into()).spawns;
        let key = spawns
            .pop()
            .and_then(|spawn| spawn.key)
            .expect("key should be present");

        assert_eq!(key.id, CommandId::new("second"));
        assert_eq!(key.policy, CancelPolicy::CancelInFlight);
    }

    #[test]
    fn test_unscoped_tuple_local_id_does_not_alias_a_scoped_identity() {
        let tupled = CommandId::new(("pane-1", "load"));
        let scoped = Command::message(1)
            .cancellable(CommandId::new("load"))
            .scoped("pane-1");
        let scoped_id = carrier_key(scoped).expect("key should be present");

        assert_ne!(tupled, scoped_id);
    }

    #[test]
    fn test_scoped_none_is_lifecycle_inert() {
        let command = Command::<i32>::none().scoped("pane-1");

        assert!(command.is_none());
        assert!(command.cancellation.cancels.is_empty());
    }

    #[test]
    fn test_scoped_qualifies_the_cancellable_key() {
        let scoped = Command::message(1)
            .cancellable(CommandId::new("load"))
            .scoped("pane-1");
        let mut spawns = kernel_parts(scoped.into()).spawns;
        let key = spawns
            .pop()
            .and_then(|spawn| spawn.key)
            .expect("key should be present");

        assert_ne!(key.id, CommandId::new("load"));
        assert_eq!(key.policy, CancelPolicy::CancelInFlight);
    }

    #[test]
    fn test_scoped_independent_child_instances_do_not_alias() {
        let pane_a = Command::message(1)
            .cancellable(CommandId::new("load"))
            .scoped("pane-a");
        let pane_b = Command::message(1)
            .cancellable(CommandId::new("load"))
            .scoped("pane-b");

        assert_ne!(
            carrier_key(pane_a).expect("key should be present"),
            carrier_key(pane_b).expect("key should be present")
        );
    }

    #[test]
    fn test_scoped_with_equal_scope_produces_equal_ids() {
        let first = Command::message(1)
            .cancellable(CommandId::new("load"))
            .scoped("pane-1");
        let second = Command::message(1)
            .cancellable(CommandId::new("load"))
            .scoped("pane-1");

        assert_eq!(
            carrier_key(first).expect("key should be present"),
            carrier_key(second).expect("key should be present")
        );
    }

    #[test]
    fn test_scoped_qualifies_explicit_cancels() {
        let id = CommandId::new("load");
        let scoped = Command::<i32>::cancel(id.clone()).scoped("pane-1");

        assert_eq!(scoped.cancellation.cancels.len(), 1);
        assert_ne!(scoped.cancellation.cancels[0], id);
    }

    #[test]
    fn test_scoped_hash_collision_does_not_alias_scopes() {
        #[derive(Eq, PartialEq)]
        struct Collision(u8);

        impl Hash for Collision {
            fn hash<H: Hasher>(&self, state: &mut H) {
                0_u8.hash(state);
            }
        }

        let first = Command::message(1)
            .cancellable(CommandId::new("load"))
            .scoped(Collision(1));
        let second = Command::message(1)
            .cancellable(CommandId::new("load"))
            .scoped(Collision(2));

        assert_ne!(
            carrier_key(first).expect("key should be present"),
            carrier_key(second).expect("key should be present")
        );
    }

    #[test]
    fn test_cancellable_after_scoped_installs_a_root_global_key() {
        let command = Command::message(1)
            .scoped("pane-1")
            .cancellable(CommandId::new("load"));
        assert_eq!(
            carrier_key(command).expect("key should be present"),
            CommandId::new("load")
        );
    }

    #[test]
    fn test_mixed_scoped_cancels_stay_scoped_while_later_global_key_does_not() {
        let cancel_id = CommandId::new("old");

        // The explicit cancel present before `scoped` remains pane-scoped.
        let cancel = Command::<i32>::cancel(cancel_id.clone()).scoped("pane-1");
        assert_ne!(cancel.cancellation.cancels[0], cancel_id);

        // The spawn key attached after `scoped` is root-global. It rides an
        // effect carrier: `cancel` is not an effect constructor, so it takes
        // no key at all now.
        let keyed = Command::<i32>::message(1)
            .scoped("pane-1")
            .cancellable(CommandId::new("global"));
        assert_eq!(
            carrier_key(keyed).expect("key should be present"),
            CommandId::new("global")
        );
    }

    #[test]
    fn test_scoped_batch_preserves_scoped_cancels_and_still_ignores_child_keys() {
        let left_cancel = CommandId::new("left");
        let right_cancel = CommandId::new("right");

        let batch = Command::batch([
            Command::<i32>::cancel(left_cancel.clone()).scoped("left-pane"),
            Command::cancel(right_cancel.clone()).scoped("right-pane"),
            Command::message(1)
                .cancellable(CommandId::new("ignored"))
                .into(),
        ]);

        assert_eq!(batch.cancellation.cancels.len(), 2);
        assert_ne!(batch.cancellation.cancels[0], left_cancel);
        assert_ne!(batch.cancellation.cancels[1], right_cancel);
        // The child key stayed on the child's own carrier; nothing put it on
        // the batch (RFC 0014 §3.4).
        assert_eq!(carrier_keys(batch), vec![Some(CommandId::new("ignored"))]);
    }

    #[test]
    fn test_scoped_after_batch_scopes_the_folded_cancels() {
        let first = CommandId::new("first");
        let second = CommandId::new("second");
        let batch = Command::batch([
            Command::<i32>::cancel(first.clone()),
            Command::cancel(second.clone()),
        ])
        .scoped("outer");

        assert_ne!(batch.cancellation.cancels[0], first);
        assert_ne!(batch.cancellation.cancels[1], second);
    }

    #[test]
    fn test_cancel_is_streamless_and_preserves_redraw_control() {
        let id = CommandId::new("search");
        let command = Command::<i32>::cancel(id.clone()).without_redraw();

        assert!(command.is_none());
        assert!(!command.requests_redraw());
        assert_eq!(command.cancellation.cancels, vec![id]);
    }

    #[test]
    fn test_map_preserves_cancellation_metadata() {
        let cancel_id = CommandId::new("old");
        let key_id = CommandId::new("current");
        let command = Command::<i32>::batch([
            Command::cancel(cancel_id.clone()),
            Command::message(1)
                .cancellable_with(key_id.clone(), CancelPolicy::KeepInFlight)
                .into(),
        ])
        .map(|value| value.to_string());

        assert_eq!(command.cancellation.cancels, vec![cancel_id]);

        // The key names the carrier it was attached to, so it is read there
        // rather than off the command: keying the batch itself no longer
        // builds (RFC 0014 §3.4).
        let parts = kernel_parts(command);
        let key = parts.spawns[0].key.as_ref().expect("key should be present");
        assert_eq!(key.id, key_id);
        assert_eq!(key.policy, CancelPolicy::KeepInFlight);
    }

    #[test]
    fn test_batch_folds_cancels_and_discards_child_keys() {
        let first = CommandId::new("first");
        let second = CommandId::new("second");
        let command: Command<i32> = Command::batch([
            Command::<i32>::cancel(first.clone()),
            Command::cancel(second.clone()),
            Command::message(1)
                .cancellable(CommandId::new("ignored"))
                .into(),
        ]);

        assert_eq!(command.cancellation.cancels, vec![first, second]);
    }

    // RFC 0014 §9 row 3: a batch no longer discards a child's key, so there
    // is nothing left to warn about. Each child lowers to its own keyed
    // entry, and the unkeyed sibling stays unkeyed.
    #[test]
    fn batch_children_keep_their_own_keys_and_warn_about_nothing() {
        let recorder = TraceRecorder::new()
            .with_target("tears::command")
            .with_level(Level::WARN);
        let _guard = recorder.set_default();

        let command = Command::batch([
            Command::from(Command::message(1).cancellable(CommandId::new("kept"))),
            Command::from(Command::message(2)),
        ]);

        assert_eq!(
            carrier_keys(command),
            vec![Some(CommandId::new("kept")), None]
        );
        assert_eq!(recorder.event_count(), 0);
    }

    #[test]
    fn test_timeout_preserves_cancellation_metadata() {
        let key_id = CommandId::new("timeout");
        let command = Command::future(pending::<i32>())
            .cancellable_with(key_id.clone(), CancelPolicy::KeepInFlight)
            .timeout(Duration::from_secs(1), || 99);
        let mut spawns = kernel_parts(command.into()).spawns;
        let key = spawns
            .pop()
            .and_then(|spawn| spawn.key)
            .expect("key should be present");

        assert_eq!(key.id, key_id);
        assert_eq!(key.policy, CancelPolicy::KeepInFlight);
    }

    #[test]
    fn test_batch_redraw_is_or_over_children() {
        let cmd = Command::batch(vec![
            Command::none().without_redraw(),
            Command::future(async { 1 }).into(),
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
        assert!(!cmd.into_command().requests_redraw());
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
            .timeout(Duration::from_secs(1), || "before".to_owned());
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
        let parts = Command::message(42).into_command().into_runtime_parts();

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
            Command::message(2).without_redraw().into(),
            Command::none().without_redraw(),
            Command::message(1).without_redraw().into(),
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
            .into_command()
            .into_runtime_parts();

        assert!(!parts.requests_redraw());

        let mut stream = parts.into_stream().expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(msg) if msg == 42));
    }

    #[tokio::test]
    async fn test_batch_empty() {
        let cmd: Command<i32> = Command::batch(Vec::<Command<i32>>::new());
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
        let cmd1 = Command::future(async { 1 }).into();
        let cmd2 = Command::<i32>::none();
        let cmd3 = Command::future(async { 3 }).into();

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
        let cmd1 = Command::future(async { 1 }).into();
        let cmd2 = Command::quit();
        let cmd3 = Command::future(async { 3 }).into();

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

        let mut stream = cmd
            .into_command()
            .into_stream()
            .expect("stream should exist");
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

        let mut stream = cmd
            .into_command()
            .into_stream()
            .expect("stream should exist");
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

        let mut stream = cmd
            .into_command()
            .into_stream()
            .expect("stream should exist");
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

        let mut stream = cmd
            .into_command()
            .into_stream()
            .expect("stream should exist");
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

        let mut stream = cmd
            .into_command()
            .into_stream()
            .expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(msg) if msg == 42));

        // Stream should be exhausted
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_perform() {
        #[expect(
            clippy::unused_async,
            reason = "fixture must be async to produce a future for Command::perform even though it awaits nothing"
        )]
        async fn fetch_value() -> i32 {
            42
        }

        let cmd = Command::perform(fetch_value(), |x| x * 2);

        let mut stream = cmd
            .into_command()
            .into_stream()
            .expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(msg) if msg == 84));
    }

    #[tokio::test]
    async fn test_perform_with_result() {
        #[expect(
            clippy::unused_async,
            reason = "fixture must be async to produce a future for Command::perform even though it awaits nothing"
        )]
        async fn fallible_operation() -> Result<String, String> {
            Ok("success".to_owned())
        }

        let cmd = Command::perform(fallible_operation(), |result| match result {
            Ok(s) => format!("Got: {s}"),
            Err(e) => format!("Error: {e}"),
        });

        let mut stream = cmd
            .into_command()
            .into_stream()
            .expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(msg) if msg == "Got: success"));
    }

    #[tokio::test]
    async fn test_message() {
        let cmd = Command::message(42);

        let mut stream = cmd
            .into_command()
            .into_stream()
            .expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(msg) if msg == 42));

        // Stream should be exhausted
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_message_with_string() {
        let cmd = Command::message("hello".to_owned());

        let mut stream = cmd
            .into_command()
            .into_stream()
            .expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(msg) if msg == "hello"));
    }

    #[tokio::test]
    async fn test_effect_with_message() {
        let cmd = Command::effect(Action::Message(100));

        let mut stream = cmd
            .into_command()
            .into_stream()
            .expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(msg) if msg == 100));
    }

    #[tokio::test]
    async fn test_effect_with_quit() {
        let cmd: Command<i32> = Command::effect(Action::Quit).into();

        let mut stream = cmd.into_stream().expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Quit));
    }

    #[tokio::test]
    async fn test_stream_empty() {
        let input_stream = stream::iter(Vec::<i32>::new());
        let cmd = Command::stream(input_stream);

        let mut stream = cmd
            .into_command()
            .into_stream()
            .expect("stream should exist");
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
            "delayed".to_owned()
        });

        let mut stream = cmd
            .into_command()
            .into_stream()
            .expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(msg) if msg == "delayed"));
    }

    #[tokio::test]
    async fn test_perform_with_error_handling() {
        #[expect(
            clippy::unused_async,
            reason = "fixture must be async to produce a future for Command::perform even though it awaits nothing"
        )]
        async fn may_fail(should_fail: bool) -> Result<i32, &'static str> {
            if should_fail {
                Err("operation failed")
            } else {
                Ok(42)
            }
        }

        // Test success case
        let cmd = Command::perform(may_fail(false), |result| result.unwrap_or(-1));

        let mut stream = cmd
            .into_command()
            .into_stream()
            .expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(msg) if msg == 42));

        // Test error case
        let cmd = Command::perform(may_fail(true), |result| result.unwrap_or(-1));

        let mut stream = cmd
            .into_command()
            .into_stream()
            .expect("stream should exist");
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

        let mut stream = mapped
            .into_command()
            .into_stream()
            .expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(msg) if msg == 84));
    }

    #[tokio::test]
    async fn test_map_with_type_conversion() {
        #[derive(Debug, PartialEq)]
        enum Message {
            Number(i32),
        }

        let cmd: Command<i32> = Command::future(async { 42 }).into();
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
            Command::future(async { Ok("data".to_owned()) }).into();

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
        let cmd = cmd.into_command();
        assert!(cmd.is_some());
        assert!(!cmd.is_none());
    }

    #[test]
    fn test_is_some_with_future() {
        let cmd = Command::future(async { 100 });
        let cmd = cmd.into_command();
        assert!(cmd.is_some());
        assert!(!cmd.is_none());
    }

    #[test]
    fn test_is_some_with_message() {
        let cmd = Command::message("test");
        let cmd = cmd.into_command();
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
        let cmd: Command<i32> = Command::batch(Vec::<Command<i32>>::new());
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
            Command::future(async { 42 }).into(),
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
        let mapped = mapped.into_command();
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
        let cmd = Command::batch(vec![batch1, batch2, Command::future(async { 5 }).into()]);

        // batch(batch(a, b), batch(c, d), e) collapses to a single flat leaf
        // sequence [a, b, c, d, e] rather than a nested structure.
        assert_eq!(cmd.effect.leaf_count(), 5);
    }

    #[test]
    fn test_batch_drops_streamless_children_from_leaves() {
        let cmd = Command::batch(vec![
            Command::future(async { 1 }).into(),
            Command::none(),
            Command::none().without_redraw(),
            Command::future(async { 2 }).into(),
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
                .into_command()
                .requests_redraw()
        );
        assert!(
            !Command::future(async { 1 })
                .without_redraw()
                .map(|v| v * 2)
                .into_command()
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

    // --- Leaf metadata: the kernel reading of the same lowering boundary ---

    fn kernel_parts<Msg: Send + 'static>(command: Command<Msg>) -> KernelParts<Msg> {
        command.into_runtime_parts().into_kernel_parts()
    }

    /// The spawn key of a command's single carrier — where a key lives now
    /// that `cancellable` attaches it to the carrier rather than the command.
    fn carrier_key<Msg: Send + 'static>(command: impl Into<Command<Msg>>) -> Option<CommandId> {
        let mut spawns = kernel_parts(command.into()).spawns;
        assert_eq!(spawns.len(), 1, "expected exactly one carrier");
        spawns.pop().and_then(|spawn| spawn.key).map(|key| key.id)
    }

    /// Every carrier's spawn key, in declaration order.
    fn carrier_keys<Msg: Send + 'static>(command: Command<Msg>) -> Vec<Option<CommandId>> {
        kernel_parts(command)
            .spawns
            .into_iter()
            .map(|spawn| spawn.key.map(|key| key.id))
            .collect()
    }

    #[test]
    fn quit_lowers_to_a_synchronous_quit_and_spawns_nothing() {
        let parts = kernel_parts(Command::<i32>::quit());

        assert!(parts.quit_now);
        assert!(parts.spawns.is_empty());
    }

    // The quit carrier is marked, not spawned: lowering applies it at the
    // dispatch's completion and no run is started for it (RFC 0014 §3.3).
    #[test]
    fn quit_lowers_to_the_marker_and_starts_no_run() {
        let parts = kernel_parts(Command::<i32>::quit());

        assert!(parts.quit_now);
        assert!(parts.spawns.is_empty());
        assert!(parts.cancels.is_empty());
    }

    // RFC 0003 INV-12 / RFC 0005 INV-17: the wrappers relay the stream and
    // preserve identity metadata, so an immediate quit stays one after a
    // `map` or a `timeout` (RFC 0014 §3.3 depends on the mark surviving).
    #[test]
    fn map_preserves_the_immediate_quit_carrier() {
        let parts = kernel_parts(Command::<i32>::quit().map(|value| value.to_string()));

        assert!(parts.quit_now);
        assert!(parts.spawns.is_empty());
    }

    #[test]
    fn timeout_preserves_the_immediate_quit_carrier() {
        let parts = kernel_parts(Command::<i32>::quit().timeout(Duration::from_secs(1), || 99));

        assert!(parts.quit_now);
        assert!(parts.spawns.is_empty());
    }

    #[test]
    fn map_over_a_batch_preserves_every_carrier_key() {
        let parts = kernel_parts(
            Command::batch([
                Command::message(1).cancellable(CommandId::new("left")),
                Command::message(2).cancellable(CommandId::new("right")),
            ])
            .map(|value| value * 10),
        );
        let keys: Vec<_> = parts
            .spawns
            .iter()
            .map(|spawn| spawn.key.as_ref().map(|key| key.id.clone()))
            .collect();

        assert_eq!(
            keys,
            vec![Some(CommandId::new("left")), Some(CommandId::new("right")),]
        );
    }

    #[test]
    fn batch_lowers_each_child_key_to_its_own_entry() {
        let parts = kernel_parts(Command::batch([
            Command::message(1).cancellable(CommandId::new("left")),
            Command::message(2),
            Command::message(3)
                .cancellable_with(CommandId::new("right"), CancelPolicy::KeepInFlight),
        ]));

        assert_eq!(parts.spawns.len(), 3);
        assert_eq!(
            parts.spawns[0].key.as_ref().expect("keyed child").id,
            CommandId::new("left")
        );
        assert!(parts.spawns[1].key.is_none());
        let right = parts.spawns[2].key.as_ref().expect("keyed child");
        assert_eq!(right.id, CommandId::new("right"));
        assert_eq!(right.policy, CancelPolicy::KeepInFlight);
    }

    #[test]
    fn a_top_level_key_names_the_single_carrier_it_reaches() {
        let parts = kernel_parts(
            Command::message(1)
                .cancellable(CommandId::new("load"))
                .into(),
        );

        assert_eq!(parts.spawns.len(), 1);
        assert_eq!(
            parts.spawns[0].key.as_ref().expect("keyed command").id,
            CommandId::new("load")
        );
    }

    #[test]
    fn scoped_qualifies_every_carrier_and_the_key_it_already_held() {
        let parts = kernel_parts(
            Command::batch([
                Command::message(1).cancellable(CommandId::new("load")),
                Command::message(2),
            ])
            .scoped("pane-1"),
        );
        let scope = ScopePath::empty().prefixed("pane-1");

        assert_eq!(parts.spawns.len(), 2);
        for spawn in &parts.spawns {
            assert_eq!(spawn.scope, scope);
        }
        // In *this* shape the two paths coincide, because one `scoped`
        // call qualified the carrier and the key it already held at the
        // same boundary. It is not an identity: the carrier's scope places
        // the run and the key's scope is part of its cancel identity, and
        // `work.scoped(s).cancellable(id)` — which this type's own docs
        // bless — separates them.
        assert_eq!(
            parts.spawns[0]
                .key
                .as_ref()
                .expect("keyed child")
                .id
                .scope(),
            &parts.spawns[0].scope
        );
    }

    #[test]
    fn sibling_scopes_do_not_alias_carrier_attribution() {
        let left = kernel_parts(Command::message(1).scoped("pane-a").into());
        let right = kernel_parts(Command::message(1).scoped("pane-b").into());

        assert_ne!(left.spawns[0].scope, right.spawns[0].scope);
    }

    #[test]
    fn an_unscoped_carrier_is_attributed_to_the_root() {
        let parts = kernel_parts(Command::message(1).into());

        assert_eq!(parts.spawns[0].scope, ScopePath::empty());
    }

    #[test]
    fn nested_scopes_nest_outermost_first() {
        let parts = kernel_parts(Command::message(1).scoped("field").scoped("pane-1").into());
        let outer = ScopePath::empty().prefixed("pane-1");

        assert!(parts.spawns[0].scope.starts_with(&outer));
    }

    #[test]
    fn teardown_is_streamless_and_carries_one_prefix() {
        let command = Command::<i32>::teardown("pane-1");

        assert!(command.is_none());
        assert!(command.cancellation.cancels.is_empty());

        let parts = kernel_parts(command);
        assert!(parts.spawns.is_empty());
        assert!(!parts.quit_now);
        assert_eq!(parts.teardowns, vec![ScopePath::empty().prefixed("pane-1")]);
    }

    #[test]
    fn scoped_prepends_to_a_teardown_prefix() {
        let parts = kernel_parts(Command::<i32>::teardown("field").scoped("pane-1"));
        let outer = ScopePath::empty().prefixed("pane-1");

        assert_eq!(parts.teardowns.len(), 1);
        assert!(parts.teardowns[0].starts_with(&outer));
    }

    #[test]
    fn batch_concatenates_teardown_prefixes_in_declaration_order() {
        let parts = kernel_parts(Command::batch([
            Command::<i32>::teardown("left"),
            Command::teardown("right"),
        ]));

        assert_eq!(
            parts.teardowns,
            vec![
                ScopePath::empty().prefixed("left"),
                ScopePath::empty().prefixed("right"),
            ]
        );
    }

    #[test]
    fn map_preserves_teardown_prefixes() {
        let parts = kernel_parts(Command::<i32>::teardown("pane-1").map(|value| value * 2));

        assert_eq!(parts.teardowns, vec![ScopePath::empty().prefixed("pane-1")]);
    }

    // Teardown is command metadata carried through the cancel phase, and it
    // starts nothing: it is a selection over live runs, not a run of its own.
    #[test]
    fn teardown_lowers_to_a_prefix_and_starts_no_run() {
        let parts = kernel_parts(Command::<i32>::teardown("pane-1"));

        assert_eq!(parts.teardowns, vec![ScopePath::empty().prefixed("pane-1")]);
        assert!(parts.cancels.is_empty());
        assert!(parts.spawns.is_empty());
    }

    #[test]
    fn test_batch_redraw_matrix_over_effect_and_map() {
        // Mixed children: OR over redraw flags holds regardless of effects.
        let cmd = Command::batch(vec![
            Command::future(async { 1 }).without_redraw().into(),
            Command::none(),
        ]);
        assert!(cmd.is_some());
        assert!(cmd.requests_redraw());

        // All opted out, both with and without a stream: stays opted out.
        let cmd = Command::batch(vec![
            Command::future(async { 1 }).without_redraw().into(),
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
