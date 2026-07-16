//! `Subscription`, `SubscriptionSource`, and `SubscriptionId`, split out from
//! `subscription.rs` so the parent module can stay `pub` (hosting the
//! `http`/`mock`/`signal`/`terminal`/`time`/`websocket` submodules) while
//! closing the `subscription::{Subscription, SubscriptionId,
//! SubscriptionSource}` paths. See `runtime::frame_rate` for the same
//! pattern applied to `Runtime`'s scheduling input.

use std::any::{Any, TypeId, type_name};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures::{StreamExt, stream::BoxStream};

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

    /// Get the structural key for this subscription.
    ///
    /// The framework combines this value with the concrete source type when it
    /// constructs the opaque subscription identity.
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
    key: AssertUnwindSafe<Arc<dyn ErasedSubscriptionKey>>,
}

impl Clone for SubscriptionId {
    fn clone(&self) -> Self {
        Self {
            source_type_id: self.source_type_id,
            source_type_name: self.source_type_name,
            key: AssertUnwindSafe(self.key.0.clone()),
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
            key: AssertUnwindSafe(Arc::new(TypedSubscriptionKey(key))),
        }
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
        self.source_type_id == other.source_type_id
            && self.key.0.erased_type_id() == other.key.0.erased_type_id()
            && self.key.0.eq_erased(other.key.0.as_ref())
    }
}

impl Eq for SubscriptionId {}

impl Hash for SubscriptionId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.source_type_id.hash(state);
        self.key.0.erased_type_id().hash(state);
        self.key.0.hash_erased(state);
    }
}

trait ErasedSubscriptionKey: Send + Sync {
    fn as_any(&self) -> &dyn Any;
    fn erased_type_id(&self) -> TypeId;
    fn type_name(&self) -> &'static str;
    fn eq_erased(&self, other: &dyn ErasedSubscriptionKey) -> bool;
    fn hash_erased(&self, state: &mut dyn Hasher);
}

struct TypedSubscriptionKey<T>(T);

impl<T> ErasedSubscriptionKey for TypedSubscriptionKey<T>
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

    fn eq_erased(&self, other: &dyn ErasedSubscriptionKey) -> bool {
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
}
