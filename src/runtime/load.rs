//! Load observability: the `tears::runtime::load` event schema (RFC 0006 §4.4).
//!
//! Three event kinds share the `tears::runtime::load` target. The batch event
//! and the capacity-wait event are stateless emissions from the batch loop and
//! the bounded send path; the producer gauges are backed by [`LoadObserver`], a
//! cheap cloneable handle over four shared counters that every producer updates,
//! so a single subscriber sees the aggregate. The schema (targets, levels,
//! fields, firing conditions) is pinned as RFC 0006 INV-L13; changing it is a
//! contract change.
//!
//! Every gauge event also carries the emitting instance's `runtime_id`, a
//! process-local `u64` allocated once per [`LoadObserver`] — that is, once per
//! runtime, since the runtime builds one observer and clones it to every
//! producer. It is an equality key for partitioning a multi-runtime process's
//! gauge events, with no meaning in its magnitude or ordering, and it is never
//! reused within the process lifetime ([`LoadObserver::new`]).
//!
//! The gauge counters live behind one mutex. Each change updates a field, bumps
//! a monotone `seq`, and snapshots the field set together under that lock, then
//! releases the lock before dispatching the snapshot to `tracing`. Two separate
//! guarantees ride on the lock, and only one of them constrains the order events
//! *arrive* in:
//!
//! - **Value fidelity** (every reached value gets its own event): capturing the
//!   update and the snapshot together under the lock prevents a value from being
//!   reached and superseded before its own snapshot re-reads the counters — a
//!   lone atomic would let a subscriber's high-water mark miss a peak. This
//!   guarantee is about the snapshot, not about arrival order.
//! - **Ordering** is carried by `seq`, not by arrival, and is per instance: the
//!   current value of each gauge is the value on the greatest-`seq` event
//!   *among events carrying that `runtime_id`* — a consumer partitions by
//!   `runtime_id` first, and comparing `seq` across instances is meaningless
//!   (RFC 0006 §4.4, INV-L13).
//!   Dispatch happens off the lock, so the contract does not promise arrival
//!   order matches `seq` order — consumers must order by `seq`. The single-
//!   drainer funnel below does in fact serialize dispatch in `seq` order today,
//!   but that is an implementation coincidence, not a guarantee — just as it was
//!   under the old under-lock dispatch — so no consumer may rely on it. Keeping
//!   the snapshot-and-`seq` capture under the lock while moving only the dispatch
//!   off it is what lets a slow subscriber no longer stall producers on the lock,
//!   and one that re-enters the runtime no longer deadlock on it, without
//!   breaking any `seq`-ordered consumer.
//!
//! Moving dispatch off the lock is only safe alongside a re-entrancy funnel. A
//! subscriber can, while handling a gauge event, cause another gauge change
//! (e.g. by spawning a subscription) — re-entering the emit path on the same
//! thread. Dispatched inline that would recurse without bound under a global
//! `tracing` dispatcher, or have the nested event silently dropped by a scoped
//! dispatcher's re-entrancy guard (breaking value fidelity). So dispatch is
//! funneled through a single drainer: the first producer to find no drainer
//! running claims the role and dispatches snapshots in a loop, while every other
//! producer — concurrent or re-entrant — only enqueues its snapshot (in `seq`
//! order) under the lock and returns, leaving the running drainer to deliver it.
//! Delivery is therefore iterative and never nested inside a `tracing` dispatch,
//! so neither hazard can arise (`LoadObserver::emit`, RFC 0006 §4.4).
//!
//! One consequence of the funnel: a snapshot is dispatched by whichever thread
//! is draining, which need not be the thread whose change produced it, so the
//! event fires under that drainer's thread and current span context. A global
//! subscriber sees only a schema-preserving change of span/thread attribution
//! (INV-L13 is unaffected); a thread-scoped dispatcher can see another thread's
//! gauge change delivered to it, or its own delivered on a different thread.
//!
//! When nothing is listening for `tears::runtime::load` at DEBUG, [`LoadObserver::emit`]
//! skips the `seq`/snapshot capture and the drain funnel — there is no
//! listener for either to serve — while still applying `mutate` under the
//! lock, so the counts stay correct for whenever a subscriber does attach.
//! Benchmarked in isolation (`benches/gauge.rs`), that fast path cuts an
//! unsubscribed gauge change from roughly the cost of two locks plus a
//! snapshot copy down to roughly one lock plus a `tracing::event_enabled!` check.
//!
//! `pub` items rather than `pub(crate)`: the enclosing `runtime` module is
//! already `pub(crate)`, so effective reachability is capped at the crate
//! (see `channel`/`frame_rate`), while `pub` avoids the redundant-`pub(crate)`
//! lint.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

static NEXT_RUNTIME_ID: AtomicU64 = AtomicU64::new(1);

/// The runtime channel a bounded send blocked on — the capacity-wait event's
/// `channel` field (`"shared"` or `"keyed"`).
#[derive(Clone, Copy)]
pub enum Channel {
    Shared,
    Keyed,
}

impl Channel {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::Keyed => "keyed",
        }
    }
}

/// Emits the batch event (RFC 0006 §4.4): `pulled` inputs taken this batch
/// (opening input included, INV-L12's counted unit), `updated` of them that
/// invoked `update`, and `shared_pending` shared-channel occupancy at batch
/// end. A quit-terminated batch does not call this — the loop exits instead.
///
/// A free function, not a [`LoadObserver`] method: the batch event carries no
/// gauge state.
pub fn batch(pulled: usize, updated: usize, shared_pending: usize) {
    tracing::trace!(
        target: "tears::runtime::load",
        pulled,
        updated,
        shared_pending,
        "processed message batch",
    );
}

/// Emits the capacity-wait event (RFC 0006 §4.4, bounded mode only): fired once
/// per send that had to await capacity, at acceptance, with the blocking
/// `channel` and the `wait_us` measured from the first unready attempt to
/// acceptance. Like [`batch`], it carries no gauge state.
pub fn capacity_wait(channel: Channel, waited: Duration) {
    tracing::debug!(
        target: "tears::runtime::load",
        channel = channel.as_str(),
        wait_us = u64::try_from(waited.as_micros()).unwrap_or(u64::MAX),
        "capacity wait",
    );
}

/// Shared handle over the runtime's producer-count gauges (RFC 0006 §4.4).
///
/// Cloning shares the same counters — and the same `runtime_id` — so every
/// producer holds a clone, updates its own field, and a single subscriber sees
/// the aggregate. All four counts are emitted together under
/// `tears::runtime::load` whenever any one changes.
#[derive(Clone)]
pub struct LoadObserver {
    gauges: Arc<Mutex<Gauges>>,
}

impl Default for LoadObserver {
    fn default() -> Self {
        Self::new()
    }
}

// Deliberately not `Copy`/`Clone`: `capture` bumps `seq` through `&mut self`, so
// a by-value copy (`let mut g = *guard; g.capture()`) would bump a throwaway
// while the shared `seq` stalled — the exact silently-dropped update this
// ordering exists to prevent. The `pending` queue makes `Copy` impossible on its
// own, but the rule is stated for `capture` regardless.
#[derive(Default)]
struct Gauges {
    /// This observer's process-local instance identifier, carried on every gauge
    /// event as `runtime_id` and fixed for the observer's lifetime. Held here
    /// rather than on [`LoadObserver`] so [`Gauges::capture`] builds the whole
    /// snapshot from the state it already has under the lock, and so every clone
    /// necessarily reports the same identifier (RFC 0006 §4.4, INV-L13).
    runtime_id: u64,
    /// Monotone per-observer counter, bumped once per captured gauge event and
    /// carried on it as `seq`. Captured under the same lock as the four counts,
    /// so a greater `seq` never carries an older value; a subscriber reads the
    /// current value of each gauge from the greatest-`seq` event *of this
    /// `runtime_id`*, so arrival order is not load-bearing (RFC 0006 §4.4,
    /// INV-L13).
    seq: u64,
    subscriptions: usize,
    unkeyed_commands: usize,
    keyed_commands: usize,
    blocked: usize,
    /// Snapshots captured but not yet dispatched, in `seq` order. Populated only
    /// while a drainer is already running — a concurrent producer or a
    /// re-entrant emit from within a subscriber; empty and unallocated on the
    /// common path (`LoadObserver::emit`).
    pending: VecDeque<GaugeSnapshot>,
    /// Whether some thread is currently draining `pending` and dispatching
    /// snapshots to `tracing` outside the lock. At most one drainer runs at a
    /// time, so dispatch is serialized in `seq` order without the lock being
    /// held across it (`LoadObserver::emit`).
    draining: bool,
}

impl Gauges {
    /// Bumps `seq` and captures the four counts plus that `seq` as one snapshot.
    /// Takes `&mut self` so the bump lands in the shared state; the caller holds
    /// the lock, fixing the counts and `seq` together at the serialization
    /// point, so the snapshot reports the state that was reached (never a later
    /// re-read) and its `seq` orders it against every other gauge event of this
    /// `runtime_id` without relying on arrival order. Dispatch to `tracing`
    /// happens later, off the lock (`GaugeSnapshot::dispatch`).
    const fn capture(&mut self) -> GaugeSnapshot {
        self.seq = self.seq.wrapping_add(1);
        GaugeSnapshot {
            runtime_id: self.runtime_id,
            seq: self.seq,
            subscriptions: self.subscriptions,
            unkeyed_commands: self.unkeyed_commands,
            keyed_commands: self.keyed_commands,
            blocked: self.blocked,
        }
    }
}

/// One producer-gauge event's payload — the four counts, the emitting
/// instance's `runtime_id`, and their ordering `seq` — captured under the lock
/// and dispatched to `tracing` after the lock is released (RFC 0006 §4.4).
#[derive(Clone, Copy)]
struct GaugeSnapshot {
    runtime_id: u64,
    seq: u64,
    subscriptions: usize,
    unkeyed_commands: usize,
    keyed_commands: usize,
    blocked: usize,
}

impl GaugeSnapshot {
    /// Emits the producer-gauge event for this snapshot. Called off the lock, so
    /// a slow or re-entrant subscriber cannot stall or deadlock a producer on
    /// the gauge lock.
    ///
    /// The `target`/level here must match [`LoadObserver::emit`]'s
    /// `tracing::event_enabled!` gate, or the gate goes stale — silently
    /// skipping (or failing to skip) dispatch for events it no longer
    /// describes.
    fn dispatch(self) {
        tracing::debug!(
            target: "tears::runtime::load",
            runtime_id = self.runtime_id,
            seq = self.seq,
            subscriptions = self.subscriptions,
            unkeyed_commands = self.unkeyed_commands,
            keyed_commands = self.keyed_commands,
            blocked = self.blocked,
            "producer gauges",
        );
    }
}

/// Which gauge a [`GaugeGuard`] owns.
#[derive(Clone, Copy)]
enum Field {
    Subscriptions,
    UnkeyedCommands,
    Blocked,
}

impl Field {
    const fn counter_mut(self, gauges: &mut Gauges) -> &mut usize {
        match self {
            Self::Subscriptions => &mut gauges.subscriptions,
            Self::UnkeyedCommands => &mut gauges.unkeyed_commands,
            Self::Blocked => &mut gauges.blocked,
        }
    }
}

impl LoadObserver {
    /// Creates an observer with fresh, all-zero gauges and a newly allocated
    /// `runtime_id` — the identifier every gauge event this observer emits
    /// carries (RFC 0006 §4.4, INV-L13). The runtime builds exactly one and
    /// clones it to its producers, so one observer is one runtime instance.
    ///
    /// # Panics
    ///
    /// Panics if the process-wide `runtime_id` space is exhausted (the
    /// allocator counter has reached `u64::MAX`): the identifier partitions a
    /// subscriber's gauge events by emitting instance, so ids are never reused
    /// within a process (RFC 0006 §4.4).
    #[must_use]
    pub fn new() -> Self {
        Self {
            // Reusing an id would merge two runtimes' gauge events into one
            // partition, and their independent `seq` streams with them. The
            // allocator fails before it can reuse a value: on exhaustion the
            // failed `fetch_update` stores nothing, leaving the counter
            // saturated at `u64::MAX`, so this and every later allocation
            // panics instead of wrapping into reuse.
            //
            // The remaining fields take `Gauges::default()`'s zero state. This
            // is the only site that builds a `Gauges`, so nothing can observe
            // the placeholder `runtime_id` that `Default` leaves behind.
            gauges: Arc::new(Mutex::new(Gauges {
                runtime_id: NEXT_RUNTIME_ID
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
                    .expect(
                        "runtime id space exhausted; gauge runtime ids are never \
                         reused within a process (RFC 0006 §4.4)",
                    ),
                ..Gauges::default()
            })),
        }
    }

    /// Tracks one active subscription forwarding task: the returned guard raises
    /// the `subscriptions` gauge now and lowers it when dropped (task end or
    /// abort).
    #[must_use]
    pub fn track_subscription(&self) -> GaugeGuard {
        self.enter(Field::Subscriptions)
    }

    /// Tracks one running unkeyed command task via the `unkeyed_commands` gauge.
    #[must_use]
    pub fn track_unkeyed_command(&self) -> GaugeGuard {
        self.enter(Field::UnkeyedCommands)
    }

    /// Tracks one producer currently awaiting bounded-channel capacity via the
    /// `blocked` gauge. Dropping the guard — on acceptance or on abort of the
    /// blocked send — lowers it, so the decrement never depends on the send
    /// completing (RFC 0006 §4.4).
    #[must_use]
    pub fn track_blocked(&self) -> GaugeGuard {
        self.enter(Field::Blocked)
    }

    /// Sets the `keyed_commands` gauge to the current active-entry count,
    /// emitting only when it changed. Keyed entries are counted directly (not
    /// via a guard) because an entry's lifetime is the runtime's, not a task's:
    /// a draining entry outlives its task.
    pub fn set_keyed_entries(&self, count: usize) {
        self.emit(|gauges| {
            if gauges.keyed_commands == count {
                return false;
            }
            gauges.keyed_commands = count;
            true
        });
    }

    fn enter(&self, field: Field) -> GaugeGuard {
        self.step(field, 1);
        GaugeGuard {
            observer: self.clone(),
            field,
        }
    }

    /// Adds `delta` (`+1`/`-1`) to `field` and dispatches the resulting
    /// snapshot. The update and its `seq`/snapshot capture happen under one lock
    /// so they cannot interleave with another producer's (RFC 0006 §4.4 "event
    /// per change"); the dispatch itself runs off the lock (`emit`).
    fn step(&self, field: Field, delta: isize) {
        self.emit(|gauges| {
            let counter = field.counter_mut(gauges);
            *counter = counter.wrapping_add_signed(delta);
            true
        });
    }

    /// Applies a gauge change and dispatches every resulting snapshot off the
    /// lock.
    ///
    /// `mutate` runs under the lock and returns whether it changed a gauge; on
    /// `false` nothing is emitted (e.g. `set_keyed_entries` with an unchanged
    /// count). On `true` the new `seq`/snapshot is captured under the same lock,
    /// then dispatched to `tracing` *after* the lock is released, so a slow
    /// subscriber never stalls producers on the lock and one that re-enters the
    /// runtime never deadlocks on it (RFC 0006 §4.4).
    ///
    /// Dispatch is funneled through a single drainer to stay safe under
    /// re-entrancy. A subscriber can, while handling a gauge event, cause
    /// another gauge change and re-enter here on the same thread; dispatched
    /// inline that would recurse without bound under a global `tracing`
    /// dispatcher, or have the nested event dropped by a scoped dispatcher's
    /// re-entrancy guard. Instead, the first caller to find no drainer running
    /// claims the role and dispatches in a loop, while every other caller —
    /// concurrent or re-entrant — only enqueues its snapshot in `seq` order and
    /// returns. Delivery is thus iterative and never nested inside a `tracing`
    /// dispatch.
    ///
    /// `mutate` always runs, whether or not anything is listening: the counts
    /// must stay correct so that whenever a subscriber does attach, the next
    /// snapshot reflects the true state rather than one that silently drifted
    /// while unobserved — in particular so a later decrement (e.g.
    /// `GaugeGuard::drop`, which cannot itself know whether the matching
    /// increment was captured) never sends a field negative. What the
    /// `tracing::event_enabled!` check below skips is only the capture/dispatch
    /// machinery — `seq`, `pending`, `draining` — which exists to serve a
    /// listener and costs a snapshot copy plus the drain funnel's second lock;
    /// with nothing listening that work has no observer to serve. Checking
    /// `enabled` *before* taking the lock, rather than under it, matters
    /// beyond the obvious "don't hold a lock longer than needed": a
    /// subscriber's `enabled()` is arbitrary external code (e.g. a reload
    /// layer re-evaluating an `EnvFilter`), and running that under the gauge
    /// mutex would reintroduce exactly the "external code under the lock"
    /// hazard the off-lock dispatch above exists to avoid — a slow `enabled()`
    /// would stall every producer, and one that itself touches this
    /// observer's gauges would deadlock.
    ///
    /// This uses `event_enabled!`, not the more general `enabled!`: the two
    /// build different `Metadata` to query with — `enabled!`'s reports as
    /// neither span nor event, while `event_enabled!` matches what
    /// [`GaugeSnapshot::dispatch`]'s `tracing::debug!` will actually query
    /// with. A subscriber that filters on `Metadata::is_event()` (a common
    /// and reasonable thing to do — `benches/runtime_load.rs`'s own
    /// `QuitDeliverySubscriber` does) sees `enabled!`'s query as neither, so
    /// its `enabled()` returns `false` unconditionally regardless of target
    /// or level, permanently silencing every gauge event even though a real
    /// `tears::runtime::load` DEBUG event fired moments later would have been
    /// accepted. Caught by that benchmark's CI run, not a unit test — every
    /// test subscriber in this module answers `enabled()` unconditionally
    /// `true`, so none of them distinguish the two.
    ///
    /// The `enabled` value is consulted only when this observer has no
    /// drainer already running (`!gauges.draining`), never for a reentrant or
    /// concurrent call arriving while one is. A reentrant call — a subscriber
    /// causing this gauge change while handling an earlier one — runs from
    /// inside that subscriber's `tracing` dispatch, where `event_enabled!`
    /// is unreliable regardless of where it is evaluated: `tracing`'s own
    /// re-entrancy guard shadows the real dispatcher for the duration (this
    /// shadowing is thread-local, not lock-scoped, so hoisting the check above
    /// the lock does not change it), so the check would report disabled even
    /// though a subscriber is verifiably attached and mid-dispatch right now.
    /// `gauges.draining` already being true is itself that proof, since it is
    /// only set once an earlier call found `enabled` true, so skipping the
    /// check and always capturing/enqueuing in that branch is both necessary
    /// (correctness) and sufficient (no re-check needed).
    fn emit(&self, mutate: impl FnOnce(&mut Gauges) -> bool) {
        // Must match `GaugeSnapshot::dispatch`'s target/level (see its doc
        // comment): this is what decides whether that event is worth building.
        // `event_enabled!`, not `enabled!` — see the doc comment above for why
        // that distinction is load-bearing here.
        let enabled =
            tracing::event_enabled!(target: "tears::runtime::load", tracing::Level::DEBUG);
        let first = {
            let mut gauges = self.lock();
            if !mutate(&mut gauges) {
                return;
            }
            if gauges.draining {
                let snapshot = gauges.capture();
                gauges.pending.push_back(snapshot);
                return;
            }
            if !enabled {
                return;
            }
            gauges.draining = true;
            gauges.capture()
        };

        // Release the drainer role if a subscriber panics mid-dispatch, so a
        // panic cannot wedge the funnel shut and silence every later gauge event
        // (the off-lock analogue of the poisoned-lock recovery in `lock`). The
        // guard stays armed until the loop relinquishes the role normally, so a
        // still-armed drop means and only means an unwinding `dispatch` — unlike
        // `thread::panicking()`, this stays correct when `emit` itself runs
        // during an unrelated unwind (`GaugeGuard::drop`).
        let mut release = DrainGuard {
            observer: self,
            armed: true,
        };
        let mut next = first;
        loop {
            next.dispatch();
            // Take the next snapshot, or relinquish the drainer role, under a
            // brief lock — never held across `dispatch`.
            let popped = {
                let mut gauges = self.lock();
                let snapshot = gauges.pending.pop_front();
                if snapshot.is_none() {
                    gauges.draining = false;
                }
                snapshot
            };
            let Some(snapshot) = popped else {
                release.armed = false;
                return;
            };
            next = snapshot;
        }
    }

    fn lock(&self) -> MutexGuard<'_, Gauges> {
        // Recover rather than propagate a poisoned lock. The gauges are plain
        // counters with no cross-field invariant, so a producer that panicked
        // mid-update leaves them merely off-by-one, not corrupt. Recovering
        // matters most in `GaugeGuard::drop`, which runs during unwinding: an
        // `expect` there would panic-during-unwind and abort the process (e.g.
        // a subscriber panicking under the lock poisons it, then every
        // unwinding producer's guard drop would double-panic).
        self.gauges.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// RAII guard that lowers its gauge on drop.
///
/// Held for the lifetime of the producer it tracks (a subscription or unkeyed
/// command task, or a blocked send), so completion and abort both lower the
/// gauge — the drop runs whether the tracked future finishes or is cancelled.
pub struct GaugeGuard {
    observer: LoadObserver,
    field: Field,
}

impl Drop for GaugeGuard {
    fn drop(&mut self) {
        self.observer.step(self.field, -1);
    }
}

/// Releases the gauge drainer role if a subscriber panics while
/// `LoadObserver::emit` is dispatching. The drain loop disarms this guard the
/// instant it relinquishes the role normally, so a drop while still armed means
/// `dispatch` unwound: it clears `draining` (and abandons any queued snapshots)
/// so later gauge changes can dispatch again instead of enqueuing forever behind
/// a drainer that will never return. Arming rather than reading
/// `thread::panicking()` is deliberate — `emit` also runs during unrelated
/// unwinds (`GaugeGuard::drop`), where a normal drain must not trigger recovery.
struct DrainGuard<'a> {
    observer: &'a LoadObserver,
    armed: bool,
}

impl Drop for DrainGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            let mut gauges = self.observer.lock();
            gauges.draining = false;
            gauges.pending.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;
    use std::panic::{self, AssertUnwindSafe};
    use std::sync::atomic::{AtomicBool, Ordering};

    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Level, Metadata, Subscriber};

    use super::*;
    use crate::test_support::{TraceRecorder, set_default_subscriber, with_silent_panic_hook};

    // INV-L13: every producer-gauge event carries the full field set together —
    // the four gauges plus the emitting instance's `runtime_id` and their
    // ordering `seq` — so a subscriber reads a complete, attributable, ordered
    // snapshot from any one event. The per-field recorder views flatten across
    // events and cannot see this; the field-set view can.
    #[test]
    fn gauge_event_carries_the_full_field_set() {
        let recorder = TraceRecorder::new().with_target("tears::runtime::load");
        let _guard = recorder.set_default();

        let observer = LoadObserver::default();
        let subscription = observer.track_subscription();
        observer.set_keyed_entries(2);
        drop(subscription);

        let gauge_events: Vec<_> = recorder
            .field_name_sets()
            .into_iter()
            .filter(|fields| fields.iter().any(|name| name == "subscriptions"))
            .collect();
        assert!(!gauge_events.is_empty(), "gauge events should have fired");
        for fields in gauge_events {
            for required in [
                "runtime_id",
                "seq",
                "subscriptions",
                "unkeyed_commands",
                "keyed_commands",
                "blocked",
            ] {
                assert!(
                    fields.iter().any(|name| name == required),
                    "a gauge event is missing `{required}`: {fields:?}"
                );
            }
        }
    }

    // INV-L13 (ordering): every gauge event carries a monotone `seq`, one per
    // emission, so a subscriber orders the events by `seq` rather than by
    // arrival — the current value of each gauge is the greatest-`seq` event's
    // among that `runtime_id`'s events.
    #[test]
    fn each_gauge_change_carries_a_monotone_seq() {
        let recorder = TraceRecorder::new().with_target("tears::runtime::load");
        let _guard = recorder.set_default();

        let observer = LoadObserver::default();
        let first = observer.track_subscription();
        let second = observer.track_subscription();
        drop(second);
        drop(first);

        assert_eq!(
            recorder.u64_values("seq"),
            vec![1, 2, 3, 4],
            "each of the four gauge changes emits one event with the next `seq`"
        );
    }

    // INV-L13 (instance identity): each runtime instance gets its own
    // `runtime_id` and carries it on every gauge event, so a subscriber
    // watching several runtimes in one process can attribute each event to its
    // emitter. One observer is one runtime — the runtime builds exactly one and
    // clones it to its producers, so a clone must report the same id rather
    // than looking like a second runtime.
    #[test]
    fn each_observer_gets_its_own_runtime_id() {
        let recorder = TraceRecorder::new().with_target("tears::runtime::load");
        let _guard = recorder.set_default();

        let first = LoadObserver::default();
        let second = LoadObserver::default();
        let first_clone = first.clone();
        drop(first.track_subscription());
        drop(second.track_subscription());
        drop(first_clone.track_subscription());

        let ids = recorder.u64_values("runtime_id");
        assert_eq!(
            ids.len(),
            6,
            "every gauge event carries a runtime_id: {ids:?}"
        );
        let (first_id, second_id) = (ids[0], ids[2]);
        assert_ne!(
            first_id, second_id,
            "two runtime instances must not share a runtime_id: {ids:?}"
        );
        assert_eq!(
            ids,
            vec![first_id, first_id, second_id, second_id, first_id, first_id],
            "each event carries its emitter's id, and a clone is the same \
             instance rather than a new one: {ids:?}"
        );
    }

    // INV-L13 (per-instance ordering): `seq` is strictly increasing among a
    // given `runtime_id`'s events and runs independently per instance, so a
    // subscriber partitions by `runtime_id` before applying the greatest-`seq`
    // rule. Emissions are interleaved here, so neither arrival order nor a
    // cross-instance `seq` comparison could stand in for that.
    #[test]
    fn gauge_seq_strictly_increases_within_each_runtime_id() {
        let recorder = TraceRecorder::new().with_target("tears::runtime::load");
        let _guard = recorder.set_default();

        let first = LoadObserver::default();
        let second = LoadObserver::default();
        let held = first.track_subscription();
        second.set_keyed_entries(1);
        let also_held = first.track_subscription();
        second.set_keyed_entries(2);
        drop(held);
        drop(also_held);

        // One `runtime_id` and one `seq` per gauge event, each log in arrival
        // order, so the two line up index by index — the other two event kinds
        // carry neither field, so nothing else can enter either log.
        let ids = recorder.u64_values("runtime_id");
        let seqs = recorder.u64_values("seq");
        assert_eq!(ids.len(), 6, "six gauge changes fired: {ids:?}");
        assert_eq!(
            seqs.len(),
            ids.len(),
            "every gauge event carries both fields: {ids:?} / {seqs:?}"
        );

        let mut partitions: Vec<(u64, Vec<u64>)> = Vec::new();
        for (id, seq) in ids.iter().zip(&seqs) {
            match partitions.iter_mut().find(|(known, _)| known == id) {
                Some((_, known_seqs)) => known_seqs.push(*seq),
                None => partitions.push((*id, vec![*seq])),
            }
        }
        assert_eq!(
            partitions.len(),
            2,
            "two runtime instances must yield two partitions: {ids:?}"
        );
        for (id, seqs) in partitions {
            assert!(
                seqs.windows(2).all(|pair| pair[0] < pair[1]),
                "runtime {id}'s seq must strictly increase across its own \
                 events: {seqs:?}"
            );
        }
    }

    // Fast-path correctness: while nothing is listening for
    // `tears::runtime::load`, `LoadObserver::emit` skips the capture/dispatch
    // machinery but must still apply `mutate`, so the counts track true state
    // rather than drifting. Changes made and reversed entirely while
    // unsubscribed emit nothing (asserted first); once a subscriber attaches,
    // the next event must report the accurate current value, not one that
    // silently rotted while unobserved (which would show up as a `usize`
    // wraparound from an unmatched decrement, RFC 0006 §4.4).
    #[test]
    fn gauge_changes_made_while_unsubscribed_are_not_lost() {
        let observer = LoadObserver::default();

        // No recorder installed: nothing is listening for
        // `tears::runtime::load`, so the fast path applies.
        let first = observer.track_subscription();
        let second = observer.track_subscription();
        drop(first);
        let third = observer.track_subscription();

        let recorder = TraceRecorder::new().with_target("tears::runtime::load");
        let _guard = recorder.set_default();
        let fourth = observer.track_subscription();

        assert_eq!(
            recorder.u64_values("subscriptions"),
            vec![3],
            "the first event after a subscriber attaches must report the true \
             current count (second, third, fourth still held), not a count \
             that missed the unobserved changes"
        );

        drop(second);
        drop(third);
        drop(fourth);

        assert_eq!(
            recorder.u64_values("subscriptions"),
            vec![3, 2, 1, 0],
            "every value reached while subscribed is still emitted, and the \
             count never wraps from an unmatched decrement"
        );
    }

    // The fast-path gate must use `tracing::event_enabled!`, not the more
    // general `enabled!`: they build different `Metadata` to query with, and
    // `enabled!`'s reports as neither span nor event. A subscriber that
    // filters on `Metadata::is_event()` — a common, reasonable thing to do,
    // and exactly what `benches/runtime_load.rs`'s `QuitDeliverySubscriber`
    // does — would see `enabled!`'s query as neither and answer `enabled()`
    // `false` unconditionally, permanently silencing every gauge event even
    // though the real DEBUG event that follows would have been accepted.
    // Every other subscriber in this module answers `enabled()`
    // unconditionally `true`, so none of them can catch a regression here;
    // this one exists specifically to.
    #[test]
    fn gauge_events_reach_a_subscriber_that_filters_on_is_event() {
        let recorder = TraceRecorder::new().with_target("tears::runtime::load");
        let subscriber = EventOnlySubscriber(recorder.clone());
        let _guard = set_default_subscriber(subscriber);

        let observer = LoadObserver::default();
        drop(observer.track_subscription());

        assert_eq!(
            recorder.u64_values("subscriptions"),
            vec![1, 0],
            "a subscriber that only answers enabled() for genuine events must \
             still see both gauge changes"
        );
    }

    /// Delegates to a [`TraceRecorder`], but only after checking
    /// `Metadata::is_event()` itself — unlike every other test subscriber in
    /// this module, which answers `enabled()` unconditionally `true`.
    struct EventOnlySubscriber(TraceRecorder);

    impl Subscriber for EventOnlySubscriber {
        fn enabled(&self, metadata: &Metadata<'_>) -> bool {
            metadata.is_event() && self.0.enabled(metadata)
        }

        fn new_span(&self, span: &Attributes<'_>) -> Id {
            self.0.new_span(span)
        }

        fn record(&self, span: &Id, values: &Record<'_>) {
            self.0.record(span, values);
        }

        fn record_follows_from(&self, span: &Id, follows: &Id) {
            self.0.record_follows_from(span, follows);
        }

        fn event(&self, event: &Event<'_>) {
            self.0.event(event);
        }

        fn enter(&self, span: &Id) {
            self.0.enter(span);
        }

        fn exit(&self, span: &Id) {
            self.0.exit(span);
        }
    }

    // INV-L13 (serialization, the value-loss guard): each change emits the value
    // it reached, not a later re-read of the counter — so a peak is never
    // skipped. Driven single-threaded here, the emitted sequence is exact; the
    // production guarantee under concurrency is the shared lock that brackets
    // update-and-emit (a lone atomic would let a peak be superseded before its
    // emit re-read it).
    #[test]
    fn each_gauge_change_emits_the_value_it_reached() {
        let recorder = TraceRecorder::new().with_target("tears::runtime::load");
        let _guard = recorder.set_default();

        let observer = LoadObserver::default();
        let first = observer.track_subscription();
        let second = observer.track_subscription();
        drop(second);
        drop(first);

        assert_eq!(
            recorder.u64_values("subscriptions"),
            vec![1, 2, 1, 0],
            "every reached value, including the peak of 2, is emitted in order"
        );
    }

    // INV-L13 levels: the batch event is TRACE, the gauge and capacity-wait
    // events are DEBUG. A level-exact recorder captures an event only at its own
    // level, so each field is present at the schema's level and absent at the
    // other.
    #[test]
    fn schema_events_fire_at_their_declared_levels() {
        // Batch event: TRACE, not DEBUG.
        let at_trace = TraceRecorder::new()
            .with_target("tears::runtime::load")
            .with_level(Level::TRACE);
        {
            let _guard = at_trace.set_default();
            batch(1, 1, 0);
        }
        assert_eq!(
            at_trace.u64_values("pulled"),
            vec![1],
            "batch event is TRACE"
        );

        let at_debug = TraceRecorder::new()
            .with_target("tears::runtime::load")
            .with_level(Level::DEBUG);
        {
            let _guard = at_debug.set_default();
            batch(1, 1, 0);
        }
        assert!(
            at_debug.u64_values("pulled").is_empty(),
            "batch event is not DEBUG"
        );

        // Capacity-wait event: DEBUG, not TRACE.
        {
            let _guard = at_debug.set_default();
            capacity_wait(Channel::Shared, Duration::from_micros(1));
        }
        assert_eq!(
            at_debug.str_values("channel"),
            vec!["shared".to_owned()],
            "capacity-wait event is DEBUG"
        );

        // Gauge event: DEBUG, not TRACE.
        {
            let _guard = at_debug.set_default();
            LoadObserver::default().set_keyed_entries(1);
        }
        assert_eq!(
            at_debug.u64_values("keyed_commands"),
            vec![1],
            "gauge event is DEBUG"
        );
        {
            let _guard = at_trace.set_default();
            LoadObserver::default().set_keyed_entries(1);
        }
        assert!(
            at_trace.u64_values("keyed_commands").is_empty(),
            "gauge event is not TRACE"
        );
    }

    // RFC 0006 §4.4 re-entrancy: a subscriber that causes a gauge change while
    // handling a gauge event re-enters the emit path on the same thread. The
    // dispatch funnel makes that safe — the nested change is enqueued and
    // delivered iteratively by the running drainer. This is what the off-lock
    // dispatch requires: under the old under-lock dispatch the re-entrant
    // `set_keyed_entries` would deadlock re-locking the gauge mutex, and under a
    // naive off-lock dispatch the nested event would be dropped by `tracing`'s
    // re-entrancy guard (breaking value fidelity). Here the re-entrant event
    // must be delivered as its own event, with a distinct `seq` and the value it
    // reached.
    #[test]
    fn reentrant_gauge_change_from_a_subscriber_is_delivered_not_dropped() {
        let observer = LoadObserver::default();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let subscriber = ReentrantGaugeSubscriber {
            observer: observer.clone(),
            reentered: Arc::new(AtomicBool::new(false)),
            seen: Arc::clone(&seen),
        };
        let _guard = set_default_subscriber(subscriber);

        // Emits seq 1 (subscriptions=1); the subscriber re-enters on that first
        // event and emits seq 2 (keyed_commands=1). Held so it does not drop and
        // emit a third event before the assertion reads `seen`.
        let _subscription = observer.track_subscription();

        let seen = seen
            .lock()
            .expect("reentrancy seen log mutex should not be poisoned")
            .clone();
        assert_eq!(
            seen,
            vec![(1, 0), (2, 1)],
            "the re-entrant gauge change must be delivered as its own event with \
             a distinct seq and its reached value, neither dropped nor deadlocked"
        );
    }

    /// Records every producer-gauge event's `(seq, keyed_commands)` and, the
    /// first time it sees one, causes exactly one more gauge change on the same
    /// observer — re-entering `LoadObserver::emit` from inside `event()`.
    struct ReentrantGaugeSubscriber {
        observer: LoadObserver,
        reentered: Arc<AtomicBool>,
        seen: Arc<Mutex<Vec<(u64, u64)>>>,
    }

    impl Subscriber for ReentrantGaugeSubscriber {
        fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _span: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }

        fn record(&self, _span: &Id, _values: &Record<'_>) {}

        fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

        fn event(&self, event: &Event<'_>) {
            if event.metadata().target() != "tears::runtime::load" {
                return;
            }
            let mut visitor = GaugeVisitor::default();
            event.record(&mut visitor);
            let Some(seq) = visitor.seq else { return };
            self.seen
                .lock()
                .expect("reentrancy seen log mutex should not be poisoned")
                .push((seq, visitor.keyed_commands.unwrap_or_default()));

            // Re-enter exactly once, from within the dispatch of the first gauge
            // event, to exercise the funnel.
            if !self.reentered.swap(true, Ordering::SeqCst) {
                self.observer.set_keyed_entries(1);
            }
        }

        fn enter(&self, _span: &Id) {}

        fn exit(&self, _span: &Id) {}
    }

    #[derive(Default)]
    struct GaugeVisitor {
        seq: Option<u64>,
        keyed_commands: Option<u64>,
    }

    impl Visit for GaugeVisitor {
        fn record_u64(&mut self, field: &Field, value: u64) {
            match field.name() {
                "seq" => self.seq = Some(value),
                "keyed_commands" => self.keyed_commands = Some(value),
                _ => {}
            }
        }

        fn record_debug(&mut self, _field: &Field, _value: &dyn Debug) {}
    }

    // A subscriber that panics while dispatching the first gauge event leaves the
    // drainer loop unwinding before it can relinquish the role. The `DrainGuard`,
    // still armed, must clear `draining` so the funnel is not wedged shut — every
    // later gauge change would otherwise enqueue behind a drainer that never
    // returns and never dispatch. This pins the recovery path that the arm/disarm
    // `DrainGuard` (rather than `thread::panicking()`) exists to make correct.
    #[tokio::test(flavor = "current_thread")]
    async fn a_subscriber_panic_mid_dispatch_does_not_wedge_the_funnel() {
        let observer = LoadObserver::default();
        let seen_after = Arc::new(Mutex::new(Vec::new()));
        let subscriber = PanicOnceGaugeSubscriber {
            panicked: Arc::new(AtomicBool::new(false)),
            seen_after: Arc::clone(&seen_after),
        };
        let _guard = set_default_subscriber(subscriber);

        let seen_after = with_silent_panic_hook(async {
            // First change: the subscriber panics mid-dispatch. Caught here so
            // the panic does not fail the test; the recovery is what is under
            // test.
            let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
                observer.set_keyed_entries(1);
            }));
            assert!(outcome.is_err(), "the subscriber panic must propagate");

            // The funnel must have recovered: this change dispatches again
            // instead of enqueuing forever behind the unwound drainer.
            observer.set_keyed_entries(2);

            seen_after
                .lock()
                .expect("panic-recovery seen log mutex should not be poisoned")
                .clone()
        })
        .await;

        assert_eq!(
            seen_after,
            vec![2],
            "after a subscriber panics mid-dispatch, later gauge events must \
             dispatch again rather than pile up behind a wedged drainer"
        );
    }

    /// Panics the first time it sees a producer-gauge event — mid-dispatch,
    /// inside the drainer loop — and records the `seq` of every one after.
    struct PanicOnceGaugeSubscriber {
        panicked: Arc<AtomicBool>,
        seen_after: Arc<Mutex<Vec<u64>>>,
    }

    impl Subscriber for PanicOnceGaugeSubscriber {
        fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _span: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }

        fn record(&self, _span: &Id, _values: &Record<'_>) {}

        fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

        #[expect(
            clippy::panic,
            clippy::manual_assert,
            reason = "the subscriber intentionally panics on its first event"
        )]
        fn event(&self, event: &Event<'_>) {
            if event.metadata().target() != "tears::runtime::load" {
                return;
            }
            let mut visitor = GaugeVisitor::default();
            event.record(&mut visitor);
            let Some(seq) = visitor.seq else { return };
            if !self.panicked.swap(true, Ordering::SeqCst) {
                panic!("subscriber panic mid-dispatch");
            }
            self.seen_after
                .lock()
                .expect("panic-recovery seen log mutex should not be poisoned")
                .push(seq);
        }

        fn enter(&self, _span: &Id) {}

        fn exit(&self, _span: &Id) {}
    }
}
