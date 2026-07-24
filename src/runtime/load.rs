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
//! The gauge counters live behind one mutex. Each change updates a field, bumps
//! a monotone `seq`, and snapshots everything under the same lock. Two separate
//! guarantees ride on that lock, and only one of them constrains the order
//! events *arrive* in:
//!
//! - **Value fidelity** (every reached value gets its own event): capturing the
//!   update and the snapshot together under the lock prevents a value from being
//!   reached and superseded before its own emit re-reads the counters — a lone
//!   atomic would let a subscriber's high-water mark miss a peak. This guarantee
//!   is about the snapshot, not about arrival order.
//! - **Ordering** is carried by `seq`, not by arrival: the current value of each
//!   gauge is the value on the greatest-`seq` event (RFC 0006 §4.4, INV-L13).
//!   The lock currently also serializes the `tracing` dispatch, so today arrival
//!   order happens to match `seq` order — but the contract does not promise it,
//!   and consumers must order by `seq`. That is deliberate: because the
//!   snapshot-and-`seq` capture stays under the lock while only the dispatch
//!   would move, a later change can emit the event out from under the lock — so
//!   a slow subscriber no longer stalls producers on that lock, and one that
//!   re-enters the runtime no longer deadlocks on it — without breaking any
//!   `seq`-ordered consumer. Moving the dispatch off the lock does not by itself
//!   make a subscriber that *causes* a gauge change safe (it re-enters the emit
//!   path); that is a separate re-entrancy hazard, out of scope here and tracked
//!   against RFC 0006 §4.4.
//!
//! `pub` items rather than `pub(crate)`: the enclosing `runtime` module is
//! already `pub(crate)`, so effective reachability is capped at the crate
//! (see `channel`/`frame_rate`), while `pub` avoids the redundant-`pub(crate)`
//! lint.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

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
/// Cloning shares the same counters; every producer holds a clone and updates
/// its own field, so a single subscriber sees the aggregate. All four counts
/// are emitted together under `tears::runtime::load` whenever any one changes.
#[derive(Clone, Default)]
pub struct LoadObserver {
    gauges: Arc<Mutex<Gauges>>,
}

// Deliberately not `Copy`/`Clone`: `emit` bumps `seq` through `&mut self`, so a
// by-value copy (`let mut g = *guard; g.emit()`) would bump and emit a throwaway
// while the shared `seq` stalled — the exact silently-dropped update this
// ordering exists to prevent. Without the derives that pattern fails to compile.
#[derive(Default)]
struct Gauges {
    /// Monotone per-observer counter, bumped once per emitted gauge event and
    /// carried on it as `seq`. Captured under the same lock as the four counts,
    /// so a greater `seq` never carries an older value; a subscriber reads the
    /// current value of each gauge from the greatest-`seq` event, so arrival
    /// order is not load-bearing (RFC 0006 §4.4, INV-L13).
    seq: u64,
    subscriptions: usize,
    unkeyed_commands: usize,
    keyed_commands: usize,
    blocked: usize,
}

impl Gauges {
    /// Bumps `seq` and emits the producer-gauge event — the four counts plus
    /// their ordering `seq`. Takes `&mut self` so the bump lands in the shared
    /// state; the caller holds the lock, fixing the counts and `seq` together
    /// at the serialization point, so
    /// the event reports the state that was reached (never a later re-read) and
    /// its `seq` orders it against every other gauge event without relying on
    /// arrival order.
    fn emit(&mut self) {
        self.seq = self.seq.wrapping_add(1);
        tracing::debug!(
            target: "tears::runtime::load",
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
        let mut gauges = self.lock();
        if gauges.keyed_commands != count {
            gauges.keyed_commands = count;
            gauges.emit();
        }
    }

    fn enter(&self, field: Field) -> GaugeGuard {
        self.step(field, 1);
        GaugeGuard {
            observer: self.clone(),
            field,
        }
    }

    /// Adds `delta` (`+1`/`-1`) to `field` and emits the resulting snapshot,
    /// all under one lock so the update and its event cannot interleave with
    /// another producer's (RFC 0006 §4.4 "event per change").
    fn step(&self, field: Field, delta: isize) {
        let mut gauges = self.lock();
        let counter = field.counter_mut(&mut gauges);
        *counter = counter.wrapping_add_signed(delta);
        gauges.emit();
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

#[cfg(test)]
mod tests {
    use tracing::Level;

    use super::*;
    use crate::test_support::TraceRecorder;

    // INV-L13: every producer-gauge event carries the full field set together —
    // the four gauges plus their ordering `seq` — so a subscriber reads a
    // complete, ordered snapshot from any one event. The per-field recorder
    // views flatten across events and cannot see this; the field-set view can.
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
    // arrival — the current value of each gauge is the greatest-`seq` event's.
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
}
