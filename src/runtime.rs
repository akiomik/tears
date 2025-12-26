use std::time::Duration;

use color_eyre::eyre::Result;
use futures::stream::StreamExt;
use ratatui::prelude::Backend;
use tokio::{sync::mpsc, time::sleep};

use crate::{application::Application, command::Action, subscription::SubscriptionManager};

#[repr(transparent)]
struct Instance<A: Application> {
    inner: A,
}

pub struct Runtime<A: Application> {
    app: Instance<A>,
    msg_tx: mpsc::UnboundedSender<A::Message>,
    msg_rx: mpsc::UnboundedReceiver<A::Message>,
    subscription_manager: SubscriptionManager<A::Message>,
}

impl<A: Application> Runtime<A> {
    pub fn new(app: A) -> Self {
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();
        let instance = Instance { inner: app };
        let subscription_manager = SubscriptionManager::new(msg_tx.clone());

        Self {
            app: instance,
            msg_tx,
            msg_rx,
            subscription_manager,
        }
    }

    async fn process_messages(&mut self) -> bool {
        while let Ok(msg) = self.msg_rx.try_recv() {
            let cmd = self.app.inner.update(msg);

            if let Some(mut stream) = cmd.stream {
                // Process the command stream immediately to check for quit
                if let Some(action) = stream.next().await {
                    match action {
                        Action::Message(msg) => {
                            let _ = self.msg_tx.send(msg);
                        }
                        Action::Quit => {
                            // Return immediately on quit
                            return true;
                        }
                    }
                }
            }

            // Restart subscriptions if needed
            self.subscription_manager
                .update(self.app.inner.subscriptions());
        }

        false
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

            // Process messages and check if quit was requested
            if self.process_messages().await {
                break;
            }

            // Wait for next frame
            sleep(frame_duration).await;
        }

        // cancel all subscriptions
        self.subscription_manager.shutdown();

        Ok(())
    }
}
