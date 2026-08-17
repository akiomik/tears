//! The reducer-first kernel.
//!
//! One driving task owns the application state and a run registry — one
//! entry per producer run, of every kind — with one join set for exit
//! observation, one origin-tagged data lane, and one control lane for
//! producer-originated quits (RFC 0014 §3.1, §3.3, §10).
//!
//! The kernel sits at the crate root rather than under `runtime` because it
//! is not a part of the old `Runtime`: it is the successor core, and
//! `runtime` keeps the entry facade, the construction-time configuration,
//! and the load observability.
//!
//! Two things this module deliberately does not have:
//!
//! - **No render abstraction.** The kernel borrows a
//!   `ratatui::Terminal<B>` directly for the pass stage that renders. A
//!   host trait would give a driver a way around `Program::view`, which is
//!   exactly the third driving difference INV-RC13 excludes; a render
//!   failure is driven by a failing backend instead, so the render owner
//!   stays one type.
//! - **No private kernel configuration type.** Construction reads the two
//!   surviving controls off [`RuntimeConfig`] and nothing else, which is
//!   what keeps the removed controls unreadable here rather than merely
//!   unread.
//!
//! **No wall clock.** The driving loop reads no clock, arms no timer, and
//! never sleeps: passes are begun by arrivals and bounded by counts, and
//! render cadence is pass-bounded rather than paced (RFC 0014 §6.3). The one
//! duration this crate still measures is the bounded-lane capacity wait on
//! the *producer* send path, which is observability under RFC 0006 §4.4 and
//! reaches no scheduling decision here.
//!
//! Visibility note: this module and everything under it are crate-capped by
//! `lib.rs`, so items are declared `pub` rather than `pub(crate)` (the
//! convention `runtime::channel` documents).

// The kernel is complete but not yet wired to an entry point: the facade
// that constructs and runs it lands with the switch, and the stage-3 driver
// that scripts it lands beside it. Until then most of this surface has no
// caller outside the tests below.
#![allow(
    dead_code,
    reason = "kernel: the entry facade that constructs and runs it lands with the switch"
)]

pub mod accounting;
#[cfg(all(feature = "loom-core", test))]
pub mod accounting_core;
pub mod arbiter;
#[cfg(test)]
pub mod conformance;
pub mod lane;
pub mod lowering;
pub mod pass;
pub mod producer;
pub mod registry;
pub mod teardown;

use std::collections::{HashMap, VecDeque};
use std::mem;
use std::num::NonZeroUsize;
use std::sync::Arc;

use ratatui::Terminal;
use ratatui::backend::Backend;
use tokio::task::{Id as TaskId, JoinError, JoinSet};

use crate::command::{Command, CommandId, SpawnEntry};
use crate::reducer::Program;
use crate::runtime::channel::channel_observed;
use crate::runtime::config::RuntimeConfig;
use crate::runtime::load::{Channel, LoadObserver};
use crate::structural_key::ScopePath;
use crate::testing::driver::{AcceptanceRecorder, IntentRecorder};

use lane::{
    ControlReceiver, ControlSender, DataReceiver, DataSender, Envelope, GateMode, RunToken,
    SendGate, control_lane,
};
use lowering::{DispatchStep, SpawnDecision};
use pass::DEFAULT_BATCH_MAX_MESSAGES;
use registry::{ExitOutcome, RunKind, ScopeRegistry};

/// Why the kernel terminated.
///
/// The render error's *value* is not carried here: it flows out through the
/// `Result` of the pass that produced it, so there is one owner for it and
/// the kernel stays independent of the backend type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExitReason {
    /// A controlled quit, of either physical route.
    Quit,
    /// A render failure.
    RenderError,
}

/// The kernel's phase — the only phase machine in the topology.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum KernelPhase {
    /// Constructed, not yet booted. Construction is inert (RFC 0011
    /// INV-LC3).
    Boot,
    /// Running steady-state passes.
    Steady,
    /// Termination applied, immediate postcondition done, settle pending.
    Terminating(ExitReason),
    /// Settled: the join set is drained.
    Settled,
}

/// What bootstrap started, in spawn order.
#[derive(Debug)]
pub struct BootReport {
    /// The producer runs the init dispatch, the initial reconcile, and the
    /// continuation pass started.
    pub producers: Vec<RunToken>,
}

/// The quiescent-postcondition evidence a settle produces (RFC 0011
/// INV-LC6, INV-LC7).
///
/// The producer gauges are deliberately *not* a field here. They are a
/// `tracing` observation under `tears::runtime::load` (RFC 0006 §4.4,
/// INV-L13) and the kernel holds no reader for them, so a field claiming
/// "the gauges read zero" could only ever restate what this type already
/// implies — the join set drained and the run bookkeeping cleared. INV-LC7's
/// gauge half is asserted where the gauges are actually observable, at that
/// tracing surface.
#[derive(Debug)]
pub struct ExitReport {
    /// Why the kernel terminated.
    pub reason: ExitReason,
    /// Tasks joined during settle, the exits already reflected included.
    pub joined: usize,
}

/// The kernel: one driving task, two lanes, one join set, one authoritative
/// registry.
pub struct Kernel<P: Program> {
    program: P,
    flags: Option<P::Flags>,
    state: Option<P::State>,
    registry: ScopeRegistry,
    join_set: JoinSet<()>,
    task_index: HashMap<TaskId, RunToken>,
    // The receivers are dropped at the immediate postcondition, which is
    // what makes an in-flight send fail and end its producer; the senders
    // are held for the kernel's whole lifetime so a live kernel's `recv`
    // can never observe a closed lane (see `lane`'s ownership note).
    data_rx: Option<DataReceiver<P::Message>>,
    data_tx: DataSender<P::Message>,
    data_buf: VecDeque<Envelope<P::Message>>,
    control_rx: Option<ControlReceiver<P::Message>>,
    control_tx: ControlSender<P::Message>,
    control_buf: VecDeque<Envelope<P::Message>>,
    exit_buf: VecDeque<(RunToken, ExitOutcome)>,
    observer: LoadObserver,
    redraw_pending: bool,
    dirty: bool,
    phase: KernelPhase,
    batch_cap: NonZeroUsize,
    /// The one send gate, kernel-wide. Every run's ingress holds a clone of
    /// this same object, which is what makes the driver's "at most one
    /// outstanding grant" rule driver-wide rather than per origin
    /// (RFC 0008 §9.6).
    gate: Arc<SendGate>,
    next_token: RunToken,
    /// Runs started since the last drain — the per-step `started` list, in
    /// start order.
    started: Vec<RunToken>,
    acceptances: AcceptanceRecorder,
    intents: IntentRecorder,
    settled: bool,
}

impl<P: Program> Kernel<P> {
    /// Inert construction: no task, no poll, no `init` call (RFC 0011
    /// INV-LC3).
    ///
    /// Only the two surviving controls are read from `config`
    /// ([`RuntimeConfig::kernel_controls`]): the data lane's capacity and
    /// the input batch's count cap. The frame rate and the per-command
    /// channel capacity have no successor here, and not reading them is
    /// enforced by that method's return type rather than by this function
    /// remembering to skip them.
    ///
    /// The data lane is built through the runtime's own observed channel, so
    /// bounded mode brings the capacity-wait event and the `blocked` gauge
    /// with it under the `data` label (RFC 0006 §4.4 as RFC 0014 §9 row 9
    /// amends it). The control lane is unbounded and not configurable
    /// (RFC 0014 §3.3). Both senders are kept for the kernel's lifetime:
    /// that is the ownership invariant `lane` documents, and it is what
    /// makes a closed lane unreachable while the kernel lives.
    pub fn new(
        program: P,
        flags: P::Flags,
        config: &RuntimeConfig,
        gate_mode: GateMode,
        observer: LoadObserver,
    ) -> Self {
        let (capacity, batch_max) = config.kernel_controls();
        let (data_tx, data_rx) = channel_observed(capacity, Channel::Data, observer.clone());
        let (control_tx, control_rx) = control_lane();
        Self {
            program,
            flags: Some(flags),
            state: None,
            registry: ScopeRegistry::new(observer.clone()),
            join_set: JoinSet::new(),
            task_index: HashMap::new(),
            data_rx: Some(data_rx),
            data_tx,
            data_buf: VecDeque::new(),
            control_rx: Some(control_rx),
            control_tx,
            control_buf: VecDeque::new(),
            exit_buf: VecDeque::new(),
            observer,
            redraw_pending: false,
            dirty: false,
            phase: KernelPhase::Boot,
            batch_cap: batch_max.unwrap_or(DEFAULT_BATCH_MAX_MESSAGES),
            gate: Arc::new(SendGate::new(gate_mode)),
            next_token: 1,
            started: Vec::new(),
            acceptances: AcceptanceRecorder::new(gate_mode),
            intents: IntentRecorder::new(gate_mode),
            settled: false,
        }
    }

    /// Bootstrap: the pinned intake order, then the continuation pass that
    /// consumes the render it left pending.
    ///
    /// Intake is RFC 0011 §3.2's, unchanged — dispatch the init command,
    /// then the initial reconcile, then mark the first redraw
    /// unconditionally and independently of the init command's own redraw
    /// directive. That leaves work outstanding, so INV-RC16's park condition
    /// ("nothing to make progress on") is not met and the kernel does not
    /// park; the continuation pass is therefore run here rather than left
    /// for a caller to remember, which is what makes the production loop and
    /// the stage-3 driver reach the same post-boot state by the same route
    /// (RFC 0008 §9.5).
    ///
    /// A quit dispatched by `init` short-circuits synchronously: the
    /// reconcile is skipped, no subscription source starts, no render
    /// happens, the continuation pass never runs, and the kernel never
    /// reaches steady state (RFC 0014 §6.2, amending RFC 0011's bootstrap).
    ///
    /// # Errors
    ///
    /// Returns the backend's error when the continuation pass's render
    /// fails; the kernel has terminated by then (RFC 0011 INV-LC5).
    ///
    /// # Panics
    ///
    /// Panics when called twice: bootstrap consumes the flags, and a second
    /// boot has none.
    pub fn boot<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> Result<BootReport, B::Error> {
        assert!(self.phase == KernelPhase::Boot, "boot runs once");
        let flags = self.flags.take().expect("boot consumes the flags once");
        let (state, init) = self.program.init(flags);
        self.state = Some(state);

        self.dispatch(init);
        if !self.terminating() {
            self.reconcile();
        }
        // Unconditional, and independent of the init command's redraw
        // directive, which the kernel never consults here (RFC 0011
        // INV-LC4). A terminated bootstrap still marks it and still never
        // renders: the continuation pass below does not run.
        self.redraw_pending = true;

        if !self.terminating() {
            self.phase = KernelPhase::Steady;
            self.pass_cycle(terminal)?;
        }

        Ok(BootReport {
            producers: self.take_started(),
        })
    }

    /// The booted state.
    pub const fn state(&self) -> &P::State {
        self.state.as_ref().expect("kernel booted")
    }

    /// Whether termination has been applied.
    pub const fn terminating(&self) -> bool {
        matches!(
            self.phase,
            KernelPhase::Terminating(_) | KernelPhase::Settled
        )
    }

    /// The reason termination was applied for, while the settle is still
    /// pending. `None` before termination and after the settle consumed it.
    const fn pending_exit_reason(&self) -> Option<ExitReason> {
        match self.phase {
            KernelPhase::Terminating(reason) => Some(reason),
            KernelPhase::Boot | KernelPhase::Steady | KernelPhase::Settled => None,
        }
    }

    /// The run bookkeeping, for the driver's probes.
    pub const fn registry(&self) -> &ScopeRegistry {
        &self.registry
    }

    /// The load observer this kernel publishes gauges to.
    pub const fn observer(&self) -> &LoadObserver {
        &self.observer
    }

    /// The one send gate every run's ingress waits on.
    ///
    /// The driver arms grants here; production's gate is transparent and
    /// this accessor is how the *same* object reaches both, rather than the
    /// driver installing one of its own (RFC 0008 §9.6).
    pub const fn gate(&self) -> &Arc<SendGate> {
        &self.gate
    }

    /// Takes the runs started since the last drain, in start order.
    ///
    /// This is what a driving step reports as its `started` list. Draining
    /// rather than reading keeps the list per-step without the kernel
    /// tracking step boundaries it otherwise has no notion of.
    pub fn take_started(&mut self) -> Vec<RunToken> {
        mem::take(&mut self.started)
    }

    /// How many runtime-owned tasks the kernel still owns.
    ///
    /// This is the settle loop's input (RFC 0011 INV-LC7), not part of any
    /// observability schema — the gauges are `tears::runtime::load`'s and
    /// keep their own vocabulary.
    pub fn owned_task_count(&self) -> usize {
        self.join_set.len()
    }

    /// The post-gate acceptance ledger: what passed the send gate, which is
    /// not the same question as what `update` saw (RFC 0008 §9.6).
    pub fn acceptances(&self) -> AcceptanceRecorder {
        self.acceptances.clone()
    }

    /// The pre-gate intent ledger.
    pub fn intents(&self) -> IntentRecorder {
        self.intents.clone()
    }

    /// Applies one command: the cancel phase (explicit cancels and teardown
    /// prefixes), then the spawn phase in declaration order, then a
    /// synchronous quit if the command carried one (RFC 0014 §3.4).
    ///
    /// The quit is applied at the *completion* of the dispatch, so siblings
    /// already spawned by this same command exist and are then torn down by
    /// termination. The order is the plan's, not this loop's: the lowering
    /// hands over one ordered sequence precisely so the phase order cannot
    /// be re-derived — or misremembered — here.
    ///
    /// The redraw directive is OR-folded into the pending mark rather than
    /// forced, which is what leaves `without_redraw` meaningful for a
    /// command whose `update` ran (RFC 0002's separation).
    pub fn dispatch(&mut self, command: Command<P::Message>) {
        let plan = lowering::dispatch_plan(lowering::lower(command.into_runtime_parts()));
        self.redraw_pending |= plan.redraw;
        for step in plan.steps {
            match step {
                DispatchStep::Cancel(id) => self.apply_cancel(&id),
                DispatchStep::Teardown(prefix) => self.apply_teardown(&prefix),
                DispatchStep::Spawn(spawn) => self.apply_spawn(spawn),
                DispatchStep::Quit => self.apply_quit(ExitReason::Quit),
            }
        }
    }

    /// Applies one explicit cancel: revoke the run holding the id's slot.
    ///
    /// Strict and total — an id with no deliverable run cancels nothing and
    /// is not an error (RFC 0003's cancel semantics on the new
    /// bookkeeping).
    fn apply_cancel(&mut self, id: &CommandId) {
        if let Some(occupant) = self.registry.keyed_occupant(id) {
            self.registry.stop_request(occupant);
        }
    }

    /// Applies one spawn entry, honoring the keyed slot policy: a
    /// `CancelInFlight` spawn stops the live occupant and starts fresh, a
    /// `KeepInFlight` spawn is discarded while the slot is occupied
    /// (RFC 0003's policy, on the new registry).
    fn apply_spawn(&mut self, spawn: SpawnEntry<P::Message>) {
        match lowering::spawn_decision(&self.registry, spawn.key.as_ref()) {
            SpawnDecision::Suppress => return,
            SpawnDecision::Replace(occupant) => {
                self.registry.stop_request(occupant);
            }
            SpawnDecision::Start => {}
        }
        let SpawnEntry { key, scope, stream } = spawn;
        let kind = key.map_or(RunKind::Anon, |key| RunKind::Keyed(key.id));
        self.spawn_producer(kind, scope, producer::command_body(stream));
    }

    /// Starts one runtime-owned producer run and records its entry.
    ///
    /// The token is minted once per run and never reused, which is what
    /// makes a predecessor's late exit and late sends inert with respect to
    /// its successor in the same identity slot (RFC 0014 §3.1).
    fn spawn_producer(
        &mut self,
        kind: RunKind,
        scope: ScopePath,
        body: producer::EffectBody<P::Message>,
    ) -> RunToken {
        let token = self.next_token;
        self.next_token += 1;
        let entry = producer::ProducerHarness {
            join_set: &mut self.join_set,
            task_index: &mut self.task_index,
            data: &self.data_tx,
            control: &self.control_tx,
            intents: &self.intents,
            acceptances: &self.acceptances,
            observer: &self.observer,
            gate: &self.gate,
        }
        .start(token, kind, scope, body);
        self.registry.insert(entry);
        self.started.push(token);
        token
    }

    /// Controlled termination: the phase transition plus the immediate
    /// postcondition, shared by every cause (RFC 0011 §4.4).
    ///
    /// All four causes — an `update`-returned quit, a producer-originated
    /// quit, a render failure, and a host-side drop — reach the postcondition
    /// through this one call, so none of them can carry a shutdown step of
    /// its own. Idempotent: the second cause to arrive finds the kernel
    /// already terminating and changes nothing, which keeps the first
    /// reason authoritative.
    fn apply_quit(&mut self, reason: ExitReason) {
        if self.terminating() {
            return;
        }
        self.phase = KernelPhase::Terminating(reason);
        self.immediate_postcondition();
    }

    /// The immediate postcondition: drop both receivers so in-flight sends
    /// fail, clear the buffers, and revoke and abort every run.
    ///
    /// Dropping the receivers is what makes a producer blocked on a bounded
    /// lane fail its send and end its own run, and clearing the buffers is
    /// what makes the "no output undelivered at termination is ever
    /// delivered late" half hold for envelopes the park already took off a
    /// lane (RFC 0011 §4.4).
    fn immediate_postcondition(&mut self) {
        self.data_rx = None;
        self.data_buf.clear();
        self.control_rx = None;
        self.control_buf.clear();
        self.registry.abort_all();
    }

    /// The quiescent postcondition: join the shared join set empty, then
    /// clear the run bookkeeping.
    ///
    /// Bounded by quiescence rather than by a fixed pass count, and it uses
    /// no clock (RFC 0011 INV-LC7): the drain ends when the join set is
    /// empty, and every entry in it has had its abort requested by the
    /// immediate postcondition, so no further kernel action is needed to get
    /// there.
    ///
    /// The registry is cleared *after* the drain and unconditionally. After
    /// the immediate postcondition no envelope can be dequeued, so no
    /// tombstone could ever satisfy the removal condition again and the
    /// bookkeeping — with the `keyed_commands` gauge on it — would stay
    /// non-zero for the rest of the kernel's life.
    ///
    /// # Panics
    ///
    /// Panics when the kernel has not terminated: there is nothing to settle
    /// and no reason to report.
    pub async fn settle(&mut self) -> ExitReport {
        let reason = self
            .pending_exit_reason()
            .expect("settle follows termination");
        // Exits already reflected into the bookkeeping left the join set
        // before this drain, so they are counted here rather than lost.
        let mut joined = self.exit_buf.len();
        self.exit_buf.clear();
        while self.join_set.join_next().await.is_some() {
            joined += 1;
        }
        self.task_index.clear();
        self.registry.clear();
        self.settled = true;
        self.phase = KernelPhase::Settled;
        ExitReport { reason, joined }
    }

    /// The production driving loop: bootstrap, then fixed passes over the
    /// shared stage implementation, parking when nothing has work.
    ///
    /// This is the only place the production path differs from the driver's:
    /// the driver replaces the park-and-initiate step with a scripted start
    /// and installs a scripted send gate. The bootstrap is the same
    /// [`Kernel::boot`] call and the pass is the same
    /// [`Kernel::pass_cycle`] call.
    ///
    /// Termination settles before returning under **either** classification,
    /// so a render error reaches the quiescent postcondition exactly as a
    /// quit does; only the return value distinguishes them (RFC 0011
    /// INV-LC5's `Err`).
    ///
    /// # Errors
    ///
    /// Returns the backend's error when a render failed. The kernel has
    /// terminated and settled by then; the error is the classification, not
    /// an escape from the postconditions.
    pub async fn run<B: Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
    ) -> Result<ExitReport, B::Error> {
        let outcome = self.drive(terminal).await;
        let report = self.settle().await;
        outcome.map(|()| report)
    }

    /// The loop itself, up to termination.
    async fn drive<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> Result<(), B::Error> {
        self.boot(terminal)?;
        while !self.terminating() {
            if self.pass_work_ready() {
                self.pass_cycle(terminal)?;
            } else {
                self.park().await;
            }
        }
        Ok(())
    }

    /// Whether a task exit is observable, buffering it if one is.
    ///
    /// Reads the real join set, so this readiness cannot be fabricated.
    fn poll_exit(&mut self) -> bool {
        if !self.exit_buf.is_empty() {
            return true;
        }
        self.join_set.try_join_next_with_id().is_some_and(|result| {
            self.buffer_exit(result);
            true
        })
    }

    /// Records one observed task exit against the run it belongs to.
    ///
    /// # Panics
    ///
    /// Panics on a task the index does not know: every runtime-owned task is
    /// started through the one harness that indexes it, so an unindexed exit
    /// is that ownership invariant failing.
    fn buffer_exit(&mut self, result: Result<(TaskId, ()), JoinError>) {
        let (id, outcome) = match result {
            Ok((id, ())) => (id, ExitOutcome::Completed),
            Err(error) => {
                let outcome = if error.is_panic() {
                    ExitOutcome::Panicked
                } else {
                    ExitOutcome::Cancelled
                };
                (error.id(), outcome)
            }
        };
        let token = self
            .task_index
            .remove(&id)
            .expect("every runtime-owned task is indexed at its start");
        self.exit_buf.push_back((token, outcome));
    }

    /// The next data-lane envelope: the park's buffer first, then the lane.
    fn next_data(&mut self) -> Option<Envelope<P::Message>> {
        if let Some(envelope) = self.data_buf.pop_front() {
            return Some(envelope);
        }
        self.data_rx.as_mut().and_then(|rx| rx.try_recv().ok())
    }

    /// The next control-lane envelope: the park's buffer first, then the
    /// lane.
    fn next_control(&mut self) -> Option<Envelope<P::Message>> {
        if let Some(envelope) = self.control_buf.pop_front() {
            return Some(envelope);
        }
        self.control_rx.as_mut().and_then(|rx| rx.try_recv().ok())
    }
}

impl<P: Program> Drop for Kernel<P> {
    /// A kernel dropped without settling still aborts what it owns, so no
    /// runtime-owned task outlives it.
    fn drop(&mut self) {
        if !self.settled {
            self.registry.abort_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::num::{NonZeroU32, NonZeroUsize};
    use std::sync::Mutex;

    use futures::stream;
    use ratatui::Frame;
    use ratatui::backend::TestBackend;
    use tokio::task::yield_now;

    use crate::reducer::Reducer;
    use crate::runtime::frame_rate::FrameRate;
    use crate::subscription::Subscription;
    use crate::subscription::mock::MockSource;
    use crate::test_support::TraceRecorder;

    use accounting::PendingReservation;
    use arbiter::WakeSource;
    use lane::Payload;
    use registry::Phase;

    /// One recorded call into the program, in the order the kernel made it.
    ///
    /// The four calls are the whole application surface a pass touches, so
    /// this journal is what pins the intake order and the stage order
    /// without any probe inside the kernel.
    #[derive(Clone, Debug, Eq, PartialEq)]
    enum Call {
        Init,
        Reduce(u8),
        View,
        Subscriptions,
    }

    #[derive(Clone, Debug, Default)]
    struct Journal(Arc<Mutex<Vec<Call>>>);

    impl Journal {
        fn record(&self, call: Call) {
            self.0.lock().expect("journal lock").push(call);
        }

        fn calls(&self) -> Vec<Call> {
            self.0.lock().expect("journal lock").clone()
        }
    }

    /// What the program is told to do, handed over at `init`.
    struct Setup {
        /// The init command, which the program returns unchanged.
        init: Command<u8>,
        /// The commands `reduce` returns, in order; exhausted it returns
        /// [`Command::none`].
        replies: VecDeque<Command<u8>>,
        /// The sources the state declares.
        sources: Vec<MockSource<u8>>,
    }

    impl Setup {
        fn new(init: Command<u8>) -> Self {
            Self {
                init,
                replies: VecDeque::new(),
                sources: Vec::new(),
            }
        }

        fn replying(mut self, replies: impl IntoIterator<Item = Command<u8>>) -> Self {
            self.replies = replies.into_iter().collect();
            self
        }

        fn declaring(mut self, sources: Vec<MockSource<u8>>) -> Self {
            self.sources = sources;
            self
        }
    }

    struct State {
        replies: VecDeque<Command<u8>>,
        sources: Vec<MockSource<u8>>,
    }

    /// A program that records every call the kernel makes into it.
    struct Probe {
        journal: Journal,
    }

    impl Reducer for Probe {
        type State = State;
        type Message = u8;

        fn reduce(&self, state: &mut State, message: u8) -> Command<u8> {
            self.journal.record(Call::Reduce(message));
            state.replies.pop_front().unwrap_or_else(Command::none)
        }

        fn subscriptions(&self, state: &State) -> Vec<Subscription<u8>> {
            self.journal.record(Call::Subscriptions);
            state
                .sources
                .iter()
                .map(|source| Subscription::new(source.clone()))
                .collect()
        }
    }

    impl Program for Probe {
        type Flags = Setup;

        fn init(&self, flags: Setup) -> (State, Command<u8>) {
            self.journal.record(Call::Init);
            let Setup {
                init,
                replies,
                sources,
            } = flags;
            (State { replies, sources }, init)
        }

        fn view(&self, _state: &State, _frame: &mut Frame<'_>) {
            self.journal.record(Call::View);
        }
    }

    fn config() -> RuntimeConfig {
        RuntimeConfig::new(
            FrameRate::new(NonZeroU32::new(60).expect("non-zero")).expect("a valid frame rate"),
        )
    }

    fn terminal() -> Terminal<TestBackend> {
        Terminal::new(TestBackend::new(8, 2)).expect("the test backend never fails")
    }

    /// A kernel over the probe program, plus the journal it records into.
    fn kernel(setup: Setup, config: &RuntimeConfig) -> (Kernel<Probe>, Journal) {
        let journal = Journal::default();
        let program = Probe {
            journal: journal.clone(),
        };
        let kernel = Kernel::new(
            program,
            setup,
            config,
            GateMode::Immediate,
            LoadObserver::new(),
        );
        (kernel, journal)
    }

    /// An effect that never produces and never ends, so a run exists to
    /// attribute injected envelopes to and no output arrives unbidden.
    fn silent_effect() -> Command<u8> {
        Command::stream(stream::pending())
    }

    impl Kernel<Probe> {
        /// Enqueues one envelope exactly as a producer's send does —
        /// reserve, enqueue, commit — so the delivery accounting sees the
        /// same sequence it would from [`lane::IngressHandle`], without the
        /// executor turns a real producer would need to reach its send.
        fn enqueue(&self, origin: RunToken, payload: Payload<u8>) {
            let counter = Arc::clone(
                &self
                    .registry
                    .get(origin)
                    .expect("the origin has a registry entry")
                    .counter,
            );
            let reservation = PendingReservation::new(counter);
            let envelope = Envelope { origin, payload };
            match payload_lane(&envelope.payload) {
                lane::Lane::Data => self
                    .data_tx
                    .try_send(envelope)
                    .unwrap_or_else(|_| unreachable!()),
                lane::Lane::Control => self
                    .control_tx
                    .send(envelope)
                    .unwrap_or_else(|_| unreachable!()),
            }
            reservation.commit();
        }
    }

    const fn payload_lane(payload: &Payload<u8>) -> lane::Lane {
        match payload {
            Payload::Msg(_) => lane::Lane::Data,
            Payload::Quit => lane::Lane::Control,
        }
    }

    // RFC 0011 §3.2 intake plus RFC 0008 §9.5's continuation pass: the init
    // command is dispatched first, the initial reconcile follows it, and the
    // render the intake left pending is consumed before `boot` returns — so
    // the kernel `boot` hands back is one that may park.
    #[tokio::test]
    async fn booting_dispatches_init_then_reconciles_then_consumes_the_first_render() {
        let mut screen = terminal();
        let (mut kernel, journal) = kernel(Setup::new(Command::none()), &config());

        kernel.boot(&mut screen).expect("the test backend renders");

        assert_eq!(
            journal.calls(),
            vec![Call::Init, Call::Subscriptions, Call::View],
            "init dispatch, then the initial reconcile, then the continuation pass's render"
        );
        assert!(
            !kernel.redraw_pending,
            "the continuation pass consumed the pending first render"
        );
        assert!(
            !kernel.pass_work_ready(),
            "a booted kernel with no arrivals has nothing left to do, so it parks"
        );
    }

    // RFC 0014 §6.2: an init quit terminates *during* the init dispatch —
    // before the initial reconcile and before any render — so neither the
    // reconcile nor the continuation pass runs.
    #[tokio::test]
    async fn an_init_quit_short_circuits_before_the_reconcile_and_before_any_render() {
        let mut screen = terminal();
        let (mut kernel, journal) = kernel(
            Setup::new(Command::batch([silent_effect(), Command::quit()])),
            &config(),
        );

        let report = kernel.boot(&mut screen).expect("no render was attempted");

        assert_eq!(
            journal.calls(),
            vec![Call::Init],
            "no reconcile and no render follow an init quit"
        );
        assert!(kernel.terminating(), "the quit applied at the dispatch");
        assert_eq!(
            report.producers.len(),
            1,
            "the sibling spawned before the quit exists, and termination tears it down"
        );
        assert_eq!(
            kernel.registry().len(),
            1,
            "the sibling is revoked rather than forgotten"
        );
    }

    // The admission site: the initial reconcile starts the declared sources,
    // and it does so on the driving task (RFC 0012 INV-SE1) — the spawner is
    // consumed here, not handed to the runtime-owned task.
    #[tokio::test]
    async fn bootstrap_admits_the_declared_subscriptions() {
        let mut screen = terminal();
        let (mut kernel, _journal) = kernel(
            Setup::new(Command::none()).declaring(vec![MockSource::new()]),
            &config(),
        );

        let report = kernel.boot(&mut screen).expect("the test backend renders");

        assert_eq!(
            report.producers.len(),
            1,
            "the declared source was admitted"
        );
        let entry = kernel
            .registry()
            .get(report.producers[0])
            .expect("the admitted run has an entry");
        assert!(
            matches!(entry.kind, RunKind::Sub(_)),
            "an admitted subscription is a subscription run"
        );
        assert_eq!(entry.phase, Phase::Running);
    }

    // INV-RC9's pass-start row: a producer quit that has arrived when a pass
    // begins is applied by that pass's control drain with **zero** further
    // inputs processed, however much input is ready behind it.
    #[tokio::test]
    async fn a_producer_quit_ready_at_pass_start_is_applied_with_zero_inputs_processed() {
        let mut screen = terminal();
        let (mut kernel, journal) = kernel(Setup::new(silent_effect()), &config());
        let report = kernel.boot(&mut screen).expect("the test backend renders");
        let run = report.producers[0];

        for message in 1..=3 {
            kernel.enqueue(run, Payload::Msg(message));
        }
        kernel.enqueue(run, Payload::Quit);

        kernel.pass_cycle(&mut screen).expect("no render failed");

        assert!(kernel.terminating(), "the quit was applied by this pass");
        assert!(
            !journal.calls().contains(&Call::Reduce(1)),
            "the control drain precedes the input batch, so no ready input ran first"
        );
    }

    // RFC 0014 §3.5's stage 3: the batch is always count-bounded, so it ends
    // after a finite prefix of the ready input and the pass reaches its frame
    // stage however much input stays ready.
    #[tokio::test]
    async fn an_input_batch_stops_at_the_configured_count_cap() {
        let config = config().batch_max_messages(NonZeroUsize::new(2).expect("non-zero"));
        let mut screen = terminal();
        let (mut kernel, journal) = kernel(Setup::new(silent_effect()), &config);
        let report = kernel.boot(&mut screen).expect("the test backend renders");
        let run = report.producers[0];

        for message in 1..=5 {
            kernel.enqueue(run, Payload::Msg(message));
        }

        kernel.pass_cycle(&mut screen).expect("no render failed");
        let after_one = journal.calls();
        kernel.pass_cycle(&mut screen).expect("no render failed");
        let after_two = journal.calls();

        let reduced = |calls: &[Call]| {
            calls
                .iter()
                .filter(|call| matches!(call, Call::Reduce(_)))
                .count()
        };
        assert_eq!(reduced(&after_one), 2, "one batch takes at most the cap");
        assert_eq!(reduced(&after_two), 4, "and the next pass takes the next");
    }

    // INV-RC5: from the revocation's application point none of the run's
    // output is delivered — buffered before it included — so `update` is
    // never reached with it.
    #[tokio::test]
    async fn a_revoked_origin_s_envelope_never_reaches_update() {
        let mut screen = terminal();
        let (mut kernel, journal) = kernel(Setup::new(silent_effect()), &config());
        let report = kernel.boot(&mut screen).expect("the test backend renders");
        let run = report.producers[0];

        kernel.enqueue(run, Payload::Msg(7));
        kernel.registry.stop_request(run);

        kernel.pass_cycle(&mut screen).expect("no render failed");

        assert!(
            !journal.calls().contains(&Call::Reduce(7)),
            "the buffered envelope of a revoked run is discarded at its dequeue"
        );
    }

    // RFC 0011 INV-LC2: within one pass the render precedes the
    // re-evaluation, and both observe the pass's current state.
    #[tokio::test]
    async fn a_batch_that_ran_update_renders_before_it_re_evaluates() {
        let mut screen = terminal();
        let (mut kernel, journal) = kernel(Setup::new(silent_effect()), &config());
        let report = kernel.boot(&mut screen).expect("the test backend renders");
        let run = report.producers[0];
        let before = journal.calls().len();

        kernel.enqueue(run, Payload::Msg(7));
        kernel.pass_cycle(&mut screen).expect("no render failed");

        assert_eq!(
            journal.calls()[before..],
            [Call::Reduce(7), Call::View, Call::Subscriptions],
            "the batch runs, then the frame stage renders, then it re-evaluates"
        );
    }

    // RFC 0002's separation, observed from the pass side: a batch marks
    // subscription dirt because it ran `update`, but the redraw is the
    // command's own directive — so `without_redraw` still suppresses the
    // render while the re-evaluation happens anyway.
    #[tokio::test]
    async fn a_without_redraw_reply_re_evaluates_without_rendering() {
        let mut screen = terminal();
        let (mut kernel, journal) = kernel(
            Setup::new(silent_effect()).replying([Command::none().without_redraw()]),
            &config(),
        );
        let report = kernel.boot(&mut screen).expect("the test backend renders");
        let run = report.producers[0];
        let before = journal.calls().len();

        kernel.enqueue(run, Payload::Msg(7));
        kernel.pass_cycle(&mut screen).expect("no render failed");

        assert_eq!(
            journal.calls()[before..],
            [Call::Reduce(7), Call::Subscriptions],
            "no render, and the re-evaluation still runs"
        );
    }

    // INV-RC16, the data row, plus rule 10: the park buffers the item that
    // woke it, so readiness after the park is the buffer's — the envelope is
    // processed by the pass the park began rather than skipped by it.
    #[tokio::test]
    async fn parking_buffers_the_data_lane_item_that_woke_it() {
        let mut screen = terminal();
        let (mut kernel, journal) = kernel(Setup::new(silent_effect()), &config());
        let report = kernel.boot(&mut screen).expect("the test backend renders");
        let run = report.producers[0];

        kernel.enqueue(run, Payload::Msg(7));
        let woken = kernel.park().await;

        assert_eq!(woken, WakeSource::Data);
        assert_eq!(kernel.data_buf.len(), 1, "the woken item is buffered");
        assert!(
            kernel.wake_source_ready(WakeSource::Data),
            "readiness reads the buffer, not only the lane"
        );

        kernel.pass_cycle(&mut screen).expect("no render failed");
        assert!(
            journal.calls().contains(&Call::Reduce(7)),
            "the pass the park began processes the item the park took"
        );
    }

    // INV-RC16, the control row.
    #[tokio::test]
    async fn parking_buffers_the_control_lane_quit_that_woke_it() {
        let mut screen = terminal();
        let (mut kernel, _journal) = kernel(Setup::new(silent_effect()), &config());
        let report = kernel.boot(&mut screen).expect("the test backend renders");
        let run = report.producers[0];

        kernel.enqueue(run, Payload::Quit);
        let woken = kernel.park().await;

        assert_eq!(woken, WakeSource::Control);
        assert_eq!(kernel.control_buf.len(), 1);

        kernel.pass_cycle(&mut screen).expect("no render failed");
        assert!(kernel.terminating(), "the buffered quit was applied");
    }

    // INV-RC16, the producer-exit row: the arming covers the join set too,
    // and the woken pass's first stage is what reflects the exit.
    #[tokio::test]
    async fn parking_wakes_on_a_producer_exit() {
        let mut screen = terminal();
        let (mut kernel, _journal) =
            kernel(Setup::new(Command::stream(stream::empty())), &config());
        let report = kernel.boot(&mut screen).expect("the test backend renders");
        let run = report.producers[0];

        let woken = kernel.park().await;

        assert_eq!(woken, WakeSource::ProducerExit);
        kernel.pass_cycle(&mut screen).expect("no render failed");
        assert!(
            kernel.registry().get(run).is_none(),
            "the reflected exit retired an entry with nothing pending"
        );
    }

    // The production kernel's ledgers are inert: a run's sends commit and
    // deliver through the ordinary path, and neither ledger gains an entry
    // for them. The split is the send gate's own — production builds an
    // immediate gate beside an inert ledger — which is what keeps an
    // append-only log with no reader out of a process that runs for days.
    #[tokio::test]
    async fn a_production_kernel_records_nothing_in_either_ledger() {
        let mut screen = terminal();
        let (mut kernel, journal) = kernel(
            Setup::new(Command::stream(stream::iter(1..=4_u8))),
            &config(),
        );
        kernel.boot(&mut screen).expect("the test backend renders");

        for _ in 0..8 {
            yield_now().await;
        }
        kernel.pass_cycle(&mut screen).expect("no render failed");

        assert!(
            journal.calls().contains(&Call::Reduce(1)),
            "the production send path ran and delivered"
        );
        assert!(
            kernel.acceptances().snapshot().is_empty(),
            "yet no acceptance was recorded"
        );
        assert!(
            kernel.intents().snapshot().is_empty(),
            "and no send-intent either"
        );
    }

    // The lane-ownership invariant the park's degenerate branch rests on:
    // the kernel holds a clone of both senders for its whole lifetime, so a
    // receive cannot observe a closed lane even with no producer alive. This
    // is the regression for dropping that clone — without it the `None` arm
    // of the park's `select!` becomes reachable, and a park would have to
    // decide between spinning and asserting.
    #[tokio::test]
    async fn a_live_kernel_never_observes_a_closed_lane() {
        let mut screen = terminal();
        let (mut kernel, _journal) = kernel(Setup::new(Command::none()), &config());
        kernel.boot(&mut screen).expect("the test backend renders");

        assert_eq!(kernel.owned_task_count(), 0, "no producer is alive");
        assert!(
            !kernel
                .data_rx
                .as_ref()
                .expect("the lane is open")
                .is_closed(),
            "the kernel's own sender clone keeps the data lane open"
        );
        assert!(
            !kernel
                .control_rx
                .as_ref()
                .expect("the lane is open")
                .is_closed(),
            "and the control lane with it"
        );
    }

    // The frame/wake split: pending frame work keeps the kernel out of the
    // park without being a source that could ever wake it.
    #[tokio::test]
    async fn a_pending_frame_is_work_but_is_no_wake_source() {
        let mut screen = terminal();
        let (mut kernel, _journal) = kernel(Setup::new(Command::none()), &config());
        kernel.boot(&mut screen).expect("the test backend renders");

        kernel.dispatch(Command::none());

        assert!(
            kernel.pass_work_ready(),
            "a marked redraw is work to make progress on"
        );
        assert!(
            WakeSource::ALL
                .into_iter()
                .all(|source| !kernel.wake_source_ready(source)),
            "and no wake source has anything: nothing would arrive to end a park"
        );
    }

    // RFC 0011 §4.4's two stages: the immediate postcondition requests every
    // cancellation, and the settle is bounded by quiescence rather than by a
    // pass count — no clock, no fixed number of turns. INV-LC7's gauge half
    // is read where the gauges live, at the `tracing` surface.
    #[tokio::test]
    async fn settling_drains_the_join_set_clears_the_bookkeeping_and_zeroes_the_gauges() {
        let recorder = TraceRecorder::new().with_target("tears::runtime::load");
        let _guard = recorder.set_default();
        let mut screen = terminal();
        let (mut kernel, _journal) = kernel(Setup::new(silent_effect()), &config());
        kernel.boot(&mut screen).expect("the test backend renders");
        assert_eq!(kernel.owned_task_count(), 1);

        kernel.dispatch(Command::quit());
        let report = kernel.settle().await;

        assert_eq!(report.reason, ExitReason::Quit);
        assert_eq!(report.joined, 1, "the one runtime-owned task was joined");
        assert_eq!(kernel.owned_task_count(), 0);
        assert_eq!(kernel.registry().len(), 0, "the bookkeeping is cleared");
        assert_eq!(
            recorder.u64_values("unkeyed_commands").last(),
            Some(&0),
            "the producer gauge fell with the task the settle drained"
        );
    }
}
