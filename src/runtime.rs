use std::{collections::HashMap, hash::Hash, marker::PhantomData, time::Duration};

use color_eyre::eyre::Result;
use futures::stream::StreamExt;
use ratatui::prelude::Backend;
use tokio::{sync::mpsc, time::sleep};

use crate::{
    application::Application,
    subscription::{Handle, Subscription},
};

#[repr(transparent)]
struct Instance<A: Application<S>, S: Subscription<A::Message> + Hash + Eq> {
    inner: A,
    _phantom: PhantomData<S>,
}

pub struct Runtime<A: Application<S>, S: Subscription<A::Message> + Hash + Eq> {
    app: Instance<A, S>,
    tx: mpsc::UnboundedSender<A::Message>,
    rx: mpsc::UnboundedReceiver<A::Message>,
    #[allow(dead_code)]
    handles: HashMap<S, Handle>,
}

impl<A: Application<S>, S: Subscription<A::Message> + Hash + Eq> Runtime<A, S> {
    pub fn new(app: A) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let instance = Instance {
            inner: app,
            _phantom: PhantomData,
        };

        // Start subscriptions
        let mut handles = HashMap::new();
        for sub in instance.inner.subscriptions() {
            let handle = sub.start(tx.clone());
            handles.insert(sub, handle);
        }

        Self {
            app: instance,
            tx,
            rx,
            handles,
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

            // TODO: Restart subscriptions if needed
        }
    }

    pub async fn run<B: Backend>(
        mut self,
        terminal: &mut ratatui::Terminal<B>,
        frame_rate: u32,
    ) -> Result<()> {
        let frame_duration = Duration::from_millis(1000 / frame_rate as u64);

        loop {
            terminal.draw(|frame| {
                self.app.inner.view(frame);
            })?;

            self.process_messages().await;

            sleep(frame_duration).await;
        }

        // NOTE: Currently unreacheable
        // // Graceful shutdown: cancel all subscriptions
        // for (_, handle) in self.handles {
        //     handle.cancel().await;
        // }
        //
        // Ok(())
    }
}
