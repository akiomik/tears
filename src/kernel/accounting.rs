//! Per-run delivery accounting: the committed-pending counter and the RAII
//! reservation around each send.
//!
//! This is the mechanism behind RFC 0014 §3.1's "every queued item"
//! guarantee (§10 states it as premise, not contract): a run's registry
//! entry outlives its task until the entry's committed queue residue has
//! drained, so a revoked run's already-enqueued envelopes are still matched
//! to a live entry at the delivery decision point and filtered there rather
//! than delivered.
//!
//! The rules, in the order one send passes through them:
//!
//! 1. reserve before the send;
//! 2. commit on a successful send, which transfers the decrement duty to the
//!    dequeue side;
//! 3. release on a failed send or an uncommitted drop;
//! 4. decrement at dequeue — a revoked envelope's discard is a dequeue too;
//! 5. remove the entry only when `exited && !poisoned && pending == 0`;
//! 6. saturate-and-freeze at the ceiling.
//!
//! Rule 6 is why the counter is a *single* atomic: the poisoned state **is**
//! the saturation value, so saturating, freezing, and testing for frozen are
//! each one transition. A two-atomic encoding (`fetch_add`/`fetch_sub`
//! beside a separate `poisoned` flag) admits two interleavings that this one
//! excludes by construction — two racing reservations both passing the
//! poisoned pre-check and stepping past the ceiling, and a racing decrement
//! thawing a counter that was poisoned just after the decrement's check.
//! [`accounting_core`](super::accounting_core) models the transitions below
//! under `loom` to keep that closure checked rather than asserted.

use std::sync::Arc;
use std::sync::atomic::AtomicU32;

/// The saturation value doubles as the poisoned marker.
pub const POISONED: u32 = u32::MAX;

/// One run's committed-plus-reserved pending count.
///
/// Poisoning is a safe degradation, never a stale delivery: a poisoned
/// counter freezes, which flips its entry's removal condition to
/// "termination only" rather than letting the entry be reclaimed while
/// envelopes it owns are still in a lane.
#[derive(Debug, Default)]
pub struct PendingCounter {
    count: AtomicU32,
}

impl PendingCounter {
    /// Rule 1: reservation increment, saturating; reaching the ceiling
    /// poisons (rule 6).
    pub fn reserve(&self) {
        todo!("single-atomic saturating increment")
    }

    /// Rules 3 and 4: release and dequeue share one decrement; a poisoned
    /// counter is frozen and skips it.
    pub fn decrement(&self) {
        todo!("single-atomic decrement")
    }

    /// Current committed-plus-reserved pending.
    pub fn value(&self) -> u32 {
        todo!("counter read")
    }

    /// Whether the saturation rule froze this counter.
    pub fn is_poisoned(&self) -> bool {
        self.value() == POISONED
    }
}

/// RAII reservation: increments on construction, and on drop either releases
/// (rule 3) or, once committed, leaves the decrement to the dequeue side
/// (rules 2 and 4).
///
/// The type is what makes rule 3 total. Every way a send can fail to reach
/// the lane — an `Err` return, a cancellation at an await point inside the
/// send, a panic unwinding through it — ends in this drop.
pub struct PendingReservation {
    counter: Arc<PendingCounter>,
    committed: bool,
}

impl PendingReservation {
    /// Rule 1: reserve.
    pub fn new(_counter: Arc<PendingCounter>) -> Self {
        todo!("reservation")
    }

    /// Rule 2: the send succeeded and the envelope is in the lane, so the
    /// dequeue side owns the decrement from here.
    pub fn commit(self) {
        todo!("reservation commit")
    }
}

impl Drop for PendingReservation {
    fn drop(&mut self) {
        if !self.committed {
            self.counter.decrement();
        }
    }
}
