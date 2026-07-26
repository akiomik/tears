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
use std::sync::{Arc, Mutex, OnceLock};

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
    u64_fields: Mutex<HashMap<String, Vec<u64>>>,
    str_fields: Mutex<HashMap<String, Vec<String>>>,
    // The sorted field-name set of each matching event, in arrival order, so a
    // test can assert which fields co-occur on a single event (the per-field
    // maps above flatten across events and cannot).
    field_sets: Mutex<Vec<Vec<String>>>,
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
        let _registry = REGISTRY_LOCK
            .lock()
            .expect("trace recorder registry mutex should not be poisoned");
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
            .u64_fields
            .lock()
            .expect("trace recorder u64 field log mutex should not be poisoned")
            .get(field)
            .cloned()
            .unwrap_or_default()
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
        let _registry = REGISTRY_LOCK
            .lock()
            .expect("trace recorder registry mutex should not be poisoned");
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
            let mut fields = self
                .state
                .u64_fields
                .lock()
                .expect("trace recorder u64 field log mutex should not be poisoned");
            for (field, value) in visitor.u64s {
                fields.entry(field).or_default().push(value);
            }
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
