# RFC 0008: TestStore — deterministic update and effect testing

- Status: Implemented for stages 1–2. Stage 3 (§9) is contract only:
  its surface enters the crate with the reducer-first kernel, after
  RFC 0014 §13.1's open tier closes
- Target: an additive test harness for the current `Application` API:
  pure `update` transitions and immediately ready effects (stage 1),
  plus time-dependent command effects under a store-held controlled
  time context (stage 2); and, beside that store, a driving layer over
  the reducer-first kernel (stage 3)
- Scope: the `Message` trait-bound decision, the exhaustive-assertion
  decision, the `TestStore` public surface, its delivery-order and
  cancellation-parity contracts, the per-leaf `RuntimeCommandParts`
  prerequisite (§4.1), the staging split with Clock DI, the stage-2
  controlled-time contract (`advance`, anchoring, the store-owned
  executor context — §3.2, §4.3, §7), and the stage-3 driving
  surface — `TestDriver`, `ParkProbe`, the grant handshake, and the
  evidence and citation rules whose contract RFC 0014 §7.2 pins (§9)
- Feature flag: none (precedent: `subscription::mock` ships
  unconditionally); stage 2's implementation added tokio's `test-util`
  to the crate's unconditional dependency features per RFC 0009 §5.1's
  decision
- CHANGELOG: `Added` entry ships with the implementation release, not
  with this RFC; stage 3's entry ships with the kernel implementation
  release, which is what RFC 0014's own header states for it

> **Staging.** Stage 1 drives pure `update` transitions and effects
> that become ready without an executor or the passage of time. Stage 2
> — gated on RFC 0009 — adds a store-held controlled time context and
> `advance`, making time-dependent *command* effects (`Command::timeout`,
> retry backoff) deliverable through ordinary `receive` flow (§4.3, §7).
> `Timer`-based subscriptions are not staged here at all: TestStore
> never executes subscription sources (§1.2), so lifting that is a
> separate subscription-execution design, not stage 2. Stage 3 —
> gated on RFC 0014 — is not a store stage: it is a separate driving
> layer beside the store (§1.3, §9) that executes the production
> kernel, subscription sources included, and leaves §1.2's
> non-execution boundary exactly where it is.

## Summary

Three decisions, ordered by urgency:

1. **`Message` boundary** (§2): `Application::Message`'s bounds stay
   exactly `Send + 'static`. `TestStore` carries its own bounds instead —
   `Debug` on the store, `PartialEq` scoped to the equality-asserting
   methods only, and `Clone` nowhere. Retrofitting a bound onto
   `Application::Message` later would be a silent breaking change for
   every existing application, so the placement was pinned before any
   TestStore code existed, and constrains future RFCs (INV-T1, INV-T2).
2. **Exhaustive assertions** (§6): exhaustive is the only mode.
   Undelivered deliverable output — a ready message never
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
   the reverse. RFC 0009 is Implemented; stage 2 consumes
   its contract and resolves the three design inputs its §5.1 records
   (§7).

The harness itself: `tears::testing::TestStore<App>` wraps an
`Application`, applies messages synchronously through `update`, consumes
the returned `Command` through the same decomposition boundary the
runtime uses (§4.1 records the named prerequisite refactor that makes
that boundary carry per-leaf streams), and lets the test assert state,
delivered messages, quit,
declared subscription identity, and the redraw directive — with RFC 0003
cancellation semantics honored on the pending output (§5). Stage 2 adds
`advance`, the store's only time control, over a controlled time
context the store itself owns (§4.3).

## 1. Scope

### 1.1 In scope

- Constructing the store from `Application::new(flags)`, including the
  init command's effects.
- `send`: apply one message through `update`, synchronously, with no
  task spawn.
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
- Time-dependent *command* effects (stage 2): `Command::timeout` and
  retry backoff, deliverable through ordinary `receive` flow once
  `advance` has moved the store's virtual clock to their deadline
  (§3.2, §4.3).

### 1.2 Out of scope

- **Effects needing facilities the controlled context lacks**: the
  store's context enables time only, never I/O (§4.3) — a leaf that
  needs an I/O reactor fails the test at its first poll, exactly as
  stage 1's time leaves did. Real I/O is nondeterministic by nature; a
  test feeds results through test-controlled sources instead.
- **Subscription source execution**: TestStore never starts, polls, or
  restarts a subscription source. It observes the *declared* set only.
  Lifecycle behavior (keyed restart, RFC 0005) stays covered by the
  runtime's own tests. This covers `Timer` and every other
  `SubscriptionSource`: subscription execution is out of scope in
  stage 1 and stage 2 alike — lifting it would be a separate
  subscription-execution design, not the Clock DI work stage 2 covers
  (RFC 0009 §5.1), which delivers command time leaves only.
- **Runtime integration contracts**: channel capacities, backpressure,
  batching, and scheduling (RFC 0006/0007) are properties of the runtime
  event loop, which TestStore replaces. A TestStore run exercises none of
  them, and passing TestStore proves nothing about them — stated here as
  negative space so no later document cites TestStore as evidence for a
  runtime scheduling property.
- **Reducer composition**: TestStore targets `Application` directly, and
  the composition core is RFC 0014's — a reducer-first kernel over which
  `Application` is a single-feature adapter. That RFC discharges the
  no-second-harness obligation on this store's terms: the adapter and
  composed programs are tested through this store's own intake,
  unchanged (its §7.1, §7.3). Designing that core is that RFC's work,
  not this one's; what it adds here is §1.3's delegated layer.

### 1.3 Delegated: the stage-3 driving layer

Stages 1 and 2 never start, poll, or restart a subscription source and
never spawn a task (§1.2); that boundary is unchanged. A third layer
sits beside them rather than inside them: a `TestDriver` that drives the
production kernel itself. Its contract is pinned by RFC 0014 §7.2 —
construction through the production path, with the production task
bookkeeping, lanes, phase machine, and termination shared rather than
re-implemented; a driving differential confined, exhaustively, to two
seams — pass-initiation arbitration and producer send grants — plus
what the application side supplies, which is **inputs and readiness**
(mock sources satisfying RFC 0012 §6.1's template, and test-controlled
gates inside application-supplied effects); scripted determinism over
the whole script — inputs, readiness, arbitration choices, and
grants — for a deterministic application, with the
grant-then-acceptance handshake as the narrower condition under which
*enqueue order* is guaranteed at all, and the whole determinism claim
scoped to its verified range: a current-thread executor and unbounded
lanes, with the bounded-lane extension and its two protocol conditions
open at RFC 0014 §13.3; and **pass-unit driving as the
evidence surface**, one driver step executing one whole production
pass, with stage-granular probes admissible as component-level
instruments but outside that surface. The one boundary no pass-unit
step reaches is the park boundary, where RFC 0014 §7.2 names a
separate instrument, `ParkProbe`, whose observations are evidence for
that RFC's park-and-wake invariant alone — never for the driver's
topology or determinism claims, and not for anything this store's
layers claim. §4.2's citation rule generalizes to both: an order the
driver establishes is never evidence of a production order. §1.2's negative
space is about the store and is unchanged — what each layer claims is
RFC 0014 §7.3's.

The API body — the concrete `TestDriver` surface — is §9: the
additive section RFC 0014 §9 row 11 records and RFC 0012 §6.2
reserves. It expresses RFC 0014 §7.2's contract as API and adds no
driving guarantee beyond it; the surface itself enters the crate when
mainlining closes RFC 0014 §13.1's open tier (§9.1).

The same landing extends this store's command intake: the lowered
parts it consumes gain teardown entries and independently keyed batch
children (RFC 0014 §3.4, §7.1). §9.10 states what that costs this
document.

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
/// Deterministic test harness for an `Application` (RFC 0008). Drives
/// `update`, immediately ready effects, and — via `advance` over a
/// store-held controlled time context — time-dependent command
/// effects, synchronously and without wall-clock waiting.
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
    /// Fails the test on the quit state (§5.3); a keyed-intake
    /// reconciliation poll can additionally surface a leaf's own poll
    /// failure (§4.3).
    pub fn send(&mut self, msg: App::Message);

    /// Advances the store's virtual clock by `duration` (stage 2):
    /// anchors first — every pending leaf without buffered output is
    /// polled exactly once, in enqueue order — then moves virtual time
    /// forward by exactly `duration`, with a timer-driver barrier.
    /// Delivers nothing; output made ready by the advance is observed
    /// at the next check whose scan polls it (§3.2, §4.3). Fails the
    /// test on the quit state (§5.3); the anchoring scan can also
    /// surface a leaf's own poll failure (§4.3), and a `duration`
    /// overflowing the clock's instant range panics.
    pub fn advance(&mut self, duration: Duration);

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
  spawns no task, awaits nothing, and returns only after the
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
- **`advance`** (stage 2) is the store's only time control. It fails on
  the quit state like `send` (§5.3). Otherwise it anchors first, then
  moves time: its **anchoring scan** polls every pending leaf not
  already holding buffered output exactly once, in enqueue order, under
  §4.1's budget — a yield is buffered at the leaf's canonical position,
  exactly as the keyed-intake reconciliation buffers (§5.1) — and only
  then does the controlled context's virtual clock move forward, by
  exactly `duration`, with a **timer-driver barrier**: `advance`
  drives its executor just far enough for the timer driver to process
  the newly reached instant — never far enough to idle awaiting a
  future deadline — because moving the paused clock alone does not fire
  the timer entries already registered against it, and an unfired entry
  reads `Pending` on a later manual poll. The barrier is executor
  progress, not a leaf poll: `advance` delivers nothing and polls no
  *leaf* after the clock moves (§4.1's budget is unchanged), and a leaf
  whose deadline the advance reached is deliverable at the next check
  whose scan polls it — the executor-progress readiness point RFC 0009
  §3.2 left for stage 2 to fix, with no wall-clock waiting.
  Tasks spawned onto the context by earlier effects may receive
  incidental polls while the barrier drives the executor (§4.3's
  negative space). The scan reaches *every* pending leaf and never
  stops early: its job is anchoring (§4.3), not delivery.
  `advance(Duration::ZERO)`
  is legal and anchors without moving time.
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
  `advance`, `receive`, `receive_matching`, and `receive_quit` all
  fail; `state`, `redraw_requested`, `subscription_ids`, and `finish`
  remain callable.
- **`subscription_ids`** calls `Application::subscriptions` and returns
  the declared IDs in declaration order, deduplicated by RFC 0005 §3.5's
  first-occurrence-stable rule: for equal full IDs in the declared list,
  only the first occurrence is kept, at its original position among the
  survivors (`[A, B, A]` → `[A, B]`, never `[B, A]` or any other
  reordering). This is the same *desired-set* `SubscriptionManager::update`
  computes before reconciling — not the set it spawns: `update` leaves an
  already-running id in that set untouched and calls a source's
  `stream()` only for an id newly entering it (`src/subscription.rs`).
  `subscription_ids` performs the same dedup without going anywhere near
  that machinery — it never calls `stream()` on any declared source and
  never constructs or runs a `SubscriptionManager` (§1.2) — so its
  return value predicts the reconciliation *input*, not which ids the
  runtime spawns or which are currently live. For the same reason it
  does not reproduce the
  warning-level tracing event RFC 0005 §3.5 requires of the ignored
  duplicate: that event is `SubscriptionManager::update`'s own side
  effect, never triggered by a call that runs no `SubscriptionManager`
  at all. `SubscriptionId` is already `Clone + Eq + Hash + Debug`, so
  the test asserts on the returned `Vec` directly. This is the
  observation the `subscriptions`-purity contract (`src/application.rs`)
  makes meaningful: the declared set is a pure function of state, so
  asserting it after a `send` is deterministic. INV-T11 (§8) pins the
  dedup rule and the no-side-effect claim as tested contract.
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

**Named prerequisite — per-leaf parts.** At this RFC's drafting that
boundary folded a multi-leaf effect into one stream before the parts
existed: `into_runtime_parts` called `Effect::into_stream()`, which
merged the leaves through an unordered select. A store built on that
parts type could not have implemented §4.2's per-leaf canonical order
without re-deriving the leaves in parallel — exactly what INV-T3
forbids — and relying on the merged stream happening to yield in
declaration order would have rested on a coincidental property of the
select combinator, not on any contract. Stage-1 implementation was
therefore gated on a prerequisite refactor, owned by this RFC's
implementation task and since landed: `RuntimeCommandParts` carries the
effect's leaves unfolded, in `Command::batch`'s flattened declaration
order (`into_runtime_parts` calls `Effect::into_leaves()`,
`src/command/core.rs`, `src/command/runtime_parts.rs`), and each
consumer folds or drives them at its own consumption site — the
runtime merging them at its spawn site with `fold_leaves`, exactly as
the pre-refactor `into_stream()` merged them (a behavior-preserving
relocation of the existing fold; `Effect` already kept its leaves
apart to preserve leaf identity for per-leaf consumers, per its own
comment in `src/command/effect.rs`), the store keeping them apart.
INV-T3 names this revised boundary.

**Deliverability and the poll budget.** A leaf's output is
**deliverable** at a given check when the leaf yields it on that
check's poll, or when an earlier poll under this budget (a keyed-intake
reconciliation, §5.1, or an anchoring scan, §3.2) has
already yielded it into the leaf's buffer — a buffered item is
deliverable at every later check without a further poll. The poll
contract is fixed, because INV-T4's determinism rests on it: polling
happens only inside `receive*`, `finish`, and drop checks, the
keyed-intake reconciliation of §5.1 (a keyed `send`'s only poll, and
only under `KeepInFlight`), and `advance`'s anchoring scan (§3.2). A
bare `send` polls no leaf (§6). The
quit-state check precedes every one of these sites, so after an
observed quit no store call polls at all: `send`/`advance`/`receive*`
fail on the quit state before reaching any leaf, and the `finish` and
drop checks pass without polling (§5.3, §6). Each check polls each leaf
its scan reaches (§4.2) exactly
once, with a waker whose wake-ups are not honored within the call. A
self-waking leaf is therefore re-polled no earlier than the next store
call and cannot loop a check — if it does not yield on its one poll, it
is not deliverable at that check; a leaf made ready between calls by a
test-controlled source (a oneshot the test completed, a
`MockSource`-style seam) is observed at the next check whose scan
reaches it. Between calls the store does nothing.

Stage 2 changes where a poll runs, not when: every poll under this
budget happens inside the store-held controlled time context (§4.3),
so a time-gated leaf finds the paused clock rather than no clock. The
budget above is otherwise unchanged, and between calls the store still
does nothing — in particular, it never lets its executor idle awaiting
a future deadline (`advance`'s driver barrier, §3.2, drives only
through the already-reached instant), so RFC 0009 §3.2's auto-advance
clause never applies to the store's own operations (INV-T12).

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
  is outstanding); `advance`'s anchoring scan walks the same order but
  never stops early (§3.2). `send` runs no such scan (§6). In every
  case, each reached leaf gets exactly one poll (§4.1).
- **Time-made-ready leaves join the same order**: a leaf whose deadline
  an `advance` reached is simply deliverable at later checks, at its
  enqueue position. Two time-gated leaves whose deadlines fall inside
  the same `advance` deliver in enqueue order like any other pair of
  deliverable leaves — the store's linearization supplying the
  equal-deadline order RFC 0009 §3.4 deliberately leaves unspecified,
  so the citation rule below applies to it.
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

### 4.3 Time-gated leaves and the controlled context

The store owns a **controlled time context** in RFC 0009 §3.2's sense:
a single-threaded executor context, constructed by `TestStore::new`,
whose clock starts paused and which enables no I/O driver. Every poll
under §4.1's budget happens inside it; the store exposes no accessor
for it, and in ordinary use the caller neither provides nor enters
it — though effect code polled inside it can capture a handle to it,
which is negative space below, not an accessor. The store's operations
never idle the executor awaiting a future deadline — §4.1's checks
poll and return, and `advance`'s timer-driver barrier (§3.2) drives
only through the already-reached instant — so RFC 0009 §3.2's
auto-advance clause never applies to them: the store's own operations
move virtual time only through `advance` (INV-T12). Like INV-T4's
determinism, that claim scopes to what the store itself does — leaf
polls run *inside* the context, so application code there can reach
the executor's clock facilities directly; what happens then is pinned
as negative space below.

A time-gated *command* leaf — `Command::timeout` wraps every leaf of
its child effect in a deadline; a backoff retry sleeps between
attempts — is therefore an ordinary pending leaf: not deliverable
while virtual now is before its deadline, deliverable at the first
check whose scan polls it once virtual now is at or past the deadline.
This is RFC 0009 §3.2's no-early-firing and readiness contract, plus
its transparency clause: RFC 0004's timeout and retry semantics hold
identically under the virtual clock (RFC 0009 INV-C3), so what a store
test observes about them is evidence about production behavior —
completing the INV-T3 argument on the time axis. (`Timer` is not in
this set: it is a subscription source, not a command leaf, so it never
enters the store's pending command set — §1.2.)

**Anchoring.** RFC 0004 anchors a timeout's deadline at the leaf's
*first poll*, and a backoff sleep starts at the poll that observes the
failed attempt; the store adds only *when* polls happen. Because the
store's own operations move virtual time only inside `advance` (a
clock-manipulating effect voids this whole derivation — negative
space below), and only after its anchoring
scan has seen every pending leaf polled at least once — the scan polls
each leaf without buffered output, and a buffered leaf was already
polled, which is how it buffered (§3.2) — no virtual time can pass
between a leaf's enqueue and its first poll — whichever check's scan
reaches it first, virtual now is still the leaf's enqueue-time now. A
timeout leaf's deadline is therefore its enqueue-time virtual now plus
its declared duration, independent of scan order — the
scan-order-dependence RFC 0009 §5.1 flagged as a design input
dissolves rather than needing per-script reasoning. Waits that start
mid-leaf (a retry's backoff after a failed attempt) anchor at the
virtual now of the scan that observed the failure, which the test's
own script determines.

**Construction check (INV-T10).** `TestStore::new`
checks `tokio::runtime::Handle::try_current()` and panics immediately,
with a diagnostic naming the precondition, if any runtime context is
entered — `#[tokio::test]` included, paused or not. The only
controlled context the store accepts is its *own*, so the caller-facing
rule is: construct the store on a plain `#[test]`, never inside
`#[tokio::test]`. An ambient runtime is rejected rather than adopted
because the store cannot verify an ambient runtime's pausedness or
thread model through any public surface, and because driving the
store's own context from inside another runtime would block a runtime
thread, which Tokio forbids. The check is structural and happens once,
at construction: a store built outside a runtime but later driven from
inside one is a misuse this RFC does not attempt to catch.

**Outside the store's contract.** A user effect that spawns onto the
ambient executor (`tokio::spawn` from inside a leaf's poll) finds
a context and succeeds, but the spawned task is outside the store's
pending set: it is not counted in exhaustiveness (§6), and the store
gives it no specified schedule — it may receive incidental polls
whenever the store drives its executor (the `advance` barrier, §3.2),
so it can progress or even complete, but neither the occurrence nor
the extent of that progress is contract. A test
whose observable delivery depends on such a task's progress is outside
INV-T4's determinism scope — the store contracts only its own polls,
the same scoping INV-T4 already applies to a nondeterministic
`update`. The same boundary covers the clock itself: a leaf's poll
runs inside the store's paused context, so effect code that calls
`tokio::time::resume` or an in-effect `tokio::time::advance` there
succeeds, and unpauses or moves the very clock the store schedules
against. The store does not detect or defend against this; every
time-related claim in this document (INV-T12, INV-T13, and the §4.3
deliverability contract) is scoped to the store's own operations and
is void for a test whose effects manipulate the clock. An effect can
likewise capture `tokio::runtime::Handle::current()` during its poll
and smuggle the handle out; everything reached through such a handle —
entering the context from outside the store's calls, spawning onto it,
driving it, manipulating its clock — is this same negative space.
`tokio::time::pause` is not in the silent set: pausing an
already-paused clock panics, so an effect calling it fails loudly — as
does constructing a nested runtime inside a leaf's poll and blocking
on it (Tokio forbids blocking a runtime thread).

**I/O leaves.** The controlled context enables time only, so a leaf
that needs an I/O reactor keeps stage 1's honest failure: its first
poll fails the test with the underlying missing-reactor panic, at the
first store call whose scan polls it — an `advance`, a `receive*`
aimed at a different message, a `finish`/drop check, or a keyed-intake
reconciliation poll — never a bare `send`, which polls no leaf (§6).
From its enqueue onward the store is effectively poisoned for those
calls unless a scan stops at an earlier-enqueued deliverable leaf
first (an `advance`'s anchoring scan never stops early, §3.2) or a
`send`-issued cancellation removes the leaf (§5.1) before a scan
reaches it. A leaf that is merely *pending* on a test-controlled
source or a not-yet-reached deadline fails nothing: it is skipped by
the canonical order until it becomes deliverable, and only `finish`
holds it to account (§6).

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
  lets a `CancelInFlight` command supersede an occupant whose poll
  would fail the test without polling it — in stage 1 that made
  cancelling a time-dependent keyed leaf expressible at all; under
  stage 2 the same escape hatch remains for I/O-dependent leaves
  (§4.3). Any poll that is issued follows §4.1's budget.
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
directive at the first frame: the first render starts out eligible
unconditionally, and when it happens it renders regardless of that
directive — its occurrence itself is not promised (RFC 0011 §3.2)
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
carve-out in RFC 0006's INV-L2), and further `send`/`advance`/`receive*`
calls fail because the application would no longer be running. A quit that is *suppressed* by cancellation (§5.1) is not
"observed" and triggers none of this — exactly RFC 0003 INV-9.

## 6. Exhaustiveness

Exhaustive assertion is the only mode. The rules, by call site:

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
- **`advance`** performs no exhaustiveness check either: its anchoring
  scan buffers what it happens to yield and fails nothing of its own
  (§3.2) — §4.3's I/O-leaf failure still fires if the scan polls such
  a leaf, and, like `send`, `advance` still fails after an observed
  quit (§5.3).
- **`finish`** (and the drop check, §3.2) fails if quit was not
  observed and either (a) a deliverable message or quit request remains,
  or (b) any pending leaf has not been driven to completion — an
  in-flight effect the test never accounted for is a leak even if it
  never produced a message. A time-gated leaf the test never advanced
  to its deadline is exactly such an unfinished leaf: exhaustiveness
  makes declared time effects part of the accounting. After an observed
  quit, `finish` and the drop check poll nothing and pass
  unconditionally (§5.3) — an I/O-dependent leaf legally discarded at
  quit cannot fail them, because §4.3's failure requires a poll.
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
command-side coalescing timers such as debounce/throttle) — gates on
RFC 0009, which is Implemented. `Timer` and other
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

Stage 2 consumes RFC 0009's contract and resolves the
three design inputs RFC 0009 §5.1 records for it:

- **Deadline anchoring.** RFC 0004's first-poll anchor, restated over
  the store's scan sites: because `advance` anchors before it moves the
  clock and no other store operation moves the clock at all (INV-T12's
  scope; clock-manipulating effects are §4.3's negative space), a leaf's
  first poll
  always happens at its enqueue-time virtual now, so the
  scan-order-dependence RFC 0009 §5.1 flagged dissolves (§4.3).
- **Advance semantics and executor context.** `advance` carries a
  timer-driver barrier: after moving the clock it drives its executor
  until the timer driver has processed the newly reached instant —
  never idling toward a future deadline — because moving the paused
  clock alone leaves already-registered timer entries unfired and
  `Pending` under the store's manual polls. Readiness is then observed
  at the next check whose scan polls the leaf (§3.2), fixing the
  executor-progress question RFC 0009 §3.2 left to stage 2;
  `advance` still polls no leaf after the clock moves. Tasks spawned
  onto the context may receive incidental polls during the barrier —
  unspecified progress, §4.3's negative space.
  The store owns its controlled time context outright; every ambient
  runtime is rejected at construction (§4.3). User effects that spawn
  tasks, and nested runtimes, are outside the store's contract (§4.3).
- **Feature availability.** Per RFC 0009 §5.1's decision, the
  implementation task added `test-util` to the crate's unconditional
  `tokio` dependency features; RFC 0009 INV-C4 carries the load-path
  regression check that covered the flip, and this document adds no
  second check for it.

`Timer` and other subscription sources stay out of scope in stage 2, as
in stage 1 (§1.2; RFC 0009 §5.1).

Excluded claims, recorded per the checklist's minimal-contract item: a
claim that `advance(Duration::ZERO)`'s barrier re-fires
reached-but-unfired timer entries is not made — under the store's own
operations every clock move carries its own barrier, so such an entry
cannot arise, and the claim would rest on unverified `advance(0)`
behavior; a
same-poll readiness guarantee ("deliverable on the poll immediately
after `advance`") is deliberately not claimed — RFC 0009 §3.2 pins
readiness without wall-clock waiting and no fixed observing poll, and
the store's §4.2 scan already determines which check observes it; and
no store-level invariant restates RFC 0004's timeout/retry semantics —
RFC 0009 INV-C3's transparency carries them onto the virtual clock, and
INV-T12's behavioral tests exercise them through the store without
re-proving them.

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
  behavior-preservation half (the relocated fold, `fold_leaves`, merges
  the leaves exactly as the pre-refactor `into_stream()` did). This is
  what makes TestStore
  results evidence about real commands rather than about a test-only
  model.
- **INV-T4**: the store introduces no nondeterminism of its own —
  `send`, `advance`, and `receive*` are synchronous (no task spawn, no
  wall-clock waiting) and polling follows §4.1's fixed budget — so for
  an application whose `update` is deterministic (the store cannot
  contract this for the application; `subscriptions` alone carries a
  purity contract) and whose effects do not depend on spawned-task
  progress (§4.3), two executions of one test program observe
  identical state transitions and delivery sequences. Behavioral: a
  repeated-run test over a deterministic application asserts equal
  delivery transcripts across runs of a multi-leaf,
  cancellation-exercising program, and a poll-counting leaf asserts
  §4.1's one-poll-per-reached-leaf budget (a double-polling
  implementation fails it), including `advance`'s anchoring scan (one
  poll per pending leaf not holding buffered output, and no leaf poll
  after the clock moves — INV-T13's budget half; the driver barrier is
  executor progress, not a leaf poll, §3.2).
- **INV-T5**: one leaf's messages are delivered in stream order.
  Behavioral: a multi-message `Command::stream` test.
- **INV-T6**: across leaves, delivery follows §4.2's canonical order
  (earliest-enqueued deliverable leaf first), and this order is
  TestStore's contract only — no test or document may cite it as a
  runtime ordering guarantee, which the runtime does not make.
  Behavioral for the order itself: a batch of ready leaves delivers in
  declaration order; a leaf made ready late delivers after an
  earlier-enqueued ready leaf but before a later one; and two
  time-gated leaves whose deadlines fall inside one `advance` deliver
  in enqueue order — §4.2's equal-deadline linearization, the store's
  own supply for RFC 0009 §3.4's negative space. The negative-space
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
  `send`/`advance`/`receive*` fail on the quit state without polling
  any leaf,
  and the `finish` and drop checks poll nothing and pass regardless of
  remaining output. Behavioral: a test quits with output still
  pending — including an I/O-dependent leaf, which the post-quit
  no-poll rule leaves untouched and which would fail the run if any
  post-quit call polled it (§4.3) — and asserts the failing `send`,
  `advance`, and
  `receive*` (whose failure is the quit-state diagnostic, not the
  reactor panic polling would produce) and the passing `finish`.
- **INV-T10**: ambient-runtime rejection —
  `TestStore::new` panics immediately, before any other construction
  work — its own controlled context included — if
  `tokio::runtime::Handle::try_current()` succeeds. The store's own
  controlled context must be the only executor context in play: an
  ambient runtime's pausedness and thread model are unverifiable from
  the store, its clock is not the store's clock (RFC 0009 §3.4: no
  contract spans contexts), and driving the store's context from
  inside another runtime would block a runtime thread (§4.3).
  Structural: review of `TestStore::new` for the
  check, placed before any other construction work. Behavioral: a test
  constructs `TestStore` from inside `#[tokio::test]` and asserts the
  panic and that its message names the precondition.
- **INV-T11**: subscription-declaration observation —
  `subscription_ids` returns RFC 0005 §3.5's first-occurrence-stable
  dedup of `Application::subscriptions()`'s declared list (duplicates
  collapse to their first occurrence, at that occurrence's original
  position — `[A, B, A]` → `[A, B]`, never `[B, A]`), and produces it
  without calling `stream()` on any declared source or otherwise
  constructing or running a `SubscriptionManager` (§1.2, §3.2). The
  returned `Vec` is the reconciliation *input* `SubscriptionManager::update`
  would compute, not a prediction of which ids it spawns or already has
  running — `update` leaves an already-running id untouched and calls
  `stream()` only for one newly entering the set
  (`src/subscription.rs`). Structural: review of `subscription_ids`
  for the absence of any `SubscriptionManager` construction or
  `Subscription::spawn`/`stream()` call. Behavioral: a duplicate-ID test
  asserting `[A, B, A]` dedups to `[A, B]`, not `[B, A]` or any other
  order — ruling out a last-occurrence or resorted implementation; a
  `MockSource`-style test asserting its `stream()` constructor is never
  invoked by a `subscription_ids` call; and, since §3.2 keeps the
  no-warning claim as contract, a `tracing` capture asserting zero
  `target: "tears::subscription"` duplicate-ignored events fire from a
  `subscription_ids` call over a duplicate-ID declaration.
- **INV-T12**: controlled-context ownership and explicit-only time —
  the store constructs and owns a controlled time context (RFC 0009
  §3.2: single-threaded, clock started paused, no I/O driver), every
  poll under §4.1's budget happens inside it, and the store's own
  operations move its virtual time only through `advance`, by exactly
  the requested duration — the store never idles its executor awaiting
  a future deadline (`advance`'s driver barrier drives only through
  the already-reached instant, §3.2), so
  RFC 0009 §3.2's auto-advance clause never fires under the store's
  own operations (RFC 0009 INV-C2's non-idling controller). The
  barrier's *presence* is enforced by this invariant's behavioral
  tests — a missing barrier leaves a reached deadline `Pending` at the
  next scan, failing the deliverable-at-the-deadline assertions
  below — but those tests cannot pin its *location*: an implementation
  driving the executor from the scan side instead would pass them, so
  the barrier-inside-`advance` decision (§3.2, resolving RFC 0009
  §5.1's design input) is checked structurally. The store
  cannot contract this for application code: a leaf's poll runs inside
  the context, and an effect that reaches the executor's clock
  facilities there is §4.3's negative space, outside every
  time-related claim — the same scoping INV-T4 applies to a
  nondeterministic `update`. Structural: review of
  `TestStore::new` for the context's construction (paused,
  current-thread, time-only), of `advance` for the barrier as the
  store's only executor-driving site, and of the store for the absence
  of any executor-idling site outside its poll and advance mechanics.
  Behavioral: a `Command::timeout` leaf enqueued ahead of a
  test-controlled leaf with several ready messages stays pending
  across repeated `receive`s of those messages — each scan gives the
  timeout leaf one poll on its way to the deliverable leaf, without
  any call failing — and across `advance`s summing to less than its
  duration, becomes deliverable once cumulative advances reach its
  deadline, and a retry command with a non-zero backoff delivers its
  retried outcome only after an advance spanning the backoff — all
  timed by scripted advances, never by wall-clock waiting (the
  transparency half is RFC 0009 INV-C3's, not re-proven here; these
  tests exercise it through the store).
- **INV-T13**: anchoring — `advance` polls every pending leaf not
  holding buffered output exactly once, in enqueue order, *before*
  moving the clock, and delivers nothing; combined with the store's
  own operations moving the clock only through `advance` (INV-T12's
  scope, clock-manipulating effects excluded), a leaf's first poll
  always happens at
  its enqueue-time virtual now, so a timeout leaf's deadline is that
  now plus its declared duration regardless of scan order (§4.3).
  Behavioral: (a) `send` a `timeout(d)` command, `advance(d)`, and
  `receive` the timeout's message — deliverable exactly at the
  deadline; (b) `advance(x)` first, then `send` a `timeout(d)`
  command, then `advance` just short of `d` — a scan reaching the leaf
  still finds it pending, and a final `advance` covering the remainder
  makes it deliverable to the following `receive`, failing an
  implementation that anchors deadlines at
  store construction instead of the leaf's first poll; (c) the
  poll-count half lives in INV-T4's budget test.

Surface–invariant coverage: `new`/`send`/`receive*`/`finish` map to
INV-T2/T3/T4/T8, with `new` additionally covered by INV-T10 and
INV-T12 (the context it constructs); `advance` maps to INV-T12
(explicit-only time movement), INV-T13 (anchoring), and INV-T4's
budget; `state` is
the pure accessor through which INV-T4's state-transition transcript is
read, covered there; delivery order maps to INV-T5/T6, including
time-made-ready leaves (§4.2); cancellation
metadata to INV-T7; `receive_quit` and the quit state to INV-T9.
`redraw_requested` is a pure observation of a contract owned elsewhere
(RFC 0002's directive) and is covered by INV-T3 (it reads what the
runtime would read, and directives are named in INV-T3's own text)
plus one behavioral test, with one carve-out: the init-command
directive `redraw_requested` reports before the first step is
TestStore-specific `Command` introspection and is *not* a prediction of
the runtime's first render (§5.2), so it falls outside INV-T3's
"runtime would read" umbrella; the behavioral test for it asserts the
init command's folded directive, not a runtime first-frame outcome.
`subscription_ids` is a pure observation of a contract owned elsewhere
(RFC 0005's declared identity) but is *not* an INV-T3 case — INV-T3
governs `Command` decomposition only, and subscriptions never pass
through it — so it gets its own invariant, INV-T11, covering the dedup
rule, the no-side-effect claim, and the no-warning claim together
(§8). The absence of an `Application` change maps to INV-T1.

## 9. The stage-3 driving layer

RFC 0014 §7.2 pins this layer's contract. This section is its API
body: the surface that expresses that contract, and — as much of the
contract as a surface can carry — the shapes it leaves
unconstructible. What it adds is the surface itself: which
constructors exist, which do not, and the rules a caller reads off
them. It restates none of RFC 0014's invariants and adds no driving
guarantee beyond RFC 0014 §7.2, which it neither narrows nor widens.

### 9.1 Placement, gating, and the boundary it does not move

- **Placement.** `src/testing/driver.rs`, paths
  `tears::testing::TestDriver` and `tears::testing::ParkProbe`.
  §3.3's rules carry over unchanged: no crate-root re-export, no
  prelude membership, no feature flag.
- **Gating.** This surface is contract, not code. It enters the crate
  with the reducer-first kernel, after RFC 0014 §13.1's open tier
  closes, and its `Added` CHANGELOG entry ships with that release —
  the same form RFC 0014's own header states for it. The crate's own
  types in §9.3 divide in three, and no statement here asserts that
  anything in the first or third division exists in the crate today.
  The partition covers those and nothing else: `Backend` and
  `ratatui::Terminal` belong to the host UI library, and `Future`,
  `Pin`, and `Poll` to `std`, so neither group is this crate's to
  place.
  - **Introduced here**, arriving with that landing: `TestDriver`,
    `ParkProbe`, `WakeSource`, `Origin`, `AnonymousRun`, `StepReport`,
    `GrantToken`, `Confirmed`, `GrantOutstanding`, `NotReady`,
    `AcceptanceLedger`, `IntentLedger`.
  - **Existing and unchanged**: `CommandId` and `SubscriptionId`,
    RFC 0005's identity types.
  - **Entering or changing in the same landing**: `Program` and
    `Exit`, which RFC 0014 §2.1 and §2.3 introduce, and
    `RuntimeConfig`, which that landing revises — it loses the
    frame-rate field and `keyed_channel_capacity`, and
    `app_channel_capacity` becomes `data_lane_capacity` with no alias
    (RFC 0014 §9 rows 2 and 4). `TestDriver::new`'s `config`
    parameter is that revised type, never today's.
- **The store's boundary is unmoved.** Stages 1 and 2 keep §1.2's
  non-execution boundary exactly: the store still never starts,
  polls, or restarts a subscription source and never spawns a task,
  and no store method gains executing behavior from this section. The
  driver sits beside the store, not inside it (§1.3), and RFC 0014
  §7.3 keeps what each layer claims apart.

### 9.2 Same topology: the five shapes with no constructor

RFC 0014 §7.2's same-topology clause (INV-RC13) requires the driver
to construct through the production path and to share task
bookkeeping, producer execution, lanes, the phase machine, and
termination with production, with five shapes not constructible from
its API. Stated over this surface:

- **Construction.** `TestDriver::new` takes the inputs the production
  entry point takes — the program, its flags, and a `RuntimeConfig`
  (RFC 0014 §2.3) — and owns a terminal to render into, producing
  that runtime. There is no reduced kernel, no alternative
  construction path, and no test-only wiring parameter. Construction
  is inert, as it is for both production entry points (RFC 0014 §6.1;
  RFC 0011 INV-LC3): nothing starts until `boot`. The driver is also
  that kernel instance's sole driver, its driving methods taking
  `&mut self` — RFC 0011 INV-LC9's exclusivity property, which §6 of
  that RFC states a step-style surface must preserve.
- **Manual run retention** has no constructor: no method returns a
  run, task, or join handle, and none accepts one. A test *names* a
  run (§9.4) and never holds one — the opaque name §9.4 mints is a
  name, not a handle to the run — so it cannot keep a run alive past
  the kernel's own bookkeeping.
- **Reimplemented reconciliation** has no constructor: no method
  applies a cancel, a teardown, or a keyed admission decision. Those
  reach the kernel only as the lowered parts of a command the
  application returned, exactly as in production.
- **A mirrored quit route** has no constructor: no method terminates
  the driven program. Quit reaches the kernel only through the two
  production routes (RFC 0014 §3.3).
- **Manual effect polling** has no constructor: no `TestDriver`
  method takes an effect, a leaf, or a future to poll, and none
  yields a run's future either, so a producer future is not
  obtainable from this surface at all. Effects run as production
  producer tasks on the executor, and what the driver releases is a
  send-intent such a task has already produced (§9.6).
  `ParkProbe::poll` is the one polling entry point this section has,
  and it does not reopen the shape. What closes it is
  unobtainability: the shape needs a *runtime* producer's future, and
  nothing in this section hands one out, so there is nothing of that
  kind to pass. A test can of course poll a future it built itself,
  which observes its own object and nothing about the kernel. The
  type bound is the second line rather than the first, and it is
  narrower than a universal: a producer future does not in general
  carry the driving future's output, `Result<Exit, E>` — across
  RFC 0011 INV-LC8's producer inventory as RFC 0014 §6.1 extends it,
  a keyed command run, an anonymous command run, and a subscription
  run all yield messages, and a cleanup run yields `()` (RFC 0014
  §4.4) — though an application that deliberately gave its message
  type that shape would coincide with it. INV-RC13's structural
  review confirms the finding at the API surface.
- **Direct kernel injection** has no constructor: no method enqueues
  onto the data lane or the control lane, and none hands the kernel
  an item to deliver. What travels either lane is producer output,
  which is the kernel's own contract (RFC 0014 §3.1).

What the surface supplies instead is the two seams and nothing else:
which ready wake source begins a pass (§9.5), and the release of a
producer's send-intent (§9.6). Inputs and readiness come from the
application side — sources conforming to RFC 0012 §6.1's template,
and test-controlled gates inside application-supplied effects — never
from a driver method.

### 9.3 The API body

```rust
/// Stage-3 driving harness: constructs the production runtime
/// through its own entry point and drives the production kernel one
/// whole pass at a time (RFC 0014 §7.2).
pub struct TestDriver<P: Program, B: Backend> { /* private */ }

impl<P: Program, B: Backend> TestDriver<P, B> {
    /// Inert construction from the production entry point's inputs
    /// (RFC 0014 §2.3), owning a terminal to render into.
    #[must_use]
    pub fn new(
        program: P,
        flags: P::Flags,
        config: RuntimeConfig,
        terminal: ratatui::Terminal<B>,
    ) -> Self;

    /// Runs the production bootstrap through to a parked kernel
    /// (§9.5): the intake order, then the continuation pass that
    /// consumes the pending first render.
    pub fn boot(&mut self) -> StepReport<B::Error>;

    /// Executes one whole production pass — RFC 0014 §3.5's four
    /// stages in their fixed order — begun by `woken_by`. Drives
    /// nothing and returns `Err(NotReady)` when that source has not
    /// arrived: readiness is read from the production sources, never
    /// scripted (§9.5).
    pub fn step_pass(
        &mut self,
        woken_by: WakeSource,
    ) -> Result<StepReport<B::Error>, NotReady>;

    /// Arms a grant at `origin`, releasing that origin's next
    /// send-intent. The returned token borrows neither the driver
    /// nor the script. At most one grant is outstanding across the
    /// whole driver; the next — at this origin or any other — is
    /// admitted only after this one resolves (§9.6).
    pub fn grant(
        &self,
        origin: Origin,
    ) -> Result<GrantToken, GrantOutstanding>;

    /// Consumes `token`, driving the executor — beginning no pass —
    /// until the released send commits or is reclaimed, and reports
    /// which of the two ended it (§9.6).
    pub fn confirm(&mut self, token: GrantToken) -> Confirmed;

    /// Drives the executor — beginning no pass and releasing no
    /// send-intent — so that runs which send nothing can reach their
    /// exits (§9.6).
    pub fn settle(&mut self);

    /// Sends admitted past the gate, tagged with origin and lane,
    /// in gate order. Admission, not delivery (§9.6).
    pub fn accepted(&self) -> AcceptanceLedger;

    /// Send-intents recorded before the gate, origin-tagged, under
    /// no ordering or completeness guarantee (§9.6).
    pub fn intents(&self) -> IntentLedger;
}

/// What a step started, and whether it terminated the program.
pub struct StepReport<E> {
    /// The runs this step started, in the order it started them.
    pub started: Vec<Origin>,
    /// Present exactly when the step terminated the program,
    /// carrying the production result (RFC 0014 §2.3, INV-RC11).
    pub terminated: Option<Result<Exit, E>>,
}

/// The sources a parked kernel arms — INV-RC16's set, exactly, and
/// the whole scripting vocabulary of the first seam (§9.5).
pub enum WakeSource {
    /// An item enqueued for delivery on the data lane.
    Data,
    /// A producer-originated quit on the control lane.
    Control,
    /// A producer-exit or subscription-quiescence notification.
    ProducerExit,
}

/// A producer run, named by the identity it already has (§9.4).
pub enum Origin {
    Keyed(CommandId),
    Subscription(SubscriptionId),
    Anonymous(AnonymousRun),
}

/// Driver-side name for an anonymous run: opaque, minted when the
/// driver observes the run start (§9.4).
pub struct AnonymousRun(/* private */);

/// One outstanding grant, correlated to the commit it releases.
pub struct GrantToken { /* private */ }

/// How a released send ended (§9.6). The two are disjoint and
/// exhaustive; both clear the outstanding grant, and only `Accepted`
/// appends to the guaranteed sequence.
#[must_use]
pub enum Confirmed {
    /// The send committed: it was admitted past the gate. Whether
    /// its run is revoked is a separate, delivery-side question.
    Accepted,
    /// The send's reservation was released without committing —
    /// the producer reclaimed it before admission.
    Reclaimed,
}

/// A grant is already outstanding on this driver.
pub struct GrantOutstanding;

/// The scripted wake source had not arrived; nothing was driven.
pub struct NotReady;

pub struct AcceptanceLedger { /* private */ }
pub struct IntentLedger { /* private */ }

/// Park-boundary instrument. Evidence for INV-RC16 only (§9.7).
pub struct ParkProbe { /* private */ }

impl ParkProbe {
    /// Polls the production driving future with this probe's own
    /// waker. The bound admits that future's output shape and no
    /// producer's (§9.2).
    pub fn poll<E, F>(&self, future: Pin<&mut F>)
        -> Poll<Result<Exit, E>>
    where
        F: Future<Output = Result<Exit, E>>;

    /// The sources the parked loop registered this probe's waker
    /// with, on the poll that parked it.
    pub fn armed(&self) -> Vec<WakeSource>;

    /// The arrival that woke the parked loop, once one has.
    pub fn woken_by(&self) -> Option<WakeSource>;

    /// Wake-ups this probe's waker has received.
    pub fn wakes(&self) -> usize;
}
```

The block is normative for **what exists and what does not**: §9.2's
absent constructors, `WakeSource`'s membership as the whole
pass-initiation vocabulary (§9.5), `grant`'s detached token and its
driver-wide admission rule (§9.6), and the ledgers' division at the
send gate (§9.6). Spellings — parameter forms, accessor names,
whether a report field is a slice or an iterator, what a probe's
constructor looks like — are implementation latitude, exactly as for
§3.1's block.

**Waiting, in full.** Every wait this layer performs is a bounded
number of executor turns: no method of this section sleeps, arms a
timer, or reads a wall clock, and the kernel they drive reads no wall
clock either (RFC 0014 §6.3). Exhausting a bound fails the test with
a diagnostic rather than waiting longer; the bound's value is
mechanism. Application-supplied effects sit outside that quantifier,
as they sit outside INV-T4's determinism scope — an effect that
sleeps times its own test.

**Executor turns are not selective, and this is a class fact about
the four calls that turn the executor.** `boot`, `step_pass`,
`confirm`, and `settle` all turn it, and a turn advances whatever is
runnable rather than a run the caller has in mind. (`grant` is a
driving call but not one of the four: it arms the gate and turns
nothing.) Two consequences hold for all four alike: any producer a
turn advances to a send point presents a send-intent, so the **intent
ledger may gain entries during any of them**; and the **guaranteed
sequence gains an entry only when a released send commits**, which
requires a send released through an armed grant, so no call appends
to it except by that route (§9.6). What separates
`settle` from the other three is purpose, not effect: it is the call
whose whole job is to supply turns, under a stated budget and
completion condition, where the others produce turns as a by-product
of the work they contract for.

**The driver's states and what is callable in each.** Three states:
*constructed* (after `new`), *running* (after `boot` returns without
a termination), and *terminated* (once any `StepReport` carries
`terminated`). The usual order is constructed → running →
terminated, with one direct edge: an init command carrying
`Command::quit()` terminates during `boot` (§9.5), so constructed →
terminated is reached without ever passing through running.

| Call | constructed | running | terminated |
| --- | --- | --- | --- |
| `boot` | legal | misuse | misuse |
| `step_pass`, `confirm` | misuse | legal | misuse |
| `grant`, `settle` | misuse | legal under §9.6 | misuse |
| `accepted`, `intents` | legal, empty | legal | legal |

The third row's condition is §9.6's grant lifecycle, which is where
those two calls' legality is stated in full: `grant` is misuse at an
origin the kernel's run bookkeeping does not hold, and `settle` is
misuse while a grant is outstanding.

Misuse **fails the test** rather than returning an error, in the
store's own style (§5.3), and the observation calls stay callable
throughout, as `state` and `finish` stay callable in the store's quit
state. The two error types are not misuse: `NotReady` reports a
production fact (§9.5) and `GrantOutstanding` a script-order fact
(§9.6), and both leave the driver untouched.

### 9.4 Naming a producer run

An `Origin` is a producer run's name, and the driver introduces **no
second identity model**. A keyed run is named by its `CommandId` and
a subscription run by its `SubscriptionId` — RFC 0005's identity
types, unchanged. An anonymous run has no logical key by decision
(RFC 0013 §9's third resolution; its §10 rejects auto-keying), so it
is named by an opaque handle the *driver* mints, under three rules:

- **Minted from an observation.** The driver mints a handle when it
  observes the run start, and reports it in that step's `started`
  list. An anonymous run is nameable only after the kernel has
  started it; there is no way to name one in advance, and nothing
  about the run is chosen by the test.
- **It reaches no kernel identity surface.** The handle is not a key
  and participates in no keyed semantics: no keyed capacity, no move
  into the keyed gauge count — an anonymous run stays counted as an
  anonymous run, under `unkeyed_commands` (RFC 0014 §9 row 9) — and
  no admission or cancellation decision reads it. The kernel's own
  identity for an
  anonymous run — kernel-side scope membership without a logical key,
  RFC 0013 §9's third resolution — is unchanged by its existence, so
  the auto-keying RFC 0013 §10 rejected stays rejected.
- **Its only uses are naming.** It names an origin to `grant` and
  tags ledger records (§9.6). The send gate `grant` releases at is a
  kernel-side seam, RFC 0014 §7.2's second driving differential, not
  a driver-side queue; what crosses the grant boundary into the
  kernel is the run identity the kernel already holds, which the
  driver resolves the handle to there. The handle itself stops at
  that boundary.

`started` lists the runs in the order the step started them, which is
what lets a test name a specific run when one step starts several.
Like every order the driver establishes, it is not evidence of a
production order (§9.9).

### 9.5 Pass initiation: one vocabulary, `WakeSource`

`step_pass(woken_by)` is the first of RFC 0014 §7.2's two driving
seams — which ready wake source begins the next pass — whose
production implementation is the unbiased selection RFC 0014 §3.5
makes normative. `WakeSource` is the seam's whole vocabulary, and it
is INV-RC16's armed set exactly: data-lane readiness, control-lane
arrival, and producer-exit or subscription-quiescence notification.
Three members, no fourth. Two facts bound the seam:

- **Readiness is not scripted.** The driver reads readiness from the
  production sources; a source that has not arrived returns
  `Err(NotReady)` and drives nothing. The script chooses *among
  arrived sources*, which is what the production selection site
  chooses among — the seam replaces the choice, never the facts it
  chooses between.
- **A step is a whole pass.** One `step_pass` runs all four of
  RFC 0014 §3.5's stages in their fixed order. No method runs one
  stage (§9.9).

**There is no fourth reason a pass begins, and the surface offers
none.** A pending frame is not one: RFC 0014 §3.5's stage 4 consumes
a pass's own redraw and dirt inside that same pass, so in steady
state no frame work survives a pass to start the next, and RFC 0014
§10 records the frame step's exclusion from the scripted set for the
same reason — it is consumed inside the pass that marks its work. A
type member for it would be dead surface, and a live one would widen
the first driving differential past the wake sources RFC 0014 §7.2
confines it to.

**Bootstrap is where a pending frame does exist, and `boot` carries
it.** RFC 0011 §3.2's intake order — init dispatch, then the initial
subscription reconcile, then the first render pending
unconditionally — leaves the kernel with work outstanding, so
INV-RC16's park condition ("nothing to make progress on") is not met
and the kernel does not park. `boot` therefore runs that intake *and*
the continuation pass that consumes the pending render. Absent a
termination it returns with that render consumed and no lane item
outstanding — no grant has released a send (§9.6) — so the kernel
parks unless the application side has already produced an exit to
notify, which is an ordinary `WakeSource::ProducerExit` the next step
names. Production reaches the same state by the same route; the
driver adds no step production does not take, and the seam above
lands exactly where it belongs — the choice, from a parked kernel,
among the three sources that can wake it.

Two consequences worth stating. First, the bootstrap arbitration
RFC 0014 §6.2 narrows to the init effect's output, initial
subscription output, and the first render is **not observed** by the
driver: no producer output reaches either lane before a grant
releases it (§9.6), so what remains of that arbitration during `boot`
is the render alone, which the continuation pass consumes. The driver
therefore witnesses no arbitration here and cites none — the citation
rule (§9.9) needs no exception for bootstrap. Second, an init command
carrying `Command::quit()` terminates during the init dispatch,
before the initial reconcile and before any render (RFC 0014 §6.2),
so `boot` returns a `StepReport` whose `terminated` is set and whose
continuation pass never runs — the row INV-RC11 carries.

### 9.6 The grant handshake, `settle`, and the two ledgers

The second seam is the send gate, whose production implementation is
immediate release. `grant(origin)` arms a release at one origin; the
returned `GrantToken` correlates to the commit that release produces,
and `confirm` consumes it once that send's acceptance — the post-send
acknowledgement — is confirmed.

**What the gate covers**, stated here because the rest of this
section quantifies over it: **every** send a producer run makes, on
either lane. RFC 0014 §3.1 splits producer output in two — message
output on the data lane, and the one producer output that does not
travel it, a producer-originated quit, on the control lane — and the
gate holds both. Three things follow. §9.5's bootstrap claim that no
producer output reaches *either* lane before a grant is complete
rather than data-lane-only, because a producer-originated quit is
gated too. A producer's quit is scriptable exactly like its
messages — the pass-unit series RFC 0014 §13.1 names for both quit
semantics is driven by granting the quit's own send. And `accepted`
records accepted producer sends from both lanes, each record carrying
the lane alongside its origin, so a test can tell a released quit
from a released message.

The gate belongs to the driver, so it reaches only what the driver
drives. RFC 0014 §13.1's `ParkProbe`-driven series are outside it:
that instrument is neither a driver nor a seam and holds no gate
(§9.7), and no `TestDriver` hands out the driving future it polls, so
the two cannot be combined. Its `parked control-quit wake` series
therefore obtains its control arrival the way production does — the
production gate is immediate, so a quit reaching a genuinely parked
kernel is one an application-supplied effect emits at that moment,
which is exactly the "scripted from a genuinely parked kernel with no
other work pending" form RFC 0014 §13.1 requires of all three. How
those three are harnessed — including what turns the emitting effect
gets, which no call of this section supplies — belongs to RFC 0014
§13.1 with the series themselves.

Three rules carry INV-RC14's enqueue-order guarantee onto the
surface:

- **One outstanding grant, driver-wide.** At most one grant is
  outstanding across the whole driver, not one per origin: while a
  token is unconfirmed, `grant` returns `Err(GrantOutstanding)` at
  issue time whatever origin it names. The sequential handshake
  *grant → enqueue-acceptance confirmed → next grant* is therefore
  the only way two releases can be ordered at all, and raw grant
  order is not expressible — neither `grant(A); grant(B)` across two
  producers, which is the model RFC 0014 §11 excludes, nor two
  releases at one origin. A per-origin rule would admit the first of
  those, so the rule is driver-wide.
- **Detached.** The token borrows neither the driver nor the script.
  The driving methods take `&mut self`, so a token that borrowed the
  driver could not coexist with a step; detached, a test holds an
  acceptance outstanding across `step_pass` calls. That is what
  RFC 0014 §13.3's **driver progress** condition needs — an
  acceptance that requires the kernel to drain the lane its send
  waits on cannot be confirmed without stepping, and a token
  borrowing the driver could not survive the step. `confirm` is the
  in-place form for the case that needs no pass; like every wait here
  it is bounded and fails rather than hangs. Where the acceptance
  does need a drain, it happens inside a `step_pass` instead, which
  is the same route by that other name.
- **The guarantee starts at the gate.** Scripted enqueue order is a
  claim about sends the gate has released and confirmed, never about
  the order producers reached the gate in.

RFC 0014 §13.3's **ack correlation** condition reads, in full: "at
most one outstanding grant per origin, or an explicit correlation of
each grant to its exact commit; the next grant to an origin only
after the previous acceptance." The driver-wide rule above satisfies
the first disjunct strictly — one outstanding grant driver-wide
implies at most one per origin — and `GrantToken` supplies the
second, being correlated to the commit its release produces. The
trailing clause holds as written wherever a grant resolves by
acceptance: no grant at any origin is admitted while a commit is
still uncorrelated. Where a grant resolves as `Confirmed::Reclaimed`
there is no commit to correlate, so the clause has nothing to range
over, and **how it reads in that case is not settled here**: it is
part of what RFC 0014 §13.3's resolution fixes, which §9.8 keeps
open. This section neither narrows nor widens that condition, and
states no reading of it beyond the acceptance case its letter covers.

**Grant lifecycle, in full.** A grant at an origin the kernel's run
bookkeeping does not currently hold — a run never started, or one
whose exit a pass has already reflected — is misuse and fails the
test: it is a script error the kernel cannot produce an outcome for,
and an error return would let a test go on scripting against a run
that is gone. `settle` is misuse while a grant is outstanding,
stranded or not, for the reason its own paragraph gives. `confirm`
and `grant` after termination are misuse under §9.3's state table,
like every other driving call.

**A released send ends in one of exactly two states, and `confirm`
reports which.** A send released at the gate either **commits** — it
is admitted, and the guaranteed sequence gains its entry — or its
reservation is **released without committing**, the producer having
reclaimed it before admission. Those two are disjoint and exhaustive:
there is no third *end* for a released send — though a send may sit
in flight for as long as the lane makes it wait, which is why
`confirm` carries a budget rather than a promise (below).
`confirm` drives until one of the two is reached and
returns `Confirmed::Accepted` or `Confirmed::Reclaimed`. Both clear
the outstanding grant, so `grant` and `settle` are legal again after
either.

**Revocation is not one of those two states, and does not stop a
send from committing.** Revoking a run filters its output at
delivery, not at admission: from the application point no output of
that run reaches `update`, buffered before or sent after (INV-RC5),
while the items themselves still occupy the lane until dequeued and
their dequeue does no `update` work (RFC 0014 §4.3, which states
plainly that such sends are *filtered, not prevented*). So a granted
send whose run is revoked mid-flight can perfectly well return
`Confirmed::Accepted`, and the entry it puts in the ledger is true:
that ledger records what passed the gate, not what reached `update`
(§9.6's ledger paragraph). A test that wants to know the item was
never delivered reads the pass that dequeues it, not the ledger —
which is where INV-RC5 is checked in any case (§9.11).

Whichever route ends a run — an explicit cancel, a `CancelInFlight`
supersession, a scope teardown, or termination, whose task
cancellation RFC 0014 §6.1 and RFC 0011 §4.4 make contract — the
released send still lands in one of the two states above. *When* an
abort falls relative to an in-flight send is mechanism, and this
surface does not depend on it: either the commit happened before the
producer was reclaimed, or it did not.

The disjunction is `confirm`'s *completion* condition, not a promise
that one of its arms arrives inside the budget. Where the commit
needs the kernel to drain the lane the send waits on, neither arm is
reached under `confirm` and it fails on its bound like every other
wait here; the test steps instead, which is what the detached token
exists to permit.

This is a sanctioned observation of a state the *kernel* produced,
and it is deliberately not the same thing as a **stranded token** — a
token dropped without `confirm`, which leaves its grant outstanding
so that every later `grant` returns `Err(GrantOutstanding)` and every
`settle` is misuse until the test ends. That one is a test-author
error with no kernel state behind it, and it stays recorded as the
misuse pattern that error most often means. The reclaimed resolution
is not a corner case looking for a use: RFC 0014 §13.1's
shutdown-scoped send-failure series is a blocked sender reclaimed by
cancellation in the full topology (RFC 0014 §6.1, where the future is
dropped at its await point), which is that resolution exactly. What
such a series then asserts — RFC 0011 §4.4's two postcondition
stages — it reads from the runtime, never from this resolution, which
reports only how one released send ended.

**`settle` is the call that contracts turns.** Some runs finish
without ever presenting a send-intent — a cleanup finalizer, whose
`Output = ()` closes the message path outright (RFC 0014 §4.4), a
future that completes with no message, a subscription run stopping
after its last output. No grant releases them, and no pass
*guarantees* them anything: a pass turns the executor and may advance
a runnable producer incidentally at an await point, but it promises
no turns at all — a pass that never awaits yields none. That is the
gap `settle` closes, and its whole content is the three things a
by-product cannot offer: turns as the *purpose* of the call, a stated
budget, and a completion condition. Bounded turns, no wall clock,
exhausting the budget fails the test. It begins no pass and releases
no send-intent, and an exit it lets a run reach becomes visible the
way every exit does, at the exit-reflection stage of the next
`step_pass(WakeSource::ProducerExit)`.

Since turns are not selective (§9.3), what `settle` does to the two
ledgers is stated exactly rather than denied. It **initiates no
append to the guaranteed sequence**, and that holds structurally
rather than by intent: `settle` is misuse while a grant is
outstanding, no send is released except through an outstanding grant,
and a released send's grant stays outstanding until that send reaches
one of its two terminal states — so during a legal `settle` there is
no armed gate and no released send still in flight, and nothing can
commit. The
**intent ledger may gain entries**, on the other hand, from any
producer the turns advance to a send point — as it may during any of
the other three driving calls (§9.3). That is what a non-guaranteed
pre-gate ledger is for, and a test reading `intents` after a `settle`
is reading exactly the kind of record this section declines to
guarantee.

Two ledgers divide at the send gate. `accepted` records the sends
admitted past it, each record carrying its origin and its lane, in
gate order: the guaranteed observation sequence INV-RC14 scopes,
which RFC 0014 §7.2 begins at the gate for exactly this reason.
Admitted is not delivered — a record says an item passed the gate and
says nothing about whether `update` ever saw it, which is why a
revoked run's committed send belongs in it. That order is the
*driver's*, established by the sequence of grants, and cross-lane
it is nobody's claim about production: RFC 0014 §3.3 declines to
order a run's own control-lane quit against its earlier data-lane
output at all, so a reading that puts one before the other is the
citation rule's ordinary case (§9.9).
`intents` records send-intents before the gate, origin-tagged:
pre-gate records, deliberately outside the guarantee. A test may read
`intents` to see that a producer reached the gate; it may not derive
an order or a completeness claim from them.

**Neither ledger is a public transcript surface** (RFC 0014 §7.2),
which fixes what each is and is not. Each is a **test-assertion
surface**: a driver test reads it to assert about the execution it
has just driven. Neither is a **schema surface** — no external
consumer's dashboard, alert rule, or log parser reads either, and
neither carries any part of RFC 0006 INV-L13's observability schema
as RFC 0014 §9 row 9 amends it, so what a ledger record contains is
free to change where a schema field is not. And neither is a
**citation surface**: what is read from one is a fact about the
driven execution, admissible only within §9.9's citation rule.
Neither is the store's delivery transcript or a substitute for it —
§6's exhaustiveness is stated over the store's pending set and has no
counterpart here (§9.11).

### 9.7 `ParkProbe`

`ParkProbe` is the park boundary's instrument, and RFC 0014 §7.2
gives the reason no driver step reaches that boundary: a step
*begins* a pass, and a parked kernel is precisely one with no pass
running and none beginning until a source arrives — so the driver's
pass-initiation seam replaces the very mechanism INV-RC16
constrains.

What the probe supplies is a waker and a poll, and nothing else. It
polls the production driving future directly; it scripts nothing
inside the kernel, adds no branch, and is neither a third runtime
seam nor a second driver. Its surface reports what RFC 0014 §7.2 says
the probe observes: whether the loop parks (a `poll` returning
`Pending`), which sources it armed (`armed`), and which arrival woke
it (`woken_by`, with `wakes` counting the waker's calls). The two
source-reporting readers speak `WakeSource`, the same vocabulary
§9.5 scripts the seam with, because it is the same set: INV-RC16's
armed sources.

**Its evidence scope is INV-RC16's arming and wake claims, and
nothing else.** A `ParkProbe` observation is never evidence for
INV-RC13's same-topology claim, for INV-RC14's scripted determinism,
for RFC 0014 §3.5's pass stage order, or for production pass
initiation. RFC 0014 §13.1 names the three series it carries; every
other series is pass-unit driven.

### 9.8 Determinism, scoped

A **script** is three things together: the application-side inputs
and readiness (§9.2), the arbitration choices (one `WakeSource` per
`step_pass`), and the grants. For a deterministic application, one
script yields one observation sequence across repeated runs, because
the driver introduces no nondeterminism of its own (INV-RC14). As in
INV-T4, the claim scopes to what the mechanism contributes: an
application whose own reduction is nondeterministic is outside it.

Two bounds on that claim, both RFC 0014 §7.2's and neither weakened
here:

- **Enqueue order is guaranteed only through the handshake** — grant,
  confirmed acceptance, next grant (§9.6). Raw grant order guarantees
  nothing, and is not expressible.
- **The verified range is a current-thread executor and unbounded
  lanes**, and the claim is scoped to it.

**The bounded extension stays open.** Extending the determinism claim
to bounded lanes and executor-independent scheduling needs its own
verification pass and the two protocol conditions RFC 0014 §13.3
names: **driver progress** — the driver stays steppable while a
grant's acceptance is outstanding, so a capacity-blocked send cannot
deadlock the handshake — and **ack correlation** — at most one
outstanding grant per origin, *or* an explicit correlation of each
grant to its exact commit, with the next grant to an origin only
after the previous acceptance. §9.6 quotes that second condition in
full and answers it clause by clause. This section's surface is
shaped to satisfy both (§9.6's detached token and its driver-wide
admission rule), but a shape is not a verification: until that pass
lands, the claim keeps the verified range above and RFC 0014 §13.3
stays open. It resolves as an addition to this section, recording
what was verified and at what scope.

### 9.9 The evidence surface and the citation rule

- **Pass-unit driving is the evidence surface for everything the
  driver can reach.** Acceptance and conformance evidence for a
  steady-state property is produced by pass-unit driving only: one
  `step_pass` executing one whole pass through RFC 0014 §3.5's stage
  order. `boot` is the same granularity for bootstrap — the whole
  production bootstrap in one call, never a stage of it (§9.5) — and
  bootstrap evidence (RFC 0014 §6.2's init-quit outcome, carried by
  INV-RC11) comes from it.
- **The remaining driving calls carry no evidence of their own.**
  `grant` opens the route by which an entry is appended to the
  guaranteed observation sequence, and the grant it opens resolves by
  one of three routes: a commit reached inside `confirm`; a commit
  reached inside a `step_pass`, where it needs the kernel to drain
  the lane the send waits on (the RFC 0014 §13.3 driver-progress
  form); or a `Confirmed::Reclaimed` resolution, which appends
  nothing because no commit occurred (§9.6). The entry, where there
  is one, becomes evidence when the pass-unit step that delivers it
  consumes it; the handshake itself witnesses nothing about
  production. `settle` contributes nothing to that sequence — it
  initiates no append to it (§9.6), and the exit it lets a run reach
  is evidence only once a `step_pass(WakeSource::ProducerExit)`
  reflects it. Any pre-gate record these calls produce is outside the
  guaranteed sequence by construction, which is what makes the intent
  ledger inadmissible here rather than merely unreliable. Neither
  call is a second evidence surface beside pass-unit driving, and
  neither is a stage of a pass: both leave the stage order untouched,
  which is why they do not fall under the probe exclusion below.
- **Stage-granular probes sit outside that surface.** A probe running
  a single stage in isolation may exist as a component-level
  white-box instrument; it is no part of this public surface, and
  nothing observed through one is evidence for INV-RC13 or for
  RFC 0014 §13.1's pass-unit series. The reason is that such a probe
  can fabricate a permuted execution the fixed stage order forbids
  (RFC 0014 §11's batch-first model).
- **`ParkProbe`'s scope is INV-RC16 alone** (§9.7).
- **The citation rule.** An order the driver establishes is never
  evidence of a production order — §4.2's rule, generalized by
  RFC 0014 §7.2 — and it reaches every order this section names: the
  scripted sequence of wake sources, the gate order `accepted`
  records, and the `started` order of a report. Which source production
  picks among several ready at once stays unobserved here (RFC 0014
  §3.5 pins the policy, not the occasion), and a
  `ParkProbe`-established fact is evidence for the park contract
  alone.

### 9.10 Store parity extension

The same landing extends this store's command intake. The lowered
parts the store consumes (§4.1) gain **teardown entries** —
`Command::teardown`, RFC 0013 §3.2's primitive whose kernel side is
RFC 0014 §4, together with the `Command::on_teardown` cleanup
registrations of RFC 0014 §4.4 — and **independently keyed batch
children**, `Command::batch` no longer folding a child's spawn key
away (RFC 0014 §3.4, §7.1).

INV-T3 needs no restatement for that: it is stated over the shared
decomposition boundary rather than over that boundary's current
member list, so the new entries are inside it as written. Its
structural review re-runs at the store's intake site once the parts
carry them.

The rest of the extension is a **named delegation**, recorded here
rather than drafted here. Its owner is the change that lands the
kernel-side lowering; the store's half lands in that same change and
not before it, so this document and the kernel never state different
lowering semantics at once. What that change must fix, in full:

- the store's own behavior for each new entry class — what a teardown
  entry selects over the store's pending set, and what a cleanup
  registration means in a harness that spawns no task;
- §5.1's last bullet, which reads batch child-key folding off
  RFC 0003 INV-11 — the invariant RFC 0014 §3.4 supersedes;
- the shared-first parity claims this document makes about the
  runtime, which RFC 0003 INV-14's supersession (RFC 0014 §3.2)
  reaches: the summary's second decision, §3.2's `send` semantics,
  §4.2's negative space, §6's `send` rationale, INV-T8's keyed
  retention test, and the RFC 0003 entry in §11.

Writing those edits before the kernel lands would state a contract
this crate does not implement, which is why they are delegated rather
than made here; nothing in stages 1–2 changes until then.

### 9.11 Coverage, models, and excluded claims

**Surface–invariant coverage.** This section introduces no invariant.
The driving contract's invariants are RFC 0014's, and every element
of §9.3's block maps to one of them, walked in order:

- `TestDriver::new` and §9.2's absent constructors → INV-RC13, whose
  declared structural half is exactly this API-surface review; the
  driving methods' `&mut self` receivers and the driver's sole
  ownership of its kernel instance additionally hold RFC 0011
  INV-LC9's exclusivity property, which that RFC's §6 requires any
  step-style surface to preserve.
- `boot`, `step_pass`, `WakeSource`, and `NotReady` → INV-RC13's
  behavioral half, which runs through pass-unit steps against the
  production seams; `boot`'s whole-bootstrap granularity additionally
  serves INV-RC11's init-quit row (§9.5, §9.9).
- `grant`, `GrantToken`, `GrantOutstanding`, and `confirm` →
  INV-RC14, whose structural half is that the raw-grant shape is
  unrepresentable — which is what the driver-wide outstanding rule
  delivers (§9.6). `Confirmed` maps to INV-RC14 through both arms: it
  reports which terminal state a released send reached, and only the
  committing one appends to the gate-scoped sequence. It maps to
  INV-RC5 through *neither* — strict revocation is a delivery-side
  property, so neither ledger witnesses it, and its behavioral rows
  read the pass that dequeues a revoked item without doing `update`
  work (RFC 0014 §4.3), not any record this surface keeps.
- `settle` → INV-RC13: it drives the production executor and adds no
  seam, so it is covered by the same API-surface review. It reaches
  INV-RC14's observation sequence only negatively, by initiating no
  append to it — a property §9.6 makes structural through the rule
  that `settle` is misuse while a grant is outstanding.
- `accepted`, `intents`, `AcceptanceLedger`, `IntentLedger` →
  INV-RC14's gate-scoped observation sequence, whose pre-gate
  exclusion is what the second ledger keeps separate.
- `ParkProbe` and its three readers → INV-RC16, whose behavioral rows
  sit on that probe; `armed` and `woken_by` are the API form of the
  arming and wake observations RFC 0014 §7.2 names.
- `StepReport::terminated` → INV-RC11 (the production result
  contract, RFC 0011 INV-LC5's, preserved).
- `Origin`, `AnonymousRun`, and `StepReport::started` → no invariant
  of their own: the identity models are RFC 0005's, and the handle's
  confinement falls inside INV-RC13's structural review, which walks
  the API for surfaces reaching the kernel.

§9.10's parity extension maps to INV-T3, unchanged.

**Adversarial models considered**, beyond the driver models RFC 0014
§11 already excludes:

- *Handle as key* — an implementation that makes anonymous runs
  nameable by synthesizing a kernel-side key would reintroduce the
  identity model RFC 0013 §10 rejects, while passing every naming
  test. Excluded by §9.4's second rule and INV-RC13's structural
  review: the handle is minted from an observation of a run the
  kernel already started, it participates in no keyed capacity,
  gauge, or admission semantics, and what crosses the grant boundary
  is the run identity the kernel already holds.
- *Fabricated readiness* — a driver that accepts any `WakeSource` and
  runs a pass anyway can script pass orders production can never
  produce, and every pass-unit series would still pass. Excluded by
  §9.5: readiness is read from the production sources and an
  unarrived source drives nothing.
- *A fourth initiation reason* — a surface that lets a test begin a
  pass for something other than an arrived wake source widens
  RFC 0014 §7.2's first differential past the set it confines it to,
  and no invariant would catch it because INV-RC16 quantifies over
  the armed sources only. Excluded by §9.5: `WakeSource` is the whole
  vocabulary, `boot` absorbs the one pending frame that ever exists,
  and there is no other entry point that begins a pass.
- *Ledger as transcript* — a test that reads `accepted` as the
  application's message transcript would turn a driver-established
  order into evidence about delivery. Excluded by §9.6's ledger
  content (send events by origin, not messages) together with §9.9's
  citation rule.
- *Shape as verification* — reading §9.6's driver-wide admission rule
  and detached token as discharging RFC 0014 §13.3. Excluded by §9.8:
  the conditions are protocol requirements the shape satisfies, and
  the claim still waits on the verification pass.
- *Selective turning* — an implementation reading any turn-driving
  call as advancing only the runs a test has in mind would let a
  script depend on which runs an executor turn happens to reach. The
  misreading is available for `confirm` exactly as for `settle`, so
  the exclusion is stated over the class: §9.3 makes non-selectivity
  a fact about all four driving calls, the intent ledger may gain
  entries during any of them, and the only thing claimed of `settle`
  is that it initiates no append to the guaranteed sequence.
- *Unresolvable grant* — a surface whose grant clears only on a
  commit leaves the driver permanently unusable whenever a released
  send is reclaimed instead: `confirm` exhausting its budget,
  `settle` barred, every later `grant` refused, with no test-author
  error anywhere. Excluded by §9.6's two terminal states, which
  partition what can become of a released send and clear the grant on
  either; a budget exhausted before either arrives still fails the
  test, and the stranded token stays a dead end because it *is* a
  test-author error.
- *Acceptance read as delivery* — reading an entry in `accepted` as
  proof that `update` saw the item. A revoked run's send can commit
  and be recorded, and is then dequeued with no `update` work at all
  (RFC 0014 §4.3). The accessor's name carries the distinction now
  rather than leaning on prose, and §9.6's ledger paragraph scopes
  the record to admission besides; §9.11's mapping puts INV-RC5's
  checks on the pass rather than on either ledger.

**Excluded claims**, per the checklist's minimal-contract item: no
INV-T-numbered restatement of INV-RC13, INV-RC14, or INV-RC16 is
added — a second statement of an invariant owned elsewhere is one a
later amendment can drift from, and the surface above maps to the
originals instead; no exhaustiveness or leak-check rule is stated for
the driver, because what becomes of undelivered output is the
kernel's own revocation and termination contract (RFC 0014 §3.1,
RFC 0011 §4.4) and a store-style pending set does not exist here; no
correspondence between `started`'s order and a command's declaration
order is claimed, that lowering order being RFC 0014 §3.4's to state;
and no bounded-lane determinism claim is made (§9.8). Two elements
were suspected of redundancy and kept, neither implied by its
suspected survivor. `confirm` against `step_pass`, which also drives
the executor: stepping runs a whole pass, so confirming an acceptance
through a step would put a pass between two grants and change the
very enqueue order the handshake exists to script. And `settle`
against `confirm`, which also drives the executor without a pass:
`confirm` requires a token to consume, so a test whose only
outstanding work is a run that never sends has no token to confirm
and no way to reach that run at all.

## 10. Open questions

1. **Unordered batch receive.** Should a set-based helper (assert that
   the next N deliverable messages equal this multiset, in any order)
   exist for tests over sibling leaves, so they need not encode the
   canonical linearization? Trigger: real tests that repeatedly assert
   cross-leaf sequences where the order is incidental. Additive either
   way; stages 1 and 2 ship without it.
2. **Non-exhaustive mode.** A lenient mode (unasserted output tolerated,
   or selectively skippable) is not designed here. Trigger: a concrete
   consumer — most plausibly migrating a large existing test suite —
   that exhaustive-only demonstrably blocks. Designing it then is a
   reviewed amendment (skippability interacts with §5.3's quit carve-out
   and §6's drop check).

## 11. References

- RFC 0002 — redraw suppression: the directive `redraw_requested`
  observes.
- RFC 0003 — command cancellation: its §4.2 pre-spawn reconciliation,
  its §5.1 explicit-cancels-before-keyed-spawn ordering, and INV-1,
  INV-3, INV-4, INV-5, INV-6, INV-7, INV-9, INV-10, INV-11, INV-14
  (cited in §§4.1, 4.2, 5.1, 5.3 of this document).
- RFC 0005 — structural lifecycle identity: `SubscriptionId`, the
  declared-set semantics `subscription_ids` observes; with `CommandId`,
  the two identity types §9.4's origins name runs by.
- RFC 0006 — runtime load control: the shutdown discard carve-out §5.3
  mirrors (INV-L2); the runtime contracts §1.2 excludes.
- RFC 0004 — command timeout and retry: the first-poll deadline anchor
  and backoff semantics §4.3's anchoring contract restates over the
  store's scan sites; RFC 0009 INV-C3 carries them onto the virtual
  clock.
- RFC 0007 — RuntimeConfig: the prelude-membership reasoning §3.3
  follows.
- RFC 0009 — Clock DI: its §3.2 controlled time context and INV-C2/
  INV-C3, consumed by §4.3 and INV-T12; its §3.4 equal-deadline
  negative space, which §4.2's linearization supplies; its §5.1 design
  inputs and `test-util` decision, resolved and carried out by §7.
- RFC 0011 — runtime lifecycle: INV-LC3's construction inertness, which
  §9.2 preserves for the driver, and INV-LC5's result contract, which
  §9.3's termination report carries.
- RFC 0012 — subscription execution: §6.2, which reserves the stage-3
  driving surface §1.3 delegates, and whose non-execution boundary it
  preserves; §6.1's source template, the injection surface §9.2's
  application-side inputs go through.
- RFC 0013 — scope teardown: §9's third resolution and §10's rejection
  of auto-keying, which §9.4's origin naming honors.
- RFC 0014 — reducer-first core: §7.1's store parity extension, §7.2's
  driving contract, §7.3's per-layer claims, and the amendment register
  §9 whose row 11 names this RFC; §3.5's pass stages and wake arming,
  §13.1's gate, and §13.3's open bounded-lane question, all consumed
  by §9; INV-RC11, INV-RC13, INV-RC14, and INV-RC16, which §9's surface
  maps to instead of restating.
- `src/application.rs` — the trait whose bounds §2 pins.
- `src/command/core.rs`, `src/command/runtime_parts.rs` — the
  decomposition boundary INV-T3 names.
- `src/subscription/mock.rs` — the unconditional test-support precedent
  §3.3 cites.
- `docs/rfcs/pre-review-checklist.md` — enforcement-class definitions
  used in §8.
