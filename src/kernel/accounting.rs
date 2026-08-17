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
use std::sync::atomic::{AtomicU32, Ordering};

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
    ///
    /// The compare-exchange loop is what makes the ceiling total under
    /// concurrent reservers: the increment is only published from a value
    /// that was still below the ceiling when the exchange succeeded, so no
    /// pair of reservations can step past it between one another's checks.
    pub fn reserve(&self) {
        let mut current = self.count.load(Ordering::SeqCst);
        loop {
            if current == POISONED {
                return;
            }
            match self.count.compare_exchange(
                current,
                current + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return,
                Err(observed) => current = observed,
            }
        }
    }

    /// Rules 3 and 4: release and dequeue share one decrement; a poisoned
    /// counter is frozen and skips it.
    ///
    /// The frozen check is inside the loop rather than before it, so a
    /// decrement that read a pre-poison value and lost its exchange to the
    /// poisoning reservation re-reads the ceiling and leaves it alone. That
    /// is the thaw the two-atomic encoding admitted.
    ///
    /// # Panics
    ///
    /// Panics on an underflow. A decrement with nothing reserved means a
    /// release and a dequeue both claimed the same envelope, which is the
    /// accounting failing rather than a condition to absorb.
    pub fn decrement(&self) {
        let mut current = self.count.load(Ordering::SeqCst);
        loop {
            if current == POISONED {
                return;
            }
            assert!(current > 0, "pending counter underflow");
            match self.count.compare_exchange(
                current,
                current - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return,
                Err(observed) => current = observed,
            }
        }
    }

    /// Current committed-plus-reserved pending.
    pub fn value(&self) -> u32 {
        self.count.load(Ordering::SeqCst)
    }

    /// Whether the saturation rule froze this counter.
    pub fn is_poisoned(&self) -> bool {
        self.value() == POISONED
    }

    /// Test-side state injection for the saturation rule: pretends `value`
    /// sends are already pending, because reaching the ceiling by
    /// reservation is not a thing a test can do. Not part of the protocol
    /// the models mirror.
    #[cfg(test)]
    pub fn preset(&self, value: u32) {
        self.count.store(value, Ordering::SeqCst);
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
    pub fn new(counter: Arc<PendingCounter>) -> Self {
        counter.reserve();
        Self {
            counter,
            committed: false,
        }
    }

    /// Rule 2: the send succeeded and the envelope is in the lane, so the
    /// dequeue side owns the decrement from here.
    pub fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for PendingReservation {
    fn drop(&mut self) {
        if !self.committed {
            self.counter.decrement();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{POISONED, PendingCounter, PendingReservation};
    use std::sync::Arc;

    fn counter() -> Arc<PendingCounter> {
        Arc::new(PendingCounter::default())
    }

    // Rule 1 and rules 3/4: the two directions balance.
    #[test]
    fn reservations_and_decrements_balance() {
        let counter = counter();
        assert_eq!(counter.value(), 0, "a fresh counter has nothing pending");

        counter.reserve();
        counter.reserve();
        assert_eq!(counter.value(), 2, "two reservations are two pending");

        counter.decrement();
        assert_eq!(counter.value(), 1, "a dequeue drops one");
        counter.decrement();
        assert_eq!(counter.value(), 0, "the last dequeue drains the run");
        assert!(!counter.is_poisoned(), "balanced traffic never poisons");
    }

    // Rule 3: an uncommitted reservation releases on drop, whatever ended
    // the send.
    #[test]
    fn an_uncommitted_reservation_releases_on_drop() {
        let counter = counter();
        {
            let _reservation = PendingReservation::new(Arc::clone(&counter));
            assert_eq!(counter.value(), 1, "the reservation is pending");
        }
        assert_eq!(counter.value(), 0, "an uncommitted drop releases");
    }

    // Rule 2: a committed reservation transfers the decrement duty rather
    // than performing it, so the count survives the producer's drop and is
    // owned by the dequeue side.
    #[test]
    fn a_committed_reservation_leaves_the_decrement_to_the_dequeue_side() {
        let counter = counter();
        PendingReservation::new(Arc::clone(&counter)).commit();
        assert_eq!(counter.value(), 1, "a committed envelope stays pending");

        counter.decrement();
        assert_eq!(counter.value(), 0, "the dequeue owns the decrement");
    }

    // Rule 6: reaching the ceiling *is* poisoning — there is no separate
    // flag to set, and no value above the ceiling to reach.
    #[test]
    fn reaching_the_ceiling_poisons_and_freezes() {
        let counter = counter();
        counter.preset(POISONED - 1);

        counter.reserve();
        assert!(counter.is_poisoned(), "crossing saturation poisons");
        assert_eq!(counter.value(), POISONED, "frozen at the ceiling");

        counter.reserve();
        assert_eq!(counter.value(), POISONED, "a poisoned counter never grows");
        counter.decrement();
        assert_eq!(counter.value(), POISONED, "a poisoned counter never thaws");
    }

    // Rule 6's consequence for rule 5, checked on the counter alone: a
    // poisoned counter never reads zero, which is what flips its entry's
    // removal condition to termination-only.
    #[test]
    fn a_poisoned_counter_never_reads_drained() {
        let counter = counter();
        counter.preset(POISONED);

        drop(PendingReservation::new(Arc::clone(&counter)));
        assert_eq!(
            counter.value(),
            POISONED,
            "a release against a frozen counter is a no-op"
        );
        assert_ne!(counter.value(), 0, "a poisoned entry is never removable");
    }

    // The underflow assert is part of the implementation rather than of the
    // tests that happen to look: a release and a dequeue claiming the same
    // envelope is the accounting failing.
    #[test]
    #[should_panic(expected = "pending counter underflow")]
    fn decrementing_an_empty_counter_is_a_defect() {
        counter().decrement();
    }
}
