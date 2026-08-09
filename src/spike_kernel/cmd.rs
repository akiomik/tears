//! Core protocol slice for the B-kernel prototype: scope paths, commands
//! with multi-keyed lowering inputs, subscription declarations backed by a
//! deterministic mock source, and the minimal `Reducer` / `Program`
//! traits plus a one-level scoping combinator.

use std::collections::VecDeque;
use std::fmt::Debug;
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use tokio::sync::Notify;

use super::lane::EffectCtx;

/// A structural scope segment. The spec's `ScopeValue` is any structurally
/// comparable value; the prototype narrows it to static strings, which is
/// enough for structural (type + value) equality over test topologies.
pub type Seg = &'static str;

/// An ordered structural scope path (spec §2 row 11's `ScopePath`).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct ScopePath(pub Vec<Seg>);

impl ScopePath {
    /// The root (empty) path.
    pub const fn root() -> Self {
        Self(Vec::new())
    }

    /// A single-segment path.
    pub fn seg(seg: Seg) -> Self {
        Self(vec![seg])
    }

    /// Structural prefix test: `self` is selected by a teardown of
    /// `prefix` iff its path starts with `prefix` segment-by-segment.
    pub fn starts_with(&self, prefix: &Self) -> bool {
        self.0.len() >= prefix.0.len() && self.0[..prefix.0.len()] == prefix.0[..]
    }

    /// Returns the path re-anchored under `seg` (the `.scoped` prefix
    /// modification, INV-ST2).
    #[must_use]
    pub fn prefixed(&self, seg: Seg) -> Self {
        let mut segments = Vec::with_capacity(self.0.len() + 1);
        segments.push(seg);
        segments.extend(self.0.iter().copied());
        Self(segments)
    }

    /// Human-readable form for ledger entries.
    pub fn display(&self) -> String {
        if self.0.is_empty() {
            "/".to_owned()
        } else {
            format!("/{}", self.0.join("/"))
        }
    }
}

/// Replacement policy for a keyed spawn whose full ID is already occupied
/// by a live run.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CancelPolicy {
    /// Revoke the in-flight run and start the new one (fresh token).
    CancelInFlight,
    /// Keep the in-flight run and suppress the new spawn.
    KeepInFlight,
}

/// A producer body: given the ingress context (which enforces the
/// gate -> reservation -> send -> commit path), returns the future the
/// runtime-owned task runs.
pub type EffectBody<M> = Box<dyn FnOnce(EffectCtx<M>) -> BoxFuture<'static, ()> + Send>;

/// One effect spawn: anonymous when `key` is `None`, keyed otherwise.
pub struct SpawnCmd<M> {
    /// Test-facing name; also the ledger vocabulary for this run.
    pub label: &'static str,
    /// Scope the run belongs to (prefixed by `scoped`).
    pub scope: ScopePath,
    /// Logical key for keyed (cancellable) spawns.
    pub key: Option<&'static str>,
    /// Replacement policy when the keyed slot is occupied.
    pub policy: CancelPolicy,
    /// The producer body.
    pub body: EffectBody<M>,
}

/// The prototype command carrier. `Batch` children keep their own spawn
/// keys (multi-keyed lowering — the spec's supersede of batch key
/// folding); `Teardown` is the manual scope-prefix primitive.
pub enum Cmd<M> {
    /// No operation.
    None,
    /// Controlled termination, applied synchronously at dispatch.
    Quit,
    /// Sequenced children; lowering flattens without folding keys.
    Batch(Vec<Self>),
    /// Spawn a runtime-owned effect run.
    Spawn(SpawnCmd<M>),
    /// Structural prefix teardown.
    Teardown(ScopePath),
}

impl<M> Cmd<M> {
    /// Applies the `.scoped(seg)` prefix modification: spawn scopes and
    /// teardown paths are re-anchored under `seg`; batches distribute
    /// (no folding); `Quit` and `None` are unaffected.
    #[must_use]
    pub fn scoped(self, seg: Seg) -> Self {
        match self {
            Self::None => Self::None,
            Self::Quit => Self::Quit,
            Self::Batch(children) => {
                Self::Batch(children.into_iter().map(|c| c.scoped(seg)).collect())
            }
            Self::Spawn(mut spawn) => {
                spawn.scope = spawn.scope.prefixed(seg);
                Self::Spawn(spawn)
            }
            Self::Teardown(path) => Self::Teardown(path.prefixed(seg)),
        }
    }
}

/// A deterministic, externally fed source (the application-side injection
/// surface B-2 allows): items are pushed by the test, the real forwarder
/// task polls them, and `close` ends the stream (natural finish).
pub struct MockSource<M> {
    inner: Arc<MockSourceInner<M>>,
}

struct MockSourceInner<M> {
    queue: Mutex<MockSourceState<M>>,
    notify: Notify,
}

struct MockSourceState<M> {
    items: VecDeque<M>,
    closed: bool,
}

impl<M> Clone for MockSource<M> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<M> Default for MockSource<M> {
    fn default() -> Self {
        Self {
            inner: Arc::new(MockSourceInner {
                queue: Mutex::new(MockSourceState {
                    items: VecDeque::new(),
                    closed: false,
                }),
                notify: Notify::new(),
            }),
        }
    }
}

impl<M> MockSource<M> {
    /// Queues one item for the forwarder.
    pub fn push(&self, item: M) {
        self.inner
            .queue
            .lock()
            .expect("mock source lock")
            .items
            .push_back(item);
        self.inner.notify.notify_one();
    }

    /// Ends the stream: the forwarder observes `None` and finishes
    /// naturally once the queue drains.
    pub fn close(&self) {
        self.inner.queue.lock().expect("mock source lock").closed = true;
        self.inner.notify.notify_one();
    }

    /// Next item, or `None` once closed and drained. Parks between items.
    pub async fn next(&self) -> Option<M> {
        loop {
            {
                let mut state = self.inner.queue.lock().expect("mock source lock");
                if let Some(item) = state.items.pop_front() {
                    return Some(item);
                }
                if state.closed {
                    return None;
                }
            }
            self.inner.notify.notified().await;
        }
    }
}

/// One subscription declaration: identity is the structural pair
/// `(scope, key)`; the run is a real forwarder task over `source`.
pub struct SubDecl<M> {
    /// Identity key within its scope.
    pub key: &'static str,
    /// Scope the declaration belongs to (prefixed by combinators).
    pub scope: ScopePath,
    /// The deterministic source the forwarder polls.
    pub source: MockSource<M>,
}

impl<M> SubDecl<M> {
    /// Declares `key` at the root scope over `source`.
    pub fn new(key: &'static str, source: &MockSource<M>) -> Self {
        Self {
            key,
            scope: ScopePath::root(),
            source: source.clone(),
        }
    }

    /// Full structural identity.
    pub fn full_id(&self) -> (ScopePath, &'static str) {
        (self.scope.clone(), self.key)
    }

    /// The `.scoped(seg)` prefix modification for declarations.
    #[must_use]
    pub fn scoped(mut self, seg: Seg) -> Self {
        self.scope = self.scope.prefixed(seg);
        self
    }
}

/// State transition + subscription declaration unit (spec §1.1, narrowed:
/// messages are a single concrete type per program, `Debug` for ledgers).
pub trait Reducer {
    /// Owned state.
    type State;
    /// Message type.
    type Msg: Send + Debug + 'static;

    /// Applies one message; returns the lowered-parts carrier.
    fn reduce(&self, state: &mut Self::State, msg: Self::Msg) -> Cmd<Self::Msg>;

    /// Pure declaration of desired subscription runs.
    fn subscriptions(&self, _state: &Self::State) -> Vec<SubDecl<Self::Msg>> {
        Vec::new()
    }
}

/// Root unit the kernel drives: `Reducer` + init + view.
pub trait Program: Reducer {
    /// Init input.
    type Flags;

    /// Initial state and command (runs at Boot, not at construction).
    fn init(&self, flags: Self::Flags) -> (Self::State, Cmd<Self::Msg>);

    /// Renders into the headless sink.
    fn view(&self, state: &Self::State, sink: &mut ViewSink);
}

/// Headless render target (placeholder: the prototype's views render
/// nothing observable).
pub struct ViewSink;

/// One-level scoping combinator (the parent-child slice of the spec's
/// composition layer): routes child messages through `route`, scopes the
/// child's commands and declarations under `seg`. The child shares the
/// parent's message type; the message-value `embed`/`extract` mapping of
/// the full combinator API is out of the prototype's scope.
pub struct ScopedChild<P: Reducer, C: Reducer<Msg = P::Msg>> {
    /// Parent reducer (non-child messages).
    pub parent: P,
    /// Child reducer (routed messages).
    pub child: C,
    /// The structural boundary segment.
    pub seg: Seg,
    /// Mutable lens from parent state to child state.
    pub lens: fn(&mut P::State) -> &mut C::State,
    /// Shared lens for declaration aggregation.
    pub lens_ref: fn(&P::State) -> &C::State,
    /// Message routing predicate: `true` routes to the child.
    pub route: fn(&P::Msg) -> bool,
}

impl<P: Reducer, C: Reducer<Msg = P::Msg>> Reducer for ScopedChild<P, C> {
    type State = P::State;
    type Msg = P::Msg;

    fn reduce(&self, state: &mut Self::State, msg: Self::Msg) -> Cmd<Self::Msg> {
        if (self.route)(&msg) {
            self.child.reduce((self.lens)(state), msg).scoped(self.seg)
        } else {
            self.parent.reduce(state, msg)
        }
    }

    fn subscriptions(&self, state: &Self::State) -> Vec<SubDecl<Self::Msg>> {
        let mut decls = self.parent.subscriptions(state);
        decls.extend(
            self.child
                .subscriptions((self.lens_ref)(state))
                .into_iter()
                .map(|d| d.scoped(self.seg)),
        );
        decls
    }
}
