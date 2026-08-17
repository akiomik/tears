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
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::kernel::lane::{GrantOutstanding, RunToken};
use crate::kernel::{BootReport, ExitReport, Kernel};
use crate::reducer::Program;
use crate::runtime::config::RuntimeConfig;

pub use crate::kernel::arbiter::{PassStart, WakeSource};

/// The driver-facing form of a run token.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProducerId(RunToken);

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

/// An append-only observation ledger.
///
/// This is a driver-side observation surface, deliberately not part of the
/// `tears::runtime::load` schema: it records what a test asserts on, and
/// widening the observability vocabulary to carry it would make an
/// evidence-gathering detail into a public contract (the RFC 0012 INV-SE8
/// boundary).
#[derive(Clone, Debug, Default)]
struct Ledger(Arc<Mutex<Vec<String>>>);

/// Deliveries observed after the send gate: what the kernel accepted, what
/// it filtered, and what reached `reduce`. This is the guaranteed
/// observation set.
#[derive(Clone, Debug, Default)]
pub struct DeliveryLedger(Ledger);

/// Send *intents* observed before the send gate.
///
/// Outside the guaranteed set on purpose: an intent records that a producer
/// reached its send, which says nothing about whether the send was admitted,
/// committed, or filtered. Tests use it to sequence, never to conclude.
#[derive(Clone, Debug, Default)]
pub struct IntentLedger(Ledger);

impl DeliveryLedger {
    /// Snapshot of every recorded entry, in order.
    pub fn snapshot(&self) -> Vec<String> {
        todo!("ledger snapshot")
    }
}

impl IntentLedger {
    /// Snapshot of every recorded entry, in order.
    pub fn snapshot(&self) -> Vec<String> {
        todo!("ledger snapshot")
    }
}

/// One issued grant's acknowledgement.
///
/// The handle **does not borrow the driver**. That is the load-bearing part
/// of its shape: a grant whose acknowledgement borrowed the driver could not
/// be held across a `step_pass`, which is exactly what a bounded lane at
/// capacity requires — the send cannot commit until the kernel drains, and
/// the kernel cannot be driven while the grant is held. Detaching the
/// acknowledgement is what satisfies the driver-progress condition.
///
/// Sequencing is enforced at issue time rather than by this type: a second
/// outstanding grant for one origin is refused
/// ([`GrantOutstanding`]), and the handle completes on its own grant's
/// commit — not on a snapshot another handle could also satisfy.
pub struct GrantHandle {
    acknowledged: Pin<Box<dyn Future<Output = ()> + Send>>,
}

impl Future for GrantHandle {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.acknowledged.as_mut().poll(cx)
    }
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

    /// Releases one of `origin`'s send intents.
    ///
    /// Takes `&self` and returns a handle that does not borrow the driver,
    /// so a test can hold the acknowledgement while stepping the kernel.
    /// Refuses while this origin's previous grant is unacknowledged, which
    /// caps per-origin outstanding grants at one and makes the next grant
    /// exist only after the previous acceptance does.
    pub fn grant(&self, _origin: ProducerId) -> Result<GrantHandle, GrantOutstanding> {
        todo!("grant handshake")
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
