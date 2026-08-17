//! The authoritative run bookkeeping.
//!
//! One entry per producer run of every kind — keyed command, anonymous
//! command, subscription — in one map, driving one lifecycle FSM:
//!
//! ```text
//! absent -> Running -> Stopping -> Draining -> removed
//! ```
//!
//! A natural finish skips `Draining` when nothing is pending. Removal is
//! always "exit observed and committed pending is zero" (the accounting's
//! rule 5), and a poisoned counter flips removal to termination-only
//! (rule 6).
//!
//! One registry rather than one per kind is what makes teardown selection
//! total over kinds (RFC 0014 §4.1, RFC 0013 INV-ST1): a prefix match runs
//! over the same entries no matter which kind produced them.

use std::collections::BTreeMap;
use std::sync::Arc;

use tokio::task::AbortHandle;

use crate::command::CommandId;
use crate::structural_key::ScopePath;
use crate::subscription::SubscriptionId;

use super::accounting::PendingCounter;
use super::lane::{RunToken, SendGate};

/// Which producer species an entry tracks. All species share one FSM and one
/// join set, so there is no per-kind ownership path for teardown or
/// termination to miss.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunKind {
    /// A cancellable keyed effect run; its logical identity is the id.
    Keyed(CommandId),
    /// An anonymous effect run, addressed by scope alone (RFC 0014
    /// INV-RC7).
    Anon,
    /// A subscription forwarder. Only this kind joins the uniform admission
    /// barrier and only its quiescence can mark subscriptions dirty
    /// (RFC 0014 §5.1, §5.2).
    Sub(SubscriptionId),
}

/// Lifecycle phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    /// Live and deliverable; holds its identity slot.
    Running,
    /// Stop requested, exit not yet observed. Does not hold its slot, which
    /// is the fresh-slot half of RFC 0013 INV-ST7.
    Stopping,
    /// Exit observed, entry retained as a tombstone for committed
    /// envelopes still in a lane.
    Draining,
}

/// How one producer task ended, as the join set reports it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExitOutcome {
    /// Ran to completion.
    Completed,
    /// Abort observed.
    Cancelled,
    /// Panic contained as a join error (RFC 0011's producer panic class).
    Panicked,
}

/// What an exit observation reports to the pass's dirt decision.
///
/// The stop *cause* is not stored on the entry: a termination-stopped run
/// can only ever be observed while the kernel is terminating, so the
/// kernel's phase at observation time distinguishes the causes exactly
/// (RFC 0014 §5.2 excludes termination-driven quiescence from dirt).
pub struct ExitObservation {
    /// Whether a stop had been requested before the exit; `false` is a
    /// natural finish, which marks no dirt.
    pub stopped: bool,
    /// Producer species.
    pub kind: RunKind,
}

/// One run's authoritative record.
pub struct RunEntry {
    /// Run identity token, unique for the kernel's lifetime. A successor in
    /// the same identity slot gets a fresh one, which is what makes a
    /// predecessor's late exit and late sends inert (RFC 0014 §3.1's "no
    /// stale resurrection").
    pub token: RunToken,
    /// Producer species and logical identity.
    pub kind: RunKind,
    /// Structural scope attribution, the subject of prefix selection.
    pub scope: ScopePath,
    /// Lifecycle phase.
    pub phase: Phase,
    /// Delivery-side filter flag: a revoked run's envelopes are discarded
    /// at the delivery decision point (RFC 0014 INV-RC5).
    pub revoked: bool,
    /// Whether the task's exit has been observed.
    pub exited: bool,
    /// Committed-pending counter shared with this run's reservations.
    pub counter: Arc<PendingCounter>,
    /// Send gate shared with this run's producer and with the driver.
    pub gate: Arc<SendGate>,
    /// Abort handle into the shared join set.
    pub abort: AbortHandle,
}

impl RunEntry {
    /// Whether this entry participates in the uniform admission barrier: a
    /// stop-requested subscription run whose exit is unobserved (RFC 0014
    /// §5.1).
    pub fn is_stopping_sub(&self) -> bool {
        matches!(self.kind, RunKind::Sub(_)) && self.phase == Phase::Stopping
    }

    /// The unified removal condition (accounting rules 5 and 6).
    fn removable(&self) -> bool {
        self.exited && !self.counter.is_poisoned() && self.counter.value() == 0
    }
}

/// Token-ordered run bookkeeping.
///
/// A `BTreeMap` rather than a hash map because iteration order is spawn
/// order: selection results, stop sequences, and every observation sequence
/// the conformance series read are deterministic as a consequence.
#[derive(Default)]
pub struct ScopeRegistry {
    entries: BTreeMap<RunToken, RunEntry>,
}

impl ScopeRegistry {
    /// Inserts a fresh `Running` entry.
    pub fn insert(&mut self, _entry: RunEntry) {
        todo!("registry insert")
    }

    /// Immutable lookup.
    pub fn get(&self, _token: RunToken) -> Option<&RunEntry> {
        todo!("registry lookup")
    }

    /// Number of entries, tombstones included.
    pub fn len(&self) -> usize {
        todo!("registry size")
    }

    /// Spawn-ordered iteration.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &RunEntry> {
        self.entries.values()
    }

    /// The single transition every stop cause shares — explicit cancel,
    /// `CancelInFlight` supersession, scope teardown, and termination.
    ///
    /// `revoked` is set unconditionally, because the delivery-side filter
    /// must apply to a tombstone's queued envelopes too; an already-exited
    /// entry is only revoked, with no abort and no phase change. Returns
    /// whether a live `Running` run was stopped, which is the input to the
    /// stopping-pass defer rule (RFC 0014 §5.3).
    pub fn stop_request(&mut self, _token: RunToken) -> bool {
        todo!("single stop transition")
    }

    /// Every run whose scope path begins with `prefix`, of every kind —
    /// teardown's selection (RFC 0014 §4.1, RFC 0013 INV-ST1).
    ///
    /// Whether this walks or indexes is mechanism (RFC 0013 §3.7); what is
    /// contract is that the match is a complete-prefix comparison from the
    /// root over structural segment equality, so shorter, reordered,
    /// subset, and deeper-position paths are not selected.
    pub fn select_prefix(&self, _prefix: &ScopePath) -> Vec<RunToken> {
        todo!("prefix selection")
    }

    /// The live keyed occupant of `id`, honoring the fresh-slot rule:
    /// `Stopping` and `Draining` entries do not hold the slot (RFC 0013
    /// INV-ST7).
    pub fn keyed_occupant(&self, _id: &CommandId) -> Option<RunToken> {
        todo!("keyed slot lookup")
    }

    /// Whether a live subscription run with `id` exists — the admission
    /// input, under the same fresh-slot rule.
    pub fn sub_running(&self, _id: &SubscriptionId) -> bool {
        todo!("subscription slot lookup")
    }

    /// The uniform admission barrier predicate, over subscription runs only
    /// (RFC 0014 §5.1).
    pub fn any_stopping_sub(&self) -> bool {
        self.entries.values().any(RunEntry::is_stopping_sub)
    }

    /// Exit observation: marks the exit, then applies the removal
    /// condition, retaining the entry as a `Draining` tombstone when
    /// committed envelopes remain. `None` when the token is unknown.
    pub fn on_exit(&mut self, _token: RunToken) -> Option<ExitObservation> {
        todo!("exit reflection")
    }

    /// Dequeue-side decrement (accounting rule 4, a revoked discard
    /// included), then the removal condition — this is where tombstones are
    /// reclaimed. Returns whether the origin was revoked, which is the
    /// delivery decision itself.
    ///
    /// Panics on a missing entry: an envelope in a lane whose origin has no
    /// entry is exactly the tombstone-lifetime invariant failing, and
    /// asserting it here keeps it checked in every test run rather than in
    /// the tests that happen to look.
    pub fn on_dequeue(&mut self, _token: RunToken) -> bool {
        todo!("delivery decision and reclamation")
    }

    /// Termination: revoke and abort everything, idempotent for entries
    /// already stopping (RFC 0011 §4.4's immediate postcondition).
    pub fn abort_all(&mut self) {
        todo!("termination sweep")
    }
}
