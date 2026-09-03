//! Shared tracing test recorder, included via `#[path]` into several
//! integration-test targets. Each accessor is exercised by some targets and not
//! others, so any given method reads as dead code in the targets that never
//! call it. `#[expect(dead_code)]` cannot express that (it would be unfulfilled
//! in the targets that *do* use the method), so the whole helper opts out of
//! `dead_code` at the module level.
#![allow(
    dead_code,
    reason = "shared cross-target test helper; per-target usage varies (see module docs)"
)]

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use tracing::callsite::rebuild_interest_cache;
use tracing::field::{Field, Visit};
use tracing::metadata::LevelFilter;
use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::{DefaultGuard, Interest, set_default};
use tracing::{Dispatch, Event, Level, Metadata, Subscriber};

// Intentionally duplicated with `src/test_support/trace_recorder.rs`. See
// docs/testing.md "Why Test Helpers Are Duplicated Instead of Shared" for why.

// `tracing`'s dispatcher registry and callsite interest cache are process-wide.
// Serialize recorder install/drop cache rebuilds, and keep a no-op dispatcher
// alive so callsites first registered on non-recorder test threads do not cache
// `Interest::never` while another test has a recorder installed.
static REGISTRY_LOCK: Mutex<()> = Mutex::new(());
static INTEREST_KEEPER: OnceLock<Dispatch> = OnceLock::new();

/// Takes the registry lock, recovering from poisoning: any panicking test
/// unwinds through [`TraceRecorderGuard`]'s drop while holding nothing but
/// this lock's discipline, and a panic inside another holder's critical
/// section must not abort the binary via a panic-while-panicking there.
/// The lock only serializes callsite-interest cache rebuilds, and the next
/// acquisition rebuilds the full cache, so a poisoned generation carries
/// no corrupt state. Every acquisition goes through here so none can
/// regress to an `.expect`.
fn registry_lock() -> MutexGuard<'static, ()> {
    REGISTRY_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A cache-safe tracing subscriber for tests that need to count events or
/// inspect simple event fields.
///
/// `tracing` caches callsite interest process-wide. This recorder keeps
/// `enabled()` open and filters in `event()` so one test does not cache
/// `Interest::never` for another test's callsite.
#[derive(Clone, Default)]
pub struct TraceRecorder {
    state: Arc<TraceRecorderState>,
    filter: EventFilter,
}

#[derive(Default)]
struct TraceRecorderState {
    events: AtomicUsize,
    bool_fields: Mutex<HashMap<String, Vec<bool>>>,
    str_fields: Mutex<HashMap<String, Vec<String>>>,
    // The sorted field-name set of each matching event, in arrival order, so a
    // test can assert which fields co-occur on a single event — across all
    // field types at once: the bool/str maps above flatten across events, and
    // the u64 event log below keeps per-event grouping for its own type only.
    field_sets: Mutex<Vec<Vec<String>>>,
    // Each event's unsigned-integer fields kept together, in arrival order —
    // the single u64 source of truth: `u64_values` flattens it per field, and
    // `current_u64` pairs a value with the `seq` recorded on its own event,
    // which a flattened per-field map could not.
    u64_events: Mutex<Vec<Vec<(String, u64)>>>,
}

#[derive(Clone, Default)]
struct EventFilter {
    target: Option<String>,
    level: Option<Level>,
}

/// Resets the thread-local tracing subscriber when dropped.
pub struct TraceRecorderGuard {
    guard: Option<DefaultGuard>,
}

impl TraceRecorder {
    /// Creates a recorder that observes every tracing event on the current
    /// thread while installed.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Restricts recorded events to a specific tracing target.
    #[must_use]
    pub fn with_target(mut self, target: impl Into<String>) -> Self {
        self.filter.target = Some(target.into());
        self
    }

    /// Restricts recorded events to a specific tracing level.
    ///
    /// `#[path]` compiles this helper per integration-test target, so methods
    /// used by one test target can be dead code in another.
    #[must_use]
    pub const fn with_level(mut self, level: Level) -> Self {
        self.filter.level = Some(level);
        self
    }

    /// Installs this recorder as the default subscriber for the current thread.
    #[must_use]
    pub fn set_default(&self) -> TraceRecorderGuard {
        let _registry = registry_lock();
        ensure_interest_keeper();
        let guard = set_default(self.clone());
        rebuild_interest_cache();
        TraceRecorderGuard { guard: Some(guard) }
    }

    /// Returns the number of events that matched this recorder's filter.
    #[must_use]
    pub fn event_count(&self) -> usize {
        self.state.events.load(Ordering::SeqCst)
    }

    /// Returns all bool values recorded for the named event field.
    #[must_use]
    pub fn bool_values(&self, field: &str) -> Vec<bool> {
        self.state
            .bool_fields
            .lock()
            .expect("trace recorder bool field log mutex should not be poisoned")
            .get(field)
            .cloned()
            .unwrap_or_default()
    }

    /// Returns all unsigned-integer values recorded for the named event field
    /// (covers `u64` and `usize` fields).
    #[must_use]
    pub fn u64_values(&self, field: &str) -> Vec<u64> {
        self.state
            .u64_events
            .lock()
            .expect("trace recorder u64 event log mutex should not be poisoned")
            .iter()
            .flat_map(|event| {
                // Every occurrence, not `field_value`'s first: the flattened
                // log must not collapse a field a callsite recorded twice.
                field_values(event, field)
            })
            .collect()
    }

    /// Returns the current value of a gauge field, read the way the gauge
    /// contract instructs (`src/runtime/load.rs`, "Ordering"): the value on
    /// the greatest-`seq` event that records `field`. Arrival order is
    /// never consulted among distinct `seq` values, so the read stays
    /// correct even if dispatch order diverges from `seq` order; a `seq`
    /// tie — impossible for a real runtime, whose `seq` strictly increases —
    /// resolves to the first-arriving match. An event carrying `field`
    /// without both a `seq` and a `runtime_id` is not a gauge snapshot and
    /// is ignored; `None` when no event carries all three. The marker
    /// names come from `GaugeSnapshot::dispatch` (`src/runtime/load.rs`),
    /// the schema's single emitter and their source of truth.
    ///
    /// # Panics
    ///
    /// Panics when the matched events span more than one `runtime_id`,
    /// since `seq` is comparable only within one runtime instance. An
    /// event is matched when it carries `seq`, `runtime_id`, and the
    /// queried field; every snapshot carries every count, so only an
    /// instance that emits no snapshot at all stays outside the scan. The
    /// panic message carries the remedies.
    #[must_use]
    #[track_caller]
    #[expect(
        clippy::panic,
        reason = "a cross-instance read is test misuse, reported with the observed ids"
    )]
    pub fn current_u64(&self, field: &str) -> Option<u64> {
        let events = self
            .state
            .u64_events
            .lock()
            .expect("trace recorder u64 event log mutex should not be poisoned");
        let mut runtime_id: Option<u64> = None;
        let mut clash: Option<(u64, u64)> = None;
        let mut current: Option<(u64, u64)> = None;
        for event in events.iter() {
            // `seq` first: the target's non-snapshot events (the batch and
            // capacity-wait families) fail that lookup, so they skip the
            // other two scans.
            let Some(seq) = field_value(event, "seq") else {
                continue;
            };
            let (Some(value), Some(id)) =
                (field_value(event, field), field_value(event, "runtime_id"))
            else {
                continue;
            };
            match runtime_id {
                None => runtime_id = Some(id),
                Some(seen) if seen != id => {
                    clash = Some((seen, id));
                    break;
                }
                Some(_) => {}
            }
            if current.is_none_or(|(greatest, _)| seq > greatest) {
                current = Some((seq, value));
            }
        }
        // Released before the panic below, so a failing guard reports its
        // own message rather than poisoning the log for every later read.
        drop(events);
        if let Some((first, second)) = clash {
            panic!(
                "current_u64({field:?}) matched gauge events from at least two runtime \
                 instances ({first} and {second}); `seq` is comparable only within one. \
                 Restructure to observe one runtime per recorder (test-helper observers, \
                 like `channel`'s throwaway, also count), read `u64_values` and partition \
                 by `runtime_id` yourself, or — if the script minted no second observer — \
                 treat it as a one-instance-per-run regression"
            );
        }
        current.map(|(_, value)| value)
    }

    /// Returns all string values recorded for the named event field.
    #[must_use]
    pub fn str_values(&self, field: &str) -> Vec<String> {
        self.state
            .str_fields
            .lock()
            .expect("trace recorder str field log mutex should not be poisoned")
            .get(field)
            .cloned()
            .unwrap_or_default()
    }

    /// Returns the sorted field-name set of every matching event, in arrival
    /// order — for asserting which fields appear together on one event.
    #[must_use]
    pub fn field_name_sets(&self) -> Vec<Vec<String>> {
        self.state
            .field_sets
            .lock()
            .expect("trace recorder field-set log mutex should not be poisoned")
            .clone()
    }
}

fn ensure_interest_keeper() {
    let _ = INTEREST_KEEPER.get_or_init(|| Dispatch::new(InterestKeeper));
}

struct InterestKeeper;

impl Subscriber for InterestKeeper {
    fn register_callsite(&self, _metadata: &'static Metadata<'static>) -> Interest {
        Interest::sometimes()
    }

    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        false
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(LevelFilter::TRACE)
    }

    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, _event: &Event<'_>) {}

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

impl Drop for TraceRecorderGuard {
    fn drop(&mut self) {
        let _registry = registry_lock();
        drop(self.guard.take());
        rebuild_interest_cache();
    }
}

impl EventFilter {
    fn matches(&self, metadata: &Metadata<'_>) -> bool {
        self.target
            .as_deref()
            .is_none_or(|target| metadata.target() == target)
            && self.level.is_none_or(|level| *metadata.level() == level)
    }
}

/// Every value the event recorded under `name`, in record order — the one
/// place the name match is spelled, so `field_value` and `u64_values`
/// cannot disagree about which events carry a field.
fn field_values<'event>(
    event: &'event [(String, u64)],
    name: &'event str,
) -> impl Iterator<Item = u64> + 'event {
    event
        .iter()
        .filter(move |(field_name, _)| field_name == name)
        .map(|(_, value)| *value)
}

/// The named unsigned-integer field of one recorded event — the *first*
/// occurrence when a callsite records the name twice. `u64_values` keeps
/// every occurrence; this is for the snapshot fields (`seq`,
/// `runtime_id`, and the gauges), which a snapshot carries once each.
fn field_value(event: &[(String, u64)], name: &str) -> Option<u64> {
    field_values(event, name).next()
}

impl Subscriber for TraceRecorder {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        None
    }

    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        if !self.filter.matches(event.metadata()) {
            return;
        }

        self.state.events.fetch_add(1, Ordering::SeqCst);

        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);

        let mut names = visitor.names.clone();
        names.sort();
        self.state
            .field_sets
            .lock()
            .expect("trace recorder field-set log mutex should not be poisoned")
            .push(names);

        if !visitor.bools.is_empty() {
            let mut fields = self
                .state
                .bool_fields
                .lock()
                .expect("trace recorder bool field log mutex should not be poisoned");
            for (field, value) in visitor.bools {
                fields.entry(field).or_default().push(value);
            }
        }
        if !visitor.u64s.is_empty() {
            self.state
                .u64_events
                .lock()
                .expect("trace recorder u64 event log mutex should not be poisoned")
                .push(visitor.u64s);
        }
        if !visitor.strs.is_empty() {
            let mut fields = self
                .state
                .str_fields
                .lock()
                .expect("trace recorder str field log mutex should not be poisoned");
            for (field, value) in visitor.strs {
                fields.entry(field).or_default().push(value);
            }
        }
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

#[derive(Default)]
struct FieldVisitor {
    bools: Vec<(String, bool)>,
    u64s: Vec<(String, u64)>,
    strs: Vec<(String, String)>,
    names: Vec<String>,
}

impl Visit for FieldVisitor {
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.names.push(field.name().to_owned());
        self.bools.push((field.name().to_owned(), value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.names.push(field.name().to_owned());
        self.u64s.push((field.name().to_owned(), value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.names.push(field.name().to_owned());
        if let Ok(value) = u64::try_from(value) {
            self.u64s.push((field.name().to_owned(), value));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.names.push(field.name().to_owned());
        self.strs.push((field.name().to_owned(), value.to_owned()));
    }

    fn record_debug(&mut self, field: &Field, _value: &dyn Debug) {
        self.names.push(field.name().to_owned());
    }
}

// Mirrored from the src copy so each integration target that includes this
// helper also pins its own copy's `current_u64` semantics — the two files
// are duplicated by policy and drift silently otherwise. The src copy's
// cross-instance `#[should_panic]` row is *not* mirrored: one copy pins
// the guard's contract, and running it per target would add a deliberate
// panic to every including binary for no new coverage of the same
// source.
#[cfg(test)]
mod tests {
    use super::*;

    const TARGET: &str = "tears::tests::common::trace_recorder";

    #[test]
    fn current_u64_reads_by_seq_not_by_arrival() {
        let recorder = TraceRecorder::new().with_target(TARGET);
        let _guard = recorder.set_default();

        tracing::trace!(target: TARGET, runtime_id = 4u64, seq = 2u64, gauge = 5u64, "later snapshot, early arrival");
        tracing::trace!(target: TARGET, runtime_id = 4u64, seq = 1u64, gauge = 9u64, "earlier snapshot, late arrival");
        tracing::trace!(target: TARGET, gauge = 7u64, "no seq: not a snapshot");
        tracing::trace!(target: TARGET, runtime_id = 4u64, seq = 3u64, other = 1u64, "snapshot without the field");
        tracing::trace!(target: TARGET, seq = 9u64, gauge = 1u64, "no runtime_id: not a snapshot either");
        tracing::trace!(target: TARGET, runtime_id = 4u64, seq = 2u64, gauge = 8u64, "a seq tie: the first arrival keeps the read");

        assert_eq!(
            recorder.current_u64("gauge"),
            Some(5),
            "the greatest-seq event carrying the field wins — whatever arrived last, whatever \
             lacks a runtime_id, and whichever of a seq tie arrived later"
        );
        assert_eq!(
            recorder.u64_values("gauge"),
            vec![5, 9, 7, 1, 8],
            "while the flattened view keeps every value in arrival order — the last is exactly \
             where the arrival-order read differs"
        );
        assert_eq!(recorder.current_u64("absent"), None);
    }
}
