use std::sync::{Arc, Mutex, PoisonError};

use futures::{
    FutureExt, Stream, StreamExt,
    stream::{self, BoxStream, select_all},
};

use super::Action;

// Effects own and compose the asynchronous action stream; runtime directives
// stay separate because they describe how the runtime treats the update result.
//
// Rather than folding children into a single opaque stream at construction
// time, an effect keeps the flat sequence of leaf streams and only folds them
// (via `select_all`) at the `into_stream()` boundary. Keeping the leaves apart
// preserves each leaf's identity through `Command` composition, which is what a
// future per-leaf cancellation id would attach to.
//
// The empty case is a distinct `None` variant rather than an empty `Vec` so
// that `is_none()`/`is_some()` stay `const fn` (these back the public
// `Command::is_none`/`is_some`); `Vec::is_empty` only became usable in `const`
// in Rust 1.87, past this crate's MSRV. The invariant is therefore that
// `Leaves` is always non-empty.
//
// TODO(msrv >= 1.87): collapse this back to a plain
// `Vec<BoxStream<'static, Action<Msg>>>` and implement the `const`
// `is_none()`/`is_some()` with `Vec::is_empty`, dropping the `None` variant and
// its non-empty invariant.
//
// Future room: to carry per-leaf metadata, `Leaves` could hold
// `Vec<(LeafMeta, BoxStream<'static, Action<Msg>>)>` without reworking the fold
// at the `into_stream()` boundary.
pub(super) enum Effect<Msg: Send + 'static> {
    None,
    Leaves(Vec<BoxStream<'static, Action<Msg>>>),
}

impl<Msg: Send + 'static> Effect<Msg> {
    pub(super) const fn none() -> Self {
        Self::None
    }

    fn from_stream(stream: BoxStream<'static, Action<Msg>>) -> Self {
        Self::Leaves(vec![stream])
    }

    fn into_leaves(self) -> Vec<BoxStream<'static, Action<Msg>>> {
        match self {
            Self::None => Vec::new(),
            Self::Leaves(leaves) => leaves,
        }
    }

    pub(super) fn future(future: impl Future<Output = Msg> + Send + 'static) -> Self {
        Self::from_stream(future.into_stream().map(Action::Message).boxed())
    }

    pub(super) fn action(action: Action<Msg>) -> Self {
        Self::from_stream(stream::once(async move { action }).boxed())
    }

    pub(super) fn stream(stream: impl Stream<Item = Msg> + Send + 'static) -> Self {
        Self::from_stream(stream.map(Action::Message).boxed())
    }

    pub(super) fn batch(effects: impl IntoIterator<Item = Self>) -> Self {
        // Concatenate the children's leaves. Because every effect already holds
        // a flat leaf sequence, nested batches flatten automatically and
        // stream-less children contribute nothing. Collapse to `None` when no
        // child had a leaf so the non-empty `Leaves` invariant holds.
        let leaves: Vec<_> = effects.into_iter().flat_map(Self::into_leaves).collect();

        if leaves.is_empty() {
            Self::None
        } else {
            Self::Leaves(leaves)
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

        // Map each leaf on its own to preserve leaf count and order. A single
        // leaf moves `f` straight into its closure with no shared-ownership
        // cost (the pre-refactor path). Several leaves must share `f`: `Arc<F>`
        // alone would require `F: Sync`, but the public `map` bound is only
        // `Fn + Send`, so a `Mutex` supplies the needed `Sync`.
        match self {
            Self::None => Effect::None,
            Self::Leaves(mut leaves) if leaves.len() == 1 => {
                let leaf = leaves.pop().expect("length checked to be 1");
                Effect::Leaves(vec![map_leaf(leaf, f)])
            }
            Self::Leaves(leaves) => {
                let f = Arc::new(Mutex::new(f));
                let mapped = leaves
                    .into_iter()
                    .map(|leaf| {
                        let f = Arc::clone(&f);
                        map_leaf(leaf, move |msg| {
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
                    .collect();
                Effect::Leaves(mapped)
            }
        }
    }

    pub(super) const fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }

    pub(super) const fn is_some(&self) -> bool {
        matches!(self, Self::Leaves(_))
    }

    // Observe the leaf count so tests in `command.rs` can pin down nested-batch
    // flattening. Not needed by non-test builds.
    #[cfg(test)]
    pub(super) fn leaf_count(&self) -> usize {
        match self {
            Self::None => 0,
            Self::Leaves(leaves) => leaves.len(),
        }
    }

    pub(super) fn into_stream(self) -> Option<BoxStream<'static, Action<Msg>>> {
        // Fold the leaves back into one stream here, at the boundary. A single
        // leaf is returned as-is (a `select_all` over one stream is observably
        // identical); `None` yields no stream, which the runtime treats as no
        // work to spawn. `Leaves` is always non-empty by invariant.
        match self {
            Self::None => None,
            Self::Leaves(mut leaves) if leaves.len() == 1 => leaves.pop(),
            Self::Leaves(leaves) => Some(select_all(leaves).boxed()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

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
}
