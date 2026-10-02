//! The collections a combinator boundary composes over, and the identities
//! that make their teardowns complete.
//!
//! [`Keyed`] holds one child state per key and [`Slot`] holds at most one.
//! Each row and each occupant is an *occupancy* with an identity of its own,
//! drawn when the occupancy begins and shared with no other (RFC 0014 §2.5).
//! The kernel compares the identities a state reports after every update
//! with the ones it reported before, and tears down the path of every one
//! that disappeared (INV-RC3). Nothing here records a removal: whatever takes
//! an occupancy out of the state — a removal method, an assignment, a swap, a
//! reducer above the boundary — takes its identity out of the next report.
//!
//! # Why identities and not keys
//!
//! Remove key `k` and re-insert `k` in one update, and the keys before and
//! after are equal while the instance under `k` is a different one. Compared
//! by key, the old instance's runs would never be torn down — RFC 0014
//! §11's *key-only removal detection* adversary. The reinserted row carries a
//! new identity, so the old one's pair disappears and its path is torn down,
//! and the new one starts fresh.
//!
//! An identity begins at [`Keyed::insert`] — into an absent or an occupied
//! key — at [`Keyed::from_iter`], and at [`Slot::present`], and nowhere else.
//! It is not readable, copyable, or assignable from outside the crate, so a
//! collection built anew holds new identities even under keys an earlier one
//! held, and moving a collection value carries its identities with it.
//! Mutating a row or an occupant in place keeps its identity.

use std::hash::Hash;
use std::mem;
use std::sync::atomic::{AtomicU64, Ordering};

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
pub trait ScopeValue: Eq + Hash + Clone + Send + Sync + 'static {}

impl<T> ScopeValue for T where T: Eq + Hash + Clone + Send + Sync + 'static {}

/// The identity of one occupancy: a row of a [`Keyed`] or the occupant of a
/// [`Slot`].
///
/// Drawn from one process-wide counter, so no two occupancies ever share one
/// — not in one collection, not across collections, not across runtimes.
/// The counter panics rather than wrap: a reused identity would make a
/// replacement look like continuity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InstanceId(u64);

impl InstanceId {
    /// The placeholder an empty slot holds. Never drawn: the counter starts
    /// above it.
    const NONE: Self = Self(0);

    fn draw() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let drawn = NEXT
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
            .expect("occupancy identities are exhausted");
        Self(drawn)
    }
}

/// A keyed collection of child states, each one an occupancy.
///
/// Iteration is insertion order and lookup is a scan. Which structure this
/// is stays mechanism; what is not mechanism is that the order is
/// *deterministic*, because the commands and subscription declarations a
/// boundary derives from a walk of this collection are observed in it
/// (RFC 0014 INV-RC14).
pub struct Keyed<K: ScopeValue, V> {
    rows: Vec<Row<K, V>>,
}

struct Row<K, V> {
    key: K,
    value: V,
    id: InstanceId,
}

impl<K: ScopeValue, V> Keyed<K, V> {
    /// An empty collection.
    #[must_use]
    pub const fn new() -> Self {
        Self { rows: Vec::new() }
    }

    /// Inserts `value` under `key`, returning the instance it replaced.
    ///
    /// Inserting always begins a new occupancy, over an occupied key too:
    /// the old instance is torn down and the new one starts fresh (RFC 0014
    /// §2.5), on the timing
    /// [`ReducerExt::for_each`](crate::reducer::ReducerExt::for_each)
    /// states. The position in the iteration order is the old instance's, so
    /// a replacement does not reorder the collection.
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        let id = InstanceId::draw();
        if let Some(row) = self.rows.iter_mut().find(|row| row.key == key) {
            row.id = id;
            return Some(mem::replace(&mut row.value, value));
        }
        self.rows.push(Row { key, value, id });
        None
    }

    /// Removes the instance under `key`.
    pub fn remove(&mut self, key: &K) -> Option<V> {
        let position = self.rows.iter().position(|row| row.key == *key)?;
        Some(self.rows.remove(position).value)
    }

    /// The instance under `key`.
    pub fn get(&self, key: &K) -> Option<&V> {
        self.rows
            .iter()
            .find(|row| row.key == *key)
            .map(|row| &row.value)
    }

    /// The instance under `key`, mutably. The occupancy continues, even when
    /// the value is replaced through it; [`insert`](Self::insert) starts a
    /// new one.
    pub fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        self.rows
            .iter_mut()
            .find(|row| row.key == *key)
            .map(|row| &mut row.value)
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
        self.rows.iter().map(|row| (&row.key, &row.value))
    }

    /// The keys, in insertion order.
    #[must_use]
    pub fn keys(&self) -> impl ExactSizeIterator<Item = &K> {
        self.rows.iter().map(|row| &row.key)
    }

    /// The rows with their identities, in insertion order, for the report.
    pub(crate) fn occupancies(&self) -> impl Iterator<Item = (&K, &V, InstanceId)> {
        self.rows.iter().map(|row| (&row.key, &row.value, row.id))
    }
}

impl<K: ScopeValue, V> Default for Keyed<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: ScopeValue, V> FromIterator<(K, V)> for Keyed<K, V> {
    /// Builds a collection from `(key, value)` pairs, each row a new
    /// occupancy; a later pair for a key already collected replaces the
    /// earlier one, as [`insert`](Keyed::insert) would.
    fn from_iter<I: IntoIterator<Item = (K, V)>>(pairs: I) -> Self {
        let mut collection = Self::new();
        for (key, value) in pairs {
            collection.insert(key, value);
        }
        collection
    }
}

/// At most one child state, an occupancy while it is present.
///
/// The one-instance counterpart of [`Keyed`]: what a modal, a detail pane,
/// or any other optionally-present child lives in.
pub struct Slot<S> {
    value: Option<S>,
    /// The occupant's identity. Meaningful only while `value` is `Some`; a
    /// field beside the value rather than inside the option, so emptying
    /// the slot moves the occupant out without a destructor to run and
    /// `dismiss` stays a `const fn`.
    id: InstanceId,
}

impl<S> Slot<S> {
    /// An empty slot.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            value: None,
            id: InstanceId::NONE,
        }
    }

    /// Puts `value` in the slot, returning the instance it replaced.
    ///
    /// Presenting always begins a new occupancy, over an occupied slot too,
    /// for the reason [`Keyed::insert`] does: replacement is a teardown of the
    /// old instance and a fresh start for the new one, on the timing
    /// [`ReducerExt::presented`](crate::reducer::ReducerExt::presented)
    /// states. Drawing the identity is why this is not a `const fn`.
    pub fn present(&mut self, value: S) -> Option<S> {
        self.id = InstanceId::draw();
        self.value.replace(value)
    }

    /// Empties the slot.
    pub const fn dismiss(&mut self) -> Option<S> {
        self.value.take()
    }

    /// The instance, if the slot holds one.
    pub const fn get(&self) -> Option<&S> {
        self.value.as_ref()
    }

    /// The instance, mutably. The occupancy continues, even when the value is
    /// replaced through it; [`present`](Self::present) starts a new one.
    pub const fn get_mut(&mut self) -> Option<&mut S> {
        self.value.as_mut()
    }

    /// Whether the slot holds an instance.
    #[must_use]
    pub const fn is_present(&self) -> bool {
        self.value.is_some()
    }

    /// The occupant with its identity, for the report.
    pub(crate) fn occupancy(&self) -> Option<(&S, InstanceId)> {
        self.value.as_ref().map(|value| (value, self.id))
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

    fn ids<K: ScopeValue, V>(rows: &Keyed<K, V>) -> Vec<InstanceId> {
        rows.occupancies().map(|(_, _, id)| id).collect()
    }

    fn slot_id<S>(slot: &Slot<S>) -> Option<InstanceId> {
        slot.occupancy().map(|(_, id)| id)
    }

    #[test]
    fn inserting_over_an_occupied_key_begins_a_new_occupancy_in_place() {
        let mut rows: Keyed<&str, u8> = Keyed::new();
        rows.insert("a", 1);
        rows.insert("b", 2);
        let before = ids(&rows);

        assert_eq!(rows.insert("a", 9), Some(1));

        let after = ids(&rows);
        assert_ne!(before[0], after[0], "the replacement is a new occupancy");
        assert_eq!(before[1], after[1], "the other row continues");
        assert_eq!(
            rows.keys().copied().collect::<Vec<_>>(),
            vec!["a", "b"],
            "and keeps the replaced row's position"
        );
    }

    #[test]
    fn a_same_key_remove_and_reinsert_is_a_new_occupancy() {
        let mut rows: Keyed<&str, u8> = Keyed::new();
        rows.insert("a", 1);
        let before = ids(&rows);

        rows.remove(&"a");
        rows.insert("a", 1);

        assert_ne!(before, ids(&rows), "equal keys and values, a new identity");
    }

    #[test]
    fn mutating_in_place_continues_the_occupancy() {
        let mut rows: Keyed<&str, u8> = Keyed::new();
        let mut slot: Slot<u8> = Slot::empty();
        rows.insert("a", 1);
        slot.present(1);
        let (row_before, slot_before) = (ids(&rows), slot_id(&slot));

        *rows.get_mut(&"a").expect("the row is present") = 7;
        *slot.get_mut().expect("the slot is occupied") = 7;

        assert_eq!(rows.get(&"a"), Some(&7));
        assert_eq!(slot.get(), Some(&7));
        assert_eq!(ids(&rows), row_before);
        assert_eq!(slot_id(&slot), slot_before);
    }

    #[test]
    fn an_absent_row_and_an_empty_slot_are_not_reachable_mutably() {
        let mut rows: Keyed<&str, u8> = Keyed::new();
        let mut slot: Slot<u8> = Slot::empty();
        rows.insert("a", 1);

        assert!(rows.get_mut(&"missing").is_none());
        assert!(slot.get_mut().is_none());
        slot.present(1);
        slot.dismiss();
        assert!(slot.get_mut().is_none(), "a dismissed slot is empty again");
    }

    #[test]
    fn a_collection_built_anew_holds_new_identities_under_the_same_keys() {
        let old: Keyed<&str, u8> = [("a", 1), ("b", 2)].into_iter().collect();
        let new: Keyed<&str, u8> = [("a", 1), ("b", 2)].into_iter().collect();

        assert!(ids(&old).iter().all(|id| !ids(&new).contains(id)));
    }

    #[test]
    fn moving_a_collection_carries_its_identities() {
        let mut held: Keyed<&str, u8> = Keyed::new();
        held.insert("a", 1);
        let before = ids(&held);

        let taken = mem::take(&mut held);

        assert_eq!(ids(&taken), before);
        assert!(held.is_empty());
    }

    #[test]
    fn presenting_over_an_occupied_slot_begins_a_new_occupancy() {
        let mut slot: Slot<u8> = Slot::empty();
        slot.present(1);
        let before = slot_id(&slot);

        assert_eq!(slot.present(2), Some(1));

        assert_ne!(slot_id(&slot), before);
        assert_eq!(slot.get(), Some(&2));
    }

    #[test]
    fn removing_and_dismissing_end_the_occupancy() {
        let mut rows: Keyed<&str, u8> = Keyed::new();
        let mut slot: Slot<u8> = Slot::empty();
        rows.insert("a", 1);
        slot.present(1);

        assert_eq!(rows.remove(&"a"), Some(1));
        assert_eq!(slot.dismiss(), Some(1));

        assert!(rows.is_empty());
        assert_eq!(slot_id(&slot), None);
        assert_eq!(rows.remove(&"missing"), None);
        assert_eq!(slot.dismiss(), None);
    }

    #[test]
    fn collecting_a_duplicate_key_keeps_the_later_pair() {
        let rows: Keyed<&str, u8> = [("a", 1), ("b", 2), ("a", 3)].into_iter().collect();

        assert_eq!(rows.get(&"a"), Some(&3));
        assert_eq!(rows.keys().copied().collect::<Vec<_>>(), vec!["a", "b"]);
    }

    // Iteration is insertion order and removal closes the gap — what a
    // boundary's walk of the collection is observed in (INV-RC14).
    #[test]
    fn iteration_is_insertion_order_and_removal_closes_the_gap() {
        let mut rows: Keyed<&str, u8> = Keyed::new();
        rows.insert("a", 1);
        rows.insert("b", 2);
        rows.insert("c", 3);

        rows.remove(&"b");

        assert_eq!(rows.keys().copied().collect::<Vec<_>>(), vec!["a", "c"]);
        assert_eq!(rows.len(), 2);
        assert!(!rows.contains_key(&"b"));
    }
}
