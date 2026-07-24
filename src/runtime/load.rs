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
//! The gauge counters live behind one mutex. Each change locks once to update a
//! field and copy out all four values, then emits that snapshot after releasing
//! the lock. Snapshotting under the lock is the value-fidelity guarantee (RFC
//! 0006 §4.4, INV-L13): the event carries the value the change reached, not a
//! later re-read, so were the update and the read separate (e.g. lone atomics)
//! a value could be reached and superseded before its own emit re-read the
//! counters, and a subscriber's high-water mark could miss it. Emitting after
//! the lock is released keeps the subscriber's handler off the counter lock, so
//! a slow or re-entrant handler neither stalls other producers nor deadlocks
//! against the bookkeeping (INV-L13 non-blocking emission). Arrival order across
//! concurrently updating producers is consequently not guaranteed — only
//! per-event value fidelity is.
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

#[derive(Clone, Copy, Default)]
struct Gauges {
    subscriptions: usize,
    unkeyed_commands: usize,
    keyed_commands: usize,
    blocked: usize,
}

impl Gauges {
    /// Emits the four-field producer-gauge event from this snapshot. Taking
    /// `self` by value fixes the values at the caller's serialization point, so
    /// the event reports the state that was reached rather than a later re-read.
    fn emit(self) {
        tracing::debug!(
            target: "tears::runtime::load",
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
        let snapshot = {
            let mut gauges = self.lock();
            if gauges.keyed_commands == count {
                return;
            }
            gauges.keyed_commands = count;
            *gauges
        };
        snapshot.emit();
    }

    fn enter(&self, field: Field) -> GaugeGuard {
        self.step(field, 1);
        GaugeGuard {
            observer: self.clone(),
            field,
        }
    }

    /// Adds `delta` (`+1`/`-1`) to `field`, copies the resulting four-field
    /// snapshot out under the lock, then emits it after the lock is released
    /// (RFC 0006 §4.4, INV-L13). Snapshotting under the lock fixes the value
    /// the change reached (value fidelity); emitting outside it keeps the
    /// subscriber's handler off the counter lock (non-blocking emission).
    fn step(&self, field: Field, delta: isize) {
        let snapshot = {
            let mut gauges = self.lock();
            let counter = field.counter_mut(&mut gauges);
            *counter = counter.wrapping_add_signed(delta);
            *gauges
        };
        snapshot.emit();
    }

    fn lock(&self) -> MutexGuard<'_, Gauges> {
        // Recover rather than propagate a poisoned lock. The gauges are plain
        // counters with no cross-field invariant, so a producer that panicked
        // while holding the lock would leave them merely off-by-one, not
        // corrupt. The lock is held only for the update and the snapshot copy —
        // emission runs after it is released — so a subscriber's handler can no
        // longer poison it; recovery remains as defense for a panic in the
        // update itself and keeps `GaugeGuard::drop` (which locks during
        // unwinding) from turning a poisoned lock into a double-panic abort.
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

    // INV-L13: every producer-gauge event carries all four fields together, so
    // a subscriber reads a complete snapshot from any one event. The per-field
    // recorder views flatten across events and cannot see this; the field-set
    // view can.
    #[test]
    fn gauge_event_carries_all_four_fields() {
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

    // INV-L13 (value fidelity, the value-loss guard): each change emits the
    // value it reached, not a later re-read of the counter — so a peak is never
    // skipped. Driven single-threaded here, the emitted sequence is exact; the
    // production guarantee under concurrency is the snapshot taken under the
    // lock, which fixes the reached value before emission (a lone atomic would
    // let a peak be superseded before its emit re-read it). Cross-producer
    // arrival order is deliberately not guaranteed, so this asserts values, not
    // a cross-thread order.
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
