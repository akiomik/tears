# RFC 0004: Command Timeout / Retry

- Status: Accepted
- Target: 0.9.3 (the next non-breaking patch at acceptance), additive public
  API only
- Scope: timeout / retry lifecycle control confined to a single effect's
  execution; no runtime changes
- Feature flag: none
- CHANGELOG: `Added` (`Command::timeout`, `Command::retry`,
  `Command::retry_if`, `RetryPolicy`, `RetryBackoff`, `RetryContext`,
  `RetryError`, `RetryStopReason`)

> **Normative boundary.** The constraints in §1.3, the public API and observable
> behavior in §§2–4 and Appendix A, and the "Contract" columns in Appendix B
> are normative. Rationale and examples explain those contracts. Appendix C is
> a non-normative implementation guide; where it cites a T/R contract, the
> contract controls.

## Summary

Long-running commands need two related but distinct lifecycle controls:

- an overall deadline when an effect does not finish in time; and
- re-execution when a repeatable operation fails transiently.

This RFC adds both without changing the runtime:

| Feature | Public API shape | Execution scope | Terminal output |
| --- | --- | --- | --- |
| Timeout | `Command::timeout` modifier | Each effect leaf | At most one timeout message per `.timeout()` call |
| Retry | `Command::retry` / `retry_if` constructors | One repeatable operation | On completion, one final success or error message |

Both controls are **effect-local**: they operate within one command execution
and require no keyed state across updates. Keyed cancellation remains the
runtime-level concern defined by
[RFC 0003](./0003-command-cancellation.md).

## 1. Motivation, Model, and Scope

### 1.1 Motivation

A command that waits on I/O may remain pending indefinitely. The caller needs
to turn that condition into an application message without changing the
command's message type. Separately, an operation that can be safely repeated
may need to retry a transient error and report only its final result.

These cases need explicit contracts for composition, shutdown, and edge
conditions. In particular:

- a batched timeout needs a defined message count;
- a retry needs a fresh future for every attempt;
- `Quit` and cancellation need defined terminal behavior; and
- time-dependent behavior needs deterministic contract tests.

#### Why this requires an RFC

Both features were originally tagged "no RFC needed — they add no invariants"
in the project backlog. Design work exposed two kinds of decisions that would
be difficult to reverse after release:

1. public contracts such as the per-call at-most-once guarantee, permanently
   claiming the `retry` name, and treating `Quit` as terminal; and
2. integration contracts with
   [RFC 0003](./0003-command-cancellation.md).

That evidence reversed the original no-RFC decision. The original boundary
assessment remains unchanged: timeout and retry are effect-local. Section 1.3
keeps that boundary as a normative constraint.

### 1.2 Model and terms

`Command<Msg>` contains an effect and runtime directives:

```rust
pub struct Command<Msg: Send + 'static> {
    effect: Effect<Msg>,
    directives: RuntimeDirectives,
}
```

`Effect<Msg>` is either `None` or a flat sequence of leaf streams. The leaves
are folded with `select_all` only at the `into_stream()` boundary.

This RFC uses the following terms:

| Term | Meaning |
| --- | --- |
| Effect leaf | One independent stream stored in `Effect::Leaves` before folding |
| Target command | The command consumed by a `.timeout()` call |
| Per call | One contract shared by all leaves wrapped by that invocation |
| Effect-local | State that exists only for one effect execution, not across updates |

The relevant concerns remain in separate layers:

| Layer | Examples | Representation |
| --- | --- | --- |
| Output treatment | `without_redraw` | Directive over the whole update result |
| Effect-local lifecycle | `timeout`, `retry` | Leaf wrapper or repeatable operation factory |
| Runtime lifecycle | `cancellable` | Keyed runtime metadata from RFC 0003 |

RFC 0003 keeps cancellation metadata at the top-level keyed task boundary.
Child keys inside `Command::batch` are intentionally not preserved.

Once an effect has become a `BoxStream`, the original async operation can no
longer be re-created. Timeout can therefore wrap an existing command, while
retry must accept a repeatable operation factory.

### 1.3 Constraints

1. **Effect-local boundary.** There is no new `RuntimeDirectives` field,
   `Action` variant, or runtime keyed state.
2. **Leaf identity.** Timeout wraps leaves individually and preserves their
   count and order. It must not preclude future per-leaf cancellation metadata
   beyond RFC 0003's top-level keyed task model.
3. **Per-child batch semantics.**
   `Command::batch([a.timeout(1s), b.timeout(2s)])` times out each child
   independently.
4. **Non-panicking constructors.** Invalid public input uses `Option` or
   builders, following the crate's `panic = "warn"` policy.
5. **Minimal prelude.** Retry support types are exported from the crate root,
   not from `src/prelude.rs`.
6. **No new dependencies.** The implementation uses the existing `tokio`,
   `futures`, and `tokio-stream` dependencies.
7. **Deterministic contract verification.** Every T/R contract must be pinned
   by a deterministic test. Time-dependent tests use Tokio's paused clock.

### 1.4 Out of scope

This RFC does not implement keyed cancellation, debounce, throttle, clock
dependency injection, or runtime observability metrics. It also does not add
stream re-subscription retry. Deferred API extensions are collected in §5.2.

## 2. Timeout

### 2.1 Public API and usage

```rust
impl<Msg: Send + 'static> Command<Msg> {
    pub fn timeout(
        self,
        duration: std::time::Duration,
        on_timeout: impl FnOnce() -> Msg + Send + 'static,
    ) -> Self;
}
```

`timeout` adds an overall deadline to the target command:

```rust
Command::perform(fetch(query.clone()), Msg::SearchLoaded)
    .timeout(Duration::from_secs(5), || Msg::SearchTimedOut);
```

This is an overall deadline, not the per-item inactivity timeout provided by
`tokio_stream::StreamExt::timeout`.

### 2.2 Core semantics

- Each leaf receives its own deadline. The deadline starts when that leaf is
  first polled, not when `.timeout()` is called.
- A `.timeout()` call emits **at most one timeout message**, even if it wraps
  several leaves. The first leaf that takes its deadline path consumes
  `on_timeout`. Other leaves still terminate at their own deadlines but emit no
  timeout message.
- A leaf that completes before its deadline does not consume `on_timeout`.
  Inner termination wins when completion and an elapsed deadline are observed
  in the same poll. Another pending leaf may still consume `on_timeout` later.
- `Action::Message(msg)` observed before the deadline is delivered normally,
  and the leaf continues.
- `Action::Quit` observed before the deadline is delivered and terminates that
  leaf without consuming `on_timeout`. At the runtime boundary, `Quit` stops
  polling the entire command stream, so later sibling output is not delivered.
  If `Quit` and an elapsed deadline are ready in the same poll, they follow the
  same unspecified tie ordering as any other item: either `Quit` is delivered
  and closes the leaf, or the deadline wins and `Quit` is not delivered.
- When the wrapper takes the deadline path, the inner stream or future is
  dropped and is never polled again. Already completed external side effects
  are not rolled back.
- A continuously ready stream cannot starve an elapsed deadline. A wrapper poll
  in which the deadline is ready may pass through at most the simultaneously
  ready inner item. If it does, no later inner item is delivered, and the
  wrapper must take a terminal transition no later than its next poll. That
  transition either observes inner termination or takes the deadline path and
  may emit the timeout message; after any such terminal output, subsequent
  polls return `None`. The exact ordering between the ready inner item and the
  deadline is otherwise outside the contract.

### 2.3 Placement and composition

Where timeout is attached determines how timeout messages are shared:

| Shape | Deadline behavior | Timeout messages |
| --- | --- | --- |
| `single_leaf.timeout(d, f)` | One deadline | At most one |
| `batch([a, b]).timeout(d, f)` | One deadline per leaf | At most one across the call |
| `batch([a.timeout(d1, f1), b.timeout(d2, f2)])` | Each child is independent | At most one per child call |
| `Command::none().timeout(d, f)` | No leaf, so no deadline | None |

`Command::none().timeout(...)` remains stream-less and preserves runtime
directives. `map` and `batch` preserve a timeout already wrapped into the leaf
streams; `timeout` itself changes only the effect and preserves directives.

On a `Command::stream`, messages flow before the deadline. A simultaneously
ready item may also win the deadline tie once under §2.2's bounded-progress
rule; the wrapper then takes a terminal transition, emitting the timeout
message if still available unless inner termination wins, and closes. This also
terminates an infinite stream, although `future` and `perform` are the primary
use cases.

### 2.4 Nested timeout edge cases

For `a.timeout(d1, f1).timeout(d2, f2)`, the two calls retain independent
at-most-once contracts:

- On one leaf with strictly ordered deadlines, only the earlier wrapper emits
  a message, then the leaf closes.
- With simultaneous deadlines, exactly one of `f1` or `f2` is emitted; which
  one is unspecified.
- Across a batch, mutual exclusion is only per leaf. Different leaves may emit
  messages belonging to different timeout calls.

The ordering of a deadline and a simultaneously ready **item** is unspecified.
Here, an item includes both `Action::Message` and `Action::Quit`. If `Quit`
wins, it is delivered and closes the wrapper without consuming `on_timeout`; if
the deadline wins, the inner stream is dropped and `Quit` is not delivered. If
a non-terminal message wins, the wrapper must take a terminal transition no
later than its next poll, as required by §2.2. Inner **termination** (`None`) is
different: it deterministically wins and closes the wrapper without a timeout
message.

### 2.5 API rationale

`on_timeout` is `FnOnce` because one `.timeout()` call can produce at most one
timeout message. The bound also permits closures that consume move-only values
and, like `Command::map`, does not expose a public `Sync` requirement.

Timeout is a modifier because an existing leaf stream can be wrapped without
reconstructing its source operation. Wrapping leaves individually preserves
leaf identity and per-child batch behavior.

## 3. Retry

### 3.1 Public API

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

`retry` retries every error while attempts remain. `retry_if` additionally
allows the caller to stop based on the error and current attempt.

### 3.2 Usage

```rust
use std::num::NonZeroUsize;
use std::time::Duration;
use tears::{Command, RetryError, RetryPolicy};

struct Todo;
struct FetchError;

enum Msg {
    TodoLoaded(Result<Todo, RetryError<FetchError>>),
}

async fn fetch_todo(
    _todo_id: u64,
    _attempt: NonZeroUsize,
) -> Result<Todo, FetchError> {
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

The public rustdoc must warn that the operation may run up to
`policy.max_attempts()` times. Callers are responsible for ensuring that
repetition is safe: non-idempotent external side effects may occur more than
once, including when an attempt performs the side effect and later returns an
error.

### 3.3 Support model

| Type | Purpose |
| --- | --- |
| `RetryPolicy` | Non-zero maximum attempt count plus backoff |
| `RetryBackoff` | No delay or one fixed delay between attempts |
| `RetryContext` | The current 1-based attempt number |
| `RetryError<E>` | Attempt count, last error, and stop reason |
| `RetryStopReason` | `Exhausted` or `StoppedByPredicate` |

`RetryPolicy::new` accepts `NonZeroUsize`. The convenience
`RetryPolicy::try_new(usize)` returns `None` for zero instead of panicking.
The default backoff is `RetryBackoff::None`. Appendix A contains the complete
support API and compatibility attributes.

### 3.4 Core semantics

- `max_attempts` includes the first execution, and
  `RetryContext::attempt()` is 1-based. If a future backoff calculation needs
  a 0-based exponent, that conversion remains inside the helper rather than
  changing the public context.
- If processing completes without cancellation or panic, `f` emits exactly one
  final message: `Ok(A)` on success or
  `Err(RetryError { attempts, last_error, reason })` on failure.
- On `Err(E)` with attempts remaining, `retry_if` calls
  `should_retry(&error, ctx)`. A true result continues after the configured
  backoff; false returns `StoppedByPredicate`.
- An error on the final attempt always returns `Exhausted`.
  `should_retry` is not called on that attempt.
- `RetryBackoff::None` starts the next attempt without sleeping.
  `RetryBackoff::Fixed { delay }` waits the same delay after each retriable
  failure and before the next attempt.
- If the task is aborted before processing completes, no final message is
  produced.

For a per-attempt deadline, the operation can use
`tokio::time::timeout(d, fetch()).await` and map the timeout to `Err`. An outer
`.timeout(...)` instead applies one overall deadline to the complete retry
process.

### 3.5 API rationale and naming

Retry is a constructor rather than a modifier because `Effect::Leaves` retains
only streams, not the factory needed to create a new future. Each attempt must
invoke the supplied operation again. In addition, an arbitrary `Command<Msg>`
has no error channel from which retry could classify failures.

`operation` and `should_retry` are `FnMut`. They run sequentially, so both may
keep small local state without requiring synchronization. The final mapper `f`
is `FnOnce` because it is invoked only for the final result.

The name `Command::retry` permanently occupies that inherent constructor name;
Rust does not permit a future inherent modifier with the same name. This
tradeoff is accepted because a modifier cannot represent real re-execution
under the current `Effect` model. If a future representation makes such a
modifier possible, it must use another inherent name such as `with_retry` or
`retry_with`. An extension-trait method could technically coexist even though
the inherent `retry` name is closed.

Arguments are ordered `policy, operation, f` (or
`policy, operation, should_retry, f`). This is the crate's first intentional
departure from the effectful-payload-first convention used by
`perform(future, f)` and `run(stream, f)`. Configuration comes first, the
repeatable operation second, and the message mapper remains last. Public
rustdoc must state that reading order explicitly: configuration → operation →
message conversion.

Retry support types are imported explicitly from the crate root and are not
added to the prelude. The prelude is deliberately small—it does not include
`Timer`—and `RetryPolicy` / `RetryError` are common, collision-prone ecosystem
names. Adding them later is non-breaking; removing them after inclusion would
be breaking.

## 4. Composition and Runtime Cancellation

### 4.1 Existing command operations

| Operation | Timeout | Retry |
| --- | --- | --- |
| `map` | Maps output while preserving wrapped leaves | Behaves like any `Command::future` result |
| `batch` | Preserves leaf count, order, and placement semantics | Batches like any other command |
| `without_redraw` | Directive is preserved | Directive is preserved |

A retry command is observably a single-leaf future command, so existing
composition behavior applies unchanged; building it on `Command::future`
(Appendix C) is one way to satisfy that contract. Timeout wraps only the
effect and never removes directives.

### 4.2 RFC 0003 integration

[RFC 0003](./0003-command-cancellation.md) is implemented separately. Once its
metadata exists, these forward-integration contracts apply:

- `timeout` preserves cancellation metadata as well as directives.
- `.timeout(...).cancellable(id)` and
  `.cancellable(id).timeout(...)` both preserve the key.
- Retry constructors carry the default cancellation metadata supplied by
  `Command::future`.
- Cancelling or superseding a keyed command suppresses both a pending timeout
  message and a retry final message through private-receiver drop.
- `Command::cancel(id).timeout(...)` remains stream-less, so timeout is inert
  and the explicit cancellation remains.

Verification of every contract in this subsection belongs to the RFC 0003
implementation and is outside the 0.9.3 implementation scope of this RFC. Its
runtime tests must additionally cover cancellation before a timeout,
supersession during retry backoff, and `KeepInFlight` preventing a second
retrying command from spawning under the same key.

## 5. Alternatives and Deferred Work

### 5.1 Alternatives not selected

| Alternative | Reason |
| --- | --- |
| Fold leaves before applying timeout | Collapses leaf identity and prevents per-leaf lifecycle metadata |
| Emit one timeout message per leaf | Conflicts with the one-call/one-message model; callers can attach timeout to each child instead |
| Keep polling a leaf after `Quit` | Produces output after a terminal runtime request |
| Use `tokio_stream::StreamExt::timeout` | Implements per-item inactivity, not an overall deadline |
| Omit `RetryStopReason` | Makes predicate stop indistinguishable from exhausted attempts |
| Accept `usize` and panic on zero | Violates the crate's non-panicking constructor policy |

### 5.2 Deferred extensions

| Extension | Reason for deferral |
| --- | --- |
| Type-changing `timeout_result` | Orthogonal to the message-preserving API and can be added later |
| Exponential or jittered backoff | Factor representation, saturating arithmetic, and randomness need a separate reproducible design |
| Retry modifier on arbitrary commands | Requires leaves to retain repeatable operation factories |
| `Command::stream` re-subscription retry | Requires separate stream retry semantics |
| Prelude re-exports | Can be added after observing usage; removal would be breaking |
| Clock dependency injection | General deterministic effect testing is a separate concern |
| Debounce / throttle | Belong to RFC 0003's keyed lifecycle model |

## Appendix A. Retry Support API (Normative)

### A.1 Types

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

### A.2 Constructors and accessors

Return-self builders carry message-bearing `#[must_use]` attributes, matching
`Command::without_redraw`. Other candidates identified by Clippy carry plain
`#[must_use]`.

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

### A.3 Compatibility consequences

- `RetryPolicy::new(NonZeroUsize)` is infallible, while
  `try_new(usize) -> Option<Self>` represents the single invalid case. This
  follows the `Timer::try_new` precedent; `FrameRate::try_new` uses `Result`
  because it distinguishes multiple invalid cases.
- `RetryBackoff` and its field-bearing variants are `#[non_exhaustive]`.
  Enum variant fields share the enum's visibility, so variant-level
  `#[non_exhaustive]` permits downstream matching with patterns such as
  `Fixed { delay, .. }` while construction goes through public constructors
  and builders. Enum-level `#[non_exhaustive]` leaves room for variants such as
  `Exponential` and `Jittered`. Rustdoc must state that downstream matches
  require a wildcard arm.
- The shipped `const fn` builders constrain future backoff fields to values
  without drop glue. A future representation requiring `Drop` needs a separate
  API.
- Removing the derived traits would be breaking. Future `RetryBackoff` fields
  must preserve `Clone + Debug + Eq + PartialEq`; future `RetryContext` fields
  must additionally preserve `Copy`. A bare `f64` therefore cannot be stored as
  an `Eq` field, although a constructor could convert it to an `Eq`-compatible
  finite representation.
- `RetryStopReason` distinguishes an exhausted policy from a predicate stop
  that occurs while attempts remain.

## Appendix B. Contract Checklist and Verification

As required by §1.3, every T/R contract below must be pinned by a deterministic
test. The "Contract" columns and that coverage requirement are normative;
exact test locations and representative cases are verification guidance.

### B.1 Timeout contracts

| ID | Contract |
| --- | --- |
| T1 | Timeout changes only the effect and preserves runtime directives. |
| T2 | Each deadline starts on the leaf's first poll, not at the `.timeout()` call. |
| T3 | One `.timeout()` call emits at most one timeout message across all wrapped leaves; its `FnOnce` closure may consume move-only state. |
| T4 | Completion before the deadline does not consume `on_timeout`; termination wins if both are observed together. |
| T5 | Pre-deadline messages pass through; a pre-deadline `Quit` is delivered, closes the leaf, and does not consume `on_timeout`. At runtime, a delivered `Quit` stops sibling delivery. A deadline/`Quit` tie follows T10's unspecified item ordering. |
| T6 | A timed-out inner stream is dropped and never polled again; no rollback is promised. |
| T7 | Timeout on a stream-less command is inert and preserves `is_none()`. |
| T8 | `map` and `batch` preserve leaf count, order, and placement semantics; child timeouts remain independent. |
| T9 | Nested timeout emits only the earlier message on one leaf; a tie emits exactly one unspecified message. Different batch leaves may emit from different calls. |
| T10 | Deadline/item tie ordering is unspecified and tests accept either outcome. `Item` includes `Message` and `Quit`. Termination (`None`) still wins. A poll in which the deadline is ready passes through at most the simultaneously ready inner item; if it does, no later inner item is delivered, the wrapper takes a terminal transition no later than its next poll, and it returns `None` after any terminal output. `Duration::ZERO` does not panic. |

### B.2 Retry contracts

| ID | Contract |
| --- | --- |
| R1 | `max_attempts` includes the first attempt; completed processing emits exactly one final message. |
| R2 | Final-attempt failure returns `Exhausted` without calling `should_retry`. |
| R3 | `StoppedByPredicate` occurs only while attempts remain. |
| R4 | `None` adds no delay; `Fixed` waits the same delay before every next attempt. |
| R5 | `RetryError` retains `last_error` and exposes it through `Display` and `Error::source()`. |
| R6 | `try_new(0)` is `None`; `new` defaults to no backoff; fixed-backoff builders preserve `max_attempts`. |
| R7 | A retry command behaves under `map`, `without_redraw`, and `batch` like a single-leaf future command. |
| R8 | `should_retry` may retain local state through its `FnMut` bound. |

### B.3 Test strategy

Time-dependent tests use `#[tokio::test(start_paused = true)]` and
`tokio::time::advance`. Tokio's `test-util` feature is already a
dev-dependency.

- Timeout unit tests in
  [`src/command.rs`](../../src/command.rs) and
  [`src/command/effect.rs`](../../src/command/effect.rs) cover T1–T10,
  including pending and early-completing futures, multiple stream messages,
  `map` on either side of timeout, batch factory consumption (completed and
  pre-deadline `Quit` leaves do not consume the factory), deadline/`Quit` ties,
  bounded progress after an elapsed deadline, nested timeout, and
  `Duration::ZERO`.
- Retry unit tests in `src/command/retry.rs` cover R1–R8, including first- and
  second-attempt success, exhaustion, predicate stop, one allowed attempt,
  policy validation, and backoff.
- Runtime smoke tests verify that timeout and retry final messages reach
  `update` and that pending timeouts or backoffs do not detach at shutdown.
  Existing `JoinSet` abort behavior covers shutdown; add a `Notify`-based test
  if that behavior needs an explicit assertion.

## Appendix C. Implementation Guide (Non-Normative)

Any implementation that satisfies the normative contracts is valid. The
sketches below show how to realize the contracts with the existing
dependencies.

### C.1 Suggested order

1. Add `Effect::timeout` and `Command::timeout` with unit tests and rustdoc.
2. Add retry support types in `src/command/retry.rs` and re-export them from
   `src/command.rs` and `src/lib.rs`, but not the prelude.
3. Add `Command::retry`, `retry_if`, and a `run_retry` helper.
4. Add runtime integration smoke tests.
5. Update CHANGELOG, README, and rustdoc.

Timeout comes first because it changes `effect.rs`. Retry can be built on
`Command::future` without changing `Effect`.

### C.2 Timeout sketch

`Effect::timeout(duration, on_timeout)` can return `Effect::None` unchanged and
wrap every `Effect::Leaves` entry in a stateful `timeout_leaf` stream.

One implementation shares `on_timeout` as `Arc<Mutex<Option<F>>>`. The first
leaf to reach its deadline takes the `FnOnce`, satisfying T3. The mutex keeps
the public bound at `FnOnce + Send` rather than exposing `Sync`. As in
`Effect::map`, a poisoned lock can be recovered with
`PoisonError::into_inner` so one sibling panic does not cascade.

Each wrapper stores the inner stream and a lazily created `Sleep`:

```text
timeout_leaf(inner, duration, shared_on_timeout):
  state = { inner: Some(inner), sleep: None, deadline_observed: false }

  loop per poll:
    if state.inner is None: end of stream
    if state.sleep is None: state.sleep = sleep(duration)   # T2

    if state.deadline_observed:
      poll inner once only to preserve termination priority:
        inner yields None                => drop inner, end of stream
        inner yields an item or Pending  => take deadline path

    otherwise poll inner and sleep:
      inner yields None                  => drop inner, end of stream
      inner yields an item, sleep Pending:
        Message(msg)                     => emit Message(msg), continue
        Quit                             => drop inner, emit Quit
      inner Pending, sleep fires         => take deadline path
      inner yields an item, sleep fires  => choose either outcome:
        deadline wins                    => take deadline path
        Message(msg) wins                => state.deadline_observed = true,
                                            emit Message(msg)
        Quit wins                        => drop inner, emit Quit
      inner Pending, sleep Pending       => Pending

    deadline path:
      drop inner
      take shared_on_timeout:
        Some(f)                          => emit Message(f())
        None                             => end of stream
```

Every terminal transition drops the inner stream. Three observable requirements
constrain the implementation:

1. **Termination wins (T4, T9).** If `inner.next()` returns `None` while the
   wrapper's deadline is also elapsed, the wrapper closes without a timeout
   message. An unbiased `select!` alone does not guarantee this for nested
   timeouts.
2. **The deadline has bounded progress (T10).** Once polling observes the sleep
   ready, that poll may choose either the simultaneously ready item or the
   deadline. If it chooses a non-terminal message, the wrapper records that the
   deadline was observed. On the next poll it checks the inner stream only for
   `None` so termination can still win; otherwise it takes the deadline path
   without yielding another item. A chosen `Quit` is already terminal. Thus at
   most one inner item is passed through after the deadline is ready and before
   the wrapper takes a terminal transition. That transition may itself emit the
   timeout message; the wrapper returns `None` on subsequent polls.
3. **All item ties are unspecified (T5, T10).** Both `Action::Message` and
   `Action::Quit` are items for this rule. If `Quit` wins, it is delivered and
   closes the wrapper; if the deadline wins, the inner stream is dropped
   without delivering `Quit`.

State layout, fast paths, and combinator choice remain implementation details.

### C.3 Retry sketch

`Command::retry` and `retry_if` can use `Command::future`:

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
        if attempt == policy.max_attempts:          # R2
          return Err(RetryError { attempts: attempt, last_error: error,
                                  reason: Exhausted })
        if !should_retry(&error, ctx):              # R3
          return Err(RetryError { attempts: attempt, last_error: error,
                                  reason: StoppedByPredicate })
        sleep per policy.backoff                    # R4
        attempt += 1
```

`RetryContext::new` remains `pub(crate)` so only crate code constructs retry
contexts.

## References

- [RFC 0002: Redraw Suppression](./0002-redraw-suppression.md) — output
  treatment versus execution lifecycle.
- [RFC 0003: Command Cancellation](./0003-command-cancellation.md) — keyed
  lifecycle and private receivers.
- [`src/command.rs`](../../src/command.rs) and
  [`src/command/effect.rs`](../../src/command/effect.rs) — current `Command`
  and `Effect` representation.
- [`src/subscription/time.rs`](../../src/subscription/time.rs) and
  [`src/runtime/frame_rate.rs`](../../src/runtime/frame_rate.rs) — validation
  API precedents.
