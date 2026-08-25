//! Deterministic testing support for [`Application`]s.
//!
//! [`TestStore`] drives an application's `update` transitions, immediately
//! ready effects, and — through a store-owned controlled time context —
//! time-dependent command effects, synchronously, deterministically, and
//! with no wall-clock waiting (RFC 0008). A test constructs the store from
//! the application's flags, scripts messages with [`TestStore::send`],
//! moves virtual time with [`TestStore::advance`], asserts effect output
//! with [`TestStore::receive`] (or [`TestStore::receive_matching`] /
//! [`TestStore::receive_quit`]), and closes the run with
//! [`TestStore::finish`], which fails the test if any deliverable output or
//! unfinished effect remains unaccounted for.
//!
//! # Scope and limitations
//!
//! - **Store-owned controlled time context**: [`TestStore::new`] builds a
//!   paused, single-threaded, time-only executor context with no public
//!   accessor; the store's own operations alone move its clock, and effect
//!   code that reaches the context's facilities from inside a poll is
//!   outside the store's contract (RFC 0008 §4.3; RFC 0009 §3.2). Construction panics if a
//!   Tokio runtime is already entered, so tests run on a plain `#[test]`,
//!   never `#[tokio::test]` (RFC 0008 INV-T10): an ambient runtime's
//!   pausedness cannot be verified, and its clock is not the store's clock.
//! - **Time-dependent command effects are driven by `advance`**: a
//!   [`Command::timeout`] or retry-backoff leaf stays pending until
//!   [`TestStore::advance`] moves the store's virtual clock to its deadline,
//!   then delivers through the ordinary `receive` flow. Virtual time never
//!   moves on its own.
//! - **I/O is out of scope**: the store's context enables no I/O driver, so
//!   a leaf that needs one fails the test at its first poll with the
//!   underlying missing-reactor panic. Feed results through test-controlled
//!   sources instead; a keyed I/O leaf can still be *cancelled*, because
//!   cancellation removes it without polling.
//! - **Subscription sources are never executed**: the store observes the
//!   *declared* subscription set via [`TestStore::subscription_ids`] and
//!   starts nothing. Source lifecycle stays covered by the runtime's tests.
//! - **Runtime integration contracts are not exercised**: channel capacities,
//!   backpressure, batching, and scheduling are properties of the runtime
//!   event loop, which the store replaces. Passing a `TestStore` test proves
//!   nothing about them.
//!
//! # Delivery order
//!
//! Within one effect leaf, messages are delivered in stream order. Across
//! leaves, the store delivers from the earliest-enqueued leaf that is
//! currently deliverable (init command first, then each step's command;
//! within one command, leaves in [`Command::batch`]'s flattened declaration
//! order). This canonical cross-leaf order is **the store's own contract**:
//! the runtime merges sibling leaves through an unordered select and pins no
//! cross-leaf delivery order, so a test asserting an interleaving across
//! sibling leaves asserts the store's linearization and must not be cited as
//! evidence of runtime ordering.
//!
//! # Deterministic time without `TestStore`
//!
//! `TestStore` covers `update` transitions and command effects. For what it
//! never executes — subscription sources, and the `http` feature's
//! staleness and cache-eviction behavior (`QueryConfig`'s
//! `stale_time`/`cache_time`) —
//! the same determinism is available directly from the executor: tears
//! reads time exclusively through the virtualizable clock (`tokio::time`;
//! RFC 0009), so a paused runtime makes every tears time read virtual with
//! no tears-side configuration.
//!
//! 1. Enable tokio's `test-util` feature in your dev-dependencies:
//!
//!    ```toml
//!    [dev-dependencies]
//!    tokio = { version = "1", features = ["macros", "rt", "test-util"] }
//!    ```
//!
//! 2. Run the test on a paused single-threaded runtime:
//!    `#[tokio::test(start_paused = true)]`. (`#[tokio::test]` uses the
//!    `current_thread` flavor by default, the only flavor that supports
//!    pausing; the production multi-thread runtime cannot be paused.)
//!
//! 3. Move virtual time explicitly with `tokio::time::advance`. No
//!    time-gated behavior fires before its deadline, and once an advance
//!    reaches a deadline the gated behavior becomes ready without
//!    wall-clock waiting (RFC 0009 §3.2).
//!
//! **Auto-advance versus real I/O**: a paused runtime auto-advances the
//! clock to the earliest pending timer deadline whenever the executor is
//! idle. Awaiting real network I/O under a paused runtime therefore lets
//! the executor judge itself idle and jump the clock to the next timer
//! before the I/O completes — a test exercising HTTP
//! `stale_time`/`cache_time` behavior must drive time and I/O explicitly
//! (feed responses through test-controlled sources, then advance) rather
//! than awaiting a live socket.
//!
//! `TestStore` itself needs none of this: it owns its paused context and
//! is constructed on a plain `#[test]`, never inside `#[tokio::test]`,
//! paused or not.
//!
//! [`Command::batch`]: crate::Command::batch
//! [`Command::timeout`]: crate::Command::timeout

// The stage-3 driving surface (RFC 0008's amendment, RFC 0014 §7.2). It
// drives the reducer-first kernel, so it stays crate-private for as long as
// that kernel does; the non-executing store above is unaffected by it.
pub(crate) mod driver;

use std::fmt::Debug;
use std::task::Poll;
use std::thread;
use std::time::Duration;

use futures::stream::BoxStream;
use tokio::runtime::{Builder, Handle, Runtime};
use tokio::time;

use crate::application::Application;
use crate::command::{Action, CancelPolicy, CommandId, RuntimeCommandParts, SpawnEntry};
use crate::noop_waker::noop_context;
use crate::structural_key::ScopePath;
use crate::subscription::core::SubscriptionId;

/// One undelivered effect leaf, held at its enqueue position.
struct PendingLeaf<Msg> {
    /// The leaf's stream; `None` once it has been observed exhausted.
    stream: Option<BoxStream<'static, Action<Msg>>>,
    /// Output yielded by an earlier check's poll but not yet delivered,
    /// waiting at this leaf's canonical position as its next deliverable
    /// output. At most one item can be buffered: the buffering sites are
    /// the keyed-intake reconciliation poll (which stops at its first
    /// yield) and `advance`'s anchoring scan (which skips already-buffered
    /// leaves), and delivery scans take the buffer before polling again.
    buffered: Option<Action<Msg>>,
    /// The cancellation id this carrier runs under, if keyed.
    key: Option<CommandId>,
    /// Where the carrier is placed — what a teardown prefix selects it by
    /// (RFC 0014 §4.1). Every carrier has one; an unscoped carrier's is the
    /// root, which only a root-wide teardown covers.
    scope: ScopePath,
    /// Zero-based enqueue position over the store's lifetime, for
    /// diagnostics.
    position: usize,
}

impl<Msg: Send + 'static> PendingLeaf<Msg> {
    /// Polls the leaf exactly once with a waker whose wake-ups are not
    /// honored within the call (RFC 0008 §4.1's poll budget). An exhausted
    /// leaf reports completion without being polled again.
    fn poll_once(&mut self) -> Poll<Option<Action<Msg>>> {
        let Some(stream) = self.stream.as_mut() else {
            return Poll::Ready(None);
        };
        let mut context = noop_context();
        let poll = stream.as_mut().poll_next(&mut context);
        if matches!(poll, Poll::Ready(None)) {
            self.stream = None;
        }
        poll
    }
}

/// Deterministic test harness for an [`Application`] (RFC 0008).
///
/// Drives `update`, immediately ready effects, and — via
/// [`advance`](Self::advance) over a store-held controlled time context —
/// time-dependent command effects, synchronously and without wall-clock
/// waiting (see the [module documentation](self) for the full scope).
///
/// Assertions are exhaustive: deliverable output that a test never receives,
/// or an effect stream never driven to completion, fails the test at
/// [`receive`](Self::receive)-time, at [`finish`](Self::finish), or when the
/// store is dropped. The one carve-out mirrors the runtime's shutdown
/// contract: output remaining after an observed quit is legally discarded.
///
/// # Examples
///
/// ```
/// use ratatui::Frame;
/// use tears::prelude::*;
/// use tears::testing::TestStore;
///
/// #[derive(Debug, PartialEq)]
/// enum Message {
///     Loaded(u32),
///     Refresh,
/// }
///
/// struct Counter {
///     value: u32,
/// }
///
/// impl Application for Counter {
///     type Message = Message;
///     type Flags = u32;
///
///     fn new(initial: u32) -> (Self, Command<Message>) {
///         let load = Command::perform(async { 41 }, Message::Loaded);
///         (Counter { value: initial }, load.into())
///     }
///
///     fn update(&mut self, msg: Message) -> Command<Message> {
///         match msg {
///             Message::Loaded(value) => {
///                 self.value = value;
///                 Command::none()
///             }
///             Message::Refresh => Command::message(Message::Loaded(42)).into(),
///         }
///     }
///
///     fn view(&self, _frame: &mut Frame<'_>) {}
///
///     fn subscriptions(&self) -> Vec<Subscription<Message>> {
///         vec![]
///     }
/// }
///
/// let mut store = TestStore::<Counter>::new(0);
/// store.receive(Message::Loaded(41));
/// assert_eq!(store.state().value, 41);
///
/// store.send(Message::Refresh);
/// store.receive(Message::Loaded(42));
/// assert_eq!(store.state().value, 42);
/// store.finish();
/// ```
pub struct TestStore<App: Application>
where
    App::Message: Debug,
{
    app: App,
    /// The controlled time context (RFC 0009 §3.2): a current-thread
    /// executor context with the clock started paused and no I/O driver.
    /// Every poll under §4.1's budget enters it, and among the store's own
    /// operations only `advance` moves its clock (RFC 0008 INV-T12;
    /// clock-manipulating effects are §4.3's negative space).
    context: Runtime,
    /// Undelivered effect leaves in enqueue order.
    pending: Vec<PendingLeaf<App::Message>>,
    redraw_requested: bool,
    /// Where the store is between running and a quit the test has assented
    /// to. Three states rather than two booleans, because the middle one is
    /// real: a quit applies at its dispatch and is only *observed* later, and
    /// the two facts constrain different calls. Two independent flags could
    /// represent "observed but never applied", which is not a state this
    /// store has.
    quit: QuitState,
    /// Total leaves ever enqueued; the next leaf's enqueue position.
    enqueued_leaves: usize,
    /// Set by [`TestStore::finish`] so the drop check does not run twice.
    finished: bool,
}

#[expect(
    clippy::panic,
    reason = "assertion failures in a test harness are panics by design"
)]
impl<App: Application> TestStore<App>
where
    App::Message: Debug,
{
    /// Runs [`Application::new`] with `flags` and enqueues the init command.
    ///
    /// Exhaustiveness applies from construction: the init command's
    /// deliverable output is held to the same `receive*` / [`finish`] / drop
    /// accounting as any later step's.
    ///
    /// # Panics
    ///
    /// Panics if called while a Tokio runtime is already entered — for
    /// example, from inside `#[tokio::test]` (RFC 0008 §4.3, INV-T10). The
    /// store owns its controlled time context, and that context must be the
    /// only executor context in play: an ambient runtime's pausedness and
    /// thread model cannot be verified, and its clock is not the store's
    /// clock — so tests run on a plain `#[test]`.
    ///
    /// [`finish`]: Self::finish
    #[must_use]
    #[track_caller]
    pub fn new(flags: App::Flags) -> Self {
        // INV-T10: checked before any other construction work, the store's
        // own controlled context included.
        assert!(
            Handle::try_current().is_err(),
            "TestStore::new: a Tokio runtime is already entered; TestStore owns its \
             controlled time context and must be constructed on a plain #[test], not \
             #[tokio::test] (RFC 0008 §4.3)"
        );
        // The controlled time context (RFC 0009 §3.2): current-thread, clock
        // started paused, time driver only — no I/O (RFC 0008 §4.3).
        let context = Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .expect("controlled time context construction should not fail");
        let (app, init_command) = App::new(flags);
        let mut store = Self {
            app,
            context,
            pending: Vec::new(),
            // Placeholder only: enqueue_command below overwrites this with
            // the init command's folded directive (§5.2).
            redraw_requested: true,
            quit: QuitState::Running,
            enqueued_leaves: 0,
            finished: false,
        };
        store.enqueue_command(init_command.into_runtime_parts());
        store
    }

    /// The application state, for plain assertions.
    pub const fn state(&self) -> &App {
        &self.app
    }

    /// Applies `msg` through [`Application::update`] and enqueues the
    /// returned command's effects.
    ///
    /// `send` is one synchronous `update` call plus bookkeeping: it spawns no
    /// task and delivers no pending output. Deliverable output left by
    /// earlier steps does not block a `send` — it stays in place for a later
    /// [`receive`](Self::receive) (or is caught by [`finish`](Self::finish)
    /// or drop) — which lets a scripted `send` supersede or cancel an earlier
    /// step's not-yet-received keyed output the way the runtime's
    /// shared-first schedule does.
    ///
    /// # Panics
    ///
    /// Fails the test if quit has already been observed via
    /// [`receive_quit`](Self::receive_quit); a keyed-intake reconciliation
    /// poll can additionally surface a leaf's own poll failure (an
    /// I/O-dependent leaf's missing-reactor panic, RFC 0008 §4.3).
    #[track_caller]
    pub fn send(&mut self, msg: App::Message) {
        self.assert_running("send");
        self.apply_update(msg);
    }

    /// Advances the store's virtual clock by `duration`.
    ///
    /// Anchors first, then moves time (RFC 0008 §3.2, §4.3, INV-T13): every
    /// pending leaf not already holding buffered output is polled exactly
    /// once, in enqueue order — so a
    /// [`Command::timeout`](crate::Command::timeout) deadline, created
    /// at its leaf's first poll, anchors at the leaf's enqueue-time virtual
    /// now, while a wait that starts mid-leaf (a retry backoff) begins at
    /// the poll that observes the failed attempt — and only then does the
    /// clock move
    /// forward, by exactly `duration`, with a timer-driver barrier: the
    /// executor is driven through the newly reached instant (never idled
    /// toward a future deadline), so already-registered timer entries whose
    /// deadlines the move reached fire. Delivers nothing and polls no leaf
    /// after the clock moves: output made ready by the advance is observed
    /// at the next
    /// [`receive`](Self::receive)-family call, [`finish`](Self::finish), or
    /// drop check that polls the leaf. `advance(Duration::ZERO)` is legal
    /// and anchors without moving time.
    ///
    /// # Panics
    ///
    /// Fails the test if quit has already been observed via
    /// [`receive_quit`](Self::receive_quit); the anchoring scan can also
    /// surface a leaf's own poll failure (an I/O-dependent leaf's
    /// missing-reactor panic, RFC 0008 §4.3), and a `duration` overflowing
    /// the clock's instant range panics.
    #[track_caller]
    pub fn advance(&mut self, duration: Duration) {
        self.assert_running("advance");
        {
            // The anchoring scan (INV-T13): one poll per pending leaf,
            // before the clock moves; a yield is buffered at the leaf's
            // canonical position like a reconciliation yield (§5.1).
            let _context = self.context.enter();
            for leaf in &mut self.pending {
                if leaf.buffered.is_some() {
                    continue;
                }
                if let Poll::Ready(Some(action)) = leaf.poll_once() {
                    leaf.buffered = Some(action);
                }
            }
        }
        self.pending
            .retain(|leaf| leaf.stream.is_some() || leaf.buffered.is_some());
        // The timer-driver barrier (§3.2): block_on drives the executor
        // through the newly reached instant, firing already-registered
        // timer entries — moving the paused clock alone would leave them
        // unfired and Pending under the manual polls above. Tasks spawned
        // onto the context may receive incidental polls here (§4.3's
        // negative space).
        self.context.block_on(time::advance(duration));
    }

    /// Asserts that the next deliverable output is a message equal to
    /// `expected`, then applies it through [`Application::update`].
    ///
    /// # Panics
    ///
    /// Fails the test on a mismatch, if the next deliverable output is a
    /// quit request (assert that with [`receive_quit`](Self::receive_quit)),
    /// if nothing is deliverable, or if quit has already been observed.
    #[track_caller]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "the expected value is consumed by the assertion (RFC 0008 §2.1)"
    )]
    pub fn receive(&mut self, expected: App::Message)
    where
        App::Message: PartialEq,
    {
        self.assert_running("receive");
        let actual = self.next_message("receive");
        assert!(
            actual == expected,
            "TestStore::receive: message mismatch\n  expected: {expected:?}\n    actual: {actual:?}"
        );
        self.apply_update(actual);
    }

    /// Like [`receive`](Self::receive), but asserts via a predicate;
    /// requires no `PartialEq`.
    ///
    /// # Panics
    ///
    /// Fails the test when the predicate rejects the delivered message, and
    /// in every case [`receive`](Self::receive) fails other than a value
    /// mismatch.
    #[track_caller]
    pub fn receive_matching(&mut self, matches: impl FnOnce(&App::Message) -> bool) {
        self.assert_running("receive_matching");
        let actual = self.next_message("receive_matching");
        assert!(
            matches(&actual),
            "TestStore::receive_matching: predicate rejected the delivered message: {actual:?}"
        );
        self.apply_update(actual);
    }

    /// Asserts that the next deliverable output is a quit request and puts
    /// the store into the quit state.
    ///
    /// After it succeeds, the store mirrors the runtime's shutdown contract:
    /// remaining undelivered output is legally discarded — the
    /// [`finish`](Self::finish) and drop checks poll nothing and pass — and
    /// further [`send`](Self::send)/[`advance`](Self::advance)/`receive*`
    /// calls fail because the
    /// application would no longer be running. [`state`](Self::state),
    /// [`redraw_requested`](Self::redraw_requested),
    /// [`subscription_ids`](Self::subscription_ids), and
    /// [`finish`](Self::finish) remain callable.
    ///
    /// # The two quit routes
    ///
    /// A quit an `update` returned was already applied, synchronously, at
    /// the dispatch that returned it (RFC 0014 §3.3) — this observes that
    /// application, and does not require the quit to be the next deliverable
    /// output, because it never queued behind any. A message the same
    /// command produced is therefore *not* a failure here, and still
    /// deliverable afterwards only in the sense that shutdown legally
    /// discards it.
    ///
    /// A producer-originated quit — one a spawned run emitted — does travel
    /// as output, so for that route this still requires the quit to be next.
    ///
    /// # Panics
    ///
    /// Fails the test if no quit was applied at a dispatch and the next
    /// deliverable output is a message, if neither is present, or if quit
    /// has already been observed.
    #[track_caller]
    pub fn receive_quit(&mut self) {
        // A second observation is a post-quit call like any other, and says
        // so in the same words, so a test asserting the family's diagnostic
        // does not have to special-case this one.
        assert!(
            self.quit != QuitState::QuitObserved,
            "TestStore::receive_quit: the application has quit; remaining output is discarded and no further steps run"
        );
        if self.quit == QuitState::QuitApplied {
            self.quit = QuitState::QuitObserved;
            return;
        }
        match self.next_deliverable("receive_quit") {
            Action::Quit => self.quit = QuitState::QuitObserved,
            Action::Message(msg) => panic!(
                "TestStore::receive_quit: expected a quit request, but the next deliverable output is a message: {msg:?}"
            ),
        }
    }

    /// Whether the command returned by the most recent step (a
    /// [`send`](Self::send), or the `update` call inside a
    /// [`receive`](Self::receive)/[`receive_matching`](Self::receive_matching))
    /// requested a redraw (RFC 0002).
    ///
    /// Before any step completes it reports the init command's folded
    /// directive — store-specific `Command` introspection, not a prediction
    /// of the runtime's first render, which always happens.
    /// [`receive_quit`](Self::receive_quit) applies no message and is not a
    /// step: after it, this keeps reporting the previous step's directive.
    #[must_use]
    pub const fn redraw_requested(&self) -> bool {
        self.redraw_requested
    }

    /// The [`SubscriptionId`]s the application currently declares, in
    /// declaration order, deduplicated by RFC 0005 §3.5's
    /// first-occurrence-stable rule: equal ids collapse to their first
    /// occurrence, at that occurrence's original position (`[A, B, A]`
    /// becomes `[A, B]`).
    ///
    /// This is the same *desired set* the runtime's subscription
    /// reconciliation computes as its input — not a prediction of which ids
    /// it spawns or already has running. Pure observation: no source's
    /// stream is started, no reconciliation machinery runs, and no
    /// duplicate-ignored warning is emitted (that event belongs to the
    /// runtime's reconciliation, which this call never invokes).
    #[must_use]
    pub fn subscription_ids(&self) -> Vec<SubscriptionId> {
        let mut ids: Vec<SubscriptionId> = Vec::new();
        for subscription in self.app.subscriptions() {
            let id = subscription.id();
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
        ids
    }

    /// Consumes the store, failing the test if deliverable output or an
    /// unfinished effect remains and quit was not observed.
    ///
    /// Dropping an unfinished store runs the same check (except while the
    /// thread is already panicking), so a test that forgets `finish` cannot
    /// silently leak output; `finish` remains the recommended spelling
    /// because its failure points at the right line.
    ///
    /// # Panics
    ///
    /// Fails the test if, with quit unobserved, a deliverable message or
    /// quit request remains or any pending effect leaf has not been driven
    /// to completion. After an observed quit it polls nothing and passes.
    #[track_caller]
    pub fn finish(mut self) {
        self.finished = true;
        self.check_exhaustive("finish");
    }

    /// One synchronous `update` plus command intake — the shared tail of
    /// `send` and the `receive*` deliveries.
    fn apply_update(&mut self, msg: App::Message) {
        let command = self.app.update(msg);
        self.enqueue_command(command.into_runtime_parts());
    }

    /// Accepts one command through the decomposition boundary the kernel
    /// consumes (RFC 0008 INV-T3), applying RFC 0014 §3.4's phase order:
    /// directives, then the cancel phase (explicit cancels and teardown
    /// prefixes) before *every* spawn of the same command, then the spawn
    /// phase in flattened declaration order, then the quit if one was
    /// carried.
    fn enqueue_command(&mut self, parts: RuntimeCommandParts<App::Message>) {
        let parts = parts.into_kernel_parts();
        self.redraw_requested = parts.redraw;

        for id in &parts.cancels {
            self.cancel_id(id);
        }
        for prefix in &parts.teardowns {
            self.teardown_under(prefix);
        }

        for spawn in parts.spawns {
            self.admit(spawn);
        }

        if parts.quit_now {
            self.quit = QuitState::QuitApplied;
        }
    }

    /// One carrier's admission decision, made per carrier rather than per
    /// command: a batch's children each lower to their own keyed entry, so
    /// two same-key children in one command apply in declaration order as
    /// two consecutive dispatches, the second replacing the first under its
    /// own policy (RFC 0014 §3.4).
    fn admit(&mut self, spawn: SpawnEntry<App::Message>) {
        let Some(key) = spawn.key else {
            self.push_leaf(None, spawn.scope, spawn.stream);
            return;
        };

        match key.policy {
            CancelPolicy::CancelInFlight => {
                // Supersede: the occupant's undelivered output — buffered
                // messages and quit requests alike — can no longer be
                // delivered (RFC 0003 INV-3, INV-6, INV-9 as RFC 0014 §3.1's
                // revocation succeeds them). No reconciliation poll: the
                // outcome does not depend on the occupant's state, which also
                // lets a reactor-dependent occupant be superseded without
                // polling it.
                self.cancel_id(&key.id);
                self.push_leaf(Some(&key.id), spawn.scope, spawn.stream);
            }
            CancelPolicy::KeepInFlight => {
                // Keyed-intake reconciliation: the admission decision reads
                // the reconciled occupancy (RFC 0003 INV-5, INV-7).
                if self.reconcile_is_occupied(&key.id) {
                    drop(spawn.stream);
                } else {
                    self.push_leaf(Some(&key.id), spawn.scope, spawn.stream);
                }
            }
        }
    }

    /// Drops every carrier placed under `prefix`, of every kind and
    /// regardless of key — the store's half of RFC 0014 §4.1's selection.
    fn teardown_under(&mut self, prefix: &ScopePath) {
        self.pending.retain(|leaf| !leaf.scope.starts_with(prefix));
    }

    /// Drops every leaf keyed under `id`, discarding streams and buffered
    /// output alike — quit requests included (RFC 0003 INV-3, INV-4, INV-9).
    /// Idempotent.
    fn cancel_id(&mut self, id: &CommandId) {
        self.pending.retain(|leaf| leaf.key.as_ref() != Some(id));
    }

    /// The store's analogue of the runtime's pre-spawn reap-and-sample
    /// (RFC 0003 §4.2): an id is occupied while its current run may still
    /// deliver output, and released once every one of the run's leaves has
    /// been observed exhausted.
    fn reconcile_is_occupied(&mut self, id: &CommandId) -> bool {
        // Buffered output occupies (INV-6); nothing is polled.
        if self
            .pending
            .iter()
            .any(|leaf| leaf.key.as_ref() == Some(id) && leaf.buffered.is_some())
        {
            return true;
        }

        // Poll the occupant's remaining leaves in enqueue order, stopping at
        // the first that shows the run still open.
        let _context = self.context.enter();
        for leaf in &mut self.pending {
            if leaf.key.as_ref() != Some(id) || leaf.stream.is_none() {
                continue;
            }
            match leaf.poll_once() {
                Poll::Ready(Some(action)) => {
                    // Buffered at this leaf's canonical position as its next
                    // deliverable output; buffered output occupies (INV-6).
                    leaf.buffered = Some(action);
                    return true;
                }
                Poll::Ready(None) => {}
                // A still-open stream remains occupied (INV-7).
                Poll::Pending => return true,
            }
        }

        // Every remaining leaf completed: the id is released (INV-7).
        false
    }

    fn push_leaf(
        &mut self,
        key: Option<&CommandId>,
        scope: ScopePath,
        stream: BoxStream<'static, Action<App::Message>>,
    ) {
        let position = self.enqueued_leaves;
        self.enqueued_leaves += 1;
        self.pending.push(PendingLeaf {
            stream: Some(stream),
            buffered: None,
            key: key.cloned(),
            scope,
            position,
        });
    }

    #[track_caller]
    fn assert_running(&self, method: &str) {
        match self.quit {
            QuitState::Running => {}
            QuitState::QuitApplied => panic!(
                "TestStore::{method}: a quit was applied at its dispatch and no later input can \
                 intervene before termination; observe it with TestStore::receive_quit"
            ),
            QuitState::QuitObserved => panic!(
                "TestStore::{method}: the application has quit; remaining output is discarded and no further steps run"
            ),
        }
    }

    #[track_caller]
    fn next_message(&mut self, method: &str) -> App::Message {
        match self.next_deliverable(method) {
            Action::Message(msg) => msg,
            Action::Quit => panic!(
                "TestStore::{method}: the next deliverable output is a quit request; assert it with TestStore::receive_quit"
            ),
        }
    }

    /// Selects the next deliverable output under the canonical order
    /// (RFC 0008 §4.2): walk the pending leaves in enqueue order — one poll
    /// per reached leaf — and stop at the first deliverable one.
    #[track_caller]
    fn next_deliverable(&mut self, method: &str) -> Action<App::Message> {
        let mut found = None;
        let mut any_open = false;
        let context = self.context.enter();
        for leaf in &mut self.pending {
            if let Some(action) = leaf.buffered.take() {
                found = Some(action);
                break;
            }
            match leaf.poll_once() {
                Poll::Ready(Some(action)) => {
                    found = Some(action);
                    break;
                }
                Poll::Ready(None) => {}
                Poll::Pending => any_open = true,
            }
        }
        drop(context);
        // Drop leaves observed exhausted so long scripts do not rescan them;
        // diagnostics carry their own enqueue positions, so removal is
        // invisible to delivery order and error messages.
        self.pending
            .retain(|leaf| leaf.stream.is_some() || leaf.buffered.is_some());
        if let Some(action) = found {
            return action;
        }
        assert!(
            !any_open,
            "TestStore::{method}: no deliverable output: effects are pending but none is ready"
        );
        panic!("TestStore::{method}: no deliverable output: no pending effects");
    }

    /// The exhaustiveness check behind [`finish`](Self::finish) and the drop
    /// check (RFC 0008 §6): scan in enqueue order — one poll per reached
    /// leaf — and fail at the first deliverable output or unfinished leaf.
    /// After an observed quit, polls nothing and passes.
    #[track_caller]
    fn check_exhaustive(&mut self, site: &str) {
        enum Leak {
            Deliverable { rendered: String, position: usize },
            Unfinished { position: usize },
        }

        match self.quit {
            // Terminal: shutdown legally discards what is left, so this
            // polls nothing and passes (INV-T9).
            QuitState::QuitObserved => return,
            // Applied but never observed. The application stopped and the
            // test never said so, which is the same class of omission as
            // output that was never received — and the more misleading one,
            // because the test reads as though it ran to completion.
            QuitState::QuitApplied => panic!(
                "TestStore::{site}: a quit was applied at its dispatch and never observed; \
                 assert it with TestStore::receive_quit"
            ),
            QuitState::Running => {}
        }

        let mut leak = None;
        let _context = self.context.enter();
        for leaf in &mut self.pending {
            if let Some(action) = &leaf.buffered {
                leak = Some(Leak::Deliverable {
                    rendered: render_action(action),
                    position: leaf.position,
                });
                break;
            }
            match leaf.poll_once() {
                Poll::Ready(Some(action)) => {
                    leak = Some(Leak::Deliverable {
                        rendered: render_action(&action),
                        position: leaf.position,
                    });
                    break;
                }
                Poll::Ready(None) => {}
                Poll::Pending => {
                    leak = Some(Leak::Unfinished {
                        position: leaf.position,
                    });
                    break;
                }
            }
        }

        match leak {
            None => {}
            Some(Leak::Deliverable { rendered, position }) => panic!(
                "TestStore::{site}: deliverable output was never received: {rendered} (leaf enqueued at position {position})"
            ),
            Some(Leak::Unfinished { position }) => {
                // Count what the store can attest without further polls:
                // leaves not yet observed exhausted at the stopping point.
                let unfinished = self
                    .pending
                    .iter()
                    .filter(|leaf| leaf.stream.is_some())
                    .count();
                panic!(
                    "TestStore::{site}: {unfinished} effect leaf(s) not driven to completion; first still pending at enqueue position {position}"
                );
            }
        }
    }
}

impl<App: Application> Drop for TestStore<App>
where
    App::Message: Debug,
{
    fn drop(&mut self) {
        if self.finished || thread::panicking() {
            return;
        }
        self.check_exhaustive("drop check");
    }
}

/// How far a quit has got.
///
/// A quit is applied by the dispatch that returns it and observed by the test
/// afterwards, and everything in between is a state the store has to be able
/// to name: the application has stopped, but the test has not yet said so.
/// Calls that would drive the application further fail from
/// [`QuitApplied`](QuitState::QuitApplied) onward — that is what "no later
/// input can intervene between the update that returned it and termination"
/// means on this side of the seam — while the exhaustiveness checks treat an
/// unobserved quit as a leak, exactly as they treat undelivered output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QuitState {
    /// No quit has been applied.
    Running,
    /// A dispatch applied a quit; no `receive_quit` has observed it yet.
    QuitApplied,
    /// The test observed the quit. The store is terminal and the
    /// exhaustiveness checks pass without polling.
    QuitObserved,
}

/// Renders one action for a leak diagnostic; messages via `Debug`, quit
/// requests by name.
fn render_action<Msg: Debug>(action: &Action<Msg>) -> String {
    match action {
        Action::Message(msg) => format!("{msg:?}"),
        Action::Quit => "a quit request".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::any::Any;
    use std::future::pending;
    #[cfg(all(not(loom), unix))]
    use std::io::stdin;
    #[cfg(all(not(loom), not(unix)))]
    use std::net::UdpSocket as StdUdpSocket;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    use std::num::NonZeroUsize;

    use futures::channel::oneshot;
    use futures::stream;
    use ratatui::Frame;
    // Tokio's I/O resources are compiled out under `--cfg loom` (tokio gates
    // them), so the I/O-dependent leaf helper and its tests are too.
    #[cfg(all(not(loom), unix))]
    use tokio::io::unix::AsyncFd;
    #[cfg(all(not(loom), not(unix)))]
    use tokio::net::UdpSocket;
    use tracing::Level;

    use crate::command::effect_command::EffectCommand;
    use crate::command::{Command, RetryPolicy};
    use crate::subscription::core::Subscription;
    use crate::subscription::mock::MockSource;
    use crate::test_support::TraceRecorder;

    type UpdateFn<Msg> = Box<dyn FnMut(Msg) -> Command<Msg> + Send>;

    /// Test application whose `update` is supplied by the test as a closure
    /// and whose state is the `Debug` transcript of every applied message.
    struct Harness<Msg: Send + Debug + 'static> {
        log: Vec<String>,
        update: UpdateFn<Msg>,
    }

    struct HarnessFlags<Msg: Send + 'static> {
        init: Command<Msg>,
        update: UpdateFn<Msg>,
    }

    impl<Msg: Send + Debug + 'static> Application for Harness<Msg> {
        type Message = Msg;
        type Flags = HarnessFlags<Msg>;

        fn new(flags: HarnessFlags<Msg>) -> (Self, Command<Msg>) {
            (
                Self {
                    log: Vec::new(),
                    update: flags.update,
                },
                flags.init,
            )
        }

        fn update(&mut self, msg: Msg) -> Command<Msg> {
            self.log.push(format!("{msg:?}"));
            (self.update)(msg)
        }

        fn view(&self, _frame: &mut Frame<'_>) {}

        fn subscriptions(&self) -> Vec<Subscription<Msg>> {
            Vec::new()
        }
    }

    fn store_with<Msg: Send + Debug + 'static>(
        init: Command<Msg>,
        update: impl FnMut(Msg) -> Command<Msg> + Send + 'static,
    ) -> TestStore<Harness<Msg>> {
        TestStore::new(HarnessFlags {
            init,
            update: Box::new(update),
        })
    }

    #[derive(Debug, PartialEq)]
    enum Msg {
        N(u32),
        Keyed(u32),
        Start,
        StartRetry,
        Restart,
        TryKeep,
        Cancel,
        Unrelated,
        Loud,
        StartQuit,
    }

    /// Registers an I/O resource with the runtime's I/O driver — the step
    /// that panics inside the store's time-only context (RFC 0008 §4.3).
    ///
    /// Registration is deliberately the *first* fallible step: an already
    /// open descriptor is registered as it is, so no syscall of the leaf's
    /// own can fail ahead of the driver lookup. A leaf that opens its own
    /// socket first is not so ordered — when the socket syscall fails
    /// synchronously (a sandbox denying networking, an exhausted descriptor
    /// table) the leaf completes with `Err` without ever reaching the
    /// driver, and its mapped message is delivered instead of the expected
    /// panic.
    #[cfg(all(not(loom), unix))]
    fn register_with_io_driver() {
        drop(AsyncFd::new(stdin()).expect(
            "registering an fd should not have reached a fallible step in the store's context",
        ));
    }

    /// The non-Unix counterpart: no descriptor-registration API is
    /// available there, so the resource is created first — a creation
    /// failure panics here, with a message of its own, instead of
    /// completing the leaf with a delivered message. Note that CI lints
    /// only on Linux, so this branch is compiled by the Windows test job
    /// but never clippy-checked.
    #[cfg(all(not(loom), not(unix)))]
    fn register_with_io_driver() {
        let socket =
            StdUdpSocket::bind("127.0.0.1:0").expect("binding a local UDP socket should succeed");
        socket
            .set_nonblocking(true)
            .expect("setting non-blocking mode should succeed");
        drop(
            UdpSocket::from_std(socket)
                .expect("registering without an I/O driver should have panicked"),
        );
    }

    /// A leaf that needs the I/O driver: polling it inside the store's
    /// time-only context fails the test (RFC 0008 §4.3), so it stands in
    /// for the would-fail-if-polled class.
    #[cfg(not(loom))]
    fn io_command() -> EffectCommand<Msg> {
        Command::perform(async { register_with_io_driver() }, |()| {
            unreachable!("the I/O-dependent leaf must fail the test before completing")
        })
    }

    /// A `Command::timeout` leaf over a never-ready future: deliverable
    /// only once the store's virtual clock reaches its deadline.
    fn timeout_command(secs: u64) -> Command<Msg> {
        Command::future(pending())
            .timeout(Duration::from_secs(secs), || Msg::N(99))
            .into()
    }

    fn failure_message<T>(result: Result<T, Box<dyn Any + Send>>) -> String {
        let payload = result.err().expect("the call should have failed");
        match payload.downcast::<String>() {
            Ok(message) => *message,
            Err(payload) => (*payload
                .downcast::<&str>()
                .expect("panic payload should be a string"))
            .to_owned(),
        }
    }

    // INV-T2: the store's bounds are `Debug` on the store, `PartialEq` only
    // on equality-asserting methods, `Clone` nowhere. `Opaque` implements
    // `Debug` and deliberately neither `PartialEq` nor `Clone`, and this
    // drive of `new` -> `send` -> `state` -> `receive_matching` -> `finish`
    // must keep compiling.
    #[derive(Debug)]
    enum Opaque {
        Ping,
        Pong,
    }

    #[test]
    fn store_bounds_are_debug_only() {
        let mut store = store_with(Command::none(), |msg| match msg {
            Opaque::Ping => Command::message(Opaque::Pong).into(),
            Opaque::Pong => Command::none(),
        });
        store.send(Opaque::Ping);
        assert_eq!(store.state().log, vec!["Ping".to_owned()]);
        store.receive_matching(|msg| matches!(msg, Opaque::Pong));
        store.finish();
    }

    // INV-T4: two executions of one deterministic, multi-leaf,
    // cancellation-exercising test program observe identical state
    // transitions and delivery sequences.
    fn deterministic_program_transcript() -> Vec<String> {
        let id = CommandId::new("det");
        let mut store = store_with(
            Command::batch([
                Command::stream(stream::iter([Msg::N(1), Msg::N(2)])),
                Command::message(Msg::N(3)),
            ]),
            move |msg| match msg {
                Msg::Start => Command::stream(stream::iter([Msg::Keyed(1), Msg::Keyed(2)]))
                    .cancellable(id.clone())
                    .into(),
                Msg::Cancel => Command::cancel(id.clone()),
                _ => Command::none(),
            },
        );
        store.receive(Msg::N(1));
        store.receive(Msg::N(2));
        store.receive(Msg::N(3));
        store.send(Msg::Start);
        store.receive(Msg::Keyed(1));
        store.send(Msg::Cancel);
        let transcript = store.state().log.clone();
        store.finish();
        transcript
    }

    #[test]
    fn deterministic_program_repeats_its_transcript() {
        assert_eq!(
            deterministic_program_transcript(),
            deterministic_program_transcript()
        );
    }

    // INV-T4: the poll budget. Polling happens only inside `receive*`,
    // `finish`, and drop checks (plus keyed-intake reconciliation), and each
    // check polls each reached leaf exactly once — a double-polling
    // implementation fails the counts below.
    #[test]
    fn checks_poll_each_reached_leaf_exactly_once() {
        let polls = Arc::new(AtomicUsize::new(0));
        let done = Arc::new(AtomicBool::new(false));
        let counting = {
            let polls = Arc::clone(&polls);
            let done = Arc::clone(&done);
            stream::poll_fn(move |_| {
                polls.fetch_add(1, Ordering::SeqCst);
                if done.load(Ordering::SeqCst) {
                    Poll::Ready(None)
                } else {
                    Poll::Pending::<Option<Msg>>
                }
            })
        };
        let mut store = store_with(
            Command::batch([Command::stream(counting), Command::message(Msg::N(1))]),
            |_| Command::none(),
        );
        // Construction enqueues but polls nothing.
        assert_eq!(polls.load(Ordering::SeqCst), 0);

        // The receive scan reaches the pending leaf on its way to the ready
        // one and gives it exactly one poll.
        store.receive(Msg::N(1));
        assert_eq!(polls.load(Ordering::SeqCst), 1);

        // A bare send polls no leaf.
        store.send(Msg::Unrelated);
        assert_eq!(polls.load(Ordering::SeqCst), 1);

        // advance's anchoring scan gives each pending leaf exactly one
        // poll, and nothing is polled after the clock moves (INV-T13's
        // budget half, asserted here with INV-T4's counting).
        store.advance(Duration::from_millis(1));
        assert_eq!(polls.load(Ordering::SeqCst), 2);

        // The finish scan gives the leaf its one poll, on which it completes.
        done.store(true, Ordering::SeqCst);
        store.finish();
        assert_eq!(polls.load(Ordering::SeqCst), 3);
    }

    // INV-T5: one leaf's messages are delivered in stream order.
    #[test]
    fn one_leaf_delivers_in_stream_order() {
        let mut store = store_with(
            Command::stream(stream::iter([Msg::N(1), Msg::N(2), Msg::N(3)])).into(),
            |_| Command::none(),
        );
        store.receive(Msg::N(1));
        store.receive(Msg::N(2));
        store.receive(Msg::N(3));
        store.finish();
    }

    // INV-T6: a batch of ready leaves delivers in flattened declaration
    // order.
    #[test]
    fn ready_leaves_deliver_in_declaration_order() {
        let mut store = store_with(
            Command::batch([
                Command::message(Msg::N(1)),
                Command::message(Msg::N(2)),
                Command::stream(stream::iter([Msg::N(3), Msg::N(4)])),
            ]),
            |_| Command::none(),
        );
        store.receive(Msg::N(1));
        store.receive(Msg::N(2));
        store.receive(Msg::N(3));
        store.receive(Msg::N(4));
        store.finish();
    }

    // INV-T6: a leaf made ready late delivers after an earlier-enqueued ready
    // leaf but before a later one.
    #[test]
    fn late_ready_leaf_delivers_at_its_enqueue_position() {
        let (tx, rx) = oneshot::channel::<u32>();
        let mut store = store_with(
            Command::batch([
                Command::future(async move { Msg::N(rx.await.expect("sender completes")) }),
                Command::message(Msg::N(2)),
                Command::message(Msg::N(3)),
            ]),
            |_| Command::none(),
        );
        // Leaf 0 is pending, so the earliest deliverable leaf is leaf 1.
        store.receive(Msg::N(2));
        tx.send(1).expect("receiver is alive");
        // Now leaf 0 is deliverable and precedes leaf 2.
        store.receive(Msg::N(1));
        store.receive(Msg::N(3));
        store.finish();
    }

    // INV-T7: a same-id `CancelInFlight` command supersedes the occupant's
    // undelivered output (RFC 0003 INV-3, INV-6), via the `send`-scripted
    // cancel-before-receive form the non-blocking `send` enables.
    #[test]
    fn cancel_in_flight_supersedes_pending_keyed_output() {
        let id = CommandId::new("k");
        let mut store = store_with(Command::none(), move |msg| match msg {
            Msg::Start => Command::stream(stream::iter([Msg::Keyed(1), Msg::Keyed(2)]))
                .cancellable(id.clone())
                .into(),
            Msg::Restart => Command::message(Msg::Keyed(9))
                .cancellable(id.clone())
                .into(),
            _ => Command::none(),
        });
        store.send(Msg::Start);
        store.send(Msg::Restart);
        store.receive(Msg::Keyed(9));
        // Keyed(1) and Keyed(2) are unobservable and not leaks.
        store.finish();
    }

    // INV-T7: `CancelInFlight` supersedes an occupant whose poll would fail
    // the test (an I/O-dependent leaf, §4.3) without polling it — the §5.1
    // escape hatch that makes cancelling such a keyed leaf expressible.
    #[cfg(not(loom))]
    #[test]
    fn cancel_in_flight_supersedes_an_io_dependent_occupant() {
        let id = CommandId::new("k");
        let mut store = store_with(Command::none(), move |msg| match msg {
            Msg::Start => io_command().cancellable(id.clone()).into(),
            Msg::Restart => Command::message(Msg::Keyed(9))
                .cancellable(id.clone())
                .into(),
            _ => Command::none(),
        });
        store.send(Msg::Start);
        store.send(Msg::Restart);
        store.receive(Msg::Keyed(9));
        store.finish();
    }

    // INV-T7: while the id is occupied, a `KeepInFlight` command's stream is
    // discarded (RFC 0003 INV-5), and an item yielded by the reconciliation
    // poll stays deliverable at its canonical position (INV-6).
    #[test]
    fn keep_in_flight_is_discarded_while_occupied() {
        let id = CommandId::new("k");
        let mut store = store_with(Command::none(), move |msg| match msg {
            Msg::Start => Command::stream(stream::iter([Msg::Keyed(1), Msg::Keyed(2)]))
                .cancellable(id.clone())
                .into(),
            Msg::TryKeep => Command::message(Msg::Keyed(9))
                .cancellable_with(id.clone(), CancelPolicy::KeepInFlight)
                .into(),
            _ => Command::none(),
        });
        store.send(Msg::Start);
        store.receive(Msg::Keyed(1));
        // The reconciliation poll yields Keyed(2), which occupies the id: the
        // new command is discarded and Keyed(2) stays deliverable.
        store.send(Msg::TryKeep);
        store.receive(Msg::Keyed(2));
        // Keyed(9) was discarded, so nothing else remains.
        store.finish();
    }

    // INV-T7: a `KeepInFlight` command arriving after the occupant's leaves
    // are exhausted is admitted (RFC 0003 INV-7).
    #[test]
    fn keep_in_flight_is_admitted_after_occupant_exhaustion() {
        let id = CommandId::new("k");
        let mut store = store_with(Command::none(), move |msg| match msg {
            Msg::Start => Command::message(Msg::Keyed(1))
                .cancellable(id.clone())
                .into(),
            Msg::TryKeep => Command::message(Msg::Keyed(2))
                .cancellable_with(id.clone(), CancelPolicy::KeepInFlight)
                .into(),
            _ => Command::none(),
        });
        store.send(Msg::Start);
        store.receive(Msg::Keyed(1));
        // The reconciliation poll observes the occupant exhausted, releasing
        // the id and admitting the new command.
        store.send(Msg::TryKeep);
        store.receive(Msg::Keyed(2));
        store.finish();
    }

    // INV-T7: `Command::cancel(id)` drops the occupant's stream and
    // undelivered output, and is idempotent (RFC 0003 INV-4), via the
    // `send`-scripted form.
    #[test]
    fn explicit_cancel_is_strict_and_idempotent() {
        let id = CommandId::new("k");
        let mut store = store_with(Command::none(), move |msg| match msg {
            Msg::Start => Command::stream(stream::iter([Msg::Keyed(1)]))
                .cancellable(id.clone())
                .into(),
            Msg::Cancel => Command::cancel(id.clone()),
            _ => Command::none(),
        });
        store.send(Msg::Start);
        store.send(Msg::Cancel);
        store.send(Msg::Cancel);
        // Keyed(1) is unobservable and not a leak.
        store.finish();
    }

    // INV-T7: one command carrying both an explicit cancel and its own keyed
    // spawn applies the cancel first, then the admission decision (RFC 0003
    // §5.1): the occupant's output is unobservable and the new work
    // immediately reclaims the id. An implementation that admitted the spawn
    // first would cancel the new work itself and fail this test.
    #[test]
    fn same_command_cancel_then_spawn_reclaims_the_id() {
        let id = CommandId::new("k");
        let mut store = store_with(Command::none(), move |msg| match msg {
            Msg::Start => Command::stream(stream::iter([Msg::Keyed(1), Msg::Keyed(2)]))
                .cancellable(id.clone())
                .into(),
            Msg::Restart => Command::batch([
                Command::cancel(id.clone()),
                Command::message(Msg::Keyed(9))
                    .cancellable(id.clone())
                    .into(),
            ]),
            _ => Command::none(),
        });
        store.send(Msg::Start);
        store.send(Msg::Restart);
        // The old occupant's Keyed(1)/Keyed(2) are gone; only the new work's
        // output is deliverable at the id.
        store.receive(Msg::Keyed(9));
        store.finish();
    }

    // INV-T7: unkeyed commands are unaffected by keyed lifecycle operations
    // (RFC 0003 INV-1).
    #[test]
    fn unkeyed_output_is_unaffected_by_cancellation() {
        let id = CommandId::new("k");
        let mut store = store_with(Command::message(Msg::N(1)).into(), move |msg| match msg {
            Msg::Start => Command::stream(stream::iter([Msg::Keyed(1)]))
                .cancellable(id.clone())
                .into(),
            Msg::Cancel => Command::cancel(id.clone()),
            _ => Command::none(),
        });
        store.send(Msg::Start);
        store.send(Msg::Cancel);
        store.receive(Msg::N(1));
        store.finish();
    }

    // INV-T8: a ready message never received fails `finish`, naming the
    // leaked value via `Debug`.
    #[test]
    #[should_panic(expected = "deliverable output was never received: N(7)")]
    fn finish_fails_on_an_unreceived_ready_message() {
        let store = store_with(Command::message(Msg::N(7)).into(), |_| Command::none());
        store.finish();
    }

    // INV-T8: an effect leaf never driven to completion fails `finish`,
    // reported by count and enqueue position (there is no value to print).
    #[test]
    #[should_panic(
        expected = "1 effect leaf(s) not driven to completion; first still pending at enqueue position 0"
    )]
    fn finish_fails_on_an_unfinished_leaf() {
        let store = store_with(Command::stream(stream::pending::<Msg>()).into(), |_| {
            Command::none()
        });
        store.finish();
    }

    // INV-T8: dropping an unfinished store runs the same check as `finish`.
    #[test]
    fn drop_without_finish_fails_on_leaked_output() {
        let failure = catch_unwind(AssertUnwindSafe(|| {
            let store = store_with(Command::message(Msg::N(7)).into(), |_| Command::none());
            drop(store);
        }));
        let message = failure_message(failure);
        assert!(
            message.contains("drop check")
                && message.contains("deliverable output was never received: N(7)"),
            "the drop check should name the leaked value: {message}"
        );
    }

    // INV-T8: a non-cancelling `send` does not fail on a keyed occupant's
    // pending output and leaves it assertable — the shared-first runtime
    // parity case (RFC 0003 INV-14).
    #[test]
    fn send_does_not_block_on_pending_keyed_output() {
        let id = CommandId::new("k");
        let mut store = store_with(Command::none(), move |msg| match msg {
            Msg::Start => Command::stream(stream::iter([Msg::Keyed(1)]))
                .cancellable(id.clone())
                .into(),
            _ => Command::none(),
        });
        store.send(Msg::Start);
        store.send(Msg::Unrelated);
        store.receive(Msg::Keyed(1));
        store.finish();
    }

    // INV-T8: a `send` does not block on pending unkeyed output either
    // (the ordering there is the store's own linearization, RFC 0008 §6).
    #[test]
    fn send_does_not_block_on_pending_unkeyed_output() {
        let mut store = store_with(Command::message(Msg::N(1)).into(), |_| Command::none());
        store.send(Msg::Unrelated);
        store.receive(Msg::N(1));
        store.finish();
    }

    // INV-T8: the same two retention guarantees crossed with the other
    // origin — a *keyed init* effect survives a first, non-cancelling `send`
    // (an implementation special-casing the first send to demand init output
    // be received first fails here) ...
    #[test]
    fn send_does_not_block_on_keyed_init_output() {
        let id = CommandId::new("k");
        let mut store = store_with(
            Command::stream(stream::iter([Msg::Keyed(1)]))
                .cancellable(id)
                .into(),
            |_| Command::none(),
        );
        store.send(Msg::Unrelated);
        store.receive(Msg::Keyed(1));
        store.finish();
    }

    // ... and an *unkeyed step* effect survives a later, unrelated `send`.
    #[test]
    fn send_does_not_block_on_unkeyed_step_output() {
        let mut store = store_with(Command::none(), |msg| match msg {
            Msg::Start => Command::message(Msg::N(1)).into(),
            _ => Command::none(),
        });
        store.send(Msg::Start);
        store.send(Msg::Unrelated);
        store.receive(Msg::N(1));
        store.finish();
    }

    // INV-T10: constructing a store inside an entered Tokio runtime panics
    // immediately with a diagnostic naming the precondition, not the generic
    // missing-reactor panic of §4.3.
    #[tokio::test]
    #[should_panic(expected = "a Tokio runtime is already entered")]
    async fn new_panics_inside_an_entered_runtime() {
        let _ = store_with(Command::<Msg>::none(), |_| Command::none());
    }

    // INV-T9: after `receive_quit`, steps fail on the quit state without
    // polling any leaf — an I/O-dependent leaf that would fail the test if
    // polled stays untouched — and the finish check polls nothing and
    // passes.
    #[cfg(not(loom))]
    #[test]
    fn quit_is_terminal_and_discards_remaining_output() {
        let mut store = store_with(Command::none(), |msg| match msg {
            Msg::StartQuit => Command::batch([Command::quit(), io_command().into()]),
            _ => Command::none(),
        });
        store.send(Msg::StartQuit);
        store.receive_quit();

        for failure in [
            catch_unwind(AssertUnwindSafe(|| store.send(Msg::Unrelated))),
            catch_unwind(AssertUnwindSafe(|| store.advance(Duration::from_secs(1)))),
            catch_unwind(AssertUnwindSafe(|| store.receive(Msg::N(1)))),
            catch_unwind(AssertUnwindSafe(|| store.receive_matching(|_| true))),
            catch_unwind(AssertUnwindSafe(|| store.receive_quit())),
        ] {
            assert!(
                failure_message(failure).contains("the application has quit"),
                "post-quit calls fail on the quit state, not by polling a leaf"
            );
        }

        // Observations stay callable, and finish passes despite the
        // reactor-dependent leaf legally discarded at quit.
        let _ = store.state();
        let _ = store.subscription_ids();
        assert!(store.redraw_requested(), "the quit step requested a redraw");
        store.finish();
    }

    // RFC 0008 §5.3, the applied-but-unobserved state. Both rows below were
    // silent before the quit state was one value instead of two booleans: the
    // store recorded the applied quit but nothing consulted it until
    // `receive_quit` did, so a test that never asked, or that kept driving,
    // passed.

    // A quit the dispatch applied and the test never observed is a leak, and
    // for the same reason undelivered output is: the run ended somewhere the
    // test did not say it ended. Without this the script reads as though it
    // ran to completion.
    #[cfg(not(loom))]
    #[test]
    #[should_panic(expected = "a quit was applied at its dispatch and never observed")]
    fn an_unobserved_applied_quit_fails_the_exhaustiveness_check() {
        let mut store = store_with(Command::none(), |msg| match msg {
            Msg::StartQuit => Command::quit(),
            _ => Command::none(),
        });
        store.send(Msg::StartQuit);
        store.finish();
    }

    // And nothing runs after it. "No later input can intervene between the
    // update that returned it and termination" is the property the
    // synchronous route buys (RFC 0014 §3.3); a store that accepted a further
    // `send` here would be scripting an execution the runtime cannot produce.
    #[cfg(not(loom))]
    #[test]
    #[should_panic(expected = "no later input can intervene before termination")]
    fn a_send_after_an_applied_quit_fails() {
        let mut store = store_with(Command::none(), |msg| match msg {
            Msg::StartQuit => Command::quit(),
            _ => Command::none(),
        });
        store.send(Msg::StartQuit);
        store.send(Msg::Unrelated);
    }

    // RFC 0008 §5.2: `redraw_requested` reports the init command's folded
    // directive before the first step — `Command` introspection, not a
    // first-render prediction.
    #[test]
    fn redraw_reports_the_init_directive_before_the_first_step() {
        let defaulted = store_with(Command::message(Msg::N(1)).into(), |_| Command::none());
        assert!(
            defaulted.redraw_requested(),
            "constructors default to redraw"
        );
        drop(catch_unwind(AssertUnwindSafe(move || defaulted.finish())));

        let opted_out = store_with(Command::<Msg>::none().without_redraw(), |_| Command::none());
        assert!(
            !opted_out.redraw_requested(),
            "without_redraw is observable"
        );
        opted_out.finish();
    }

    // RFC 0008 §5.2: `redraw_requested` follows each step's command, a
    // `receive` is a step, and `receive_quit` is not.
    #[test]
    fn redraw_tracks_steps_and_receive_quit_is_not_a_step() {
        let mut store = store_with(Command::message(Msg::N(1)).into(), |msg| match msg {
            Msg::N(_) => Command::none().without_redraw(),
            Msg::StartQuit => Command::quit().without_redraw(),
            _ => Command::none(),
        });
        assert!(
            store.redraw_requested(),
            "init directive defaults to redraw"
        );

        // The update inside a receive is a step.
        store.receive(Msg::N(1));
        assert!(!store.redraw_requested(), "the receive step opted out");

        store.send(Msg::Loud);
        assert!(store.redraw_requested(), "the send step defaults to redraw");

        store.send(Msg::StartQuit);
        assert!(!store.redraw_requested(), "the quit step opted out");

        // receive_quit applies no message: the previous step's directive
        // stays reported.
        store.receive_quit();
        assert!(!store.redraw_requested(), "receive_quit is not a step");
        store.finish();
    }

    // RFC 0008 §3.2: `subscription_ids` observes the declared set in
    // declaration order, as a pure function of state, starting no source.
    struct SubApp {
        first: MockSource<()>,
        second: MockSource<()>,
        both: bool,
    }

    impl Application for SubApp {
        type Message = ();
        type Flags = (MockSource<()>, MockSource<()>);

        fn new((first, second): Self::Flags) -> (Self, Command<()>) {
            (
                Self {
                    first,
                    second,
                    both: true,
                },
                Command::none(),
            )
        }

        fn update(&mut self, (): ()) -> Command<()> {
            self.both = false;
            Command::none()
        }

        fn view(&self, _frame: &mut Frame<'_>) {}

        fn subscriptions(&self) -> Vec<Subscription<()>> {
            if self.both {
                vec![
                    Subscription::new(self.first.clone()),
                    Subscription::new(self.second.clone()),
                ]
            } else {
                vec![Subscription::new(self.first.clone())]
            }
        }
    }

    #[test]
    fn subscription_ids_observe_the_declared_set_in_declaration_order() {
        let first = MockSource::<()>::new();
        let second = MockSource::<()>::new();
        let first_id = Subscription::new(first.clone()).id().clone();
        let second_id = Subscription::new(second.clone()).id().clone();

        let mut store = TestStore::<SubApp>::new((first, second));
        assert_eq!(
            store.subscription_ids(),
            vec![first_id.clone(), second_id],
            "declared ids in declaration order"
        );

        store.send(());
        assert_eq!(
            store.subscription_ids(),
            vec![first_id],
            "the declared set follows the state after a send"
        );
        store.finish();
    }

    // INV-T11: `subscription_ids` observation over a duplicate declaration
    // ([A, B, A]).
    struct DupApp {
        first: MockSource<()>,
        second: MockSource<()>,
    }

    impl Application for DupApp {
        type Message = ();
        type Flags = (MockSource<()>, MockSource<()>);

        fn new((first, second): Self::Flags) -> (Self, Command<()>) {
            (Self { first, second }, Command::none())
        }

        fn update(&mut self, (): ()) -> Command<()> {
            Command::none()
        }

        fn view(&self, _frame: &mut Frame<'_>) {}

        fn subscriptions(&self) -> Vec<Subscription<()>> {
            vec![
                Subscription::new(self.first.clone()),
                Subscription::new(self.second.clone()),
                Subscription::new(self.first.clone()),
            ]
        }
    }

    // INV-T11: duplicates collapse to their first occurrence, at its
    // original position — [A, B, A] becomes [A, B], never [B, A].
    #[test]
    fn subscription_ids_dedup_is_first_occurrence_stable() {
        let first = MockSource::<()>::new();
        let second = MockSource::<()>::new();
        let first_id = Subscription::new(first.clone()).id().clone();
        let second_id = Subscription::new(second.clone()).id().clone();

        let store = TestStore::<DupApp>::new((first, second));
        assert_eq!(
            store.subscription_ids(),
            vec![first_id, second_id],
            "duplicates collapse to the first occurrence in declaration order"
        );
        store.finish();
    }

    // INV-T11: `subscription_ids` never calls `stream()` on any declared
    // source (a MockSource's receiver count observes stream construction).
    #[test]
    fn subscription_ids_starts_no_source() {
        let first = MockSource::<()>::new();
        let second = MockSource::<()>::new();
        let store = TestStore::<DupApp>::new((first.clone(), second.clone()));

        let _ = store.subscription_ids();

        assert_eq!(first.receiver_count(), 0, "no declared source is started");
        assert_eq!(second.receiver_count(), 0, "no declared source is started");
        store.finish();
    }

    // INV-T11: no duplicate-ignored warning fires from `subscription_ids` —
    // that tracing event belongs to the runtime's reconciliation, which the
    // store never runs.
    #[test]
    fn subscription_ids_emits_no_duplicate_ignored_warning() {
        let recorder = TraceRecorder::new()
            .with_target("tears::subscription")
            .with_level(Level::WARN);
        let _guard = recorder.set_default();

        let store = TestStore::<DupApp>::new((MockSource::new(), MockSource::new()));
        let _ = store.subscription_ids();

        assert_eq!(
            recorder.event_count(),
            0,
            "the duplicate-ignored warning is reconciliation's side effect, not the store's"
        );
        store.finish();
    }

    // Diagnostics: receive fails on a value mismatch naming both values.
    #[test]
    #[should_panic(expected = "message mismatch")]
    fn receive_fails_on_a_mismatch() {
        let mut store = store_with(Command::message(Msg::N(1)).into(), |_| Command::none());
        store.receive(Msg::N(2));
    }

    // Diagnostics: quit-versus-message confusion in both directions. The
    // quit here is producer-originated — the route that still travels as
    // deliverable output; an `update`-returned quit applies at its dispatch
    // and never reaches a delivery scan (RFC 0014 §3.3).
    #[test]
    #[should_panic(expected = "assert it with TestStore::receive_quit")]
    fn receive_fails_when_the_next_output_is_a_quit_request() {
        let mut store = store_with(
            Command::<Msg>::actions(stream::once(async { Action::Quit })).into(),
            |_| Command::none(),
        );
        store.receive(Msg::N(1));
    }

    #[test]
    #[should_panic(expected = "the next deliverable output is a message: N(1)")]
    fn receive_quit_fails_when_the_next_output_is_a_message() {
        let mut store = store_with(Command::message(Msg::N(1)).into(), |_| Command::none());
        store.receive_quit();
    }

    // Diagnostics: the nothing-deliverable failure distinguishes "no pending
    // effects" from "effects pending but not ready".
    #[test]
    #[should_panic(expected = "no deliverable output: no pending effects")]
    fn receive_fails_with_no_pending_effects() {
        let mut store = store_with(Command::<Msg>::none(), |_| Command::none());
        store.receive(Msg::N(1));
    }

    #[test]
    #[should_panic(expected = "no deliverable output: effects are pending but none is ready")]
    fn receive_fails_with_effects_pending_but_not_ready() {
        let mut store = store_with(Command::stream(stream::pending::<Msg>()).into(), |_| {
            Command::none()
        });
        store.receive(Msg::N(1));
    }

    // Diagnostics: receive_matching names the rejected value.
    #[test]
    #[should_panic(expected = "predicate rejected the delivered message: N(1)")]
    fn receive_matching_fails_when_the_predicate_rejects() {
        let mut store = store_with(Command::message(Msg::N(1)).into(), |_| Command::none());
        store.receive_matching(|msg| matches!(msg, Msg::N(2)));
    }

    // RFC 0008 §4.3: polling a leaf that needs the I/O driver fails the
    // test with the underlying missing-reactor panic; the scan reaches it
    // even when the receive targets a different message.
    #[cfg(not(loom))]
    #[test]
    #[should_panic(expected = "IO is disabled")]
    fn polling_an_io_dependent_leaf_fails_the_test() {
        let mut store = store_with(
            Command::batch([io_command(), Command::message(Msg::N(1))]),
            |_| Command::none(),
        );
        store.receive(Msg::N(1));
    }

    // INV-T12: a `Command::timeout` leaf enqueued ahead of a
    // test-controlled leaf stays pending across repeated non-failing
    // receive scans — each scan gives it one poll on its way to the
    // deliverable leaf, and delivering the control message proves the
    // timeout leaf was not deliverable — and across advances summing to
    // less than its duration; it becomes deliverable exactly when the
    // cumulative advances reach its deadline (INV-T13's exact-deadline
    // half), with no wall-clock waiting.
    #[test]
    fn timeout_leaf_is_pending_until_advance_reaches_its_deadline() {
        let mut store = store_with(Command::none(), |msg| match msg {
            Msg::Start => Command::batch([
                timeout_command(60),
                Command::stream(stream::iter([Msg::N(1), Msg::N(2), Msg::N(3)])).into(),
            ]),
            _ => Command::none(),
        });
        store.send(Msg::Start);

        // No advance yet: the timeout leaf is polled and pending.
        store.receive(Msg::N(1));

        store.advance(Duration::from_secs(30));
        store.receive(Msg::N(2));

        // 59 of 60 seconds: still pending.
        store.advance(Duration::from_secs(29));
        store.receive(Msg::N(3));

        store.advance(Duration::from_secs(1));
        store.receive(Msg::N(99));
        store.finish();
    }

    // INV-T13: the deadline anchors at the leaf's first poll (its
    // enqueue-time virtual now), not at store construction — time advanced
    // before the leaf exists does not count against its deadline.
    #[test]
    fn timeout_deadline_anchors_at_first_poll_not_construction() {
        let mut store = store_with(Command::none(), |msg| match msg {
            Msg::Start => Command::batch([
                timeout_command(60),
                Command::stream(stream::iter([Msg::N(1)])).into(),
            ]),
            _ => Command::none(),
        });
        store.advance(Duration::from_secs(10));
        store.send(Msg::Start);

        // 59s after the anchor (69s absolute): a construction-anchored
        // implementation (deadline at t=60s) would deliver N(99) here
        // instead of the control message.
        store.advance(Duration::from_secs(59));
        store.receive(Msg::N(1));

        store.advance(Duration::from_secs(1));
        store.receive(Msg::N(99));
        store.finish();
    }

    // INV-T12: a retry with a non-zero backoff delivers its retried outcome
    // only after an advance spanning the backoff (RFC 0004's semantics,
    // carried onto the virtual clock by RFC 0009 INV-C3, exercised through
    // the store).
    #[test]
    fn retry_backoff_delivers_after_an_advance_spanning_the_backoff() {
        let mut store = store_with(Command::none(), |msg| match msg {
            Msg::StartRetry => Command::batch([
                Command::retry(
                    RetryPolicy::new(NonZeroUsize::new(2).expect("non-zero"))
                        .with_fixed_backoff(Duration::from_secs(5)),
                    |ctx| async move {
                        if ctx.attempt().get() == 1 {
                            Err("first attempt fails")
                        } else {
                            Ok(42)
                        }
                    },
                    |result| Msg::N(result.expect("the second attempt succeeds")),
                ),
                Command::stream(stream::iter([Msg::N(1)])),
            ]),
            _ => Command::none(),
        });
        store.send(Msg::StartRetry);

        // The anchoring scan runs the failing first attempt and starts the
        // backoff sleep; one second short of the backoff, the receive scan
        // polls the retry leaf (still sleeping) on its way to the control
        // message.
        store.advance(Duration::from_secs(4));
        store.receive(Msg::N(1));

        store.advance(Duration::from_secs(1));
        store.receive(Msg::N(42));
        store.finish();
    }

    // RFC 0008 §4.2: leaves made ready by the same advance deliver in
    // enqueue order — the store's linearization of the equal-deadline order
    // RFC 0009 §3.4 leaves unspecified.
    #[test]
    fn equal_deadline_timeout_leaves_deliver_in_enqueue_order() {
        let mut store = store_with(
            Command::batch([
                Command::future(pending()).timeout(Duration::from_secs(5), || Msg::N(1)),
                Command::future(pending()).timeout(Duration::from_secs(5), || Msg::N(2)),
            ]),
            |_| Command::none(),
        );
        store.advance(Duration::from_secs(5));
        store.receive(Msg::N(1));
        store.receive(Msg::N(2));
        store.finish();
    }

    // RFC 0008 §3.2: advance's anchoring scan buffers a ready yield at its
    // canonical position instead of delivering or dropping it.
    #[test]
    fn advance_buffers_ready_output_without_delivering() {
        let mut store = store_with(Command::message(Msg::N(1)).into(), |_| Command::none());
        store.advance(Duration::ZERO);
        store.receive(Msg::N(1));
        store.finish();
    }

    // RFC 0008 §6: a time-gated leaf the test never advanced to its
    // deadline is an ordinary unfinished-leaf leak at finish.
    #[test]
    #[should_panic(expected = "effect leaf(s) not driven to completion")]
    fn finish_fails_on_an_unadvanced_timeout_leaf() {
        let store = store_with(timeout_command(60), |_| Command::none());
        store.finish();
    }
}
