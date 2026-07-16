use std::fmt;
use std::hash::{Hash, Hasher};

use crate::structural_key::StructuralKey;

/// Identifies one cancellable command-output lifecycle.
///
/// Equality is structural and includes the concrete Rust type of the stored
/// value. Consequently, values that compare equal but have different types are
/// distinct command ids.
#[derive(Clone)]
pub struct CommandId {
    inner: StructuralKey,
}

impl CommandId {
    /// Creates an id from an owned, structurally comparable value.
    pub fn new<T>(value: T) -> Self
    where
        T: Eq + Hash + Send + Sync + 'static,
    {
        Self {
            inner: StructuralKey::new(value),
        }
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
        self.inner == other.inner
    }
}

impl Eq for CommandId {}

impl Hash for CommandId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.inner.hash(state);
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
}
