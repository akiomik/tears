use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tracing::callsite::rebuild_interest_cache;
use tracing::field::{Field, Visit};
use tracing::metadata::LevelFilter;
use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::{DefaultGuard, set_default};
use tracing::{Event, Level, Metadata, Subscriber};

// Intentionally duplicated with `tests/common/trace_recorder.rs`. A shared
// workspace-only crate made publish/tarball behavior less intuitive for this
// small helper, and a feature-gated public test API would add test invocation
// constraints. Unit and integration tests keep local copies for now.

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
}

impl Drop for TraceRecorderGuard {
    fn drop(&mut self) {
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

        let mut visitor = BoolFieldVisitor::default();
        event.record(&mut visitor);
        if visitor.values.is_empty() {
            return;
        }

        let mut fields = self
            .state
            .bool_fields
            .lock()
            .expect("trace recorder bool field log mutex should not be poisoned");
        for (field, value) in visitor.values {
            fields.entry(field).or_default().push(value);
        }
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

#[derive(Default)]
struct BoolFieldVisitor {
    values: Vec<(String, bool)>,
}

impl Visit for BoolFieldVisitor {
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.values.push((field.name().to_owned(), value));
    }

    fn record_debug(&mut self, _field: &Field, _value: &dyn Debug) {}
}
