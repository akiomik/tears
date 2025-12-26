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
    quit_tx: mpsc::Sender<()>,
    quit_rx: mpsc::Receiver<()>,
    subscription_manager: SubscriptionManager<A::Message>,
}

impl<A: Application> Runtime<A> {
    pub fn new(app: A) -> Self {
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();
        let (quit_tx, quit_rx) = mpsc::channel(1);
        let instance = Instance { inner: app };
        let subscription_manager = SubscriptionManager::new(msg_tx.clone());

        Self {
            app: instance,
            msg_tx,
            msg_rx,
            quit_tx,
            quit_rx,
            subscription_manager,
        }
    }

    fn process_messages(&mut self) {
        while let Ok(msg) = self.msg_rx.try_recv() {
            let cmd = self.app.inner.update(msg);

            let msg_tx = self.msg_tx.clone();
            let quit_tx = self.quit_tx.clone();
            if let Some(mut stream) = cmd.stream {
                tokio::spawn(async move {
                    if let Some(action) = stream.next().await {
                        match action {
                            Action::Message(msg) => {
                                let _ = msg_tx.send(msg);
                            }
                            Action::Quit => {
                                let _ = quit_tx.send(()).await;
                            }
                        }
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

            self.process_messages();

            if self.quit_rx.try_recv().is_ok() {
                break;
            }

            sleep(frame_duration).await;
        }

        // cancel all subscriptions
        self.subscription_manager.shutdown();

        Ok(())
    }
}
