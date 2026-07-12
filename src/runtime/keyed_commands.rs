use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::FutureExt;
use futures::stream::{BoxStream, Stream, StreamExt};
use futures::task::noop_waker_ref;
use tokio::sync::mpsc;
use tokio::task::{AbortHandle, JoinSet};
use tokio_stream::StreamMap;

use crate::command::{Action, CancelPolicy, CommandId};

#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(super) enum CommandOutput<Msg> {
    Message(Msg),
    Quit,
}

#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(super) enum ReceiverEvent<Msg> {
    Output(CommandOutput<Msg>),
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReceiverFacts {
    sender_closed: bool,
    buffered: usize,
}

struct CommandReceiver<Msg> {
    receiver: mpsc::UnboundedReceiver<CommandOutput<Msg>>,
    reported_closed: bool,
}

impl<Msg> CommandReceiver<Msg> {
    const fn new(receiver: mpsc::UnboundedReceiver<CommandOutput<Msg>>) -> Self {
        Self {
            receiver,
            reported_closed: false,
        }
    }

    fn facts(&self) -> ReceiverFacts {
        ReceiverFacts {
            sender_closed: self.receiver.is_closed(),
            buffered: self.receiver.len(),
        }
    }
}

impl<Msg> Stream for CommandReceiver<Msg> {
    type Item = ReceiverEvent<Msg>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.receiver.poll_recv(cx) {
            Poll::Ready(Some(output)) => Poll::Ready(Some(ReceiverEvent::Output(output))),
            Poll::Ready(None) if !self.reported_closed => {
                self.reported_closed = true;
                Poll::Ready(Some(ReceiverEvent::Closed))
            }
            Poll::Ready(None) | Poll::Pending => Poll::Pending,
        }
    }
}

struct KeyedEntry<Msg> {
    receiver: CommandReceiver<Msg>,
    run: KeyRun,
}

impl<Msg> Stream for KeyedEntry<Msg> {
    type Item = ReceiverEvent<Msg>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.receiver).poll_next(cx)
    }
}

enum KeyRun {
    Running { token: RunToken, abort: AbortHandle },
    Draining { token: RunToken },
}

impl KeyRun {
    const fn token(&self) -> RunToken {
        match self {
            Self::Running { token, .. } | Self::Draining { token } => *token,
        }
    }

    const fn lifecycle_state(&self) -> LifecycleState {
        match self {
            Self::Running { token, .. } => LifecycleState::Running { token: *token },
            Self::Draining { token } => LifecycleState::Draining { token: *token },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RunToken(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LifecycleState {
    Running { token: RunToken },
    Draining { token: RunToken },
}

impl LifecycleState {
    const fn token(self) -> RunToken {
        match self {
            Self::Running { token } | Self::Draining { token } => token,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum LifecycleEvent {
    Spawn {
        token: RunToken,
        policy: CancelPolicy,
    },
    Cancel,
    Reconcile(ReceiverFacts),
    Output(ReceiverFacts),
    TaskExit {
        token: RunToken,
        facts: ReceiverFacts,
    },
    Closed,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct LifecycleEffects {
    spawn: bool,
    abort: bool,
    drop_receiver: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LifecycleTransition {
    state: Option<LifecycleState>,
    effects: LifecycleEffects,
}

fn lifecycle_transition(
    state: Option<LifecycleState>,
    event: LifecycleEvent,
) -> LifecycleTransition {
    let occupied = state.is_some();
    let running = matches!(state, Some(LifecycleState::Running { .. }));

    match event {
        LifecycleEvent::Spawn { token, policy } => {
            if occupied && policy == CancelPolicy::KeepInFlight {
                return LifecycleTransition {
                    state,
                    effects: LifecycleEffects::default(),
                };
            }

            LifecycleTransition {
                state: Some(LifecycleState::Running { token }),
                effects: LifecycleEffects {
                    spawn: true,
                    abort: running,
                    drop_receiver: occupied,
                },
            }
        }
        LifecycleEvent::Cancel => LifecycleTransition {
            state: None,
            effects: LifecycleEffects {
                spawn: false,
                abort: running,
                drop_receiver: occupied,
            },
        },
        LifecycleEvent::Reconcile(facts) | LifecycleEvent::Output(facts) => {
            let state = match state {
                Some(_) if facts.sender_closed && facts.buffered == 0 => None,
                Some(state) if facts.sender_closed => Some(LifecycleState::Draining {
                    token: state.token(),
                }),
                state => state,
            };
            LifecycleTransition {
                effects: LifecycleEffects {
                    drop_receiver: occupied && state.is_none(),
                    ..LifecycleEffects::default()
                },
                state,
            }
        }
        LifecycleEvent::TaskExit { token, facts } => {
            let matching = matches!(
                state,
                Some(LifecycleState::Running { token: current }) if current == token
            );
            let state = if matching {
                if facts.buffered == 0 {
                    None
                } else {
                    Some(LifecycleState::Draining { token })
                }
            } else {
                state
            };
            LifecycleTransition {
                effects: LifecycleEffects {
                    drop_receiver: matching && state.is_none(),
                    ..LifecycleEffects::default()
                },
                state,
            }
        }
        LifecycleEvent::Closed => LifecycleTransition {
            state: None,
            effects: LifecycleEffects {
                drop_receiver: occupied,
                ..LifecycleEffects::default()
            },
        },
    }
}

struct TaskExit {
    id: CommandId,
    token: RunToken,
}

pub(super) struct KeyedCommands<Msg: Send + 'static> {
    entries: StreamMap<CommandId, KeyedEntry<Msg>>,
    tasks: JoinSet<TaskExit>,
    next_token: u64,
}

impl<Msg: Send + 'static> KeyedCommands<Msg> {
    pub(super) fn new() -> Self {
        Self {
            entries: StreamMap::new(),
            tasks: JoinSet::new(),
            next_token: 0,
        }
    }

    pub(super) fn spawn(
        &mut self,
        id: CommandId,
        policy: CancelPolicy,
        stream: BoxStream<'static, Action<Msg>>,
    ) {
        self.reconcile_available();
        let token = RunToken(self.next_token);
        let state = self.reconcile_receiver(&id);
        let transition = lifecycle_transition(state, LifecycleEvent::Spawn { token, policy });
        if !transition.effects.spawn {
            tracing::trace!(
                target: "tears::runtime",
                id = ?id,
                "keyed command kept in-flight; new stream dropped"
            );
            return;
        }

        self.apply_transition(&id, state, transition);
        self.next_token = self.next_token.wrapping_add(1);
        let (output_tx, output_rx) = mpsc::unbounded_channel();
        let task_id = id.clone();

        let abort = self.tasks.spawn(async move {
            let result = AssertUnwindSafe(async move {
                futures::pin_mut!(stream);
                while let Some(action) = stream.next().await {
                    match action {
                        Action::Message(message) => {
                            if output_tx.send(CommandOutput::Message(message)).is_err() {
                                break;
                            }
                        }
                        Action::Quit => {
                            let _ = output_tx.send(CommandOutput::Quit);
                            break;
                        }
                    }
                }
            })
            .catch_unwind()
            .await;

            if let Err(error) = result {
                tracing::error!(
                    target: "tears::runtime",
                    panic = ?error,
                    "keyed command task panicked"
                );
            }

            TaskExit { id: task_id, token }
        });

        tracing::trace!(target: "tears::runtime", id = ?id, "keyed command spawned");
        self.entries.insert(
            id,
            KeyedEntry {
                receiver: CommandReceiver::new(output_rx),
                run: KeyRun::Running { token, abort },
            },
        );
    }

    pub(super) fn cancel(&mut self, id: &CommandId) {
        let state = self.lifecycle_state(id);
        let transition = lifecycle_transition(state, LifecycleEvent::Cancel);
        if transition.effects.drop_receiver {
            self.apply_transition(id, state, transition);
            tracing::trace!(target: "tears::runtime", id = ?id, "keyed command cancelled");
        }
    }

    pub(super) fn reconcile_available(&mut self) {
        while let Some(result) = self.tasks.try_join_next() {
            if let Ok(exit) = result {
                self.record_task_exit(&exit);
            }
        }
    }

    fn record_task_exit(&mut self, exit: &TaskExit) {
        let Some((state, facts)) = self.entries.iter().find_map(|(id, entry)| {
            (id == &exit.id).then(|| (entry.run.lifecycle_state(), entry.receiver.facts()))
        }) else {
            return;
        };
        let transition = lifecycle_transition(
            Some(state),
            LifecycleEvent::TaskExit {
                token: exit.token,
                facts,
            },
        );
        self.apply_transition(&exit.id, Some(state), transition);
    }

    fn record_receiver_event(&mut self, id: &CommandId, event: &ReceiverEvent<Msg>) {
        let Some((state, facts)) = self.entries.iter().find_map(|(entry_id, entry)| {
            (entry_id == id).then(|| (entry.run.lifecycle_state(), entry.receiver.facts()))
        }) else {
            return;
        };
        let lifecycle_event = match event {
            ReceiverEvent::Output(_) => LifecycleEvent::Output(facts),
            ReceiverEvent::Closed => LifecycleEvent::Closed,
        };
        let transition = lifecycle_transition(Some(state), lifecycle_event);
        self.apply_transition(id, Some(state), transition);
    }

    fn reconcile_receiver(&mut self, id: &CommandId) -> Option<LifecycleState> {
        let (state, facts) = self.entries.iter().find_map(|(entry_id, entry)| {
            (entry_id == id).then(|| (entry.run.lifecycle_state(), entry.receiver.facts()))
        })?;
        let transition = lifecycle_transition(Some(state), LifecycleEvent::Reconcile(facts));
        self.apply_transition(id, Some(state), transition);
        transition.state
    }

    fn lifecycle_state(&self, id: &CommandId) -> Option<LifecycleState> {
        self.entries
            .iter()
            .find_map(|(entry_id, entry)| (entry_id == id).then(|| entry.run.lifecycle_state()))
    }

    fn apply_transition(
        &mut self,
        id: &CommandId,
        current: Option<LifecycleState>,
        transition: LifecycleTransition,
    ) {
        if current == transition.state && transition.effects == LifecycleEffects::default() {
            return;
        }
        let Some(mut entry) = self.entries.remove(id) else {
            return;
        };

        if transition.effects.abort
            && let KeyRun::Running { abort, .. } = &entry.run
        {
            abort.abort();
        }

        if transition.effects.drop_receiver {
            return;
        }

        match transition.state {
            None => {
                debug_assert!(transition.effects.drop_receiver);
            }
            Some(LifecycleState::Running { token }) => {
                debug_assert_eq!(entry.run.token(), token);
                self.entries.insert(id.clone(), entry);
            }
            Some(LifecycleState::Draining { token }) => {
                entry.run = KeyRun::Draining { token };
                self.entries.insert(id.clone(), entry);
            }
        }
    }

    pub(super) fn try_next_ready(&mut self) -> Option<(CommandId, ReceiverEvent<Msg>)> {
        self.reconcile_available();
        let mut context = Context::from_waker(noop_waker_ref());
        match Pin::new(&mut self.entries).poll_next(&mut context) {
            Poll::Ready(Some((id, event))) => {
                self.record_receiver_event(&id, &event);
                Some((id, event))
            }
            Poll::Ready(None) | Poll::Pending => None,
        }
    }

    pub(super) fn shutdown(&mut self) {
        self.tasks.abort_all();
        self.entries.clear();
    }

    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[cfg(test)]
    pub(super) fn contains(&self, id: &CommandId) -> bool {
        self.entries.contains_key(id)
    }

    #[cfg(test)]
    pub(super) fn has_closed_buffered(&self, id: &CommandId) -> bool {
        self.entries.iter().any(|(entry_id, entry)| {
            entry_id == id && {
                let facts = entry.receiver.facts();
                facts.sender_closed && facts.buffered > 0
            }
        })
    }
}

impl<Msg: Send + 'static> Stream for KeyedCommands<Msg> {
    type Item = (CommandId, ReceiverEvent<Msg>);

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let manager = &mut *self;
        manager.reconcile_available();
        match Pin::new(&mut manager.entries).poll_next(cx) {
            Poll::Ready(Some((id, event))) => {
                manager.record_receiver_event(&id, &event);
                Poll::Ready(Some((id, event)))
            }
            Poll::Ready(None) | Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::future::pending;
    use std::num::NonZeroUsize;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    use futures::stream;
    use tokio::sync::oneshot;
    use tokio::task::yield_now;
    use tokio::time::{advance, timeout};
    use tracing::Level;

    use crate::Command;
    use crate::command::RetryPolicy;
    use crate::test_support::{TraceRecorder, wait_until, with_silent_panic_hook};

    fn actions<I>(items: I) -> BoxStream<'static, Action<i32>>
    where
        I: IntoIterator<Item = Action<i32>>,
        I::IntoIter: Send + 'static,
    {
        stream::iter(items).boxed()
    }

    fn command_stream(command: Command<i32>) -> BoxStream<'static, Action<i32>> {
        let (_, _, stream) = command.into_runtime_parts().into_execution_parts();
        stream.expect("command should have a stream")
    }

    async fn wait_for_closed_buffered(manager: &mut KeyedCommands<i32>, id: &CommandId) {
        wait_until(
            || {
                manager.entries.iter().any(|(entry_id, entry)| {
                    entry_id == id && {
                        let facts = entry.receiver.facts();
                        facts.sender_closed && facts.buffered > 0
                    }
                })
            },
            "keyed output should become buffered after its task finishes",
        )
        .await;
        manager.reconcile_available();
    }

    fn take_message(manager: &mut KeyedCommands<i32>, expected_id: &CommandId) -> i32 {
        let (id, event) = manager
            .try_next_ready()
            .expect("keyed output should be ready");
        assert_eq!(&id, expected_id);
        match event {
            ReceiverEvent::Output(CommandOutput::Message(message)) => Some(message),
            ReceiverEvent::Output(CommandOutput::Quit) | ReceiverEvent::Closed => None,
        }
        .expect("expected a keyed message")
    }

    #[tokio::test]
    async fn cancel_in_flight_drops_finished_buffered_output() {
        let id = CommandId::new("search");
        let mut manager = KeyedCommands::new();
        manager.spawn(
            id.clone(),
            CancelPolicy::CancelInFlight,
            actions([Action::Message(1)]),
        );
        wait_for_closed_buffered(&mut manager, &id).await;

        manager.spawn(
            id.clone(),
            CancelPolicy::CancelInFlight,
            actions([Action::Message(2)]),
        );
        wait_for_closed_buffered(&mut manager, &id).await;

        assert_eq!(take_message(&mut manager, &id), 2);
        assert!(manager.try_next_ready().is_none());
    }

    #[tokio::test]
    async fn explicit_cancel_is_strict_and_idempotent() {
        let id = CommandId::new("search");
        let mut manager = KeyedCommands::new();
        manager.spawn(
            id.clone(),
            CancelPolicy::CancelInFlight,
            actions([Action::Message(1), Action::Message(2)]),
        );
        wait_for_closed_buffered(&mut manager, &id).await;

        manager.cancel(&id);
        manager.cancel(&id);

        assert!(!manager.contains(&id));
        assert!(manager.try_next_ready().is_none());
    }

    #[tokio::test]
    async fn explicit_cancel_aborts_running_work() {
        struct AbortGuard(Arc<AtomicBool>);

        impl Drop for AbortGuard {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let id = CommandId::new("search");
        let dropped = Arc::new(AtomicBool::new(false));
        let guard = AbortGuard(Arc::clone(&dropped));
        let (started_tx, started_rx) = oneshot::channel();
        let mut manager = KeyedCommands::new();
        manager.spawn(
            id.clone(),
            CancelPolicy::CancelInFlight,
            command_stream(Command::future(async move {
                let _guard = guard;
                let _ = started_tx.send(());
                pending().await
            })),
        );

        timeout(Duration::from_secs(1), started_rx)
            .await
            .expect("keyed command should start before the timeout")
            .expect("keyed command should signal that it started");
        manager.cancel(&id);

        wait_until(
            || dropped.load(Ordering::SeqCst),
            "explicit cancellation should drop running keyed work",
        )
        .await;
        assert!(!manager.contains(&id));
        assert!(manager.try_next_ready().is_none());
    }

    #[tokio::test]
    async fn transition_applier_honors_abort_without_dropping_the_receiver() {
        struct AbortGuard(Arc<AtomicBool>);

        impl Drop for AbortGuard {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let id = CommandId::new("abort-only");
        let dropped = Arc::new(AtomicBool::new(false));
        let guard = AbortGuard(Arc::clone(&dropped));
        let (started_tx, started_rx) = oneshot::channel();
        let mut manager = KeyedCommands::new();
        manager.spawn(
            id.clone(),
            CancelPolicy::CancelInFlight,
            command_stream(Command::future(async move {
                let _guard = guard;
                let _ = started_tx.send(());
                pending().await
            })),
        );
        timeout(Duration::from_secs(1), started_rx)
            .await
            .expect("keyed command should start before the timeout")
            .expect("keyed command should signal that it started");

        let state = manager.lifecycle_state(&id);
        manager.apply_transition(
            &id,
            state,
            LifecycleTransition {
                state,
                effects: LifecycleEffects {
                    abort: true,
                    ..LifecycleEffects::default()
                },
            },
        );

        wait_until(
            || dropped.load(Ordering::SeqCst),
            "abort-only transition should stop the task",
        )
        .await;
        assert!(manager.contains(&id));
        manager.cancel(&id);
    }

    #[tokio::test]
    async fn keep_in_flight_preserves_finished_buffered_output() {
        let id = CommandId::new("submit");
        let mut manager = KeyedCommands::new();
        manager.spawn(
            id.clone(),
            CancelPolicy::CancelInFlight,
            actions([Action::Message(1)]),
        );
        wait_for_closed_buffered(&mut manager, &id).await;

        manager.spawn(
            id.clone(),
            CancelPolicy::KeepInFlight,
            actions([Action::Message(2)]),
        );

        assert_eq!(take_message(&mut manager, &id), 1);
        assert!(manager.try_next_ready().is_none());
    }

    #[tokio::test]
    async fn keep_in_flight_rechecks_a_closed_empty_receiver_before_task_exit() {
        let id = CommandId::new("submit");
        let token = RunToken(0);
        let mut manager = KeyedCommands::new();
        // Model the panic-reporting window: the task still has no published
        // TaskExit, but unwinding has already dropped its output sender.
        let (output_tx, output_rx) = mpsc::unbounded_channel();
        drop(output_tx);
        let abort = manager.tasks.spawn(pending::<TaskExit>());
        manager.entries.insert(
            id.clone(),
            KeyedEntry {
                receiver: CommandReceiver::new(output_rx),
                run: KeyRun::Running { token, abort },
            },
        );
        manager.next_token = 1;

        manager.spawn(
            id.clone(),
            CancelPolicy::KeepInFlight,
            actions([Action::Message(2)]),
        );

        assert_eq!(
            manager.lifecycle_state(&id),
            Some(LifecycleState::Running { token: RunToken(1) })
        );
        wait_for_closed_buffered(&mut manager, &id).await;
        assert_eq!(take_message(&mut manager, &id), 2);
        manager.shutdown();
    }

    #[tokio::test]
    async fn delivering_the_closed_senders_last_item_releases_the_id_for_retry() {
        let id = CommandId::new("submit");
        let mut manager = KeyedCommands::new();
        manager.spawn(
            id.clone(),
            CancelPolicy::CancelInFlight,
            actions([Action::Message(1)]),
        );
        wait_for_closed_buffered(&mut manager, &id).await;

        assert_eq!(take_message(&mut manager, &id), 1);
        assert!(!manager.contains(&id));

        manager.spawn(
            id.clone(),
            CancelPolicy::KeepInFlight,
            actions([Action::Message(2)]),
        );
        wait_for_closed_buffered(&mut manager, &id).await;
        assert_eq!(take_message(&mut manager, &id), 2);
    }

    #[tokio::test]
    async fn keep_in_flight_does_not_spawn_while_sender_is_open() {
        let id = CommandId::new("submit");
        let mut manager = KeyedCommands::new();
        manager.spawn(
            id.clone(),
            CancelPolicy::CancelInFlight,
            command_stream(Command::future(pending())),
        );

        manager.spawn(
            id.clone(),
            CancelPolicy::KeepInFlight,
            actions([Action::Message(2)]),
        );

        assert!(manager.contains(&id));
        assert!(manager.try_next_ready().is_none());
        manager.cancel(&id);
    }

    #[tokio::test]
    async fn cancelling_buffered_quit_suppresses_it() {
        let id = CommandId::new("quit");
        let mut manager = KeyedCommands::new();
        manager.spawn(
            id.clone(),
            CancelPolicy::CancelInFlight,
            actions([Action::Quit]),
        );
        wait_for_closed_buffered(&mut manager, &id).await;

        manager.cancel(&id);

        assert!(manager.try_next_ready().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn cancelling_suppresses_a_pending_timeout_message() {
        let id = CommandId::new("timeout");
        let mut manager = KeyedCommands::new();
        let command = Command::future(pending())
            .timeout(Duration::from_secs(5), || 99)
            .cancellable(id.clone());
        let (_, key, stream) = command.into_runtime_parts().into_execution_parts();
        let key = key.expect("key should be present");
        manager.spawn(key.id, key.policy, stream.expect("stream should exist"));

        assert!(manager.try_next_ready().is_none());
        manager.cancel(&id);
        advance(Duration::from_secs(5)).await;

        assert!(manager.try_next_ready().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn superseding_during_retry_backoff_suppresses_the_old_final_message() {
        let id = CommandId::new("retry");
        let attempts = Arc::new(AtomicUsize::new(0));
        let operation_attempts = Arc::clone(&attempts);
        let policy = RetryPolicy::new(NonZeroUsize::new(3).expect("non-zero"))
            .with_fixed_backoff(Duration::from_secs(5));
        let retry = Command::retry(
            policy,
            move |_| {
                operation_attempts.fetch_add(1, Ordering::SeqCst);
                async { Err::<i32, &'static str>("temporary") }
            },
            |_| 1,
        );
        let mut manager = KeyedCommands::new();
        manager.spawn(
            id.clone(),
            CancelPolicy::CancelInFlight,
            command_stream(retry),
        );
        wait_until(
            || attempts.load(Ordering::SeqCst) == 1,
            "retry should enter its first backoff",
        )
        .await;

        manager.spawn(
            id.clone(),
            CancelPolicy::CancelInFlight,
            actions([Action::Message(2)]),
        );
        advance(Duration::from_secs(10)).await;
        wait_for_closed_buffered(&mut manager, &id).await;

        assert_eq!(take_message(&mut manager, &id), 2);
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(manager.try_next_ready().is_none());
    }

    #[tokio::test]
    async fn keep_in_flight_prevents_a_second_retry_from_starting() {
        let id = CommandId::new("retry");
        let second_attempts = Arc::new(AtomicUsize::new(0));
        let observed_second_attempts = Arc::clone(&second_attempts);
        let policy = RetryPolicy::new(NonZeroUsize::new(2).expect("non-zero"));
        let first = Command::future(pending());
        let second = Command::retry(
            policy,
            move |_| {
                observed_second_attempts.fetch_add(1, Ordering::SeqCst);
                async { Ok::<i32, &'static str>(2) }
            },
            |result| result.expect("second retry would succeed"),
        );
        let mut manager = KeyedCommands::new();
        manager.spawn(
            id.clone(),
            CancelPolicy::CancelInFlight,
            command_stream(first),
        );
        manager.spawn(
            id.clone(),
            CancelPolicy::KeepInFlight,
            command_stream(second),
        );
        yield_now().await;

        assert_eq!(second_attempts.load(Ordering::SeqCst), 0);
        manager.cancel(&id);
    }

    #[tokio::test]
    async fn stale_completion_cannot_remove_or_mutate_a_successor() {
        let id = CommandId::new("search");
        let mut manager = KeyedCommands::new();
        manager.spawn(
            id.clone(),
            CancelPolicy::CancelInFlight,
            command_stream(Command::future(pending())),
        );
        manager.spawn(
            id.clone(),
            CancelPolicy::CancelInFlight,
            command_stream(Command::future(pending())),
        );

        // Inject the old run's normal completion directly. The real aborted
        // task reaps as JoinError::cancelled and cannot exercise this branch.
        manager.record_task_exit(&TaskExit {
            id: id.clone(),
            token: RunToken(0),
        });

        assert!(manager.contains(&id));
        assert_eq!(
            manager.lifecycle_state(&id),
            Some(LifecycleState::Running { token: RunToken(1) })
        );
        manager.cancel(&id);
    }

    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::panic, reason = "the command intentionally panics")]
    async fn keyed_task_panic_is_logged() {
        let recorder = TraceRecorder::new()
            .with_target("tears::runtime")
            .with_level(Level::ERROR);
        let _guard = recorder.set_default();
        let id = CommandId::new("panic");
        let mut manager = KeyedCommands::new();
        let (event_count, contains_id) = with_silent_panic_hook(async {
            manager.spawn(
                id.clone(),
                CancelPolicy::CancelInFlight,
                command_stream(Command::future(async {
                    panic!("boom");
                    #[allow(unreachable_code)]
                    1
                })),
            );

            wait_until(
                || recorder.event_count() == 1,
                "keyed task panic should emit an error event",
            )
            .await;
            manager.reconcile_available();
            (recorder.event_count(), manager.contains(&id))
        })
        .await;

        assert_eq!(event_count, 1);
        assert!(!contains_id);
    }

    #[tokio::test]
    async fn shutdown_aborts_keyed_tasks() {
        struct AbortGuard(Arc<AtomicBool>);
        impl Drop for AbortGuard {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let dropped = Arc::new(AtomicBool::new(false));
        let guard = AbortGuard(Arc::clone(&dropped));
        let id = CommandId::new("running");
        let mut manager = KeyedCommands::new();
        manager.spawn(
            id,
            CancelPolicy::CancelInFlight,
            command_stream(Command::future(async move {
                let _guard = guard;
                pending().await
            })),
        );
        yield_now().await;

        manager.shutdown();
        wait_until(
            || dropped.load(Ordering::SeqCst),
            "shutdown should drop the running keyed future",
        )
        .await;
    }
}

#[cfg(test)]
mod lifecycle_model_tests {
    use super::*;
    use proptest::prelude::*;

    #[derive(Clone, Copy, Debug)]
    enum Input {
        SpawnCancel,
        SpawnKeep,
        Cancel,
        Reconcile(ReceiverFacts),
        Output(ReceiverFacts),
        TaskExit {
            token: RunToken,
            facts: ReceiverFacts,
        },
        Closed,
    }

    #[derive(Debug)]
    struct Model {
        state: Option<LifecycleState>,
        next_token: u64,
    }

    impl Model {
        const fn new() -> Self {
            Self {
                state: None,
                next_token: 0,
            }
        }

        fn apply(&mut self, input: Input) -> LifecycleEffects {
            let event = match input {
                Input::SpawnCancel => LifecycleEvent::Spawn {
                    token: RunToken(self.next_token),
                    policy: CancelPolicy::CancelInFlight,
                },
                Input::SpawnKeep => LifecycleEvent::Spawn {
                    token: RunToken(self.next_token),
                    policy: CancelPolicy::KeepInFlight,
                },
                Input::Cancel => LifecycleEvent::Cancel,
                Input::Reconcile(facts) => LifecycleEvent::Reconcile(facts),
                Input::Output(facts) => LifecycleEvent::Output(facts),
                Input::TaskExit { token, facts } => LifecycleEvent::TaskExit { token, facts },
                Input::Closed => LifecycleEvent::Closed,
            };
            let transition = lifecycle_transition(self.state, event);
            if transition.effects.spawn {
                self.next_token = self.next_token.wrapping_add(1);
            }
            self.state = transition.state;
            transition.effects
        }
    }

    fn facts() -> impl Strategy<Value = ReceiverFacts> {
        (any::<bool>(), 0_usize..4).prop_map(|(sender_closed, buffered)| ReceiverFacts {
            sender_closed,
            buffered,
        })
    }

    fn inputs() -> impl Strategy<Value = Vec<Input>> {
        prop::collection::vec(
            prop_oneof![
                Just(Input::SpawnCancel),
                Just(Input::SpawnKeep),
                Just(Input::Cancel),
                facts().prop_map(Input::Reconcile),
                facts().prop_map(Input::Output),
                (any::<u64>(), facts()).prop_map(|(token, facts)| Input::TaskExit {
                    token: RunToken(token),
                    facts,
                }),
                Just(Input::Closed),
            ],
            0..128,
        )
    }

    #[test]
    fn closed_empty_reconciliation_drops_the_owned_receiver() {
        let transition = lifecycle_transition(
            Some(LifecycleState::Running { token: RunToken(7) }),
            LifecycleEvent::Reconcile(ReceiverFacts {
                sender_closed: true,
                buffered: 0,
            }),
        );

        assert_eq!(
            transition,
            LifecycleTransition {
                state: None,
                effects: LifecycleEffects {
                    drop_receiver: true,
                    ..LifecycleEffects::default()
                },
            }
        );
    }

    proptest! {
        #[test]
        fn lifecycle_invariants_hold_for_arbitrary_sequences(sequence in inputs()) {
            let mut model = Model::new();

            for input in sequence {
                let before = model.state;
                let effects = model.apply(input);

                match input {
                    Input::Cancel => {
                        prop_assert_eq!(effects.drop_receiver, before.is_some());
                        prop_assert_eq!(model.state, None);
                    }
                    Input::SpawnKeep if before.is_some() => {
                        prop_assert!(!effects.spawn);
                        prop_assert_eq!(model.state, before);
                    }
                    Input::SpawnCancel => {
                        prop_assert!(effects.spawn);
                        prop_assert_eq!(effects.drop_receiver, before.is_some());
                    }
                    Input::TaskExit { token, .. } => {
                        let matches_current = matches!(
                            before,
                            Some(LifecycleState::Running { token: current }) if current == token
                        );
                        if !matches_current {
                            prop_assert_eq!(model.state, before);
                        }
                    }
                    Input::Reconcile(ReceiverFacts { sender_closed: true, buffered: 0 })
                    | Input::Output(ReceiverFacts { sender_closed: true, buffered: 0 }) => {
                        prop_assert_eq!(model.state, None);
                    }
                    _ => {}
                }
            }
        }

        #[test]
        fn cancel_is_idempotent(sequence in inputs()) {
            let mut model = Model::new();
            for input in sequence {
                model.apply(input);
            }

            model.apply(Input::Cancel);
            let after_first = model.state;
            let second_effects = model.apply(Input::Cancel);

            prop_assert_eq!(after_first, None);
            prop_assert_eq!(model.state, None);
            prop_assert_eq!(second_effects, LifecycleEffects::default());
        }
    }
}
