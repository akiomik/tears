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
    pub(super) fn apply_teardown(&mut self, _prefix: &ScopePath) {
        todo!("teardown selection, revocation, and stop requests")
    }
}
