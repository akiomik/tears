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
//!    use;
//! 4. **consumes and starts** the prefix's unfired cleanup registrations
//!    (RFC 0014 §4.4, RFC 0013 §5).
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
    /// **Step 4 — the prefix's unfired cleanup registrations are consumed
    /// and started**, at this same point: the head of the
    /// stop-requested→quiesced interval RFC 0013 §1.2 places the cleanup
    /// window in, so a finalizer runs *concurrently* with the quiescence of
    /// the runs this application just stopped rather than after it. Nothing
    /// here awaits a run, and nothing here awaits a finalizer.
    ///
    /// Consumption is removal, which is what makes the hook at-most-once and
    /// the second application of a prefix re-fire nothing (INV-RC8,
    /// RFC 0013 INV-ST5) — and it is the *whole* mechanism, so there is no
    /// fired-flag for a later path to read wrongly.
    ///
    /// Selection is read whole before any stop is issued, and the stops are
    /// issued before any finalizer starts. Both orders are load-bearing. A
    /// finalizer started here is a run under this very prefix, so a
    /// selection taken afterwards would include it; RFC 0013 §3.1 excludes
    /// already-started cleanup runs, and the registry carries that exclusion
    /// by kind so it holds for a *later* teardown of the same prefix too,
    /// not only for this one.
    ///
    /// No dirt is marked here. A teardown-stopped subscription run marks
    /// subscriptions dirty when it *quiesces*, not when it is stopped
    /// (RFC 0014 §5.2), and that observation belongs to the pass's exit
    /// reflection stage. A cleanup run's own quiescence marks none at all,
    /// whichever way it ends.
    pub(super) fn apply_teardown(&mut self, prefix: &ScopePath) {
        for token in self.registry.select_prefix(prefix) {
            self.registry.stop_request(token);
        }

        for registration in self.cleanups.take_under(prefix) {
            self.start_cleanup(registration);
        }
    }
}
