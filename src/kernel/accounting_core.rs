//! Minimal synchronous mirror of the delivery-accounting protocol, used for
//! exhaustive `loom` interleaving checks — the concurrency check of the
//! multi-writer accounting that RFC 0014 §13.1 names.
//!
//! The real [`PendingCounter`](super::accounting::PendingCounter) is
//! entangled with the lanes and the ingress handle; this mirror re-expresses
//! exactly the counter's transition rules over `loom` atomics, isolated from
//! async plumbing, so `loom` can explore every thread interleaving of:
//!
//! - reservation/release/dequeue balance: concurrent releases and dequeue
//!   decrements never underflow, and the final count equals the
//!   committed-but-undrained envelopes;
//! - removal safety: with exit observation ordered after reservation
//!   resolution (the join happens-after the task future's drop), the removal
//!   condition "exited and pending == 0" never retires an entry that still
//!   owns an in-flight envelope;
//! - saturation and poison: crossing the ceiling under concurrent writers
//!   neither panics, wraps, nor thaws.
//!
//! The algorithm here must stay line-for-line equivalent to
//! `PendingCounter`; any divergence voids the verification. This module has
//! the same shape and the same feature gate the query cell's own loom
//! mirror uses.

use loom::sync::atomic::AtomicU32;

/// The saturation value doubles as the poisoned marker (same encoding as
/// [`PendingCounter`](super::accounting::PendingCounter)).
const POISONED: u32 = u32::MAX;

/// Mirror of `PendingCounter` (same transitions, loom atomics).
#[derive(Debug, Default)]
pub(super) struct CounterCore {
    count: AtomicU32,
}

impl CounterCore {
    /// Rule 1: reservation increment, saturating; reaching the ceiling
    /// poisons (rule 6).
    pub(super) fn reserve(&self) {
        todo!("mirror of PendingCounter::reserve")
    }

    /// Rules 3 and 4: release and dequeue share one decrement; a poisoned
    /// counter is frozen and skips it.
    pub(super) fn decrement(&self) {
        todo!("mirror of PendingCounter::decrement")
    }

    /// Current committed-plus-reserved pending.
    pub(super) fn value(&self) -> u32 {
        todo!("mirror of PendingCounter::value")
    }

    /// Whether the saturation rule froze this counter.
    pub(super) fn is_poisoned(&self) -> bool {
        self.value() == POISONED
    }
}
