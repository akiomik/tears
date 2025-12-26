use std::hash::{Hash, Hasher};
use std::{sync::Arc, time::Duration};

use tokio::task::JoinHandle;
use tokio::{sync::mpsc, time::interval};
use tokio_util::sync::CancellationToken;

use super::{Handle, Subscription};

/// Time-based subscription
pub struct TimeSub<Msg> {
    interval_ms: u64,
    msg_fn: Arc<dyn Fn() -> Msg + Send + Sync>,
}

impl<Msg> TimeSub<Msg> {
    pub fn new<F>(interval_ms: u64, msg_fn: F) -> Self
    where
        F: Fn() -> Msg + Send + Sync + 'static,
    {
        Self {
            interval_ms,
            msg_fn: Arc::new(msg_fn),
        }
    }
}

impl<M: Send + 'static> Subscription<M> for TimeSub<M> {
    fn start(&self, tx: mpsc::UnboundedSender<M>) -> Handle {
        let interval_ms = self.interval_ms;
        let msg_fn = Arc::clone(&self.msg_fn);
        let token = CancellationToken::new();
        let child = token.child_token();

        let join: JoinHandle<()> = tokio::spawn(async move {
            let mut interval = interval(Duration::from_millis(interval_ms));
            loop {
                tokio::select! {
                    _ = child.cancelled() => break,
                    _ = interval.tick() => {
                        let _ = tx.send(msg_fn());
                    }
                }
            }
        });

        Handle::new(token, join)
    }

    fn hash<H: Hasher>(&self, state: &mut H) {
        Hash::hash(&self, state);
    }
}

impl<M> PartialEq for TimeSub<M> {
    fn eq(&self, other: &Self) -> bool {
        self.interval_ms == other.interval_ms
    }
}

impl<M> Eq for TimeSub<M> {}

impl<M> Hash for TimeSub<M> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.interval_ms.hash(state);
    }
}
