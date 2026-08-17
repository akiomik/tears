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

use crate::command::{KernelParts, RuntimeCommandParts};

/// Lowers one command into the phase buckets a dispatch applies: the cancel
/// phase (explicit cancels and teardown prefixes) before every spawn of the
/// same command, then the spawn phase in declaration order (RFC 0014 §3.4).
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

    let parts = parts.into_kernel_parts();

    // A keyed run's scope attribution and its key's own scope path are the
    // same path by construction: `Command::scoped` prefixes both with the
    // same segment at the same boundary. Asserting it here keeps the two
    // sources of the same fact from drifting apart silently.
    #[cfg(debug_assertions)]
    for spawn in &parts.spawns {
        if let Some(key) = spawn.key.as_ref() {
            debug_assert_eq!(
                key.id.scope(),
                &spawn.scope,
                "a keyed run's scope and its key's scope are one path"
            );
        }
    }

    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::command::{CancelPolicy, Command, CommandId};
    use crate::structural_key::ScopePath;

    fn lowered(command: Command<i32>) -> KernelParts<i32> {
        lower(command.into_runtime_parts())
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
}
