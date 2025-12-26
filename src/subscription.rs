pub mod time;

use std::hash::Hasher;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Handle for a running subscription task
pub struct Handle {
    token: CancellationToken,
    join: JoinHandle<()>,
}

impl Handle {
    pub fn new(token: CancellationToken, join: JoinHandle<()>) -> Self {
        Self { token, join }
    }

    /// Cancel the subscription and wait for task completion
    pub async fn cancel(self) {
        self.token.cancel();
        let _ = self.join.await;
    }
}

pub trait Subscription<Msg>: Send {
    /// Start the subscription and return a handle for cancellation.
    fn start(&self, tx: mpsc::UnboundedSender<Msg>) -> Handle;

    fn hash<H: Hasher>(&self, state: &mut H);
}
