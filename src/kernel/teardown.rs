//! Scope teardown: selection, revocation, and the stop requests it issues.
//!
//! The contract is RFC 0013's; this is its kernel side (RFC 0014 §4). One
//! application of a teardown for a prefix:
//!
//! 1. **selects** every run under the prefix — keyed commands, anonymous
//!    effects, subscription runs alike — by complete prefix path over
//!    structural segment equality, plus the prefix's unfired cleanup
//!    registrations;
//! 2. **revokes** each selected run, so from this point none of its output
//!    is delivered, a buffered producer quit under the prefix included
//!    (RFC 0014 INV-RC5, INV-RC6);
//! 3. **requests a stop** for each, through the registry's single stop
//!    transition, which is also what cancel, supersession, and termination
//!    use.
//!
//! Teardown applies in the cancel phase, before every spawn of the same
//! command, and commutes with explicit cancels. It is total and idempotent:
//! re-applying it selects whatever is under the prefix at that moment and
//! consumes nothing twice. Scope reuse observes nothing stale, and it does
//! so without any scope-generation state — per-run tokens and the fresh-slot
//! rule carry it (RFC 0013 INV-ST7's absence half, which is a structural
//! claim about what this module does *not* keep).
//!
//! What teardown cannot reach is small and named: what has already crossed
//! delivery or left the runtime's custody, and input carrying no run origin
//! (RFC 0013 §3.8). Everything else — anonymous-run reachability and
//! undelivered-output retraction in particular — is regression-tested, not
//! negative space.

use crate::reducer::Program;
use crate::structural_key::ScopePath;

use super::Kernel;

impl<P: Program> Kernel<P> {
    /// Applies one teardown for `prefix`.
    ///
    /// Steps 2 and 3 are one call: the registry's stop transition revokes
    /// unconditionally and aborts only what is still executing, so a
    /// selected tombstone is revoked (its queued envelopes are filtered at
    /// their dequeue) while a selected live run is revoked *and* aborted.
    /// That is also why this is total and idempotent — every selected token
    /// takes a transition that is defined for it, and a second application
    /// re-revokes an already-revoked set with no further effect.
    ///
    /// Selection is read whole before any stop is issued. Today that is not
    /// load-bearing — the stop transition changes only revocation and phase,
    /// neither of which the scope-path predicate reads, so the selected set
    /// is invariant under the loop. It becomes load-bearing with the cleanup
    /// hooks: a finalizer started by this very application is a run under
    /// this very prefix, and RFC 0013 §3.1 excludes already-started cleanup
    /// runs from selection.
    ///
    /// No dirt is marked here. A teardown-stopped subscription run marks
    /// subscriptions dirty when it *quiesces*, not when it is stopped
    /// (RFC 0014 §5.2), and that observation belongs to the pass's exit
    /// reflection stage.
    pub(super) fn apply_teardown(&mut self, prefix: &ScopePath) {
        for token in self.registry.select_prefix(prefix) {
            self.registry.stop_request(token);
        }

        // The prefix's unfired cleanup registrations are consumed and
        // started here (RFC 0014 §4.4, INV-RC8). The ledger that holds them
        // lands with the cleanup run kind; until then a teardown has none to
        // consume, and there is deliberately no partial half of the hook
        // contract in the meantime.
    }
}
