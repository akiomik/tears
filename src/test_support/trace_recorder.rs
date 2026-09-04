use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::{self, Debug};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

use tracing::callsite::rebuild_interest_cache;
use tracing::field::{Field, Visit};
use tracing::metadata::LevelFilter;
use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::{DefaultGuard, Interest, set_default};
use tracing::{Dispatch, Event, Level, Metadata, Subscriber};

// Intentionally duplicated with `tests/common/trace_recorder.rs`. See
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
    // the single u64 source of truth: `u64_values` flattens it per field,
    // `u64_event_values` keeps the grouping, and the current-value reads pair
    // a value with the `seq` recorded on its own event, which a flattened
    // per-field map could not.
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
    #[must_use]
    pub const fn with_level(mut self, level: Level) -> Self {
        self.filter.level = Some(level);
        self
    }

    /// Installs this recorder as the default subscriber for the current thread.
    #[must_use]
    pub fn set_default(&self) -> TraceRecorderGuard {
        set_default_subscriber(self.clone())
    }

    /// Returns the number of events that matched this recorder's filter.
    #[must_use]
    pub fn event_count(&self) -> usize {
        self.state.events.load(Ordering::SeqCst)
    }

    /// Returns all unsigned-integer values recorded for the named event field
    /// (covers `u64` and `usize` fields), as [`Readings`] — which withholds
    /// the positional reads a current-value assertion would reach for by
    /// reflex. See that type for which reads stay open and how to name the
    /// one that does not.
    #[must_use]
    pub fn u64_values(&self, field: &str) -> Readings {
        Readings::new(
            self.state
                .u64_events
                .lock()
                .expect("trace recorder u64 event log mutex should not be poisoned")
                .iter()
                .flat_map(|event| {
                    // Every occurrence, not `field_value`'s first: the
                    // flattened log must not collapse a field a callsite
                    // recorded twice.
                    field_values(event, field)
                })
                .collect(),
        )
    }

    /// The named unsigned-integer fields of every event that records them,
    /// grouped per event and ordered as named — the per-event view
    /// [`Self::u64_values`] flattens away. Each name reads its event's
    /// *first* occurrence, the way the snapshot fields are read, so a name
    /// repeated in `fields` simply repeats its column. An event recording
    /// none of the names is skipped, so the target's other event families
    /// stay out of the result.
    ///
    /// Reading several fields this way rather than zipping as many
    /// `u64_values` logs is what keeps a caller aligned with the schema:
    /// `zip` pairs by position, truncates to its shortest input, and reports
    /// no mismatch, so a field that stops riding every event shifts the
    /// tuples or shortens the vector silently. Here a partial event fails,
    /// by name.
    ///
    /// That alignment is what two or more names buy. With one name there is
    /// nothing to align — the read is [`Self::u64_values`] narrowed to each
    /// event's first occurrence — and with none there would be nothing to
    /// read at all, which is a compile-time error rather than an empty
    /// answer (see the const assertion in the body).
    ///
    /// These rows are a plain `Vec`, so `.last()` compiles here even though
    /// [`Readings`] refuses it. On a gauge field that read is the
    /// arrival-order current value the contract forbids: take it from
    /// [`Self::current_u64`] instead, and keep this accessor for what it is
    /// for — reading the names on one event together.
    ///
    /// # Panics
    ///
    /// Panics when an event records some of `fields` but not all: such an
    /// event has no aligned reading, and both ways of inventing one —
    /// dropping it, or padding the missing name — are the silent
    /// misalignment this accessor exists to refuse.
    #[must_use]
    #[track_caller]
    #[expect(
        clippy::panic,
        reason = "a partially recorded event is a schema break, reported with the event"
    )]
    pub fn u64_event_values<const N: usize>(&self, fields: [&str; N]) -> Vec<[u64; N]> {
        // The one shape where "refuses a partial event" would degrade to
        // silence: with no names every event records none of them, so the
        // scan below would answer `vec![]` whatever the log holds — an empty
        // trace that reads like a checked one. No row can pin that, since the
        // answer is a value rather than a failure, so it is refused where the
        // caller is written instead.
        const { assert!(N > 0, "u64_event_values needs at least one field name") };
        let events = self
            .state
            .u64_events
            .lock()
            .expect("trace recorder u64 event log mutex should not be poisoned");
        let mut rows: Vec<[u64; N]> = Vec::new();
        let mut partial: Option<Vec<(String, u64)>> = None;
        for event in events.iter() {
            let values: Vec<u64> = fields
                .iter()
                .filter_map(|name| field_value(event, name))
                .collect();
            if values.is_empty() {
                continue;
            }
            // At most one value per name, so a short vector is a name this
            // event never recorded — never a name recorded twice.
            let Ok(row) = <[u64; N]>::try_from(values.as_slice()) else {
                partial = Some(event.clone());
                break;
            };
            rows.push(row);
        }
        // Released before the panic below, so a failing guard reports its
        // own message rather than poisoning the log for every later read.
        drop(events);
        if let Some(event) = partial {
            let missing: Vec<&str> = fields
                .into_iter()
                .filter(|name| field_value(&event, name).is_none())
                .collect();
            panic!(
                "u64_event_values({fields:?}) found an event recording only some of the \
                 names: {missing:?} missing from {event:?}. A partial event has no \
                 aligned reading — either a name here is not on this event's schema, or \
                 a field stopped riding every event. The log holds a name only where the \
                 event recorded it as a non-negative integer"
            );
        }
        rows
    }

    /// Each observed runtime instance's current value of a gauge field,
    /// keyed by `runtime_id`: the value on that instance's greatest-`seq`
    /// event recording `field`. This is the gauge contract's read rule
    /// (`src/runtime/load.rs`, "Ordering") in full — a subscriber watching
    /// several runtimes in one process partitions by `runtime_id` first and
    /// applies the max-`seq` rule inside each partition, since `seq` is
    /// comparable only within one instance.
    ///
    /// Arrival order is never consulted among distinct `seq` values, so the
    /// read stays correct even if dispatch order diverges from `seq` order;
    /// a `seq` tie — impossible for a real runtime, whose `seq` strictly
    /// increases — resolves to the first-arriving match. An event carrying
    /// `field` without both a `seq` and a `runtime_id` is not a gauge
    /// snapshot and is ignored; the map is empty when no event carries all
    /// three. The marker names must match `GAUGE_EVENT_FIELDS`
    /// (`src/runtime/load.rs`), the schema's source of truth, which a unit
    /// row holds to what `GaugeSnapshot::dispatch` actually emits. Nothing
    /// binds these two literals to that array — a coordinated rename empties
    /// this map, and the census's strict branch is what refuses it.
    #[must_use]
    pub fn current_u64_by_runtime(&self, field: &str) -> BTreeMap<u64, u64> {
        let events = self
            .state
            .u64_events
            .lock()
            .expect("trace recorder u64 event log mutex should not be poisoned");
        let mut current: BTreeMap<u64, (u64, u64)> = BTreeMap::new();
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
            let slot = current.entry(id).or_insert((seq, value));
            if seq > slot.0 {
                *slot = (seq, value);
            }
        }
        drop(events);
        current
            .into_iter()
            .map(|(id, (_, value))| (id, value))
            .collect()
    }

    /// Returns the current value of a gauge field, read the way the gauge
    /// contract instructs (`src/runtime/load.rs`, "Ordering"): the value on
    /// the greatest-`seq` event that records `field`. This is the
    /// one-instance convenience over [`Self::current_u64_by_runtime`], and
    /// inherits every property described there: the `seq` ordering, the tie,
    /// and what counts as a gauge snapshot at all. `None` when no event
    /// carries `field` beside both markers.
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
    pub fn current_u64(&self, field: &str) -> Option<u64> {
        let current = self.current_u64_by_runtime(field);
        assert!(
            current.len() <= 1,
            "current_u64({field:?}) matched gauge events from at least two runtime \
             instances ({ids:?}); `seq` is comparable only within one. Restructure to \
             observe one runtime per recorder (test-helper observers, like `channel`'s \
             throwaway, also count), read `current_u64_by_runtime` and name the \
             instance you mean, or — if the script minted no second observer — treat \
             it as a one-instance-per-run regression",
            ids = current.keys().copied().collect::<Vec<_>>()
        );
        current.into_values().next()
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

/// The readings one event field took, as the recorder's log holds them —
/// the gauge counts, whose current value the contract below defines, and
/// the per-event fields beside them (`pulled`, `wait_us`) that have no
/// current value to take at all.
///
/// The gauge contract (`src/runtime/load.rs`, "Ordering") puts a gauge's
/// current value on its greatest-`seq` event, and says arrival order
/// matching `seq` order is an implementation coincidence no consumer may
/// rely on. A plain `Vec<u64>` hands `.last()` to every caller as the
/// ergonomic default, which puts the arrival-order read one keystroke from
/// any current-value assertion — the read #318 removed from six files,
/// and that nothing but a line of `docs/testing.md` then kept out.
///
/// So this type publishes no positional read: no first, no last, no index,
/// no slice, no iterator. Its methods answer what the log *contains*, and a
/// caller who means to observe the log *as a sequence* says so by name,
/// through [`Self::arrival_order`] or [`Self::into_arrival_order`]. Those
/// two and the `Debug` rendering are order-dependent by construction: they
/// are the sanctioned way to depend on arrival order rather than exceptions
/// to a wider claim. No method here takes a closure either, so evaluation
/// order stays unobservable through a stateful predicate as well.
///
/// The current value is not on this type at all. It is
/// [`TraceRecorder::current_u64`], which reads by `seq`.
///
/// The refusal reaches exactly the reads that come through this type, and
/// the compiler applies it wherever such a call site is written. Two things
/// stay outside it. A later edit could add `last()` to the impl below —
/// nothing gates the surface itself. And [`TraceRecorder::u64_event_values`]
/// reads the same log into plain rows, where `.last()` still compiles: with
/// one name it is this accessor narrowed to each event's first occurrence,
/// so a current-value read written that way is the arrival-order read again,
/// on a gauge field. Both are held by review against this paragraph and
/// `docs/testing.md`, not by the compiler.
pub struct Readings(Vec<u64>);

impl Readings {
    /// Wraps a field's flattened log. Private to this helper, so a test
    /// takes its readings from the recorder rather than minting them in an
    /// order the runtime never emitted; the rows below build logs directly
    /// because a boundary is a shape, not a run.
    const fn new(values: Vec<u64>) -> Self {
        Self(values)
    }

    /// How many readings the field recorded.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the field was never recorded — the "this gauge never fired"
    /// claim, which is about the log itself and so has no current value to
    /// read.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether some reading was `value`. By value rather than by
    /// reference: `Readings` is not a slice and should not read like one,
    /// and a `u64` is `Copy`.
    #[must_use]
    pub fn contains(&self, value: u64) -> bool {
        self.0.contains(&value)
    }

    /// Whether some reading was above zero — "this gauge rose at some
    /// point". `false` for an empty log, which is what a gauge that never
    /// fired never did.
    #[must_use]
    pub fn contains_nonzero(&self) -> bool {
        self.0.iter().any(|&value| value > 0)
    }

    /// The single reading, when the field recorded exactly one; `None` for
    /// none, and `None` for several. A caller that has established a lone
    /// reading takes it here rather than by index, so the "exactly one"
    /// claim and the value it licenses stay in one place.
    #[must_use]
    pub fn only(&self) -> Option<u64> {
        match *self.0.as_slice() {
            [value] => Some(value),
            _ => None,
        }
    }

    /// Whether no reading repeats. Vacuously `true` for an empty log and
    /// for a single reading, which is what the sort-dedup-and-compare-lengths
    /// idiom this replaces answers there. A row that must also refuse an
    /// empty log asserts [`Self::is_empty`] beside this one: vacuity is its
    /// own claim.
    #[must_use]
    pub fn all_unique(&self) -> bool {
        let mut seen = HashSet::with_capacity(self.0.len());
        self.0.iter().all(|value| seen.insert(*value))
    }

    /// Whether every reading agrees. Vacuously `true` on the same two
    /// boundaries as [`Self::all_unique`], and for the same reason: it is
    /// what the `windows(2)` comparison this replaces answers there.
    #[must_use]
    pub fn all_equal(&self) -> bool {
        self.0.windows(2).all(|pair| pair[0] == pair[1])
    }

    /// The readings in the order they arrived — the opt-out, for a row
    /// whose claim really is about arrival order: an exact emission
    /// sequence, an index into it, a suffix of it.
    ///
    /// One grep for this name enumerates the rows that take that dependency
    /// *through this type*, which is not the whole list. A row reading the
    /// same log through [`TraceRecorder::u64_event_values`] depends on
    /// arrival order while naming nothing: `src/runtime/load.rs`'s
    /// strictly-increasing-`seq` row builds its per-`runtime_id` partitions
    /// by iterating the rows as they arrived, and `src/kernel/producer.rs`'s
    /// gauge trace compares exact arrival-order sequences. A divergence
    /// between dispatch order and `seq` order has to re-examine both sets.
    #[must_use]
    pub fn arrival_order(&self) -> &[u64] {
        &self.0
    }

    /// [`Self::arrival_order`] by value, for a caller that sorts the
    /// readings, keeps them past the borrow, or indexes them after a move.
    #[must_use]
    pub fn into_arrival_order(self) -> Vec<u64> {
        self.0
    }
}

impl Debug for Readings {
    /// Renders as the readings alone, so a failure message interpolating a
    /// `Readings` reads exactly as it did when this was a `Vec<u64>`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Debug::fmt(&self.0, f)
    }
}

/// Installs an arbitrary subscriber as the current thread's default,
/// cache-safely — the same process-wide callsite-interest handling
/// [`TraceRecorder::set_default`] uses. For tests that need a bespoke subscriber
/// rather than a [`TraceRecorder`], e.g. one that re-enters the code under test
/// from inside `event()`. Returns a guard that restores the previous default on
/// drop.
#[must_use]
pub fn set_default_subscriber<S>(subscriber: S) -> TraceRecorderGuard
where
    S: Subscriber + Send + Sync + 'static,
{
    let _registry = registry_lock();
    ensure_interest_keeper();
    let guard = set_default(subscriber);
    rebuild_interest_cache();
    TraceRecorderGuard { guard: Some(guard) }
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

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    const TARGET: &str = "tears::test_support::trace_recorder";

    fn emit_cross_thread_registration_event() {
        tracing::trace!(target: TARGET, "cross-thread registration");
    }

    #[test]
    fn recorder_observes_callsite_registered_first_on_unrecorded_thread() {
        let recorder = TraceRecorder::new().with_target(TARGET);
        let _guard = recorder.set_default();

        thread::spawn(emit_cross_thread_registration_event)
            .join()
            .expect("event registration thread should not panic");
        emit_cross_thread_registration_event();

        assert_eq!(
            recorder.event_count(),
            1,
            "recorder should observe an event even if the callsite was first registered on a non-recorder thread"
        );
    }

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
            recorder.u64_values("gauge").arrival_order(),
            [5, 9, 7, 1, 8],
            "while the flattened view keeps every value in arrival order — the last is exactly \
             where the arrival-order read differs"
        );
        assert_eq!(recorder.current_u64("absent"), None);
    }

    // The partition half of the same rule: `seq` orders events only inside
    // one `runtime_id`, so a reader watching several instances keeps one
    // current value per instance.
    #[test]
    fn current_u64_by_runtime_reads_each_instance_by_its_own_seq() {
        let recorder = TraceRecorder::new().with_target(TARGET);
        let _guard = recorder.set_default();

        // Interleaved, with the greater `seq` on the instance whose current
        // value is not the last to arrive: a read that compared `seq` across
        // instances, or that took the latest arrival, gets both partitions
        // wrong.
        tracing::trace!(target: TARGET, runtime_id = 1u64, seq = 1u64, gauge = 3u64, "first instance");
        tracing::trace!(target: TARGET, runtime_id = 2u64, seq = 9u64, gauge = 7u64, "second instance, a greater seq");
        tracing::trace!(target: TARGET, runtime_id = 1u64, seq = 2u64, gauge = 5u64, "first instance's own current value");
        tracing::trace!(target: TARGET, runtime_id = 2u64, seq = 8u64, gauge = 0u64, "second instance, an earlier seq arriving later");
        tracing::trace!(target: TARGET, gauge = 4u64, "no markers: not a snapshot");
        tracing::trace!(target: TARGET, runtime_id = 3u64, seq = 1u64, other = 1u64, "a third instance, recording another field only");

        assert_eq!(
            recorder.current_u64_by_runtime("gauge"),
            BTreeMap::from([(1, 5), (2, 7)]),
            "each instance's greatest-seq reading, whatever arrived last and \
             whatever `seq` another instance reached — and no key at all for \
             the instance whose events never carried the field"
        );
        assert_eq!(recorder.current_u64_by_runtime("absent"), BTreeMap::new());
    }

    // The surface `Readings` publishes, boundaries included. Every answer
    // below is the one the idiom it replaced already gave — `only` for a
    // length check and an index, `all_unique` for sort-dedup-and-compare,
    // `all_equal` for `windows(2)`, `contains_nonzero` for `any(> 0)` — so
    // the migration moved no call site's meaning. Built through the private
    // constructor: the recorder path is pinned by the row above, and these
    // shapes (no reading, one, a repeat) are what a log's boundaries are.
    #[test]
    fn readings_answer_what_the_log_contains() {
        let empty = Readings::new(vec![]);
        let single = Readings::new(vec![7]);
        let mixed = Readings::new(vec![0, 3, 0]);

        assert_eq!(empty.only(), None, "no reading can be the only one");
        assert_eq!(single.only(), Some(7), "the lone reading, without an index");
        assert_eq!(
            mixed.only(),
            None,
            "and three readings have no single one either — the claim `only` \
             carries is `exactly one`, not `take the first`"
        );

        assert!(
            empty.all_unique(),
            "an empty log has no repeat, which is what sort-dedup-and-compare answered"
        );
        assert!(
            empty.all_equal(),
            "and no disagreement, which is what `windows(2)` answered"
        );
        assert!(single.all_unique(), "one reading cannot repeat");
        assert!(single.all_equal(), "or disagree with itself");
        assert!(!mixed.all_unique(), "the zero recurs");
        assert!(!mixed.all_equal(), "and the three differs from it");
        assert!(
            Readings::new(vec![4, 4]).all_equal(),
            "two agreeing readings agree"
        );
        assert!(
            Readings::new(vec![4, 5]).all_unique(),
            "and two differing ones are distinct"
        );

        assert!(
            !empty.contains_nonzero(),
            "a gauge that never fired never rose"
        );
        assert!(
            !Readings::new(vec![0, 0]).contains_nonzero(),
            "nor did one that only ever read zero"
        );
        assert!(mixed.contains_nonzero(), "the three is a rise");

        assert!(mixed.contains(3), "a reading the log holds");
        assert!(!mixed.contains(9), "and one it does not");
        assert_eq!(mixed.len(), 3, "every occurrence, including the repeat");
        assert!(empty.is_empty(), "no event carried the field");
        assert!(!mixed.is_empty(), "while three readings did");
        assert_eq!(
            format!("{mixed:?}"),
            "[0, 3, 0]",
            "and the readings render as the log alone: a failure message that \
             interpolates them reads as it did when this was a `Vec<u64>`"
        );
    }

    // Order lives behind the opt-out, and nowhere else on the surface.
    //
    // Three of the reads can fail here, and the fixtures are picked so that
    // they can: a pair whose first reading differs separates a
    // `contains_nonzero` that consulted a position, and a pair that moves a
    // repeat separates an `all_equal` that compared the ends and an
    // `all_unique` that compared neighbours.
    //
    // The other four cannot be separated by any permutation, since they are
    // determined by the log's length or by the set of its values, neither of
    // which a permutation changes. `len` and `contains` ride along here as
    // the count and membership reads; `only` and `is_empty` are left out
    // rather than asserted vacuously. The row above pins all four.
    //
    // So this row documents intent for the reads it names; it is not a proof
    // of the type's rule, since a method added later is outside every
    // assertion here, and review against the type's docs is what covers that.
    #[test]
    fn only_the_arrival_order_reads_depend_on_order() {
        for (forward, backward) in [
            (Readings::new(vec![0, 3]), Readings::new(vec![3, 0])),
            (Readings::new(vec![1, 2, 1]), Readings::new(vec![1, 1, 2])),
        ] {
            assert_eq!(
                forward.len(),
                backward.len(),
                "a permuted log is as long: {forward:?} against {backward:?}"
            );
            assert_eq!(
                forward.contains_nonzero(),
                backward.contains_nonzero(),
                "and rose in the same readings: {forward:?} against {backward:?}"
            );
            assert_eq!(
                forward.all_unique(),
                backward.all_unique(),
                "with the same repeats: {forward:?} against {backward:?}"
            );
            assert_eq!(
                forward.all_equal(),
                backward.all_equal(),
                "and the same disagreement: {forward:?} against {backward:?}"
            );
            assert_eq!(
                forward.contains(3),
                backward.contains(3),
                "and holds the same readings: {forward:?} against {backward:?}"
            );
            assert_ne!(
                forward.arrival_order(),
                backward.arrival_order(),
                "while the opt-out is exactly where the two logs differ"
            );
        }

        assert_eq!(
            Readings::new(vec![1, 2]).into_arrival_order(),
            vec![1, 2],
            "which the owned form hands over unchanged"
        );
    }

    #[test]
    fn u64_event_values_groups_fields_per_event() {
        let recorder = TraceRecorder::new().with_target(TARGET);
        let _guard = recorder.set_default();

        tracing::trace!(target: TARGET, left = 1u64, right = 2u64, "both names");
        tracing::trace!(target: TARGET, elsewhere = 9u64, "neither name: skipped, not padded");
        tracing::trace!(target: TARGET, right = 4u64, left = 3u64, spare = 9u64, "recorded in the other order, beside a name not asked for");

        assert_eq!(
            recorder.u64_event_values(["left", "right"]),
            vec![[1, 2], [3, 4]],
            "one row per event recording the names, ordered as named rather \
             than as the event recorded them"
        );
    }

    // Pinned here and not in the tests/ mirror: one copy suffices for the
    // contract, and mirroring would add a deliberate panic to every
    // including binary for no new coverage of the same source. No
    // `hook_guard`: this row swaps no hook, and a recording hook's defence
    // against deliberate panics is its thread-name filter
    // (docs/testing.md, "Process-Global Panic Hook Tests").
    #[test]
    #[should_panic(expected = "matched gauge events from at least two runtime instances")]
    fn current_u64_refuses_a_cross_instance_read() {
        let recorder = TraceRecorder::new().with_target(TARGET);
        let _guard = recorder.set_default();

        tracing::trace!(target: TARGET, runtime_id = 1u64, seq = 1u64, gauge = 3u64, "one instance");
        tracing::trace!(target: TARGET, runtime_id = 2u64, seq = 9u64, gauge = 0u64, "another");

        let _ = recorder.current_u64("gauge");
    }

    // Pinned in this copy only, for the reason the row above it carries.
    #[test]
    #[should_panic(expected = "recording only some of the names")]
    fn u64_event_values_refuses_a_partial_event() {
        let recorder = TraceRecorder::new().with_target(TARGET);
        let _guard = recorder.set_default();

        tracing::trace!(target: TARGET, left = 1u64, right = 2u64, "a whole event");
        tracing::trace!(target: TARGET, left = 3u64, "and one that dropped a name");

        let _ = recorder.u64_event_values(["left", "right"]);
    }
}
