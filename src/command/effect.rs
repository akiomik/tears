use std::{
    pin::Pin,
    sync::{Arc, Mutex, PoisonError},
    task::{Context, Poll},
    time::Duration,
};

use futures::{
    FutureExt, Stream, StreamExt,
    stream::{self, BoxStream},
};
use tokio::time::{Sleep, sleep};

use crate::structural_key::{ScopePath, StructuralKey};

use super::Action;
use super::cancellation::CancellableCommand;

/// Wraps one effect leaf with a lazily started overall deadline and terminal
/// timeout handling.
struct TimeoutLeaf<Msg, F>
where
    Msg: Send + 'static,
{
    inner: Option<BoxStream<'static, Action<Msg>>>,
    sleep: Option<Pin<Box<Sleep>>>,
    duration: Duration,
    on_timeout: Arc<Mutex<Option<F>>>,
    deadline_observed: bool,
}

impl<Msg, F> TimeoutLeaf<Msg, F>
where
    Msg: Send + 'static,
    F: FnOnce() -> Msg,
{
    fn new(
        inner: BoxStream<'static, Action<Msg>>,
        duration: Duration,
        on_timeout: Arc<Mutex<Option<F>>>,
    ) -> Self {
        Self {
            inner: Some(inner),
            sleep: None,
            duration,
            on_timeout,
            deadline_observed: false,
        }
    }

    fn finish(&mut self) {
        self.inner = None;
        self.sleep = None;
    }

    fn take_deadline_path(&mut self) -> Poll<Option<Action<Msg>>> {
        self.finish();
        let on_timeout = self
            .on_timeout
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();

        Poll::Ready(on_timeout.map(|on_timeout| Action::Message(on_timeout())))
    }
}

impl<Msg, F> Stream for TimeoutLeaf<Msg, F>
where
    Msg: Send + 'static,
    F: FnOnce() -> Msg + Send + 'static,
{
    type Item = Action<Msg>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let state = &mut *self;

        if state.inner.is_none() {
            return Poll::Ready(None);
        }

        // Constructing the sleep on the first poll makes the timeout an
        // execution deadline rather than a construction deadline.
        if state.sleep.is_none() {
            state.sleep = Some(Box::pin(sleep(state.duration)));
        }

        // If a deadline/item tie previously passed through a message, inspect
        // the inner stream once more only so termination can retain priority.
        // Any other result takes the already-observed deadline path.
        if state.deadline_observed {
            let inner_poll = state
                .inner
                .as_mut()
                .expect("checked above")
                .as_mut()
                .poll_next(cx);

            if matches!(inner_poll, Poll::Ready(None)) {
                state.finish();
                return Poll::Ready(None);
            }

            return state.take_deadline_path();
        }

        // Poll both sides before choosing an outcome. This lets inner
        // termination deterministically win while bounding a continuously
        // ready inner stream to one item after the deadline becomes ready.
        let inner_poll = state
            .inner
            .as_mut()
            .expect("checked above")
            .as_mut()
            .poll_next(cx);
        let deadline_ready = state
            .sleep
            .as_mut()
            .expect("initialized above")
            .as_mut()
            .poll(cx)
            .is_ready();

        match inner_poll {
            Poll::Ready(None) => {
                state.finish();
                Poll::Ready(None)
            }
            Poll::Ready(Some(Action::Quit)) => {
                state.finish();
                Poll::Ready(Some(Action::Quit))
            }
            Poll::Ready(Some(action @ Action::Message(_))) => {
                if deadline_ready {
                    state.deadline_observed = true;
                }
                Poll::Ready(Some(action))
            }
            Poll::Pending if deadline_ready => state.take_deadline_path(),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// What a leaf lowers to.
///
/// The distinction is metadata, not stream shape: an `ImmediateQuit` leaf
/// still carries the same one-item `Action::Quit` stream any other leaf
/// would, so a consumer that reads streams alone observes no difference.
/// What it names is the lowering bucket — an `update`-returned quit applies
/// synchronously at the dispatch that returned it rather than being spawned
/// as a producer run (RFC 0014 §3.3).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LeafKind {
    /// An ordinary effect carrier: the runtime spawns its stream.
    Effect,
    /// The carrier [`Command::quit`](super::Command::quit) builds.
    ImmediateQuit,
}

/// One effect carrier: its stream plus the identity metadata that travels
/// with it through `Command` composition.
///
/// `key` is the carrier's spawn key. It is `None` on construction — a
/// top-level `cancellable` call still records the key on the command — and
/// is filled in when [`Command::batch`](super::Command::batch) pushes a
/// child's key down to the child's own carriers, which is what lets each
/// child lower to an independent keyed entry (RFC 0014 §3.4).
///
/// `scope` is the carrier's composition-boundary attribution, applied by
/// [`Command::scoped`](super::Command::scoped). A carrier with no key still
/// carries one, so a prefix teardown reaches anonymous effects spawned
/// through a boundary (RFC 0014 INV-RC7).
pub(super) struct Leaf<Msg: Send + 'static> {
    pub(super) key: Option<CancellableCommand>,
    pub(super) scope: ScopePath,
    pub(super) kind: LeafKind,
    pub(super) stream: BoxStream<'static, Action<Msg>>,
}

impl<Msg: Send + 'static> Leaf<Msg> {
    fn new(kind: LeafKind, stream: BoxStream<'static, Action<Msg>>) -> Self {
        Self {
            key: None,
            scope: ScopePath::empty(),
            kind,
            stream,
        }
    }

    /// Rewraps the stream — possibly at a new message type — keeping every
    /// piece of metadata. This is the one shape `map` and `timeout` use, so
    /// their metadata pass-through (RFC 0003 INV-12, RFC 0005 INV-17) holds
    /// by construction rather than by each wrapper remembering to copy three
    /// fields.
    fn relay<T: Send + 'static>(
        self,
        wrap: impl FnOnce(BoxStream<'static, Action<Msg>>) -> BoxStream<'static, Action<T>>,
    ) -> Leaf<T> {
        Leaf {
            key: self.key,
            scope: self.scope,
            kind: self.kind,
            stream: wrap(self.stream),
        }
    }
}

// Effects own and compose the asynchronous action stream; runtime directives
// stay separate because they describe how the runtime treats the update result.
//
// Rather than folding children into a single opaque stream at construction
// time, an effect keeps the flat sequence of leaves and hands them out
// unfolded at the `into_leaves()` boundary, from where `RuntimeCommandParts`
// carries them to each consumer (RFC 0008 §4.1): the runtime folds their
// streams (via `fold_leaves`) at its spawn site, `TestStore` drives them one
// by one. Keeping the leaves apart preserves each leaf's identity through
// `Command` composition, which is what the per-leaf spawn key and scope
// attach to.
pub(super) struct Effect<Msg: Send + 'static> {
    leaves: Vec<Leaf<Msg>>,
}

impl<Msg: Send + 'static> Effect<Msg> {
    pub(super) const fn none() -> Self {
        Self { leaves: Vec::new() }
    }

    fn from_stream(stream: BoxStream<'static, Action<Msg>>) -> Self {
        Self {
            leaves: vec![Leaf::new(LeafKind::Effect, stream)],
        }
    }

    pub(super) fn future(future: impl Future<Output = Msg> + Send + 'static) -> Self {
        Self::from_stream(future.into_stream().map(Action::Message).boxed())
    }

    pub(super) fn action(action: Action<Msg>) -> Self {
        Self::from_stream(stream::once(async move { action }).boxed())
    }

    /// The carrier behind [`Command::quit`](super::Command::quit): the same
    /// single-`Action::Quit` stream [`Effect::action`] would build, marked
    /// [`LeafKind::ImmediateQuit`] so lowering can apply it synchronously
    /// (RFC 0014 §3.3) instead of spawning it.
    pub(super) fn immediate_quit() -> Self {
        Self {
            leaves: vec![Leaf::new(
                LeafKind::ImmediateQuit,
                stream::once(async { Action::Quit }).boxed(),
            )],
        }
    }

    pub(super) fn stream(stream: impl Stream<Item = Msg> + Send + 'static) -> Self {
        Self::from_stream(stream.map(Action::Message).boxed())
    }

    pub(super) fn batch(effects: impl IntoIterator<Item = Self>) -> Self {
        // Concatenate the children's leaves. Because every effect already holds
        // a flat leaf sequence, nested batches flatten automatically and
        // stream-less children contribute nothing.
        let leaves: Vec<_> = effects
            .into_iter()
            .flat_map(|effect| effect.leaves)
            .collect();

        Self { leaves }
    }

    pub(super) fn timeout<F>(self, duration: Duration, on_timeout: F) -> Self
    where
        F: FnOnce() -> Msg + Send + 'static,
    {
        let on_timeout = Arc::new(Mutex::new(Some(on_timeout)));
        let leaves = self
            .leaves
            .into_iter()
            .map(|leaf| {
                let on_timeout = Arc::clone(&on_timeout);
                leaf.relay(|stream| TimeoutLeaf::new(stream, duration, on_timeout).boxed())
            })
            .collect();
        Self { leaves }
    }

    /// Pushes a spawn key down onto every carrier that does not already have
    /// one, so a batched child's key reaches the child's own carriers rather
    /// than the batch (RFC 0014 §3.4).
    ///
    /// Carriers that already hold a key keep it: the key nearest the effect
    /// is the one that names its run.
    pub(super) fn attach_key(&mut self, key: &CancellableCommand) {
        for leaf in &mut self.leaves {
            if leaf.key.is_none() {
                leaf.key = Some(key.clone());
            }
        }
    }

    /// Qualifies every carrier's spawn key and scope attribution with one
    /// already-erased composition-boundary segment.
    ///
    /// Both halves matter: the key half keeps a batched child's key distinct
    /// per boundary, and the scope half is what gives an *unkeyed* carrier a
    /// scope for a prefix teardown to select (RFC 0014 INV-RC7).
    pub(super) fn apply_scope(&mut self, segment: &StructuralKey) {
        for leaf in &mut self.leaves {
            leaf.key = leaf.key.take().map(|cancellable| CancellableCommand {
                id: cancellable.id.scoped_with(segment.clone()),
                policy: cancellable.policy,
            });
            leaf.scope = leaf.scope.prefixed_key(segment.clone());
        }
    }

    pub(super) fn map<T>(self, f: impl Fn(Msg) -> T + Send + 'static) -> Effect<T>
    where
        T: Send + 'static,
    {
        fn map_leaf<Msg, T>(
            leaf: BoxStream<'static, Action<Msg>>,
            f: impl Fn(Msg) -> T + Send + 'static,
        ) -> BoxStream<'static, Action<T>>
        where
            Msg: Send + 'static,
            T: Send + 'static,
        {
            leaf.map(move |action| match action {
                Action::Message(msg) => Action::Message(f(msg)),
                Action::Quit => Action::Quit,
            })
            .boxed()
        }

        // Map each leaf on its own to preserve leaf count, order, and every
        // leaf's identity metadata (RFC 0003 INV-12, RFC 0005 INV-17). A
        // single leaf moves `f` straight into its closure with no
        // shared-ownership cost (the pre-refactor path). Several leaves must
        // share `f`: `Arc<F>` alone would require `F: Sync`, but the public
        // `map` bound is only `Fn + Send`, so a `Mutex` supplies the needed
        // `Sync`.
        let mut leaves = self.leaves;
        if leaves.len() == 1 {
            let leaf = leaves.pop().expect("length checked to be 1");
            return Effect {
                leaves: vec![leaf.relay(|stream| map_leaf(stream, f))],
            };
        }

        let f = Arc::new(Mutex::new(f));
        let mapped = leaves
            .into_iter()
            .map(|leaf| {
                let f = Arc::clone(&f);
                leaf.relay(|stream| {
                    map_leaf(stream, move |msg| {
                        // The mutex only lends `Sync` to the shared `Fn`; it
                        // guards no mutable state, so a poisoned lock carries
                        // no corrupted invariant. Recover the guard rather
                        // than panicking, which would otherwise turn one
                        // leaf's panic into a misleading "mutex poisoned"
                        // cascade across its sibling leaves.
                        let guard = f.lock().unwrap_or_else(PoisonError::into_inner);
                        (*guard)(msg)
                    })
                })
            })
            .collect();
        Effect { leaves: mapped }
    }

    pub(super) const fn is_none(&self) -> bool {
        self.leaves.is_empty()
    }

    pub(super) const fn is_some(&self) -> bool {
        !self.leaves.is_empty()
    }

    // Observe the leaf count so tests in `command.rs` can pin down nested-batch
    // flattening. Not needed by non-test builds.
    #[cfg(test)]
    pub(super) fn leaf_count(&self) -> usize {
        self.leaves.len()
    }

    /// Hands the flat leaf sequence to `RuntimeCommandParts`, preserving
    /// `Command::batch`'s flattened declaration order (RFC 0008 §4.1).
    pub(super) fn into_leaves(self) -> Vec<Leaf<Msg>> {
        self.leaves
    }

    // Test-only convenience: production consumers receive the leaves unfolded
    // through `into_leaves()` and fold at their own site.
    #[cfg(test)]
    pub(super) fn into_stream(self) -> Option<BoxStream<'static, Action<Msg>>> {
        super::runtime_parts::fold_leaves(self.leaves.into_iter().map(|leaf| leaf.stream).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        future::pending,
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use futures::{StreamExt, poll};
    use tokio::time::advance;

    async fn drain<Msg>(stream: BoxStream<'static, Action<Msg>>) -> (Vec<Msg>, bool) {
        let mut messages = Vec::new();
        let mut quit = false;
        let mut stream = stream;
        while let Some(action) = stream.next().await {
            match action {
                Action::Message(msg) => messages.push(msg),
                Action::Quit => quit = true,
            }
        }
        (messages, quit)
    }

    #[test]
    fn test_effect_none_has_no_stream() {
        let effect = Effect::<i32>::none();

        assert!(effect.is_none());
        assert!(!effect.is_some());
        assert_eq!(effect.leaf_count(), 0);
        assert!(effect.into_stream().is_none());
    }

    #[test]
    fn test_effect_empty_batch_is_none() {
        let effect = Effect::<i32>::batch(Vec::new());

        assert!(effect.is_none());
        assert_eq!(effect.leaf_count(), 0);
    }

    #[test]
    fn test_effect_batch_of_all_none_is_none() {
        let effect = Effect::<i32>::batch(vec![Effect::none(), Effect::none()]);

        assert!(effect.is_none());
        assert!(!effect.is_some());
        assert_eq!(effect.leaf_count(), 0);
    }

    #[test]
    fn test_effect_map_over_none_is_none() {
        let effect = Effect::<i32>::none().map(|value| value * 2);

        assert!(effect.is_none());
        assert_eq!(effect.leaf_count(), 0);
    }

    #[test]
    fn test_effect_batch_drops_none_children_from_leaves() {
        let effect = Effect::batch(vec![
            Effect::none(),
            Effect::future(async { 1 }),
            Effect::none(),
            Effect::future(async { 2 }),
        ]);

        assert_eq!(effect.leaf_count(), 2);
    }

    #[test]
    fn test_effect_nested_batch_is_flattened() {
        let inner = Effect::batch(vec![
            Effect::future(async { 1 }),
            Effect::future(async { 2 }),
        ]);
        let effect = Effect::batch(vec![inner, Effect::future(async { 3 })]);

        // batch(batch(a, b), c) collapses to the flat leaf sequence [a, b, c].
        assert_eq!(effect.leaf_count(), 3);
    }

    #[test]
    fn test_effect_map_preserves_leaf_count() {
        let effect = Effect::batch(vec![
            Effect::future(async { 1 }),
            Effect::future(async { 2 }),
        ])
        .map(|value| value * 10);

        assert_eq!(effect.leaf_count(), 2);
    }

    #[tokio::test]
    async fn test_effect_single_leaf_into_stream() {
        let effect = Effect::future(async { 1 });

        assert_eq!(effect.leaf_count(), 1);
        let stream = effect.into_stream().expect("stream should exist");
        let (messages, quit) = drain(stream).await;

        assert_eq!(messages, vec![1]);
        assert!(!quit);
    }

    #[tokio::test]
    async fn test_effect_batch_combines_streams() {
        let effect = Effect::batch(vec![Effect::none(), Effect::future(async { 1 })]);

        let mut stream = effect.into_stream().expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(1)));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_effect_batch_delivers_all_leaves() {
        let effect = Effect::batch(vec![
            Effect::future(async { 1 }),
            Effect::future(async { 2 }),
            Effect::future(async { 3 }),
        ]);

        let stream = effect.into_stream().expect("stream should exist");
        let (mut messages, quit) = drain(stream).await;

        messages.sort_unstable();
        assert_eq!(messages, vec![1, 2, 3]);
        assert!(!quit);
    }

    #[tokio::test]
    async fn test_effect_map_over_batch_applies_to_every_leaf() {
        let effect = Effect::batch(vec![
            Effect::future(async { 1 }),
            Effect::future(async { 2 }),
            Effect::future(async { 3 }),
        ])
        .map(|value| value * 10);

        let stream = effect.into_stream().expect("stream should exist");
        let (mut messages, _) = drain(stream).await;

        messages.sort_unstable();
        assert_eq!(messages, vec![10, 20, 30]);
    }

    #[tokio::test]
    async fn test_effect_map_over_batch_preserves_quit() {
        let effect = Effect::batch(vec![
            Effect::future(async { 1 }),
            Effect::action(Action::Quit),
        ])
        .map(|value: i32| value * 10);

        let stream = effect.into_stream().expect("stream should exist");
        let (_, quit) = drain(stream).await;

        assert!(quit, "Quit should pass through map over a batch");
    }

    #[tokio::test]
    async fn test_effect_map_preserves_quit() {
        let effect = Effect::<i32>::action(Action::Quit).map(|value| value * 2);

        let mut stream = effect.into_stream().expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Quit));
    }

    #[tokio::test(start_paused = true)]
    async fn test_timeout_deadline_starts_on_first_poll() {
        let effect = Effect::future(pending::<i32>()).timeout(Duration::from_secs(1), || 99);
        let mut stream = effect.into_stream().expect("stream should exist");

        advance(Duration::from_secs(10)).await;
        assert!(poll!(stream.next()).is_pending());

        advance(Duration::from_millis(999)).await;
        assert!(poll!(stream.next()).is_pending());

        advance(Duration::from_millis(1)).await;
        assert!(matches!(stream.next().await, Some(Action::Message(99))));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn test_timeout_factory_is_shared_across_batch_and_accepts_move_only_state() {
        let effect = Effect::batch(vec![
            Effect::future(pending::<String>()),
            Effect::future(pending::<String>()),
        ])
        .timeout(Duration::from_secs(1), {
            let timeout = String::from("timed out");
            move || timeout
        });
        let mut stream = effect.into_stream().expect("stream should exist");

        assert!(poll!(stream.next()).is_pending());
        advance(Duration::from_secs(1)).await;

        assert!(matches!(
            stream.next().await,
            Some(Action::Message(message)) if message == "timed out"
        ));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn test_timeout_completion_does_not_consume_factory() {
        let called = Arc::new(AtomicBool::new(false));
        let called_by_timeout = Arc::clone(&called);
        let effect = Effect::future(async { 42 }).timeout(Duration::ZERO, move || {
            called_by_timeout.store(true, Ordering::SeqCst);
            99
        });
        let mut stream = effect.into_stream().expect("stream should exist");

        assert!(matches!(stream.next().await, Some(Action::Message(42))));
        assert!(stream.next().await.is_none());
        assert!(!called.load(Ordering::SeqCst));
    }

    #[tokio::test(start_paused = true)]
    async fn test_completed_batch_leaf_leaves_timeout_factory_for_pending_sibling() {
        let effect = Effect::batch([
            Effect::future(async { 42 }),
            Effect::future(pending::<i32>()),
        ])
        .timeout(Duration::from_secs(1), || 99);
        let mut stream = effect.into_stream().expect("stream should exist");

        assert!(matches!(stream.next().await, Some(Action::Message(42))));
        assert!(poll!(stream.next()).is_pending());
        advance(Duration::from_secs(1)).await;

        assert!(matches!(stream.next().await, Some(Action::Message(99))));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn test_timeout_passes_through_messages_before_deadline() {
        let effect = Effect::stream(stream::iter([1, 2, 3])).timeout(Duration::from_secs(1), || 99);
        let stream = effect.into_stream().expect("stream should exist");
        let (messages, quit) = drain(stream).await;

        assert_eq!(messages, vec![1, 2, 3]);
        assert!(!quit);
    }

    #[tokio::test(start_paused = true)]
    async fn test_timeout_quit_is_terminal_and_does_not_consume_factory() {
        let called = Arc::new(AtomicBool::new(false));
        let called_by_timeout = Arc::clone(&called);
        let effect =
            Effect::<i32>::action(Action::Quit).timeout(Duration::from_secs(1), move || {
                called_by_timeout.store(true, Ordering::SeqCst);
                99
            });
        let mut stream = effect.into_stream().expect("stream should exist");

        assert!(matches!(stream.next().await, Some(Action::Quit)));
        assert!(stream.next().await.is_none());
        assert!(!called.load(Ordering::SeqCst));
    }

    #[tokio::test(start_paused = true)]
    async fn test_quit_batch_leaf_leaves_timeout_factory_for_pending_sibling() {
        let effect = Effect::batch([
            Effect::<i32>::action(Action::Quit),
            Effect::future(pending::<i32>()),
        ])
        .timeout(Duration::from_secs(1), || 99);
        let mut stream = effect.into_stream().expect("stream should exist");

        assert!(matches!(stream.next().await, Some(Action::Quit)));
        assert!(poll!(stream.next()).is_pending());
        advance(Duration::from_secs(1)).await;

        assert!(matches!(stream.next().await, Some(Action::Message(99))));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn test_timeout_drops_inner_stream_and_never_polls_it_again() {
        struct DropMarker(Arc<AtomicBool>);

        impl Drop for DropMarker {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let dropped = Arc::new(AtomicBool::new(false));
        let polls = Arc::new(AtomicUsize::new(0));
        let marker = DropMarker(Arc::clone(&dropped));
        let polls_by_stream = Arc::clone(&polls);
        let pending = stream::poll_fn(move |_| {
            let _marker = &marker;
            polls_by_stream.fetch_add(1, Ordering::SeqCst);
            Poll::Pending::<Option<i32>>
        });
        let effect = Effect::stream(pending).timeout(Duration::from_secs(1), || 99);
        let mut stream = effect.into_stream().expect("stream should exist");

        assert!(poll!(stream.next()).is_pending());
        advance(Duration::from_secs(1)).await;
        assert!(matches!(stream.next().await, Some(Action::Message(99))));

        let polls_at_timeout = polls.load(Ordering::SeqCst);
        assert!(dropped.load(Ordering::SeqCst));
        assert!(stream.next().await.is_none());
        assert_eq!(polls.load(Ordering::SeqCst), polls_at_timeout);
    }

    #[test]
    fn test_timeout_over_none_is_inert_and_preserves_leaf_shape() {
        let effect = Effect::<i32>::none().timeout(Duration::ZERO, || 99);

        assert!(effect.is_none());
        assert_eq!(effect.leaf_count(), 0);
        assert!(effect.into_stream().is_none());
    }

    #[test]
    fn test_timeout_and_map_preserve_leaf_count() {
        let effect = Effect::batch(vec![
            Effect::future(async { 1 }),
            Effect::future(async { 2 }),
        ])
        .timeout(Duration::from_secs(1), || 99)
        .map(|message| message.to_string());

        assert_eq!(effect.leaf_count(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn test_child_timeouts_in_a_batch_are_independent() {
        let first = Effect::future(pending::<i32>()).timeout(Duration::from_secs(1), || 10);
        let second = Effect::future(pending::<i32>()).timeout(Duration::from_secs(2), || 20);
        let effect = Effect::batch([first, second]);
        let mut stream = effect.into_stream().expect("stream should exist");

        assert!(poll!(stream.next()).is_pending());
        advance(Duration::from_secs(1)).await;
        assert!(matches!(stream.next().await, Some(Action::Message(10))));
        advance(Duration::from_secs(1)).await;
        assert!(matches!(stream.next().await, Some(Action::Message(20))));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn test_nested_timeout_emits_only_the_earlier_message() {
        let effect = Effect::future(pending::<i32>())
            .timeout(Duration::from_secs(1), || 10)
            .timeout(Duration::from_secs(2), || 20);
        let mut stream = effect.into_stream().expect("stream should exist");

        assert!(poll!(stream.next()).is_pending());
        advance(Duration::from_secs(1)).await;
        assert!(matches!(stream.next().await, Some(Action::Message(10))));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn test_nested_simultaneous_timeouts_emit_exactly_one_message() {
        let effect = Effect::future(pending::<i32>())
            .timeout(Duration::from_secs(1), || 10)
            .timeout(Duration::from_secs(1), || 20);
        let mut stream = effect.into_stream().expect("stream should exist");

        assert!(poll!(stream.next()).is_pending());
        advance(Duration::from_secs(1)).await;

        assert!(matches!(
            stream.next().await,
            Some(Action::Message(10 | 20))
        ));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn test_elapsed_deadline_bounds_continuously_ready_stream_progress() {
        let effect = Effect::stream(stream::repeat(1)).timeout(Duration::from_secs(1), || 99);
        let mut stream = effect.into_stream().expect("stream should exist");

        assert!(matches!(stream.next().await, Some(Action::Message(1))));
        advance(Duration::from_secs(1)).await;

        let tied_output = stream.next().await;
        let item_won = matches!(tied_output.as_ref(), Some(Action::Message(1)));
        let deadline_won = matches!(tied_output.as_ref(), Some(Action::Message(99)));
        assert!(item_won || deadline_won);

        if item_won {
            // Passing through the tied item requires the deadline transition
            // on the very next poll, before another inner item can escape.
            assert!(matches!(stream.next().await, Some(Action::Message(99))));
        }
        assert!(stream.next().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn test_deadline_quit_tie_allows_either_terminal_outcome() {
        let timeout_called = Arc::new(AtomicBool::new(false));
        let called_by_timeout = Arc::clone(&timeout_called);
        let effect = Effect::<i32>::action(Action::Quit).timeout(Duration::ZERO, move || {
            called_by_timeout.store(true, Ordering::SeqCst);
            99
        });
        let mut stream = effect.into_stream().expect("stream should exist");

        let output = stream.next().await;
        let quit_won = matches!(output.as_ref(), Some(Action::Quit));
        let deadline_won = matches!(output.as_ref(), Some(Action::Message(99)));
        assert!(quit_won || deadline_won);
        assert_eq!(timeout_called.load(Ordering::SeqCst), deadline_won);
        assert!(stream.next().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn test_zero_timeout_is_non_panicking_and_termination_wins() {
        let effect = Effect::stream(stream::empty::<i32>()).timeout(Duration::ZERO, || 99);
        let mut stream = effect.into_stream().expect("stream should exist");

        assert!(stream.next().await.is_none());
    }
}
