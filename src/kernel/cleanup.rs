//! The cleanup ledger: the kernel's armed, not-yet-fired finalizers.
//!
//! One ledger for the whole kernel, holding every registration a dispatch
//! has armed and not yet handed to a teardown. Three operations, and they
//! are the whole of INV-RC8's bookkeeping half:
//!
//! - **arm** — a dispatch's spawn phase adds a registration
//!   ([`register`](CleanupLedger::register));
//! - **consume** — a teardown's application point *removes* the
//!   registrations under its prefix and starts them
//!   ([`take_under`](CleanupLedger::take_under)). Removal is what makes the
//!   hook at-most-once: a second teardown of the same prefix finds nothing
//!   left, so idempotence is structural rather than a flag the ledger has to
//!   remember to check (RFC 0013 INV-ST5);
//! - **discard** — termination drops what is still armed
//!   ([`discard_all`](CleanupLedger::discard_all)), because termination is
//!   not a teardown and fires no hooks (RFC 0011 §4.4 takes precedence).
//!
//! What the ledger deliberately does *not* hold is a started cleanup run:
//! once a registration is consumed the ledger has nothing about it, and the
//! run it became is the registry's like any other. That is also why an
//! already-started cleanup run is not selectable by a later teardown
//! (RFC 0013 §3.1) — there is nothing here for one to select, and the
//! registry excludes the run kind.

use std::mem;

use crate::command::CleanupRegistration;
use crate::structural_key::ScopePath;

/// The armed finalizers, in arming order.
#[derive(Default)]
pub struct CleanupLedger {
    /// Registrations that no teardown has consumed yet.
    ///
    /// A `Vec` because the operations are "append" and "take every entry
    /// under a prefix": prefix membership is a path comparison rather than a
    /// lookup key, so an index over exact scopes would answer the wrong
    /// question. Which structure this is stays mechanism (RFC 0013 §3.7),
    /// and the order it preserves is arming order.
    entries: Vec<CleanupRegistration>,
}

impl CleanupLedger {
    /// An empty ledger.
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Arms one registration.
    pub fn register(&mut self, registration: CleanupRegistration) {
        self.entries.push(registration);
    }

    /// Removes and returns every registration whose scope lies under
    /// `prefix`, in arming order.
    ///
    /// The same complete-prefix comparison from the path root that selects
    /// runs (RFC 0013 INV-ST1), applied to registrations: a registration
    /// anchored *at* the prefix is consumed, and shorter, reordered, subset,
    /// and deeper-position anchors are not.
    ///
    /// Consuming rather than marking is what makes the finalizer run at most
    /// once across any number of teardowns.
    pub fn take_under(&mut self, prefix: &ScopePath) -> Vec<CleanupRegistration> {
        let (selected, kept) = mem::take(&mut self.entries)
            .into_iter()
            .partition(|registration| registration.scope.starts_with(prefix));
        self.entries = kept;
        selected
    }

    /// Drops every armed registration without firing any.
    ///
    /// Termination's, and only termination's: it is not a teardown, so the
    /// hooks it discards never run (RFC 0014 INV-RC8's last clause).
    pub fn discard_all(&mut self) {
        self.entries.clear();
    }

    /// How many registrations are armed.
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is armed.
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::future::ready;

    fn path(segments: &[&'static str]) -> ScopePath {
        // Root-first storage: the *last* `prefixed` call names the outermost
        // segment, so the slice reads root-first when applied in reverse.
        segments
            .iter()
            .rev()
            .fold(ScopePath::empty(), |acc, segment| acc.prefixed(*segment))
    }

    fn armed(ledger: &mut CleanupLedger, scope: &ScopePath) {
        let mut registration = CleanupRegistration::new(ready(()));
        registration.scope = scope.clone();
        ledger.register(registration);
    }

    #[test]
    fn a_prefix_consumes_the_registrations_under_it_and_leaves_the_rest() {
        let mut ledger = CleanupLedger::new();
        armed(&mut ledger, &path(&["pane"]));
        armed(&mut ledger, &path(&["pane", "field"]));
        armed(&mut ledger, &path(&["other"]));
        armed(&mut ledger, &ScopePath::empty());

        let taken = ledger.take_under(&path(&["pane"]));

        assert_eq!(taken.len(), 2, "the anchor itself and the deeper anchor");
        assert_eq!(
            ledger.len(),
            2,
            "a sibling anchor and a root anchor are outside the prefix"
        );
    }

    // INV-RC8's at-most-once clause, in the ledger: consumption removes, so a
    // repeat application of the same prefix has nothing left to start
    // (RFC 0013 INV-ST5's "re-firing nothing").
    #[test]
    fn a_second_application_of_the_same_prefix_consumes_nothing() {
        let mut ledger = CleanupLedger::new();
        armed(&mut ledger, &path(&["pane"]));

        assert_eq!(ledger.take_under(&path(&["pane"])).len(), 1);
        assert!(ledger.take_under(&path(&["pane"])).is_empty());
        assert!(ledger.is_empty());
    }

    #[test]
    fn a_prefix_nothing_is_anchored_under_consumes_nothing() {
        let mut ledger = CleanupLedger::new();
        armed(&mut ledger, &path(&["pane"]));

        assert!(ledger.take_under(&path(&["nothing-is-here"])).is_empty());
        assert_eq!(ledger.len(), 1, "and leaves what was armed alone");
    }

    // Selection is the run predicate applied to anchors: a shorter, a
    // reordered, and a deeper-position anchor are all outside the prefix.
    #[test]
    fn anchor_selection_is_a_complete_prefix_from_the_root() {
        let mut ledger = CleanupLedger::new();
        armed(&mut ledger, &path(&["pane"]));
        armed(&mut ledger, &path(&["field", "pane"]));
        armed(&mut ledger, &path(&["outer", "pane", "field"]));

        assert!(ledger.take_under(&path(&["pane", "field"])).is_empty());
        assert_eq!(ledger.len(), 3, "none of the three is under it");
    }

    #[test]
    fn termination_discards_every_armed_registration() {
        let mut ledger = CleanupLedger::new();
        armed(&mut ledger, &path(&["pane"]));
        armed(&mut ledger, &ScopePath::empty());

        ledger.discard_all();

        assert!(ledger.is_empty());
        assert!(
            ledger.take_under(&ScopePath::empty()).is_empty(),
            "the root prefix covers everything, and there is nothing left for it"
        );
    }
}
