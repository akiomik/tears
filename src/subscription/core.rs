//! `Subscription`, `SubscriptionSource`, and `SubscriptionId`, split out from
//! `subscription.rs` so the parent module can stay `pub` (hosting the
//! `http`/`mock`/`signal`/`terminal`/`time`/`websocket` submodules) while
//! closing the `subscription::{Subscription, SubscriptionId,
//! SubscriptionSource}` paths. See `command::core` for the same
//! pattern applied to `Runtime`'s scheduling input.

use std::any::{TypeId, type_name};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::panic::AssertUnwindSafe;

use futures::{StreamExt, stream::BoxStream};

use crate::structural_key::{ScopePath, StructuralKey};

/// A subscription represents an ongoing source of messages.
///
/// Subscriptions are used to listen to external events that occur over time, such as:
/// - Keyboard and mouse input
/// - Timer ticks
/// - WebSocket messages
/// - File system changes
/// - Network events
///
/// Unlike commands which are one-time operations, subscriptions continue to produce
/// messages until they are cancelled.
///
/// # Example
///
/// ```
/// use std::num::NonZeroU64;
/// use tears::Subscription;
/// use tears::subscription::time::Timer;
///
/// enum Message {
///     Tick,
/// }
///
/// // Create a subscription that sends a message every second
/// let sub = Subscription::new(Timer::new(NonZeroU64::new(1000).expect("non-zero")))
///     .map(|_| Message::Tick);
/// ```
pub struct Subscription<Msg: 'static> {
    pub(super) id: SubscriptionId,
    pub(super) spawn: Box<dyn FnOnce() -> BoxStream<'static, Msg> + Send>,
}

impl<Msg: 'static> Subscription<Msg> {
    /// Create a new subscription from a type implementing [`SubscriptionSource`].
    ///
    /// # Examples
    ///
    /// ```
    /// use std::num::NonZeroU64;
    /// use tears::Subscription;
    /// use tears::subscription::time::Timer;
    ///
    /// let sub = Subscription::new(Timer::new(NonZeroU64::new(1000).expect("non-zero")));
    /// ```
    #[must_use]
    pub fn new<Source>(source: Source) -> Self
    where
        Source: SubscriptionSource<Output = Msg> + 'static,
    {
        let id = SubscriptionId::from_source::<Source>(source.key());

        Self {
            id,
            spawn: Box::new(move || source.stream().boxed()),
        }
    }

    /// Transform the messages produced by this subscription.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::num::NonZeroU64;
    /// use tears::Subscription;
    /// use tears::subscription::time::Timer;
    ///
    /// enum AppMessage { TimerTick }
    ///
    /// let sub = Subscription::new(Timer::new(NonZeroU64::new(1000).expect("non-zero")))
    ///     .map(|_| AppMessage::TimerTick);
    /// ```
    #[must_use]
    pub fn map<F, NewMsg>(self, f: F) -> Subscription<NewMsg>
    where
        F: Fn(Msg) -> NewMsg + Send + 'static,
        Msg: 'static,
        NewMsg: 'static,
    {
        let spawn = self.spawn;
        Subscription {
            id: self.id,
            spawn: Box::new(move || {
                let stream = spawn();
                stream.map(f).boxed()
            }),
        }
    }

    /// Qualifies this subscription's identity with one structural scope
    /// segment, expressing that this instance belongs to a distinct child
    /// composition boundary (see RFC 0005 section 4.2).
    ///
    /// Chaining `scoped` calls nests segments in call order: the last call
    /// becomes the outermost segment. Reversing the order of two unequal
    /// scope values produces a different identity, so
    /// `sub.scoped(a).scoped(b)` and `sub.scoped(b).scoped(a)` are distinct
    /// subscriptions when `a != b`. `scoped` and [`Subscription::map`]
    /// commute: applying them in either order around the same call produces
    /// the same identity and output.
    ///
    /// Applying the same scope to two composed child instances that reuse
    /// the same local source and key still aliases them; give each instance
    /// its own scope value (for example a per-instance id) to keep them
    /// independent.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::num::NonZeroU64;
    /// use tears::Subscription;
    /// use tears::subscription::time::Timer;
    ///
    /// enum Message {
    ///     Tick(u32),
    /// }
    ///
    /// let pane_id = 1;
    /// let sub = Subscription::new(Timer::new(NonZeroU64::new(1000).expect("non-zero")))
    ///     .map(move |_| Message::Tick(pane_id))
    ///     .scoped(pane_id);
    /// ```
    #[must_use = "scoped consumes the subscription and returns the modified value"]
    pub fn scoped<Scope>(mut self, scope: Scope) -> Self
    where
        Scope: Eq + Hash + Send + Sync + 'static,
    {
        self.id.scope = AssertUnwindSafe(self.id.scope.prefixed(scope));
        self
    }

    /// The subscription's declared identity, read without starting its source.
    /// Used by [`TestStore::subscription_ids`](crate::testing::TestStore::subscription_ids).
    pub(crate) const fn id(&self) -> &SubscriptionId {
        &self.id
    }

    /// Starts this declaration's source and returns its stream.
    ///
    /// The one way out of this module for the spawner, and it *consumes* the
    /// declaration, so a source is started at most once per value — which is
    /// how "the spawner is invoked exactly once per admission" (RFC 0012
    /// INV-SE1) holds structurally rather than by the caller's discipline.
    /// The call runs wherever the admitting code runs, which the kernel
    /// fixes to its driving task so a lazy source constructor's panic
    /// unwinds there (RFC 0011 §4.3) rather than inside a runtime-owned
    /// task, where it would be contained.
    pub(crate) fn into_stream(self) -> BoxStream<'static, Msg> {
        (self.spawn)()
    }
}

impl<A: SubscriptionSource<Output = Msg> + 'static, Msg> From<A> for Subscription<Msg> {
    fn from(value: A) -> Self {
        Self::new(value)
    }
}

/// Trait for types that can be used as subscription sources.
///
/// # Example
///
/// ```
/// use tears::SubscriptionSource;
/// use tears::BoxStream;
/// use futures::{StreamExt, stream};
///
/// struct MySubscription {
///     interval_ms: u64,
/// }
///
/// impl SubscriptionSource for MySubscription {
///     type Output = ();
///     type Key = u64;
///
///     fn stream(&self) -> BoxStream<'static, Self::Output> {
///         stream::empty().boxed()
///     }
///
///     fn key(&self) -> Self::Key {
///         self.interval_ms
///     }
/// }
/// ```
pub trait SubscriptionSource: Send {
    /// The type of messages this subscription produces.
    type Output;

    /// The owned structural key for one subscription lifecycle.
    ///
    /// This key must be stable across equivalent evaluations of a source. A
    /// changing key expresses a new lifecycle, causing the old subscription to
    /// stop and a new one to start during reconciliation.
    type Key: Eq + Hash + Send + Sync + 'static;

    /// Create the stream of messages for this subscription.
    fn stream(&self) -> BoxStream<'static, Self::Output>;

    /// Get the owned structural key for this subscription.
    ///
    /// The framework combines this value with the concrete source type when it
    /// constructs the opaque subscription identity.
    ///
    /// This method must return equal keys for the same logical source identity
    /// across calls, including when fresh source values are constructed by
    /// successive [`Application::subscriptions`](crate::Application::subscriptions)
    /// evaluations. A per-instance source must generate its instance token
    /// once, store it, and return the stored token here. Generating a fresh key
    /// on each evaluation aborts and respawns the subscription during
    /// reconciliation.
    fn key(&self) -> Self::Key;
}

/// A unique identifier for a subscription.
///
/// Two subscriptions with the same ID are considered identical.
///
/// IDs compare their concrete source type and original structural key. Hashing
/// is used only for indexing and never makes unequal keys equal.
pub struct SubscriptionId {
    pub(super) source_type_id: TypeId,
    source_type_name: &'static str,
    key: AssertUnwindSafe<StructuralKey>,
    scope: AssertUnwindSafe<ScopePath>,
}

impl Clone for SubscriptionId {
    fn clone(&self) -> Self {
        Self {
            source_type_id: self.source_type_id,
            source_type_name: self.source_type_name,
            key: AssertUnwindSafe(self.key.0.clone()),
            scope: AssertUnwindSafe(self.scope.0.clone()),
        }
    }
}

impl SubscriptionId {
    fn from_source<Source>(key: Source::Key) -> Self
    where
        Source: SubscriptionSource + 'static,
    {
        Self {
            source_type_id: TypeId::of::<Source>(),
            source_type_name: type_name::<Source>(),
            key: AssertUnwindSafe(StructuralKey::new(key)),
            scope: AssertUnwindSafe(ScopePath::empty()),
        }
    }

    /// This id's scope path, used to attribute a subscription run to the
    /// composition boundary that declared it so a prefix teardown selects
    /// it alongside command runs (RFC 0014 §4.1).
    pub(crate) const fn scope(&self) -> &ScopePath {
        &self.scope.0
    }
}

impl fmt::Debug for SubscriptionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SubscriptionId")
            .field("source", &self.source_type_name)
            .field("key", &self.key.0.type_name())
            .finish_non_exhaustive()
    }
}

impl PartialEq for SubscriptionId {
    fn eq(&self, other: &Self) -> bool {
        // One concrete source type has exactly one associated `Key` type, so
        // the source namespace already fixes the erased key type.
        self.source_type_id == other.source_type_id
            && self.key.0.value_eq(&other.key.0)
            && self.scope.0 == other.scope.0
    }
}

impl Eq for SubscriptionId {}

impl Hash for SubscriptionId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Hash the source namespace once; its associated `Key` type does not
        // need a second type namespace in this identity.
        self.source_type_id.hash(state);
        self.key.0.hash_value(state);
        self.scope.0.hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::hash_map::DefaultHasher;
    use std::panic::{RefUnwindSafe, UnwindSafe};

    use futures::{StreamExt, stream};

    struct SourceA(u8);
    struct SourceB(u8);

    impl SubscriptionSource for SourceA {
        type Output = ();
        type Key = u8;

        fn stream(&self) -> BoxStream<'static, Self::Output> {
            stream::empty().boxed()
        }

        fn key(&self) -> Self::Key {
            self.0
        }
    }

    impl SubscriptionSource for SourceB {
        type Output = ();
        type Key = u8;

        fn stream(&self) -> BoxStream<'static, Self::Output> {
            stream::empty().boxed()
        }

        fn key(&self) -> Self::Key {
            self.0
        }
    }

    #[derive(Eq, PartialEq)]
    struct Collision(u8);

    impl Hash for Collision {
        fn hash<H: Hasher>(&self, state: &mut H) {
            0_u8.hash(state);
        }
    }

    struct CollisionSource(Collision);

    impl SubscriptionSource for CollisionSource {
        type Output = ();
        type Key = Collision;

        fn stream(&self) -> BoxStream<'static, Self::Output> {
            stream::empty().boxed()
        }

        fn key(&self) -> Self::Key {
            Collision(self.0.0)
        }
    }

    fn hash(id: &SubscriptionId) -> u64 {
        let mut hasher = DefaultHasher::new();
        id.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn subscription_ids_are_structural_and_source_namespaced() {
        let first = Subscription::new(SourceA(1)).id;
        let equal = Subscription::new(SourceA(1)).id;
        let different_key = Subscription::new(SourceA(2)).id;
        let different_source = Subscription::new(SourceB(1)).id;

        assert_eq!(first, equal);
        assert_ne!(first, different_key);
        assert_ne!(first, different_source);
        assert_eq!(hash(&first), hash(&equal));
        assert_eq!(first, first.clone());
    }

    #[test]
    fn hash_collisions_do_not_make_subscription_ids_equal() {
        let first = Subscription::new(CollisionSource(Collision(1))).id;
        let second = Subscription::new(CollisionSource(Collision(2))).id;

        assert_eq!(hash(&first), hash(&second));
        assert_ne!(first, second);
    }

    #[test]
    fn debug_only_reports_type_names() {
        let first = format!("{:?}", Subscription::new(SourceA(1)).id);
        let second = format!("{:?}", Subscription::new(SourceA(2)).id);

        assert_eq!(first, second);
        assert!(first.contains("SourceA"));
        assert!(!first.contains('1'));
    }

    #[test]
    fn subscription_id_preserves_marker_traits() {
        fn assert_traits<T: Send + Sync + UnwindSafe + RefUnwindSafe>() {}
        assert_traits::<SubscriptionId>();
    }

    #[test]
    fn unscoped_tuple_local_key_does_not_alias_a_scoped_identity() {
        struct TupleKeyedSource;

        impl SubscriptionSource for TupleKeyedSource {
            type Output = ();
            type Key = (&'static str, u8);

            fn stream(&self) -> BoxStream<'static, Self::Output> {
                stream::empty().boxed()
            }

            fn key(&self) -> Self::Key {
                ("pane-1", 1)
            }
        }

        let tupled = Subscription::new(TupleKeyedSource).id;
        let scoped = Subscription::new(SourceA(1)).scoped("pane-1").id;

        assert_ne!(tupled, scoped);
    }

    #[test]
    fn scoped_makes_independent_child_instances_distinct() {
        let first = Subscription::new(SourceA(1)).scoped("pane-1").id;
        let second = Subscription::new(SourceA(1)).scoped("pane-2").id;

        assert_ne!(first, second);
    }

    #[test]
    fn scoped_with_equal_scope_and_local_key_is_equal() {
        let first = Subscription::new(SourceA(1)).scoped("pane-1").id;
        let second = Subscription::new(SourceA(1)).scoped("pane-1").id;

        assert_eq!(first, second);
        assert_eq!(hash(&first), hash(&second));
    }

    #[test]
    fn scoped_differs_from_unscoped() {
        let unscoped = Subscription::new(SourceA(1)).id;
        let scoped = Subscription::new(SourceA(1)).scoped("pane-1").id;

        assert_ne!(unscoped, scoped);
    }

    #[test]
    fn scope_segment_type_differences_affect_equality() {
        let as_str = Subscription::new(SourceA(1)).scoped("1").id;
        let as_u32 = Subscription::new(SourceA(1)).scoped(1_u32).id;

        assert_ne!(as_str, as_u32);
    }

    #[test]
    fn reversing_two_unequal_scope_segments_changes_identity() {
        let forward = Subscription::new(SourceA(1))
            .scoped("inner")
            .scoped("outer")
            .id;
        let backward = Subscription::new(SourceA(1))
            .scoped("outer")
            .scoped("inner")
            .id;

        assert_ne!(forward, backward);
    }

    #[test]
    fn reversing_two_equal_scope_segments_preserves_identity() {
        let forward = Subscription::new(SourceA(1))
            .scoped("pane")
            .scoped("pane")
            .id;
        let backward = Subscription::new(SourceA(1))
            .scoped("pane")
            .scoped("pane")
            .id;

        assert_eq!(forward, backward);
    }

    #[test]
    fn map_and_scoped_placement_are_equivalent() {
        let before = Subscription::new(SourceA(1))
            .map(|()| 1)
            .scoped("pane-1")
            .id;
        let after = Subscription::new(SourceA(1))
            .scoped("pane-1")
            .map(|()| 1)
            .id;

        assert_eq!(before, after);
    }

    #[test]
    fn scope_hash_collisions_do_not_make_scoped_ids_equal() {
        let first = Subscription::new(SourceA(1)).scoped(Collision(1)).id;
        let second = Subscription::new(SourceA(1)).scoped(Collision(2)).id;

        assert_eq!(hash(&first), hash(&second));
        assert_ne!(first, second);
    }
}
