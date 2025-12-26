use futures::{stream::BoxStream, FutureExt, StreamExt};

pub struct Command<M> {
    pub(super) stream: Option<BoxStream<'static, M>>,
}

impl<M> Command<M> {
    pub fn none() -> Self {
        Self { stream: None }
    }

    pub fn perform<A>(
        future: impl Future<Output = A> + Send + 'static,
        f: impl FnOnce(A) -> M + Send + 'static,
    ) -> Command<M> {
        Self {
            stream: Some(future.map(f).into_stream().boxed()),
        }
    }
}
