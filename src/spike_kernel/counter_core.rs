//! Minimal synchronous mirror of the per-origin pending-counter protocol
//! (spec §2.1), used for exhaustive `loom` interleaving checks — the
//! multi-writer accounting verification RFC 0014 §13 item 1 names as an
//! acceptance condition.
//!
//! The real [`PendingCounter`](super::lane::PendingCounter) is entangled
//! with tokio lanes and the `IngressHandle`; this mirror re-expresses
//! exactly the counter's transition rules over `loom` atomics, isolated
//! from async plumbing, so `loom` can explore every thread interleaving
//! of:
//!
//! - reservation/release/dequeue balance: concurrent RAII releases and
//!   dequeue decrements never underflow, and the final count equals the
//!   committed-but-undrained envelopes;
//! - removal safety: with exit observation ordered after RAII resolution
//!   (the `JoinSet` join happens-after the task future's drop), the
//!   removal condition "exited && committed pending == 0" never removes
//!   an entry that still has a committed envelope in flight ("removal
//!   then commit" is unreachable);
//! - saturation/poison: crossing the saturation boundary under
//!   concurrent writers neither panics, wraps, nor thaws — once
//!   poisoned, the counter is frozen for every interleaving.
//!
//! The algorithm here must stay line-for-line equivalent to
//! `PendingCounter`; any divergence voids the verification.

use loom::sync::atomic::{AtomicU32, Ordering};

/// The saturation value doubles as the poisoned marker (same encoding as
/// `PendingCounter`).
const POISONED: u32 = u32::MAX;

/// Mirror of `PendingCounter` (same transitions, loom atomics).
///
/// History: the first cut mirrored the original two-atomic algorithm
/// (`fetch_add`/`fetch_sub` + a separate `poisoned` flag). Loom found two
/// counterexamples against it — `saturation_freeze_holds_under_concurrent_writers`
/// hit `reserve past saturation` (two racing reserves both pass the
/// poisoned pre-check and the second steps past `u32::MAX`), and
/// `decrement_racing_poisoning_cannot_thaw_the_counter` observed a frozen
/// counter thawed to `u32::MAX - 1` (a decrement that passed the poisoned
/// check before the poisoning landed after it). The single-atomic
/// encoding below closes both; the real `PendingCounter` was changed in
/// lockstep.
#[derive(Debug, Default)]
pub(super) struct CounterCore {
    count: AtomicU32,
}

impl CounterCore {
    /// Rule 1: reservation increment (saturating; reaching the ceiling
    /// poisons — rule 6).
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

    /// Rules 3 and 4: release and dequeue share the same decrement; a
    /// poisoned counter is frozen and skips it.
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

    pub(super) fn value(&self) -> u32 {
        self.count.load(Ordering::SeqCst)
    }

    pub(super) fn is_poisoned(&self) -> bool {
        self.value() == POISONED
    }

    pub(super) fn preset(&self, value: u32) {
        self.count.store(value, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering as StdOrdering};

    use loom::sync::Arc;
    use loom::thread;

    use super::CounterCore;

    /// Std-side interleaving counter: `loom::model` requires a `'static`
    /// closure and runs it once per explored execution, so a shared std
    /// atomic (untracked by loom) counts the explored interleavings.
    fn interleaving_counter() -> std::sync::Arc<AtomicUsize> {
        std::sync::Arc::new(AtomicUsize::new(0))
    }

    fn explored(counter: &AtomicUsize, what: &str) {
        eprintln!(
            "loom[{what}]: explored {} interleavings",
            counter.load(StdOrdering::SeqCst)
        );
    }

    /// A committed envelope's dequeue decrement racing an uncommitted
    /// reservation's RAII release: no underflow in any interleaving, and
    /// the final count is exactly the committed-but-undrained envelopes
    /// (here zero).
    #[test]
    fn concurrent_release_and_dequeue_balance_without_underflow() {
        let interleavings = interleaving_counter();
        let in_model = std::sync::Arc::clone(&interleavings);
        loom::model(move || {
            in_model.fetch_add(1, StdOrdering::SeqCst);
            let counter = Arc::new(CounterCore::default());
            // One envelope already committed (reserve + commit transfers
            // the decrement duty to the dequeue side).
            counter.reserve();

            let producer = {
                let counter = Arc::clone(&counter);
                // An in-flight send that fails: reserve, then RAII release.
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
            assert_eq!(counter.value(), 0, "balanced after release + dequeue");
        });
        explored(&interleavings, "release/dequeue balance");
    }

    /// Exit observation is ordered after RAII resolution (JoinSet join
    /// happens-after the task future's drop). Under that ordering, the
    /// removal condition can never remove an entry whose committed
    /// envelope is still undrained: commit-after-removal is unreachable.
    #[test]
    fn removal_condition_never_strands_a_committed_envelope() {
        let interleavings = interleaving_counter();
        let in_model = std::sync::Arc::clone(&interleavings);
        loom::model(move || {
            in_model.fetch_add(1, StdOrdering::SeqCst);
            let counter = Arc::new(CounterCore::default());
            let queued = Arc::new(loom::sync::atomic::AtomicU32::new(0));

            let producer = {
                let counter = Arc::clone(&counter);
                let queued = Arc::clone(&queued);
                thread::spawn(move || {
                    // Send #1 commits (envelope enters the lane).
                    counter.reserve();
                    queued.fetch_add(1, loom::sync::atomic::Ordering::SeqCst);
                    // Send #2 fails: RAII release before the task ends.
                    counter.reserve();
                    counter.decrement();
                })
            };
            let kernel = {
                let counter = Arc::clone(&counter);
                let queued = Arc::clone(&queued);
                thread::spawn(move || {
                    // A dequeue only happens for a committed envelope.
                    if queued.load(loom::sync::atomic::Ordering::SeqCst) > 0 {
                        counter.decrement();
                        queued.fetch_sub(1, loom::sync::atomic::Ordering::SeqCst);
                    }
                })
            };
            kernel.join().expect("kernel thread");
            // Exit observation: happens-after every producer reservation
            // was committed or released (thread join = JoinSet order).
            producer.join().expect("producer thread");
            let removable = counter.value() == 0 && !counter.is_poisoned();
            let stranded = queued.load(loom::sync::atomic::Ordering::SeqCst);
            assert!(
                !removable || stranded == 0,
                "an entry must never be removable while a committed envelope is undrained"
            );
        });
        explored(&interleavings, "removal safety");
    }

    /// Two reservations racing across the saturation boundary, with a
    /// concurrent dequeue decrement: no panic, no wrap, and once
    /// saturated the counter is frozen (poison observed by everyone).
    #[test]
    fn saturation_freeze_holds_under_concurrent_writers() {
        let interleavings = interleaving_counter();
        let in_model = std::sync::Arc::clone(&interleavings);
        loom::model(move || {
            in_model.fetch_add(1, StdOrdering::SeqCst);
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
            assert_eq!(counter.value(), u32::MAX, "poisoned counter is frozen");
        });
        explored(&interleavings, "saturation freeze");
    }

    /// A decrement racing the poisoning reservation: the freeze must hold
    /// in every interleaving — once poisoned is observable, the count
    /// stays at saturation.
    #[test]
    fn decrement_racing_poisoning_cannot_thaw_the_counter() {
        let interleavings = interleaving_counter();
        let in_model = std::sync::Arc::clone(&interleavings);
        loom::model(move || {
            in_model.fetch_add(1, StdOrdering::SeqCst);
            let counter = Arc::new(CounterCore::default());
            counter.preset(u32::MAX - 1);

            let reserver = {
                let counter = Arc::clone(&counter);
                thread::spawn(move || counter.reserve())
            };
            let dequeuer = {
                let counter = Arc::clone(&counter);
                // A legitimate dequeue of one of the pre-existing
                // committed envelopes, racing the saturating reserve.
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
        explored(&interleavings, "poison/decrement race");
    }
}
