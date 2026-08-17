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
//! The last two models are the two counterexamples' own models. A
//! two-atomic encoding (`fetch_add`/`fetch_sub` beside a separate
//! `poisoned` flag) fails them: two racing reservations both pass the
//! poisoned pre-check and the second steps past the ceiling, and a
//! decrement that passed the poisoned check before the poisoning landed
//! thaws a frozen counter to `POISONED - 1`. They are kept as models rather
//! than as prose so the closure stays checked against this encoding on
//! every run.
//!
//! The algorithm here must stay line-for-line equivalent to
//! `PendingCounter`; any divergence voids the verification. This module has
//! the same shape and the same feature gate the query cell's own loom
//! mirror uses.

use loom::sync::atomic::{AtomicU32, Ordering};

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
    pub(super) fn decrement(&self) {
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
    pub(super) fn value(&self) -> u32 {
        self.count.load(Ordering::SeqCst)
    }

    /// Whether the saturation rule froze this counter.
    pub(super) fn is_poisoned(&self) -> bool {
        self.value() == POISONED
    }

    /// Starts a model at a chosen pending count, because reaching the
    /// ceiling by reservation is not a thing a model can explore.
    pub(super) fn preset(&self, value: u32) {
        self.count.store(value, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    // Each model holds the guard for its duration, because a loom model
    // *is* a hook swapper: loom drives each model thread as a `generator`
    // coroutine, and generator takes the global hook, installs a no-op
    // around its own unwind, and reinstalls the previous one. That is the
    // swap the guard exists to serialize — the recording tests'
    // thread-name filter guards their counts, but not against a hook being
    // swapped under them (docs/testing.md "Process-Global Panic Hook
    // Tests").

    use loom::sync::Arc;
    use loom::sync::atomic::{AtomicU32, Ordering};
    use loom::thread;

    use super::CounterCore;
    use crate::test_support::hook_guard;

    /// A committed envelope's dequeue decrement racing an uncommitted
    /// reservation's release: no underflow in any interleaving, and the
    /// final count is exactly the committed-but-undrained envelopes (here
    /// zero).
    #[test]
    fn concurrent_release_and_dequeue_balance_without_underflow() {
        let _hook_guard = hook_guard();
        loom::model(|| {
            let counter = Arc::new(CounterCore::default());
            // One envelope already committed: reserve plus commit transfers
            // the decrement duty to the dequeue side.
            counter.reserve();

            let producer = {
                let counter = Arc::clone(&counter);
                // An in-flight send that fails: reserve, then release.
                thread::spawn(move || {
                    counter.reserve();
                    counter.decrement();
                })
            };
            let kernel = {
                let counter = Arc::clone(&counter);
                // The kernel dequeues the committed envelope.
                thread::spawn(move || counter.decrement())
            };
            producer.join().expect("producer thread");
            kernel.join().expect("kernel thread");
            assert_eq!(counter.value(), 0, "balanced after release and dequeue");
        });
    }

    /// Exit observation is ordered after reservation resolution (the join
    /// happens-after the task future's drop). Under that ordering the
    /// removal condition can never retire an entry whose committed envelope
    /// is still undrained: commit-after-removal is unreachable.
    #[test]
    fn removal_condition_never_strands_a_committed_envelope() {
        let _hook_guard = hook_guard();
        loom::model(|| {
            let counter = Arc::new(CounterCore::default());
            let queued = Arc::new(AtomicU32::new(0));

            let producer = {
                let counter = Arc::clone(&counter);
                let queued = Arc::clone(&queued);
                thread::spawn(move || {
                    // The first send commits: the envelope enters the lane.
                    counter.reserve();
                    queued.fetch_add(1, Ordering::SeqCst);
                    // The second fails: it releases before the task ends.
                    counter.reserve();
                    counter.decrement();
                })
            };
            let kernel = {
                let counter = Arc::clone(&counter);
                let queued = Arc::clone(&queued);
                thread::spawn(move || {
                    // A dequeue only happens for a committed envelope.
                    if queued.load(Ordering::SeqCst) > 0 {
                        counter.decrement();
                        queued.fetch_sub(1, Ordering::SeqCst);
                    }
                })
            };
            kernel.join().expect("kernel thread");
            // Exit observation happens-after every reservation of this run
            // was committed or released.
            producer.join().expect("producer thread");
            let removable = counter.value() == 0 && !counter.is_poisoned();
            let stranded = queued.load(Ordering::SeqCst);
            assert!(
                !removable || stranded == 0,
                "an entry must never be removable while a committed envelope is undrained"
            );
        });
    }

    /// The first rejected counterexample's model: two reservations racing
    /// across the ceiling. No panic, no wrap, and once saturated the
    /// counter is frozen for everyone.
    #[test]
    fn saturation_freeze_holds_under_concurrent_writers() {
        let _hook_guard = hook_guard();
        loom::model(|| {
            let counter = Arc::new(CounterCore::default());
            counter.preset(u32::MAX - 1);

            let first = {
                let counter = Arc::clone(&counter);
                thread::spawn(move || counter.reserve())
            };
            let second = {
                let counter = Arc::clone(&counter);
                thread::spawn(move || counter.reserve())
            };
            first.join().expect("first reserver");
            second.join().expect("second reserver");
            assert!(counter.is_poisoned(), "crossing saturation poisons");
            assert_eq!(counter.value(), u32::MAX, "frozen at saturation");
            // Frozen: a dequeue decrement after poisoning must not thaw.
            counter.decrement();
            assert_eq!(counter.value(), u32::MAX, "a poisoned counter is frozen");
        });
    }

    /// The second rejected counterexample's model: a decrement racing the
    /// poisoning reservation. The freeze must hold in every interleaving —
    /// once poison is observable, the count stays at saturation.
    #[test]
    fn decrement_racing_poisoning_cannot_thaw_the_counter() {
        let _hook_guard = hook_guard();
        loom::model(|| {
            let counter = Arc::new(CounterCore::default());
            counter.preset(u32::MAX - 1);

            let reserver = {
                let counter = Arc::clone(&counter);
                thread::spawn(move || counter.reserve())
            };
            let dequeuer = {
                let counter = Arc::clone(&counter);
                // A legitimate dequeue of one of the pre-existing committed
                // envelopes, racing the saturating reserve.
                thread::spawn(move || counter.decrement())
            };
            reserver.join().expect("reserver");
            dequeuer.join().expect("dequeuer");
            if counter.is_poisoned() {
                assert_eq!(
                    counter.value(),
                    u32::MAX,
                    "a poisoned counter must be frozen at saturation"
                );
            }
        });
    }
}
