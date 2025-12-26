use std::time::Duration;

use color_eyre::eyre::Result;
use futures::stream::StreamExt;
use ratatui::prelude::Backend;
use tokio::{sync::mpsc, time::sleep};

use crate::{application::Application, subscription::SubscriptionManager};

#[repr(transparent)]
struct Instance<A: Application> {
    inner: A,
}

pub struct Runtime<A: Application> {
    app: Instance<A>,
    tx: mpsc::UnboundedSender<A::Message>,
    rx: mpsc::UnboundedReceiver<A::Message>,
    subscription_manager: SubscriptionManager<A::Message>,
}

impl<A: Application> Runtime<A> {
    pub fn new(app: A) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let instance = Instance { inner: app };
        let subscription_manager = SubscriptionManager::new(tx.clone());

        Self {
            app: instance,
            tx,
            rx,
            subscription_manager,
        }
    }

    async fn process_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            let cmd = self.app.inner.update(msg);

            let tx_clone = self.tx.clone();
            if let Some(mut stream) = cmd.stream {
                tokio::spawn(async move {
                    if let Some(value) = stream.next().await {
                        let _ = tx_clone.send(value);
                    }
                });
            }

            // Restart subscriptions if needed
            self.subscription_manager
                .update(self.app.inner.subscriptions());
        }
    }

    pub async fn run<B: Backend>(
        mut self,
        terminal: &mut ratatui::Terminal<B>,
        frame_rate: u32,
    ) -> Result<()> {
        let frame_duration = Duration::from_millis(1000 / frame_rate as u64);

        let subscriptions = self.app.inner.subscriptions();
        self.subscription_manager.update(subscriptions);

        loop {
            terminal.draw(|frame| {
                self.app.inner.view(frame);
            })?;

            self.process_messages().await;

            sleep(frame_duration).await;
        }

        // // cancel all subscriptions
        // self.subscription_manager.shutdown();
        //
        // Ok(())
    }
}
