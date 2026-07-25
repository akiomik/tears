# RFC 0008: TestStore — deterministic update and effect testing

- Status: Accepted
- Target: an additive, executor-free test harness for the current
  `Application` API (stage 1: pure `update` + immediately ready effects)
- Scope: the `Message` trait-bound decision, the exhaustive-assertion
  decision, the `TestStore` public surface, its delivery-order and
  cancellation-parity contracts, the per-leaf `RuntimeCommandParts`
  prerequisite (§4.1), and the staging split with Clock DI
- Feature flag: none (precedent: `subscription::mock` ships
  unconditionally)
- CHANGELOG: `Added` entry lands at the implementation release, not with
  this RFC

> **Staging.** This RFC specifies stage 1 only: driving pure `update`
> transitions and effects that become ready without an executor, a timer,
> or wall-clock time. Time-dependent *command* effects (`Command::timeout`,
> retry backoff) are stage 2, which waits on a separate Clock DI RFC (§7)
> and lands as a reviewed amendment to this document. `Timer`-based
> subscriptions are not staged here at all: TestStore never executes
> subscription sources (§1.2), so lifting that is a separate
> subscription-execution design, not stage 2.

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
   test at `receive`, `finish`, or drop; `send` does not block on
   pending output, so a scripted `send` can supersede or cancel an
   earlier step's not-yet-received keyed output the way the runtime's
   shared-first schedule does (§6). The one carve-out mirrors the
   runtime's shutdown contract: output remaining after an observed quit
   is legally discarded. A non-exhaustive mode is deliberately not
   designed here (open question 2).
3. **Clock DI split** (§7): Clock injection is a separate RFC. This RFC
   does not gate on it, and stage 2 of TestStore gates on that RFC, not
   the reverse.

The harness itself: `tears::testing::TestStore<App>` wraps an
`Application`, applies messages synchronously through `update`, consumes
the returned `Command` through the same decomposition boundary the
runtime uses (§4.1 records the named prerequisite refactor that makes
that boundary carry per-leaf streams), and lets the test assert state,
delivered messages, quit,
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
  runtime's own tests. This covers `Timer` and every other
  `SubscriptionSource`: subscription execution is out of scope in
  stage 1 and stage 2 alike — lifting it would be a separate
  subscription-execution design, not the Clock DI stage-2 amendment
  (RFC 0009 §5.1), which delivers command time leaves only.
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
  - `App::Message: Debug` on the store itself. Exhaustiveness and
    mismatch failures name the offending messages wherever a value
    exists to name (§6's unfinished-leaf class has none and reports
    count and enqueue position instead); a diagnostic that cannot print
    what leaked is not worth the harness.
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
    ///
    /// Panics if called while a Tokio runtime is already entered — for
    /// example, from inside `#[tokio::test]` (§4.3, INV-T10).
    #[must_use]
    pub fn new(flags: App::Flags) -> Self;

    /// The application state, for plain assertions.
    pub fn state(&self) -> &App;

    /// Applies `msg` through `update` and enqueues the returned
    /// command's effects. Does not deliver or poll pending output;
    /// undelivered output stays caught by `receive*`/`finish`/drop (§6).
    /// Fails the test only if quit has been observed (§5.3).
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
    /// step requested a redraw (RFC 0002). `receive_quit` is not a
    /// step (§5.2).
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
  exactly as a `send` enqueues an update's command. The init command's
  deliverable output is subject to the same `receive*`/`finish`/drop
  accounting as any step's output (§6); `send` does not require it to
  be received first.
- **`send`** is one synchronous `update` call plus bookkeeping. It
  spawns no task, requires no executor, and returns only after the
  command's metadata (directives, cancellation) has been applied to the
  store. It runs no deliverable-output exhaustiveness precondition and
  polls no pending leaf for output; pending deliverable output is left
  in place for a later `receive*` (or caught by `finish`/drop, §6),
  which lets a scripted `send` supersede or cancel an earlier step's
  not-yet-received keyed output as the runtime's shared-first schedule
  allows (§6). Its only poll is the keyed-intake reconciliation of §5.1,
  and only when the returned command is keyed under
  `CancelPolicy::KeepInFlight`; it never delivers output, which happens
  only in `receive*` calls.
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
  the declared IDs in declaration order, deduplicated by RFC 0005 §3.5's
  first-wins rule: for equal full IDs in the declared list, only the
  first occurrence is returned. This mirrors the set the runtime would
  actually start — `SubscriptionManager::update` applies the same rule
  before spawning — so asserting on `subscription_ids()` predicts which
  subscriptions would be live, not merely which lines of `subscriptions`
  were written. TestStore does not reproduce the warning-level tracing
  event RFC 0005 §3.5 requires of the ignored duplicate: that event is
  `SubscriptionManager::update`'s own side effect, and TestStore never
  constructs or runs a `SubscriptionManager` (§1.2). `SubscriptionId` is
  already `Clone + Eq + Hash + Debug`, so the test asserts on the
  returned `Vec` directly. This is the observation the
  `subscriptions`-purity contract (`src/application.rs`) makes
  meaningful: the declared set is a pure function of state, so asserting
  it after a `send` is deterministic.
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

### 4.1 Command intake, deliverability, and the poll budget

TestStore holds the effects of every command it has accepted (init
command, then each step's command) as a pending set of leaf streams in
enqueue order, consumed through `Command`'s runtime decomposition
boundary (`into_runtime_parts`) so that what the store observes —
directives, cancellation metadata, effects — is what the runtime
observes (INV-T3).

**Named prerequisite — per-leaf parts.** Today that boundary folds a
multi-leaf effect into one stream before the parts exist:
`into_runtime_parts` calls `Effect::into_stream()`, which merges the
leaves through an unordered select (`src/command/core.rs`,
`src/command/effect.rs`). A store built on the current parts type
therefore could not implement §4.2's per-leaf canonical order without
re-deriving the leaves in parallel — exactly what INV-T3 forbids — and
relying on the merged stream happening to yield in declaration order
would rest on a coincidental property of the select combinator, not on
any contract. Stage-1 implementation is therefore gated on a
prerequisite refactor, owned by this RFC's implementation task:
`RuntimeCommandParts` carries the effect's leaves unfolded, in
`Command::batch`'s flattened declaration order, and each consumer folds
or drives them at its own consumption site — the runtime merging them
at its spawn site exactly as `into_stream()` merges them today (a
behavior-preserving relocation of the existing fold; `Effect` already
keeps its leaves apart to preserve leaf identity for future per-leaf
consumers, per its own comment in `src/command/effect.rs`), the store
keeping them apart. INV-T3 names this revised boundary.

**Deliverability and the poll budget.** A leaf's output is
**deliverable** at a given check when the leaf yields it on that
check's poll, or when a keyed-intake reconciliation poll (§5.1) has
already yielded it into the leaf's buffer — a buffered item is
deliverable at every later check without a further poll. The poll
contract is fixed, because INV-T4's determinism rests on it: polling
happens only inside `receive*`, `finish`, and drop checks, plus the
keyed-intake reconciliation of §5.1 (a keyed `send`'s only poll, and
only under `KeepInFlight`). A bare `send` polls no leaf (§6). The
quit-state check precedes every one of these sites, so after an
observed quit no store call polls at all: `send`/`receive*` fail on
the quit state before reaching any leaf, and the `finish` and drop
checks pass without polling (§5.3, §6). Each check polls each leaf its
scan reaches (§4.2) exactly
once, with a waker whose wake-ups are not honored within the call. A
self-waking leaf is therefore re-polled no earlier than the next store
call and cannot loop a check — if it does not yield on its one poll, it
is not deliverable at that check; a leaf made ready between calls by a
test-controlled source (a oneshot the test completed, a
`MockSource`-style seam) is observed at the next check whose scan
reaches it. Between calls the store does nothing.

### 4.2 Ordering

- **Within one leaf**: messages are delivered in stream order. This is
  the leaf's own contract and the store preserves it.
- **Across leaves**: the store delivers from the *earliest-enqueued*
  leaf that is currently deliverable. Enqueue order is: init command
  first, then each step's command in step order; within one command,
  leaves in `Command::batch`'s flattened declaration order. Two runs of
  the same test program therefore observe the same delivery sequence
  (INV-T4, INV-T6).
- **Scan semantics**: a `receive*` check walks the pending leaves in
  enqueue order and stops at the first deliverable one (a buffered
  head, or a yield on that leaf's one poll); the pre-quit `finish`/drop
  checks scan in the same order until they find a deliverable output or
  an unfinished leaf (failing) or exhaust the set (establishing nothing
  is outstanding). `send` runs no such scan (§6). Either way, each
  reached leaf gets exactly one poll (§4.1).
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

A command leaf that requires a timer or reactor (`Command::timeout`
wraps every leaf in a deadline; a backoff retry sleeps) cannot be
driven without an executor. (`Timer` is not in this set: it is a
subscription source, not a command leaf, so it never enters the store's
pending command set — §1.2.) Stage 1's contract is honest about this
rather than silently lenient: polling such a leaf fails the test.

This guarantee holds only because stage 1 requires the reactor's
genuine *absence*, not merely the store's own inaction, and an entered
Tokio runtime does not reliably restore it: with the default
`#[tokio::test]` configuration (`enable_all`), the same leaf's first
poll finds a real reactor and returns `Pending` instead of panicking —
it never fails, it just never completes, which is a silent stage-1
hang, not the documented failure — while a runtime entered with a
narrower driver configuration can still panic on the same poll. Because
whether polling panics or hangs depends on which drivers the entered
runtime happens to enable, and the store has no cheap, safe way to
inspect that from inside, stage 1 does not try to distinguish: it
refuses to run inside any entered runtime at all. `TestStore::new`
therefore checks
`tokio::runtime::Handle::try_current()` and panics immediately, with a
diagnostic naming the precondition, if one is currently entered. Stage 1
tests must run on a plain `#[test]` (or equivalent), never
`#[tokio::test]`. The check is structural and happens once, at
construction, not per leaf: a store built outside a runtime but later
polled from inside one (a nested `block_on`) is a misuse this RFC does
not attempt to catch, matching stage 1's synchronous, executor-free
design (§1, §3.2). This precondition is stage-1-only (INV-T10, §8):
stage 2's controlled time context (§7; RFC 0009 §3.2) is itself a
paused single-threaded executor, so the Clock DI amendment replaces this
check with one that accepts *that specific* controlled context and
rejects any other runtime, rather than rejecting every runtime as stage
1 does.

Note what that means under §4.2's scan semantics: the failure fires at the
*first store call whose scan polls the leaf* — a `receive` aimed at a
different message, a `finish`/drop check, or a keyed-intake
reconciliation poll (§5.1) that reaches it, not only a call targeting
the leaf itself; a bare `send` never triggers it, because `send` polls
no leaf (§6). So from its enqueue onward the store is effectively
poisoned for those calls, unless every scan stops at an
earlier-enqueued yielding leaf first, or a `send`-issued cancellation
removes the leaf (§5.1) before a scan reaches it — the latter is now
expressible precisely because `send` itself does not poll. The failure
today surfaces as the underlying missing-reactor panic, which is a poor
diagnostic; the documentation on `TestStore` names the limitation and
points at the stage-2 plan (§7). A leaf that is merely *pending* on a
test-controlled source does not fail anything: it is skipped by the
canonical order
until it becomes deliverable, and only `finish` holds it to account
(§6).

## 5. Cancellation, directives, quit

### 5.1 Cancellation parity (RFC 0003)

The store applies RFC 0003's delivery semantics to its pending set.
Occupancy follows RFC 0003's own accounting (INV-6, INV-7): **an id is
occupied while its current run may still deliver output, and is
released once every one of the run's leaves has been observed
exhausted.** Exhaustion is observable only by polling, so the store
mirrors the runtime's pre-spawn reconciliation (before any
`Spawn(policy)` decision the runtime reaps completed keyed tasks and
samples the target receiver once — RFC 0003 §4.2) with **keyed-intake
reconciliation**: when a keyed command arrives for an occupied id and
its policy's admission decision depends on the occupant's state
(`CancelPolicy::KeepInFlight`), the store reconciles before that
decision. (`CancelInFlight`'s outcome does not depend on the occupant's
state, so it reconciles nothing — see the per-policy bullets below.) If
the occupant already has buffered output, the id is occupied and nothing
is polled; otherwise the store polls the occupant's remaining leaves in
enqueue order, stopping at the first that shows the run still open — a
yield (the item is buffered at that leaf's canonical position as its next
deliverable output; buffered output occupies, INV-6, and §6's
exhaustiveness counts it like any deliverable message) or a pending
(INV-7: "a still-open stream remains occupied even after delivering
one item"). The id is released exactly when every remaining leaf
completes — the analogue of INV-7's sender-closed empty receiver.

- A keyed command under `CancelPolicy::CancelInFlight` supersedes the
  same-id occupant: the occupant's undelivered output — buffered
  messages and quit requests alike — can no longer be delivered
  (RFC 0003 INV-3, INV-6, INV-9), and the new stream takes the id.
- Under `CancelPolicy::KeepInFlight`, the admission decision reads the
  reconciled state: while the id remains occupied the new command's
  stream is discarded and the occupant is untouched (INV-5); when
  reconciliation observes the occupant exhausted, the id is released
  and the new command is admitted (INV-7). The reconciliation poll is
  issued only for `KeepInFlight`, whose admission outcome depends on
  whether the occupant is still open. For `CancelPolicy::CancelInFlight`
  the outcome is fixed — the occupant is superseded and its undelivered
  output discarded regardless of its state — so the store issues no
  reconciliation poll: superseding an exhausted occupant and spawning
  into a released id are indistinguishable. The runtime does sample the
  target receiver's facts before *every* `Spawn(policy)`, `CancelInFlight`
  included (`reconcile_receiver` in `src/runtime/keyed_commands.rs`;
  RFC 0003 §4.2), but that sample cannot change a `CancelInFlight`
  admission outcome, and the store has no equivalent receiver snapshot to
  reconcile — so skipping the poll preserves the delivery contract while
  matching the runtime's outcome, not its every step. Not polling also
  lets a `CancelInFlight` command supersede a reactor-dependent occupant
  (a `timeout` or backoff leaf) without polling it, so cancellation of a
  time-dependent keyed leaf is expressible in stage 1 (§4.3). Any poll
  that is issued follows §4.1's budget.
- `Command::cancel(id)` drops the occupant's stream and undelivered
  output, and is idempotent (INV-4).
- When one command carries both explicit cancels and its own keyed
  spawn, the store applies RFC 0003's fixed order (RFC 0003 §5.1): the
  explicit cancels apply first, then the keyed spawn's admission
  decision — so a command can cancel its own occupant and immediately
  reclaim the id in one step.
  `Command::batch([Command::cancel(id), work]).cancellable(id)` drops
  the old occupant's undelivered output exactly as the bullet above
  describes, then admits `work` under `id`; the old run's output is
  gone, and only `work`'s output is thereafter deliverable at `id`. An
  implementation that instead admitted the new spawn before applying
  the batch's own cancel would cancel `work` itself and leave `id`
  empty — the wrong outcome — so this ordering is load-bearing, not
  incidental, and is asserted directly rather than left to fall out of
  the two bullets above.
- Unkeyed commands are unaffected by any of the above (INV-1's default
  path).
- `Command::batch`'s child-key folding needs no restatement: the store
  consumes real `Command` values, so batch has already discarded child
  keys and folded cancels before the store sees the parts (RFC 0003
  INV-11).

These are the deterministic core of RFC 0003 — what may still be
delivered, and when an id releases — restated over the store's pending
set, with keyed-intake reconciliation as the store's analogue of the
runtime's pre-spawn reap-and-sample. The mechanics that exist only
because the runtime is concurrent (stale-exit tokens, INV-8; bounded
bookkeeping, INV-13) have no TestStore counterpart and are deliberately
not modeled.

The residual negative space is the reconciliation instrument itself:
the store's proof of exhaustion is §4.1's single poll per leaf at
intake. A leaf that needs further polls to complete (a self-waking
future mid-completion) reads as still open and keeps the id occupied —
deterministically — while at the runtime's decision point the same
run's task may or may not have exited yet, a scheduling fact the
runtime's reconciliation resolves whichever way it finds. In that
window the store deterministically selects one of the runtime's legal
outcomes; a test pinning a `KeepInFlight` discard there asserts the
store's selection, not a runtime guarantee (§4.2's citation rule
applies).

### 5.2 Redraw directive (RFC 0002)

`redraw_requested()` reports the folded redraw directive of the command
returned by the most recent step (a `send`, or the `update` call inside
a `receive` / `receive_matching`). Before any step completes it reports
the init command's directive. `receive_quit` applies no message and is
not a step: after it, `redraw_requested` keeps reporting the previous
step's directive. This makes `without_redraw` decisions assertable per
transition, which is the granularity RFC 0002 defines them at.

The init-command reading is TestStore-specific introspection and does
*not* predict the production runtime's first render. The runtime
enqueues the init command directly and never consults its redraw
directive; its first frame always renders regardless
(`src/runtime/core.rs`, `src/runtime/pending_work.rs`). So
`redraw_requested()` before the first step exposes the init command's
folded directive as a `Command` property, not as a claim about whether
the runtime would redraw its first frame — the only production redraw
decisions it mirrors are the per-step ones, which are what INV-T3's
"runtime would read" covers (§8).

### 5.3 Quit

Quit is a deliverable output like any message and is asserted explicitly
via `receive_quit` (§3.2). After quit is observed the store mirrors the
runtime's shutdown contract: remaining undelivered output is legally
discarded — the `finish` and drop checks poll nothing and pass
regardless of what remains (the analogue of the shutdown discard
carve-out in RFC 0006's INV-L2), and further `send`/`receive*` calls
fail because the application would no longer be running. A quit that is *suppressed* by cancellation (§5.1) is not
"observed" and triggers none of this — exactly RFC 0003 INV-9.

## 6. Exhaustiveness

Exhaustive assertion is the only stage-1 mode. The rules, by call site:

- **`send`** performs no exhaustiveness check: pending deliverable
  output does not block a `send`, and neither do effects that are
  pending but not deliverable. Leaving deliverable output in place lets
  a test script a `send` that supersedes or cancels an earlier step's
  not-yet-received *keyed* output — the sequence
  `send(Start); send(Cancel)` cancels a keyed effect before its output
  is received — which is exactly the runtime's shared-first schedule:
  a shared input (the next message) is processed ahead of a keyed
  effect's already-ready output (RFC 0003 INV-14, §4.2;
  `src/runtime/app_input.rs`). Ordering a `send` ahead of *unkeyed*
  pending output is **not** a runtime guarantee: unkeyed command output
  travels the shared channel in FIFO order alongside other shared
  traffic (a `send`'s message does not jump it), so a `send` scripted
  before unkeyed output is TestStore's own linearization, not evidence
  of runtime scheduling (§4.2's citation rule). Undelivered output is
  not lost track of either way: it stays subject to the `receive*`,
  `finish`, and drop checks below, which remain exhaustive. `send` still
  fails after an observed quit (§5.3).
- **`receive` / `receive_matching` / `receive_quit`** fail on a
  mismatch, on quit-versus-message confusion, or when nothing is
  deliverable — each with a diagnostic that names the actual value
  (`Debug`) and, in the nothing-deliverable case, distinguishes "no
  pending effects" from "effects pending but not ready".
- **`finish`** (and the drop check, §3.2) fails if quit was not
  observed and either (a) a deliverable message or quit request remains,
  or (b) any pending leaf has not been driven to completion — an
  in-flight effect the test never accounted for is a leak even if it
  never produced a message. After an observed quit, `finish` and the
  drop check poll nothing and pass unconditionally (§5.3) — a
  reactor-dependent leaf legally discarded at quit cannot fail them,
  because §4.3's failure requires a poll.
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
it.** Stage 2 of TestStore — deterministic driving of the
time-dependent *command* effects (`timeout`, retry backoff, and future
command-side coalescing timers such as debounce/throttle) — lands as an
amendment to this document after that RFC is accepted. `Timer` and other
subscription sources are not part of stage 2 (§1.2): they are not
command leaves and TestStore never executes subscription sources.

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
shapes are chosen with it in mind: a store-held controlled time context
(RFC 0009 §3.2 supersedes the earlier "clock handle" phrasing — there
is no clock value to hold), an `advance(duration)` call that makes
time-gated *command* leaves (`timeout`, retry backoff) deliverable, and
the §4.3 limitation for those leaves dissolving into ordinary `receive`
flow. `Timer` and other subscription sources stay out of scope in
stage 2 as in stage 1 (§1.2; RFC 0009 §5.1). Nothing in the stage-1
surface above assumes otherwise or blocks that shape. INV-T10's
construction-time check (§4.3, §8) is stage-1-only for the same reason:
it rejects *every* Tokio runtime context, while stage 2 is meant to run
inside the specific paused single-threaded executor RFC 0009 §3.2 calls
a controlled time context, so the amendment narrows the check rather
than dropping it outright — it will still reject an *unpaused* or
multi-threaded runtime, which RFC 0009 §3.2's controlled time context
already excludes.

## 8. Invariants

Enforcement classes follow the pre-review checklist's definitions
(structural / behavioral / statistical).

- **INV-T1**: `Application`'s definition is unchanged by this RFC —
  `type Message: Send + 'static` and no new bound on any associated
  item. Structural: review of `src/application.rs` against the pre-RFC
  definition. Behavioral: a compile test, added with the
  implementation, instantiates `Application` with a message type that
  implements nothing beyond `Send + 'static`. Two `src/application.rs`
  doctests already do this incidentally — `new`'s `enum Message { Init
  }` and `update`'s `enum Message { Save, Quit }` carry no derives at
  all, so a smuggled bound would already break them — but neither is
  written as a bound-check, and either could grow a derive for
  unrelated doc-readability reasons without anyone noticing the
  coverage had disappeared. The compile test above names the case
  explicitly and does not depend on those two examples' current shape.
- **INV-T2**: TestStore's bounds are exactly §2.1's — `Debug` on the
  store, `PartialEq` on equality-asserting methods only, `Clone` on
  nothing. Behavioral: a compile test drives `new` → `send` → `state` →
  `receive_matching` → `finish` with a message type implementing `Debug`
  but neither `PartialEq` nor `Clone`. Structural: review of every
  public TestStore signature for stray bounds.
- **INV-T3**: TestStore consumes each command through the same
  decomposition boundary the runtime consumes — after the §4.1
  prerequisite refactor, a `RuntimeCommandParts` that carries the
  effect's leaves unfolded in declaration order, folded or driven only
  at each consumer's own site — never a parallel re-derivation of
  directives, cancellation, or effects. Structural, in two parts:
  review of the store's single command-intake site (it accepts the
  parts type and touches no `Command` or `Effect` internals), and
  review of the runtime's spawn site for the prerequisite's
  behavior-preservation half (the relocated fold merges the leaves
  exactly as `into_stream()` does today). This is what makes TestStore
  results evidence about real commands rather than about a test-only
  model.
- **INV-T4**: the store introduces no nondeterminism of its own —
  `send` and `receive*` are synchronous (no task spawn, no executor
  requirement) and polling follows §4.1's fixed budget — so for an
  application whose `update` is deterministic (the store cannot
  contract this for the application; `subscriptions` alone carries a
  purity contract), two executions of one test program observe
  identical state transitions and delivery sequences. Behavioral: a
  repeated-run test over a deterministic application asserts equal
  delivery transcripts across runs of a multi-leaf,
  cancellation-exercising program, and a poll-counting leaf asserts
  §4.1's one-poll-per-reached-leaf budget (a double-polling
  implementation fails it).
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
- **INV-T7**: cancellation parity — the six behaviors of §5.1
  (supersede, keep-in-flight discard, reconciliation release, explicit
  cancel, same-command cancel-then-spawn ordering, unkeyed unaffected)
  hold over the store's pending output as RFC 0003's INV-3, INV-4,
  INV-5, INV-6, INV-7, and INV-9 state them for deliverable output.
  Behavioral: one test per behavior, including
  quit suppression (a superseded keyed quit is never observable via
  `receive_quit`) and the two reconciliation edges: a `KeepInFlight`
  command arriving after the occupant's leaves are exhausted is
  admitted, and one arriving while the reconciliation poll yields a
  buffered item is discarded with that item still deliverable at its
  canonical position. The supersede and explicit-cancel tests use the
  `send`-scripted form the §6 non-blocking `send` enables — a `send`
  carrying a same-id `CancelInFlight` command or a `Command::cancel(id)`
  over a keyed occupant whose output is still pending: the `send` does
  not fail (§6), and the occupant's undelivered output is thereafter
  unobservable via any `receive*` (INV-3, INV-9). This is the
  cancel-before-receive scenario the §6 rationale motivates; INV-T8's
  retention tests deliberately do *not* cancel, since a cancelled
  output cannot be retained. The same-command cancel-then-spawn test
  (RFC 0003 §5.1) `send`s `Command::batch([Command::cancel(id),
  work]).cancellable(id)` over an occupied id and asserts both halves
  at once: the occupant's
  undelivered output is unobservable via any `receive*` (as the
  explicit-cancel test already establishes) *and* a following `receive`
  at `id` yields `work`'s message — an implementation that admits the
  spawn before applying the batch's own cancel fails this test even
  though it could still pass the supersede and explicit-cancel tests in
  isolation, since neither of those combines a cancel and a spawn in one
  command.
- **INV-T8**: exhaustiveness — each leak class in §6 fails at its named
  call site, with a diagnostic naming the leaked values for the
  message classes and the count and enqueue position for the
  unfinished-leaf class (§6). `send` is not a leak-check site: it never
  fails on pending deliverable output (§6). Behavioral: one test per
  leak class (ready message at `finish`; unfinished leaf at `finish`;
  drop-without-finish), each asserting the failure fires *and* its
  message contains the class's required content — the leaked value's
  `Debug` rendering for the message classes — because the wrong-value
  adversary is exactly what the message-content assertion exists to
  fail; plus two negative tests that a `send` issued while a deliverable
  message is still pending does *not* fail and leaves that message
  assertable by a later `receive` — one over a **keyed** occupant's
  output using a *non-cancelling* `send` (a message that does not target
  the occupant's id, e.g. `send(Start); send(Unrelated)`) so the keyed
  output survives: the shared-first ordering is runtime parity (RFC 0003
  INV-14), and the output is then `receive`d — and one over **unkeyed**
  output (which asserts only that `send` does not block on it; the
  ordering is TestStore's linearization, not runtime parity, §6). The
  keyed-only test would pass an implementation that wrongly fails `send`
  on unkeyed pending output; the unkeyed-only test would miss a broken
  keyed shared-first path — so both are required. Each of these two
  tests is duplicated with the pending output sourced from the *init*
  command instead of a step's — an unkeyed init effect surviving a
  first, unrelated `send`, and a keyed init effect surviving a first,
  non-cancelling `send` — for four negative tests total. §3.2's `new`
  rule states the init command's output is subject to the same
  accounting as any step's output; an implementation that special-cases
  the very first `send` to still enforce the retracted "receive init
  output before the first send" reading of the old §3.2 text would pass
  the two step-sourced tests but fail the two init-sourced ones, so both
  origins are required alongside both key classes. The `send`-carried
  supersede/cancel case (where the pending output is *removed*, not
  retained) is INV-T7's, not a retention test.
- **INV-T9**: quit terminality and carve-out — after `receive_quit`,
  `send`/`receive*` fail on the quit state without polling any leaf,
  and the `finish` and drop checks poll nothing and pass regardless of
  remaining output. Behavioral: a test quits with output still
  pending — including a reactor-dependent leaf, which the post-quit
  no-poll rule leaves untouched and which would fail the run if any
  post-quit call polled it (§4.3) — and asserts the failing `send` and
  `receive*` (whose failure is the quit-state diagnostic, not the
  reactor panic polling would produce) and the passing `finish`.
- **INV-T10**: reactor-absence precondition (stage 1 only, §7) —
  `TestStore::new` panics immediately, before returning a store, if
  `tokio::runtime::Handle::try_current()` succeeds, so §4.3's "polling a
  time-dependent leaf fails the test" guarantee cannot silently degrade
  into an unpolled hang inside a Tokio runtime context. Structural:
  review of `TestStore::new` for the check, placed before any other
  construction work. Behavioral: a test constructs `TestStore` from
  inside `#[tokio::test]` and asserts the panic and that its message
  names the precondition (not the generic missing-reactor panic §4.3
  otherwise produces).

Surface–invariant coverage: `new`/`send`/`receive*`/`finish` map to
INV-T2/T3/T4/T8, with `new` additionally covered by INV-T10; `state` is
the pure accessor through which INV-T4's state-transition transcript is
read, covered there; delivery order maps to INV-T5/T6; cancellation
metadata to INV-T7; `receive_quit` and the quit state to INV-T9;
`redraw_requested` and `subscription_ids` are pure observations of
contracts owned elsewhere (RFC 0002's directive; RFC 0005's declared
identity) and are covered by INV-T3 (they read what the runtime would
read) plus one behavioral test each — with two carve-outs. First: the
init-command directive `redraw_requested` reports before the first step
is TestStore-specific `Command` introspection and is *not* a prediction
of the runtime's first render (§5.2), so it falls outside INV-T3's
"runtime would read" umbrella; the behavioral test for it asserts the
init command's folded directive, not a runtime first-frame outcome.
Second: `subscription_ids`'s first-wins dedup (RFC 0005 §3.5) applies,
but the duplicate-ignored warning event does not, since TestStore never
runs a `SubscriptionManager` to emit it (§3.2); the behavioral test for
`subscription_ids` therefore includes a duplicate-declared-ID case
asserting the returned `Vec` keeps only the first occurrence, without
asserting any tracing event. The absence of an `Application` change
maps to INV-T1.

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
- RFC 0003 — command cancellation: its §4.2 pre-spawn reconciliation,
  its §5.1 explicit-cancels-before-keyed-spawn ordering, and INV-1,
  INV-3, INV-4, INV-5, INV-6, INV-7, INV-9, INV-10, INV-11, INV-14
  (cited in §§4.1, 4.2, 5.1, 5.3 of this document).
- RFC 0005 — structural lifecycle identity: `SubscriptionId`, the
  declared-set semantics `subscription_ids` observes.
- RFC 0006 — runtime load control: the shutdown discard carve-out §5.3
  mirrors (INV-L2); the runtime contracts §1.2 excludes.
- RFC 0007 — RuntimeConfig: the prelude-membership reasoning §3.3
  follows.
- RFC 0009 — Clock DI: its §3.2 controlled time context, which §7's
  stage-2 sketch and INV-T10's stage-1 scoping (§4.3, §8) both cite.
- `src/application.rs` — the trait whose bounds §2 pins.
- `src/command/core.rs`, `src/command/runtime_parts.rs` — the
  decomposition boundary INV-T3 names.
- `src/subscription/mock.rs` — the unconditional test-support precedent
  §3.3 cites.
- `docs/rfcs/pre-review-checklist.md` — enforcement-class definitions
  used in §8.
