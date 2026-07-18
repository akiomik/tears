//! Shared structural-key erasure for command and subscription lifecycle IDs.
//!
//! This implements the private helper described in RFC 0005 section 8.1 while
//! letting each ID apply the namespace rules required by its lifecycle model.

use std::any::{Any, TypeId, type_name};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// An owned, type-erased key with structural equality and hashing.
///
/// The default [`Eq`] and [`Hash`] implementations include the concrete key
/// type. Owners that already provide a type namespace may use the value-only
/// operations to avoid hashing the same type distinction twice.
#[derive(Clone)]
pub struct StructuralKey {
    inner: Arc<dyn ErasedStructuralKey>,
}

impl fmt::Debug for StructuralKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StructuralKey")
            .field("type", &self.type_name())
            .finish_non_exhaustive()
    }
}

impl StructuralKey {
    pub fn new<T>(value: T) -> Self
    where
        T: Eq + Hash + Send + Sync + 'static,
    {
        Self {
            inner: Arc::new(TypedStructuralKey(value)),
        }
    }

    pub fn type_name(&self) -> &'static str {
        self.inner.type_name()
    }

    /// Compares only the erased structural value.
    ///
    /// The erased implementation still downcasts safely before comparing. This
    /// operation is intended for an owner whose own namespace guarantees the
    /// concrete key type, such as a subscription source type and its associated
    /// `Key`.
    pub fn value_eq(&self, other: &Self) -> bool {
        self.inner.eq_erased(other.inner.as_ref())
    }

    /// Hashes only the erased structural value.
    ///
    /// The caller must provide the namespace that fixes the concrete key type.
    pub fn hash_value<H: Hasher>(&self, state: &mut H) {
        self.inner.hash_erased(state);
    }

    fn type_id(&self) -> TypeId {
        self.inner.erased_type_id()
    }
}

impl PartialEq for StructuralKey {
    fn eq(&self, other: &Self) -> bool {
        self.type_id() == other.type_id() && self.value_eq(other)
    }
}

impl Eq for StructuralKey {}

impl Hash for StructuralKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.type_id().hash(state);
        self.hash_value(state);
    }
}

/// An ordered sequence of structural composition-boundary segments.
///
/// This implements the scope path described in RFC 0005 section 2.3.
/// Segments are recorded in call order (the order successive `scoped()`
/// calls append them), which is a stable internal convention: comparing two
/// paths only needs to agree with itself, not with any particular
/// outermost-to-innermost display order. Path equality therefore compares
/// segment count and each segment in order, so reversing two unequal
/// segments changes identity while reversing two equal segments preserves
/// it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ScopePath(Vec<StructuralKey>);

impl ScopePath {
    pub const fn empty() -> Self {
        Self(Vec::new())
    }

    /// Returns a new path with `scope` appended after this path's existing
    /// segments, erasing it into a fresh [`StructuralKey`].
    pub fn appended<Scope>(&self, scope: Scope) -> Self
    where
        Scope: Eq + Hash + Send + Sync + 'static,
    {
        self.appended_key(StructuralKey::new(scope))
    }

    /// Returns a new path with an already-erased `segment` appended.
    ///
    /// Lets a caller erase one scope value once and apply it to several ids
    /// sharing one composition boundary (for example a command's spawn key
    /// and every explicit cancel id), without requiring the scope type
    /// itself to be `Clone`.
    pub fn appended_key(&self, segment: StructuralKey) -> Self {
        let mut segments = self.0.clone();
        segments.push(segment);
        Self(segments)
    }
}

trait ErasedStructuralKey: Send + Sync {
    fn as_any(&self) -> &dyn Any;
    fn erased_type_id(&self) -> TypeId;
    fn type_name(&self) -> &'static str;
    fn eq_erased(&self, other: &dyn ErasedStructuralKey) -> bool;
    fn hash_erased(&self, state: &mut dyn Hasher);
}

struct TypedStructuralKey<T>(T);

impl<T> ErasedStructuralKey for TypedStructuralKey<T>
where
    T: Eq + Hash + Send + Sync + 'static,
{
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn erased_type_id(&self) -> TypeId {
        TypeId::of::<T>()
    }

    fn type_name(&self) -> &'static str {
        type_name::<T>()
    }

    fn eq_erased(&self, other: &dyn ErasedStructuralKey) -> bool {
        other
            .as_any()
            .downcast_ref::<Self>()
            .is_some_and(|other| self.0 == other.0)
    }

    fn hash_erased(&self, mut state: &mut dyn Hasher) {
        self.0.hash(&mut state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::hash_map::DefaultHasher;

    fn hash(key: &StructuralKey) -> u64 {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        hasher.finish()
    }

    fn hash_value(key: &StructuralKey) -> u64 {
        let mut hasher = DefaultHasher::new();
        key.hash_value(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn equality_is_structural_and_type_sensitive() {
        assert_eq!(StructuralKey::new(7_u64), StructuralKey::new(7_u64));
        assert_ne!(StructuralKey::new(7_u64), StructuralKey::new(8_u64));
        assert_ne!(StructuralKey::new(7_u64), StructuralKey::new(7_i64));
        assert!(!StructuralKey::new(7_u64).value_eq(&StructuralKey::new(7_i64)));
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

        let first = StructuralKey::new(Collision(1));
        let second = StructuralKey::new(Collision(2));

        assert_eq!(hash(&first), hash(&second));
        assert_ne!(first, second);
    }

    #[test]
    fn value_only_hash_does_not_add_the_key_type_namespace() {
        #[derive(Eq, PartialEq)]
        struct First;

        #[derive(Eq, PartialEq)]
        struct Second;

        impl Hash for First {
            fn hash<H: Hasher>(&self, state: &mut H) {
                0_u8.hash(state);
            }
        }

        impl Hash for Second {
            fn hash<H: Hasher>(&self, state: &mut H) {
                0_u8.hash(state);
            }
        }

        assert_eq!(
            hash_value(&StructuralKey::new(First)),
            hash_value(&StructuralKey::new(Second))
        );
    }

    fn hash_path(path: &ScopePath) -> u64 {
        let mut hasher = DefaultHasher::new();
        path.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn empty_path_is_distinct_from_a_one_segment_path() {
        let empty = ScopePath::empty();
        let one = ScopePath::empty().appended(1_u64);

        assert_ne!(empty, one);
    }

    #[test]
    fn equal_segments_in_equal_order_are_equal() {
        let first = ScopePath::empty().appended(1_u64).appended(2_u64);
        let second = ScopePath::empty().appended(1_u64).appended(2_u64);

        assert_eq!(first, second);
        assert_eq!(hash_path(&first), hash_path(&second));
    }

    #[test]
    fn reversing_two_unequal_segments_changes_identity() {
        let forward = ScopePath::empty().appended(1_u64).appended(2_u64);
        let backward = ScopePath::empty().appended(2_u64).appended(1_u64);

        assert_ne!(forward, backward);
    }

    #[test]
    fn reversing_two_equal_segments_preserves_identity() {
        let forward = ScopePath::empty().appended(1_u64).appended(1_u64);
        let backward = ScopePath::empty().appended(1_u64).appended(1_u64);

        assert_eq!(forward, backward);
    }

    #[test]
    fn segment_type_differences_affect_equality() {
        let as_u64 = ScopePath::empty().appended(1_u64);
        let as_i64 = ScopePath::empty().appended(1_i64);

        assert_ne!(as_u64, as_i64);
    }

    #[test]
    fn hash_collisions_do_not_make_unequal_paths_equal() {
        #[derive(Eq, PartialEq)]
        struct Collision(u8);

        impl Hash for Collision {
            fn hash<H: Hasher>(&self, state: &mut H) {
                0_u8.hash(state);
            }
        }

        let first = ScopePath::empty().appended(Collision(1));
        let second = ScopePath::empty().appended(Collision(2));

        assert_eq!(hash_path(&first), hash_path(&second));
        assert_ne!(first, second);
    }

    #[test]
    fn appended_key_shares_one_erasure_across_paths() {
        let segment = StructuralKey::new(7_u64);
        let first = ScopePath::empty().appended_key(segment.clone());
        let second = ScopePath::empty().appended_key(segment);

        assert_eq!(first, second);
    }
}
