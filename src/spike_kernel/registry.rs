//! The authoritative run bookkeeping (`ScopeRegistry`) and the run
//! lifecycle FSM (spec §2 row 5):
//! `Absent -> Running -> Stopping -> Draining -> removed`, with natural
//! finish skipping Draining when no committed pending remains. Removal is
//! always "exit observed and committed pending == 0" (§2.1 rule 5), and a
//! poisoned counter flips removal to "termination only" (rule 6).

use std::collections::BTreeMap;
use std::sync::Arc;

use tokio::task::AbortHandle;

use super::cmd::ScopePath;
use super::lane::{OriginGate, PendingCounter, RunToken};

/// Which producer species the entry tracks. All species share one FSM and
/// one `JoinSet` (spec §2 row 4).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RunKind {
    /// Cancellable keyed effect run; identity is `(scope, key)`.
    Keyed(&'static str),
    /// Anonymous effect run (scope-attributed, no logical key).
    Anon,
    /// Subscription forwarder; identity is `(scope, key)`.
    Sub(&'static str),
}

/// Lifecycle phase (Draining = exit observed with committed pending > 0).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    /// Live and deliverable.
    Running,
    /// Stop requested, exit not yet observed.
    Stopping,
    /// Exit observed, tombstone retained for committed envelopes.
    Draining,
}

/// One run's authoritative record.
pub struct RunEntry {
    /// Run identity token.
    pub token: RunToken,
    /// Ledger vocabulary for this run.
    pub label: &'static str,
    /// Producer species.
    pub kind: RunKind,
    /// Structural scope attribution.
    pub scope: ScopePath,
    /// Lifecycle phase.
    pub phase: Phase,
    /// Delivery-side filter flag: revoked envelopes drop at dequeue.
    pub revoked: bool,
    /// Whether the task's exit was observed (`JoinExit`).
    pub exited: bool,
    /// Committed-pending counter shared with the producer's reservations.
    pub counter: Arc<PendingCounter>,
    /// Send gate shared with the producer and the driver.
    pub gate: Arc<OriginGate>,
    /// Abort handle into the shared `JoinSet`.
    pub abort: AbortHandle,
}

impl RunEntry {
    /// Whether this entry participates in the uniform admission barrier:
    /// a stop-requested subscription run whose exit is unobserved.
    pub fn is_stopping_sub(&self) -> bool {
        matches!(self.kind, RunKind::Sub(_)) && self.phase == Phase::Stopping
    }

    /// Whether the entry satisfies the unified removal condition.
    fn removable(&self) -> bool {
        self.exited && !self.counter.is_poisoned() && self.counter.value() == 0
    }
}

/// Token-ordered registry; iteration order is spawn order, keeping ledger
/// output deterministic.
#[derive(Default)]
pub struct ScopeRegistry {
    entries: BTreeMap<RunToken, RunEntry>,
}

impl ScopeRegistry {
    /// Inserts a fresh `Running` entry.
    pub fn insert(&mut self, entry: RunEntry) {
        self.entries.insert(entry.token, entry);
    }

    /// Immutable lookup.
    pub fn get(&self, token: RunToken) -> Option<&RunEntry> {
        self.entries.get(&token)
    }

    /// Number of live entries (tombstones included).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Spawn-ordered iteration.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &RunEntry> {
        self.entries.values()
    }

    /// The single live (non-exited, non-stopping) entry labelled `label`,
    /// if any — the driver's producer lookup.
    pub fn live_by_label(&self, label: &str) -> Option<&RunEntry> {
        self.entries
            .values()
            .find(|e| e.label == label && e.phase == Phase::Running)
    }

    /// Stop request (cancel / replace / teardown / termination share this
    /// one transition): revoke, request abort, and move Running to
    /// Stopping. Exited tombstones are only revoked (their queued
    /// envelopes must still filter). Returns whether a live run was
    /// stopped.
    pub fn stop_request(&mut self, token: RunToken) -> bool {
        let Some(entry) = self.entries.get_mut(&token) else {
            return false;
        };
        entry.revoked = true;
        if entry.exited {
            return false;
        }
        entry.abort.abort();
        let was_running = entry.phase == Phase::Running;
        entry.phase = Phase::Stopping;
        was_running
    }

    /// Tokens whose scope path starts with `prefix` (teardown selection —
    /// all kinds, INV-ST1's successor domain).
    pub fn select_prefix(&self, prefix: &ScopePath) -> Vec<RunToken> {
        self.entries
            .values()
            .filter(|e| e.scope.starts_with(prefix))
            .map(|e| e.token)
            .collect()
    }

    /// The live keyed occupant of `(scope, key)`, honoring the fresh-slot
    /// rule (Stopping/Draining entries do not hold the slot, INV-ST7).
    pub fn keyed_occupant(&self, scope: &ScopePath, key: &str) -> Option<RunToken> {
        self.entries
            .values()
            .find(|e| {
                e.phase == Phase::Running
                    && e.scope == *scope
                    && matches!(e.kind, RunKind::Keyed(k) if k == key)
            })
            .map(|e| e.token)
    }

    /// Whether a live subscription run with `id` exists (admission input;
    /// Stopping/Draining entries do not hold the slot).
    pub fn sub_running(&self, scope: &ScopePath, key: &str) -> bool {
        self.entries.values().any(|e| {
            e.phase == Phase::Running
                && e.scope == *scope
                && matches!(e.kind, RunKind::Sub(k) if k == key)
        })
    }

    /// The uniform admission barrier predicate (subscription runs only).
    pub fn any_stopping_sub(&self) -> bool {
        self.entries.values().any(RunEntry::is_stopping_sub)
    }

    /// `JoinExit` observation: marks the exit, then applies the unified
    /// removal condition. Returns `(label, revoked)` of the observed run;
    /// `None` if the token is unknown (already removed — an invariant
    /// violation surfaced to the caller).
    pub fn on_exit(&mut self, token: RunToken) -> Option<(&'static str, bool)> {
        let entry = self.entries.get_mut(&token)?;
        entry.exited = true;
        let observed = (entry.label, entry.revoked);
        if entry.removable() {
            self.entries.remove(&token);
        } else if let Some(entry) = self.entries.get_mut(&token) {
            entry.phase = Phase::Draining;
        }
        Some(observed)
    }

    /// Dequeue-side decrement (rule 4; revoked drops included), followed
    /// by the removal condition — this is where tombstones are reclaimed.
    /// Returns `(label, revoked)`; panics on a missing entry because that
    /// is exactly the tombstone-lifetime invariant under test.
    pub fn on_dequeue(&mut self, token: RunToken) -> (&'static str, bool) {
        let entry = self
            .entries
            .get_mut(&token)
            .expect("tombstone invariant: dequeued envelope must have a registry entry");
        entry.counter.decrement();
        let observed = (entry.label, entry.revoked);
        if entry.removable() {
            self.entries.remove(&token);
        }
        observed
    }

    /// Termination: abort everything (idempotent for already-stopping
    /// entries); the registry is destroyed wholesale at settle.
    pub fn abort_all(&mut self) {
        for entry in self.entries.values_mut() {
            entry.revoked = true;
            if !entry.exited {
                entry.abort.abort();
                entry.phase = Phase::Stopping;
            }
        }
    }
}
