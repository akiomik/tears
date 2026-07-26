# RFC 0003: Command Cancellation

- Status: Implemented
- Target: 0.10.0, additive public API with an internal runtime rework
- Scope: opt-in cancellation for command output delivery, keyed by `CommandId`
- Feature flag: none
- CHANGELOG: `Added` (`CommandId`, `CancelPolicy`,
  `Command::cancellable`, `Command::cancellable_with`, `Command::cancel`)

## Summary

`Command` tasks are currently spawned into one unkeyed `JoinSet<()>`. The runtime
can abort all command tasks on shutdown, but it cannot cancel one in-flight
effect's output lifecycle. Applications that need "latest search wins", "cancel
the request when leaving this screen", or "ignore a second submit while the
first is running" must build their own task registry outside the TEA loop.

This RFC adds an opt-in keyed lifecycle for command output:

- `Command::cancellable(id)` runs a command under an identity.
- `Command::cancellable_with(id, policy)` chooses how same-id arrivals interact.
- `Command::cancel(id)` cancels the current deliverable effect for that id
  without starting a replacement.

The core guarantee is strict output cancellation: after a keyed effect is
superseded or cancelled, none of its messages or quit requests may affect the
application, even if that output was already buffered. This is not rollback for
external work that already happened. The runtime makes the guarantee by
construction: keyed effects send into runtime-owned private receivers, and
cancellation drops the receiver that could have delivered stale output.

Commands with no `CommandId` continue to use the existing unkeyed path.

## 1. Context and Constraints

### 1.1 Background

The current command path is:

1. `Application::update` returns a `Command`.
2. `Runtime::dispatch_update_command` lowers it once with
   `Command::into_runtime_parts()`.
3. The runtime reads redraw from `RuntimeCommandParts`, then
   `RuntimeCore::enqueue_command` consumes the optional stream.
4. If a stream exists, the runtime spawns one task into
   `command_tasks: JoinSet<()>`.
5. The task forwards `Action::Message` into `msg_tx` and `Action::Quit` into
   `quit_tx`.

Application messages are already routed through the internal `AppInputs` mux.
Today it contains only `AppInput::Shared`, wrapping the shared message receiver;
this RFC extends that mux with keyed command output.

That model intentionally tracks tasks so shutdown and panic unwinding abort them,
but it forgets per-command identity after spawn. In contrast,
`SubscriptionManager` already uses `SubscriptionId` to reconcile a keyed set of
long-lived streams. Commands need the same lifecycle discipline for short-lived
effects whose validity can be superseded by later application state.

### 1.2 Non-Negotiables

**A. Default behavior is unchanged.** A command without cancellation metadata
uses the existing unkeyed task path and shared channels. Existing applications do
not observe new cancellation behavior.

**B. Cancellation is strict over all command output.** Once a keyed run is
cancelled or superseded, no output from that run may be delivered later. This
covers `Action::Message`, `Action::Quit`, output buffered before the abort, and
output from a run whose task finished before the runtime drained its receiver.

**C. The design must be correct by construction.** The runtime must not rely on
application-level "is this response stale?" checks, generation filtering in
`update`, or a best-effort abort boundary. If output is stale, the runtime should
own and drop the only receiver from which that output could be observed.

**D. No detached tasks or unbounded completed-task records.** Running keyed tasks
are abortable by id and by shutdown. Completed keyed tasks are reaped. A stale
task completion must not be able to mutate a newer same-id run.

**E. `Action` stays closed.** Cancellation metadata is command state, not a new
`Action` variant. Effect streams still yield only `Action::Message` or
`Action::Quit`.

### 1.3 Goals and Non-Goals

#### Goals

- Add a public identity type for cancellable command output lifecycles.
- Support latest-wins (`CancelInFlight`) and double-submit prevention
  (`KeepInFlight`).
- Support pure cancellation with `Command::cancel(id)`.
- Define the runtime state machine and event-loop ordering tightly enough that
  implementation can start without resolving design questions.
- Pin the design with command unit tests, lifecycle property tests, and runtime
  contract tests.

#### Non-Goals

- `timeout` and `retry`. They are effect-local combinators and do not require
  cross-update runtime state. They shipped separately as
  [RFC 0004](./0004-command-timeout-retry.md). RFC 0004 §4.2 defines
  forward-integration contracts against this RFC's keyed metadata (`timeout`
  preserves `key`/`cancels`; cancelling or superseding a keyed command
  suppresses a pending timeout or retry final message through private-receiver
  drop); verifying those contracts is in scope for this RFC's implementation.
  See §5.1 and §7.3.
- `debounce` and `throttle`. They need clock injection for deterministic tests
  and can build on the keyed lifecycle later.
- Rolling back external work that has already started. Aborting a task may stop
  future polling and dropping the receiver stops output delivery, but this RFC
  does not guarantee cancellation of an HTTP request already accepted by a
  server, filesystem writes already performed, or other irreversible side
  effects.
- Public cancellation handles. Handles would move cancellation authority into
  the model and away from TEA values returned by `update`.
- Per-child cancellation inside `Command::batch`. This RFC defines cancellation
  at top-level command granularity. Child ids in a batch emit a warning and are
  a no-op in this scope; preserving them is future work.
- Refactoring `SubscriptionManager` to share the keyed task registry. The shapes
  are similar, but commands need private delivery channels and imperative
  replacement, while subscriptions use a declarative set diff.

## 2. Decision

This RFC accepts a keyed command-output lifecycle with these decisions:

- Public API adds `CommandId`, `CancelPolicy`, `Command::cancellable`,
  `Command::cancellable_with`, and `Command::cancel`.
- `CommandId` stores an erased structural value; equality is not reduced to a
  precomputed hash.
- Keyed commands deliver through runtime-owned private receivers, not through
  the shared message or quit channels.
- `CancelInFlight` replaces the current same-id run; `KeepInFlight` drops the
  new same-id stream while the id is occupied.
- `Command::cancel(id)` is strict and idempotent. It drops buffered output and
  aborts running work for the id when present.
- Occupancy is based on deliverability, not only task liveness: a finished run
  with buffered output still occupies the id.
- Cancellation is top-level-command scoped. Child cancellation keys inside
  `Command::batch` warn and are ignored; child explicit cancels fold into the
  batch.
- Commands returned by one app input are dispatched before the runtime pulls the
  next app input.
- At each app-input pull point, shared input is checked before keyed output so a
  shared navigation message can cancel ready keyed output before delivery.

## 3. Public API Details

`CommandId` and `CancelPolicy` are re-exported from `tears::command`
(`tears::command::CommandId`, `tears::command::CancelPolicy`), not the crate
root. Per `docs/api-guidelines.md`'s root promotion criteria, cancellation is
opt-in vocabulary: it is not named in an `Application` skeleton that doesn't
use it, and it is not the extension contract behind a skeleton item the way
`SubscriptionSource` is for `Subscription`. `Command::cancellable`,
`Command::cancellable_with`, and `Command::cancel` stay on `Command` itself
and need no separate import.

### 3.1 `CommandId`

`CommandId` is structural, not a pre-hashed surrogate. It stores an erased
`Eq + Hash + Send + Sync + 'static` value behind an opaque public type:

```rust
#[derive(Clone)]
pub struct CommandId { /* private */ }

impl CommandId {
    pub fn new<T>(value: T) -> Self
    where
        T: Eq + std::hash::Hash + Send + Sync + 'static;
}

impl std::fmt::Debug for CommandId { /* prints the erased type name only */ }
impl PartialEq for CommandId { /* TypeId + erased Eq */ }
impl Eq for CommandId {}
impl std::hash::Hash for CommandId { /* TypeId + erased Hash */ }
```

The type namespace is part of equality. `CommandId::new("search")` and
`CommandId::new(String::from("search"))` are different ids because their Rust
types differ. A borrowed field such as `self.query.as_str()` is rejected unless
the borrow is actually `'static`; use an owned value or a small marker enum:

```rust
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct PaneId(u64);

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
enum RequestId {
    Search,
    SearchPane(PaneId),
    Submit,
}

CommandId::new(RequestId::Search);
CommandId::new(RequestId::SearchPane(pane_id));
CommandId::new(RequestId::Submit);
```

Choose ids for the slot of work to supersede, not for the input payload. For
"latest search wins", `Search("a")` and `Search("ab")` are different ids and
therefore do not cancel each other. Use a stable slot such as `Search` or
`SearchPane(pane_id)` and pass the query only to the effect.

This deliberately differs from `SubscriptionId`'s `{ TypeId, u64 }` shape.
Command cancellation can discard user-visible work, so two distinct logical ids
must not alias merely because their hashes collide. Hash collisions remain
possible inside `HashMap`, but equality compares the erased values, so they do
not make two ids equal.

`CommandId::new<T>` does not require `T: Debug`, so `CommandId`'s own `Debug`
implementation must not promise to print the stored value. It is a diagnostic,
non-stable representation that identifies the erased type namespace only, for
example with an output shape such as:

```rust
CommandId { type: "my_crate::RequestId", .. }
```

Two unequal ids of the same Rust type may therefore have identical `Debug`
output. Equality and hashing are the semantic identity; `Debug` is only a clue
for logs and test failures.

### 3.2 `CancelPolicy`

```rust
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum CancelPolicy {
    #[default]
    CancelInFlight,
    KeepInFlight,
}
```

- `CancelInFlight`: cancel the currently deliverable same-id run and spawn the
  new one. This is the default and covers search-as-you-type.
- `KeepInFlight`: if the id is occupied, drop the new run and keep the current
  deliverable run. This covers double-submit prevention.

Occupancy means "a run is still running or has buffered output that has not yet
been delivered or dropped." It does not mean "there is an entry in `StreamMap`".
Before a policy decision, the runtime reconciles the id so a closed empty
entry is not considered occupied.

### 3.3 `Command` methods

```rust
impl<Msg: Send + 'static> Command<Msg> {
    pub fn cancellable(self, id: CommandId) -> Self;

    pub fn cancellable_with(self, id: CommandId, policy: CancelPolicy) -> Self;

    pub fn cancel(id: CommandId) -> Self;
}
```

Examples:

```rust
use tears::command::{CancelPolicy, CommandId};

Command::perform(fetch(query.clone()), Msg::Loaded)
    .cancellable(CommandId::new(RequestId::Search));

Command::perform(submit(form), Msg::Submitted)
    .cancellable_with(
        CommandId::new(RequestId::Submit),
        CancelPolicy::KeepInFlight,
    );

Command::cancel(CommandId::new(RequestId::Search));
```

`Command::cancel(id)` is idempotent. It aborts the running task for `id`, if any,
and drops all buffered output for that id. If the id is absent, it is a no-op.
It is policy-independent: explicit cancellation cancels a `KeepInFlight` run too.
Like other command constructors, it requests redraw by default; callers can use
`Command::cancel(id).without_redraw()` when cancellation is purely background
cleanup.

`Command::none().cancellable(id)` is inert because there is no effect stream to
spawn under the id. The same applies to
`Command::cancel(id).cancellable(other_id)`: the cancel still applies, but
`other_id` has no stream to key. Pure cancellation must use `Command::cancel(id)`.

## 4. Runtime Semantics

Runtime semantics are defined in terms of deliverable output, not just task
liveness. A keyed run remains current while it can still deliver output, even if
the producing task has already ended and output is buffered.

This section uses `KeyedCommands`, `AppInputs`, and helper method names as
semantic handles. Their concrete internal shapes are sketched in sections 5.4
and 5.5.

### 4.1 Private Receiver

Keyed command output travels through a private receiver:

```rust
enum CommandOutput<Msg> {
    Message(Msg),
    Quit,
}

enum ReceiverEvent<Msg> {
    Output(CommandOutput<Msg>),
    Closed,
}
```

The task wrapper owns the only sender. User code never sees it. The wrapper
translates `Action::Message` and `Action::Quit` into `CommandOutput` before they
reach the runtime.

`CommandReceiver` wraps `mpsc::UnboundedReceiver<CommandOutput<Msg>>`. When the
inner receiver is closed and empty, it yields `ReceiverEvent::Closed` instead of
returning `None`, then parks pending until the runtime removes it. This avoids a
`StreamMap` footgun: inner streams that return `None` are silently removed
without revealing which key closed.

`CommandReceiver` also exposes after-pull facts to its owning `KeyedCommands`:

```rust
struct ReceiverFacts {
    sender_closed: bool,
    buffered: usize,
}
```

These facts stay inside the keyed-command module; they are not fields of
`ReceiverEvent` or `AppInput`, and callers do not participate in lifecycle
accounting. `KeyedCommands` samples them immediately after an `Output` item is
pulled, applies the lifecycle transition, and only then returns the payload
toward `update()`. They are not a cleanup event; they tell the lifecycle whether
the just-delivered item was the last possible item from that sender. The
implementation reads `sender_closed` from
`mpsc::UnboundedReceiver::is_closed()` and `buffered` from `len()`:

- `sender_closed && buffered == 0` releases the id before `update()` can return
  same-id work (INV-7).
- `sender_closed && buffered > 0` moves the entry to `Draining`.

`ReceiverEvent::Closed` remains the cleanup signal for a receiver that is closed
and empty. Task-exit reconciliation is still required to reap `JoinSet` records
and to mark a running entry as `Draining` when the task exit is observed before
receiver facts are sampled.

### 4.2 State Machine

For each `CommandId`, the abstract state is:

```text
Absent
Running  { token, abort }  // task may still send; receiver may have output
Draining { token }         // task ended; receiver has buffered output
```

`Draining` is non-empty by invariant. If the receiver is closed and empty, the
state is `Absent`.

The production transition returns a closed decision type. Its shape is:

```rust
enum LifecycleDecision<Msg> {
    NoChange,
    KeepInFlight { stream: BoxStream<'static, Action<Msg>> },
    Start { token: RunToken, stream: BoxStream<'static, Action<Msg>> },
    ReplaceRunning { token: RunToken, stream: BoxStream<'static, Action<Msg>> },
    ReplaceDraining { token: RunToken, stream: BoxStream<'static, Action<Msg>> },
    AbortAndRemove,
    Remove,
    MarkDraining { token: RunToken },
}
```

The transition table is therefore expressed in decisions, not in an
independent product of a next state and boolean effects:

```text
state     input                              decision
--------- ---------------------------------- -----------------------------------------
Absent    Spawn(any policy)                  Start { token, stream }
Absent    Cancel                             NoChange
Absent    TaskExit(any)                      NoChange

Running   Spawn(CancelInFlight)              ReplaceRunning { token, stream }
Running   Spawn(KeepInFlight)                KeepInFlight { stream }
Running   Cancel                             AbortAndRemove
Running   Output(sender open)                NoChange
Running   Output(sender_closed, buf > 0)     MarkDraining { token }
Running   Output(sender_closed, buf == 0)    Remove
Running   ReceiverEvent::Closed              Remove
Running   TaskExit matching, buf > 0         MarkDraining { token }
Running   TaskExit matching, buf == 0        Remove
Running   TaskExit stale                     NoChange

Draining  Spawn(CancelInFlight)              ReplaceDraining { token, stream }
Draining  Spawn(KeepInFlight)                KeepInFlight { stream }
Draining  Cancel                             Remove
Draining  Output(buf > 0)                    NoChange
Draining  Output(buf == 0)                   Remove
Draining  ReceiverEvent::Closed              Remove
Draining  TaskExit matching                  NoChange
Draining  TaskExit stale                     NoChange
```

The transition function is total for task-exit inputs because late completions
are normal under cancellation. Delivery inputs are generated only while a
receiver is owned; property tests should still include stale and late `TaskExit`
sequences across all states.

Each decision variant fixes both the next logical state and the permitted
physical action. For example, `ReplaceRunning` always removes the old receiver,
aborts the old running task, and starts the supplied successor;
`ReplaceDraining` removes the old receiver and starts the successor without an
abort; `AbortAndRemove` cannot retain a receiver; and `MarkDraining` cannot
remove one. There is no representation corresponding to arbitrary combinations
such as “spawn without becoming `Running`” or “abort while retaining the same
running entry.” The type therefore makes logical-state/physical-action
contradictions unrepresentable.

Before any `Spawn(policy)` decision, the manager reaps every completed keyed task
currently available, then samples receiver facts once for the target id. Each
matching `TaskExit` and the target receiver snapshot apply the same pure
lifecycle transition used by delivery accounting: a stale token is ignored, a
closed-empty run becomes `Absent`, and a run with buffered output becomes
`Draining`. The targeted snapshot closes the interval where a panicking task has
dropped its sender but has not yet published its `TaskExit` because panic logging
is still in progress.

`KeepInFlight` consults this reconciled state. A same-id retry returned by the
`update` that just handled keyed output is spawned only when the pre-spawn
reconciliation can prove the previous receiver is both sender-closed and empty.
If the previous receiver is still open, as it may be for an arbitrary
`Command::stream`, the id remains occupied and `KeepInFlight` drops the retry.
The runtime does not infer that a delivered item was a stream's final result
unless internally sampled `ReceiverFacts` or a reaped task exit establish that
the previous run no longer owns deliverable output.

### 4.3 Dispatching a Command

The current runtime already lowers `Command` into `RuntimeCommandParts` before
enqueueing:

```text
dispatch_update_command(cmd):
  parts = cmd.into_runtime_parts()
  needs_redraw |= parts.requests_redraw()
  state.enqueue_command(parts)

enqueue_command(parts):
  if parts.stream is None:
      return
  spawn_unkeyed(parts.stream)   // current behavior
```

RFC 0003 extends the same parts value with `cancels` and `key`, then changes the
enqueue path to:

```text
enqueue_command(parts):
  app_inputs.reconcile_keyed_available()
  for id in parts.cancels:
      app_inputs.cancel_keyed(id)
  if parts.stream is None:
      return
  if parts.key is None:
      spawn_unkeyed(parts.stream)   // current behavior
  else:
      app_inputs.spawn_keyed(parts.key.id, parts.key.policy, parts.stream)
```

The stream-less early return happens after cancels are applied. A command with
neither cancels nor a stream still takes today's no-op path. Enqueue-time
reconciliation remains necessary for cancel-only commands; it only drains
currently ready `JoinSet` exits and does not scan every live keyed receiver.

### 4.4 Event Loop and Drain Discipline

Application-facing inputs are already multiplexed behind one internal source.
RFC 0003 extends that mux with keyed receiver events:

```rust
enum AppInput<Msg> {
    Shared(Msg),
    Keyed(ReceiverEvent<Msg>),
}

struct AppInputs<Msg> {
    shared: mpsc::UnboundedReceiver<Msg>,
    keyed: KeyedCommands<Msg>,
}

impl<Msg: Send + 'static> AppInputs<Msg> {
    fn try_next_ready(&mut self) -> Option<AppInput<Msg>>;
    fn reconcile_keyed_available(&mut self);
    fn cancel_keyed(&mut self, id: CommandId);
    fn spawn_keyed(
        &mut self,
        id: CommandId,
        policy: CancelPolicy,
        stream: BoxStream<'static, Action<Msg>>,
    );
}
```

`AppInputs` owns the shared message receiver and the keyed receiver merge. It
implements `Stream<Item = AppInput<Msg>>` for the outer wait path and exposes
`try_next_ready()` for the micro-batch path. Both paths use the same internal
priority: at each app-input pull point, shared input is checked before keyed
output.

```rust
tokio::select! {
    Some(input) = self.core.app_inputs.next() => {
        match self.process_input_batch(input) {
            BatchOutcome::Continue => {}
            BatchOutcome::Quit => break,
        }
    }

    () = self.scheduler.next_work_frame() => {
        self.process_frame_tick(terminal)?;
    }

    _ = self.core.quit_rx.recv() => break,
}
```

The top-level `select!` keeps Tokio's normal fairness between app input, frame,
and quit. The shared-first bias exists only inside `AppInputs`: if a shared
message and keyed output are ready at the same app-input pull point, the shared
message is returned first. This lets navigation messages such as `LeaveScreen`
cancel a ready keyed result before it is delivered, without making shared
messages globally higher priority than frame ticks or quit signals.

The guarantee is local to the pull point. Once a keyed item has been pulled
because no shared message was ready, a later shared message does not
retroactively precede it. A continuous stream of ready shared inputs can delay
keyed output; that cancellation-safety tradeoff is accepted for this RFC.

`process_input_batch` already exists on `main` as the prerequisite rename of
the former `process_message_batch`; today it only drains shared messages and
returns `()`. RFC 0003 extends it to also drain keyed receiver events and to
return `BatchOutcome::Quit` only for keyed `Quit`, otherwise
`BatchOutcome::Continue`. It keeps the existing 100 microsecond micro-batch
window. The control contract is:

```text
process_input_batch(first_input):
  process first_input through process_app_input
  loop until batch deadline:
    item = app_inputs.try_next_ready()
    if no item is ready: break
    process item through process_app_input
  set subscriptions_dirty only if at least one item ran update()
```

`process_app_input` is the only place that interprets `AppInput`; keyed lifecycle
accounting has already happened inside `KeyedCommands` before an event reaches
this layer:

- `ReceiverEvent::Closed` does not call `update()`.
- A keyed message calls `update(msg)`.
- A shared message calls `update(msg)` directly.
- Any command returned by `update()` is dispatched before another app input is
  pulled.
- A keyed `Quit` exits through the same shutdown path as `quit_rx`; it does not
  call `update()`.

`AppInputs::try_next_ready()` is the non-blocking batch-drain counterpart to
`AppInputs::poll_next`:

```text
AppInputs::try_next_ready():
  if shared.try_recv() yields a shared message:
      return Shared(msg)
  if keyed.try_next_ready() yields a keyed receiver event:
      return Keyed(event)
  return None
```

`keyed.try_next_ready()` polls the keyed `StreamMap` at most once and treats
`Poll::Pending` as "not ready"; it must not await. The loop drains already-ready
work, then returns to the outer `select!`, where `poll_next` registers real
wakers. Keeping shared-first inside `AppInputs` prevents the wait path and batch
drain from diverging.

The blocking path does not interpret a keyed `Poll::Pending` through a separate
emptiness query. `KeyedCommands::poll_event` reconciles completed tasks, polls
the receiver merge, and returns a dedicated result:

```rust
enum KeyedPoll<Msg> {
    Item(CommandId, ReceiverEvent<Msg>),
    PendingWithWakeSource,
    Quiescent,
}
```

`Quiescent` means reconciliation has completed and no keyed entry remains.
`PendingWithWakeSource` means at least one keyed receiver remains and was polled
with the current context, so it registered the current waker with a channel that
can wake after output or sender closure. An empty manager never returns
`PendingWithWakeSource`.

After checking shared input first, `AppInputs::poll_next` applies these rules:

```text
shared result   keyed result              AppInputs result
--------------- ------------------------- -----------------------------
Item(message)   not polled                Ready(Some(Shared(message)))
open/pending    Item(event)               Ready(Some(Keyed(event)))
closed          Item(event)               Ready(Some(Keyed(event)))
open/pending    PendingWithWakeSource     Pending
closed          PendingWithWakeSource     Pending
open/pending    Quiescent                 Pending
closed          Quiescent                 Ready(None)
```

The last row is decided in the same `AppInputs::poll_next` invocation in which
keyed reconciliation reports `Quiescent`; it does not first return an unwakeable
`Pending`. Consequently, every `Poll::Pending` returned by `AppInputs` is
future-wakeable: either the open shared receiver or at least one keyed receiver
has registered the current waker.

No prefetching across `update` is allowed. This is part of the cancellation
contract: if one message returns `Command::cancel(id)`, buffered keyed output for
`id` that has not yet been pulled is dropped before the next pull. For keyed
messages, delivery is recorded before `update()` so a sender-closed empty
receiver can release the id before same-id retry work is returned.

`subscriptions_dirty` is set only when at least one item in the batch actually
ran through `update()`. `ReceiverEvent::Closed` and keyed `Quit` do not dirty
subscriptions by themselves.

## 5. Implementation Notes

### 5.1 Command Representation

`Command` gains cancellation metadata beside `effect` and `directives`:

```rust
pub struct Command<Msg: Send + 'static> {
    effect: Effect<Msg>,
    directives: RuntimeDirectives,
    cancellation: CommandCancellation,
}

struct CommandCancellation {
    key: Option<CancellableCommand>,
    cancels: Vec<CommandId>,
}

struct CancellableCommand {
    id: CommandId,
    policy: CancelPolicy,
}
```

`Command::into_runtime_parts()` is the only runtime lowering boundary. This RFC
extends that existing internal boundary so cancellation metadata, runtime
directives, and the optional effect stream are observed together:

```rust
struct RuntimeCommandParts<Msg: Send + 'static> {
    directives: RuntimeDirectives,
    stream: Option<BoxStream<'static, Action<Msg>>>,
    cancels: Vec<CommandId>,
    key: Option<CancellableCommand>,
}
```

Construction rules:

- Every existing constructor defaults to `CommandCancellation::default()`.
- `cancellable(id)` sets `key = Some({ id, CancelInFlight })`.
- `cancellable_with(id, policy)` sets `key = Some({ id, policy })`.
- Repeated calls to `cancellable` / `cancellable_with` are last-call-wins. They
  replace the command's keyed spawn metadata, not the explicit cancel list or
  runtime directives.
- `cancel(id)` creates a stream-less command with `cancels = vec![id]`.
- `map` preserves `key`, `policy`, `cancels`, and runtime directives.
- `batch` folds runtime directives and unions child `cancels`.
- `timeout` (RFC 0004) preserves `key`, `policy`, `cancels`, and runtime
  directives; it changes only the wrapped effect. `.timeout(...).cancellable(id)`
  and `.cancellable(id).timeout(...)` both leave the command keyed under `id`.
  A `retry` / `retry_if` command carries the default (empty) cancellation
  metadata supplied by `Command::future`, same as any other fresh command.

The public docs for `cancellable` and `cancellable_with` must state the batch
boundary explicitly: applying them to a command that later becomes a child of
`Command::batch` is not preserved by this RFC. Put the cancellation key on the
top-level batch if the whole batch should be cancellable.

Cancellation application order is fixed: for one returned command, all explicit
cancels are applied before the command's own keyed spawn. Therefore:

```rust
Command::batch(vec![Command::cancel(id.clone()), work]).cancellable(id)
```

first drops the old `id` run and then starts `work` under `id`.

### 5.2 Batch Boundary

This RFC's granularity is one top-level command task. A batch returned directly
from `update` is one command and can have at most one cancellation key:

```rust
Command::batch(vec![load_user(), load_posts()])
    .cancellable(CommandId::new(RequestId::RefreshPage));
```

A cancellable child inside a batch is ignored in this RFC, with a warning-level
tracing event:

```rust
Command::batch(vec![
    load_user().cancellable(CommandId::new(RequestId::LoadUser(user_id))),
    load_posts(),
]);
```

The warning is intentional. Preserving child ids requires the runtime to spawn
multiple independently keyed tasks from one returned command, and that is a
separate per-effect cancellation design. Child `Command::cancel(id)` values are
not ignored because they fold unambiguously into the batch's cancel list.

### 5.3 `CommandId` Erasure

Implementation sketch:

```rust
trait ErasedCommandId: Send + Sync {
    fn as_any(&self) -> &dyn std::any::Any;
    fn type_id(&self) -> std::any::TypeId;
    fn type_name(&self) -> &'static str;
    fn eq_erased(&self, other: &dyn ErasedCommandId) -> bool;
    fn hash_erased(&self, state: &mut dyn std::hash::Hasher);
}

struct TypedCommandId<T>(T);
```

`eq_erased` uses `other.as_any().downcast_ref::<T>()` and compares the stored
value. `hash_erased` hashes `TypeId::of::<T>()` and the stored value directly
with `&mut state`; std's `Hasher` implementation for `&mut H` makes that work
with `&mut dyn Hasher`. The public `CommandId` is
`Arc<dyn ErasedCommandId>`.

### 5.4 Keyed Command Manager

The keyed command runtime is a lifecycle manager owned by `RuntimeCore`.
Its `entries` map is the single source of truth for deliverability: an absent
key means `Absent`; a present entry owns both the receiver and the run state.

```rust
struct KeyedCommands<Msg: Send + 'static> {
    entries: StreamMap<CommandId, KeyedEntry<Msg>>,
    tasks: JoinSet<TaskExit>,
    next_token: u64,
}

struct KeyedEntry<Msg> {
    receiver: CommandReceiver<Msg>,
    run: KeyRun,
}

enum KeyRun {
    Running {
        token: RunToken,
        abort: tokio::task::AbortHandle,
    },
    Draining {
        token: RunToken,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
struct RunToken(u64);

struct TaskExit {
    id: CommandId,
    token: RunToken,
}
```

`RunToken` is load-bearing. A superseded or cancelled task can complete after a
new same-id task has started, so task exits mutate state only when `(id, token)`
still matches the current `Running` entry. Stale exits are ignored.

`KeyedCommands` is a dynamic manager, not a normally terminating `Stream`.
Having no entries is a quiescent snapshot, not permanent termination: a later
runtime command may insert a new keyed run. A conventional stream result cannot
express both that reusable quiescence and pending keyed work without collapsing
one of them into `Ready(None)` or `Pending`. The manager therefore exposes the
dedicated `KeyedPoll` result from section 4.4 and leaves only `AppInputs` as the
terminating application-input stream.

`KeyedEntry<Msg>` implements `Stream<Item = ReceiverEvent<Msg>>` by delegating to
`CommandReceiver`; `StreamMap` is only the polling merge. Policy decisions,
delivery accounting, receiver removal, and task-exit reconciliation all go
through `KeyedCommands` methods. After a receiver event is pulled, its facts are
sampled and the transition is applied before the event leaves this module. If an
implementation needs keyed mutable access that `StreamMap` does not expose, it
may temporarily remove, update, and reinsert an entry, but that pattern must stay
inside `KeyedCommands`; identity transitions return early without churning the
`StreamMap`.

The pure production transition returns `LifecycleDecision`; one transition
applier matches that closed type exhaustively. `Start`, `ReplaceRunning`, and
`ReplaceDraining` perform their corresponding task/receiver insertion paths;
`AbortAndRemove`, `Remove`, and `MarkDraining` perform exactly the removal,
abort, or state update encoded by the variant; `KeepInFlight` drops only the
rejected incoming stream; and `NoChange` performs no `StreamMap`
remove/reinsert. Adding a lifecycle action therefore requires updating both the
closed decision type and this exhaustive match, rather than constructing a new
combination of independent effect flags.

### 5.5 Spawning a Keyed Run

Keyed runs use their own `JoinSet<TaskExit>`, separate from unkeyed
`command_tasks`. The spawn path:

1. Allocate a fresh `RunToken`.
2. Create a private unbounded channel.
3. Spawn the translation task into `tasks` and keep the returned `AbortHandle`.
4. Insert `KeyedEntry { receiver, run: KeyRun::Running { token, abort } }` into
   `entries` under the `CommandId`.

The task body:

- catches unwinds and logs `"keyed command task panicked"`;
- forwards `Action::Message(msg)` as `CommandOutput::Message(msg)` and stops if
  sending fails because the receiver has already been dropped;
- attempts to forward `Action::Quit` as `CommandOutput::Quit`, ignores the send
  result, and then stops polling the stream;
- returns `TaskExit { id, token }` on normal or panic-caught completion.

The sender is dropped when this wrapper exits, so receiver closure means the task
body has ended. Aborted tasks usually reap as `JoinError::cancelled`; those
results carry no `TaskExit` and are ignored because the lifecycle transition that
called `abort()` already removed the state and receiver.

### 5.6 Shutdown and Drop

`RuntimeCore::shutdown`:

- shuts down subscriptions as today;
- aborts all unkeyed command tasks as today;
- aborts all keyed command tasks;
- clears keyed entries.

The keyed task set is a `JoinSet`, not a map of bare `JoinHandle`s. A bare
`JoinHandle` detaches on drop; `JoinSet` aborts its tasks on drop, matching the
existing command-task shutdown guarantee.

## 6. Invariants

- **INV-1: default path.** A command without cancellation metadata uses the same
  shared `msg_tx` / `quit_tx` path as today.
- **INV-2: single deliverable receiver.** For one `CommandId`, the lifecycle owns
  at most one receiver. Supersede and cancel drop the previous receiver before a
  successor can be deliverable.
- **INV-3: strict latest-wins.** After `CancelInFlight` supersedes a run, no
  message or quit from the old run can affect the app, even if it was already
  buffered.
- **INV-4: explicit cancel is strict and idempotent.** `Command::cancel(id)` drops
  buffered output and aborts the running task if present; repeating it is the
  same as applying it once.
- **INV-5: `KeepInFlight` drops only the new stream.** While an id is occupied,
  a `KeepInFlight` arrival is not spawned. The command's redraw directive and
  explicit cancel list have already been processed.
- **INV-6: finished-but-buffered output is cancellable.** A task that has ended
  but whose receiver still contains output occupies the id until the output is
  delivered or the receiver is dropped.
- **INV-7: sender-closed empty receivers release before retry.** If pre-spawn
  reconciliation observes that the previous receiver is sender-closed and empty,
  same-id work returned by the current `update` is not dropped as in-flight. A
  still-open stream remains occupied even after delivering one item.
- **INV-8: stale task exits are inert.** A `TaskExit` whose `(id, token)` does not
  match the current `Running` state cannot remove or mutate current state.
- **INV-9: buffered quit is cancellable.** A cancelled or superseded keyed
  `Action::Quit` does not quit the application. A live keyed `Action::Quit` does.
- **INV-10: one-item drain.** Commands returned by one message are dispatched
  before the next shared or keyed item is pulled.
- **INV-11: batch folding.** `Command::batch` folds cancels and directives;
  child cancellation keys are ignored with a warning-level tracing event; a key
  on the batch itself applies to the batch task.
- **INV-12: map propagation.** `Command::map` preserves cancellation key,
  policy, cancel list, and directives.
- **INV-13: bounded keyed bookkeeping.** Completed keyed tasks are reaped,
  closed empty entries are explicitly removed, and no auxiliary state survives
  after the entry for an id is gone.
- **INV-14: shared-first app-input scheduling.** Inside `AppInputs`, when a
  shared message and a keyed output are ready at the same app-input pull point,
  the shared message is returned first. The top-level event loop remains fair
  between app input, frame ticks, and quit signals, but this RFC does not provide
  bounded fairness between shared and keyed app inputs.
- **INV-15: closed lifecycle action space.** Invalid lifecycle action
  combinations are not representable. The production transition returns a
  closed decision whose variant determines both the next logical state and the
  exhaustive physical action applied by `KeyedCommands`.
- **INV-16: pending is future-wakeable.** Every `Poll::Pending` returned by
  `AppInputs` has registered the current waker with an open shared receiver or a
  remaining keyed receiver. If shared input is closed and keyed reconciliation
  reports `Quiescent`, the same poll returns `Poll::Ready(None)`.

## 7. Testing Strategy

### 7.1 Command Unit Tests

White-box tests in `src/command.rs`:

- `CommandId` equality is structural: same type and equal value match; distinct
  types do not match; hash collisions do not imply equality.
- `CancelPolicy::default()` is `CancelInFlight`.
- constructors default to no cancellation metadata;
- `cancellable` and `cancellable_with` set key and policy;
- repeated `cancellable` / `cancellable_with` calls are last-call-wins and do
  not drop explicit cancels or directives;
- `cancel` creates a stream-less command with one cancel id;
- `map` preserves all cancellation metadata;
- `batch` unions child cancels, preserves directives, ignores child keys with a
  warning event, and lets a key applied to the batch win.

### 7.2 Pure Lifecycle Property Tests

Use `proptest` to call the same pure `lifecycle_transition` function used by
production `KeyedCommands`. Generated sequences supply lifecycle states, spawn
policies, run tokens, and abstract receiver facts (`sender_closed`, buffered
count) directly to that production function and assert on its closed
`LifecycleDecision` result.

The property-test sequence driver may retain the current state and next token so
it can feed the next generated event, but it must derive state changes from the
production decision. It must not implement and test a separate pure transition
model or a parallel state/effect table. Deterministic transition tests also
exercise every decision variant so a variant cannot exist only for an
unexplained applier-internal purpose.

Properties:

- at most one receiver is owned per id;
- cancel is total and idempotent: `Running` produces `AbortAndRemove`,
  `Draining` produces `Remove`, and `Absent` produces `NoChange`;
- `CancelInFlight` produces `Start`, `ReplaceRunning`, or `ReplaceDraining`
  according to the reconciled state;
- stale and late `TaskExit` inputs are accepted in every state and do not change
  current state unless they match the current `Running` token;
- `KeepInFlight` on occupied state produces `KeepInFlight`, retains the current
  state, and never starts a successor;
- delivering the last item of a sender-closed receiver transitions to `Absent`;
- `Draining` is reachable only with buffered output.

### 7.3 Runtime Contract Tests

Runtime tests should use deterministic synchronization (`Notify`, oneshot
channels, and paused time where useful), not sleeps.

Required coverage:

- **Default path and shutdown:** unkeyed commands still deliver messages and
  quit as before; keyed task panics are logged; dropping or shutting down the
  runtime aborts keyed tasks.
- **Strict cancellation:** `CancelInFlight` drops output from both running and
  already-finished old runs; `Command::cancel(id)` aborts running work and drops
  buffered output; a cancelled buffered keyed `Quit` does not exit, while a live
  keyed `Quit` exits through the same path as `quit_rx`.
- **Policy and occupancy:** `KeepInFlight` drops the new arrival while the old
  run is running or finished-but-buffered; redraw directives and explicit
  cancels on that dropped arrival have already been processed; same-id retry
  work is spawned only when reconciliation proves the previous receiver is
  sender-closed and empty.
- **Stale completion safety:** a stale `TaskExit` from an aborted old run cannot
  remove or mutate a successor.
- **Drain ordering:** shared-first ordering lets a shared `LeaveScreen` cancel a
  ready keyed result before delivery; `try_next_ready()` follows the same
  shared-first rule as `poll_next` and never awaits keyed streams; an unkeyed
  message returning `Command::cancel(id)` prevents the next buffered keyed item
  for `id` in the same batch from being delivered.
- **Poll liveness:** pending keyed work wakes after output or sender closure;
  when shared input is closed and reconciliation removes the final keyed entry,
  the same app-input poll returns `Ready(None)` rather than `Pending`.
- **RFC 0004 forward integration:** cancelling a keyed command before its
  `.timeout(...)` deadline elapses suppresses the timeout message; superseding
  a keyed command during a retry's backoff delay suppresses that retry's final
  message; `KeepInFlight` prevents a second retrying command from spawning
  under an occupied id. These contracts are defined in
  [RFC 0004](./0004-command-timeout-retry.md) §4.2 and are pinned here, not
  there.

## 8. Alternatives Considered

### Best-effort abort

Rejected. `AbortHandle::abort()` stops future polling, but it cannot recall a
message or quit already sent into the shared channels. Applications would still
need stale-result guards, and they cannot guard a stale `Action::Quit` because it
does not pass through `update`.

### Generation filtering on the shared channel

Rejected. Tagging keyed messages with `(CommandId, generation)` and filtering at
drain time can suppress stale messages, but it is not structurally clean:

- stale output still exists in the shared channel and must be filtered correctly
  at every drain site;
- `Action::Quit` currently uses a separate channel, so it would need a parallel
  tagging path or a larger event-loop rewrite;
- releasing old generation state without racing an abort requires extra
  bookkeeping.

Private receivers remove the stale output source instead of compensating for it.

### Pre-hashed `CommandId` matching `SubscriptionId`

Rejected. `SubscriptionId::of::<T>(u64)` is cheap and already shipped, but it
can alias if two logical ids produce the same hash. For cancellation, aliasing
can abort or suppress the wrong side effect. `CommandId` therefore stores the
erased value and uses real equality.

### `Command::none().cancellable(id)` as pure cancel

Rejected. It makes a call that reads like "no side effect" perform a cancel, and
`none().cancellable_with(id, KeepInFlight)` becomes well-typed but meaningless.
Pure cancellation is explicit: `Command::cancel(id)`.

### Cancellation handles

Rejected. A handle-based API asks the model to store imperative capabilities.
The TEA-shaped API keeps cancellation as a value returned by `update`, which is
easier to reason about and test.

### Preserve child ids inside `Command::batch` now

Deferred. It is the better long-term composition story, but it changes the
runtime lowering from "one returned command spawns one task" to "one command may
spawn multiple independently keyed tasks." This RFC keeps the first
implementation small and explicit: the batch is the cancellation boundary, child
keys warn and do nothing, child cancels fold.

### Occupancy liveness and generation filtering

Rejected. Task liveness is not occupancy, and delivery does not use generation
filtering over the shared channels: a finished task with buffered output is
still deliverable stale output, and a dying task can enqueue after a recorded
generation release point. The prerequisite `Effect` leaf refactor (PR #137)
keeps per-effect ids possible later, but this RFC deliberately ships the smaller
per-command lifecycle first.

## 9. Implementation Plan

The prerequisite refactors are already on `main`: `AppInputs` owns the
application input mux, and `Command::into_runtime_parts()` lowers commands into
`RuntimeCommandParts` before runtime enqueueing. The cancellation implementation
starts from those seams.

1. Add `src/command/cancellation.rs` with `CommandId`, `CancelPolicy`, and
   internal `CommandCancellation` helpers.
2. Extend `RuntimeCommandParts` with `cancels` and `key`; extend `Command` with
   a `cancellation` field; update every constructor, `map`, `batch`, docs, and
   tests.
3. Export `CommandId` and `CancelPolicy` only from the canonical
   `crate::command` module; do not re-export them from the crate root or
   `prelude`.
4. Add the runtime-internal keyed command manager in
   `src/runtime/keyed_commands.rs`.
5. Implement `CommandReceiver` so receiver closure is surfaced as
   `ReceiverEvent::Closed` instead of letting `StreamMap` silently remove keys.
6. Extend `AppInputs` with `AppInput::Keyed` and route keyed receiver polling
   through the existing app-input mux.
7. Add `proptest` as a dev-dependency and property-test the same pure lifecycle
   transition function used by production.
8. Add lifecycle property tests and runtime contract tests before widening the
   public docs.

## 10. Future Work

- Per-effect cancellation ids inside `Command::batch`.
- `debounce(id, duration)` and `throttle(id, duration)` once clock injection is
  available.
- Scoped cancellation for feature teardown, such as cancelling all tasks owned
  by a pane when it closes.
- A shared keyed-task helper for command cancellation and subscriptions after
  both concrete lifecycle shapes have shipped.
- A structural `SubscriptionId` follow-up, if the allocation and API tradeoff is
  acceptable there too.

## References

- `src/command.rs`
- `src/command/effect.rs`
- `src/command/retry.rs`
- `src/command/runtime_directives.rs`
- `src/command/runtime_parts.rs`
- `src/runtime.rs`
- `src/runtime/core.rs`
- `src/runtime/app_input.rs`
- `src/subscription.rs`
- `docs/rfcs/0001-http-module-redesign.md`
- `docs/rfcs/0002-redraw-suppression.md`
- `docs/rfcs/0004-command-timeout-retry.md`
- TCA `Effect.cancellable(id:cancelInFlight:)`
- TCA `Effect.cancel(id:)`
- RxJS `switchMap` and `exhaustMap`
- redux-saga `takeLatest` and `takeLeading`
