use futures::{
    FutureExt, Stream, StreamExt,
    stream::{self, BoxStream, select_all},
};

use super::Action;

// Effects own and compose the asynchronous action stream; runtime directives
// stay separate because they describe how the runtime treats the update result.
pub(super) struct Effect<Msg: Send + 'static> {
    stream: Option<BoxStream<'static, Action<Msg>>>,
}

impl<Msg: Send + 'static> Effect<Msg> {
    pub(super) const fn none() -> Self {
        Self { stream: None }
    }

    fn from_stream(stream: BoxStream<'static, Action<Msg>>) -> Self {
        Self {
            stream: Some(stream),
        }
    }

    pub(super) fn future(future: impl Future<Output = Msg> + Send + 'static) -> Self {
        Self::from_stream(future.into_stream().map(Action::Message).boxed())
    }

    pub(super) fn action(action: Action<Msg>) -> Self {
        Self::from_stream(stream::once(async move { action }).boxed())
    }

    pub(super) fn stream(stream: impl Stream<Item = Msg> + Send + 'static) -> Self {
        Self::from_stream(stream.map(Action::Message).boxed())
    }

    pub(super) fn batch(effects: impl IntoIterator<Item = Self>) -> Self {
        let streams: Vec<_> = effects
            .into_iter()
            .filter_map(|effect| effect.stream)
            .collect();

        if streams.is_empty() {
            Self::none()
        } else {
            Self::from_stream(select_all(streams).boxed())
        }
    }

    pub(super) fn map<T>(self, f: impl Fn(Msg) -> T + Send + 'static) -> Effect<T>
    where
        T: Send + 'static,
    {
        let stream = self.stream.map(|stream| {
            stream
                .map(move |action| match action {
                    Action::Message(msg) => Action::Message(f(msg)),
                    Action::Quit => Action::Quit,
                })
                .boxed()
        });

        Effect { stream }
    }

    pub(super) const fn is_none(&self) -> bool {
        self.stream.is_none()
    }

    pub(super) const fn is_some(&self) -> bool {
        self.stream.is_some()
    }

    pub(super) fn into_stream(self) -> Option<BoxStream<'static, Action<Msg>>> {
        self.stream
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[test]
    fn test_effect_none_has_no_stream() {
        let effect = Effect::<i32>::none();

        assert!(effect.is_none());
        assert!(!effect.is_some());
        assert!(effect.into_stream().is_none());
    }

    #[tokio::test]
    async fn test_effect_batch_combines_streams() {
        let effect = Effect::batch(vec![Effect::none(), Effect::future(async { 1 })]);

        let mut stream = effect.into_stream().expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Message(1)));
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn test_effect_map_preserves_quit() {
        let effect = Effect::<i32>::action(Action::Quit).map(|value| value * 2);

        let mut stream = effect.into_stream().expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        assert!(matches!(action, Action::Quit));
    }
}
