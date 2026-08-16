use std::num::NonZeroUsize;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::FutureExt;
use futures::stream::{BoxStream, Stream, StreamExt};
use tokio::task::{AbortHandle, JoinSet};
use tokio_stream::StreamMap;

use crate::command::{Action, CancelPolicy, CommandId};
use crate::noop_waker::noop_context;
use crate::panic::contained_producer;

use super::channel;
use super::load::{Channel, LoadObserver};

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
    receiver: channel::Receiver<CommandOutput<Msg>>,
    reported_closed: bool,
}

impl<Msg> CommandReceiver<Msg> {
    const fn new(receiver: channel::Receiver<CommandOutput<Msg>>) -> Self {
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

enum LifecycleEvent<Msg> {
    Spawn {
        token: RunToken,
        policy: CancelPolicy,
        stream: BoxStream<'static, Action<Msg>>,
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

enum LifecycleDecision<Msg> {
    NoChange,
    KeepInFlight {
        stream: BoxStream<'static, Action<Msg>>,
    },
    Start {
        token: RunToken,
        stream: BoxStream<'static, Action<Msg>>,
    },
    ReplaceRunning {
        token: RunToken,
        stream: BoxStream<'static, Action<Msg>>,
    },
    ReplaceDraining {
        token: RunToken,
        stream: BoxStream<'static, Action<Msg>>,
    },
    AbortAndRemove,
    Remove,
    MarkDraining {
        token: RunToken,
    },
}

impl<Msg> LifecycleDecision<Msg> {
    const fn next_state(&self, current: Option<LifecycleState>) -> Option<LifecycleState> {
        match self {
            Self::NoChange | Self::KeepInFlight { .. } => current,
            Self::Start { token, .. }
            | Self::ReplaceRunning { token, .. }
            | Self::ReplaceDraining { token, .. } => {
                Some(LifecycleState::Running { token: *token })
            }
            Self::AbortAndRemove | Self::Remove => None,
            Self::MarkDraining { token } => Some(LifecycleState::Draining { token: *token }),
        }
    }

    const fn starts_run(&self) -> bool {
        matches!(
            self,
            Self::Start { .. } | Self::ReplaceRunning { .. } | Self::ReplaceDraining { .. }
        )
    }

    const fn removes_without_replacement(&self) -> bool {
        matches!(self, Self::AbortAndRemove | Self::Remove)
    }
}

fn lifecycle_transition<Msg>(
    state: Option<LifecycleState>,
    event: LifecycleEvent<Msg>,
) -> LifecycleDecision<Msg> {
    match event {
        LifecycleEvent::Spawn {
            token,
            policy,
            stream,
        } => match (state, policy) {
            (Some(_), CancelPolicy::KeepInFlight) => LifecycleDecision::KeepInFlight { stream },
            (None, _) => LifecycleDecision::Start { token, stream },
            (Some(LifecycleState::Running { .. }), CancelPolicy::CancelInFlight) => {
                LifecycleDecision::ReplaceRunning { token, stream }
            }
            (Some(LifecycleState::Draining { .. }), CancelPolicy::CancelInFlight) => {
                LifecycleDecision::ReplaceDraining { token, stream }
            }
        },
        LifecycleEvent::Cancel => match state {
            None => LifecycleDecision::NoChange,
            Some(LifecycleState::Running { .. }) => LifecycleDecision::AbortAndRemove,
            Some(LifecycleState::Draining { .. }) => LifecycleDecision::Remove,
        },
        LifecycleEvent::Reconcile(facts) | LifecycleEvent::Output(facts) => match state {
            Some(_) if facts.sender_closed && facts.buffered == 0 => LifecycleDecision::Remove,
            Some(LifecycleState::Running { token }) if facts.sender_closed => {
                LifecycleDecision::MarkDraining { token }
            }
            None | Some(_) => LifecycleDecision::NoChange,
        },
        LifecycleEvent::TaskExit { token, facts } => {
            let matching = matches!(
                state,
                Some(LifecycleState::Running { token: current }) if current == token
            );
            if !matching {
                LifecycleDecision::NoChange
            } else if facts.buffered == 0 {
                LifecycleDecision::Remove
            } else {
                LifecycleDecision::MarkDraining { token }
            }
        }
        LifecycleEvent::Closed => match state {
            Some(_) => LifecycleDecision::Remove,
            None => LifecycleDecision::NoChange,
        },
    }
}

pub(super) enum KeyedPoll<Msg> {
    Item(CommandId, ReceiverEvent<Msg>),
    PendingWithWakeSource,
    Quiescent,
}

struct TaskExit {
    id: CommandId,
    token: RunToken,
}

pub(super) struct KeyedCommands<Msg: Send + 'static> {
    entries: StreamMap<CommandId, KeyedEntry<Msg>>,
    tasks: JoinSet<TaskExit>,
    next_token: u64,
    /// Per-command private-channel capacity: `None` keeps each keyed channel
    /// unbounded, `Some(n)` bounds each one to `n` independently — never a
    /// shared pool (INV-L9).
    keyed_capacity: Option<NonZeroUsize>,
    /// Shared load observer: keyed producers' channels emit through it, and the
    /// `keyed_commands` gauge is set to the active-entry count after each
    /// transition (RFC 0006 §4.4).
    observer: LoadObserver,
}

impl<Msg: Send + 'static> KeyedCommands<Msg> {
    pub(super) fn new(keyed_capacity: Option<NonZeroUsize>, observer: LoadObserver) -> Self {
        Self {
            entries: StreamMap::new(),
            tasks: JoinSet::new(),
            next_token: 0,
            keyed_capacity,
            observer,
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
        let decision = lifecycle_transition(
            state,
            LifecycleEvent::Spawn {
                token,
                policy,
                stream,
            },
        );
        let starts_run = decision.starts_run();
        if !starts_run {
            tracing::trace!(
                target: "tears::runtime",
                id = ?id,
                "keyed command kept in-flight; new stream dropped"
            );
        }
        self.apply_transition(id, decision);
    }

    fn start_run(
        &mut self,
        id: CommandId,
        token: RunToken,
        stream: BoxStream<'static, Action<Msg>>,
    ) {
        self.next_token = self.next_token.wrapping_add(1);
        let (output_tx, output_rx) =
            channel::channel_observed(self.keyed_capacity, Channel::Keyed, self.observer.clone());
        let task_id = id.clone();

        let abort = self.tasks.spawn(async move {
            // `contained_producer` marks the body so the panic hook leaves the
            // terminal of the still-running application alone (RFC 0011
            // INV-LC8); the panic itself is caught and logged below.
            let result = AssertUnwindSafe(contained_producer(async move {
                futures::pin_mut!(stream);
                while let Some(action) = stream.next().await {
                    match action {
                        Action::Message(message) => {
                            // Awaiting the send applies backpressure in bounded
                            // mode (the producer waits for capacity; INV-L2) and
                            // completes immediately in unbounded mode (INV-L6).
                            // An abort during this await drops the in-flight
                            // output exactly like aborting a running task.
                            if output_tx
                                .send(CommandOutput::Message(message))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                        Action::Quit => {
                            // A keyed quit travels this private channel like any
                            // other keyed output (INV-L10), so in bounded mode it
                            // waits on `keyed_channel_capacity` too (RFC 0006 §4.2).
                            let _ = output_tx.send(CommandOutput::Quit).await;
                            break;
                        }
                    }
                }
            }))
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
        let decision = lifecycle_transition(state, LifecycleEvent::Cancel);
        let will_remove_entry = decision.removes_without_replacement();
        self.apply_transition(id.clone(), decision);
        if will_remove_entry {
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
        let decision = lifecycle_transition(
            Some(state),
            LifecycleEvent::TaskExit {
                token: exit.token,
                facts,
            },
        );
        self.apply_transition(exit.id.clone(), decision);
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
        let decision = lifecycle_transition(Some(state), lifecycle_event);
        self.apply_transition(id.clone(), decision);
    }

    fn reconcile_receiver(&mut self, id: &CommandId) -> Option<LifecycleState> {
        let (state, facts) = self.entries.iter().find_map(|(entry_id, entry)| {
            (entry_id == id).then(|| (entry.run.lifecycle_state(), entry.receiver.facts()))
        })?;
        let decision = lifecycle_transition(Some(state), LifecycleEvent::Reconcile(facts));
        let next_state = decision.next_state(Some(state));
        self.apply_transition(id.clone(), decision);
        next_state
    }

    fn lifecycle_state(&self, id: &CommandId) -> Option<LifecycleState> {
        self.entries
            .iter()
            .find_map(|(entry_id, entry)| (entry_id == id).then(|| entry.run.lifecycle_state()))
    }

    fn apply_transition(&mut self, id: CommandId, decision: LifecycleDecision<Msg>) {
        match decision {
            LifecycleDecision::NoChange => {}
            LifecycleDecision::KeepInFlight { stream } => drop(stream),
            LifecycleDecision::Start { token, stream } => {
                debug_assert!(
                    !self.entries.contains_key(&id),
                    "entry should not already exist before starting a new run"
                );
                self.start_run(id, token, stream);
            }
            LifecycleDecision::ReplaceRunning { token, stream } => {
                self.abort_running_entry(&id);
                self.start_run(id, token, stream);
            }
            LifecycleDecision::ReplaceDraining { token, stream } => {
                self.remove_draining_entry(&id);
                self.start_run(id, token, stream);
            }
            LifecycleDecision::AbortAndRemove => self.abort_running_entry(&id),
            LifecycleDecision::Remove => self.remove_entry(&id),
            LifecycleDecision::MarkDraining { token } => self.mark_draining(id, token),
        }
        // Every entry insert/remove funnels through here; publish the resulting
        // active-entry count to the `keyed_commands` gauge (RFC 0006 §4.4). A
        // `MarkDraining` (remove-then-reinsert) or a no-op transition leaves the
        // count unchanged, so the observer emits nothing.
        self.observer.set_keyed_entries(self.entries.len());
    }

    fn abort_running_entry(&mut self, id: &CommandId) {
        let Some(entry) = self.entries.remove(id) else {
            debug_assert!(false, "running entry should exist before removal");
            return;
        };
        if let KeyRun::Running { abort, .. } = entry.run {
            abort.abort();
        } else {
            debug_assert!(false, "entry should be running before abort");
        }
    }

    fn remove_draining_entry(&mut self, id: &CommandId) {
        let Some(entry) = self.entries.remove(id) else {
            debug_assert!(false, "draining entry should exist before replacement");
            return;
        };
        debug_assert!(
            matches!(entry.run, KeyRun::Draining { .. }),
            "entry should be draining before replacement"
        );
    }

    fn remove_entry(&mut self, id: &CommandId) {
        let removed = self.entries.remove(id);
        debug_assert!(removed.is_some(), "entry should exist before removal");
    }

    fn mark_draining(&mut self, id: CommandId, token: RunToken) {
        let Some(mut entry) = self.entries.remove(&id) else {
            debug_assert!(false, "running entry should exist before draining");
            return;
        };
        debug_assert!(
            matches!(entry.run, KeyRun::Running { .. }),
            "entry should be running before draining"
        );
        debug_assert_eq!(
            entry.run.token(),
            token,
            "token should match the entry's current run before draining"
        );
        entry.run = KeyRun::Draining { token };
        self.entries.insert(id, entry);
    }

    pub(super) fn poll_event(&mut self, cx: &mut Context<'_>) -> KeyedPoll<Msg> {
        self.reconcile_available();
        if self.entries.is_empty() {
            return KeyedPoll::Quiescent;
        }

        match Pin::new(&mut self.entries).poll_next(cx) {
            Poll::Ready(Some((id, event))) => {
                self.record_receiver_event(&id, &event);
                KeyedPoll::Item(id, event)
            }
            Poll::Ready(None) => KeyedPoll::Quiescent,
            Poll::Pending if self.entries.is_empty() => KeyedPoll::Quiescent,
            Poll::Pending => KeyedPoll::PendingWithWakeSource,
        }
    }

    pub(super) fn try_next_ready(&mut self) -> Option<(CommandId, ReceiverEvent<Msg>)> {
        let mut context = noop_context();
        match self.poll_event(&mut context) {
            KeyedPoll::Item(id, event) => Some((id, event)),
            KeyedPoll::PendingWithWakeSource | KeyedPoll::Quiescent => None,
        }
    }

    pub(super) fn shutdown(&mut self) {
        self.tasks.abort_all();
        self.entries.clear();
        self.observer.set_keyed_entries(self.entries.len());
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

impl<Msg: Send + 'static> Drop for KeyedCommands<Msg> {
    fn drop(&mut self) {
        // Dropping abandons every entry and aborts every task (the `StreamMap`
        // and `JoinSet` do that themselves), so publish the `keyed_commands`
        // gauge returning to zero here. `shutdown()` already did this on the
        // clean path; the gauge is count-based rather than guard-based, so a
        // path that drops the runtime without `shutdown()` — a render error
        // propagating out of `run()` — would otherwise leave it stuck above
        // zero. `set_keyed_entries` re-emits only on a change, so a
        // shutdown-then-drop pair emits once.
        self.observer.set_keyed_entries(0);
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
    use futures::task::{ArcWake, waker_ref};
    use tokio::sync::oneshot;
    use tokio::task::yield_now;
    use tokio::time::{advance, timeout};
    use tracing::Level;

    use crate::Command;
    use crate::command::{RetryPolicy, fold_leaves};
    use crate::test_support::{
        HookProbe, TraceRecorder, hook_guard, wait_until, with_silent_panic_hook,
    };

    fn actions<I>(items: I) -> BoxStream<'static, Action<i32>>
    where
        I: IntoIterator<Item = Action<i32>>,
        I::IntoIter: Send + 'static,
    {
        stream::iter(items).boxed()
    }

    fn command_stream(command: Command<i32>) -> BoxStream<'static, Action<i32>> {
        let (_, _, leaves) = command.into_runtime_parts().into_execution_parts();
        fold_leaves(leaves).expect("command should have a stream")
    }

    struct WakeCounter(AtomicUsize);

    impl ArcWake for WakeCounter {
        fn wake_by_ref(arc_self: &Arc<Self>) {
            arc_self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn insert_pending_receiver(
        manager: &mut KeyedCommands<i32>,
        id: CommandId,
    ) -> channel::Sender<CommandOutput<i32>> {
        let (output_tx, output_rx) = channel::channel(None);
        let token = RunToken(manager.next_token);
        manager.next_token = manager.next_token.wrapping_add(1);
        let abort = manager.tasks.spawn(pending::<TaskExit>());
        manager.entries.insert(
            id,
            KeyedEntry {
                receiver: CommandReceiver::new(output_rx),
                run: KeyRun::Running { token, abort },
            },
        );
        output_tx
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
    async fn pending_keyed_poll_wakes_after_output() {
        let id = CommandId::new("output-wake");
        let mut manager = KeyedCommands::new(None, LoadObserver::default());
        let output_tx = insert_pending_receiver(&mut manager, id.clone());
        let wake_counter = Arc::new(WakeCounter(AtomicUsize::new(0)));
        let waker = waker_ref(&wake_counter);
        let mut context = Context::from_waker(&waker);

        assert!(matches!(
            manager.poll_event(&mut context),
            KeyedPoll::PendingWithWakeSource
        ));
        output_tx
            .try_send(CommandOutput::Message(42))
            .expect("receiver should remain open");
        assert!(wake_counter.0.load(Ordering::SeqCst) > 0);
        assert!(matches!(
            manager.poll_event(&mut context),
            KeyedPoll::Item(
                event_id,
                ReceiverEvent::Output(CommandOutput::Message(42))
            ) if event_id == id
        ));

        manager.shutdown();
    }

    #[tokio::test]
    async fn pending_keyed_poll_wakes_after_sender_closure() {
        let id = CommandId::new("closure-wake");
        let mut manager = KeyedCommands::new(None, LoadObserver::default());
        let output_tx = insert_pending_receiver(&mut manager, id.clone());
        let wake_counter = Arc::new(WakeCounter(AtomicUsize::new(0)));
        let waker = waker_ref(&wake_counter);
        let mut context = Context::from_waker(&waker);

        assert!(matches!(
            manager.poll_event(&mut context),
            KeyedPoll::PendingWithWakeSource
        ));
        drop(output_tx);
        assert!(wake_counter.0.load(Ordering::SeqCst) > 0);
        assert!(matches!(
            manager.poll_event(&mut context),
            KeyedPoll::Item(event_id, ReceiverEvent::Closed) if event_id == id
        ));
        assert!(matches!(
            manager.poll_event(&mut context),
            KeyedPoll::Quiescent
        ));

        manager.shutdown();
    }

    #[tokio::test]
    async fn cancel_in_flight_drops_finished_buffered_output() {
        let id = CommandId::new("search");
        let mut manager = KeyedCommands::new(None, LoadObserver::default());
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
        let mut manager = KeyedCommands::new(None, LoadObserver::default());
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
        let mut manager = KeyedCommands::new(None, LoadObserver::default());
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
    async fn keep_in_flight_preserves_finished_buffered_output() {
        let id = CommandId::new("submit");
        let mut manager = KeyedCommands::new(None, LoadObserver::default());
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
        let mut manager = KeyedCommands::new(None, LoadObserver::default());
        // Model the panic-reporting window: the task still has no published
        // TaskExit, but unwinding has already dropped its output sender.
        let (output_tx, output_rx) = channel::channel(None);
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
        let mut manager = KeyedCommands::new(None, LoadObserver::default());
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
        let mut manager = KeyedCommands::new(None, LoadObserver::default());
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
    async fn keyed_quit_is_delivered_after_the_same_runs_earlier_output() {
        let id = CommandId::new("save-then-quit");
        let mut manager = KeyedCommands::new(None, LoadObserver::default());
        manager.spawn(
            id.clone(),
            CancelPolicy::CancelInFlight,
            actions([Action::Message(1), Action::Quit]),
        );
        wait_for_closed_buffered(&mut manager, &id).await;

        assert_eq!(take_message(&mut manager, &id), 1);
        let (event_id, event) = manager
            .try_next_ready()
            .expect("keyed quit should be ready after the earlier message");
        assert_eq!(event_id, id);
        assert_eq!(event, ReceiverEvent::Output(CommandOutput::Quit));
    }

    #[tokio::test]
    async fn cancelling_buffered_quit_suppresses_it() {
        let id = CommandId::new("quit");
        let mut manager = KeyedCommands::new(None, LoadObserver::default());
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
        let mut manager = KeyedCommands::new(None, LoadObserver::default());
        let command = Command::future(pending())
            .timeout(Duration::from_secs(5), || 99)
            .cancellable(id.clone());
        let (_, key, leaves) = command.into_runtime_parts().into_execution_parts();
        let key = key.expect("key should be present");
        let stream = fold_leaves(leaves).expect("stream should exist");
        manager.spawn(key.id, key.policy, stream);

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
        let mut manager = KeyedCommands::new(None, LoadObserver::default());
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
        let mut manager = KeyedCommands::new(None, LoadObserver::default());
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
        let mut manager = KeyedCommands::new(None, LoadObserver::default());
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
    #[expect(clippy::panic, reason = "the command intentionally panics")]
    async fn keyed_task_panic_is_logged() {
        let recorder = TraceRecorder::new()
            .with_target("tears::runtime")
            .with_level(Level::ERROR);
        let _guard = recorder.set_default();
        let id = CommandId::new("panic");
        let mut manager = KeyedCommands::new(None, LoadObserver::default());
        let (event_count, contains_id) = with_silent_panic_hook(async {
            manager.spawn(
                id.clone(),
                CancelPolicy::CancelInFlight,
                command_stream(Command::future(async {
                    panic!("boom");
                    #[expect(
                        unreachable_code,
                        reason = "the value follows an unconditional panic! that never returns, but types the async block"
                    )]
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

    // Guards the wiring, not the mechanism: `start_run` must wrap the keyed
    // task body in `contained_producer`, so the panic hook skips the terminal
    // restore for a panic the runtime contains (RFC 0011 INV-LC8). The
    // mechanism itself is covered in `crate::panic`; this row fails if a
    // future rewrite of the spawn path drops the wrapper.
    #[tokio::test]
    #[expect(
        clippy::panic,
        reason = "driving the panic hook requires a real panic in the task body"
    )]
    #[expect(
        clippy::await_holding_lock,
        reason = "the test intentionally serializes the process-global hook across its current-thread awaits"
    )]
    async fn a_panicking_keyed_command_task_skips_the_terminal_restore() {
        let _hook_guard = hook_guard();
        let probe = HookProbe::install(
            "runtime::keyed_commands::tests::a_panicking_keyed_command_task_skips_the_terminal_restore",
        );

        let id = CommandId::new("panic-restore");
        let mut manager = KeyedCommands::new(None, LoadObserver::default());
        manager.spawn(
            id,
            CancelPolicy::CancelInFlight,
            command_stream(Command::future(async {
                panic!("boom");
                #[expect(
                    unreachable_code,
                    reason = "the value follows an unconditional panic! that never returns, but types the async block"
                )]
                1
            })),
        );

        wait_until(
            || probe.counts().1 == 1,
            "the contained panic should reach the delegated hook",
        )
        .await;
        let counts = probe.counts();
        probe.finish();
        assert_eq!(
            counts,
            (0, 1),
            "a keyed command task panic must delegate without restoring"
        );
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
        let mut manager = KeyedCommands::new(None, LoadObserver::default());
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

    // INV-L5 under bounded keyed delivery: a keyed command whose task is parked
    // awaiting capacity on a full channel is still cancelled cleanly. With
    // capacity 1 the task buffers its first output (filling the only slot) and
    // blocks on the second send; an explicit cancel removes the entry, drops the
    // buffered-but-undelivered output with it, and aborts the parked task.
    #[tokio::test]
    async fn bounded_keyed_cancel_drops_buffered_output_and_aborts_the_blocked_send() {
        struct AbortGuard(Arc<AtomicBool>);
        impl Drop for AbortGuard {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let id = CommandId::new("search");
        let dropped = Arc::new(AtomicBool::new(false));
        let guard = AbortGuard(Arc::clone(&dropped));
        let mut manager = KeyedCommands::new(
            Some(NonZeroUsize::new(1).expect("non-zero")),
            LoadObserver::default(),
        );

        // The guard rides the stream's state so its drop witnesses the parked
        // task being aborted (the second send never completes).
        let stream = stream::unfold((guard, 0_u8), |(guard, n)| async move {
            match n {
                0 => Some((Action::Message(1), (guard, 1_u8))),
                1 => Some((Action::Message(2), (guard, 2_u8))),
                _ => None,
            }
        })
        .boxed();
        manager.spawn(id.clone(), CancelPolicy::CancelInFlight, stream);

        wait_until(
            || {
                manager.entries.iter().any(|(entry_id, entry)| {
                    entry_id == &id && {
                        let facts = entry.receiver.facts();
                        !facts.sender_closed && facts.buffered == 1
                    }
                })
            },
            "the task should buffer its first output and block on the second send",
        )
        .await;

        manager.cancel(&id);

        assert!(!manager.contains(&id));
        assert!(manager.try_next_ready().is_none());
        wait_until(
            || dropped.load(Ordering::SeqCst),
            "cancelling the bounded keyed command should abort its parked send",
        )
        .await;
    }

    // INV-L9 under bounded keyed delivery: a `CancelInFlight` replacement gives
    // the successor a fresh private channel, so the old run's buffered and
    // blocked outputs cannot leak into it. Run A buffers 10 and parks on sending
    // 11 (capacity 1); run B replaces it and only B's output is ever delivered.
    #[tokio::test]
    async fn bounded_cancel_in_flight_replacement_isolates_the_old_runs_blocked_send() {
        let id = CommandId::new("search");
        let mut manager = KeyedCommands::new(
            Some(NonZeroUsize::new(1).expect("non-zero")),
            LoadObserver::default(),
        );

        manager.spawn(
            id.clone(),
            CancelPolicy::CancelInFlight,
            actions([Action::Message(10), Action::Message(11)]),
        );
        wait_until(
            || {
                manager.entries.iter().any(|(entry_id, entry)| {
                    entry_id == &id && {
                        let facts = entry.receiver.facts();
                        !facts.sender_closed && facts.buffered == 1
                    }
                })
            },
            "run A should buffer its first output and block on the second send",
        )
        .await;

        manager.spawn(
            id.clone(),
            CancelPolicy::CancelInFlight,
            actions([Action::Message(20)]),
        );

        wait_for_closed_buffered(&mut manager, &id).await;
        assert_eq!(take_message(&mut manager, &id), 20);
        assert!(manager.try_next_ready().is_none());
    }

    // INV-L10 under bounded keyed delivery: a keyed quit travels the private
    // channel in FIFO behind the same run's earlier output even when capacity 1
    // forces the quit send to park behind that output. This repeats the
    // unbounded ordering guarantee (`keyed_quit_is_delivered_after_the_same_runs_earlier_output`)
    // under `Some(1)`, where the ordering is enforced by backpressure rather
    // than by buffering.
    #[tokio::test]
    async fn keyed_quit_follows_earlier_output_under_capacity_one() {
        let id = CommandId::new("save-then-quit");
        let mut manager = KeyedCommands::new(
            Some(NonZeroUsize::new(1).expect("non-zero")),
            LoadObserver::default(),
        );
        manager.spawn(
            id.clone(),
            CancelPolicy::CancelInFlight,
            actions([Action::Message(1), Action::Quit]),
        );

        wait_until(
            || {
                manager.entries.iter().any(|(entry_id, entry)| {
                    entry_id == &id && entry.receiver.facts().buffered == 1
                })
            },
            "the first message should buffer before the blocked quit",
        )
        .await;
        assert_eq!(take_message(&mut manager, &id), 1);

        // Freeing the slot lets the parked quit send complete; the task then
        // exits and closes the channel, leaving the quit as the last item.
        wait_for_closed_buffered(&mut manager, &id).await;
        let (event_id, event) = manager
            .try_next_ready()
            .expect("keyed quit should be ready after the earlier message");
        assert_eq!(event_id, id);
        assert_eq!(event, ReceiverEvent::Output(CommandOutput::Quit));
    }
}

#[cfg(test)]
mod lifecycle_model_tests {
    use super::*;
    use futures::stream;
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

        fn apply(&mut self, input: Input) -> LifecycleDecision<()> {
            let event = match input {
                Input::SpawnCancel => LifecycleEvent::Spawn {
                    token: RunToken(self.next_token),
                    policy: CancelPolicy::CancelInFlight,
                    stream: stream::empty().boxed(),
                },
                Input::SpawnKeep => LifecycleEvent::Spawn {
                    token: RunToken(self.next_token),
                    policy: CancelPolicy::KeepInFlight,
                    stream: stream::empty().boxed(),
                },
                Input::Cancel => LifecycleEvent::Cancel,
                Input::Reconcile(facts) => LifecycleEvent::Reconcile(facts),
                Input::Output(facts) => LifecycleEvent::Output(facts),
                Input::TaskExit { token, facts } => LifecycleEvent::TaskExit { token, facts },
                Input::Closed => LifecycleEvent::Closed,
            };
            let decision = lifecycle_transition(self.state, event);
            if decision.starts_run() {
                self.next_token = self.next_token.wrapping_add(1);
            }
            self.state = decision.next_state(self.state);
            decision
        }
    }

    fn spawn_event(token: u64, policy: CancelPolicy) -> LifecycleEvent<()> {
        LifecycleEvent::Spawn {
            token: RunToken(token),
            policy,
            stream: stream::empty().boxed(),
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
    fn transition_produces_every_decision_variant() {
        let running = Some(LifecycleState::Running { token: RunToken(7) });
        let draining = Some(LifecycleState::Draining { token: RunToken(7) });
        let decisions = [
            lifecycle_transition(None, LifecycleEvent::<()>::Cancel),
            lifecycle_transition(running, spawn_event(8, CancelPolicy::KeepInFlight)),
            lifecycle_transition(None, spawn_event(8, CancelPolicy::CancelInFlight)),
            lifecycle_transition(running, spawn_event(8, CancelPolicy::CancelInFlight)),
            lifecycle_transition(draining, spawn_event(8, CancelPolicy::CancelInFlight)),
            lifecycle_transition(running, LifecycleEvent::<()>::Cancel),
            lifecycle_transition(draining, LifecycleEvent::<()>::Cancel),
            lifecycle_transition(
                running,
                LifecycleEvent::<()>::Reconcile(ReceiverFacts {
                    sender_closed: true,
                    buffered: 1,
                }),
            ),
        ];
        let mut seen = [false; 8];

        for decision in decisions {
            let variant_index = match decision {
                LifecycleDecision::NoChange => 0,
                LifecycleDecision::KeepInFlight { .. } => 1,
                LifecycleDecision::Start { token, .. } => {
                    assert_eq!(token, RunToken(8));
                    2
                }
                LifecycleDecision::ReplaceRunning { token, .. } => {
                    assert_eq!(token, RunToken(8));
                    3
                }
                LifecycleDecision::ReplaceDraining { token, .. } => {
                    assert_eq!(token, RunToken(8));
                    4
                }
                LifecycleDecision::AbortAndRemove => 5,
                LifecycleDecision::Remove => 6,
                LifecycleDecision::MarkDraining { token } => {
                    assert_eq!(token, RunToken(7));
                    7
                }
            };
            seen[variant_index] = true;
        }

        assert_eq!(seen, [true; 8]);
    }

    #[test]
    fn closed_empty_reconciliation_removes_the_owned_receiver() {
        let decision = lifecycle_transition(
            Some(LifecycleState::Running { token: RunToken(7) }),
            LifecycleEvent::<()>::Reconcile(ReceiverFacts {
                sender_closed: true,
                buffered: 0,
            }),
        );

        assert!(matches!(decision, LifecycleDecision::Remove));
    }

    proptest! {
        #[test]
        fn lifecycle_invariants_hold_for_arbitrary_sequences(sequence in inputs()) {
            let mut model = Model::new();

            for input in sequence {
                let before = model.state;
                let decision = model.apply(input);

                match input {
                    Input::Cancel => {
                        match before {
                            None => prop_assert!(matches!(decision, LifecycleDecision::NoChange)),
                            Some(LifecycleState::Running { .. }) => {
                                prop_assert!(matches!(decision, LifecycleDecision::AbortAndRemove));
                            }
                            Some(LifecycleState::Draining { .. }) => {
                                prop_assert!(matches!(decision, LifecycleDecision::Remove));
                            }
                        }
                        prop_assert_eq!(model.state, None);
                    }
                    Input::SpawnKeep if before.is_some() => {
                        let keeps_in_flight = matches!(
                            decision,
                            LifecycleDecision::KeepInFlight { .. }
                        );
                        prop_assert!(keeps_in_flight);
                        prop_assert_eq!(model.state, before);
                    }
                    Input::SpawnCancel => {
                        match before {
                            None => {
                                let starts = matches!(decision, LifecycleDecision::Start { .. });
                                prop_assert!(starts);
                            }
                            Some(LifecycleState::Running { .. }) => {
                                let replaces_running = matches!(
                                    decision,
                                    LifecycleDecision::ReplaceRunning { .. }
                                );
                                prop_assert!(replaces_running);
                            }
                            Some(LifecycleState::Draining { .. }) => {
                                let replaces_draining = matches!(
                                    decision,
                                    LifecycleDecision::ReplaceDraining { .. }
                                );
                                prop_assert!(replaces_draining);
                            }
                        }
                    }
                    Input::TaskExit { token, .. } => {
                        let matches_current = matches!(
                            before,
                            Some(LifecycleState::Running { token: current }) if current == token
                        );
                        if !matches_current {
                            prop_assert!(matches!(decision, LifecycleDecision::NoChange));
                            prop_assert_eq!(model.state, before);
                        }
                    }
                    Input::Reconcile(ReceiverFacts { sender_closed: true, buffered: 0 })
                    | Input::Output(ReceiverFacts { sender_closed: true, buffered: 0 }) => {
                        if before.is_some() {
                            prop_assert!(matches!(decision, LifecycleDecision::Remove));
                        } else {
                            prop_assert!(matches!(decision, LifecycleDecision::NoChange));
                        }
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
            let second_decision = model.apply(Input::Cancel);

            prop_assert_eq!(after_first, None);
            prop_assert_eq!(model.state, None);
            prop_assert!(matches!(second_decision, LifecycleDecision::NoChange));
        }
    }
}
