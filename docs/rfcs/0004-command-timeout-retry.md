# RFC 0004: Command Timeout / Retry

- Status: Accepted
- Target: 0.9.3 (next non-breaking patch), additive public API only
- Scope: timeout / retry lifecycle control confined to a single effect's
  execution; no runtime changes
- Feature flag: none
- CHANGELOG: `Added` (`Command::timeout`, `Command::retry`, `Command::retry_if`,
  `RetryPolicy`, `RetryBackoff`, `RetryContext`, `RetryError`, `RetryStopReason`)

> This RFC holds the observable contracts, invariants, and rationale, plus a
> minimal implementation sketch (§9) for the mechanisms the contracts depend
> on. Implementation details not fixed here are left to the implementer.

## Summary

This RFC adds two lifecycle controls to `Command` that are confined to a single
effect's execution.

- `Command::timeout(duration, on_timeout)`: attaches an overall deadline to
  each effect leaf of the target command and emits a timeout message
  **at most once per `.timeout()` call** when a deadline is reached.
- `Command::retry(policy, operation, f)` / `Command::retry_if(policy,
  operation, should_retry, f)`: constructors that take a repeatable future
  factory and build a command that re-executes it according to a
  `RetryPolicy`.

Both are effect-local: they do not need the keyed, multi-update runtime state
that RFC 0003 (command cancellation) introduces. The runtime (`src/runtime.rs`,
`src/runtime/app_input.rs`) is not changed.

These two features were originally tagged "no RFC needed — they add no
invariants" in the project backlog. Working out the design showed that they do
carry (a) public contracts that are hard to walk back (the at-most-once
guarantee, permanently claiming the `retry` name, treating `Quit` as terminal)
and (b) integration contracts with RFC 0003, so the judgment was revised and
this RFC records them. The original boundary judgment — that both features are
effect-local — was correct and is kept here as a non-negotiable.

## 1. Context and Constraints

### 1.1 Background

`Command<Msg>` is a two-layer structure of effect plus directives.

```rust
pub struct Command<Msg: Send + 'static> {
    effect: Effect<Msg>,
    directives: RuntimeDirectives,
}
```

- `Effect<Msg>` is `None | Leaves(Vec<BoxStream<'static, Action<Msg>>>)`. It
  keeps a flat sequence of leaf streams and only folds them (via `select_all`)
  at the `into_stream()` boundary.
- As noted in the RFC 0002 addendum and RFC 0003, these concerns live in
  separate layers:
  - Output treatment (`without_redraw`): a directive over the whole update
    result. Held as a field, folded by `batch`.
  - Effect-local lifecycle (`timeout` / `retry`): `timeout` wraps effect leaf
    streams, while `retry` is a constructor that closes over a repeatable
    operation factory.
  - Runtime lifecycle (`cancellable`): RFC 0003 represents cancellation as
    runtime metadata (`cancels` / `key`) on `RuntimeCommandParts`, not as a
    leaf-stream wrapper. Child keys inside `Command::batch` are intentionally
    not preserved by RFC 0003.
- `timeout` and `retry` close over a single effect. RFC 0003's cancellation
  metadata is separate because cancellation needs keyed state spanning
  multiple updates plus stale-output suppression.
- Once an `Effect` has become a `BoxStream`, the information needed to
  re-create the original async operation is gone. This constraint dictates the
  shape of the retry API (constructor, not modifier).

### 1.2 Non-Negotiables

1. **Effect-local boundary.** No new field on `RuntimeDirectives`. No new
   `Action` variant. No changes to the runtime's keyed state machine.
2. **Leaf identity is preserved.** `timeout` wraps leaves individually,
   preserving leaf count and order. It must not close the door on future
   per-leaf cancellation metadata beyond RFC 0003's top-level keyed task
   model.
3. **Per-child batch semantics hold.**
   `Command::batch([a.timeout(1s), b.timeout(2s)])` times out per child,
   independently.
4. **Public constructors do not panic.** Following the crate's
   `panic = "warn"` policy, invalid input is expressed via `Option` /
   builders.
5. **The prelude stays minimal.** Retry support types are re-exported from the
   crate root only, not from `src/prelude.rs` (removing a name from the
   prelude is a breaking change; adding one later is not).
6. **No new dependencies.** Implemented with the existing `tokio` / `futures`
   / `tokio-stream` dependencies.

### 1.3 Goals and Non-Goals

#### Goals

- Provide an overall deadline for a single effect (`timeout`) and
  re-execution on failure (`retry`) as additive public API.
- Fix the composition laws with `map` / `batch` / `without_redraw` as
  contracts.
- Pin every contract with deterministic tests on Tokio paused time.
- Define up front the integration contracts that must still hold after
  RFC 0003 is implemented.

#### Non-Goals

- Implementing RFC 0003 command cancellation.
- Debounce / throttle.
- Clock DI (generalizing deterministic effect testing is a separate item).
- Exponential or jittered backoff in the initial release.
- A retroactive `.retry(...)` modifier on an arbitrary `Command<Msg>`.
- Re-subscription retry for `Command::stream`.
- Runtime-level observability metrics for timeout / retry.

## 2. Decision

- `timeout` is a **modifier** that wraps each leaf stream of the `Effect` at
  the `.timeout()` call. The deadline's `Sleep` is created on the leaf
  stream's first poll, not at wrap time. The timeout message is emitted at
  most once per `.timeout()` call.
- `retry` is a **constructor** that takes a repeatable future factory and
  rides on `Command::future`. `Effect` needs no changes, and `map` / `batch` /
  `without_redraw` compose as they do for any existing command. After RFC 0003,
  the constructor carries the default cancellation metadata.
- Implementation order is `timeout` first (it touches `effect.rs`), `retry`
  second (`Command::future(async move { ... })` suffices). The asymmetry is
  deliberate.

## 3. Public API Details

### 3.1 `Command::timeout`

```rust
impl<Msg: Send + 'static> Command<Msg> {
    pub fn timeout(
        self,
        duration: std::time::Duration,
        on_timeout: impl FnOnce() -> Msg + Send + 'static,
    ) -> Self;
}
```

Usage:

```rust
Command::perform(fetch(query.clone()), Msg::SearchLoaded)
    .timeout(Duration::from_secs(5), || Msg::SearchTimedOut);
```

Rationale for `on_timeout: FnOnce`:

- It matches the mental model of `tokio::time::timeout`: one `.timeout()`
  call yields at most one timeout message.
- Like the `Command::perform` mapper, it accepts closures that consume
  move-only values.
- The public bound does not require `Sync` (same reasoning as
  `Command::map`).

A type-changing API (`timeout_result(duration) -> Command<Result<Msg,
CommandTimeout>>` or similar) is not part of the initial implementation (§8).

### 3.2 Retry support types

```rust
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetryPolicy {
    max_attempts: NonZeroUsize,
    backoff: RetryBackoff,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RetryBackoff {
    None,
    #[non_exhaustive]
    Fixed {
        delay: Duration,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct RetryContext {
    attempt: NonZeroUsize,
}

#[derive(Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct RetryError<E> {
    attempts: NonZeroUsize,
    last_error: E,
    reason: RetryStopReason,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RetryStopReason {
    Exhausted,
    StoppedByPredicate,
}
```

Constructors and accessors. The return-self builders carry message-bearing
`#[must_use]`, mirroring `Command::without_redraw`; everything else clippy's
`must_use_candidate` would flag carries a plain `#[must_use]`:

```rust
impl RetryPolicy {
    #[must_use]
    pub const fn new(max_attempts: NonZeroUsize) -> Self;
    #[must_use]
    pub const fn try_new(max_attempts: usize) -> Option<Self>;
    #[must_use]
    pub const fn max_attempts(&self) -> NonZeroUsize;
    #[must_use]
    pub const fn backoff(&self) -> &RetryBackoff;
    #[must_use = "with_backoff returns a modified policy and does not mutate in place"]
    pub const fn with_backoff(self, backoff: RetryBackoff) -> Self;
    #[must_use = "with_fixed_backoff returns a modified policy and does not mutate in place"]
    pub const fn with_fixed_backoff(self, delay: Duration) -> Self;
}

impl RetryBackoff {
    #[must_use]
    pub const fn none() -> Self;
    #[must_use]
    pub const fn fixed(delay: Duration) -> Self;
}

impl RetryContext {
    pub(crate) const fn new(attempt: NonZeroUsize) -> Self;
    #[must_use]
    pub const fn attempt(&self) -> NonZeroUsize;
}

impl<E> RetryError<E> {
    #[must_use]
    pub const fn attempts(&self) -> NonZeroUsize;
    #[must_use]
    pub const fn last_error(&self) -> &E;
    #[must_use]
    pub const fn reason(&self) -> RetryStopReason;
    #[must_use]
    pub fn into_last_error(self) -> E;
}

impl<E: std::fmt::Display> std::fmt::Display for RetryError<E>;
impl<E: std::error::Error + 'static> std::error::Error for RetryError<E>;
```

Design decisions:

- **`RetryPolicy::new` is an infallible API taking `NonZeroUsize`.** `usize`
  input, which may be zero, goes through `try_new -> Option`. This matches
  `Timer::try_new` (`Option`), which likewise has exactly one invalidity
  reason. `FrameRate::try_new` returns `Result` because it has two failure
  reasons (`Zero` / `TooHigh`), so its situation differs. A `new(usize)` that
  panics on zero would violate the `panic = "warn"` policy and is not used.
- **The default backoff is `RetryBackoff::None`.**
- **`#[non_exhaustive]` strategy.** Both the enum itself and the variants with
  fields carry `#[non_exhaustive]`, making future variant additions
  (`Exponential` / `Jittered`) and field additions non-breaking. Enum variant
  fields share the enum's visibility, so variant-level `#[non_exhaustive]`
  achieves exactly: matching stays possible via `Fixed { delay, .. }`, while
  construction is funneled through `RetryBackoff::fixed(...)` /
  `RetryPolicy::with_*_backoff(...)`. The rustdoc must state that downstream
  `match` expressions need a wildcard arm.
- **Const-ness constrains drop glue.** Since `RetryPolicy::with_backoff` and
  friends ship as `const fn`, future variant fields must stay free of drop
  glue. Adding a field that needs `Drop` (a `Box`, a randomness source, etc.)
  cannot keep const-ness and would require a separate API. This is one reason
  exponential backoff is deferred: fixing the factor type (integer /
  finite-guaranteeing newtype / `f64`) under this constraint is premature.
- **Derived traits constrain future fields.** `RetryBackoff` ships with
  derived `Clone, Debug, Eq, PartialEq`, and removing a derive later is
  breaking. Future variant fields must therefore preserve all four (plus the
  no-drop-glue constraint above). In particular, a bare `f64` multiplier can
  never be stored (`f64` is not `Eq`): if a future constructor accepts `f64`,
  it must convert it into an `Eq`-compatible finite representation for
  storage, such as an integer-scaled multiplier or a finite-guaranteeing
  newtype with a manual `Eq` impl.
- **`RetryStopReason` exists.** A single-variant `RetryError::Exhausted` is
  not enough: when the `retry_if` predicate returns false, attempts have not
  been used up, so that case is represented separately as
  `StoppedByPredicate`.

### 3.3 `Command::retry` / `Command::retry_if`

```rust
impl<Msg: Send + 'static> Command<Msg> {
    pub fn retry<A, E, Fut, Op, F>(
        policy: RetryPolicy,
        operation: Op,
        f: F,
    ) -> Self
    where
        A: Send + 'static,
        E: Send + 'static,
        Fut: Future<Output = Result<A, E>> + Send + 'static,
        Op: FnMut(RetryContext) -> Fut + Send + 'static,
        F: FnOnce(Result<A, RetryError<E>>) -> Msg + Send + 'static;

    pub fn retry_if<A, E, Fut, Op, P, F>(
        policy: RetryPolicy,
        operation: Op,
        should_retry: P,
        f: F,
    ) -> Self
    where
        A: Send + 'static,
        E: Send + 'static,
        Fut: Future<Output = Result<A, E>> + Send + 'static,
        Op: FnMut(RetryContext) -> Fut + Send + 'static,
        P: FnMut(&E, RetryContext) -> bool + Send + 'static,
        F: FnOnce(Result<A, RetryError<E>>) -> Msg + Send + 'static;
}
```

- `retry` is the convenience wrapper that retries every error. `retry_if` is
  the lower-level API that can stop based on the error kind or the attempt.
- `should_retry` is `FnMut`. Like `operation`, it is called sequentially, so
  the wider bound costs nothing and lets the predicate keep small local state
  (e.g. a consecutive-failure counter) inside the closure.
- The public rustdoc for both constructors must warn that `operation` can run
  up to `policy.max_attempts()` times. Callers are responsible for ensuring
  that repeating the operation is safe: non-idempotent external side effects
  may occur more than once, including when an earlier attempt performs the
  side effect and subsequently returns an error.

Usage:

```rust
use std::num::NonZeroUsize;
use std::time::Duration;
use tears::{Command, RetryError, RetryPolicy};

struct Todo;
struct FetchError;

enum Msg {
    TodoLoaded(Result<Todo, RetryError<FetchError>>),
}

async fn fetch_todo(_todo_id: u64, _attempt: NonZeroUsize) -> Result<Todo, FetchError> {
    Ok(Todo)
}

let policy = RetryPolicy::try_new(3)
    .expect("max attempts must be non-zero")
    .with_fixed_backoff(Duration::from_millis(200));

let todo_id = 42;
let command: Command<Msg> = Command::retry(
    policy,
    move |ctx| fetch_todo(todo_id, ctx.attempt()),
    Msg::TodoLoaded,
);
```

### 3.4 Naming decisions

The following are deliberate decisions that become breaking to change once
shipped.

1. **`Command::retry` permanently claims the constructor name.** In Rust,
   items with the same name across inherent impls collide (E0592, duplicate
   definitions), so this choice closes the door on ever adding an inherent
   modifier method `retry(...)` to `impl<A, E> Command<Result<A, E>>`. A true
   retry modifier on arbitrary commands would require a major redesign in
   which `Effect` leaves hold repeatable operation factories — speculative —
   so the short, natural constructor name wins. If a modifier is ever needed,
   it takes another name such as `with_retry(...)` / `retry_with(...)` (the
   inherent door closes, but an extension-trait method could technically
   coexist). Retry semantics for `Command::stream` would also need their own
   definition at that point.
2. **Argument order is `policy, operation, f`.** This is the first API to
   break the crate's own convention of "the effectful payload comes first"
   (`perform(future, f)`, `run(stream, f)`), and the break is accepted
   deliberately: `retry` can take two closures, and leading with the small
   configuration value reads better. The mapper stays last, as in the existing
   APIs, and the rustdoc spells out the reading order: configuration → what to
   run → how to turn it into a message.
3. **Retry support types are imported explicitly from the root**
   (`tears::RetryPolicy`). They are not added to the prelude initially
   (§1.2, item 5).

## 4. Semantics

### 4.1 timeout

- **An overall deadline, not an inter-item inactivity timeout.**
  (`tokio_stream::StreamExt::timeout` is a per-item timeout and serves a
  different purpose.)
- The deadline applies **per leaf** of the target command. It starts at the
  leaf stream's first poll (when the runtime starts executing the effect),
  not at the `.timeout()` call.
- **The timeout message is emitted at most once per `.timeout()` call.** Even
  when the target has several leaves, as in
  `Command::batch([a, b]).timeout(...)`, only the first leaf to reach its
  deadline consumes `on_timeout` and emits the timeout message. The other
  leaves terminate at their own deadlines without a message.
- A leaf that completes before its deadline does not consume `on_timeout`.
  This holds even when completion and the elapsed deadline are observed in the
  same poll: inner termination wins over the wrapper's own elapsed deadline,
  and the leaf closes without a timeout message (§9.2). If `a` completes early
  and `b` is still pending inside a batch, `b` can still emit the timeout
  message at its deadline.
- An `Action::Message(msg)` before the deadline is delivered normally and the
  leaf continues.
- An `Action::Quit` before the deadline is delivered as `Quit` and
  **terminates that leaf**. This is deliberate non-transparency: the timeout
  wrapper treats `Quit` as a terminal action. Termination via `Quit` does not
  consume `on_timeout`. At the runtime boundary, however, `Action::Quit`
  stops polling the whole command stream, so later sibling output under the
  same `.timeout()` is not delivered by the runtime.
- When a deadline fires, the inner stream/future is dropped and never polled
  again. As in RFC 0003, stopping the polling does not roll back external
  side effects that already happened.
- `Command::none().timeout(...)` is inert — there is no stream, so a timeout
  message is never emitted. Runtime directives such as `without_redraw` are
  preserved unchanged.
- `map` / `batch` naturally preserve a timeout that is wrapped into the leaf
  streams. `timeout` wraps only the `effect` and never drops `directives`.
- Applying `.timeout(d1, f1).timeout(d2, f2)` twice: the two wrappers are
  independent at-most-once contracts. For a single leaf, they do not emit both
  messages: with strictly ordered deadlines, only the message of the wrapper
  whose deadline fires first is emitted, then the leaf closes; with
  simultaneous deadlines (equal durations, or nested `Duration::ZERO`),
  exactly one of `f1` / `f2` is emitted, unspecified which. For a batched
  command, this cross-wrapper exclusion is not a whole-command guarantee:
  different leaves may still emit messages from different `.timeout()` calls,
  for example one leaf emitting the outer timeout and another later emitting
  the inner timeout. Each individual `.timeout()` call still emits at most
  once.
- The precise ordering when a deadline and a stream item become ready
  simultaneously is **not part of the contract**.
- On a `Command::stream`, messages flow until the deadline, then the timeout
  message is emitted and the stream closes. An infinite stream is always
  terminated at the deadline, so the primary use cases are `future` /
  `perform`.

### 4.2 retry

- `max_attempts` includes the first execution.
- If retry processing completes without cancellation or panic, the final
  message is emitted exactly once: `Ok(A)` on success, otherwise
  `Err(RetryError { attempts, last_error, reason })`, both converted to a
  message by `f`.
- On `Err(E)` with attempts remaining and `should_retry(&error, ctx)` true,
  the operation re-runs after the backoff.
- **An `Err(E)` on the final attempt takes `RetryStopReason::Exhausted`
  priority.** `should_retry` is not called on that attempt, so input for
  which the predicate would return false still yields `Exhausted`, never
  `StoppedByPredicate`. `StoppedByPredicate` occurs only when the predicate
  returns false while attempts remain.
- `RetryBackoff::None` proceeds to the next attempt without sleeping (no
  virtual-time advance is needed). `RetryBackoff::Fixed { delay }` waits the
  same `delay` after every failure, before the next attempt.
- `RetryContext::attempt` is 1-based. If a backoff computation needs a 0-based
  exponent index, that conversion stays inside the helper and the contract is
  pinned by tests.
- If the task is aborted before retry processing completes (runtime shutdown,
  or future cancellation), no final message is produced.
- For a per-attempt timeout, use `tokio::time::timeout(d, fetch()).await`
  inside `operation` and map the timeout into an `Err`; that makes it subject
  to retry. An outer `.timeout(...)` acts as a deadline over the whole retry.
- No jitter in the initial implementation; deterministic tests and
  reproducibility take priority.

## 5. Invariants (contract tests)

Each item is pinned by a test. Timeout (T), retry (R).

- T1. `timeout` wraps only the `effect` and preserves `directives` (and,
  later, cancellation metadata).
- T2. The deadline starts at the leaf stream's first poll, not at the
  `.timeout()` call.
- T3. The timeout message is at-most-once per `.timeout()` call: even if
  several leaves reach their deadlines, at most one message is emitted.
- T4. A leaf that completes before its deadline does not consume
  `on_timeout`; inner termination observed together with an elapsed deadline
  still wins (no timeout message).
- T5. Pre-deadline `Message`s pass through; `Quit` is delivered and closes the
  leaf without consuming `on_timeout`. Once delivered to the runtime, `Quit`
  stops polling the whole command stream, so later sibling output is not
  runtime-delivered.
- T6. When the timeout fires, the inner stream is dropped and never polled
  again (no rollback is promised).
- T7. Timeout on a stream-less command is inert and keeps `is_none()`.
- T8. Composition with `map` / `batch` preserves leaf count, order, and
  timeout behavior; `batch([a.timeout(..), b.timeout(..)])` times out per
  child, independently.
- T9. Double `.timeout()` on a single leaf: strictly ordered deadlines emit
  only the earlier wrapper's message; simultaneous deadlines emit exactly one
  of the two messages, unspecified which. Never both for that leaf. Batched
  double timeouts do not have a cross-wrapper mutual-exclusion guarantee
  across different leaves.
- T10. Simultaneous readiness ordering between the deadline and a stream
  *item* is out of contract. Tests must not depend on it (`Duration::ZERO`
  must not panic; for a single-leaf double timeout, only assert that one of
  the messages is emitted). Inner *termination*, by contrast,
  deterministically wins over an elapsed deadline (T4, T9).
- R1. `max_attempts` includes the first attempt; if retry processing completes
  without cancellation or panic, the final message is emitted exactly once.
- R2. Failure on the final attempt yields `Exhausted` and does not call
  `should_retry` on that attempt.
- R3. `StoppedByPredicate` occurs only while attempts remain.
- R4. `RetryBackoff::None` advances no time before the next attempt; `Fixed`
  waits the same delay after each failure.
- R5. `RetryError` keeps `last_error` and exposes it through `Display` /
  `Error::source()`.
- R6. `RetryPolicy::try_new(0)` is `None`. `new` defaults the backoff to
  `RetryBackoff::None`. `with_fixed_backoff` preserves `max_attempts`.
- R7. The retry constructors go through `Command::future`, so `map` /
  `without_redraw` / `batch` behave as they do for any existing command.
- R8. `should_retry` can hold state as an `FnMut`; `on_timeout` can consume
  move-only values as an `FnOnce` (compile-time contracts).

## 6. Testing Strategy

Time-dependent tests are made deterministic with
`#[tokio::test(start_paused = true)]` and `tokio::time::advance` (`tokio`'s
`test-util` is already a dev-dependency). Clock DI is future work; the tests in
this RFC are fixed on Tokio paused time.

- **Timeout unit tests** (`src/command.rs` / `src/command/effect.rs`): pin
  T1–T10 directly. Representative cases: deadline firing on a pending future,
  completion before the deadline, multiple messages passing through on a
  `Command::stream`, `map` on both sides (`timeout.map` / `map.timeout`),
  factory-consumption rules under batch × timeout (completed leaves and
  `Quit` leaves do not consume), single-leaf double timeout, batched double
  timeout without cross-wrapper mutual exclusion, `Duration::ZERO`.
- **Retry unit tests** (`src/command/retry.rs`): R1–R8. Success on the first
  attempt / success on the second (one backoff), exhaustion, predicate stop,
  `max_attempts = 1`, policy validation, backoff computation.
- **Integration smoke tests**: the timeout message and the retry final
  message (success / exhausted) reach the runtime's `update`. That pending
  timeouts / backoffs do not detach at shutdown rides on the existing
  `JoinSet` abort behavior (add a `Notify`-based smoke test if needed).

## 7. Interaction with RFC 0003 (command cancellation)

RFC 0003 proceeds as a separate implementation, but the integration contracts
are fixed now.

- `timeout` wraps only the `effect` and preserves `directives` and
  cancellation metadata (an extension of T1).
- The retry constructors go through `Command::future` and therefore carry the
  default cancellation metadata.
- `.timeout(...).cancellable(id)` and `.cancellable(id).timeout(...)` both
  preserve the key.
- When a keyed command is cancelled or superseded, the timeout message and the
  retry final message are not delivered, by virtue of the private-receiver
  drop.
- `Command::cancel(id).timeout(...)` is stream-less, so the timeout is inert;
  the explicit cancel remains.

Required RFC 0003 runtime coverage, outside the 0.9.3 implementation scope of
this RFC:

- Cancelling a keyed command before the timeout deadline suppresses the
  timeout message.
- Superseding a keyed command during a retry backoff suppresses the old retry
  final message.
- While a retrying command holds a key under `KeepInFlight`, a new retrying
  command is not spawned.

## 8. Alternatives Considered

### Retry as a modifier (`Command::future(fetch()).retry(policy)`)

Rejected. `Effect::Leaves` holds only `BoxStream`s, and a future cannot be
re-used once polled, so a retroactive retry on an existing command cannot be a
real re-execution. `Msg` has no error channel, so retry classification is also
impossible. A constructor taking a repeatable factory is the only honest shape
under the current `Effect` representation.

### Fold first, then wrap a single timeout

Rejected. Folding the leaves via `select_all` at `.timeout()` time and
wrapping once is simpler, but it collapses leaf identity and closes the door
on future per-leaf cancellation metadata beyond RFC 0003's top-level keyed
task model (violates §1.2, item 2). Per-leaf wrapping with a shared factory
keeps the per-call at-most-once message behavior while preserving the leaf
structure.

### Emit a timeout message per leaf

Rejected. Multiple timeout messages out of one `.timeout()` call contradicts
the "I attached one deadline" mental model. When per-child messages are
wanted, attach `.timeout(...)` to each child command; that composes cleanly
with the per-call at-most-once contract.

### A `Quit`-transparent timeout wrapper

Rejected. Continuing to poll the inner stream after `Quit` would be "fully
transparent to the bare stream", but `Quit` is a terminal request to the
runtime and later output has no meaning. The wrapper closing the leaf is made
an explicit contract instead.

### Type-changing timeout (`timeout_result`)

Deferred. An API returning `Command<Result<Msg, CommandTimeout>>` can be added
orthogonally to the message-preserving API, so it is left out of the initial
implementation (§10).

### `RetryError` as a single variant / without a reason

Rejected. A predicate stop has not used up the attempts; if it were
indistinguishable from `Exhausted`, callers could make wrong re-try decisions.

### `RetryPolicy::new(usize)` panicking on zero

Rejected. It violates the crate's `panic = "warn"` policy. Split into the
infallible `new(NonZeroUsize)` and `try_new(usize) -> Option` (the same shape
as `Timer::try_new`).

### Exponential backoff in the initial release

Deferred. Fixing the factor to `NonZeroU32` would make fractional multipliers
like 1.5 hard to add later. `#[non_exhaustive]` makes adding the variant
non-breaking, so the factor-type decision (integer / finite-guaranteeing
newtype / `f64` constructor) is made when it is actually needed. If added, the
`Duration` computation must saturate, and whatever the constructor accepts,
the stored field must satisfy the derived-trait and drop-glue constraints of
§3.2: `f64` may appear as constructor input only, converted to an
`Eq`-compatible finite representation for storage.

### Re-exporting the retry types from the prelude

Deferred. The prelude is a glob-import surface, and removing a name later is a
breaking change. The current prelude is deliberately small (it does not even
include `Timer`), and `RetryPolicy` / `RetryError` are collision-prone common
names in the ecosystem. Add later based on real-world usage of the root
re-exports, if warranted.

### Using `tokio_stream::StreamExt::timeout`

Rejected. It is a per-item (inactivity) timeout, which differs from this RFC's
overall-deadline semantics.

## 9. Implementation Plan

### 9.1 Order

1. `Effect::timeout` / `Command::timeout` + unit tests + rustdoc.
2. Retry support types in `src/command/retry.rs` + validation tests.
   Re-exports from `src/command.rs` / `src/lib.rs` (not the prelude).
3. `Command::retry` / `retry_if` + the `run_retry` helper + paused-time tests.
4. Runtime integration smoke tests.
5. CHANGELOG / README / rustdoc.

Timeout comes first because it touches `effect.rs`; retry rides on
`Command::future` and needs no `Effect` changes (§2).

### 9.2 Timeout sketch

The sketch below fixes the mechanisms that the contracts depend on (T2, T3,
T4, T5). Anything not shown — state-struct layout, single-leaf fast paths,
exact combinator choice — is up to the implementer.

- `Effect::timeout(duration, on_timeout)` returns `Effect::None` unchanged and
  wraps each leaf of `Effect::Leaves` in a `timeout_leaf` combinator.
- `on_timeout` is shared across the leaves as `Arc<Mutex<Option<F>>>`; the
  first leaf to reach its deadline `take()`s the `FnOnce` and emits the
  message. This realizes the per-call at-most-once contract (T3). The mutex
  only lends `Sync` so the public bound stays `FnOnce + Send`, and lock
  recovery follows `Effect::map`: a poisoned lock is recovered via
  `PoisonError::into_inner` rather than cascading a sibling leaf's panic.
- `timeout_leaf` is a stateful stream whose state holds the inner stream and a
  lazily created `Sleep`. The `Sleep` is created on the leaf's first poll
  (T2), then raced against `inner.next()`:

```text
timeout_leaf(inner, duration, shared_on_timeout):
  state = { inner: Some(inner), sleep: None }

  loop per poll:
    if state.inner is None: end of stream
    if state.sleep is None: state.sleep = sleep(duration)   # first poll (T2)

    race inner.next() against sleep:
      inner yields None                  => end of stream (T4: factory untouched)
      inner yields Action::Message(msg)  => emit Message(msg), continue
      inner yields Action::Quit          => drop inner, emit Quit
                                            (T5: factory untouched)
      sleep fires                        => drop inner,
                                            take shared_on_timeout:
                                              Some(f) => emit Message(f()) (T3)
                                              None    => end of stream
```

- Every terminal transition (inner completion, `Quit`, deadline) drops the
  inner stream, after which the leaf yields nothing (T6).
- Two disambiguation rules are part of the mechanism, not implementer
  freedom, because contracts depend on them:
  1. **Termination wins.** When `inner.next()` yields `None`, the wrapper
     closes without emitting its timeout message, even if its own deadline
     has also elapsed (T4). A plain unbiased `select!` does not guarantee
     this: after the inner wrapper of a double timeout emits its message and
     terminates on a single leaf, an unbiased race between the observed
     `None` and the outer wrapper's already-elapsed `Sleep` could emit the
     second timeout message and violate T9. Check inner termination before
     honoring the deadline.
  2. **The deadline must not starve.** A continuously ready inner stream must
     not keep an elapsed deadline from being honored, or an infinite stream
     would never terminate at its deadline (§4.1). How ties between a ready
     *item* and an elapsed deadline are broken is otherwise unspecified
     (T10).

### 9.3 Retry sketch

`Command::retry` / `retry_if` do not construct an `Effect` directly; they ride
on `Command::future` (R7):

```rust
Command::future(async move {
    f(run_retry(policy, operation, should_retry).await)
})
```

```text
run_retry(policy, operation, should_retry):
  attempt = 1
  loop:
    ctx = RetryContext::new(attempt)
    match operation(ctx).await:
      Ok(value) =>
        return Ok(value)
      Err(error) =>
        if attempt == policy.max_attempts:          # R2: predicate not called
          return Err(RetryError { attempts: attempt, last_error: error,
                                  reason: Exhausted })
        if !should_retry(&error, ctx):              # R3: attempts remain
          return Err(RetryError { attempts: attempt, last_error: error,
                                  reason: StoppedByPredicate })
        sleep per policy.backoff                    # R4
        attempt += 1
```

`RetryContext::new` stays `pub(crate)` so only `run_retry` can construct
contexts.

## 10. Future Work

- A type-changing timeout API (`timeout_result` or similar).
- `RetryBackoff::Exponential` / jitter (factor-type decision and saturating
  arithmetic; stored fields must keep the derived `Clone + Debug + Eq +
  PartialEq` and stay free of drop glue, so `f64` may appear as constructor
  input only — see §3.2).
- A retry modifier on arbitrary commands (`with_retry` / `retry_with`),
  contingent on `Effect` leaves holding repeatable operation factories.
- Clock DI to generalize deterministic effect testing.
- Debounce / throttle (as extensions of RFC 0003's keyed lifecycle).

## References

- `docs/rfcs/0002-redraw-suppression.md` — the two modifier axes (output
  treatment / execution lifecycle).
- `docs/rfcs/0003-command-cancellation.md` — keyed lifecycle, private
  receivers, the counterpart of the integration contracts.
- `src/command.rs`, `src/command/effect.rs` — the current `Command` / `Effect`
  representation.
- `src/subscription/time.rs` (`Timer::try_new`),
  `src/runtime/frame_rate.rs` (`FrameRate::try_new`) — precedents for the
  validation APIs.
