//! `Subscription`, `SubscriptionSource`, and `SubscriptionId`, split out from
//! `subscription.rs` so the parent module can stay `pub` (hosting the
//! `http`/`mock`/`signal`/`terminal`/`time`/`websocket` submodules) while
//! closing the `subscription::{Subscription, SubscriptionId,
//! SubscriptionSource}` paths. See `runtime::frame_rate` for the same
//! pattern applied to `Runtime`'s scheduling input.

use std::any::TypeId;

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
    pub fn new(source: impl SubscriptionSource<Output = Msg> + 'static) -> Self {
        let id = source.id();

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
/// use tears::{SubscriptionSource, SubscriptionId};
/// use tears::BoxStream;
/// use futures::{StreamExt, stream};
/// use std::hash::{Hash, Hasher};
///
/// struct MySubscription {
///     interval_ms: u64,
/// }
///
/// impl SubscriptionSource for MySubscription {
///     type Output = ();
///
///     fn stream(&self) -> BoxStream<'static, Self::Output> {
///         stream::empty().boxed()
///     }
///
///     fn id(&self) -> SubscriptionId {
///         let mut hasher = std::collections::hash_map::DefaultHasher::new();
///         self.interval_ms.hash(&mut hasher);
///         SubscriptionId::of::<Self>(hasher.finish())
///     }
/// }
/// ```
pub trait SubscriptionSource: Send {
    /// The type of messages this subscription produces.
    type Output;

    /// Create the stream of messages for this subscription.
    fn stream(&self) -> BoxStream<'static, Self::Output>;

    /// Get a unique identifier for this subscription.
    ///
    /// Subscriptions with the same ID are considered identical.
    fn id(&self) -> SubscriptionId;
}

/// A unique identifier for a subscription.
///
/// Two subscriptions with the same ID are considered identical.
/// The ID includes type information and a hash value to prevent collisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriptionId {
    pub(super) type_id: TypeId,
    hash: u64,
}

impl SubscriptionId {
    /// Create a subscription ID from a type and a hash value.
    ///
    /// Typically used when implementing [`SubscriptionSource::id`].
    ///
    /// # Examples
    ///
    /// ```
    /// use tears::SubscriptionId;
    /// use std::hash::{Hash, Hasher};
    /// use std::collections::hash_map::DefaultHasher;
    ///
    /// struct MySubscription { interval_ms: u64 }
    ///
    /// impl MySubscription {
    ///     fn compute_id(&self) -> SubscriptionId {
    ///         let mut hasher = DefaultHasher::new();
    ///         self.interval_ms.hash(&mut hasher);
    ///         SubscriptionId::of::<Self>(hasher.finish())
    ///     }
    /// }
    /// ```
    #[must_use]
    pub fn of<T: 'static>(hash: u64) -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            hash,
        }
    }
}
