//! The stage-3 driving surface.
//!
//! [`TestDriver`] drives the very same [`Kernel`] production does: the same
//! construction path, the same runtime-owned tasks on the same join set, the
//! same lanes, the same pass implementation, the same termination. The
//! driving differential is confined to **two seams** plus application-side
//! inputs (RFC 0014 §7.2, INV-RC13):
//!
//! 1. **pass initiation** — the driver names which armed source begins a
//!    pass, instead of a park choosing among the ready ones;
//! 2. **the send gate** — the driver releases producer sends one at a time,
//!    instead of every send being ready on its first poll.
//!
//! Readiness is not part of the differential and cannot be fabricated:
//! [`TestDriver::step_pass`] checks the named start against the real lanes
//! and the real join set, so an `Exit` start requires an actual task exit.
//!
//! # What has no constructor here
//!
//! Five shapes are absent by design, because each would make the driver a
//! second architecture rather than a script over the first: retaining a run
//! by hand; reimplementing reconciliation; a second quit route; polling an
//! effect directly; ingesting into the kernel without a producer.
//!
//! # Evidence surface
//!
//! Whole-pass driving is the evidence surface. The stage-granular probe is a
//! white-box view of one stage's mechanism — it bypasses the fixed stage
//! order, so it proves nothing about the order — and tests using it are
//! named with a `whitebox_` prefix and excluded from the same-topology
//! acceptance evidence.

// Scaffolding stage: the API shape is fixed, the bodies are not written
// yet, and the conformance series that will drive it do not exist.
#![allow(
    dead_code,
    unused_imports,
    reason = "driver scaffolding: the API shape lands before its bodies and before its callers"
)]
#![allow(
    clippy::needless_pass_by_ref_mut,
    reason = "driver scaffolding: `todo!()` bodies read nothing, so no `&mut` looks used yet"
)]

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::kernel::lane::{GrantOutstanding, Lane, RunToken};
use crate::kernel::{BootReport, ExitReport, Kernel};
use crate::reducer::Program;
use crate::runtime::config::RuntimeConfig;

pub use crate::kernel::arbiter::{PassStart, WakeSource};

/// The driver-facing form of a run token.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProducerId(RunToken);

impl ProducerId {
    /// Names the run the kernel knows by `token`.
    pub const fn new(token: RunToken) -> Self {
        Self(token)
    }

    /// The kernel-side identity this names.
    pub const fn token(self) -> RunToken {
        self.0
    }
}

/// Why a scripted step was refused.
#[derive(Debug)]
pub enum StepError {
    /// The named start has no work. Readiness is read from the real lanes
    /// and join set, so this cannot be scripted away.
    NotReady(PassStart),
    /// The kernel has terminated; only settling remains.
    Terminated,
    /// The pass's render failed (RFC 0011 INV-LC5's `Err` classification).
    Render(io::Error),
}

/// One recorded send, on either side of the gate.
///
/// A record carries the two facts a driving test sequences on: which run
/// produced the send, and which lane carried it — so a released quit is
/// distinguishable from a released message (RFC 0008 §9.6). The origin is
/// the kernel-side run identity rather than a driver-side name, because the
/// append happens on the producer's own send path, where that is the only
/// identity in hand.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SendRecord {
    /// The producing run.
    pub origin: RunToken,
    /// The lane the send took.
    pub lane: Lane,
}

/// An append-only observation ledger.
///
/// This is a driver-side observation surface, deliberately not part of the
/// `tears::runtime::load` schema: it records what a test asserts on, and
/// widening the observability vocabulary to carry it would make an
/// evidence-gathering detail into a public contract (the RFC 0012 INV-SE8
/// boundary).
#[derive(Clone, Debug, Default)]
struct Ledger(Arc<Mutex<Vec<SendRecord>>>);

impl Ledger {
    /// Appends one record.
    fn push(&self, record: SendRecord) {
        self.lock().push(record);
    }

    /// Snapshot of every record, in append order.
    fn snapshot(&self) -> Vec<SendRecord> {
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
    fn lock(&self) -> MutexGuard<'_, Vec<SendRecord>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Deliveries observed after the send gate: the sends the kernel accepted,
/// each carrying its origin and its lane, in gate order. This is the
/// guaranteed observation set, and it gains an entry only at an acceptance.
///
/// Cross-lane, the order is the driver's own — established by its sequence
/// of grants — and is nobody's claim about a production order (RFC 0014
/// §3.3 declines to order a run's control-lane quit against its earlier
/// data-lane output at all).
#[derive(Clone, Debug, Default)]
pub struct DeliveryLedger(Ledger);

/// Send *intents* observed before the send gate.
///
/// Outside the guaranteed set on purpose: an intent records that a producer
/// reached its send, which says nothing about whether the send was admitted,
/// committed, or filtered. Tests use it to sequence, never to conclude.
///
/// This is also the *only* record of an intent. The gate keeps no second
/// count beside it, so "how many intents has this origin reached" is a
/// question with one answer, derived from the records themselves
/// ([`count_for`](Self::count_for)) rather than agreed between two sources.
#[derive(Clone, Debug, Default)]
pub struct IntentLedger(Ledger);

impl DeliveryLedger {
    /// Appends the acceptance of one send. Called at the commit, which is
    /// the moment the envelope is in the lane.
    pub fn record(&self, origin: RunToken, lane: Lane) {
        self.0.push(SendRecord { origin, lane });
    }

    /// Snapshot of every recorded entry, in order.
    pub fn snapshot(&self) -> Vec<SendRecord> {
        self.0.snapshot()
    }

    /// How many of `origin`'s sends were accepted.
    pub fn count_for(&self, origin: RunToken) -> usize {
        self.0.count_for(origin)
    }
}

impl IntentLedger {
    /// Appends one send-intent. Called before the gate wait, so it records
    /// a producer that reached its send and nothing more.
    pub fn record(&self, origin: RunToken, lane: Lane) {
        self.0.push(SendRecord { origin, lane });
    }

    /// Snapshot of every recorded entry, in order.
    pub fn snapshot(&self) -> Vec<SendRecord> {
        self.0.snapshot()
    }

    /// How many send-intents `origin` has reached — the derived form of the
    /// count the gate deliberately does not keep.
    pub fn count_for(&self, origin: RunToken) -> usize {
        self.0.count_for(origin)
    }
}

/// One issued grant, correlated to the release it produced.
///
/// The token **borrows neither the driver nor the script**, and it is **not
/// a future**. Both are load-bearing. Detached, a test holds a grant
/// unresolved across `step_pass` calls, which is exactly what a bounded lane
/// at capacity requires — the send cannot commit until the kernel drains,
/// and the kernel cannot be driven while the token borrows it; that is
/// RFC 0014 §13.3's driver-progress condition. Not a future, because
/// awaiting is not how this layer waits: `confirm` drives a bounded number
/// of executor turns and fails on its bound rather than parking forever
/// (RFC 0008 §9.3).
///
/// Sequencing is enforced at issue time rather than by this type: at most
/// one grant is outstanding **driver-wide**, so a second — at this origin or
/// any other — is refused ([`GrantOutstanding`]) until this one resolves.
/// The token names its own grant, so confirming it can never be satisfied by
/// some other release's resolution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GrantToken {
    sequence: u64,
    origin: ProducerId,
}

impl GrantToken {
    /// Correlates a token to the grant the gate issued at `origin`.
    pub const fn new(sequence: u64, origin: ProducerId) -> Self {
        Self { sequence, origin }
    }

    /// The gate-side sequence this token confirms.
    pub const fn sequence(self) -> u64 {
        self.sequence
    }

    /// The origin the grant was armed at.
    pub const fn origin(self) -> ProducerId {
        self.origin
    }
}

/// How a grant resolved.
///
/// A released send has exactly two terminal states, and both clear the
/// outstanding grant so that `grant` and `settle` are legal again after
/// either (RFC 0008 §9.6):
///
/// - `Accepted` — the send committed and its envelope is in a lane. A
///   revoked run's send can still reach this state; revocation is a
///   delivery-side filter, not a send-side one (RFC 0014 §3.1, INV-RC5).
/// - `Reclaimed` — the released send's reservation was released without
///   committing, which is what a run revoked while its send awaited
///   capacity produces. Nothing is appended to the guaranteed sequence,
///   which is what strict revocation requires rather than a concession to
///   it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use = "a resolved grant reports which of the two terminals it reached"]
pub enum Confirmed {
    /// The granted send committed past the gate.
    Accepted,
    /// The granted send's reservation was released without committing.
    Reclaimed,
}

/// The same-topology scripted driver.
pub struct TestDriver<P: Program> {
    kernel: Kernel<P>,
    terminal: Terminal<TestBackend>,
}

impl<P: Program> TestDriver<P> {
    /// Inert construction through the production construction path, with the
    /// scripted gate installed.
    ///
    /// The terminal is a real `ratatui::Terminal`, so `Program::view` runs
    /// exactly as it does in production and there is no way for the driver
    /// to reach a frame without going through it.
    pub fn new(
        _program: P,
        _flags: P::Flags,
        _config: &RuntimeConfig,
        _terminal: Terminal<TestBackend>,
    ) -> Self {
        todo!("driver construction")
    }

    /// Drives the real bootstrap.
    pub fn boot(&mut self) -> BootReport {
        self.kernel.boot()
    }

    /// Runs one whole pass in the fixed stage order, through the same
    /// implementation the production loop runs.
    ///
    /// `start` names the wake source that begins the pass and must be ready.
    /// This is the evidence surface: pass initiation is exactly the
    /// arbitration RFC 0014 §3.5 leaves scriptable, and everything after it
    /// is the pinned pipeline.
    pub fn step_pass(&mut self, _start: PassStart) -> Result<(), StepError> {
        todo!("scripted pass initiation")
    }

    /// Arms a grant at `origin`, releasing that origin's next send-intent.
    ///
    /// Takes `&self` and returns a token that does not borrow the driver, so
    /// a test can hold a grant unresolved while stepping the kernel.
    /// Refuses while a grant is outstanding anywhere on the driver, which is
    /// what makes *grant, confirm, next grant* the only way two releases can
    /// be ordered — raw grant order across two producers is not expressible
    /// (RFC 0008 §9.6).
    pub fn grant(&self, _origin: ProducerId) -> Result<GrantToken, GrantOutstanding> {
        todo!("grant handshake")
    }

    /// Consumes `token`, driving the executor — beginning no pass — until
    /// the released send reaches one of its two terminals, and reports
    /// which.
    ///
    /// The wait is a bounded number of executor turns and fails the test on
    /// its bound rather than waiting longer. Where the acceptance needs the
    /// kernel to drain the lane the send waits on, neither terminal arrives
    /// under `confirm` and the test steps instead — which is what the
    /// detached token exists to permit.
    pub fn confirm(&mut self, _token: GrantToken) -> Confirmed {
        todo!("grant resolution")
    }

    /// Post-gate observations: the guaranteed set.
    pub fn deliveries(&self) -> DeliveryLedger {
        self.kernel.delivery()
    }

    /// Pre-gate observations: outside the guaranteed set.
    pub fn intents(&self) -> IntentLedger {
        self.kernel.intents()
    }

    /// The booted state.
    pub const fn state(&self) -> &P::State {
        self.kernel.state()
    }

    /// How many runtime-owned tasks the kernel still owns — the settle
    /// loop's input.
    pub fn owned_task_count(&self) -> usize {
        self.kernel.owned_task_count()
    }

    /// The shared two-stage termination: settle to quiescence.
    pub async fn settle(&mut self) -> ExitReport {
        self.kernel.settle().await
    }
}

/// A park witness.
///
/// The probe polls the production driving future with its own waker and
/// counts wakes. It adds no branch to the kernel: what it observes is the
/// same future production awaits, so a park it witnesses is the park
/// production performs.
///
/// Establishing a park takes two steps, and both are needed:
///
/// - *within the turn* — a re-poll returns `Pending`, the delivery ledger is
///   unchanged, and the wake count is unchanged;
/// - *across executor turns* — after yielding, the wake count moves by
///   exactly one, which distinguishes a park that a single arrival ends from
///   a spin that wakes itself repeatedly. The number of turns yielded is a
///   test parameter, not a correctness condition.
#[derive(Debug, Default)]
pub struct ParkProbe {
    wakes: Arc<WakeCount>,
}

/// The probe's shared wake counter.
#[derive(Debug, Default)]
struct WakeCount;

impl ParkProbe {
    /// A probe with a fresh waker and a zeroed count.
    pub fn new() -> Self {
        todo!("park probe construction")
    }

    /// Polls `future` once with the probe's waker.
    pub fn poll<F: Future>(&self, _future: Pin<&mut F>) -> Poll<F::Output> {
        todo!("park probe poll")
    }

    /// How many times the probe's waker has been woken.
    pub fn wakes(&self) -> usize {
        todo!("park probe wake count")
    }
}

#[cfg(test)]
mod tests {
    use super::{DeliveryLedger, IntentLedger, Lane, SendRecord};

    const ALICE: u64 = 1;
    const BOB: u64 = 2;

    fn record(origin: u64, lane: Lane) -> SendRecord {
        SendRecord { origin, lane }
    }

    // Each record carries the two facts a test sequences on, and the order
    // is the order of the appends.
    #[test]
    fn a_ledger_records_lane_and_origin_in_append_order() {
        let deliveries = DeliveryLedger::default();

        deliveries.record(ALICE, Lane::Data);
        deliveries.record(BOB, Lane::Data);
        deliveries.record(ALICE, Lane::Control);

        assert_eq!(
            deliveries.snapshot(),
            vec![
                record(ALICE, Lane::Data),
                record(BOB, Lane::Data),
                record(ALICE, Lane::Control),
            ],
            "records keep both facts and their order"
        );
    }

    // The per-origin count is derived from the records rather than agreed
    // between two sources: there is one record of a send-intent, and this is
    // a question asked of it.
    #[test]
    fn a_per_origin_count_is_derived_from_the_records() {
        let intents = IntentLedger::default();

        intents.record(ALICE, Lane::Data);
        intents.record(BOB, Lane::Data);
        intents.record(ALICE, Lane::Control);

        assert_eq!(intents.count_for(ALICE), 2, "two of this origin's intents");
        assert_eq!(intents.count_for(BOB), 1, "one of the other's");
        assert_eq!(
            intents.count_for(3),
            0,
            "and none for a run that has not sent"
        );
    }

    // A handle holds a clone of each ledger, so a clone must be the same
    // ledger rather than a copy of its contents.
    #[test]
    fn a_cloned_ledger_shares_its_records() {
        let intents = IntentLedger::default();
        let held_by_a_handle = intents.clone();

        held_by_a_handle.record(ALICE, Lane::Data);

        assert_eq!(
            intents.snapshot(),
            vec![record(ALICE, Lane::Data)],
            "a clone appends to the same ledger"
        );
    }

    // The division at the gate is a division between two ledgers: an intent
    // is not an acceptance.
    #[test]
    fn the_two_ledgers_are_separate() {
        let intents = IntentLedger::default();
        let deliveries = DeliveryLedger::default();

        intents.record(ALICE, Lane::Data);

        assert_eq!(intents.count_for(ALICE), 1, "the intent was recorded");
        assert!(
            deliveries.snapshot().is_empty(),
            "an intent appends nothing to the guaranteed sequence"
        );
    }
}
