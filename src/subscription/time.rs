use std::hash::{DefaultHasher, Hash, Hasher};
use std::marker::PhantomData;
use std::time::Duration;

use futures::stream::BoxStream;
use futures::{StreamExt, stream};
use tokio::time::sleep;

use super::{SubscriptionId, SubscriptionInner};

#[derive(Debug, Clone)]
pub enum Message {
    Tick,
}

/// Time-based subscription
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeSub<Msg> {
    interval_ms: u64,
    _phantom: PhantomData<Msg>,
}

impl<Msg> TimeSub<Msg> {
    pub fn new(interval_ms: u64) -> Self {
        Self {
            interval_ms,
            _phantom: PhantomData,
        }
    }
}

impl SubscriptionInner for TimeSub<Message> {
    type Output = Message;

    fn stream(&self) -> BoxStream<'static, Message> {
        let interval_ms = self.interval_ms;
        stream::unfold((), move |_| async move {
            let interval = Duration::from_millis(interval_ms);
            sleep(interval).await;
            Some((Message::Tick, ()))
        })
        .boxed()
    }

    fn id(&self) -> SubscriptionId {
        let mut hasher = DefaultHasher::new();
        self.interval_ms.hash(&mut hasher);
        SubscriptionId(hasher.finish())
    }
}

impl<M> Hash for TimeSub<M> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.interval_ms.hash(state);
    }
}
