use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::sync::mpsc;

#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(super) enum AppInput<Msg> {
    Shared(Msg),
}

pub(super) struct AppInputs<Msg> {
    shared: mpsc::UnboundedReceiver<Msg>,
}

impl<Msg> AppInputs<Msg> {
    pub(super) const fn new(shared: mpsc::UnboundedReceiver<Msg>) -> Self {
        Self { shared }
    }

    pub(super) fn try_next_ready(&mut self) -> Option<AppInput<Msg>> {
        self.shared.try_recv().ok().map(AppInput::Shared)
    }
}

impl<Msg> futures::Stream for AppInputs<Msg> {
    type Item = AppInput<Msg>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.shared
            .poll_recv(cx)
            .map(|input| input.map(AppInput::Shared))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream::StreamExt;

    #[tokio::test]
    async fn test_app_inputs_poll_next_returns_shared_after_send() {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut inputs = AppInputs::new(rx);

        tx.send(42).expect("receiver should be open");

        assert_eq!(inputs.next().await, Some(AppInput::Shared(42)));
    }

    #[test]
    fn test_app_inputs_try_next_ready_empty_returns_none() {
        let (_tx, rx) = mpsc::unbounded_channel::<i32>();
        let mut inputs = AppInputs::new(rx);

        assert_eq!(inputs.try_next_ready(), None);
    }

    #[test]
    fn test_app_inputs_try_next_ready_returns_queued_messages() {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut inputs = AppInputs::new(rx);

        tx.send(1).expect("receiver should be open");
        tx.send(2).expect("receiver should be open");

        assert_eq!(inputs.try_next_ready(), Some(AppInput::Shared(1)));
        assert_eq!(inputs.try_next_ready(), Some(AppInput::Shared(2)));
        assert_eq!(inputs.try_next_ready(), None);
    }

    #[test]
    fn test_app_inputs_try_next_ready_preserves_fifo_order() {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut inputs = AppInputs::new(rx);

        for msg in [1, 2, 3] {
            tx.send(msg).expect("receiver should be open");
        }

        assert_eq!(inputs.try_next_ready(), Some(AppInput::Shared(1)));
        assert_eq!(inputs.try_next_ready(), Some(AppInput::Shared(2)));
        assert_eq!(inputs.try_next_ready(), Some(AppInput::Shared(3)));
    }

    #[tokio::test]
    async fn test_app_inputs_poll_next_returns_none_after_sender_closes() {
        let (tx, rx) = mpsc::unbounded_channel::<i32>();
        let mut inputs = AppInputs::new(rx);

        drop(tx);

        assert_eq!(inputs.next().await, None);
    }
}
