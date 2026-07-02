//! Minimal synchronous mirror of the query cell's concurrency primitives, used
//! for exhaustive `loom` interleaving checks.
//!
//! [`Cell`](super::cell::Cell) drives single-flight, generation
//! compare-and-commit, in-flight release, and post-error retry suppression, but
//! it is entangled with `tokio` watch channels and async fetchers, which `loom`
//! cannot model. `CellCore` re-expresses just those synchronization primitives
//! over plain atomics, isolated from any async plumbing, so `loom` can explore
//! every thread interleaving of:
//!
//! - single-flight: at most one fetcher acquires the in-flight slot per generation;
//! - compare-and-commit: a result commits only at the generation it was fetched at,
//!   and a stale completion is discarded while still releasing the in-flight slot;
//! - liveness: completion (success, error, or discard) always releases the slot,
//!   so a later generation can always acquire one;
//! - post-error retry suppression: a generation that failed does not refetch until
//!   the generation advances.
//!
//! End-to-end behavior on the real `Cell` (watch wiring, timing, reconcile
//! reasons) is covered separately by the `tokio` integration tests in
//! [`super::query`].

use loom::sync::atomic::{AtomicU64, Ordering};

#[allow(dead_code)]
const NONE: u64 = u64::MAX;

#[allow(dead_code)]
#[allow(clippy::struct_field_names)]
#[derive(Debug)]
pub(super) struct CellCore {
    current_generation: AtomicU64,
    in_flight_generation: AtomicU64,
    last_error_generation: AtomicU64,
}

#[allow(dead_code)]
impl CellCore {
    pub(super) fn new() -> Self {
        Self {
            current_generation: AtomicU64::new(0),
            in_flight_generation: AtomicU64::new(NONE),
            last_error_generation: AtomicU64::new(NONE),
        }
    }

    pub(super) fn begin_fetch(&self) -> Option<u64> {
        let current_generation = self.current_generation.load(Ordering::Acquire);
        if self.last_error_generation.load(Ordering::Acquire) == current_generation {
            return None;
        }

        self.in_flight_generation
            .compare_exchange(
                NONE,
                current_generation,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .ok()
            .map(|_| current_generation)
    }

    pub(super) fn invalidate(&self) -> u64 {
        self.current_generation.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub(super) fn complete_success(&self, generation: u64) -> bool {
        self.release_in_flight(generation);

        if self.current_generation.load(Ordering::Acquire) == generation {
            self.last_error_generation.store(NONE, Ordering::Release);
            true
        } else {
            false
        }
    }

    pub(super) fn complete_error(&self, generation: u64) {
        self.release_in_flight(generation);

        if self.current_generation.load(Ordering::Acquire) == generation {
            self.last_error_generation
                .store(generation, Ordering::Release);
        }
    }

    pub(super) fn needs_fetch_after_error(&self) -> bool {
        self.in_flight_generation.load(Ordering::Acquire) == NONE
            && self.last_error_generation.load(Ordering::Acquire)
                != self.current_generation.load(Ordering::Acquire)
    }

    pub(super) fn in_flight_generation(&self) -> Option<u64> {
        match self.in_flight_generation.load(Ordering::Acquire) {
            NONE => None,
            generation => Some(generation),
        }
    }

    fn release_in_flight(&self, generation: u64) {
        let _ = self.in_flight_generation.compare_exchange(
            generation,
            NONE,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

#[cfg(test)]
mod tests {
    use loom::sync::Arc;
    use loom::thread;

    use super::CellCore;

    #[test]
    fn single_flight_selects_at_most_one_fetcher_per_generation() {
        loom::model(|| {
            let cell = Arc::new(CellCore::new());

            let left = {
                let cell = cell.clone();
                thread::spawn(move || cell.begin_fetch().is_some())
            };
            let right = { thread::spawn(move || cell.begin_fetch().is_some()) };

            let selected = u8::from(left.join().expect("left fetcher should finish"))
                + u8::from(right.join().expect("right fetcher should finish"));

            assert_eq!(selected, 1, "exactly one fetcher should win single-flight");
        });
    }

    #[test]
    fn stale_generation_success_is_discarded_and_releases_in_flight() {
        loom::model(|| {
            let cell = CellCore::new();
            let generation = cell
                .begin_fetch()
                .expect("initial generation should acquire in-flight slot");

            cell.invalidate();

            assert!(
                !cell.complete_success(generation),
                "stale generation result must not commit"
            );
            assert_eq!(
                cell.in_flight_generation(),
                None,
                "stale completion must release in-flight slot"
            );
            assert!(
                cell.begin_fetch().is_some(),
                "new generation must be able to acquire a fresh in-flight slot"
            );
        });
    }

    #[test]
    fn error_completion_releases_in_flight_without_retrying_same_generation() {
        loom::model(|| {
            let cell = CellCore::new();
            let generation = cell
                .begin_fetch()
                .expect("initial generation should acquire in-flight slot");

            cell.complete_error(generation);

            assert_eq!(
                cell.in_flight_generation(),
                None,
                "error completion must release in-flight slot"
            );
            assert!(
                !cell.needs_fetch_after_error(),
                "same generation should not retry immediately after an error"
            );
        });
    }

    #[test]
    fn invalidate_after_error_allows_next_generation_to_fetch() {
        loom::model(|| {
            let cell = CellCore::new();
            let generation = cell
                .begin_fetch()
                .expect("initial generation should acquire in-flight slot");

            cell.complete_error(generation);
            cell.invalidate();

            // A new generation can only fetch if error completion released the
            // in-flight slot; otherwise retry suppression would appear to hold
            // for the wrong reason (a stuck slot rather than the error record).
            assert!(
                cell.begin_fetch().is_some(),
                "generation bump should clear retry suppression for the new generation"
            );
        });
    }
}
