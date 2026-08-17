//! Command lowering: one `Command` to the kernel's phase buckets.
//!
//! The command layer owns the decomposition
//! ([`RuntimeCommandParts::into_kernel_parts`]); this module is where the
//! kernel reads it, and where the two shapes RFC 0014 §3.4 declares **not
//! constructible** in the new API are asserted against until the API
//! shape that makes them unconstructible lands:
//!
//! - *Keying a batch.* "A spawn key attaches to a single effect carrier
//!   only." Today's `cancellable` is a `Command` method, so
//!   `Command::batch([..]).cancellable(id)` builds — one key reaching
//!   several carriers, which would open the same identity twice in one
//!   dispatch, the second replacing the first under its policy.
//! - *Keying a quit.* An `update`-returned quit applies synchronously at
//!   the dispatch's completion (RFC 0014 §3.3), so there is no run for a
//!   key to name and nothing for a later cancel to suppress.
//!
//! Both are `debug_assert!`s rather than errors on purpose: they are
//! placeholders for a type-level split, and turning a constructible public
//! call into a release-mode panic would be the worse of the two failures.
//!
//! [`RuntimeCommandParts::into_kernel_parts`]: crate::command::RuntimeCommandParts::into_kernel_parts

use crate::command::{
    CancelPolicy, CancellableCommand, CommandId, KernelParts, RuntimeCommandParts, SpawnEntry,
};
use crate::structural_key::ScopePath;

use super::lane::RunToken;
use super::registry::ScopeRegistry;

/// Lowers one command into the phase buckets a dispatch applies: the cancel
/// phase (explicit cancels and teardown prefixes) before every spawn of the
/// same command, then the spawn phase in declaration order (RFC 0014 §3.4).
///
/// **A spawn's scope and its key's scope are two paths, not one.** The
/// spawn's `scope` is where the run is *placed* — what a prefix teardown
/// selects it by (RFC 0014 §4.1) — and the key's own scope path is part of
/// the *cancel identity* a boundary qualifies (RFC 0005 §4.3). One
/// `Command::scoped` call qualifies both, so the common shapes make them
/// coincide, but nothing requires it and the surface deliberately admits a
/// shape where they diverge: `work.scoped(s).cancellable(id)` places the run
/// under `s` while giving it a root-global key, which
/// [`Command::scoped`](crate::command::Command::scoped) documents as an
/// intentional composition — a scoped effect participating in an
/// application-wide slot. Such a run is reachable from both directions, by
/// the prefix and by the id, and neither reading is derived from the other.
pub fn lower<Msg: Send + 'static>(parts: RuntimeCommandParts<Msg>) -> KernelParts<Msg> {
    #[cfg(debug_assertions)]
    if let Some((effect_carriers, quit_carriers)) = parts.command_key_reach() {
        debug_assert!(
            effect_carriers <= 1,
            "a spawn key attaches to a single effect carrier only; keying a batch is not a \
             lowering shape (RFC 0014 §3.4)"
        );
        debug_assert_eq!(
            quit_carriers, 0,
            "an update-returned quit applies synchronously at its dispatch, so a spawn key \
             names no run (RFC 0014 §3.3)"
        );
    }

    parts.into_kernel_parts()
}

/// One step of a dispatch.
///
/// The phase order RFC 0014 §3.4 pins is a property of the *sequence* these
/// come in, not of any one entry, so it is expressed as a sequence rather
/// than as buckets a caller is trusted to read in the right order.
pub enum DispatchStep<Msg: Send + 'static> {
    /// Cancel phase: revoke the run holding one keyed id.
    Cancel(CommandId),
    /// Cancel phase: revoke every run under one scope prefix.
    Teardown(ScopePath),
    /// Spawn phase: start one producer run.
    Spawn(SpawnEntry<Msg>),
    /// The dispatch's completion: apply the `update`-returned quit.
    Quit,
}

/// One lowered command as the ordered sequence a dispatch applies.
pub struct DispatchPlan<Msg: Send + 'static> {
    /// Whether the command asks for a redraw (RFC 0011 INV-LC1's mark).
    ///
    /// Not a step: a redraw is a mark the pass's frame stage consumes, not
    /// a position in the dispatch.
    pub redraw: bool,
    /// Every step the command carries, in application order.
    ///
    /// One kind of entry is not here yet: cleanup registrations, which
    /// RFC 0014 §3.4 places in the spawn phase — after the same command's
    /// cancel phase, so a teardown-and-reregister command consumes the old
    /// hooks and leaves the new registration armed. They land with the
    /// cleanup run kind, as a variant of [`DispatchStep`] ordered with the
    /// spawns.
    pub steps: Vec<DispatchStep<Msg>>,
}

/// Orders one lowered command's entries into the sequence a dispatch
/// applies (RFC 0014 §3.4):
///
/// 1. the **cancel phase** — the command's explicit cancels and its teardown
///    prefixes — which precedes *every* spawn of the same command, batch
///    children included (RFC 0013 INV-ST3);
/// 2. the **spawn phase**, in the command's flattened declaration order, so
///    two same-key spawns in one batch apply as two consecutive dispatches
///    and the second is a replacement under its own policy;
/// 3. the **quit**, if the command carried one, at the dispatch's
///    *completion* — siblings this same command spawned exist by then and
///    are torn down by termination rather than skipped (RFC 0014 §3.3).
///
/// Cancels and teardowns commute, so their relative order carries no
/// meaning: both are strict, idempotent revocations, and a prefix covering
/// an explicitly cancelled id applies the same removal twice.
pub fn dispatch_plan<Msg: Send + 'static>(parts: KernelParts<Msg>) -> DispatchPlan<Msg> {
    let KernelParts {
        redraw,
        quit_now,
        cancels,
        teardowns,
        spawns,
    } = parts;

    let steps = cancels
        .into_iter()
        .map(DispatchStep::Cancel)
        .chain(teardowns.into_iter().map(DispatchStep::Teardown))
        .chain(spawns.into_iter().map(DispatchStep::Spawn))
        .chain(quit_now.then_some(DispatchStep::Quit))
        .collect();

    DispatchPlan { redraw, steps }
}

/// What the spawn phase does with one lowered spawn entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpawnDecision {
    /// Start the run: the entry is anonymous, or its keyed slot is free.
    Start,
    /// A `CancelInFlight` spawn over an occupied slot: stop the occupant —
    /// which revokes its queued output — and start the successor.
    Replace(RunToken),
    /// A `KeepInFlight` spawn over an occupied slot: the new stream is
    /// discarded and nothing is started (RFC 0003 INV-5).
    Suppress,
}

/// The keyed slot policy, over the occupant the registry reports.
///
/// This is RFC 0003's `Spawn` transition on the new bookkeeping. Its
/// correspondence with the old pure state machine — `lifecycle_transition`
/// in `runtime::keyed_commands` — is exact once "occupant" is read as the
/// old machine reads it: a run that finished with output still queued
/// occupies its id, so *its* row is `Replace`/`Suppress` and not `Start`.
/// The registry's occupancy predicate is what supplies that reading (its
/// `holds_keyed_slot`), and the correspondence is pinned by the tests below.
const fn keyed_slot_decision(occupant: Option<RunToken>, policy: CancelPolicy) -> SpawnDecision {
    match (occupant, policy) {
        (None, _) => SpawnDecision::Start,
        (Some(token), CancelPolicy::CancelInFlight) => SpawnDecision::Replace(token),
        (Some(_), CancelPolicy::KeepInFlight) => SpawnDecision::Suppress,
    }
}

/// What to do with one spawn entry, read against the current registry.
///
/// An entry with no key is always started: an anonymous run has no logical
/// identity to contend for, only a scope attribution (RFC 0014 INV-RC7).
pub fn spawn_decision(registry: &ScopeRegistry, key: Option<&CancellableCommand>) -> SpawnDecision {
    key.map_or(SpawnDecision::Start, |key| {
        keyed_slot_decision(registry.keyed_occupant(&key.id), key.policy)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::command::Command;
    use crate::runtime::load::LoadObserver;

    fn lowered(command: Command<i32>) -> KernelParts<i32> {
        lower(command.into_runtime_parts())
    }

    fn planned(command: Command<i32>) -> Vec<&'static str> {
        dispatch_plan(lowered(command))
            .steps
            .iter()
            .map(|step| match *step {
                DispatchStep::Cancel(_) => "cancel",
                DispatchStep::Teardown(_) => "teardown",
                DispatchStep::Spawn(_) => "spawn",
                DispatchStep::Quit => "quit",
            })
            .collect()
    }

    #[test]
    fn a_keyed_effect_lowers_to_one_keyed_entry() {
        let parts = lowered(Command::message(1).cancellable(CommandId::new("load")));

        assert_eq!(parts.spawns.len(), 1);
        assert_eq!(
            parts.spawns[0].key.as_ref().expect("keyed").id,
            CommandId::new("load")
        );
    }

    #[test]
    fn batch_children_lower_to_independent_entries() {
        let parts = lowered(Command::batch([
            Command::message(1).cancellable(CommandId::new("left")),
            Command::message(2),
            Command::message(3).cancellable(CommandId::new("right")),
        ]));

        assert_eq!(parts.spawns.len(), 3);
    }

    // RFC 0014 §3.4: two same-key spawns in one batch are a *legal* shape —
    // they apply in declaration order as two consecutive dispatches, the
    // second replacing the first under its policy. The placeholders must not
    // mistake this for a keyed batch.
    #[test]
    fn two_same_key_spawns_in_one_batch_lower_cleanly() {
        let parts = lowered(Command::batch([
            Command::message(1).cancellable(CommandId::new("slot")),
            Command::message(2)
                .cancellable_with(CommandId::new("slot"), CancelPolicy::KeepInFlight),
        ]));

        assert_eq!(parts.spawns.len(), 2);
    }

    #[test]
    fn a_scoped_batch_lowers_with_every_carrier_under_the_boundary() {
        let parts = lowered(
            Command::batch([
                Command::message(1).cancellable(CommandId::new("load")),
                Command::message(2),
            ])
            .scoped("pane-1"),
        );
        let boundary = ScopePath::empty().prefixed("pane-1");

        assert!(parts.spawns.iter().all(|spawn| spawn.scope == boundary));
    }

    #[test]
    fn a_teardown_lowers_to_a_cancel_phase_entry_and_no_spawn() {
        let parts = lowered(Command::teardown("pane-1"));

        assert_eq!(parts.teardowns.len(), 1);
        assert!(parts.spawns.is_empty());
    }

    #[test]
    fn an_update_returned_quit_lowers_synchronously() {
        let parts = lowered(Command::quit());

        assert!(parts.quit_now);
        assert!(parts.spawns.is_empty());
    }

    // A quit *inside* a batch is a legal shape: it applies at the dispatch's
    // completion and its already-spawned siblings are then torn down by
    // termination (RFC 0014 §3.4).
    #[test]
    fn a_quit_beside_a_keyed_sibling_lowers_cleanly() {
        let parts = lowered(Command::batch([
            Command::quit(),
            Command::message(1).cancellable(CommandId::new("load")),
        ]));

        assert!(parts.quit_now);
        assert_eq!(parts.spawns.len(), 1);
    }

    // The two shapes RFC 0014 §3.4 declares not constructible. They still
    // build through today's `Command` surface, so the placeholders are what
    // reports them until the constructors are split.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "keying a batch")]
    fn keying_a_batch_trips_the_placeholder() {
        let _ = lowered(
            Command::batch([Command::message(1), Command::message(2)])
                .cancellable(CommandId::new("whole")),
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "names no run")]
    fn keying_a_quit_trips_the_placeholder() {
        let _ = lowered(Command::quit().cancellable(CommandId::new("whole")));
    }

    // The dispatch order (RFC 0014 §3.4).

    #[test]
    fn the_cancel_phase_precedes_every_spawn_of_the_same_command() {
        let steps = planned(Command::batch([
            Command::message(1).cancellable(CommandId::new("left")),
            Command::cancel(CommandId::new("other")),
            Command::message(2).cancellable(CommandId::new("right")),
            Command::teardown("pane-1"),
        ]));

        let last_cancel_phase = steps
            .iter()
            .rposition(|step| matches!(*step, "cancel" | "teardown"))
            .expect("the command carries a cancel phase");
        let first_spawn = steps
            .iter()
            .position(|step| *step == "spawn")
            .expect("the command carries spawns");

        assert!(
            last_cancel_phase < first_spawn,
            "every cancel-phase entry applies before every spawn: {steps:?}"
        );
    }

    #[test]
    fn a_same_command_spawn_under_a_torn_down_prefix_follows_the_teardown() {
        // RFC 0013 INV-ST3: the cancel phase has already emptied the slot, so
        // this spawn takes the fresh-slot transition under either policy.
        let steps = planned(
            Command::batch([
                Command::teardown("pane-1"),
                Command::message(1).cancellable(CommandId::new("load")),
            ])
            .scoped("pane-1"),
        );

        assert_eq!(steps, vec!["teardown", "spawn"]);
    }

    #[test]
    fn two_same_key_spawns_apply_in_declaration_order() {
        let plan = dispatch_plan(lowered(Command::batch([
            Command::message(1).cancellable(CommandId::new("slot")),
            Command::message(2)
                .cancellable_with(CommandId::new("slot"), CancelPolicy::KeepInFlight),
        ])));

        let policies: Vec<CancelPolicy> = plan
            .steps
            .iter()
            .filter_map(|step| match step {
                DispatchStep::Spawn(spawn) => Some(spawn.key.as_ref().expect("keyed").policy),
                _ => None,
            })
            .collect();

        assert_eq!(
            policies,
            vec![CancelPolicy::CancelInFlight, CancelPolicy::KeepInFlight],
            "the second same-key spawn is a replacement under its own policy"
        );
    }

    #[test]
    fn the_quit_applies_at_the_dispatch_completion() {
        let steps = planned(Command::batch([
            Command::quit(),
            Command::message(1).cancellable(CommandId::new("load")),
            Command::cancel(CommandId::new("other")),
        ]));

        assert_eq!(steps, vec!["cancel", "spawn", "quit"]);
    }

    #[test]
    fn a_command_with_nothing_to_apply_plans_no_steps() {
        assert!(planned(Command::none()).is_empty());
    }

    // The keyed slot policy, and its correspondence with the pure state
    // machine it succeeds.

    // RFC 0003's `lifecycle_transition` (`runtime::keyed_commands`) is the
    // semantics this decision has to agree with. Its `Spawn` rows:
    //
    // | state      | policy           | decision           |
    // | ---------- | ---------------- | ------------------ |
    // | `Absent`   | either           | `Start`            |
    // | `Running`  | `CancelInFlight` | `ReplaceRunning`   |
    // | `Running`  | `KeepInFlight`   | `KeepInFlight`     |
    // | `Draining` | `CancelInFlight` | `ReplaceDraining`  |
    // | `Draining` | `KeepInFlight`   | `KeepInFlight`     |
    //
    // `Draining` there is "the task ended and the receiver still holds
    // output", which on this kernel is the exited-with-pending tombstone —
    // and both of its rows treat the id as *occupied*. The two `Replace`
    // rows differ only in the mechanism the old machine had to name (an
    // abort was pointless for a run whose task had already ended), which the
    // single stop transition now covers by construction.
    #[derive(Clone, Copy)]
    enum LifecycleState {
        Absent,
        Running,
        Draining,
    }

    fn occupant_of(state: LifecycleState) -> Option<RunToken> {
        match state {
            LifecycleState::Absent => None,
            // Both occupy; the token distinguishes them for the assertion.
            LifecycleState::Running => Some(1),
            LifecycleState::Draining => Some(2),
        }
    }

    #[test]
    fn an_absent_slot_starts_under_either_policy() {
        for policy in [CancelPolicy::CancelInFlight, CancelPolicy::KeepInFlight] {
            assert_eq!(
                keyed_slot_decision(occupant_of(LifecycleState::Absent), policy),
                SpawnDecision::Start
            );
        }
    }

    #[test]
    fn cancel_in_flight_replaces_a_running_occupant() {
        assert_eq!(
            keyed_slot_decision(
                occupant_of(LifecycleState::Running),
                CancelPolicy::CancelInFlight
            ),
            SpawnDecision::Replace(1)
        );
    }

    #[test]
    fn cancel_in_flight_replaces_a_draining_occupant() {
        assert_eq!(
            keyed_slot_decision(
                occupant_of(LifecycleState::Draining),
                CancelPolicy::CancelInFlight
            ),
            SpawnDecision::Replace(2),
            "a supersession still drops a finished run's queued output \
             (RFC 0003 §6.1's INV-6 successor)"
        );
    }

    #[test]
    fn keep_in_flight_suppresses_over_any_occupant() {
        for state in [LifecycleState::Running, LifecycleState::Draining] {
            assert_eq!(
                keyed_slot_decision(occupant_of(state), CancelPolicy::KeepInFlight),
                SpawnDecision::Suppress
            );
        }
    }

    #[test]
    fn an_anonymous_spawn_never_consults_a_slot() {
        let registry = ScopeRegistry::new(LoadObserver::new());

        assert_eq!(spawn_decision(&registry, None), SpawnDecision::Start);
    }

    #[test]
    fn a_keyed_spawn_against_an_empty_registry_starts() {
        let registry = ScopeRegistry::new(LoadObserver::new());
        let key = CancellableCommand {
            id: CommandId::new("load"),
            policy: CancelPolicy::KeepInFlight,
        };

        assert_eq!(
            spawn_decision(&registry, Some(&key)),
            SpawnDecision::Start,
            "an empty slot is not occupied, whatever the policy says"
        );
    }
}
