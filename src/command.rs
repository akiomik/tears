use futures::{
    FutureExt, StreamExt,
    stream::{self, BoxStream, select_all},
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

    /// Batch multiple commands into a single command.
    /// All commands will be executed concurrently.
    pub fn batch(commands: impl IntoIterator<Item = Command<Msg>>) -> Self {
        let streams: Vec<_> = commands.into_iter().filter_map(|cmd| cmd.stream).collect();

        if streams.is_empty() {
            Self::none()
        } else {
            Self {
                stream: Some(select_all(streams).boxed()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[tokio::test]
    async fn test_batch_empty() {
        let cmd: Command<i32> = Command::batch(vec![]);
        assert!(cmd.stream.is_none());
    }

    #[tokio::test]
    async fn test_batch_single_command() {
        let cmd1 = Command::future(async { 1 });
        let cmd = Command::batch(vec![cmd1]);

        let mut stream = cmd.stream.expect("stream should exist");
        let action = stream.next().await.expect("should have action");

        match action {
            Action::Message(msg) => assert_eq!(msg, 1),
            Action::Quit => panic!("unexpected quit"),
        }
    }

    #[tokio::test]
    async fn test_batch_multiple_commands() {
        let cmd1 = Command::future(async { 1 });
        let cmd2 = Command::future(async { 2 });
        let cmd3 = Command::future(async { 3 });

        let cmd = Command::batch(vec![cmd1, cmd2, cmd3]);

        let mut stream = cmd.stream.expect("stream should exist");
        let mut results = vec![];

        while let Some(action) = stream.next().await {
            match action {
                Action::Message(msg) => results.push(msg),
                Action::Quit => break,
            }
        }

        // All messages should be received (order may vary due to concurrent execution)
        results.sort();
        assert_eq!(results, vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn test_batch_with_none_commands() {
        let cmd1 = Command::future(async { 1 });
        let cmd2 = Command::<i32>::none();
        let cmd3 = Command::future(async { 3 });

        let cmd = Command::batch(vec![cmd1, cmd2, cmd3]);

        let mut stream = cmd.stream.expect("stream should exist");
        let mut results = vec![];

        while let Some(action) = stream.next().await {
            match action {
                Action::Message(msg) => results.push(msg),
                Action::Quit => break,
            }
        }

        // Only non-none commands should produce messages
        results.sort();
        assert_eq!(results, vec![1, 3]);
    }

    #[tokio::test]
    async fn test_batch_all_none() {
        let cmd1 = Command::<i32>::none();
        let cmd2 = Command::<i32>::none();

        let cmd = Command::batch(vec![cmd1, cmd2]);
        assert!(cmd.stream.is_none());
    }

    #[tokio::test]
    async fn test_batch_with_quit_action() {
        let cmd1 = Command::future(async { 1 });
        let cmd2 = Command::effect(Action::Quit);
        let cmd3 = Command::future(async { 3 });

        let cmd = Command::batch(vec![cmd1, cmd2, cmd3]);

        let mut stream = cmd.stream.expect("stream should exist");
        let mut has_quit = false;
        let mut messages = vec![];

        while let Some(action) = stream.next().await {
            match action {
                Action::Message(msg) => messages.push(msg),
                Action::Quit => {
                    has_quit = true;
                    break;
                }
            }
        }

        assert!(has_quit, "should receive quit action");
    }
}
