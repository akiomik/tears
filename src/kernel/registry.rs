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
//!
//! Each rule the FSM applies is written once, as a free function over the
//! facts it reads, and every method below delegates to it. The rules are
//! what the RFCs pin; the map is mechanism (RFC 0013 §3.7), and keeping them
//! apart is what lets the rules be checked directly.

use std::collections::BTreeMap;
use std::sync::Arc;

use tokio::task::AbortHandle;

use crate::command::CommandId;
use crate::runtime::load::LoadObserver;
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
///
/// The phase alone does not decide identity occupancy — [`holds_keyed_slot`]
/// does, and it reads the revocation flag beside the phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Phase {
    /// Live and deliverable; holds its identity slot.
    Running,
    /// Stop requested, exit not yet observed. Does not hold its slot, which
    /// is the fresh-slot half of RFC 0013 INV-ST7.
    Stopping,
    /// Exit observed, entry retained as a tombstone for committed
    /// envelopes still in a lane.
    ///
    /// Reached by two different runs: one that was stopped, whose queued
    /// envelopes are now filtered at their dequeue, and one that finished on
    /// its own, whose queued envelopes are still deliverable and whose
    /// identity is therefore still occupied.
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

/// The removal condition, over the three facts it reads (the accounting's
/// rules 5 and 6).
///
/// The rule is uniform over exit causes: a panicking exit is removed by the
/// same condition as a completed one, and a `pending > 0` exit of either
/// cause becomes a `Draining` tombstone instead. A poisoned counter never
/// satisfies it, which is what flips a saturated run's removal to
/// termination-only rather than letting its entry be reclaimed while
/// envelopes it owns are still in a lane.
const fn removal_rule(exited: bool, poisoned: bool, pending: u32) -> bool {
    exited && !poisoned && pending == 0
}

/// Whether an entry still holds its keyed identity slot.
///
/// Occupancy is **deliverability**, not liveness: what frees a slot is the
/// revocation, and a run that finished on its own with output still queued
/// stays deliverable and therefore still holds its id (RFC 0003 §6.1's
/// successor statements for INV-2 — "at most one run per `CommandId` is
/// deliverable" — INV-5 — "occupancy is read from the run's deliverability"
/// — INV-6, and INV-7: an identity whose run has finished *and* whose output
/// has drained is free, which on this registry is the entry being gone).
///
/// The four reachable combinations:
///
/// | phase | revoked | holds the slot |
/// | --- | --- | --- |
/// | `Running` | no | yes |
/// | `Stopping` | yes | no |
/// | `Draining` | no (natural finish, output queued) | yes |
/// | `Draining` | yes (stopped, output being filtered) | no |
///
/// `Stopping` implies `revoked`, because the single stop transition sets the
/// flag before it advances the phase; naming the phase here keeps that from
/// being load-bearing, so a future path that stops without revoking cannot
/// silently hand a live run's slot away.
const fn holds_keyed_slot(phase: Phase, revoked: bool) -> bool {
    !revoked && !matches!(phase, Phase::Stopping)
}

/// The effect one stop request has on one entry, decided before it is
/// applied.
///
/// The revocation itself is not a case: it is unconditional, so it is not
/// part of the decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StopEffect {
    /// The task's exit is already observed. There is nothing to abort and
    /// no phase to advance — a tombstone is only revoked, so that the
    /// envelopes it still owns are filtered at their dequeue.
    RevokeOnly,
    /// The task is still executing: request the abort and move to
    /// `Stopping`. `was_running` is `true` only for the first stop, which
    /// is what distinguishes a stop this pass issued from a repeat.
    Stop {
        /// Whether this request is what took the run out of `Running`.
        was_running: bool,
    },
}

/// The single stop transition, shared by every stop cause — explicit
/// cancel, `CancelInFlight` supersession, scope teardown, and termination.
const fn stop_effect(exited: bool, phase: Phase) -> StopEffect {
    if exited {
        StopEffect::RevokeOnly
    } else {
        StopEffect::Stop {
            was_running: matches!(phase, Phase::Running),
        }
    }
}

/// Teardown's selection predicate (RFC 0013 §3.1, INV-ST1).
///
/// A complete-prefix comparison from the path root over structural segment
/// equality: a path equal to the prefix is selected, and shorter, reordered,
/// subset, and deeper-position paths are not. The run's *kind* and its local
/// key are not arguments, which is how "uniform across every run kind" and
/// "local keys never participate in selection" are structural here rather
/// than asserted per kind.
fn selected_by(scope: &ScopePath, prefix: &ScopePath) -> bool {
    scope.starts_with(prefix)
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

    /// Whether this entry is a keyed run still holding `id`'s slot.
    fn occupies_keyed_slot(&self, id: &CommandId) -> bool {
        holds_keyed_slot(self.phase, self.revoked)
            && matches!(&self.kind, RunKind::Keyed(keyed) if keyed == id)
    }

    /// Whether this entry is a live subscription run for `id`.
    ///
    /// Liveness rather than deliverability, and deliberately not the keyed
    /// rule: a subscription run that finished on its own is *absent* for
    /// admission purposes, because a finished, still-declared subscription
    /// restarts at the next re-evaluation (RFC 0014 §5.2, RFC 0005 INV-13
    /// through RFC 0012 §4.3). Its queued output stays deliverable all the
    /// same — that is the entry's business, not the slot's.
    fn is_running_sub(&self, id: &SubscriptionId) -> bool {
        self.phase == Phase::Running && matches!(&self.kind, RunKind::Sub(sub) if sub == id)
    }

    /// The unified removal condition (accounting rules 5 and 6).
    fn removable(&self) -> bool {
        removal_rule(
            self.exited,
            self.counter.is_poisoned(),
            self.counter.value(),
        )
    }
}

/// Token-ordered run bookkeeping.
///
/// A `BTreeMap` rather than a hash map because iteration order is spawn
/// order: selection results, stop sequences, and every observation sequence
/// the conformance series read are deterministic as a consequence.
pub struct ScopeRegistry {
    entries: BTreeMap<RunToken, RunEntry>,
    /// How many entries are keyed runs, maintained in step with `entries`.
    ///
    /// A rescan per membership change would make the gauge publish `O(n)` on
    /// a path the dequeue side takes once per envelope. The count is a
    /// derived fact and therefore a second source of truth, so debug builds
    /// check it against the rescan it replaces.
    keyed: usize,
    observer: LoadObserver,
}

impl ScopeRegistry {
    /// Builds an empty registry publishing to `observer`.
    ///
    /// The observer is the kernel's own: the `keyed_commands` gauge is
    /// published from here and nowhere else, so a registry built with a
    /// throwaway observer reports to nobody.
    pub const fn new(observer: LoadObserver) -> Self {
        Self {
            entries: BTreeMap::new(),
            keyed: 0,
            observer,
        }
    }

    /// Inserts a fresh `Running` entry.
    pub fn insert(&mut self, entry: RunEntry) {
        self.admit(entry);
    }

    /// Immutable lookup.
    pub fn get(&self, token: RunToken) -> Option<&RunEntry> {
        self.entries.get(&token)
    }

    /// Number of entries, tombstones included.
    pub fn len(&self) -> usize {
        self.entries.len()
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
    pub fn stop_request(&mut self, token: RunToken) -> bool {
        let Some(entry) = self.entries.get_mut(&token) else {
            return false;
        };
        entry.revoked = true;
        match stop_effect(entry.exited, entry.phase) {
            StopEffect::RevokeOnly => false,
            StopEffect::Stop { was_running } => {
                entry.abort.abort();
                entry.phase = Phase::Stopping;
                was_running
            }
        }
    }

    /// Every run whose scope path begins with `prefix`, of every kind —
    /// teardown's selection (RFC 0014 §4.1, RFC 0013 INV-ST1).
    ///
    /// Whether this walks or indexes is mechanism (RFC 0013 §3.7); what is
    /// contract is that the match is a complete-prefix comparison from the
    /// root over structural segment equality, so shorter, reordered,
    /// subset, and deeper-position paths are not selected. The walk's cost
    /// in the number of live runs is part of that mechanism and is not
    /// pinned here: where it matters is the load acceptance RFC 0014 §13.5
    /// re-derives on this topology, under RFC 0006's ownership, and a wider
    /// index is a change this signature already admits.
    ///
    /// Tombstones are selected like any other entry: a run that finished
    /// with output still queued is exactly the case INV-ST4 names, and
    /// revoking it is what retracts that output.
    pub fn select_prefix(&self, prefix: &ScopePath) -> Vec<RunToken> {
        self.entries
            .values()
            .filter(|entry| selected_by(&entry.scope, prefix))
            .map(|entry| entry.token)
            .collect()
    }

    /// The keyed occupant of `id`, honoring the fresh-slot rule: a revoked
    /// run has released its slot at its revocation's application point
    /// (RFC 0013 INV-ST7), while a naturally finished run holds it until
    /// its queued output has drained (RFC 0003 §6.1's INV-6/INV-7).
    pub fn keyed_occupant(&self, id: &CommandId) -> Option<RunToken> {
        let occupant = self
            .entries
            .values()
            .find(|entry| entry.occupies_keyed_slot(id));

        // The uniqueness the lookup assumes, checked rather than assumed:
        // one id has at most one deliverable run (RFC 0003 §6.1, INV-2's
        // successor). The release path keeps its early exit.
        #[cfg(debug_assertions)]
        {
            let deliverable = self
                .entries
                .values()
                .filter(|entry| entry.occupies_keyed_slot(id))
                .count();
            debug_assert!(
                deliverable <= 1,
                "at most one run per CommandId is deliverable (RFC 0003 §6.1, INV-2's successor)"
            );
        }

        occupant.map(|entry| entry.token)
    }

    /// Whether a live subscription run with `id` exists — the admission
    /// input.
    pub fn sub_running(&self, id: &SubscriptionId) -> bool {
        self.entries.values().any(|entry| entry.is_running_sub(id))
    }

    /// The uniform admission barrier predicate, over subscription runs only
    /// (RFC 0014 §5.1).
    pub fn any_stopping_sub(&self) -> bool {
        self.entries.values().any(RunEntry::is_stopping_sub)
    }

    /// Exit observation: marks the exit, then applies the removal
    /// condition, retaining the entry as a `Draining` tombstone when
    /// committed envelopes remain. `None` when the token is unknown.
    pub fn on_exit(&mut self, token: RunToken) -> Option<ExitObservation> {
        let entry = self.entries.get_mut(&token)?;
        entry.exited = true;
        let observed = ExitObservation {
            stopped: entry.revoked,
            kind: entry.kind.clone(),
        };
        let reclaimed = entry.removable();
        if !reclaimed {
            entry.phase = Phase::Draining;
        }
        if reclaimed {
            self.retire(token);
        }
        Some(observed)
    }

    /// Dequeue-side decrement (accounting rule 4, a revoked discard
    /// included), then the removal condition — this is where tombstones are
    /// reclaimed. Returns whether the origin was revoked, which is the
    /// delivery decision itself.
    ///
    /// # Panics
    ///
    /// Panics on a missing entry: an envelope in a lane whose origin has no
    /// entry is exactly the tombstone-lifetime invariant failing, and
    /// asserting it here keeps it checked in every test run rather than in
    /// the tests that happen to look.
    pub fn on_dequeue(&mut self, token: RunToken) -> bool {
        let entry = self
            .entries
            .get_mut(&token)
            .expect("tombstone invariant: a dequeued envelope's origin must have a registry entry");
        entry.counter.decrement();
        let revoked = entry.revoked;
        let reclaimed = entry.removable();
        if reclaimed {
            self.retire(token);
        }
        revoked
    }

    /// Termination: revoke and abort everything, idempotent for entries
    /// already stopping (RFC 0011 §4.4's immediate postcondition).
    ///
    /// Termination is the fourth stop cause and takes the same transition
    /// as the other three, applied to every entry rather than to a
    /// selection.
    pub fn abort_all(&mut self) {
        let tokens: Vec<RunToken> = self.entries.keys().copied().collect();
        for token in tokens {
            self.stop_request(token);
        }
    }

    /// Settle-time teardown: drops every entry and publishes the resulting
    /// zero count.
    ///
    /// This is the one place a registry is emptied without the removal
    /// condition, and it is sound only where it is called. After the
    /// immediate postcondition no envelope can be dequeued, so no tombstone
    /// can ever be reclaimed again and the removal condition would hold the
    /// bookkeeping — and the gauge with it — non-zero for the rest of the
    /// kernel's life. The quiescent postcondition is where that bookkeeping
    /// ends (RFC 0011 INV-LC7).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.keyed = 0;
        self.publish_keyed_gauge();
    }

    /// Membership insertion — half of the registry's single mutation point.
    fn admit(&mut self, entry: RunEntry) {
        let keyed = matches!(entry.kind, RunKind::Keyed(_));
        if let Some(replaced) = self.entries.insert(entry.token, entry) {
            // A token is minted once per run, so nothing is ever replaced;
            // keeping the count exact costs one branch and does not rest on
            // that.
            self.keyed -= usize::from(matches!(replaced.kind, RunKind::Keyed(_)));
        }
        self.keyed += usize::from(keyed);
        self.publish_keyed_gauge();
    }

    /// Membership removal — the other half.
    fn retire(&mut self, token: RunToken) {
        if let Some(removed) = self.entries.remove(&token) {
            self.keyed -= usize::from(matches!(removed.kind, RunKind::Keyed(_)));
        }
        self.publish_keyed_gauge();
    }

    /// Publishes the `keyed_commands` gauge.
    ///
    /// The gauge stays count-based rather than becoming a per-task RAII
    /// guard, because a keyed entry's lifetime is the runtime's and not its
    /// task's — a draining entry outlives its task, so there is no
    /// task-scoped guard that could carry it. Its publish site is therefore
    /// the registry's membership mutation point, and only that: every
    /// change to the count passes through [`admit`](Self::admit),
    /// [`retire`](Self::retire), or [`clear`](Self::clear) — `entries` is
    /// private and no method hands out a mutable entry — and
    /// `set_keyed_entries` re-emits only when the value actually moved.
    fn publish_keyed_gauge(&self) {
        debug_assert_eq!(
            self.keyed,
            self.entries
                .values()
                .filter(|entry| matches!(entry.kind, RunKind::Keyed(_)))
                .count(),
            "the maintained keyed count and the entries it derives from have diverged"
        );
        self.observer.set_keyed_entries(self.keyed);
    }
}

impl Drop for ScopeRegistry {
    /// Publishes the `keyed_commands` gauge returning to zero.
    ///
    /// Nothing else lowers it on a drop without a settle — a render error
    /// propagating out of the driving loop, or a caller dropping the run
    /// future mid-run (RFC 0011 INV-LC6's first cause). The guard-backed
    /// gauges fall on their own when the join set dismantles the task
    /// futures; a count-based one cannot, and INV-LC7 requires every
    /// producer gauge to read zero. This is the successor of the same
    /// defense on the superseded keyed bookkeeping, kept for the same
    /// reason. `set_keyed_entries` re-emits only on a change, so a
    /// clear-then-drop pair emits once.
    fn drop(&mut self) {
        self.observer.set_keyed_entries(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::hash::{Hash, Hasher};

    fn path(segments: &[&'static str]) -> ScopePath {
        // Root-first storage: the *last* `prefixed` call names the outermost
        // segment, so the slice reads root-first when applied in reverse.
        segments
            .iter()
            .rev()
            .fold(ScopePath::empty(), |acc, segment| acc.prefixed(*segment))
    }

    // Rule 5 and rule 6 (the accounting's), stated once and read by every
    // removal site.

    #[test]
    fn a_run_is_removed_only_once_it_has_exited_with_nothing_pending() {
        assert!(removal_rule(true, false, 0), "exited and drained");
        assert!(!removal_rule(false, false, 0), "still executing");
        assert!(
            !removal_rule(true, false, 1),
            "committed envelope in a lane"
        );
    }

    #[test]
    fn a_poisoned_counter_moves_removal_to_termination_only() {
        assert!(
            !removal_rule(true, true, 0),
            "a poisoned counter never satisfies the removal condition"
        );
    }

    // The single stop transition (verbatim rule 7): revocation is
    // unconditional and therefore not a case; an exited entry is not aborted
    // and does not change phase.

    #[test]
    fn stopping_a_live_run_aborts_it_and_reports_the_first_stop() {
        assert_eq!(
            stop_effect(false, Phase::Running),
            StopEffect::Stop { was_running: true }
        );
    }

    #[test]
    fn repeating_a_stop_still_transitions_but_reports_no_live_stop() {
        assert_eq!(
            stop_effect(false, Phase::Stopping),
            StopEffect::Stop { was_running: false }
        );
    }

    #[test]
    fn stopping_a_tombstone_only_revokes_it() {
        assert_eq!(stop_effect(true, Phase::Draining), StopEffect::RevokeOnly);
        assert_eq!(stop_effect(true, Phase::Stopping), StopEffect::RevokeOnly);
    }

    // The fresh-slot rule (RFC 0013 INV-ST7) and its deliverability half
    // (RFC 0003 §6.1's INV-2/INV-5/INV-6/INV-7 successors).

    #[test]
    fn a_running_run_holds_its_slot() {
        assert!(holds_keyed_slot(Phase::Running, false));
    }

    #[test]
    fn a_revoked_run_releases_its_slot_at_the_application_point() {
        assert!(
            !holds_keyed_slot(Phase::Stopping, true),
            "a stop-requested run's successor observes a fresh slot"
        );
        assert!(
            !holds_keyed_slot(Phase::Draining, true),
            "a revoked tombstone's output is retracted, so it holds nothing"
        );
    }

    #[test]
    fn a_naturally_finished_run_holds_its_slot_until_its_output_drains() {
        assert!(
            holds_keyed_slot(Phase::Draining, false),
            "finishing is not revocation: the queued output is still deliverable"
        );
    }

    #[test]
    fn a_stopping_phase_releases_the_slot_even_without_the_flag() {
        // Unreachable through `stop_request`, which revokes first; asserted
        // so the phase stays load-bearing if a future path forgets the flag.
        assert!(!holds_keyed_slot(Phase::Stopping, false));
    }

    // Selection (RFC 0013 §7.2's selection unit tests).

    #[test]
    fn a_path_equal_to_the_prefix_is_selected() {
        assert!(selected_by(&path(&["pane-1"]), &path(&["pane-1"])));
    }

    #[test]
    fn a_deeper_path_under_the_prefix_is_selected() {
        assert!(selected_by(&path(&["pane-1", "field"]), &path(&["pane-1"])));
    }

    #[test]
    fn a_shorter_path_is_not_selected() {
        assert!(!selected_by(
            &path(&["pane-1"]),
            &path(&["pane-1", "field"])
        ));
    }

    #[test]
    fn a_root_spawned_anonymous_run_is_never_selected() {
        assert!(!selected_by(&ScopePath::empty(), &path(&["pane-1"])));
    }

    #[test]
    fn the_prefix_segments_deeper_in_the_path_are_not_selected() {
        assert!(!selected_by(
            &path(&["outer", "pane-1"]),
            &path(&["pane-1"])
        ));
    }

    #[test]
    fn reordered_prefix_segments_are_not_selected() {
        assert!(!selected_by(
            &path(&["pane-1", "field"]),
            &path(&["field", "pane-1"])
        ));
    }

    #[test]
    fn a_subset_of_the_prefix_segments_is_not_selected() {
        assert!(!selected_by(
            &path(&["pane-1", "field"]),
            &path(&["pane-1", "row", "field"])
        ));
    }

    #[test]
    fn a_sibling_scope_is_not_selected() {
        assert!(!selected_by(&path(&["pane-2"]), &path(&["pane-1"])));
    }

    #[test]
    fn constant_hash_scope_values_are_still_separated_structurally() {
        #[derive(Eq, PartialEq)]
        struct Collision(u8);

        impl Hash for Collision {
            fn hash<H: Hasher>(&self, state: &mut H) {
                0_u8.hash(state);
            }
        }

        let first = ScopePath::empty().prefixed(Collision(1));
        let second = ScopePath::empty().prefixed(Collision(2));

        assert!(selected_by(&first, &first));
        assert!(
            !selected_by(&first, &second),
            "selection compares structural values, not hashes"
        );
    }

    #[test]
    fn every_path_is_selected_by_its_own_prefixes() {
        let deep = path(&["pane-1", "row-2", "field"]);

        assert!(selected_by(&deep, &path(&["pane-1"])));
        assert!(selected_by(&deep, &path(&["pane-1", "row-2"])));
        assert!(selected_by(&deep, &deep));
    }
}
