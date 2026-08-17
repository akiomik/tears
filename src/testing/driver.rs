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
//! (RFC 0008 §9.3). Where the bound is the driver's own its value is
//! mechanism ([`TURN_BUDGET`]); [`TestDriver::settle`]'s is the caller's and
//! is part of the script. Application-supplied effects sit outside that
//! quantifier — an effect that sleeps times its own test.

// The driver's callers are the conformance series, which land after it: only
// this module's own tests exercise the surface today.
#![allow(
    dead_code,
    reason = "the stage-3 driver lands before the conformance series that drive it"
)]

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Wake, Waker};

use ratatui::Terminal;
use ratatui::backend::Backend;
use tokio::runtime::{Builder, Runtime as Executor};
use tokio::task::yield_now;

use crate::command::CommandId;
use crate::kernel::Kernel;
use crate::kernel::lane::{GateMode, RunToken};
use crate::kernel::registry::RunKind as EntryKind;
use crate::reducer::{Exit, Program};
use crate::runtime::config::RuntimeConfig;
use crate::runtime::load::LoadObserver;
use crate::subscription::SubscriptionId;

pub use crate::kernel::arbiter::WakeSource;
pub use crate::kernel::lane::{GrantOutstanding, Lane};

/// The bound on the waits this layer chooses for itself, in executor turns.
///
/// Its value is mechanism: a grant resolves or it does not, and any two
/// bounds agree on every execution where the resolution arrives — a longer
/// one is only more patient — so the bound here is a guard against a test
/// that hangs. What is contract is that every wait is finite, counted in
/// turns rather than in elapsed time, and fails the test on its bound
/// (RFC 0008 §9.3, §9.6). The one bound that is *not* the driver's is
/// [`TestDriver::settle`]'s, which is semantic and therefore the caller's.
const TURN_BUDGET: usize = 1024;

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

/// An append-only observation log shared with the producers that append to
/// it.
///
/// This is a driver-side observation surface, deliberately not part of the
/// `tears::runtime::load` schema: it records what a test asserts on, and
/// widening the observability vocabulary to carry it would make an
/// evidence-gathering detail into a public contract (the RFC 0012 INV-SE8
/// boundary).
#[derive(Clone, Debug, Default)]
struct Records(Arc<Mutex<Vec<GatedSend>>>);

impl Records {
    /// Appends one record.
    fn push(&self, record: GatedSend) {
        self.lock().push(record);
    }

    /// Snapshot of every record, in append order.
    fn snapshot(&self) -> Vec<GatedSend> {
        self.lock().clone()
    }

    /// How many records name `origin`.
    fn count_for(&self, origin: RunToken) -> usize {
        self.lock()
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
    fn lock(&self) -> MutexGuard<'_, Vec<GatedSend>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The recorder every run's ingress appends an **acceptance** to, at the
/// commit that puts its envelope in a lane.
///
/// Accepted is not delivered: a record says an item passed the gate and says
/// nothing about whether `update` ever saw it, which is why a revoked run's
/// committed send belongs in it (RFC 0008 §9.6).
#[derive(Clone, Debug, Default)]
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
#[derive(Clone, Debug, Default)]
pub struct IntentRecorder(Records);

impl AcceptanceRecorder {
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
    state: DriverState,
    /// Every run the driver has observed starting, by the identity the
    /// kernel holds. This is the whole of the driver's naming: minting adds
    /// an entry, ledger reads resolve through it, and a grant resolves a
    /// name back to the run the kernel holds here rather than at the kernel.
    names: HashMap<RunToken, RunName>,
}

#[expect(
    clippy::panic,
    reason = "assertion failures in a test harness are panics by design"
)]
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
    #[expect(
        clippy::needless_pass_by_value,
        reason = "RFC 0008 §9.3's constructor takes the production entry point's own inputs by \
                  value, config included"
    )]
    pub fn new(program: P, flags: P::Flags, config: RuntimeConfig, terminal: Terminal<B>) -> Self {
        let executor = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a current-thread executor for the driven kernel");
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
                yield_now().await;
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
                yield_now().await;
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
        assert!(
            self.kernel.registry().get(run.run).is_some(),
            "`grant` names a run the kernel's bookkeeping does not hold — a run never started, \
             or one whose exit a pass has already reflected (RFC 0008 §9.6): {:?}",
            run.kind
        );
        let sequence = self.kernel.gate().issue_grant(run.run)?;
        Ok(GrantToken { sequence, run })
    }

    /// Consumes `token`, driving the executor — beginning no pass — until
    /// the grant resolves, and reports how (RFC 0008 §9.6).
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
    /// Panics when the budget is exhausted with the grant unresolved, and
    /// outside the running state.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "the call consumes the token by contract: a token that survived its resolution \
                  would name a grant the gate no longer holds (RFC 0008 §9.6)"
    )]
    pub fn confirm(&mut self, token: GrantToken) -> Confirmed {
        self.assert_running("confirm");
        for _ in 0..TURN_BUDGET {
            if let Some(outcome) = self.kernel.gate().take_resolution(token.sequence) {
                return outcome;
            }
            self.turn();
        }
        panic!(
            "bounded `confirm` exhausted after {TURN_BUDGET} executor turns with the grant at \
             {:?} unresolved: a send waiting on lane capacity needs the `step_pass` that drains \
             the lane, and a run's exit reaches the bookkeeping only at a pass's first stage — \
             step first and confirm after (RFC 0008 §9.6)",
            token.run.kind
        );
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
    /// A **turn** is the driving task yielding control to the executor once,
    /// under this condition: every task ready at that yield gets the
    /// executor before the predicate is evaluated again. A turn is a unit of
    /// opportunity, not of progress — it says nothing about how far any task
    /// runs, in what order they are picked, or whether one completes. The
    /// current-thread executor this driver owns satisfies it through its
    /// FIFO ready queue, which is the scope the determinism claim is
    /// verified over (RFC 0008 §9.8).
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
    ///
    /// The naming happens before the settle, because settling clears the run
    /// bookkeeping the mint reads.
    fn finish_step(
        &mut self,
        outcome: Result<(), B::Error>,
        started: Vec<RunToken>,
    ) -> StepReport<B::Error> {
        let started = started.into_iter().map(|run| self.mint(run)).collect();
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

    /// Mints the name for one run the kernel has just started, reading its
    /// kind from the bookkeeping rather than choosing one.
    fn mint(&mut self, run: RunToken) -> RunName {
        let kind = match &self
            .kernel
            .registry()
            .get(run)
            .expect("a run the kernel has just started has an entry")
            .kind
        {
            EntryKind::Keyed(id) => RunKind::Keyed(id.clone()),
            EntryKind::Sub(id) => RunKind::Subscription(id.clone()),
            EntryKind::Anon => RunKind::Anonymous,
        };
        let name = RunName { run, kind };
        self.names.insert(run, name.clone());
        name
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
                yield_now().await;
            }
            None
        });
        assert!(
            settled.is_some(),
            "bounded settle exhausted after {TURN_BUDGET} executor turns: the terminated \
             kernel's join set did not drain"
        );
    }

    /// Hands the executor one turn: the driving task yields, and every task
    /// ready at that point runs before this returns.
    ///
    /// A turn advances whatever is runnable rather than a run the caller has
    /// in mind: turns are not selective, which is a class fact about every
    /// driving call that supplies them (RFC 0008 §9.3).
    fn turn(&self) {
        self.executor.block_on(yield_now());
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

    use std::collections::VecDeque;
    use std::io::ErrorKind;
    use std::num::{NonZeroU32, NonZeroUsize};

    use futures::{StreamExt, stream};
    use ratatui::Frame;
    use ratatui::backend::TestBackend;
    use tokio::sync::Notify;

    use crate::command::Command;
    use crate::kernel::lane::SendGate;
    use crate::reducer::Reducer;
    use crate::runtime::frame_rate::FrameRate;
    use crate::subscription::Subscription;
    use crate::subscription::mock::MockSource;
    use crate::test_support::FailingBackend;

    /// Application-side instrumentation: the only thing a `settle`
    /// predicate is meant to read (RFC 0008 §9.6).
    #[derive(Clone, Debug, Default)]
    struct Beacon(Arc<AtomicUsize>);

    impl Beacon {
        fn mark(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }

        fn marks(&self) -> usize {
            self.0.load(Ordering::SeqCst)
        }
    }

    /// What the scripted program is told to do, handed over at `init`.
    struct Script {
        /// The command `init` returns unchanged.
        init: Command<u8>,
        /// The commands `reduce` returns, in order; exhausted, it returns
        /// [`Command::none`].
        replies: VecDeque<Command<u8>>,
        /// The sources the state declares.
        sources: Vec<MockSource<u8>>,
    }

    impl Script {
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

    /// The application-side record of what reached `update`.
    ///
    /// The driver reports no state and no frame (RFC 0008 §9.11), so a test
    /// that needs to know what `update` saw records it here, where the
    /// application under test can.
    #[derive(Clone, Debug, Default)]
    struct Journal(Arc<Mutex<Vec<u8>>>);

    impl Journal {
        fn record(&self, message: u8) {
            self.0.lock().expect("journal lock").push(message);
        }

        fn reduced(&self) -> Vec<u8> {
            self.0.lock().expect("journal lock").clone()
        }
    }

    /// A program that replies from its script and records what it reduced.
    struct Scripted {
        journal: Journal,
    }

    impl Reducer for Scripted {
        type State = State;
        type Message = u8;

        fn reduce(&self, state: &mut State, message: u8) -> Command<u8> {
            self.journal.record(message);
            state.replies.pop_front().unwrap_or_else(Command::none)
        }

        fn subscriptions(&self, state: &State) -> Vec<Subscription<u8>> {
            state
                .sources
                .iter()
                .map(|source| Subscription::new(source.clone()))
                .collect()
        }
    }

    impl Program for Scripted {
        type Flags = Script;

        fn init(&self, flags: Script) -> (State, Command<u8>) {
            let Script {
                init,
                replies,
                sources,
            } = flags;
            (State { replies, sources }, init)
        }

        fn view(&self, _state: &State, _frame: &mut Frame<'_>) {}
    }

    fn config() -> RuntimeConfig {
        RuntimeConfig::new(
            FrameRate::new(NonZeroU32::new(60).expect("non-zero")).expect("a valid frame rate"),
        )
    }

    fn terminal() -> Terminal<TestBackend> {
        Terminal::new(TestBackend::new(8, 2)).expect("the test backend never fails")
    }

    /// A driver over the scripted program, plus the journal it records into.
    fn driver(script: Script) -> (TestDriver<Scripted, TestBackend>, Journal) {
        driver_with(script, config())
    }

    fn driver_with(
        script: Script,
        config: RuntimeConfig,
    ) -> (TestDriver<Scripted, TestBackend>, Journal) {
        let journal = Journal::default();
        let program = Scripted {
            journal: journal.clone(),
        };
        (
            TestDriver::new(program, script, config, terminal()),
            journal,
        )
    }

    /// An effect that never produces and never ends, so a run exists to name
    /// and no output arrives unbidden.
    fn silent_effect() -> Command<u8> {
        Command::stream(stream::pending())
    }

    /// An effect that sends each of `messages` and then ends.
    fn sending_effect<I>(messages: I) -> Command<u8>
    where
        I: IntoIterator<Item = u8>,
        I::IntoIter: Send + 'static,
    {
        Command::stream(stream::iter(messages))
    }

    /// An effect that marks `beacon` and then ends, sending nothing — the
    /// shape a cleanup finalizer has, and the one a `settle` predicate
    /// watches for.
    fn marking_effect(beacon: Beacon) -> Command<u8> {
        Command::stream(stream::unfold(Some(beacon), |state| async move {
            state?.mark();
            None::<(u8, Option<Beacon>)>
        }))
    }

    fn cap(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).expect("non-zero")
    }

    /// The turn budget these scripts hand `settle`.
    ///
    /// Every condition below is one a single turn establishes; the margin is
    /// there so a failure reads as "the script never reaches this" rather
    /// than "the budget was tight".
    const TEST_TURNS: usize = 8;

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
        assert_eq!(driver.confirm(token), Confirmed::Accepted);
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
        assert_eq!(driver.confirm(token), Confirmed::Accepted);

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
        assert_eq!(driver.confirm(token), Confirmed::Accepted);

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

        assert_eq!(driver.confirm(token), Confirmed::Accepted);
        let next = driver
            .grant(second.clone())
            .expect("the resolved grant admits the next");
        assert_eq!(driver.confirm(next), Confirmed::Accepted);

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
        assert_eq!(driver.confirm(token), Confirmed::Accepted);
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
        assert_eq!(driver.confirm(blocked), Confirmed::Reclaimed);
        assert_eq!(
            driver.accepted().len(),
            1,
            "a reclaimed grant appends nothing to the guaranteed sequence"
        );
    }

    // §9.11's negative space, read from the driver's side: the terminated
    // report carries the backend's own error, and the render owner stays
    // `Program::view` and the terminal (RFC 0011 INV-LC5's `Err`).
    #[test]
    fn a_failing_render_terminates_the_bootstrap_with_the_backend_s_error() {
        let program = Scripted {
            journal: Journal::default(),
        };
        let terminal = Terminal::new(FailingBackend::new(8, 2, 0)).expect("sizing never fails");
        let mut driver = TestDriver::new(program, Script::new(silent_effect()), config(), terminal);

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
        let journal = Journal::default();
        let program = Scripted {
            journal: journal.clone(),
        };
        let terminal = Terminal::new(FailingBackend::new(8, 2, 1)).expect("sizing never fails");
        let mut driver = TestDriver::new(
            program,
            Script::new(sending_effect([7])),
            config(),
            terminal,
        );
        let run = driver.boot().started[0].clone();

        let token = driver.grant(run).expect("no other grant");
        assert_eq!(driver.confirm(token), Confirmed::Accepted);
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

    /// A kernel over the scripted program, for the probe's series: the probe
    /// drives the production loop itself, so it takes a kernel rather than a
    /// driver.
    fn park_kernel(script: Script) -> (Kernel<Scripted>, Journal) {
        let journal = Journal::default();
        let program = Scripted {
            journal: journal.clone(),
        };
        let kernel = Kernel::new(
            program,
            script,
            &config(),
            GateMode::Immediate,
            LoadObserver::new(),
        );
        (kernel, journal)
    }

    /// The two-stage park witness (RFC 0008 §9.7), synchronous half: a
    /// further poll suspends again having run no pass — the acceptance
    /// ledger is unchanged — and no wake source is signalled, so by the
    /// `Future` contract the loop cannot resume until one does. Returns the
    /// wake count the park starts from.
    fn assert_parked<E, F>(
        probe: &ParkProbe,
        future: Pin<&mut F>,
        accepted: &AcceptanceRecorder,
        what: &str,
    ) -> usize
    where
        F: Future<Output = Result<Exit, E>>,
    {
        let before = probe.wakes();
        let ledger = accepted.snapshot();
        assert!(
            probe.poll(future).is_pending(),
            "the loop is suspended: {what}"
        );
        assert_eq!(
            accepted.snapshot(),
            ledger,
            "re-polling ran no pass, so the suspension is a park and not a gap between passes: \
             {what}"
        );
        assert_eq!(
            probe.wakes(),
            before,
            "no wake source is signalled while parked: {what}"
        );
        before
    }

    /// The same witness hardened with executor turns: every other runnable
    /// task, and any wake a self-re-arming loop deferred, gets to run — and
    /// the loop's waker must still be unsignalled afterwards. That is what
    /// separates a park from a loop that yields and re-arms itself every
    /// turn. The turn count is a test parameter, not a correctness
    /// condition.
    async fn assert_parked_across_turns<E, F>(
        probe: &ParkProbe,
        mut future: Pin<&mut F>,
        accepted: &AcceptanceRecorder,
        what: &str,
    ) -> usize
    where
        F: Future<Output = Result<Exit, E>>,
    {
        let before = assert_parked(probe, future.as_mut(), accepted, what);
        for _ in 0..8 {
            yield_now().await;
        }
        assert_eq!(
            probe.wakes(),
            before,
            "executor turns signalled no wake source, so the loop is parked on a waker rather \
             than re-arming itself: {what}"
        );
        assert_parked(probe, future, accepted, what)
    }

    /// Polls to completion, handing the executor a turn between polls so the
    /// runtime-owned tasks the kernel waits on can run. The bound converts a
    /// would-be hang into a failed assertion.
    #[expect(
        clippy::panic,
        reason = "an exhausted bound fails the test, which is what a panic is here"
    )]
    async fn drive_to_ready<E, F>(probe: &ParkProbe, mut future: Pin<&mut F>) -> Result<Exit, E>
    where
        F: Future<Output = Result<Exit, E>>,
    {
        for _ in 0..TURN_BUDGET {
            if let Poll::Ready(output) = probe.poll(future.as_mut()) {
                return output;
            }
            yield_now().await;
        }
        panic!("bounded drive exhausted: the loop did not finish in {TURN_BUDGET} turns");
    }

    /// Yields until `condition` holds, failing on the same turn bound every
    /// other wait here uses.
    #[expect(
        clippy::panic,
        reason = "an exhausted bound fails the test, which is what a panic is here"
    )]
    async fn settle_until(mut condition: impl FnMut() -> bool, what: &str) {
        for _ in 0..TURN_BUDGET {
            if condition() {
                return;
            }
            yield_now().await;
        }
        panic!("bounded settle exhausted: {what}");
    }

    // §9.7's whole claim, on the data-lane arm: the production loop parks
    // after the boot pass — established in both stages — and a single
    // arrival, and nothing else, signals its waker exactly once. The
    // counterexample this excludes is a loop that re-arms itself every turn,
    // which accumulates signals at the across-turns stage.
    #[tokio::test(flavor = "current_thread")]
    async fn the_production_loop_parks_until_one_arrival_wakes_it() {
        let latch = Arc::new(Notify::new());
        let held = Arc::clone(&latch);
        let (mut kernel, journal) = park_kernel(
            // The run parks forever after its one message, so the message
            // is the *only* arrival: a run that ended here would notify its
            // exit too, and the wake count could not tell the two apart.
            Script::new(Command::stream(
                stream::once(async move {
                    held.notified().await;
                    7_u8
                })
                .chain(stream::pending()),
            ))
            .replying([Command::quit()]),
        );
        let accepted = kernel.acceptances();
        let mut screen = terminal();
        let probe = ParkProbe::new();
        let mut run = Box::pin(async {
            kernel
                .run(&mut screen)
                .await
                .map(|_report| Exit::Quit)
                .map_err(|_error| ())
        });

        let parked_at =
            assert_parked_across_turns(&probe, run.as_mut(), &accepted, "after the boot pass")
                .await;
        assert_eq!(parked_at, 0, "nothing has woken the loop yet");

        // The one event: the effect's message reaches the data lane. The
        // gate is production's, so the acceptance is the arrival.
        latch.notify_waiters();
        settle_until(
            || !accepted.snapshot().is_empty(),
            "the message is accepted by the data lane",
        )
        .await;

        assert_eq!(
            probe.wakes(),
            parked_at + 1,
            "the data-lane arrival alone woke the parked loop, with exactly one signal"
        );
        assert_eq!(
            drive_to_ready(&probe, run.as_mut()).await,
            Ok(Exit::Quit),
            "the woken pass delivered the message, whose update quit"
        );
        assert_eq!(journal.reduced(), vec![7], "the woken pass delivered it");
    }

    // The producer-exit arm of the same claim: a run reaching its own end is
    // an arrival too, and it wakes the parked loop exactly once.
    #[tokio::test(flavor = "current_thread")]
    async fn a_producer_exit_wakes_the_parked_loop() {
        let latch = Arc::new(Notify::new());
        let held = Arc::clone(&latch);
        let beacon = Beacon::default();
        let marked = beacon.clone();
        let (mut kernel, _journal) = park_kernel(Script::new(Command::stream(stream::unfold(
            Some((held, marked)),
            |state| async move {
                let (latch, beacon) = state?;
                latch.notified().await;
                beacon.mark();
                None::<(u8, Option<(Arc<Notify>, Beacon)>)>
            },
        ))));
        let accepted = kernel.acceptances();
        let mut screen = terminal();
        let probe = ParkProbe::new();
        let mut run = Box::pin(async {
            kernel
                .run(&mut screen)
                .await
                .map(|_report| Exit::Quit)
                .map_err(|_error| ())
        });

        let parked_at =
            assert_parked_across_turns(&probe, run.as_mut(), &accepted, "after the boot pass")
                .await;

        // The one event: the run ends, having sent nothing at all.
        latch.notify_waiters();
        settle_until(|| beacon.marks() > 0, "the run reaches its end").await;

        assert_eq!(
            probe.wakes(),
            parked_at + 1,
            "the producer-exit notification alone woke the parked loop"
        );
        assert!(
            accepted.snapshot().is_empty(),
            "the run that woke it sent nothing"
        );
    }
}
