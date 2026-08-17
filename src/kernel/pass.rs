//! The processing pass: four fixed stages in fixed order, plus the park and
//! wake arming that keeps their bounds from holding vacuously.
//!
//! RFC 0014 §3.5 pins the order (normative, not mechanism):
//!
//! 1. **Exit reflection** — producer exits the executor has completed are
//!    reflected into the run bookkeeping: exit-observed lifecycle facts,
//!    entry retirement where the delivery accounting permits it, and
//!    subscription dirt per §5.2's sources. This stage processes no input
//!    and applies no quit. Its position decides which quiescence facts the
//!    same pass's control drain, admissions, and frame stage observe.
//! 2. **Control-lane drain** — every quit that has arrived is drained
//!    *before* this pass's input batch, and applied if its origin is live,
//!    discarded if revoked.
//! 3. **At most one input batch**, always count-bounded.
//! 4. **Frame step** — render if a redraw is pending, then re-evaluation if
//!    subscriptions are dirty.
//!
//! The stages are not independently arbitrated branches: no sequence of
//! ready inputs can defer any of them. Two bounds follow and are contract —
//! a producer quit is applied before any input batch that begins after its
//! arrival (INV-RC9), and a redraw a batch marks is rendered before the next
//! input batch begins (INV-RC10).
//!
//! [`Kernel::pass_cycle`] is a single implementation shared verbatim by the
//! production loop and by the driver's pass-unit stepping, which is what
//! makes the driving differential the two seams and nothing else.

use std::num::NonZeroUsize;

use ratatui::Terminal;
use ratatui::backend::Backend;

use super::Kernel;
use super::arbiter::{PassStart, WakeSource};
use crate::reducer::Program;

/// The input-batch count cap used when `batch_max_messages` is unset.
///
/// What is contract is only that the cap is finite, so every batch ends
/// after a finite prefix of the ready input and the pass reaches its frame
/// stage (RFC 0014 §3.5's finite-prefix eventual progress). The wall-clock
/// batching window RFC 0006 INV-L6 defaulted to is superseded: the driving
/// loop reads no clock, and a time-windowed batch would make the pass bounds
/// wall-clock-relative. The value itself is mechanism.
pub const DEFAULT_BATCH_MAX_MESSAGES: NonZeroUsize = NonZeroUsize::new(1024).expect("non-zero");

impl<P: Program> Kernel<P> {
    /// One pass in the normative stage order.
    ///
    /// A render failure terminates the kernel and returns the backend's
    /// error, which the caller classifies (RFC 0011 INV-LC5's `Err`).
    pub fn pass_cycle<B: Backend>(&mut self, _terminal: &mut Terminal<B>) -> Result<(), B::Error> {
        todo!("fixed four-stage pass")
    }

    /// Stage 1: reflects every task exit currently observable.
    fn reflect_available_exits(&mut self) {
        todo!("exit reflection stage")
    }

    /// Stage 2: drains the control lane, applying each quit whose origin is
    /// still live and discarding the rest (RFC 0014 §3.3).
    fn control_drain(&mut self) {
        todo!("control drain stage")
    }

    /// Stage 3: one count-bounded input batch. Every dequeue passes the
    /// delivery decision first, so a revoked origin's envelope is discarded
    /// without reaching `reduce` (RFC 0014 INV-RC5).
    fn input_batch(&mut self) {
        todo!("input batch stage")
    }

    /// Stage 4: at most one render, then at most one re-evaluation, both on
    /// the pass's current state (RFC 0011 INV-LC1, INV-LC2).
    fn frame_step<B: Backend>(&mut self, _terminal: &mut Terminal<B>) -> Result<(), B::Error> {
        todo!("frame stage")
    }

    /// One subscription re-evaluation: the stop phase, then admissions under
    /// the uniform barrier (subscription runs only, RFC 0014 §5.1) and the
    /// stopping-pass defer rule — a pass that issued any stop admits nothing
    /// in that same pass (RFC 0014 §5.3).
    fn reconcile(&mut self) {
        todo!("subscription re-evaluation")
    }

    /// Whether the named start has work for a pass.
    ///
    /// Readiness is read from the real lanes and the real join set, so the
    /// driver cannot fabricate it — an `Exit` start in particular requires
    /// an actual task exit to be observable.
    pub fn pass_start_ready(&mut self, _start: PassStart) -> bool {
        todo!("pass readiness")
    }

    /// Whether any start has work, the production loop's park condition.
    pub fn pass_work_ready(&mut self) -> bool {
        [
            PassStart::Data,
            PassStart::Control,
            PassStart::Exit,
            PassStart::PendingFrame,
        ]
        .into_iter()
        .any(|start| self.pass_start_ready(start))
    }

    /// Parks until one of the three armed wake sources arrives (INV-RC16).
    ///
    /// The woken item is *buffered*, not consumed: readiness is then
    /// "buffer non-empty or the lane has queued items", so the one envelope
    /// the park received is processed by the pass it began rather than
    /// skipped by it.
    ///
    /// A `None` from either lane is unreachable by construction — the
    /// kernel holds a clone of both senders for its whole lifetime — so
    /// that branch asserts in debug and otherwise parks forever rather than
    /// spinning. Parking is what RFC 0014 §9 row 1 puts in place of RFC 0011
    /// INV-16's `Ready(None)` half ("a live kernel with no work parks"), and
    /// it keeps a future change in lane ownership from turning a degenerate
    /// state into a busy loop.
    pub async fn park(&mut self) -> WakeSource {
        todo!("park with three armed wake sources")
    }
}
