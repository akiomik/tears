use std::num::NonZeroUsize;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::stream::BoxStream;

use crate::command::{Action, CancelPolicy, CommandId};

use super::channel;
use super::keyed_commands::{KeyedCommands, KeyedPoll, ReceiverEvent};
use super::load::LoadObserver;

/// Input events consumed by the runtime's application loop.
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(super) enum AppInput<Msg> {
    /// A message sent through the application's shared message channel.
    Shared(Msg),
    /// Output sent through a cancellable command's private receiver.
    Keyed(ReceiverEvent<Msg>),
}

/// Stream of application inputs backed by runtime message channels.
pub(super) struct AppInputs<Msg: Send + 'static> {
    shared: channel::Receiver<Msg>,
    keyed: KeyedCommands<Msg>,
}

impl<Msg: Send + 'static> AppInputs<Msg> {
    /// Creates an input stream from the shared message receiver, the keyed
    /// commands' per-command channel capacity, and the shared load observer.
    pub(super) fn new(
        shared: channel::Receiver<Msg>,
        keyed_capacity: Option<NonZeroUsize>,
        observer: LoadObserver,
    ) -> Self {
        Self {
            shared,
            keyed: KeyedCommands::new(keyed_capacity, observer),
        }
    }

    /// Number of messages buffered in the shared channel — the batch event's
    /// `shared_pending` field (RFC 0006 §4.4).
    pub(super) fn shared_pending(&self) -> usize {
        self.shared.len()
    }

    /// Returns the next queued input without waiting for new messages.
    pub(super) fn try_next_ready(&mut self) -> Option<AppInput<Msg>> {
        if let Ok(message) = self.shared.try_recv() {
            return Some(AppInput::Shared(message));
        }
        self.keyed
            .try_next_ready()
            .map(|(_, event)| AppInput::Keyed(event))
    }

    pub(super) fn reconcile_keyed_available(&mut self) {
        self.keyed.reconcile_available();
    }

    pub(super) fn cancel_keyed(&mut self, id: &CommandId) {
        self.keyed.cancel(id);
    }

    pub(super) fn spawn_keyed(
        &mut self,
        id: CommandId,
        policy: CancelPolicy,
        stream: BoxStream<'static, Action<Msg>>,
    ) {
        self.keyed.spawn(id, policy, stream);
    }

    pub(super) fn shutdown_keyed(&mut self) {
        self.keyed.shutdown();
    }

    #[cfg(test)]
    pub(super) fn has_closed_buffered(&self, id: &CommandId) -> bool {
        self.keyed.has_closed_buffered(id)
    }
}

impl<Msg: Send + 'static> futures::Stream for AppInputs<Msg> {
    type Item = AppInput<Msg>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let shared_closed = match self.shared.poll_recv(cx) {
            Poll::Ready(Some(message)) => return Poll::Ready(Some(AppInput::Shared(message))),
            Poll::Ready(None) => true,
            Poll::Pending => false,
        };

        match self.keyed.poll_event(cx) {
            KeyedPoll::Item(_, event) => Poll::Ready(Some(AppInput::Keyed(event))),
            KeyedPoll::Quiescent if shared_closed => Poll::Ready(None),
            KeyedPoll::PendingWithWakeSource | KeyedPoll::Quiescent => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream::{self, StreamExt, pending};
    use tokio::task::yield_now;
    use tokio::time::{Duration, timeout};

    use super::super::keyed_commands::{CommandOutput, ReceiverEvent};
    use crate::test_support::wait_until;

    #[tokio::test]
    async fn test_app_inputs_poll_next_returns_shared_after_send() {
        let (tx, rx) = channel::channel(None);
        let mut inputs = AppInputs::new(rx, None, LoadObserver::default());

        tx.try_send(42).expect("receiver should be open");

        assert_eq!(inputs.next().await, Some(AppInput::Shared(42)));
    }

    #[test]
    fn test_app_inputs_try_next_ready_empty_returns_none() {
        let (_tx, rx) = channel::channel::<i32>(None);
        let mut inputs = AppInputs::new(rx, None, LoadObserver::default());

        assert_eq!(inputs.try_next_ready(), None);
    }

    #[test]
    fn test_app_inputs_try_next_ready_returns_queued_messages() {
        let (tx, rx) = channel::channel(None);
        let mut inputs = AppInputs::new(rx, None, LoadObserver::default());

        tx.try_send(1).expect("receiver should be open");
        tx.try_send(2).expect("receiver should be open");

        assert_eq!(inputs.try_next_ready(), Some(AppInput::Shared(1)));
        assert_eq!(inputs.try_next_ready(), Some(AppInput::Shared(2)));
        assert_eq!(inputs.try_next_ready(), None);
    }

    #[test]
    fn test_app_inputs_try_next_ready_preserves_fifo_order() {
        let (tx, rx) = channel::channel(None);
        let mut inputs = AppInputs::new(rx, None, LoadObserver::default());

        for msg in [1, 2, 3] {
            tx.try_send(msg).expect("receiver should be open");
        }

        assert_eq!(inputs.try_next_ready(), Some(AppInput::Shared(1)));
        assert_eq!(inputs.try_next_ready(), Some(AppInput::Shared(2)));
        assert_eq!(inputs.try_next_ready(), Some(AppInput::Shared(3)));
    }

    #[tokio::test]
    async fn test_app_inputs_poll_next_returns_none_after_sender_closes() {
        let (tx, rx) = channel::channel::<i32>(None);
        let mut inputs = AppInputs::new(rx, None, LoadObserver::default());

        drop(tx);

        assert_eq!(inputs.next().await, None);
    }

    #[tokio::test]
    async fn shared_input_wins_when_keyed_output_is_also_ready() {
        let (tx, rx) = channel::channel(None);
        let mut inputs = AppInputs::new(rx, None, LoadObserver::default());
        let id = CommandId::new("search");
        inputs.spawn_keyed(
            id.clone(),
            CancelPolicy::CancelInFlight,
            stream::iter([Action::Message(2)]).boxed(),
        );
        yield_now().await;
        tx.try_send(1).expect("receiver should be open");

        assert_eq!(inputs.next().await, Some(AppInput::Shared(1)));
        inputs
            .next()
            .await
            .and_then(|input| match input {
                AppInput::Keyed(ReceiverEvent::Output(CommandOutput::Message(2))) => Some(()),
                AppInput::Shared(_)
                | AppInput::Keyed(ReceiverEvent::Output(_) | ReceiverEvent::Closed) => None,
            })
            .expect("keyed output should follow the shared input");
    }

    #[tokio::test]
    async fn shared_input_wins_the_nonwaiting_pull_when_keyed_output_is_also_ready() {
        let (tx, rx) = channel::channel(None);
        let mut inputs = AppInputs::new(rx, None, LoadObserver::default());
        let id = CommandId::new("search");
        inputs.spawn_keyed(
            id.clone(),
            CancelPolicy::CancelInFlight,
            stream::iter([Action::Message(2)]).boxed(),
        );
        wait_until(
            || inputs.has_closed_buffered(&id),
            "keyed output should be buffered before the pull",
        )
        .await;
        tx.try_send(1).expect("receiver should be open");

        assert_eq!(inputs.try_next_ready(), Some(AppInput::Shared(1)));
        assert_eq!(
            inputs.try_next_ready(),
            Some(AppInput::Keyed(ReceiverEvent::Output(
                CommandOutput::Message(2)
            )))
        );
    }

    #[tokio::test]
    async fn shared_input_wins_when_keyed_quit_is_also_ready() {
        let (tx, rx) = channel::channel(None);
        let mut inputs = AppInputs::new(rx, None, LoadObserver::default());
        let id = CommandId::new("quit");
        inputs.spawn_keyed(
            id.clone(),
            CancelPolicy::CancelInFlight,
            stream::iter([Action::Quit]).boxed(),
        );
        wait_until(
            || inputs.has_closed_buffered(&id),
            "keyed quit should be buffered before the pull",
        )
        .await;
        tx.try_send(1).expect("receiver should be open");

        assert_eq!(inputs.try_next_ready(), Some(AppInput::Shared(1)));
        assert_eq!(
            inputs.try_next_ready(),
            Some(AppInput::Keyed(ReceiverEvent::Output(CommandOutput::Quit)))
        );
    }

    #[tokio::test]
    async fn shared_input_wins_the_blocking_pull_when_keyed_quit_is_also_ready() {
        let (tx, rx) = channel::channel(None);
        let mut inputs = AppInputs::new(rx, None, LoadObserver::default());
        let id = CommandId::new("quit");
        inputs.spawn_keyed(
            id.clone(),
            CancelPolicy::CancelInFlight,
            stream::iter([Action::Quit]).boxed(),
        );
        wait_until(
            || inputs.has_closed_buffered(&id),
            "keyed quit should be buffered before the pull",
        )
        .await;
        tx.try_send(1).expect("receiver should be open");

        assert_eq!(inputs.next().await, Some(AppInput::Shared(1)));
        assert_eq!(
            inputs.next().await,
            Some(AppInput::Keyed(ReceiverEvent::Output(CommandOutput::Quit)))
        );
    }

    #[tokio::test]
    async fn closed_shared_channel_and_same_poll_keyed_reconciliation_terminate_the_stream() {
        let (tx, rx) = channel::channel::<i32>(None);
        let mut inputs = AppInputs::new(rx, None, LoadObserver::default());
        inputs.spawn_keyed(
            CommandId::new("empty"),
            CancelPolicy::CancelInFlight,
            stream::empty().boxed(),
        );
        drop(tx);
        yield_now().await;

        let next = timeout(Duration::from_secs(1), inputs.next())
            .await
            .expect("AppInputs should not hang after every sender closes");

        assert_eq!(next, None);
    }

    #[tokio::test]
    async fn try_next_ready_does_not_wait_for_pending_keyed_streams() {
        let (_tx, rx) = channel::channel::<i32>(None);
        let mut inputs = AppInputs::new(rx, None, LoadObserver::default());
        inputs.spawn_keyed(
            CommandId::new("pending"),
            CancelPolicy::CancelInFlight,
            pending().boxed(),
        );

        assert_eq!(inputs.try_next_ready(), None);
    }
}
