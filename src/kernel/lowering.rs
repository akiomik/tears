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
