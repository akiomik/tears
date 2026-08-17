use std::fmt;
use std::hash::{Hash, Hasher};

use crate::structural_key::{ScopePath, StructuralKey};

/// Identifies one cancellable command-output lifecycle.
///
/// Equality is structural and includes the concrete Rust type of the stored
/// value. Consequently, values that compare equal but have different types are
/// distinct command ids.
#[derive(Clone)]
pub struct CommandId {
    inner: StructuralKey,
    scope: ScopePath,
}

impl CommandId {
    /// Creates an id from an owned, structurally comparable value.
    pub fn new<T>(value: T) -> Self
    where
        T: Eq + Hash + Send + Sync + 'static,
    {
        Self {
            inner: StructuralKey::new(value),
            scope: ScopePath::empty(),
        }
    }

    /// Returns a new id with an already-erased scope segment prepended (see
    /// RFC 0005 section 4.3). Used by [`Command::scoped`](super::Command::scoped)
    /// to apply one scope value to every lifecycle id present at its call
    /// boundary without requiring the scope type to be `Clone`.
    ///
    /// The segment goes to the head of the path because the enclosing
    /// boundary is the outer one: scope paths are root-first, so prefix
    /// selection reads from the root (see [`ScopePath`]).
    pub(super) fn scoped_with(&self, segment: StructuralKey) -> Self {
        Self {
            inner: self.inner.clone(),
            scope: self.scope.prefixed_key(segment),
        }
    }

    /// This id's scope path, used by the kernel to attribute a keyed run to
    /// the composition boundary that spawned it (RFC 0014 §4.1).
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the kernel that attributes runs by scope lands after this accessor"
        )
    )]
    pub(crate) const fn scope(&self) -> &ScopePath {
        &self.scope
    }
}

impl fmt::Debug for CommandId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CommandId")
            .field("type", &self.inner.type_name())
            .finish_non_exhaustive()
    }
}

impl PartialEq for CommandId {
    fn eq(&self, other: &Self) -> bool {
        self.inner == other.inner && self.scope == other.scope
    }
}

impl Eq for CommandId {}

impl Hash for CommandId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.inner.hash(state);
        self.scope.hash(state);
    }
}

/// Controls how a new command interacts with deliverable same-id work.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum CancelPolicy {
    /// Drop the current receiver, abort its work, and start the new command.
    #[default]
    CancelInFlight,
    /// Keep the current deliverable command and discard the new command stream.
    KeepInFlight,
}

#[derive(Default)]
pub(super) struct CommandCancellation {
    pub(super) key: Option<CancellableCommand>,
    pub(super) cancels: Vec<CommandId>,
}

#[derive(Clone, Debug)]
pub struct CancellableCommand {
    pub id: CommandId,
    pub policy: CancelPolicy,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::hash_map::DefaultHasher;

    fn hash(id: &CommandId) -> u64 {
        let mut hasher = DefaultHasher::new();
        id.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn command_id_equality_is_structural_and_type_sensitive() {
        assert_eq!(CommandId::new(7_u64), CommandId::new(7_u64));
        assert_ne!(CommandId::new(7_u64), CommandId::new(8_u64));
        assert_ne!(CommandId::new(7_u64), CommandId::new(7_i64));
        assert_eq!(hash(&CommandId::new(7_u64)), hash(&CommandId::new(7_u64)));
    }

    #[test]
    fn hash_collisions_do_not_make_distinct_values_equal() {
        #[derive(Eq, PartialEq)]
        struct Collision(u8);

        impl Hash for Collision {
            fn hash<H: Hasher>(&self, state: &mut H) {
                0_u8.hash(state);
            }
        }

        let first = CommandId::new(Collision(1));
        let second = CommandId::new(Collision(2));

        assert_eq!(hash(&first), hash(&second));
        assert_ne!(first, second);
    }

    #[test]
    fn debug_identifies_only_the_erased_type() {
        let first = format!("{:?}", CommandId::new(1_u64));
        let second = format!("{:?}", CommandId::new(2_u64));

        assert_eq!(first, second);
        assert!(first.contains("u64"));
        assert!(!first.contains('1'));
    }

    #[test]
    fn default_policy_cancels_in_flight_work() {
        assert_eq!(CancelPolicy::default(), CancelPolicy::CancelInFlight);
    }

    #[test]
    fn scoped_with_differs_from_unscoped() {
        let unscoped = CommandId::new("load");
        let scoped = unscoped.scoped_with(StructuralKey::new("pane-1"));

        assert_ne!(unscoped, scoped);
    }

    #[test]
    fn scoped_with_makes_independent_child_instances_distinct() {
        let base = CommandId::new("load");
        let first = base.scoped_with(StructuralKey::new("pane-1"));
        let second = base.scoped_with(StructuralKey::new("pane-2"));

        assert_ne!(first, second);
    }

    #[test]
    fn scoped_with_equal_scope_is_equal() {
        let base = CommandId::new("load");
        let first = base.scoped_with(StructuralKey::new("pane-1"));
        let second = base.scoped_with(StructuralKey::new("pane-1"));

        assert_eq!(first, second);
        assert_eq!(hash(&first), hash(&second));
    }

    #[test]
    fn scoped_with_applies_one_shared_erasure_to_different_ids() {
        let segment = StructuralKey::new("pane-1");
        let first = CommandId::new("load").scoped_with(segment.clone());
        let second = CommandId::new("save").scoped_with(segment);

        // Different local ids under the same scope segment remain distinct.
        assert_ne!(first, second);
    }
}
