use futures::{
    FutureExt, StreamExt,
    stream::{self, BoxStream},
};

pub enum Action<Msg> {
    Message(Msg),
    Quit,
}

pub struct Command<Msg: Send + 'static> {
    pub(super) stream: Option<BoxStream<'static, Action<Msg>>>,
}

impl<Msg: Send + 'static> Command<Msg> {
    pub fn none() -> Self {
        Self { stream: None }
    }

    pub fn perform<A>(
        future: impl Future<Output = A> + Send + 'static,
        f: impl FnOnce(A) -> Msg + Send + 'static,
    ) -> Self {
        Self::future(future.map(f))
    }

    pub fn future(future: impl Future<Output = Msg> + Send + 'static) -> Self {
        Self {
            stream: Some(future.into_stream().map(Action::Message).boxed()),
        }
    }

    pub fn effect(action: Action<Msg>) -> Self {
        Self {
            stream: Some(stream::once(async move { action }).boxed()),
        }
    }
}
