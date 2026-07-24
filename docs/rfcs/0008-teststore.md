# RFC 0008: TestStore — deterministic update and effect testing

- Status: Draft
- Target: an additive, executor-free test harness for the current
  `Application` API (stage 1: pure `update` + immediately ready effects)
- Scope: the `Message` trait-bound decision, the exhaustive-assertion
  decision, the `TestStore` public surface, its delivery-order and
  cancellation-parity contracts, and the staging split with Clock DI
- Feature flag: none (precedent: `subscription::mock` ships
  unconditionally)
- CHANGELOG: `Added` entry lands at the implementation release, not with
  this RFC

> **Staging.** This RFC specifies stage 1 only: driving pure `update`
> transitions and effects that become ready without an executor, a timer,
> or wall-clock time. Time-dependent effects (`Command::timeout`, retry
> backoff, `Timer`-based subscriptions) are stage 2, which waits on a
> separate Clock DI RFC (§7) and lands as a reviewed amendment to this
> document.

## Summary

Three decisions, ordered by urgency:

1. **`Message` boundary** (§2): `Application::Message`'s bounds stay
   exactly `Send + 'static`. `TestStore` carries its own bounds instead —
   `Debug` on the store, `PartialEq` scoped to the equality-asserting
   methods only, and `Clone` nowhere. Retrofitting a bound onto
   `Application::Message` later would be a silent breaking change for
   every existing application, so the placement is pinned now, before any
   TestStore code exists, and constrains future RFCs (INV-T1, INV-T2).
2. **Exhaustive assertions** (§6): exhaustive is the only mode in
   stage 1. Undelivered deliverable output — a ready message never
   received, an effect stream never driven to completion — fails the
   test. The one carve-out mirrors the runtime's shutdown contract:
   output remaining after an observed quit is legally discarded. A
   non-exhaustive mode is deliberately not designed here (open
   question 2).
3. **Clock DI split** (§7): Clock injection is a separate RFC. This RFC
   does not gate on it, and stage 2 of TestStore gates on that RFC, not
   the reverse.

The harness itself: `tears::testing::TestStore<App>` wraps an
`Application`, applies messages synchronously through `update`, consumes
the returned `Command` through the same decomposition boundary the
runtime uses, and lets the test assert state, delivered messages, quit,
declared subscription identity, and the redraw directive — with RFC 0003
cancellation semantics honored on the pending output (§5).

## 1. Scope

### 1.1 In scope (stage 1)

- Constructing the store from `Application::new(flags)`, including the
  init command's effects.
- `send`: apply one message through `update`, synchronously, with no
  executor and no task spawn.
- `receive`: assert the next deliverable message (or quit) and apply it
  through `update`, closing the effect→message loop the runtime closes.
- State observation (`state(&self) -> &App`) for plain-assertion access.
- Effects that become ready without an executor: `Command::message`,
  `Command::future`/`perform` over immediately ready futures,
  `Command::stream`/`run`, `Command::quit`, `Command::batch`,
  `Command::map`, and effects fed by test-controlled sources (for
  example a channel-backed future the test completes between calls).
- Cancellation metadata: `cancellable` / `cancellable_with` / `cancel` /
  `scoped`, with RFC 0003's delivery semantics applied to the store's
  pending output (§5.1).
- The redraw directive (RFC 0002) as a per-step observation (§5.2).
- Declared subscription identity: the `SubscriptionId` set returned by
  `subscriptions()`, observed without starting any source (§3.2).

### 1.2 Out of scope (stage 1)

- **Time-dependent effects**: `Command::timeout`, `RetryPolicy` with a
  non-zero backoff, and any effect that needs a timer or reactor. Polling
  such a leaf inside TestStore fails the test (§4.3); making it pass
  deterministically is exactly stage 2's job (§7).
- **Subscription source execution**: TestStore never starts, polls, or
  restarts a subscription source. It observes the *declared* set only.
  Lifecycle behavior (keyed restart, RFC 0005) stays covered by the
  runtime's own tests.
- **Runtime integration contracts**: channel capacities, backpressure,
  batching, and scheduling (RFC 0006/0007) are properties of the runtime
  event loop, which TestStore replaces. A TestStore run exercises none of
  them, and passing TestStore proves nothing about them — stated here as
  negative space so no later document cites TestStore as evidence for a
  runtime scheduling property.
- **Reducer composition**: there is no reducer trait yet. TestStore
  targets `Application` directly; when a composition API lands, its
  `Application` adapter is expected to reuse this store rather than grow
  a second harness, but that is that RFC's obligation, not this one's.

## 2. The `Message` boundary

### 2.1 Decision

- `Application::Message`'s bounds remain exactly `Send + 'static`. This
  RFC adds no bound to any `Application` associated item, and pins that
  as an invariant (INV-T1) rather than an accident: a future RFC that
  wants a bound on `Application::Message` must state it as a breaking
  change with a migration path, and cannot present it as implied by
  TestStore.
- TestStore's bounds, in full:
  - `App::Message: Debug` on the store itself. Every exhaustiveness and
    mismatch failure names the offending messages; a diagnostic that
    cannot print what leaked is not worth the harness.
  - `App::Message: PartialEq` on the equality-asserting methods only
    (`receive`, and any future `receive_*` that compares values).
  - `Clone` nowhere. A delivered message is asserted once and then moved
    into `update`; the expected value is consumed by the assertion.
    Nothing in the store's contract needs two copies of a message.
- `receive_matching(predicate)` is provided alongside `receive` as the
  bound-free escape hatch: it asserts via a caller predicate and needs
  neither `PartialEq` nor anything beyond the store's `Debug`. It exists
  so `PartialEq` never becomes load-bearing — an application whose
  message type cannot implement `PartialEq` (for example one carrying a
  connection handle) still gets the full harness.

### 2.2 Rejected alternatives

- **Add `PartialEq` (or `Debug`) to `Application::Message`.** Breaking
  for every existing application whose message type lacks the impl, in
  exchange for nothing stage 1 needs: the store-scoped placement above
  reaches the same test ergonomics. Rejected.
- **Require `Clone` for re-delivery or replay.** No stage-1 API replays
  a message. If a future replay/snapshot feature wants `Clone`, it can
  carry the bound on its own method under INV-T2's placement rule; it
  does not need it here. Rejected.
- **Matcher-only API (no `PartialEq` anywhere).** Keeps bounds minimal
  but makes the common case — "the effect resolved to exactly this
  message" — a closure with a hand-written match on every call site.
  Equality assertion is the ergonomic default in every comparable
  harness; the matcher stays as the escape hatch, not the front door.
  Rejected as the sole API.

## 3. Public API

### 3.1 Type and construction

```rust
/// Deterministic, executor-free test harness for an `Application`
/// (RFC 0008). Drives `update` and immediately ready effects
/// synchronously; time-dependent effects are out of scope until the
/// Clock DI RFC lands.
pub struct TestStore<App: Application>
where
    App::Message: Debug,
{ /* private */ }

impl<App: Application> TestStore<App>
where
    App::Message: Debug,
{
    /// Runs `Application::new(flags)` and enqueues the init command.
    #[must_use]
    pub fn new(flags: App::Flags) -> Self;

    /// The application state, for plain assertions.
    pub fn state(&self) -> &App;

    /// Applies `msg` through `update` and enqueues the returned
    /// command's effects. Fails the test if a deliverable message is
    /// pending (§6) or quit has been observed (§5.3).
    pub fn send(&mut self, msg: App::Message);

    /// Asserts that the next deliverable output is a message equal to
    /// `expected`, then applies it through `update`.
    pub fn receive(&mut self, expected: App::Message)
    where
        App::Message: PartialEq;

    /// Like `receive`, but asserts via a predicate; requires no
    /// `PartialEq`.
    pub fn receive_matching(&mut self, matches: impl FnOnce(&App::Message) -> bool);

    /// Asserts that the next deliverable output is a quit request and
    /// puts the store into the quit state (§5.3).
    pub fn receive_quit(&mut self);

    /// Whether the command returned by the most recent `send`/`receive`
    /// step requested a redraw (RFC 0002).
    pub fn redraw_requested(&self) -> bool;

    /// The `SubscriptionId`s the application currently declares. Pure
    /// observation; no source is started.
    pub fn subscription_ids(&self) -> Vec<SubscriptionId>;

    /// Consumes the store, failing the test if deliverable output or an
    /// unfinished effect remains and quit was not observed (§6).
    pub fn finish(self);
}
```

The signatures above are normative for bound *placement* (which bound
appears where — INV-T2); parameter spellings (`impl FnOnce` vs a named
generic) are implementation latitude.

### 3.2 Method semantics

- **`new`** applies `Application::new` and enqueues the init command
  exactly as a `send` enqueues an update's command. Exhaustiveness
  applies from construction: an init command's ready message must be
  received before the first `send`.
- **`send`** is one synchronous `update` call plus bookkeeping. It
  spawns no task, requires no executor, and returns only after the
  command's metadata (directives, cancellation) has been applied to the
  store. It does not poll effects; polling happens in `receive*` calls.
- **`receive` / `receive_matching`** select the next deliverable output
  under the canonical order (§4.2), assert it, and apply it through
  `update` — so the store advances exactly as the runtime would on that
  delivery, including enqueuing the resulting command. If the next
  deliverable output is a quit request, both fail with a diagnostic
  saying so (quit is asserted only via `receive_quit`). If nothing is
  deliverable, both fail with a diagnostic that distinguishes "no
  pending effects" from "effects pending but not ready" (§4.3).
- **`receive_quit`** asserts the next deliverable output is a quit
  request. After it succeeds, the store is in the quit state: `send`,
  `receive`, `receive_matching`, and `receive_quit` all fail; `state`,
  `redraw_requested`, `subscription_ids`, and `finish` remain callable.
- **`subscription_ids`** calls `Application::subscriptions` and returns
  the declared IDs in declaration order. `SubscriptionId` is already
  `Clone + Eq + Hash + Debug`, so the test asserts on it directly. This
  is the observation the `subscriptions`-purity contract
  (`src/application.rs`) makes meaningful: the declared set is a pure
  function of state, so asserting it after a `send` is deterministic.
- **`finish` / drop**: `finish` runs the exhaustiveness check (§6).
  Dropping an unfinished store runs the same check, except when the
  thread is already panicking (so a failed assertion does not cascade
  into a double panic). The drop check exists so a test that forgets
  `finish` cannot silently leak output; `finish` remains the recommended
  spelling because its failure points at the right line.

### 3.3 Placement

- Module: `src/testing.rs` (`pub mod testing`), path
  `tears::testing::TestStore`. No crate-root re-export and no prelude
  membership: a minimal skeleton app never names the type
  (`docs/api-guidelines.md`, "Prelude Membership" — the same test that
  keeps `RuntimeConfig` out of the prelude, RFC 0007 §2.3), and a single
  path keeps `tests/api_surface.rs` invariants trivially satisfied.
- No feature flag. Precedent: `subscription::mock` ships
  unconditionally as public test support. A flag would put a compile
  gate between a user and their first test for zero build-cost benefit
  (the store is small and pulls no new dependency).

## 4. Determinism and delivery contract

### 4.1 Deliverable output

TestStore holds the effects of every command it has accepted (init
command, then each step's command) as a pending set of leaf streams,
consumed through `Command`'s runtime decomposition boundary — the same
`into_runtime_parts` lowering the runtime uses — so what the store
observes (directives, cancellation metadata, effect stream) is what the
runtime observes (INV-T3). A leaf's output is **deliverable** when the
leaf yields it under polling with no executor, no timer, and no external
wake — either immediately, or because a test-controlled source (a
oneshot the test completed, a `MockSource`-style seam) has made it ready
since the last call. Polling happens only inside `receive*`,
`send`-precondition, and `finish` checks; between calls the store does
nothing.

### 4.2 Ordering

- **Within one leaf**: messages are delivered in stream order. This is
  the leaf's own contract and the store preserves it.
- **Across leaves**: the store delivers from the *earliest-enqueued*
  leaf that is currently deliverable. Enqueue order is: init command
  first, then each step's command in step order; within one command,
  leaves in `Command::batch`'s flattened declaration order. Two runs of
  the same test program therefore observe the same delivery sequence
  (INV-T4, INV-T6).
- **Negative space, stated deliberately**: the canonical cross-leaf
  order is *TestStore's* contract, not the runtime's. The runtime folds
  a command's leaves through an unordered select and pins no cross-leaf
  delivery order (RFC 0003's ordering-adjacent invariants — INV-10's
  one-item dispatch and INV-14's shared-first app-input scheduling —
  order dispatch and pull points, not sibling leaves).
  A test that asserts an interleaving across sibling leaves is asserting
  TestStore's linearization and remains valid as a TestStore test, but
  must not be cited as evidence of runtime ordering. Whether a
  set-based `receive_unordered` helper should exist for such tests is
  open question 1.

### 4.3 Leaves that cannot become ready

A leaf that requires a timer or reactor (`Command::timeout` wraps every
leaf in a deadline; a backoff retry sleeps; `Timer` is
`tokio::time::interval`-backed) cannot be driven without an executor.
Stage 1's contract is honest about this rather than silently lenient:
polling such a leaf fails the test. The failure today surfaces as the
underlying missing-reactor panic, which is a poor diagnostic; the
documentation on `TestStore` names the limitation and points at the
stage-2 plan (§7). A leaf that is merely *pending* on a test-controlled
source does not fail anything: it is skipped by the canonical order
until it becomes deliverable, and only `finish` holds it to account
(§6).

## 5. Cancellation, directives, quit

### 5.1 Cancellation parity (RFC 0003)

The store applies RFC 0003's delivery semantics to its pending set, with
occupancy defined on the store's own lifecycle: **an id is occupied
while its current run's stream has not been driven to completion.**

- A keyed command under `CancelPolicy::CancelInFlight` supersedes the
  same-id occupant: the occupant's undelivered output — buffered
  messages and quit requests alike — can no longer be delivered
  (RFC 0003 INV-3, INV-6, INV-9), and the new stream takes the id.
- Under `CancelPolicy::KeepInFlight`, while the id is occupied the new
  command's stream is discarded and the occupant is untouched (INV-5).
- `Command::cancel(id)` drops the occupant's stream and undelivered
  output, and is idempotent (INV-4).
- Unkeyed commands are unaffected by any of the above (INV-1's default
  path).
- `Command::batch`'s child-key folding needs no restatement: the store
  consumes real `Command` values, so batch has already discarded child
  keys and folded cancels before the store sees the parts (RFC 0003
  INV-11).

These are the deterministic core of RFC 0003 — what may still be
delivered — restated over the store's pending set. The runtime-side
mechanics that exist only because the runtime is concurrent (task
reaping, stale-exit tokens, INV-7/8/13) have no TestStore counterpart
and are deliberately not modeled.

### 5.2 Redraw directive (RFC 0002)

`redraw_requested()` reports the folded redraw directive of the command
returned by the most recent step (a `send`, or the `update` call inside
a `receive*`). Before any step completes it reports the init command's
directive. This makes `without_redraw` decisions assertable per
transition, which is the granularity RFC 0002 defines them at.

### 5.3 Quit

Quit is a deliverable output like any message and is asserted explicitly
via `receive_quit` (§3.2). After quit is observed the store mirrors the
runtime's shutdown contract: remaining undelivered output is legally
discarded — `finish` passes regardless of what remains (the analogue of
the shutdown discard carve-out in RFC 0006's INV-L2), and further
`send`/`receive*` calls fail because the application would no longer be
running. A quit that is *suppressed* by cancellation (§5.1) is not
"observed" and triggers none of this — exactly RFC 0003 INV-9.

## 6. Exhaustiveness

Exhaustive assertion is the only stage-1 mode. The rules, by call site:

- **`send`** fails if any deliverable message or quit request is
  pending. Effects that are pending but not deliverable (§4.1) do not
  block `send` — they may be waiting on a test-controlled source the
  test will feed later.
- **`receive` / `receive_matching` / `receive_quit`** fail on a
  mismatch, on quit-versus-message confusion, or when nothing is
  deliverable — each with a diagnostic that names the actual value
  (`Debug`) and, in the nothing-deliverable case, distinguishes "no
  pending effects" from "effects pending but not ready".
- **`finish`** (and the drop check, §3.2) fails if quit was not
  observed and either (a) a deliverable message or quit request remains,
  or (b) any pending leaf has not been driven to completion — an
  in-flight effect the test never accounted for is a leak even if it
  never produced a message. After an observed quit, `finish` passes
  unconditionally (§5.3).
- Every exhaustiveness failure names the leaked messages via `Debug`;
  unfinished leaves that have produced no value are reported by count
  and enqueue position (there is no value to print).

Rationale for exhaustive-only: the harness exists to make effect flow
*fully* explicit — TCA's experience is that the exhaustive mode is where
the testing value concentrates, because unasserted output is exactly
where regressions hide. A lenient mode is a real feature with real
semantics to design (what is skippable, what drop does, how it composes
with quit) and no current consumer; it is deferred with a trigger, not
smuggled in half-specified (open question 2).

## 7. Clock DI split

**Decision: Clock injection is its own RFC; this RFC does not contain
it.** Stage 2 of TestStore — deterministic driving of `timeout`, retry
backoff, debounce/throttle, and `Timer` — lands as an amendment to this
document after that RFC is accepted.

Rationale:

- The `Message`-bound decision (§2) is urgent and wholly independent of
  time: deferring it until a Clock design settles would leave the silent
  breaking-change risk open for no gain.
- Clock DI is a cross-cutting determinism contract — it decides what is
  injectable for *every* time-dependent subscription and command
  modifier, with the runtime and future debounce/throttle work as
  consumers alongside TestStore. Folding it in here would put a
  runtime-wide contract inside a testing RFC, inverting the dependency:
  TestStore should consume the Clock contract, not own it.
- Stage 1 is additive and independently shippable; bundling would gate
  it behind the larger design.

Non-normative sketch of what the amendment adds, recorded so stage-1 API
shapes are chosen with it in mind: a store-held clock handle, an
`advance(duration)` call that makes time-gated leaves deliverable, and
the §4.3 limitation dissolving into ordinary `receive` flow. Nothing in
the stage-1 surface above assumes otherwise or blocks that shape.

## 8. Invariants

Enforcement classes follow the pre-review checklist's definitions
(structural / behavioral / statistical).

- **INV-T1**: `Application`'s definition is unchanged by this RFC —
  `type Message: Send + 'static` and no new bound on any associated
  item. Structural: review of `src/application.rs` against the pre-RFC
  definition. Behavioral: a compile test, added with the
  implementation, instantiates `Application` with a message type that
  implements nothing beyond `Send + 'static` (no existing test does —
  every current test app's message type also derives comparison and
  formatting traits, so none would catch a smuggled bound).
- **INV-T2**: TestStore's bounds are exactly §2.1's — `Debug` on the
  store, `PartialEq` on equality-asserting methods only, `Clone` on
  nothing. Behavioral: a compile test drives `new` → `send` → `state` →
  `receive_matching` → `finish` with a message type implementing `Debug`
  but neither `PartialEq` nor `Clone`. Structural: review of every
  public TestStore signature for stray bounds.
- **INV-T3**: TestStore consumes each command through the same
  decomposition boundary the runtime consumes
  (`Command::into_runtime_parts`), never a parallel re-derivation of
  directives, cancellation, or effects. Structural: review of the
  store's single command-intake site. This is what makes TestStore
  results evidence about real commands rather than about a test-only
  model.
- **INV-T4**: `send` and `receive*` are synchronous — no task spawn, no
  executor requirement — and two executions of one test program observe
  identical state transitions and delivery sequences. Behavioral: a
  repeated-run test asserts equal delivery transcripts across runs of a
  multi-leaf, cancellation-exercising program.
- **INV-T5**: one leaf's messages are delivered in stream order.
  Behavioral: a multi-message `Command::stream` test.
- **INV-T6**: across leaves, delivery follows §4.2's canonical order
  (earliest-enqueued deliverable leaf first), and this order is
  TestStore's contract only — no test or document may cite it as a
  runtime ordering guarantee, which the runtime does not make.
  Behavioral for the order itself: a batch of ready leaves delivers in
  declaration order; a leaf made ready late delivers after an
  earlier-enqueued ready leaf but before a later one. The negative-space
  half is documentation, checked in review of the rustdoc (structural).
- **INV-T7**: cancellation parity — the four behaviors of §5.1
  (supersede, keep-in-flight discard, explicit cancel, unkeyed
  unaffected) hold over the store's pending output as RFC 0003's INV-3,
  INV-4, INV-5, INV-6, and INV-9 state them for deliverable output.
  Behavioral: one test per behavior, including quit suppression
  (a superseded keyed quit is never observable via `receive_quit`).
- **INV-T8**: exhaustiveness — each leak class in §6 fails at its named
  call site, with a diagnostic naming the leaked values for the
  message classes and the count and enqueue position for the
  unfinished-leaf class (§6). Behavioral: one test per class (ready
  message at `send`; ready message at `finish`; unfinished leaf at
  `finish`; drop-without-finish), each asserting the failure fires
  *and* its message contains the class's required content — the leaked
  value's `Debug` rendering for the message classes — because the
  wrong-value adversary is exactly what the message-content assertion
  exists to fail.
- **INV-T9**: quit terminality and carve-out — after `receive_quit`,
  `send`/`receive*` fail and `finish` passes regardless of remaining
  output. Behavioral: a test quits with output still pending and
  asserts both halves.

Surface–invariant coverage: `new`/`send`/`receive*`/`finish` map to
INV-T2/T3/T4/T8; delivery order to INV-T5/T6; cancellation metadata to
INV-T7; `receive_quit` and the quit state to INV-T9;
`redraw_requested` and `subscription_ids` are pure observations of
contracts owned elsewhere (RFC 0002's directive; RFC 0005's declared
identity) and are covered by INV-T3 (they read what the runtime would
read) plus one behavioral test each; the absence of an `Application`
change maps to INV-T1.

## 9. Open questions

1. **Unordered batch receive.** Should a set-based helper (assert that
   the next N deliverable messages equal this multiset, in any order)
   exist for tests over sibling leaves, so they need not encode the
   canonical linearization? Trigger: real tests that repeatedly assert
   cross-leaf sequences where the order is incidental. Additive either
   way; stage 1 ships without it.
2. **Non-exhaustive mode.** A lenient mode (unasserted output tolerated,
   or selectively skippable) is not designed here. Trigger: a concrete
   consumer — most plausibly migrating a large existing test suite —
   that exhaustive-only demonstrably blocks. Designing it then is a
   reviewed amendment (skippability interacts with §5.3's quit carve-out
   and §6's drop check).

## 10. References

- RFC 0002 — redraw suppression: the directive `redraw_requested`
  observes.
- RFC 0003 — command cancellation: INV-1, INV-3, INV-4, INV-5, INV-6,
  INV-9, INV-10, INV-11, INV-14 (cited in §§4.2, 5.1, 5.3).
- RFC 0005 — structural lifecycle identity: `SubscriptionId`, the
  declared-set semantics `subscription_ids` observes.
- RFC 0006 — runtime load control: the shutdown discard carve-out §5.3
  mirrors (INV-L2); the runtime contracts §1.2 excludes.
- RFC 0007 — RuntimeConfig: the prelude-membership reasoning §3.3
  follows.
- `src/application.rs` — the trait whose bounds §2 pins.
- `src/command/core.rs`, `src/command/runtime_parts.rs` — the
  decomposition boundary INV-T3 names.
- `src/subscription/mock.rs` — the unconditional test-support precedent
  §3.3 cites.
- `docs/rfcs/pre-review-checklist.md` — enforcement-class definitions
  used in §8.
