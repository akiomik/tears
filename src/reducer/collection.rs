//! The collections a combinator boundary composes over, and the removal
//! journals that make their teardowns complete.
//!
//! [`Keyed`] holds one child state per key and [`Slot`] holds at most one.
//! Both record **removals** rather than exposing their contents for a
//! diff — that difference is the whole contract here (RFC 0014 INV-RC3).
//!
//! # Why a journal and not a diff
//!
//! A combinator that compared the collection before and after an update
//! would see nothing at all in the one case that matters: remove key `k` and
//! re-insert `k` in the same update, and the before/after states are equal
//! while the *instance* under `k` is a different one. The old instance's
//! runs would then never be torn down — RFC 0014 §11's *diff-based removal
//! detection* adversary, which passes every single-removal test. A journal
//! records the removal when it happens, so the same-update reinsertion still
//! yields the old instance's teardown and the new instance's fresh spawns.
//!
//! # The four removal shapes
//!
//! INV-RC3 quantifies over exactly four, and each records one entry:
//!
//! | shape | method |
//! | --- | --- |
//! | explicit removal by key | [`Keyed::remove`] |
//! | explicit dismissal | [`Slot::dismiss`] |
//! | replacement over an occupied key | [`Keyed::insert`] |
//! | replacement over an occupied slot | [`Slot::present`] |
//!
//! The two replacement shapes are removals because that is what replacement
//! *means* here: the old instance is torn down and the new one starts fresh
//! (RFC 0014 §2.5). Insertion into an absent key and presentation into an
//! empty slot record nothing — there was no instance to remove.
//!
//! **Four shapes is the whole surface, and that is deliberate.** There is no
//! `retain`, no `clear`, no `drain`, no `IndexMut`, and no `&mut` iterator:
//! removing several rows is several [`Keyed::remove`] calls, which is the
//! only thing that keeps INV-RC3's "every removal shape" exhaustive by
//! construction rather than by review. Any bulk operation added later
//! **must record one journal entry per instance it removes** — a `retain`
//! that quietly kept the surviving rows and dropped the rest would leak
//! every dropped row's runs, which is §11's diff-based adversary arriving
//! through a convenience method. The same goes for any accessor that hands
//! out mutable access to the backing sequence.
//!
//! Draining is the combinator's, not the application's: the two `drain_*`
//! methods are crate-private, so an application can neither consume a
//! pending removal before the boundary sees it nor manufacture one.
//!
//! # Mutating outside a `reduce`
//!
//! A journal entry is drained by the next `reduce` the boundary runs, so a
//! mutation made *outside* one is still owed its teardown — and will get it
//! at the first message that reaches the boundary. That is right for a
//! removal and wrong for construction, where no instance ever ran.
//!
//! Only the four removal shapes record anything, so only they are affected.
//! [`Keyed::from_iter`], a collected literal, [`Keyed::insert`] into an
//! absent key, and [`Slot::present`] into an empty slot record nothing and
//! are as safe outside a `reduce` as inside one — growing a collection
//! during `Program::init` is fine. What belongs inside a `reduce` is the
//! four that do record: `insert` over an occupied key, `present` over an
//! occupied slot, `remove`, and `dismiss`, whose entries the boundary drains
//! in the same update.

use std::hash::Hash;
use std::mem;

/// The values a composition boundary may be segmented by.
///
/// This is RFC 0005's segment-value contract restated as a bound, and it
/// carries no invariant of its own: `Eq + Hash` are what structural segment
/// identity is defined over, `Send + Sync + 'static` are what erasure into
/// the crate's type-erased segment key requires, and
/// `Clone` is what lets one boundary apply its segment to several carriers
/// and to several updates.
///
/// The blanket implementation below is the whole of it: nothing opts in, and
/// no type that satisfies the bound can be excluded.
///
/// RFC 0014 §2.5's block writes `PartialEq + Eq + …`, which `Eq: PartialEq`
/// makes redundant; the bound is written here without it, and the RFC's
/// wording is synced when §2.5's surface is made public at the switch.
pub trait ScopeValue: Eq + Hash + Clone + Send + Sync + 'static {}

impl<T> ScopeValue for T where T: Eq + Hash + Clone + Send + Sync + 'static {}

/// A keyed collection of child states, with a removal journal.
///
/// Iteration is insertion order and lookup is a scan. Which structure this
/// is stays mechanism; what is not mechanism is that the order is
/// *deterministic*, because the commands and subscription declarations a
/// boundary derives from a walk of this collection are observed in it
/// (RFC 0014 INV-RC14).
pub struct Keyed<K: ScopeValue, V> {
    rows: Vec<(K, V)>,
    /// Keys whose instance was removed since the last drain, in removal
    /// order. One entry per removal, so a remove-and-reinsert-and-remove
    /// within one update records two.
    removals: Vec<K>,
}

impl<K: ScopeValue, V> Keyed<K, V> {
    /// An empty collection.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            rows: Vec::new(),
            removals: Vec::new(),
        }
    }

    /// Inserts `value` under `key`, returning the instance it replaced.
    ///
    /// Replacing an occupied key **records a removal**: the old instance is
    /// torn down and the new one starts fresh (RFC 0014 §2.5). The position
    /// in the iteration order is the old instance's, so a replacement does
    /// not reorder the collection.
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        if let Some(row) = self.rows.iter_mut().find(|(held, _)| *held == key) {
            self.removals.push(key);
            return Some(mem::replace(&mut row.1, value));
        }
        self.rows.push((key, value));
        None
    }

    /// Removes the instance under `key`, recording the removal.
    ///
    /// Removing an absent key records nothing: there is no instance whose
    /// runs would need tearing down.
    pub fn remove(&mut self, key: &K) -> Option<V> {
        let position = self.rows.iter().position(|(held, _)| held == key)?;
        self.removals.push(key.clone());
        Some(self.rows.remove(position).1)
    }

    /// The instance under `key`.
    pub fn get(&self, key: &K) -> Option<&V> {
        self.rows
            .iter()
            .find(|(held, _)| held == key)
            .map(|(_, value)| value)
    }

    /// The instance under `key`, mutably.
    pub fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        self.rows
            .iter_mut()
            .find(|(held, _)| held == key)
            .map(|(_, value)| value)
    }

    /// Whether `key` holds an instance.
    pub fn contains_key(&self, key: &K) -> bool {
        self.get(key).is_some()
    }

    /// How many instances the collection holds.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the collection is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The instances, in insertion order.
    #[must_use]
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&K, &V)> {
        self.rows.iter().map(|(key, value)| (key, value))
    }

    /// The keys, in insertion order.
    #[must_use]
    pub fn keys(&self) -> impl ExactSizeIterator<Item = &K> {
        self.rows.iter().map(|(key, _)| key)
    }

    /// Takes the recorded removals, in removal order.
    ///
    /// Crate-private: the boundary drains this once per `reduce` it runs and
    /// turns each entry into one teardown (RFC 0014 INV-RC3). An application
    /// draining it would be able to lose a removal the boundary has not seen
    /// yet, which is the completeness the invariant is about.
    pub(crate) fn drain_removals(&mut self) -> Vec<K> {
        mem::take(&mut self.removals)
    }
}

impl<K: ScopeValue, V> Default for Keyed<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: ScopeValue, V> FromIterator<(K, V)> for Keyed<K, V> {
    /// Builds a collection from `(key, value)` pairs, recording **no**
    /// removal — a duplicate key in the input included.
    ///
    /// Construction is not replacement. What the journal records is that an
    /// *instance* left the collection, and an instance that only ever
    /// existed as an entry in this iterator never ran anything: nothing was
    /// spawned under its key, nothing declared, nothing to tear down. A
    /// teardown emitted for it would name a prefix no run was ever placed
    /// under. That makes this — and a sequence of [`insert`](Keyed::insert)
    /// calls into absent keys, which record nothing either — the ways to
    /// build initial state; see the module note on mutating outside a
    /// `reduce`.
    fn from_iter<I: IntoIterator<Item = (K, V)>>(pairs: I) -> Self {
        let mut collection = Self::new();
        for (key, value) in pairs {
            if let Some(row) = collection.rows.iter_mut().find(|(held, _)| *held == key) {
                row.1 = value;
            } else {
                collection.rows.push((key, value));
            }
        }
        collection
    }
}

/// At most one child state, with a dismissal journal.
///
/// The one-instance counterpart of [`Keyed`]: what a modal, a detail pane,
/// or any other optionally-present child lives in.
pub struct Slot<S> {
    value: Option<S>,
    /// How many instances have been removed since the last drain. A count
    /// rather than a list because a dismissed instance has no key — what a
    /// boundary derives from each entry is one teardown of its own segment.
    dismissals: usize,
}

impl<S> Slot<S> {
    /// An empty slot.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            value: None,
            dismissals: 0,
        }
    }

    /// Puts `value` in the slot, returning the instance it replaced.
    ///
    /// Presenting over an occupied slot **records a removal**, for the reason
    /// [`Keyed::insert`] does: replacement is a teardown of the old instance
    /// and a fresh start for the new one.
    pub const fn present(&mut self, value: S) -> Option<S> {
        let replaced = self.value.replace(value);
        if replaced.is_some() {
            self.dismissals += 1;
        }
        replaced
    }

    /// Empties the slot, recording the removal.
    ///
    /// Dismissing an empty slot records nothing.
    pub const fn dismiss(&mut self) -> Option<S> {
        let dismissed = self.value.take();
        if dismissed.is_some() {
            self.dismissals += 1;
        }
        dismissed
    }

    /// The instance, if the slot holds one.
    pub const fn get(&self) -> Option<&S> {
        self.value.as_ref()
    }

    /// The instance, mutably.
    pub const fn get_mut(&mut self) -> Option<&mut S> {
        self.value.as_mut()
    }

    /// Whether the slot holds an instance.
    #[must_use]
    pub const fn is_present(&self) -> bool {
        self.value.is_some()
    }

    /// Takes the recorded dismissals.
    ///
    /// Crate-private for the reason [`Keyed::drain_removals`] is.
    pub(crate) const fn drain_dismissals(&mut self) -> usize {
        mem::replace(&mut self.dismissals, 0)
    }
}

impl<S> Default for Slot<S> {
    fn default() -> Self {
        Self::empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The four removal shapes of INV-RC3, one test each.

    #[test]
    fn removing_a_key_records_the_removal() {
        let mut rows: Keyed<&str, u8> = Keyed::new();
        rows.insert("a", 1);
        rows.drain_removals();

        assert_eq!(rows.remove(&"a"), Some(1));

        assert_eq!(rows.drain_removals(), vec!["a"]);
        assert!(rows.is_empty());
    }

    #[test]
    fn dismissing_an_occupied_slot_records_the_removal() {
        let mut slot: Slot<u8> = Slot::empty();
        slot.present(1);
        slot.drain_dismissals();

        assert_eq!(slot.dismiss(), Some(1));

        assert_eq!(slot.drain_dismissals(), 1);
        assert!(!slot.is_present());
    }

    #[test]
    fn inserting_over_an_occupied_key_records_the_removal() {
        let mut rows: Keyed<&str, u8> = Keyed::new();
        rows.insert("a", 1);
        rows.drain_removals();

        assert_eq!(rows.insert("a", 2), Some(1));

        assert_eq!(
            rows.drain_removals(),
            vec!["a"],
            "replacement tears the old instance down and starts the new one fresh"
        );
        assert_eq!(rows.get(&"a"), Some(&2));
    }

    #[test]
    fn presenting_over_an_occupied_slot_records_the_removal() {
        let mut slot: Slot<u8> = Slot::empty();
        slot.present(1);
        slot.drain_dismissals();

        assert_eq!(slot.present(2), Some(1));

        assert_eq!(slot.drain_dismissals(), 1);
        assert_eq!(slot.get(), Some(&2));
    }

    // The two shapes that are *not* removals: there was no instance.

    #[test]
    fn a_first_insertion_and_a_first_presentation_record_nothing() {
        let mut rows: Keyed<&str, u8> = Keyed::new();
        let mut slot: Slot<u8> = Slot::empty();

        assert_eq!(rows.insert("a", 1), None);
        assert_eq!(slot.present(1), None);

        assert!(rows.drain_removals().is_empty());
        assert_eq!(slot.drain_dismissals(), 0);
    }

    #[test]
    fn removing_an_absent_key_and_dismissing_an_empty_slot_record_nothing() {
        let mut rows: Keyed<&str, u8> = Keyed::new();
        let mut slot: Slot<u8> = Slot::empty();

        assert_eq!(rows.remove(&"missing"), None);
        assert_eq!(slot.dismiss(), None);

        assert!(rows.drain_removals().is_empty());
        assert_eq!(slot.drain_dismissals(), 0);
    }

    // RFC 0014 §11's *diff-based removal detection* adversary, at the
    // collection level: the state before and after is identical, and the
    // journal is what still reports that an instance was removed.
    #[test]
    fn a_same_update_remove_and_reinsert_leaves_no_state_difference_but_a_recorded_removal() {
        let mut rows: Keyed<&str, u8> = Keyed::new();
        rows.insert("a", 1);
        rows.drain_removals();
        let before: Vec<(&str, u8)> = rows.iter().map(|(key, value)| (*key, *value)).collect();

        rows.remove(&"a");
        rows.insert("a", 1);

        let after: Vec<(&str, u8)> = rows.iter().map(|(key, value)| (*key, *value)).collect();
        assert_eq!(before, after, "no diff distinguishes the two instances");
        assert_eq!(
            rows.drain_removals(),
            vec!["a"],
            "the journal records the removal a diff cannot see"
        );
    }

    #[test]
    fn every_removal_in_one_update_is_recorded_in_order() {
        let mut rows: Keyed<&str, u8> = Keyed::new();
        rows.insert("a", 1);
        rows.insert("b", 2);
        rows.drain_removals();

        rows.remove(&"b");
        rows.insert("a", 9);
        rows.insert("b", 8);

        assert_eq!(
            rows.drain_removals(),
            vec!["b", "a"],
            "one entry per removal, in removal order"
        );
    }

    #[test]
    fn a_drain_leaves_the_journal_empty() {
        let mut rows: Keyed<&str, u8> = Keyed::new();
        let mut slot: Slot<u8> = Slot::empty();
        rows.insert("a", 1);
        slot.present(1);
        rows.remove(&"a");
        slot.dismiss();

        assert_eq!(rows.drain_removals().len(), 1);
        assert_eq!(slot.drain_dismissals(), 1);
        assert!(
            rows.drain_removals().is_empty(),
            "a removal is reported to exactly one drain"
        );
        assert_eq!(slot.drain_dismissals(), 0);
    }

    // Iteration order is insertion order, and a replacement keeps the
    // replaced instance's position — what a boundary's walk of the
    // collection is observed in (INV-RC14).
    #[test]
    fn iteration_is_insertion_order_and_replacement_keeps_its_position() {
        let mut rows: Keyed<&str, u8> = Keyed::new();
        rows.insert("a", 1);
        rows.insert("b", 2);
        rows.insert("c", 3);
        rows.insert("a", 9);

        assert_eq!(
            rows.keys().copied().collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
    }

    #[test]
    fn removal_closes_the_gap_it_leaves() {
        let mut rows: Keyed<&str, u8> = Keyed::new();
        rows.insert("a", 1);
        rows.insert("b", 2);
        rows.insert("c", 3);

        rows.remove(&"b");

        assert_eq!(rows.keys().copied().collect::<Vec<_>>(), vec!["a", "c"]);
        assert_eq!(rows.len(), 2);
        assert!(!rows.contains_key(&"b"));
    }

    // Construction is not replacement: an instance that only ever existed as
    // an entry in the input iterator never ran anything, so a teardown for
    // it would name a prefix no run was placed under.
    #[test]
    fn collecting_duplicate_keys_records_no_removal() {
        let mut rows: Keyed<&str, u8> = [("a", 1), ("b", 2), ("a", 3)].into_iter().collect();

        assert_eq!(rows.get(&"a"), Some(&3));
        assert_eq!(rows.len(), 2);
        assert!(
            rows.drain_removals().is_empty(),
            "nothing had run under the key the later pair replaced"
        );
    }

    #[test]
    fn a_row_is_reachable_mutably_and_a_missing_one_is_not() {
        let mut rows: Keyed<&str, u8> = Keyed::new();
        rows.insert("a", 1);

        *rows.get_mut(&"a").expect("the row is present") = 7;

        assert_eq!(rows.get(&"a"), Some(&7));
        assert!(rows.get_mut(&"missing").is_none());
    }

    #[test]
    fn a_slot_is_reachable_mutably_and_an_empty_one_is_not() {
        let mut slot: Slot<u8> = Slot::empty();

        assert!(slot.get_mut().is_none());
        slot.present(1);
        *slot.get_mut().expect("the slot is occupied") = 7;

        assert_eq!(slot.get(), Some(&7));
    }
}
