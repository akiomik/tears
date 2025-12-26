pub mod terminal;
pub mod time;

use std::{
    collections::{HashMap, HashSet},
    hash::Hash,
};

use futures::{StreamExt, stream::BoxStream};
use tokio::{
    sync::mpsc::{self},
    task::JoinHandle,
};

pub struct Subscription<Msg: 'static> {
    pub(super) id: SubscriptionId,
    pub(super) stream: BoxStream<'static, Msg>,
}

impl<Msg: 'static> Subscription<Msg> {
    pub fn new(inner: impl SubscriptionInner<Output = Msg>) -> Subscription<Msg> {
        let id = inner.id();

        Self {
            id,
            stream: inner.stream().boxed(),
        }
    }

    pub fn map<F, NewMsg>(self, f: F) -> Subscription<NewMsg>
    where
        F: Fn(Msg) -> NewMsg + Send + 'static,
        Msg: 'static,
        NewMsg: 'static,
    {
        let stream: BoxStream<'static, NewMsg> = self.stream.map(f).boxed();
        Subscription {
            id: self.id,
            stream,
        }
    }
}

impl<A: SubscriptionInner<Output = Msg>, Msg> From<A> for Subscription<Msg> {
    fn from(value: A) -> Self {
        Self::new(value)
    }
}

pub trait SubscriptionInner: Send {
    type Output;

    fn stream(&self) -> BoxStream<'static, Self::Output>;

    fn id(&self) -> SubscriptionId;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriptionId(u64);

struct RunningSubscription {
    handle: JoinHandle<()>,
}

pub struct SubscriptionManager<Msg> {
    running: HashMap<SubscriptionId, RunningSubscription>,
    msg_sender: mpsc::UnboundedSender<Msg>,
}

impl<Msg: Send + 'static> SubscriptionManager<Msg> {
    pub fn new(msg_sender: mpsc::UnboundedSender<Msg>) -> Self {
        Self {
            running: HashMap::new(),
            msg_sender,
        }
    }

    pub fn update<I>(&mut self, subscriptions: I)
    where
        I: IntoIterator<Item = Subscription<Msg>>,
    {
        let mut new_subs: HashMap<_, _> = subscriptions
            .into_iter()
            .map(|sub| (sub.id, sub.stream))
            .collect();
        let new_ids: HashSet<_> = new_subs.keys().copied().collect();
        let current_ids: HashSet<_> = self.running.keys().copied().collect();

        let to_remove: Vec<_> = current_ids.difference(&new_ids).copied().collect();
        let to_add: Vec<_> = new_ids.difference(&current_ids).copied().collect();

        for id in to_remove {
            if let Some(running) = self.running.remove(&id) {
                running.handle.abort();
            }
        }

        for id in to_add {
            if let Some(stream) = new_subs.remove(&id) {
                let handle = self.spawn_subscription(stream);
                self.running.insert(id, RunningSubscription { handle });
            }
        }
    }

    fn spawn_subscription(&self, mut stream: BoxStream<'static, Msg>) -> JoinHandle<()> {
        let sender = self.msg_sender.clone();

        tokio::spawn(async move {
            while let Some(msg) = stream.next().await {
                if sender.send(msg).is_err() {
                    break;
                }
            }
        })
    }

    pub fn shutdown(&mut self) {
        for (_, running) in self.running.drain() {
            running.handle.abort();
        }
    }
}
