//! The stage-3 driving surface (RFC 0008 §9).
//!
//! [`TestDriver`] drives the very same [`Kernel`] production does: the same
//! construction path, the same runtime-owned tasks on the same join set, the
//! same lanes, the same pass implementation, the same termination. The
//! driving differential is confined to **two seams** plus application-side
//! inputs (RFC 0014 §7.2, INV-RC13):
//!
//! 1. **pass initiation** — the driver names which armed source begins a
//!    pass, instead of a park choosing among the ready ones (§9.5);
//! 2. **the send gate** — the driver releases producer sends one at a time,
//!    instead of every send being ready on its first poll (§9.6).
//!
//! Readiness is not part of the differential and cannot be fabricated:
//! [`TestDriver::step_pass`] reads the named source from the real lanes and
//! the real join set, so a `ProducerExit` step requires an actual task exit.
//!
//! # What has no constructor here
//!
//! Five shapes are absent by design, because each would make the driver a
//! second architecture rather than a script over the first (RFC 0008 §9.2):
//! retaining a run by hand; reimplementing reconciliation; a second quit
//! route; polling an effect directly; ingesting into the kernel without a
//! producer. No method here takes or returns a run, a task, a join handle, a
//! leaf, or a future, and none enqueues onto either lane.
//!
//! # Waiting
//!
//! Every wait this layer performs is a bounded number of executor turns:
//! nothing here sleeps, arms a timer, or reads a wall clock, and exhausting
//! a bound fails the test with a diagnostic rather than waiting longer
//! (RFC 0008 §9.3). Both waiting calls — [`TestDriver::settle`] and
//! [`TestDriver::confirm`] — take their budget from the caller, so both
//! budgets are elements of the script; the one bound that stays the
//! driver's own is the terminated kernel's settle drain ([`TURN_BUDGET`]),
//! which no script reaches. Application-supplied effects sit outside that
//! quantifier — an effect that sleeps times its own test.
//!
//! A **turn** is defined by construction (RFC 0008 §9.6): the driving task
//! spawns a fresh no-op task onto its own executor and awaits that task's
//! completion. Nothing but public primitives — a spawn and a join — and no
//! scheduler instrumentation, which RFC 0014 §7.2 forbids outright.

// The driver's callers are the conformance series, which land after it: only
// this module's own tests exercise the surface today.
#![allow(
    dead_code,
    reason = "the stage-3 driver lands before the conformance series that drive it"
)]

use std::collections::HashMap;
use std::future::{Future, ready};
#[cfg(test)]
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Wake, Waker};

use ratatui::Terminal;
use ratatui::backend::Backend;
use tokio::runtime::{Builder, Runtime as Executor};

use crate::command::CommandId;
use crate::kernel::lane::{GateMode, RunToken};
use crate::kernel::registry::RunKind as EntryKind;
use crate::kernel::{Kernel, StartedRun};
use crate::reducer::{Exit, Program};
use crate::runtime::config::RuntimeConfig;
use crate::runtime::load::LoadObserver;
use crate::subscription::SubscriptionId;

pub use crate::kernel::arbiter::WakeSource;
pub use crate::kernel::lane::{GrantOutstanding, Lane};

/// The bound on the one wait this layer still chooses for itself, in
/// executor turns: the terminated kernel's settle drain.
///
/// Its value is mechanism, because its completion condition is not the
/// caller's — the join set drains or it does not, and any two bounds agree
/// on every execution where it does. What is contract is that the wait is
/// finite, counted in turns rather than in elapsed time, and fails the test
/// on its bound (RFC 0008 §9.3). The two waits a *script* reaches —
/// [`TestDriver::settle`] and [`TestDriver::confirm`] — take their budgets
/// from the caller instead (RFC 0008 §9.6).
const TURN_BUDGET: usize = 1024;

/// Hands each driver an identity of its own, so the names and tokens it
/// mints are its own too.
///
/// Run tokens are the *kernel's*, and every kernel starts counting at one:
/// two drivers' first runs carry the same token, so without this their names
/// would compare equal and one driver's token would name the other's grant.
/// A monotone counter is enough — it reads no clock and no entropy, and
/// nothing about the value is observable beyond "not the other driver's".
static NEXT_DRIVER: AtomicU64 = AtomicU64::new(1);

/// One executor turn, by construction (RFC 0008 §9.6).
///
/// The driving task spawns a fresh no-op task onto its own executor and
/// awaits that task's completion. The definition is a construction rather
/// than a property because the property one would rather state — that every
/// task ready at the yield gets the executor first — is not something the
/// executor's public contract offers, and the only way to *assert* it would
/// be to instrument the scheduler. Spawning one ordinary task is not
/// instrumentation: it adds no hook and reads no scheduler state.
///
/// # Panics
///
/// Panics when the no-op task did not complete, which a task that neither
/// panics nor is aborted cannot do.
async fn executor_turn() {
    tokio::spawn(ready(()))
        .await
        .expect("a no-op task neither panics nor is aborted");
}

/// One send at the gate, in the kernel's own vocabulary.
///
/// A record carries the two facts a driving test sequences on: which run
/// produced the send, and which lane carried it — so a released quit is
/// distinguishable from a released message (RFC 0008 §9.6). The run is the
/// kernel-side identity rather than a driver-side name, because the append
/// happens on the producer's own send path, where that is the only identity
/// in hand; the driver resolves it to a [`RunName`] when a test reads a
/// ledger.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GatedSend {
    /// The producing run.
    pub origin: RunToken,
    /// The lane the send took.
    pub lane: Lane,
}

/// An append-only observation log, or nothing at all.
///
/// This is a driver-side observation surface, deliberately not part of the
/// `tears::runtime::load` schema: it records what a test asserts on, and
/// widening the observability vocabulary to carry it would make an
/// evidence-gathering detail into a public contract (the RFC 0012 INV-SE8
/// boundary).
///
/// **It divides the way the send gate does, on the same construction-time
/// value.** Production builds it [`Inert`](Records::Inert): no allocation,
/// no lock on the send path, and no log that grows for the life of the
/// process with nobody to read it. A driven kernel builds it
/// [`Recording`](Records::Recording). That split is part of the second
/// driving seam's instrumentation rather than a third difference: the
/// producer's send path is one function either way, and the branch it takes
/// is the one [`GateMode`] already decides — an inert gate beside an inert
/// ledger, both observably equivalent to a kernel with neither.
#[derive(Clone, Debug)]
enum Records {
    /// Production: nothing is recorded and nothing is held.
    Inert,
    /// Driven: an append-only log shared with the producers that append.
    Recording(Arc<Mutex<Vec<GatedSend>>>),
}

impl Records {
    /// The log a kernel in `mode` records into.
    fn new(mode: GateMode) -> Self {
        match mode {
            GateMode::Immediate => Self::Inert,
            GateMode::Scripted => Self::Recording(Arc::default()),
        }
    }

    /// Appends one record, where there is a log to append to.
    fn push(&self, record: GatedSend) {
        if let Self::Recording(log) = self {
            Self::lock(log).push(record);
        }
    }

    /// Snapshot of every record, in append order. Empty when inert.
    fn snapshot(&self) -> Vec<GatedSend> {
        match self {
            Self::Inert => Vec::new(),
            Self::Recording(log) => Self::lock(log).clone(),
        }
    }

    /// How many records name `origin`.
    fn count_for(&self, origin: RunToken) -> usize {
        self.snapshot()
            .iter()
            .filter(|record| record.origin == origin)
            .count()
    }

    /// Recovers from a poisoned lock rather than propagating it: an append
    /// happens on a producer's send path, so a producer that panicked
    /// mid-send would otherwise turn every later read into a poison error
    /// in place of the test's own assertion. The data is an append-only
    /// list with no cross-record invariant, so a partial append cannot
    /// leave it inconsistent.
    fn lock(log: &Mutex<Vec<GatedSend>>) -> MutexGuard<'_, Vec<GatedSend>> {
        log.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The recorder every run's ingress appends an **acceptance** to, at the
/// commit that puts its envelope in a lane.
///
/// Accepted is not delivered: a record says an item passed the gate and says
/// nothing about whether `update` ever saw it, which is why a revoked run's
/// committed send belongs in it (RFC 0008 §9.6).
#[derive(Clone, Debug)]
pub struct AcceptanceRecorder(Records);

/// The recorder every run's ingress appends a **send-intent** to, before the
/// gate wait.
///
/// Outside the guaranteed sequence on purpose: an intent records that a
/// producer reached its send, which says nothing about whether the send was
/// admitted, committed, or filtered.
///
/// This is also the *only* record of an intent. The gate keeps no second
/// count beside it, so "how many intents has this run reached" is a question
/// with one answer, derived from the records themselves.
#[derive(Clone, Debug)]
pub struct IntentRecorder(Records);

impl AcceptanceRecorder {
    /// The acceptance ledger a kernel in `mode` records into — inert in
    /// production, recording under the driver.
    pub fn new(mode: GateMode) -> Self {
        Self(Records::new(mode))
    }

    /// Appends the acceptance of one send. Called at the commit, which is
    /// the moment the envelope is in the lane.
    pub fn record(&self, origin: RunToken, lane: Lane) {
        self.0.push(GatedSend { origin, lane });
    }

    /// Snapshot of every recorded entry, in gate order.
    pub fn snapshot(&self) -> Vec<GatedSend> {
        self.0.snapshot()
    }

    /// How many of `origin`'s sends were accepted.
    pub fn count_for(&self, origin: RunToken) -> usize {
        self.0.count_for(origin)
    }
}

impl IntentRecorder {
    /// The intent ledger a kernel in `mode` records into — inert in
    /// production, recording under the driver.
    pub fn new(mode: GateMode) -> Self {
        Self(Records::new(mode))
    }

    /// Appends one send-intent. Called before the gate wait, so it records
    /// a producer that reached its send and nothing more.
    pub fn record(&self, origin: RunToken, lane: Lane) {
        self.0.push(GatedSend { origin, lane });
    }

    /// Snapshot of every recorded entry, in append order.
    pub fn snapshot(&self) -> Vec<GatedSend> {
        self.0.snapshot()
    }

    /// How many send-intents `origin` has reached — the derived form of the
    /// count the gate deliberately does not keep.
    pub fn count_for(&self, origin: RunToken) -> usize {
        self.0.count_for(origin)
    }
}

/// Names one producer **run** — one start, not one identity (RFC 0008 §9.4).
///
/// It is per run rather than per identity because a logical identity does
/// not pick out a run: a `CancelInFlight` supersession frees the identity
/// slot for a successor at the revocation's application point while the
/// revoked run may still send late, and a subscription restarted under the
/// same `SubscriptionId` is the same case. A name carrying only the id would
/// leave a grant unable to say which of the two it means, and a ledger
/// unable to record which one sent.
///
/// Three rules, holding for every producer kind alike:
///
/// - **Minted from an observation.** A name exists only after the kernel has
///   started the run it names, and [`StepReport::started`] is the only place
///   one is minted. Nothing about a run is chosen by the test.
/// - **It reaches no kernel identity surface.** It is not a key: no keyed
///   capacity, no move into or out of a gauge count, and no admission,
///   cancellation, or teardown decision reads it. What crosses the grant
///   boundary is the run the kernel already holds, which the driver resolves
///   the name to there; the name itself stops at that boundary.
/// - **Its only uses are naming.** It names a run to [`TestDriver::grant`]
///   and tags ledger records.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct RunName {
    /// The driver that minted this name.
    ///
    /// Part of the identity, not decoration. A run token counts from one in
    /// every kernel, so two drivers' first runs share a token; without this
    /// their names would be equal and a name minted by one driver would be
    /// accepted by the other, naming a run its holder never observed.
    driver: u64,
    /// The run the kernel already holds. Private: this is the side of the
    /// name that stops at the grant boundary.
    run: RunToken,
    kind: RunKind,
}

/// A run's kind, read off a [`RunName`], carrying the logical identity the
/// kernel holds for it where there is one (RFC 0005's types, unchanged).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum RunKind {
    /// A keyed command run.
    Keyed(CommandId),
    /// A subscription run.
    Subscription(SubscriptionId),
    /// An anonymous command run, which has no logical key by decision
    /// (RFC 0013 §9's third resolution; its §10 rejects auto-keying).
    Anonymous,
}

impl RunName {
    /// What kind of run this names, with the logical identity the kernel
    /// holds for it where there is one.
    #[must_use]
    pub fn kind(&self) -> RunKind {
        self.kind.clone()
    }
}

/// One send, named by the run that made it and the lane it was for.
///
/// The same record shape serves both ledgers; what differs is what each
/// ledger's *order* is worth (RFC 0008 §9.6).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SendRecord {
    run: RunName,
    lane: Lane,
}

impl SendRecord {
    /// The run that made the send.
    #[must_use]
    pub const fn run(&self) -> &RunName {
        &self.run
    }

    /// The lane it was for.
    #[must_use]
    pub const fn lane(&self) -> Lane {
        self.lane
    }
}

/// Sends admitted past the send gate, in gate order (RFC 0008 §9.6).
///
/// This is the guaranteed observation sequence INV-RC14 scopes, and it gains
/// an entry only at an acceptance. Admitted is not delivered: a record says
/// an item passed the gate and says nothing about whether `update` ever saw
/// it. A test that wants to know an item was never delivered reads the pass
/// that dequeues it, not this ledger.
///
/// Cross-lane, the order is the *driver's* own — established by its sequence
/// of grants — and is nobody's claim about a production order (RFC 0014 §3.3
/// declines to order a run's control-lane quit against its earlier data-lane
/// output at all).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AcceptanceLedger {
    records: Vec<SendRecord>,
}

/// Send-intents recorded before the gate, in no guaranteed order (RFC 0008
/// §9.6).
///
/// A test may read this to see that a producer reached the gate; it may not
/// derive an order or a completeness claim from it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct IntentLedger {
    records: Vec<SendRecord>,
}

impl AcceptanceLedger {
    /// How many sends the gate admitted.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether the gate has admitted nothing.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// The admitted sends, in gate order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &SendRecord> {
        self.records.iter()
    }
}

impl IntentLedger {
    /// How many send-intents were recorded.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether no producer has reached a send.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// The recorded intents, in append order — which records arrival at the
    /// gate and is not an order a test may conclude from.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &SendRecord> {
        self.records.iter()
    }
}

/// What a step started, and whether it terminated the program (RFC 0008
/// §9.3).
#[derive(Debug)]
pub struct StepReport<E> {
    /// The runs this step started, in the order it started them — the only
    /// place a [`RunName`] is minted (RFC 0008 §9.4).
    ///
    /// That order is the driver's own and is not evidence of a production
    /// order (RFC 0008 §9.9); what it is for is naming a specific run when
    /// one step starts several.
    pub started: Vec<RunName>,
    /// Present exactly when the step terminated the program, carrying the
    /// production result (RFC 0014 §2.3, INV-RC11).
    pub terminated: Option<Result<Exit, E>>,
}

/// The scripted wake source had not arrived; nothing was driven (RFC 0008
/// §9.5).
///
/// Not misuse: this reports a production fact, and it leaves the driver
/// untouched.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NotReady;

/// One issued grant, correlated to the release it produced.
///
/// The token **borrows neither the driver nor the script**, and it is **not
/// a future**. Both are load-bearing. Detached, a test holds a grant
/// unresolved across `step_pass` calls, which is exactly what a send waiting
/// on lane capacity requires — the send cannot commit until the kernel
/// drains, and the kernel cannot be driven while the token borrows it; that
/// is RFC 0014 §13.3's driver-progress condition. Not a future, because
/// awaiting is not how this layer waits: [`TestDriver::confirm`] drives a
/// bounded number of executor turns and fails on its bound rather than
/// parking forever.
///
/// Neither `Copy` nor `Clone`: [`TestDriver::confirm`] consumes the token,
/// and a second copy of it would name a grant the gate no longer holds.
///
/// Sequencing is enforced at issue time rather than by this type: at most
/// one grant is outstanding **driver-wide**, so a second — at this run or
/// any other — is refused ([`GrantOutstanding`]) until this one resolves.
#[derive(Debug)]
pub struct GrantToken {
    /// The driver whose gate issued this grant.
    ///
    /// Grant sequences count from one per gate, so another driver's token
    /// would match this one's outstanding grant by number alone and take a
    /// resolution it never asked for.
    driver: u64,
    sequence: u64,
    run: RunName,
}

/// How a grant ended (RFC 0008 §9.6).
///
/// The subject is the grant, not any particular send, because a grant can be
/// armed at a run that never presents one. The two are disjoint and
/// exhaustive; both clear the outstanding grant, so `grant` and `settle` are
/// legal again after either, and only `Accepted` appends to the guaranteed
/// sequence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "a resolved grant reports which of the two terminals it reached"]
pub enum Confirmed {
    /// The send this grant released got into the lane. Whether its run is
    /// revoked is a separate, delivery-side question: revocation filters a
    /// run's output at delivery, not at admission (RFC 0014 §4.3), so a
    /// revoked run's granted send can perfectly well reach this state.
    Accepted,
    /// This grant will never put anything into the lane — the send it
    /// released ended without getting in, or the run it was armed at is gone
    /// with no send released at all.
    Reclaimed,
}

/// The driver's three states and the calls each admits (RFC 0008 §9.3).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DriverState {
    /// After `new`. Only `boot` and the two observation calls are legal.
    Constructed,
    /// After a `boot` that returned without a termination.
    Running,
    /// Once any `StepReport` carried a termination. Every driving call is
    /// misuse; the observation calls stay legal.
    Terminated,
}

impl DriverState {
    /// The state's name, for a misuse diagnostic.
    const fn name(self) -> &'static str {
        match self {
            Self::Constructed => "constructed",
            Self::Running => "running",
            Self::Terminated => "terminated",
        }
    }
}

/// The same-topology scripted driver (RFC 0008 §9.3).
pub struct TestDriver<P: Program, B: Backend> {
    /// The driver's own current-thread executor.
    ///
    /// The driving calls are synchronous and turn this executor themselves,
    /// which is what lets a whole script read as a sequence of statements.
    /// Current-thread is also the range the determinism claim is verified
    /// over (RFC 0008 §9.8).
    executor: Executor,
    kernel: Kernel<P>,
    terminal: Terminal<B>,
    /// This driver's identity, stamped into every name and token it mints.
    id: u64,
    state: DriverState,
    /// Every run the driver has observed starting, by the identity the
    /// kernel holds. This is the whole of the driver's naming: minting adds
    /// an entry, ledger reads resolve through it, and a grant resolves a
    /// name back to the run the kernel holds here rather than at the kernel.
    names: HashMap<RunToken, RunName>,
}

#[expect(
    clippy::needless_pass_by_ref_mut,
    reason = "RFC 0008 §9.3's receivers are normative rather than a spelling: every driving call \
              takes `&mut self`, which is what carries RFC 0011 INV-LC9's exclusivity and makes a \
              borrowing grant token unrepresentable"
)]
impl<P: Program, B: Backend> TestDriver<P, B> {
    /// Inert construction from the production entry point's inputs
    /// (RFC 0014 §2.3), owning a terminal to render into.
    ///
    /// Nothing starts until [`boot`](Self::boot): construction is inert for
    /// both production entry points (RFC 0011 INV-LC3) and is inert here for
    /// the same reason. The terminal is a real `ratatui::Terminal`, so
    /// `Program::view` runs exactly as it does in production and there is no
    /// way for the driver to reach a frame without going through it.
    ///
    /// # Panics
    ///
    /// Panics when a Tokio runtime is already entered on this thread: the
    /// driver owns the executor it turns, and a driving call inside another
    /// runtime cannot turn it.
    #[must_use]
    pub fn new(program: P, flags: P::Flags, config: RuntimeConfig, terminal: Terminal<B>) -> Self {
        let executor = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a current-thread executor for the driven kernel");
        Self::on(executor, program, flags, config, terminal)
    }

    /// The same construction on a multi-worker executor, for the series that
    /// need a producer to commit while a pass is running.
    ///
    /// **This is harness plumbing, not a second seam.** The executor a
    /// driver turns is mechanism: RFC 0008 §9.3's block fixes what exists on
    /// the surface, and §9.8 scopes INV-RC14's determinism *claim* to a
    /// current-thread executor — the verified range — rather than fixing the
    /// driver's own construction. Nothing else differs: the same production
    /// construction path, the same two seams, the same pass-unit stepping.
    ///
    /// What it buys is the one window a single-threaded executor does not
    /// have. A pass is a synchronous region — RFC 0014 §3.5's four stages
    /// run without the driving task yielding — so on one thread no producer
    /// can commit inside a pass, and INV-RC9's mid-batch and
    /// cancel-beats-quit rows have nothing to observe. With worker threads
    /// beside the driving thread they do, and the series that use this
    /// **carry their own determinism**: the commit is synchronized to a
    /// stage boundary by an application-side handshake, never by the
    /// scheduler, and those series cite no part of INV-RC14.
    ///
    /// Crate-visible and test-only. Whether the driving contract should
    /// carry a public form of it is the executor-independence question
    /// RFC 0014 §13.3 leaves open, and this is an input to it rather than an
    /// answer.
    ///
    /// # Panics
    ///
    /// Panics when the multi-worker executor cannot be built.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn on_worker_threads(
        program: P,
        flags: P::Flags,
        config: RuntimeConfig,
        terminal: Terminal<B>,
        workers: NonZeroUsize,
    ) -> Self {
        let executor = Builder::new_multi_thread()
            .worker_threads(workers.get())
            .enable_all()
            .build()
            .expect("a multi-worker executor for the driven kernel");
        Self::on(executor, program, flags, config, terminal)
    }

    /// The one construction body both entry points share, so the executor is
    /// the only thing that differs between them.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "RFC 0008 §9.3's constructor takes the production entry point's own inputs by \
                  value, config included, and this body is where both entry points hand them over"
    )]
    fn on(
        executor: Executor,
        program: P,
        flags: P::Flags,
        config: RuntimeConfig,
        terminal: Terminal<B>,
    ) -> Self {
        Self {
            kernel: Kernel::new(
                program,
                flags,
                &config,
                GateMode::Scripted,
                LoadObserver::new(),
            ),
            executor,
            terminal,
            id: NEXT_DRIVER.fetch_add(1, Ordering::Relaxed),
            state: DriverState::Constructed,
            names: HashMap::new(),
        }
    }

    /// Runs the production bootstrap through to a parked kernel (RFC 0008
    /// §9.5): the intake order, then the continuation pass that consumes the
    /// pending first render.
    ///
    /// Absent a termination this returns with that render consumed and no
    /// lane item outstanding — no grant has released a send — so the kernel
    /// is in the state production reaches by the same route, and the next
    /// step names one of the three sources that can wake it. An init command
    /// carrying `Command::quit()` terminates during the init dispatch,
    /// before the initial reconcile and before any render (RFC 0014 §6.2),
    /// so the report carries the termination and the continuation pass never
    /// ran.
    ///
    /// # Panics
    ///
    /// Panics when the driver has already booted: the state table admits
    /// `boot` in exactly one position.
    pub fn boot(&mut self) -> StepReport<B::Error> {
        assert!(
            self.state == DriverState::Constructed,
            "`boot` is misuse in the {} state: the state table admits it once, from the \
             constructed state (RFC 0008 §9.3)",
            self.state.name()
        );
        let (outcome, started) = {
            let Self {
                executor,
                kernel,
                terminal,
                ..
            } = self;
            executor.block_on(async {
                let stepped = match kernel.boot(terminal) {
                    // Bootstrap drains its own started list into the report
                    // it returns; a render failure returns before that
                    // drain, so the runs it started are still queued.
                    Ok(report) => (Ok(()), report.producers),
                    Err(error) => (Err(error), kernel.take_started()),
                };
                executor_turn().await;
                stepped
            })
        };
        self.finish_step(outcome, started)
    }

    /// Executes one whole production pass — RFC 0014 §3.5's four stages in
    /// their fixed order — begun by `woken_by`.
    ///
    /// Drives nothing and returns `Err(NotReady)` when that source has not
    /// arrived: readiness is read from the production lanes and the
    /// production join set, never scripted (RFC 0008 §9.5). One call is one
    /// whole pass; no method runs a stage of one.
    ///
    /// # Errors
    ///
    /// Returns [`NotReady`] when the named source has nothing, which is a
    /// production fact rather than misuse and leaves the driver untouched.
    ///
    /// # Panics
    ///
    /// Panics when the driver has not booted or has terminated (RFC 0008
    /// §9.3's state table).
    pub fn step_pass(&mut self, woken_by: WakeSource) -> Result<StepReport<B::Error>, NotReady> {
        self.assert_running("step_pass");
        let stepped = {
            let Self {
                executor,
                kernel,
                terminal,
                ..
            } = self;
            executor.block_on(async {
                // Readiness is read before any turn, so a refused step
                // drives nothing at all. The turn that follows it is the
                // one production takes at its park before the woken pass
                // runs, and it cannot unmake the readiness that admitted
                // this step: only a pass consumes from a lane or the join
                // set, and a turn only ever adds arrivals.
                if !kernel.wake_source_ready(woken_by) {
                    return None;
                }
                executor_turn().await;
                let outcome = kernel.pass_cycle(terminal);
                Some((outcome, kernel.take_started()))
            })
        };
        let (outcome, started) = stepped.ok_or(NotReady)?;
        Ok(self.finish_step(outcome, started))
    }

    /// Arms a grant at `run`, releasing the next of that run's send-intents
    /// that no grant has released yet — one already waiting at the gate when
    /// this is called, or else the first to arrive after it (RFC 0008 §9.6).
    ///
    /// A grant is therefore issuable ahead of the producer that will satisfy
    /// it, which is what lets a script fix an order before the run reaches
    /// its send point. The returned token borrows neither the driver nor the
    /// script. At most one grant is outstanding across the whole driver; the
    /// next — at this run or any other — is admitted only after this one
    /// resolves.
    ///
    /// # Errors
    ///
    /// Returns [`GrantOutstanding`] while a grant is unresolved, whatever
    /// run this one names. That is a script-order fact rather than misuse,
    /// and it leaves the driver untouched.
    ///
    /// # Panics
    ///
    /// Panics at a run the kernel's bookkeeping does not currently hold — a
    /// run never started, or one whose exit a pass has already reflected.
    /// That is a script error the kernel can produce no outcome for, and an
    /// error return would let a test go on scripting against a run that is
    /// gone. Panics too outside the running state.
    pub fn grant(&mut self, run: RunName) -> Result<GrantToken, GrantOutstanding> {
        self.assert_running("grant");
        self.assert_own_name(&run, "grant");
        assert!(
            self.holds(&run),
            "`grant` names a run the kernel's bookkeeping does not hold — a run never started, \
             or one whose exit a pass has already reflected (RFC 0008 §9.6): {:?}",
            run.kind
        );
        let sequence = self.kernel.gate().issue_grant(run.run)?;
        Ok(GrantToken {
            driver: self.id,
            sequence,
            run,
        })
    }

    /// Consumes `token`, driving the executor — beginning no pass — for at
    /// most `max_turns` turns, until the grant resolves, and reports how
    /// (RFC 0008 §9.6).
    ///
    /// A grant ends in one of exactly two states, and the two facts that
    /// establish the second are read from different places. The gate holds
    /// the first — a released send that ended without getting into the lane
    /// records its own terminal there. The second is the *kernel's*: the
    /// granted run's exit is reflected in the run bookkeeping and this grant
    /// released no send at all, so nothing is left that could arrive. The
    /// gate cannot see that one, so this call reads the bookkeeping and
    /// clears the grant on it.
    ///
    /// **The budget is the caller's**, as [`settle`](Self::settle)'s is, and
    /// for the same reason: a producer granted a release may take several
    /// turns to reach its send, so a bound of one can report exhaustion
    /// where a bound of three reports [`Confirmed::Accepted`] on the same
    /// finite execution. A driver-chosen bound would make conformance depend
    /// on a mechanism, so the number is named at the call site and is an
    /// element of the script (RFC 0008 §9.8). What is *not* the caller's is
    /// the completion condition: that one is the gate's — the grant
    /// resolving one way or the other.
    ///
    /// The disjunction is this call's *completion* condition, not a promise
    /// that one of its arms arrives inside the budget. Two of the ways a
    /// grant ends need a pass first, and in both the test steps and confirms
    /// after: a commit that waits on lane capacity needs the pass that
    /// drains the lane ahead of it, and a run's exit reaches the kernel's
    /// bookkeeping only at stage 1 of a pass. The token is detached exactly
    /// so it survives the step that makes the resolution reachable.
    ///
    /// # Panics
    ///
    /// Panics when `max_turns` is spent with the grant unresolved, reporting
    /// how many turns were consumed, and outside the running state.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "the call consumes the token by contract: a token that survived its resolution \
                  would name a grant the gate no longer holds (RFC 0008 §9.6)"
    )]
    pub fn confirm(&mut self, max_turns: usize, token: GrantToken) -> Confirmed {
        self.assert_running("confirm");
        self.assert_own_token(&token, "confirm");
        for turns in 0..=max_turns {
            if let Some(outcome) = self.kernel.gate().take_resolution(token.sequence) {
                return outcome;
            }
            // The second reclaiming fact, in the order the two are checked:
            // a terminal the gate already holds is the released send's own
            // and wins, and this arm is reached only where no send was
            // released — the gate refuses to clear a taken grant, so a
            // release still in flight keeps its own resolution.
            if !self.holds(&token.run) && self.kernel.gate().reclaim_untaken(token.sequence) {
                return Confirmed::Reclaimed;
            }
            assert!(
                turns < max_turns,
                "bounded `confirm` exhausted: the grant at {:?} was still unresolved after \
                 {turns} executor turns — a send waiting on lane capacity needs the `step_pass` \
                 that drains the lane, and a run's exit reaches the bookkeeping only at a pass's \
                 first stage, so step first and confirm after (RFC 0008 §9.6)",
                token.run.kind
            );
            self.turn();
        }
        unreachable!("the budget assertion above ends the loop");
    }

    /// Whether the grant `token` names has reached a terminal yet, without
    /// consuming the token, spending a turn, or clearing the grant.
    ///
    /// **Test-only internal, outside RFC 0008 §9.3's block**, on the same
    /// footing as [`on_worker_threads`](Self::on_worker_threads). It exists
    /// because the published surface has no non-destructive way to ask the
    /// question: [`confirm`](Self::confirm) consumes the token and fails on
    /// its budget, so a row that wants to assert "this release has *not*
    /// committed yet" and then go on driving has nothing to assert with —
    /// and a row that instead reads the acceptance ledger asserts nothing at
    /// all, since a grant that has not been taken leaves the ledger
    /// unchanged for reasons that have nothing to do with the lane. Whether
    /// the driving contract should carry a public form of it belongs to the
    /// bounded-lane work RFC 0014 §13.3 leaves open.
    ///
    /// It reports the terminal the *gate* holds. A grant that released no
    /// send has none, so this is `None` there — the second reclaiming fact
    /// is the caller's to establish and `confirm` is where it is reported.
    ///
    /// The receiver is `&self`, with the observation calls rather than the
    /// driving ones: this turns nothing, takes nothing, and clears nothing.
    /// The normative `&mut self` spelling belongs to RFC 0008 §9.3's block,
    /// which this is outside of, and claiming exclusivity it does not need
    /// would misreport what the call is.
    ///
    /// # Panics
    ///
    /// Panics outside the running state, and on a token this driver's gate
    /// no longer holds.
    #[cfg(test)]
    pub(crate) fn try_confirm(&self, token: &GrantToken) -> Option<Confirmed> {
        self.assert_running("try_confirm");
        self.assert_own_token(token, "try_confirm");
        self.kernel.gate().peek_resolution(token.sequence)
    }

    /// Drives the executor — beginning no pass and releasing no send-intent
    /// — until `until` holds (RFC 0008 §9.6).
    ///
    /// Some runs finish without ever presenting a send-intent: a cleanup
    /// finalizer whose `Output = ()` closes the message path outright, a
    /// future that completes with no message, a subscription run stopping
    /// after its last output. No grant releases them, and no pass guarantees
    /// them anything — a pass that never awaits yields no turns at all. That
    /// is the gap this call closes, with turns as its purpose, a stated
    /// budget, and a completion condition.
    ///
    /// **Both the budget and the completion condition are the caller's**,
    /// and both are elements of the script. `until` is evaluated once before
    /// the first turn — so a condition already true costs no turns — and
    /// again after each turn, for at most `max_turns` of them. What the
    /// predicate can see is what the test can see, which is ordinarily the
    /// test's own application-side instrumentation: a finalizer that sets a
    /// flag, a mock source that records its stop. It is deliberately not a
    /// run's exit as the driver knows it, which reaches the driver only at a
    /// pass's first stage.
    ///
    /// A **turn** is [`executor_turn`]'s construction: the driving task
    /// spawns a fresh no-op task onto this driver's executor and awaits it.
    /// A turn is a unit of opportunity, not of progress — it says nothing
    /// about how far any task runs, in what order tasks are picked, or
    /// whether one completes. Informatively, on the current-thread executor
    /// this driver owns, a FIFO ready queue means the tasks ready at the
    /// spawn do in practice run before the join resolves; that is an
    /// observation about that executor rather than a promise (RFC 0008
    /// §9.6, §9.8).
    ///
    /// This call initiates no append to the guaranteed sequence, and that
    /// holds structurally rather than by intent: it is misuse while a grant
    /// is outstanding, and no send is released except through one. The
    /// intent ledger may gain entries all the same, from any producer the
    /// turns advance to a send point — as it may during any driving call.
    ///
    /// # Panics
    ///
    /// Panics while a grant is outstanding, stranded or not; panics when
    /// `max_turns` is spent with `until` still false, reporting how many
    /// turns were consumed; and panics outside the running state.
    pub fn settle(&mut self, max_turns: usize, mut until: impl FnMut() -> bool) {
        self.assert_running("settle");
        assert!(
            !self.kernel.gate().grant_outstanding(),
            "`settle` is misuse while a grant is outstanding, stranded or not: a released send \
             still in flight is exactly what makes an append to the guaranteed sequence possible \
             during it (RFC 0008 §9.6)"
        );
        for turns in 0..=max_turns {
            if until() {
                return;
            }
            assert!(
                turns < max_turns,
                "bounded `settle` exhausted: its condition was still false after {turns} \
                 executor turns (RFC 0008 §9.6)"
            );
            self.turn();
        }
    }

    /// Sends admitted past the gate, in gate order. Admission, not delivery
    /// (RFC 0008 §9.6).
    ///
    /// Legal in every state, and empty before `boot`.
    ///
    /// # Panics
    ///
    /// Panics on a record naming a run the driver never observed starting,
    /// which would be the naming rule failing rather than a test error.
    #[must_use]
    pub fn accepted(&self) -> AcceptanceLedger {
        AcceptanceLedger {
            records: self.records(self.kernel.acceptances().snapshot()),
        }
    }

    /// Send-intents recorded before the gate, under no ordering or
    /// completeness guarantee (RFC 0008 §9.6).
    ///
    /// Legal in every state, and empty before `boot`.
    ///
    /// # Panics
    ///
    /// Panics on a record naming a run the driver never observed starting.
    #[must_use]
    pub fn intents(&self) -> IntentLedger {
        IntentLedger {
            records: self.records(self.kernel.intents().snapshot()),
        }
    }

    /// Whether the kernel's run bookkeeping still holds `run`.
    ///
    /// "Holds" is the bookkeeping's own reading and not merely map
    /// membership: an entry whose exit a pass has reflected is a tombstone
    /// kept for envelopes still in a lane, and the run it names is gone. A
    /// grant is misuse at such a run, and a grant outstanding at one has
    /// nothing left that could take its release.
    fn holds(&self, run: &RunName) -> bool {
        self.kernel
            .registry()
            .get(run.run)
            .is_some_and(|entry| !entry.exited)
    }

    /// The check that a name this driver was handed is a name it minted.
    ///
    /// A name from another driver denotes a run this kernel never started,
    /// so this is the same misuse the bookkeeping check states — "a run this
    /// kernel does not hold" — caught one step earlier, where the token it
    /// carries would otherwise collide with a local run's by number.
    fn assert_own_name(&self, run: &RunName, call: &str) {
        assert!(
            run.driver == self.id,
            "`{call}` names a run another driver minted: a name denotes a run of the kernel that \
             started it, and this one never did (RFC 0008 §9.4)"
        );
    }

    /// The same, for a grant token.
    fn assert_own_token(&self, token: &GrantToken, call: &str) {
        assert!(
            token.driver == self.id,
            "`{call}` names a grant another driver's gate issued: resolving it here would take a \
             terminal this driver never asked for (RFC 0008 §9.6)"
        );
    }

    /// The state-table check every driving call but `boot` shares.
    fn assert_running(&self, call: &str) {
        assert!(
            self.state == DriverState::Running,
            "`{call}` is misuse in the {} state: the driving calls are legal only while the \
             driver is running (RFC 0008 §9.3)",
            self.state.name()
        );
    }

    /// Names the runs a step started, then reports whether it terminated the
    /// program, settling the kernel where it did.
    fn finish_step(
        &mut self,
        outcome: Result<(), B::Error>,
        started: Vec<StartedRun>,
    ) -> StepReport<B::Error> {
        // `filter_map`, because a run the driver has no name for is one it
        // does not report: naming is per producer kind (RFC 0008 §9.4), and
        // a cleanup run is none of them. Filtering here rather than
        // asserting inside `mint` keeps a future routing mistake a missing
        // entry rather than a panic.
        let started = started
            .into_iter()
            .filter_map(|run| self.mint(run))
            .collect();
        let terminated = match outcome {
            Err(error) => Some(Err(error)),
            Ok(()) if self.kernel.terminating() => Some(Ok(Exit::Quit)),
            Ok(()) => None,
        };
        if terminated.is_some() {
            self.settle_kernel();
            self.state = DriverState::Terminated;
        } else {
            self.state = DriverState::Running;
        }
        StepReport {
            started,
            terminated,
        }
    }

    /// Mints the name for one run a step started, from the kind the kernel
    /// recorded at the start rather than from bookkeeping read back after —
    /// or `None` for a run there is no name for.
    ///
    /// Reading the kind back would be a lookup that can fail for a reason
    /// the test has no part in: a run whose exit a pass has already
    /// reflected has no entry left, and bootstrap can reach exactly that
    /// state for an init effect that finishes before its continuation pass.
    /// Nothing about the name is chosen here either way — the kind is still
    /// the kernel's, just taken at the moment it was true.
    ///
    /// A cleanup run is the `None`: it is none of the three kinds a
    /// [`RunName`] carries (RFC 0008 §9.4), it presents no send-intent to
    /// grant and makes no ledger record to tag, so there is nothing for a
    /// name to do. How a test observes one is its own instrumentation and a
    /// bounded [`settle`](Self::settle), which is what §9.6 says a run
    /// presenting no send-intent is observed by. Returning `None` rather
    /// than asserting keeps the mapping total: a kind this driver has no
    /// name for is one it does not report.
    fn mint(&mut self, started: StartedRun) -> Option<RunName> {
        let StartedRun { token, kind } = started;
        let kind = match kind {
            EntryKind::Keyed(id) => RunKind::Keyed(id),
            EntryKind::Sub(id) => RunKind::Subscription(id),
            EntryKind::Anon => RunKind::Anonymous,
            EntryKind::Cleanup => return None,
        };
        let name = RunName {
            driver: self.id,
            run: token,
            kind,
        };
        self.names.insert(token, name.clone());
        Some(name)
    }

    /// Resolves the kernel-side records of a ledger to the names a test
    /// scripts with.
    fn records(&self, sends: Vec<GatedSend>) -> Vec<SendRecord> {
        sends
            .into_iter()
            .map(|send| SendRecord {
                run: self
                    .names
                    .get(&send.origin)
                    .expect("a recorded send names a run the driver observed starting")
                    .clone(),
                lane: send.lane,
            })
            .collect()
    }

    /// The production quiescent postcondition, run where production runs it:
    /// after termination and before the result is reported (RFC 0011 §4.4,
    /// INV-LC7).
    ///
    /// Bounded in turns like every other wait here. Production bounds the
    /// same drain by quiescence, which it reaches because the immediate
    /// postcondition has already requested every abort.
    fn settle_kernel(&mut self) {
        let Self {
            executor, kernel, ..
        } = self;
        let settled = executor.block_on(async {
            let settle = kernel.settle();
            tokio::pin!(settle);
            for _ in 0..TURN_BUDGET {
                if let Poll::Ready(report) = futures::poll!(settle.as_mut()) {
                    return Some(report);
                }
                executor_turn().await;
            }
            None
        });
        assert!(
            settled.is_some(),
            "bounded settle exhausted after {TURN_BUDGET} executor turns: the terminated \
             kernel's join set did not drain"
        );
    }

    /// Hands the executor one turn (RFC 0008 §9.6's construction).
    ///
    /// A turn advances whatever is runnable rather than a run the caller has
    /// in mind: turns are not selective, which is a class fact about every
    /// driving call that supplies them (RFC 0008 §9.3).
    fn turn(&self) {
        self.executor.block_on(executor_turn());
    }
}

/// The park boundary's instrument (RFC 0008 §9.7).
///
/// No driver step reaches that boundary — a step *begins* a pass, and a
/// parked kernel is precisely one with no pass running and none beginning
/// until a source arrives, so the driver's pass-initiation seam replaces the
/// very mechanism the park contract constrains. The probe polls the
/// production driving future directly, with its own waker: the future comes
/// from the production entry point the test calls itself, never from a
/// `TestDriver`, which is what keeps the manual-effect-polling shape closed
/// (RFC 0008 §9.2).
///
/// What the probe supplies is a waker and a poll, and nothing else. It
/// scripts nothing inside the kernel, adds no branch, and is neither a third
/// runtime seam nor a second driver. **Its evidence scope is the park
/// contract's arming and wake claims and nothing else**: a probe observation
/// is never evidence for the same-topology claim, for scripted determinism,
/// for the pass stage order, or for production pass initiation.
///
/// Establishing a park takes two steps, and both are needed:
///
/// - *within the turn* — a re-poll returns `Pending`, the acceptance ledger
///   is unchanged, and the wake count is unchanged;
/// - *across executor turns* — the runtime is handed turns and the count
///   still does not move, which distinguishes a park that a single arrival
///   ends from a loop that re-arms itself every turn. The number of turns is
///   a test parameter, not a correctness condition.
#[derive(Debug, Default)]
pub struct ParkProbe {
    wakes: Arc<WakeCount>,
}

/// The probe's shared wake counter, which is also its waker.
#[derive(Debug, Default)]
struct WakeCount(AtomicUsize);

impl Wake for WakeCount {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

impl ParkProbe {
    /// A probe with a fresh waker and a zeroed count.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Polls the production driving future once with this probe's waker.
    ///
    /// The bound admits that future's output shape and no producer's: a
    /// keyed run, an anonymous run, and a subscription run all yield
    /// messages, and a cleanup run yields `()`, so a producer future is not
    /// of this shape — which is the second line that keeps manual effect
    /// polling unconstructible (RFC 0008 §9.2).
    pub fn poll<E, F>(&self, future: Pin<&mut F>) -> Poll<Result<Exit, E>>
    where
        F: Future<Output = Result<Exit, E>>,
    {
        let waker = Waker::from(Arc::clone(&self.wakes));
        future.poll(&mut Context::from_waker(&waker))
    }

    /// Wake-ups this probe's waker has received.
    #[must_use]
    pub fn wakes(&self) -> usize {
        self.wakes.0.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::ErrorKind;

    use crate::command::{Command, CommandId};
    use crate::kernel::conformance::support::{
        Beacon, Script, TEST_TURNS, cap, config, driver, driver_with, failing_driver,
        marking_effect, parking_effect, sending_effect, silent_effect,
    };
    use crate::kernel::lane::SendGate;
    use crate::subscription::mock::MockSource;

    // §9.3's state table, first row: `boot` is legal exactly once.
    #[test]
    #[should_panic(expected = "`boot` is misuse in the running state")]
    fn booting_twice_is_misuse() {
        let (mut driver, _journal) = driver(Script::new(Command::none()));

        driver.boot();
        drop(driver.boot());
    }

    // §9.3's state table, second and third rows read from the constructed
    // state: every driving call but `boot` is misuse before bootstrap.
    #[test]
    #[should_panic(expected = "`step_pass` is misuse in the constructed state")]
    fn stepping_before_boot_is_misuse() {
        let (mut driver, _journal) = driver(Script::new(Command::none()));

        drop(driver.step_pass(WakeSource::Data));
    }

    #[test]
    #[should_panic(expected = "`settle` is misuse in the constructed state")]
    fn settling_before_boot_is_misuse() {
        let (mut driver, _journal) = driver(Script::new(Command::none()));

        driver.settle(TEST_TURNS, || true);
    }

    // The last row: the observation calls are legal in every state, and
    // empty before bootstrap.
    #[test]
    fn the_observation_calls_are_legal_before_boot_and_empty() {
        let (driver, _journal) = driver(Script::new(silent_effect()));

        assert!(driver.accepted().is_empty(), "nothing has passed the gate");
        assert!(driver.intents().is_empty(), "no producer has been started");
    }

    // The terminated column: every driving call is misuse once a report has
    // carried a termination, and the observation calls stay callable.
    #[test]
    #[should_panic(expected = "`step_pass` is misuse in the terminated state")]
    fn driving_after_termination_is_misuse() {
        let (mut driver, _journal) = driver(Script::new(Command::quit()));

        assert!(driver.boot().terminated.is_some(), "the init quit applied");
        drop(driver.step_pass(WakeSource::Data));
    }

    #[test]
    fn the_observation_calls_outlive_termination() {
        let (mut driver, _journal) = driver(Script::new(Command::quit()));

        assert!(driver.boot().terminated.is_some(), "the init quit applied");

        assert!(driver.accepted().is_empty(), "no send passed the gate");
        assert!(driver.intents().is_empty(), "and none was attempted");
    }

    // §9.4: a run is named from an observation of its start, with its kind
    // and the identity the kernel holds readable off the name. The three
    // kinds are named by one rule.
    #[test]
    fn bootstrap_names_every_run_it_started_by_kind() {
        let keyed = CommandId::new("worker");
        let source = MockSource::new();
        let (mut driver, _journal) = driver(
            Script::new(Command::batch([
                silent_effect().cancellable(keyed.clone()),
                silent_effect(),
            ]))
            .declaring(vec![source]),
        );

        let report = driver.boot();

        let kinds: Vec<RunKind> = report.started.iter().map(RunName::kind).collect();
        assert!(
            matches!(
                kinds.as_slice(),
                [RunKind::Keyed(id), RunKind::Anonymous, RunKind::Subscription(_)] if *id == keyed
            ),
            "the init command's two runs in declaration order, then the reconcile's: {kinds:?}"
        );
    }

    // §9.4's per-run rule, at the one place naming by identity would differ:
    // a `CancelInFlight` successor holds the same `CommandId` as the run it
    // superseded, and the two names are still distinct.
    #[test]
    fn a_superseded_run_and_its_successor_are_named_apart() {
        let keyed = CommandId::new("worker");
        let (mut driver, _journal) = driver(
            Script::new(sending_effect([7]).cancellable(keyed.clone()))
                .replying([silent_effect().cancellable(keyed)]),
        );
        let first = driver.boot().started[0].clone();

        let token = driver.grant(first.clone()).expect("no other grant");
        assert_eq!(driver.confirm(TEST_TURNS, token), Confirmed::Accepted);
        let successor = driver
            .step_pass(WakeSource::Data)
            .expect("the granted send is in the lane")
            .started[0]
            .clone();

        assert_eq!(
            first.kind(),
            successor.kind(),
            "both runs hold the same logical identity"
        );
        assert_ne!(
            first, successor,
            "and the names still tell the superseded run from its successor"
        );
    }

    // §9.4's second rule, at the one place a name could outlive its run:
    // what crosses the grant boundary is the run the kernel holds, and a
    // name whose run the bookkeeping no longer holds is misuse rather than
    // a fresh start.
    #[test]
    #[should_panic(expected = "does not hold")]
    fn granting_at_a_run_the_bookkeeping_no_longer_holds_is_misuse() {
        let beacon = Beacon::default();
        let (mut driver, _journal) = driver(Script::new(marking_effect(beacon.clone())));
        let run = driver.boot().started[0].clone();

        driver.settle(TEST_TURNS, || beacon.marks() > 0);
        let reflected = driver
            .step_pass(WakeSource::ProducerExit)
            .expect("the settled run's exit is observable");
        assert!(reflected.terminated.is_none(), "an exit is not a quit");

        drop(driver.grant(run));
    }

    // §9.5: an init quit terminates during the init dispatch, so `boot`
    // returns a report whose termination is set and whose continuation pass
    // never ran (RFC 0014 §6.2, INV-RC11).
    #[test]
    fn an_init_quit_terminates_during_boot() {
        let (mut driver, _journal) = driver(Script::new(Command::batch([
            silent_effect(),
            Command::quit(),
        ])));

        let report = driver.boot();

        assert_eq!(
            report.terminated.map(|result| result.map_err(drop)),
            Some(Ok(Exit::Quit)),
            "the production result is a controlled quit"
        );
        assert_eq!(
            report.started.len(),
            1,
            "the sibling spawned before the quit is still a run this step started"
        );
    }

    // §9.5's first bound: readiness is read from the production sources, so
    // an unarrived source drives nothing. The fabricated-readiness model is
    // excluded here.
    #[test]
    fn a_step_at_an_unarrived_source_drives_nothing() {
        let (mut driver, journal) = driver(Script::new(silent_effect()));
        driver.boot();

        assert_eq!(
            driver.step_pass(WakeSource::Data).err(),
            Some(NotReady),
            "no producer has been granted a send, so the data lane is empty"
        );
        assert_eq!(
            driver.step_pass(WakeSource::Control).err(),
            Some(NotReady),
            "and no quit has arrived"
        );
        assert!(journal.reduced().is_empty(), "a refused step ran no update");
    }

    // §9.6, the whole handshake: the grant releases the named run's next
    // intent, `confirm` reports the commit, and the acceptance ledger
    // carries the run and the lane in gate order.
    #[test]
    fn a_grant_releases_a_send_and_confirm_reports_its_acceptance() {
        let (mut driver, journal) = driver(Script::new(sending_effect([7])));
        let run = driver.boot().started[0].clone();

        let token = driver.grant(run.clone()).expect("no other grant");
        assert_eq!(driver.confirm(TEST_TURNS, token), Confirmed::Accepted);

        let accepted = driver.accepted();
        assert_eq!(accepted.len(), 1, "one send passed the gate");
        let record = accepted.iter().next().expect("the one record");
        assert_eq!(record.run(), &run, "the record names the run that sent");
        assert_eq!(record.lane(), Lane::Data, "and the lane it took");
        assert!(
            journal.reduced().is_empty(),
            "admission is not delivery: no pass has dequeued it yet"
        );

        driver
            .step_pass(WakeSource::Data)
            .expect("the granted send is in the lane");
        assert_eq!(journal.reduced(), vec![7], "the pass delivered it");
    }

    // §9.6, the already-waiting arm: a producer the boot turn advanced to
    // its send point is waiting at the gate before any grant, and the grant
    // releases that intent rather than a later one.
    #[test]
    fn a_grant_releases_an_intent_already_waiting_at_the_gate() {
        let (mut driver, _journal) = driver(Script::new(sending_effect([7, 8])));
        let run = driver.boot().started[0].clone();

        assert_eq!(
            driver.intents().len(),
            1,
            "the producer reached its first send and is held at the gate"
        );
        assert!(
            driver.accepted().is_empty(),
            "no producer output reaches a lane before a grant releases it"
        );

        let token = driver.grant(run.clone()).expect("no other grant");
        assert_eq!(driver.confirm(TEST_TURNS, token), Confirmed::Accepted);

        let accepted = driver.accepted();
        assert_eq!(
            accepted.len(),
            1,
            "the intent that was waiting was released"
        );
        assert_eq!(
            accepted.iter().next().map(SendRecord::run),
            Some(&run),
            "and it was that run's"
        );
    }

    // §9.6's driver-wide rule: while a token is unconfirmed, `grant` is
    // refused whatever run it names, so raw grant order across two producers
    // is not expressible.
    #[test]
    fn a_second_grant_is_refused_at_every_run() {
        let (mut driver, _journal) = driver(Script::new(Command::batch([
            sending_effect([7]),
            sending_effect([9]),
        ])));
        let report = driver.boot();
        let (first, second) = (report.started[0].clone(), report.started[1].clone());

        let token = driver.grant(first.clone()).expect("the first grant");
        assert_eq!(
            driver.grant(first.clone()).err(),
            Some(GrantOutstanding),
            "a second grant at the same run is refused"
        );
        assert_eq!(
            driver.grant(second.clone()).err(),
            Some(GrantOutstanding),
            "and so is one at another run"
        );

        assert_eq!(driver.confirm(TEST_TURNS, token), Confirmed::Accepted);
        let next = driver
            .grant(second.clone())
            .expect("the resolved grant admits the next");
        assert_eq!(driver.confirm(TEST_TURNS, next), Confirmed::Accepted);

        let accepted = driver.accepted();
        let order: Vec<&RunName> = accepted.iter().map(SendRecord::run).collect();
        assert_eq!(
            order,
            vec![&first, &second],
            "the handshake is the only thing that ordered the two releases"
        );
    }

    // §9.6: `settle` is misuse while a grant is outstanding, stranded or
    // not — which is what makes an append to the guaranteed sequence
    // structurally impossible during a legal settle.
    #[test]
    #[should_panic(expected = "`settle` is misuse while a grant is outstanding")]
    fn settling_under_an_outstanding_grant_is_misuse() {
        let (mut driver, _journal) = driver(Script::new(sending_effect([7])));
        let run = driver.boot().started[0].clone();

        let _token = driver.grant(run).expect("no other grant");
        driver.settle(TEST_TURNS, || true);
    }

    // §9.6's settle: a run that sends nothing reaches its exit under the
    // turns this call contracts for, its completion condition is the
    // caller's own application-side instrumentation, and the exit becomes
    // visible at the exit-reflection stage of the next producer-exit step.
    #[test]
    fn settling_carries_a_silent_run_to_an_exit_a_later_step_reflects() {
        let beacon = Beacon::default();
        let (mut driver, _journal) = driver(Script::new(Command::batch([
            marking_effect(beacon.clone()),
            silent_effect(),
        ])));
        let report = driver.boot();
        assert_eq!(report.started.len(), 2, "both runs started");

        driver.settle(TEST_TURNS, || beacon.marks() > 0);
        let reflected = driver
            .step_pass(WakeSource::ProducerExit)
            .expect("the settled run's exit is observable");

        assert!(reflected.terminated.is_none(), "an exit is not a quit");
        assert!(
            driver.accepted().is_empty(),
            "a settle initiates no append to the guaranteed sequence"
        );
    }

    // The predicate is evaluated before the first turn, so a condition
    // already true costs no turns — which is what makes an exhausted budget
    // a statement about the script rather than about the bound.
    #[test]
    #[should_panic(expected = "bounded `settle` exhausted")]
    fn a_settle_whose_condition_never_holds_fails_the_test() {
        let (mut driver, _journal) = driver(Script::new(silent_effect()));
        driver.boot();

        driver.settle(TEST_TURNS, || false);
    }

    // The other end of the same rule, on the other waiting call: a
    // `confirm` whose grant does not resolve inside its budget fails the
    // test with the turns it spent, rather than waiting longer. The grant
    // here is armed at a run parked forever with nothing to send, so no
    // number of turns would resolve it — which is the shape a script error
    // takes.
    #[test]
    #[should_panic(expected = "bounded `confirm` exhausted")]
    fn a_confirm_whose_grant_never_resolves_fails_the_test() {
        let (mut driver, _journal) = driver(Script::new(silent_effect()));
        let run = driver.boot().started[0].clone();

        let token = driver.grant(run).expect("no other grant");
        let _confirmed = driver.confirm(TEST_TURNS, token);
    }

    // Zero is a budget like any other, and both calls agree on what it
    // means: the condition is evaluated before any turn is spent, so an
    // already-settled one costs none — which is what makes the budget a
    // statement about how many turns the *script* needs rather than a
    // constant the driver picked.
    #[test]
    fn a_zero_budget_admits_a_condition_that_already_holds() {
        let (mut driver, journal) = driver(Script::new(Command::batch([
            sending_effect([7]),
            sending_effect([9]),
        ])));
        let report = driver.boot();
        let (first, second) = (report.started[0].clone(), report.started[1].clone());

        driver.settle(0, || true);

        let token = driver.grant(first).expect("no other grant");
        assert_eq!(driver.confirm(TEST_TURNS, token), Confirmed::Accepted);

        // A grant can also resolve inside a `step_pass` — that call turns
        // the executor too — which leaves the confirm that reports it
        // needing no turn of its own.
        let banked = driver.grant(second).expect("the previous grant resolved");
        driver
            .step_pass(WakeSource::Data)
            .expect("the first release is in the lane");
        assert_eq!(
            driver.confirm(0, banked),
            Confirmed::Accepted,
            "already resolved, so the budget is never spent"
        );
        assert_eq!(
            journal.reduced(),
            vec![7, 9],
            "the step's own turn committed the second release, and its batch delivered both"
        );
    }

    // And zero refuses one that does not, on the same evaluation order.
    #[test]
    #[should_panic(expected = "still false after 0 executor turns")]
    fn a_zero_budget_refuses_a_condition_that_does_not() {
        let (mut driver, _journal) = driver(Script::new(silent_effect()));
        driver.boot();

        driver.settle(0, || false);
    }

    #[test]
    #[should_panic(expected = "unresolved after 0 executor turns")]
    fn a_zero_budget_confirm_refuses_an_unresolved_grant() {
        let (mut driver, _journal) = driver(Script::new(silent_effect()));
        let run = driver.boot().started[0].clone();

        let token = driver.grant(run).expect("no other grant");
        let _confirmed = driver.confirm(0, token);
    }

    // A grant the gate has already resolved is the other thing
    // `try_confirm` can report, and the one a probe that simply saw nothing
    // would fail to distinguish: two `None` assertions on their own are
    // satisfied by a peek that reads no state at all.
    //
    // The turn that resolves it comes from a `step_pass` whose source is a
    // *producer exit* — the marking run's end makes that source ready — so
    // the step is admitted and its leading turn hands the parked run its
    // release, which commits before the pass itself runs.
    #[test]
    fn try_confirm_reports_a_grant_the_gate_has_already_resolved() {
        let ended = Beacon::default();
        let (mut driver, journal) = driver(Script::new(Command::batch([
            marking_effect(ended.clone()),
            parking_effect([7]),
        ])));
        let parker = driver.boot().started[1].clone();

        driver.settle(TEST_TURNS, || ended.marked());
        let token = driver.grant(parker).expect("no other grant");
        driver
            .step_pass(WakeSource::ProducerExit)
            .expect("the marking run's exit is observable");

        assert_eq!(
            driver.try_confirm(&token),
            Some(Confirmed::Accepted),
            "the step's own turn released the send, and the gate holds its terminal"
        );
        assert_eq!(
            driver.confirm(TEST_TURNS, token),
            Confirmed::Accepted,
            "and the peek took neither the token nor the grant"
        );
        assert_eq!(
            journal.reduced(),
            vec![7],
            "the pass delivered the release its own turn let commit"
        );
    }

    // §9.5's first bound has an edge the journal cannot show: a refused step
    // drives *nothing*, not even a turn. An empty journal is equally what a
    // step that spent a turn and delivered nothing would leave; an
    // unresolved grant is not, because a turn is exactly what the release
    // was waiting for — as the confirm at the end shows by supplying one.
    #[test]
    fn a_refused_step_hands_the_executor_no_turn() {
        let (mut driver, _journal) = driver(Script::new(parking_effect([7])));
        let run = driver.boot().started[0].clone();

        let token = driver.grant(run).expect("no other grant");
        assert_eq!(
            driver.step_pass(WakeSource::Data).err(),
            Some(NotReady),
            "nothing has reached the data lane, so the step is refused"
        );
        assert_eq!(
            driver.try_confirm(&token),
            None,
            "and the refusal drove nothing: the release is still untaken"
        );

        assert_eq!(
            driver.confirm(TEST_TURNS, token),
            Confirmed::Accepted,
            "one turn is all it was waiting for"
        );
    }

    // A run can be gone by the time the step that started it is reported,
    // and the name still has to be minted. The state is: a token in the
    // kernel's `started` log whose registry entry a later pass has already
    // reflected and retired — reachable in production during bootstrap,
    // whose continuation pass can reflect an init effect that finished
    // while it was being set up.
    //
    // **White-box, and outside the evidence surface**: no driving call
    // leaves the log undrained, so reaching the state at all means driving
    // the kernel's stages directly. What it guards is one property and not
    // a contract — that the mint reads the kind the kernel recorded at the
    // start, and consults no bookkeeping that a pass is free to retire
    // first. Nothing observed here is evidence for INV-RC13 or for any
    // §13.1 series (RFC 0014 §7.2).
    #[test]
    fn whitebox_mint_names_a_run_whose_entry_a_pass_has_retired() {
        let (mut driver, _journal) =
            driver(Script::new(parking_effect([1])).replying([marking_effect(Beacon::default())]));
        let trigger = driver.boot().started[0].clone();
        let token = driver.grant(trigger).expect("no other grant");
        assert_eq!(driver.confirm(TEST_TURNS, token), Confirmed::Accepted);

        let started = {
            let TestDriver {
                executor,
                kernel,
                terminal,
                ..
            } = &mut driver;
            executor.block_on(async {
                // The pass that starts the run. Its `started` entry stays in
                // the log, because nothing here is a driving call.
                kernel
                    .pass_cycle(terminal)
                    .expect("the test backend renders");

                // Turns until the kernel itself can see the exit. The wait
                // ends on the fact it is waiting for — the kernel's own
                // readiness read — rather than on a count, so no scheduling
                // order is load-bearing.
                for _ in 0..TEST_TURNS {
                    if kernel.wake_source_ready(WakeSource::ProducerExit) {
                        break;
                    }
                    executor_turn().await;
                }
                assert!(
                    kernel.wake_source_ready(WakeSource::ProducerExit),
                    "the started run reached its end"
                );

                // The pass that reflects it. The run sent nothing, so the
                // entry is removable and this retires it.
                kernel
                    .pass_cycle(terminal)
                    .expect("the test backend renders");
                kernel.take_started()
            })
        };

        assert_eq!(started.len(), 1, "the log still holds the run it started");
        let run = started[0].token;
        assert!(
            driver.kernel.registry().get(run).is_none(),
            "and the bookkeeping no longer does: a mint that read it back would find nothing"
        );

        let name = driver
            .mint(started.into_iter().next().expect("the one entry"))
            .expect("an anonymous run is one of the three kinds a name carries");
        assert_eq!(
            name.kind(),
            RunKind::Anonymous,
            "the name carries the kind the kernel recorded when it started the run"
        );
    }

    // A name is one driver's, and two drivers' first runs are not the same
    // run. Run tokens count from one in every kernel, so without the
    // driver's own identity in the name these two would compare equal — and
    // a test holding both would have no way to tell which kernel it was
    // scripting.
    #[test]
    fn two_drivers_first_runs_are_named_apart() {
        let (mut left, _left_journal) = driver(Script::new(silent_effect()));
        let (mut right, _right_journal) = driver(Script::new(silent_effect()));

        let first = left.boot().started[0].clone();
        let second = right.boot().started[0].clone();

        assert_eq!(
            first.kind(),
            second.kind(),
            "the two runs are alike in everything the kernel holds for them"
        );
        assert_ne!(
            first, second,
            "and are still not the same run, because they are not the same kernel's"
        );
    }

    // The misuse that identity closes: granting at another driver's name.
    // It denotes a run this kernel never started, which is the bookkeeping
    // rule's own case — caught here before the token it carries can collide
    // with a local run's by number.
    #[test]
    #[should_panic(expected = "names a run another driver minted")]
    fn granting_at_another_driver_s_name_is_misuse() {
        let (mut left, _left_journal) = driver(Script::new(silent_effect()));
        let (mut right, _right_journal) = driver(Script::new(silent_effect()));
        left.boot();
        let foreign = right.boot().started[0].clone();

        drop(left.grant(foreign));
    }

    // And the same for a grant token, which sequences from one per gate:
    // confirming another driver's token here would take a terminal this
    // driver never asked for.
    #[test]
    #[should_panic(expected = "names a grant another driver's gate issued")]
    fn confirming_another_driver_s_token_is_misuse() {
        let (mut left, _left_journal) = driver(Script::new(sending_effect([7])));
        let (mut right, _right_journal) = driver(Script::new(sending_effect([9])));
        left.boot();
        let elsewhere = right.boot().started[0].clone();
        let foreign = right.grant(elsewhere).expect("no other grant");

        let _confirmed = left.confirm(TEST_TURNS, foreign);
    }

    // §9.6's first reclaiming fact: the send this grant released ended
    // without getting into the lane. A capacity-blocked send whose run is
    // then cancelled is that shape, and reaching it needs the step the
    // token's detachment exists to survive.
    #[test]
    fn a_released_send_that_never_commits_confirms_reclaimed() {
        let keyed = CommandId::new("worker");
        let beacon = Beacon::default();
        let (mut driver, journal) = driver_with(
            Script::new(Command::batch([
                sending_effect([1, 2]).cancellable(keyed.clone()),
                marking_effect(beacon.clone()),
            ]))
            .replying([Command::cancel(keyed)]),
            config().app_channel_capacity(cap(1)),
        );
        let worker = driver.boot().started[0].clone();
        assert!(beacon.marks() > 0, "the marking run ended on the boot turn");

        // The lane takes the worker's first message and is then full.
        let token = driver.grant(worker.clone()).expect("no other grant");
        assert_eq!(driver.confirm(TEST_TURNS, token), Confirmed::Accepted);
        assert_eq!(
            driver.intents().len(),
            2,
            "the worker reached its second send and waits at the gate"
        );

        // The next step's turn hands the worker its release, and the send
        // it takes then waits on the full lane; the same step's pass drains
        // that lane and cancels the worker, so the released send ends
        // without ever getting in.
        let blocked = driver.grant(worker).expect("the previous grant resolved");
        driver
            .step_pass(WakeSource::ProducerExit)
            .expect("the marking run's exit is observable");

        assert_eq!(journal.reduced(), vec![1], "the drained message ran update");
        assert_eq!(driver.confirm(TEST_TURNS, blocked), Confirmed::Reclaimed);
        assert_eq!(
            driver.accepted().len(),
            1,
            "a reclaimed grant appends nothing to the guaranteed sequence"
        );
    }

    // §9.6's second reclaiming fact: the granted run's exit is reflected in
    // the bookkeeping and this grant released no send at all, so nothing is
    // left that could arrive. The fact reaches the driver only at a pass's
    // first stage, which is why the step is interposed between the grant and
    // the confirm — and why the token is detached.
    #[test]
    fn a_grant_at_a_run_that_exits_without_sending_confirms_reclaimed() {
        let beacon = Beacon::default();
        let (mut driver, _journal) = driver(Script::new(Command::batch([
            marking_effect(beacon.clone()),
            silent_effect(),
        ])));
        let report = driver.boot();
        let (ended, still_running) = (report.started[0].clone(), report.started[1].clone());

        // The run reaches its end under the settle's turns; its exit is not
        // reflected yet, so the bookkeeping still holds it and the grant is
        // legal.
        driver.settle(TEST_TURNS, || beacon.marks() > 0);
        let token = driver
            .grant(ended)
            .expect("the bookkeeping still holds the run");

        let reflected = driver
            .step_pass(WakeSource::ProducerExit)
            .expect("the settled run's exit is observable");
        assert!(reflected.terminated.is_none(), "an exit is not a quit");

        assert_eq!(
            driver.confirm(TEST_TURNS, token),
            Confirmed::Reclaimed,
            "the grant will never put anything into the lane"
        );
        assert!(
            driver.accepted().is_empty(),
            "a reclaimed grant appends nothing to the guaranteed sequence"
        );

        // Both arms clear the outstanding grant, so the driver is usable
        // again: this is what keeps the reclaimed resolution from being a
        // dead end.
        assert!(
            driver.grant(still_running).is_ok(),
            "the cleared grant admits the next one"
        );
    }

    // §9.11's negative space, read from the driver's side: the terminated
    // report carries the backend's own error, and the render owner stays
    // `Program::view` and the terminal (RFC 0011 INV-LC5's `Err`).
    #[test]
    fn a_failing_render_terminates_the_bootstrap_with_the_backend_s_error() {
        let (mut driver, _journal) = failing_driver(Script::new(silent_effect()), 0);

        let report = driver.boot();

        let error = report
            .terminated
            .expect("the continuation pass's render failed")
            .expect_err("a render failure is the production result's error side");
        assert_eq!(error.kind(), ErrorKind::Other);
    }

    // The same classification from a steady-state pass: the bootstrap
    // render succeeds and the pass that renders next fails, terminating the
    // driver.
    #[test]
    fn a_failing_steady_state_render_terminates_the_step() {
        let (mut driver, journal) = failing_driver(Script::new(sending_effect([7])), 1);
        let run = driver.boot().started[0].clone();

        let token = driver.grant(run).expect("no other grant");
        assert_eq!(driver.confirm(TEST_TURNS, token), Confirmed::Accepted);
        let report = driver
            .step_pass(WakeSource::Data)
            .expect("the granted send is in the lane");

        assert!(
            report.terminated.is_some_and(|result| result.is_err()),
            "the pass that ran update rendered, and the render failed"
        );
        assert_eq!(journal.reduced(), vec![7], "the batch ran before the frame");
    }

    // The gate is one object for the whole kernel, which is what makes the
    // driver-wide rule expressible at all: a per-run gate could only ever
    // refuse per run.
    #[test]
    fn the_gate_is_one_object_for_every_run() {
        let (mut driver, _journal) = driver(Script::new(Command::batch([
            silent_effect(),
            silent_effect(),
        ])));
        let report = driver.boot();

        let gates: Vec<*const SendGate> = report
            .started
            .iter()
            .map(|name| {
                Arc::as_ptr(
                    &driver
                        .kernel
                        .registry()
                        .get(name.run)
                        .expect("a started run has an entry")
                        .gate,
                )
            })
            .collect();

        assert!(
            gates.windows(2).all(|pair| pair[0] == pair[1]),
            "every run's ingress holds a clone of the one gate"
        );
    }
}
