# RFC 0008: TestStore — deterministic update and effect testing

- Status: Implemented — stages 1–2 with the store, stage 3 (§9) with
  the reducer-first kernel, at the paths §9.1 places it
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
> separate subscription-execution design, not stage 2. Stage 3 — which
> waited on RFC 0014's kernel and landed with it — is not a store stage
> either: it is a separate driving layer beside the store (§1.3, §9)
> that executes the production kernel, subscription sources included,
> and leaves §1.2's non-execution boundary exactly where it is.

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
   earlier step's not-yet-received keyed output — an ordering the runtime
   once guaranteed through shared-first pull and no longer does (§6). The one carve-out mirrors the
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
never spawn a task (§1.2); that boundary is unchanged. A third layer sits
beside them rather than inside them: a `TestDriver` that drives the
production kernel itself. Its contract is pinned by RFC 0014 §7.2 —
construction through the production path, with the production task
bookkeeping, lanes, phase machine, and termination shared rather than
re-implemented; a driving differential confined, exhaustively, to two
seams — pass-initiation arbitration and producer send grants — plus one
recorded pre-pass executor turn that is no seam (§9.2), plus what
the application side supplies, which is **inputs and readiness** (mock
sources satisfying RFC 0012 §6.1's template, and test-controlled gates
inside application-supplied effects); scripted determinism over the whole
script — inputs, readiness, arbitration choices, and grants, together
with the driving calls §9 adds and their arguments — for a deterministic
application, with the grant-then-acceptance handshake as the narrower
condition under which *enqueue order* is guaranteed at all, and the whole
determinism claim scoped to its verified range: a current-thread
executor, on either lane mode once §9.12 records the bounded extension's
verification pass, with executor independence still open at RFC 0014
§13.3; and **pass-unit driving as the evidence surface**, one driver step
executing one whole production pass, with stage-granular probes
admissible as component-level instruments but outside that surface. The
one boundary no pass-unit step reaches is the park boundary, where RFC
0014 §7.2 names a separate instrument, `ParkProbe`, whose observations
are evidence for that RFC's park-and-wake invariant alone — never for the
driver's topology or determinism claims, and not for anything this
store's layers claim. §4.2's citation rule generalizes to both: an order
the driver establishes is never evidence of a production order. §1.2's
negative space is about the store and is unchanged — what each layer
claims is RFC 0014 §7.3's.

The API body — the concrete `TestDriver` surface — is §9: the
additive section RFC 0014 §9 row 11 records and RFC 0012 §6.2
reserves. It expresses RFC 0014 §7.2's contract as API and adds no
driving guarantee beyond it; the surface is in the crate at the paths
§9.1 places it.

The same landing extended this store's command intake: the lowered
parts it consumes carry teardown entries and independently keyed batch
children (RFC 0014 §3.4, §7.1). §9.10 states what that cost this
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
  not-yet-received keyed output — the store's own linearization, no
  longer backed by a runtime schedule (§6). Its only poll is the keyed-intake reconciliation of §5.1,
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
- **`receive_quit`** observes a quit by either route. A quit an
  `update` returned was applied at that dispatch, synchronously, and
  does not travel as output — `receive_quit` observes the application
  and requires nothing to be deliverable. A producer-originated quit
  does travel, so for that route `receive_quit` asserts the next
  deliverable output is a quit request. After it succeeds, the store is
  in the quit state: `send`, `advance`, `receive`, `receive_matching`,
  and `receive_quit` all fail; `state`, `redraw_requested`,
  `subscription_ids`, and `finish` remain callable.
- **A dispatch-applied quit is terminal before it is observed.** The
  application stopped when the quit applied, not when the test noticed,
  so `send`, `advance`, `receive`, and `receive_matching` fail from that
  dispatch onward — a store that accepted a further input there would be
  scripting an execution the runtime cannot produce, since no later
  input intervenes between the update that returned a quit and
  termination. `receive_quit` is the one driving call that stays
  available, because observing it is how the state is left.
- **`subscription_ids`** calls `Application::subscriptions` and returns
  the declared IDs in declaration order, deduplicated by RFC 0005 §3.5's
  first-occurrence-stable rule: for equal full IDs in the declared list,
  only the first occurrence is kept, at its original position among the
  survivors (`[A, B, A]` → `[A, B]`, never `[B, A]` or any other
  reordering). This is the same *desired set* the runtime's reconcile
  computes before admitting anything — not the set it starts: a
  reconcile leaves an already-running id untouched and calls a source's
  `stream()` only for an id newly entering the set
  (`src/kernel/pass.rs`, `src/subscription/core.rs`).
  `subscription_ids` performs the same dedup without going anywhere near
  that machinery — it never calls `stream()` on any declared source and
  never runs a reconcile (§1.2) — so its return value predicts the
  reconciliation *input*, not which ids the runtime starts or which are
  currently live. For the same reason it does not reproduce the
  warning-level tracing event RFC 0005 §3.5 requires of the ignored
  duplicate: that event is the reconcile's own side effect, never
  triggered by a call that runs no reconcile at all. `SubscriptionId`
  is already `Clone + Eq + Hash + Debug`, so
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
  delivery order (RFC 0003 INV-10's one-item dispatch orders dispatch
  points, not sibling leaves; INV-14's shared-first app-input scheduling
  ordered pull points and is superseded — RFC 0014 §3.2 replaces the two
  delivery classes with one FIFO lane, so there is no second class to
  prefer).
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
exhausted.** The runtime reads that fact from bookkeeping rather than
by polling: a run holds its keyed slot until its queued output has
drained, and the exits that end runs are reflected once at the head of
every pass rather than sampled per dispatch
(`ScopeRegistry::keyed_occupant`; RFC 0014 §3.1, and RFC 0003 §4.2's
contract on the successor's accounting). The store keeps no such
accounting, and exhaustion is observable to it only by polling, so it
reaches the same fact by **keyed-intake reconciliation**: when a keyed
command arrives for an occupied id and its policy's admission decision
depends on the occupant's state (`CancelPolicy::KeepInFlight`), the
store reconciles before that decision. (`CancelInFlight`'s outcome does
not depend on the occupant's state, so it reconciles nothing — see the
per-policy bullets below.) If
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
  into a released id are indistinguishable. The runtime does read the
  target slot's occupancy before *every* keyed `Spawn(policy)`,
  `CancelInFlight` included (`spawn_decision` in
  `src/kernel/lowering.rs`, applied by `Kernel::apply_spawn`;
  RFC 0003 §4.2), but that read cannot change a `CancelInFlight`
  admission outcome: an occupied slot means the occupant is superseded
  first, an empty one means there is nothing to supersede, and the new
  stream starts either way. The store has no equivalent occupancy to
  read, so skipping the poll preserves the delivery contract while
  matching the runtime's outcome, not its every step. Not polling also
  lets a `CancelInFlight` command supersede an occupant whose poll
  would fail the test without polling it — in stage 1 that made
  cancelling a time-dependent keyed leaf expressible at all; under
  stage 2 the same escape hatch remains for I/O-dependent leaves
  (§4.3). Any poll that is issued follows §4.1's budget.
- `Command::cancel(id)` drops the occupant's stream and undelivered
  output, and is idempotent (INV-4).
- When one command carries both explicit cancels and a keyed spawn, the
  store applies the fixed phase order (RFC 0003 §5.1 as RFC 0014 §3.4
  extends it): the cancel phase — explicit cancels and teardown
  prefixes — applies before *every* spawn of the same command, so a
  command can cancel its own occupant and immediately reclaim the id in
  one step.
  `Command::batch([Command::cancel(id), work.cancellable(id).into()])`
  drops the old occupant's undelivered output exactly as the bullet
  above describes, then admits `work` under `id`; the old run's output
  is gone, and only `work`'s output is thereafter deliverable at `id`.
  The key rides the carrier rather than the command around it, which is
  what makes the shape expressible at all — a key attaches to one effect
  carrier, so there is no batch-level key to write. An implementation
  that instead admitted the spawn before applying the command's own
  cancel would cancel `work` itself and leave `id` empty — the wrong
  outcome — so this ordering is load-bearing, not incidental, and is
  asserted directly rather than left to fall out of the two bullets
  above.
- Unkeyed commands are unaffected by any of the above (INV-1's default
  path).
- `Command::batch`'s children each keep their own spawn key: a key
  attaches to one effect carrier, so batching neither folds keys nor
  discards them (RFC 0014 §3.4, superseding RFC 0003 INV-11). The store
  consumes real `Command` values, so it sees exactly the per-carrier
  keys the runtime does, and two same-key children in one command apply
  in declaration order as two consecutive admissions — the second a
  replacement under its own policy.

These are the deterministic core of RFC 0003 — what may still be
delivered, and when an id releases — restated over the store's pending
set, with keyed-intake reconciliation as the store's analogue of the
occupancy the runtime reads off its delivery accounting. The mechanics
that exist only
because the runtime is concurrent (stale-exit tokens, INV-8; bounded
bookkeeping, INV-13) have no TestStore counterpart and are deliberately
not modeled.

The residual negative space is the reconciliation instrument itself:
the store's proof of exhaustion is §4.1's single poll per leaf at
intake. A leaf that needs further polls to complete (a self-waking
future mid-completion) reads as still open and keeps the id occupied —
deterministically — while at the runtime's decision point the same
run's exit may or may not have been reflected yet, a scheduling fact
the pass boundary resolves whichever way it finds. In that
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
directive — its occurrence itself is not promised (RFC 0011 §3.2,
RFC 0014 §6.2). So
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
  is received. **This was runtime parity and is now the store's own
  linearization.** It read off shared-first pull — a shared input
  processed ahead of a keyed effect's already-ready output (RFC 0003
  INV-14) — and RFC 0014 §3.2 supersedes that: keyed, unkeyed and
  subscription output share one FIFO lane, so a `send`'s message is
  ordered against pending output by arrival, not by class. The
  historical reading stands as the record of why `send` was made
  non-blocking; the scripting freedom it justified is unchanged, and
  what changes is that scripting it is no longer evidence of runtime
  ordering for *either* key class (§4.2's citation rule) — which is what
  the unkeyed half of this bullet already said, now true of both. Undelivered output is
  not lost track of either way: it stays subject to the `receive*`,
  `finish`, and drop checks below, which remain exhaustive. `send` still
  fails after a quit, applied or observed (§5.3).
- **An applied quit that was never observed fails the `finish` and drop
  checks**, with the same standing as output that was never received:
  the run ended somewhere the test did not say it ended, and a script
  that omits it reads as though it ran to completion. Only an *observed*
  quit reaches the carve-out below.
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
  observed and any of: (a) a deliverable message or quit request
  remains; (b) any pending leaf has not been driven to completion — an
  in-flight effect the test never accounted for is a leak even if it
  never produced a message; (c) a cleanup registration is still armed,
  never fired by a teardown; or (d) a cleanup run started by a teardown
  has not finished. The last two are the same omission as the first two
  read over hooks: a finalizer's external side effects are its whole
  purpose, so a script that ends with one unfired ended without doing
  the thing it set up, and one the store cannot attest finished is not a
  hook it can report as run. Class (d) is recoverable exactly as an
  unfinished leaf is — the run is held, `advance`'s anchoring scan and
  this check both poll it again, and a finalizer waiting on the
  controlled clock completes once the clock reaches it — so the
  diagnostic says so rather than only naming the leak. A time-gated leaf the test never advanced
  to its deadline is exactly such an unfinished leaf: exhaustiveness
  makes declared time effects part of the accounting. After an observed
  quit, `finish` and the drop check poll nothing and pass
  unconditionally (§5.3) — an I/O-dependent leaf legally discarded at
  quit cannot fail them, because §4.3's failure requires a poll.
- Every exhaustiveness failure names the leaked messages via `Debug`;
  unfinished leaves that have produced no value are reported by count
  and enqueue position (there is no value to print). Cleanup classes
  report the same way, by count and by the position their registration
  was armed at: a hook has no value to print either, and its scope is
  structurally erased, so the arming order is what identifies *which*
  registration a failure means.

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
  quit suppression and the two reconciliation edges: a `KeepInFlight`
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
  work.cancellable(id).into()])` over an occupied id and asserts both
  halves at once: the occupant's
  undelivered output is unobservable via any `receive*` (as the
  explicit-cancel test already establishes) *and* a following `receive`
  at `id` yields `work`'s message — an implementation that admits the
  spawn before applying the batch's own cancel fails this test even
  though it could still pass the supersede and explicit-cancel tests in
  isolation, since neither of those combines a cancel and a spawn in one
  command.
  **Quit suppression is stated over the producer-originated route.** A
  keyed run that emits a quit has its output revoked with the run, so a
  cancelled keyed producer quit is never observable via `receive_quit` —
  that is the behavior tested. The `update`-returned route carries no
  such row, and not because it was dropped: a quit returned from
  `update` applies at its own dispatch and names no run, so it takes no
  key and there is nothing for a later cancel to reach (RFC 0014 §3.3,
  §3.4). The suppression claim is therefore scoped to the route that can
  still be suppressed, rather than asserted of quits in general.
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
  output survives, and the output is then `receive`d — and one over
  **unkeyed** output. Both assert the same thing now: that `send` does
  not block on pending output, and that the output survives it. The
  ordering is TestStore's linearization in both cases, not runtime
  parity — the keyed half used to be parity via RFC 0003 INV-14 and is
  not since RFC 0014 §3.2 (§6). Both are still required, because they
  fail different implementations: the keyed-only test would pass one
  that wrongly fails `send` on unkeyed pending output, and the
  unkeyed-only test would miss one that drops keyed output at a
  non-cancelling `send`. Each of these two
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
- **INV-T9**: quit terminality and carve-out — from the moment a quit
  applies, `send`/`advance`/`receive`/`receive_matching` fail on the
  quit state without polling any leaf, and after `receive_quit` so does
  `receive_quit`;
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
  running a reconcile (§1.2, §3.2). The returned `Vec` is the
  reconciliation *input* a reconcile would compute, not a prediction of
  which ids it starts or already has running — a reconcile leaves an
  already-running id untouched and calls `stream()` only for one newly
  entering the set (`src/kernel/pass.rs`). Structural: review of
  `subscription_ids` for the absence of any reconcile or
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
- **Placement, delivered.** This surface entered the crate with the
  reducer-first kernel, and its `Added` CHANGELOG entry ships with that
  release — the same form RFC 0014's own header states for it. The
  documents that reserved it pointed at it as future work until that
  delivery, deliberately, because a contract stated is not a surface
  delivered: RFC 0014 §13.2, RFC 0012 §6.2 and its §12's second
  question, and RFC 0010's TS-1, TS-3 and TS-6 rows were all
  conditioned on the delivery rather than on this section, and each
  closes at it. The crate's own types in §9.3 divided in three when
  this section was written, and all three divisions are in the crate
  now. The partition covers those and nothing else. Two groups of names
  appear in this section without belonging to it: `Backend` and
  `ratatui::Terminal`, which are the host UI library's, and `Future`,
  `Pin`, `Poll`, and `NonZeroUsize`, which are `std`'s. Neither group
  is this crate's to place.
  - **Introduced here**, arriving with that landing: `TestDriver`,
    `ParkProbe`, `WakeSource`, `RunName`, `RunKind`, `SendRecord`,
    `Lane`, `StepReport`, `GrantToken`, `Confirmed`, `GrantOutstanding`,
    `NotReady`, `AcceptanceLedger`, `IntentLedger`.
  - **Existing and unchanged**: `CommandId` and `SubscriptionId`,
    RFC 0005's identity types.
  - **Entering or changing in the same landing**: `Program`, `Exit`,
    and `ProgramRuntime`, which RFC 0014 §2.1 and §2.3 introduce —
    the last of them named by §9.7 as where a probe's future comes
    from — and `RuntimeConfig`, which that landing revises: it loses
    the frame-rate field and `keyed_channel_capacity`, and
    `app_channel_capacity` becomes `data_lane_capacity` with no alias
    (RFC 0014 §9 rows 2 and 4). That revised type is the
    `RuntimeConfig` the crate has, and it is `TestDriver::new`'s `config`
    parameter.
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
  that kernel instance's sole driver, and every `TestDriver` driving
  method takes `&mut self` — RFC 0011 INV-LC9's exclusivity
  property, which §6 of that RFC states a step-style surface must
  preserve.
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

What the surface supplies instead is the two seams — which ready wake
source begins a pass (§9.5), and the release of a producer's
send-intent (§9.6) — plus one difference that is not a seam and is
recorded here rather than hidden: **`step_pass` takes one executor
turn before it runs the pass**, unconditionally. Production takes its
turn at the park it is woken from; a scripted step has no park to be
woken from, so the turn is taken outright. It changes no branch
inside the pass and gates nothing — readiness is read *before* it
(§9.5), so the turn cannot unmake what admitted the step, since only
a pass consumes from a lane or the join set and a turn only ever adds
arrivals. It is a third driving differential all the same, because it
is a step production does not take in that position, and RFC 0014
§7.2's differential is exhaustive only if this is in it. `ParkProbe`
takes no such turn, which is why the park boundary's series are
unaffected (§9.7).

Inputs and readiness come from the application side — sources
conforming to RFC 0012 §6.1's template, and test-controlled gates
inside application-supplied effects — never from a driver method.

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

    /// The same construction on a multi-worker executor. Outside the
    /// determinism claim's verified range (§9.8) by design: it exists
    /// to drive what that range excludes.
    #[must_use]
    pub fn on_worker_threads(
        program: P,
        flags: P::Flags,
        config: RuntimeConfig,
        terminal: ratatui::Terminal<B>,
        workers: NonZeroUsize,
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

    /// Arms a grant at `run`, releasing the next of that run's
    /// send-intents that no grant has released yet — one already
    /// waiting at the gate when this is called, or else the first to
    /// arrive after it. The returned token borrows neither the
    /// driver nor the script. At most one grant is outstanding
    /// across the whole driver; the next — at this run or any
    /// other — is admitted only after this one resolves (§9.6).
    pub fn grant(
        &mut self,
        run: RunName,
    ) -> Result<GrantToken, GrantOutstanding>;

    /// Consumes `token`, driving the executor — beginning no pass —
    /// until the grant resolves, and reports how. Checked before
    /// the first turn, so a grant already resolved costs none;
    /// otherwise at most `max_turns`, and exhausting them fails the
    /// test, reporting the turns consumed (§9.6).
    pub fn confirm(
        &mut self,
        max_turns: usize,
        token: GrantToken,
    ) -> Confirmed;

    /// Reports the gate's terminal for `token` without driving,
    /// consuming, or clearing anything — `Some` once the gate holds
    /// one, `None` while it does not (§9.6). An observation call, so
    /// `&self` and a borrowed token.
    pub fn try_confirm(&self, token: &GrantToken) -> Option<Confirmed>;

    /// Drives the executor — beginning no pass and releasing no
    /// send-intent — for at most `max_turns` turns, until `until`
    /// holds. `until` is evaluated before the first turn, so an
    /// already-true condition costs none; exhausting `max_turns`
    /// fails the test, reporting the turns consumed. A turn is
    /// defined in §9.6.
    pub fn settle(
        &mut self,
        max_turns: usize,
        until: impl FnMut() -> bool,
    );

    /// Sends admitted past the gate, in gate order. Admission, not
    /// delivery (§9.6).
    pub fn accepted(&self) -> AcceptanceLedger;

    /// Send-intents recorded before the gate, under no ordering or
    /// completeness guarantee (§9.6).
    pub fn intents(&self) -> IntentLedger;
}

/// What a step started, and whether it terminated the program.
pub struct StepReport<E> {
    /// The runs this step started, in the order it started them.
    /// The only place a `RunName` is minted (§9.4).
    pub started: Vec<RunName>,
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

/// Names one producer *run* — one start, not one identity. Opaque,
/// minted only when the driver observes that run start, and never a
/// handle to the run itself (§9.4).
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct RunName(/* private */);

impl RunName {
    /// What kind of run this names, with the logical identity the
    /// kernel holds for it where there is one.
    pub fn kind(&self) -> RunKind;
}

/// A run's kind, read off a `RunName` (§9.4).
pub enum RunKind {
    Keyed(CommandId),
    Subscription(SubscriptionId),
    Anonymous,
}

/// One outstanding grant: correlated to the send it releases, and
/// through it to that send's commit where one happens.
pub struct GrantToken { /* private */ }

/// How a grant ended (§9.6). The two are disjoint and exhaustive;
/// both clear the outstanding grant, and only `Accepted` appends to
/// the guaranteed sequence.
#[must_use]
pub enum Confirmed {
    /// The send this grant released got into the lane. Whether its
    /// run is revoked is a separate, delivery-side question.
    Accepted,
    /// This grant will never put anything into the lane — the send
    /// it released ended without getting in, or the run it was armed
    /// at is gone with no send released at all.
    Reclaimed,
}

/// A grant is already outstanding on this driver.
pub struct GrantOutstanding;

/// The scripted wake source had not arrived; nothing was driven.
pub struct NotReady;

/// One send, named by the run that made it and the lane it was for.
/// The same record shape serves both ledgers; what differs is what
/// each ledger's *order* is worth (§9.6).
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct SendRecord { /* private */ }

impl SendRecord {
    pub fn run(&self) -> &RunName;
    pub fn lane(&self) -> Lane;
}

/// Which lane a send was for (RFC 0014 §3.1).
pub enum Lane { Data, Control }

/// Sends admitted past the gate, in gate order (§9.6).
pub struct AcceptanceLedger { /* private */ }

/// Send-intents recorded before the gate, in no guaranteed order
/// (§9.6).
pub struct IntentLedger { /* private */ }

// Both ledgers read the same way: a length and an ordered walk.
impl AcceptanceLedger {
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn iter(&self) -> impl Iterator<Item = &SendRecord>;
}

impl IntentLedger {
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn iter(&self) -> impl Iterator<Item = &SendRecord>;
}

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

    /// Wake-ups this probe's waker has received. Which source caused
    /// one is established by the script, not reported here (§9.7).
    pub fn wakes(&self) -> usize;
}
```

The block is normative for **what exists and what does not**: §9.2's
absent constructors, `WakeSource`'s membership as the whole
pass-initiation vocabulary (§9.5), `grant`'s detached token and its
driver-wide admission rule (§9.6), and the ledgers' division at the
send gate (§9.6). The **receivers are normative too**, and not as a
spelling: every `TestDriver` driving method takes `&mut self` and
every `TestDriver` observation method — `accepted`, `intents`, and
`try_confirm` — takes `&self`, which is how the
block carries RFC 0011 INV-LC9's exclusivity (§9.2, §9.11) and what
makes a borrowing grant token unrepresentable (§9.6).
Spellings — parameter names, accessor names, whether a report field
is a slice or an iterator, what a probe's constructor looks like —
are implementation latitude, exactly as for §3.1's block.

**Waiting, in full.** Every wait this layer performs is a bounded number
of executor turns: no method of this section sleeps, arms a timer, or
reads a wall clock, and the kernel they drive reads no wall clock either
(RFC 0014 §6.3). Exhausting a bound fails the test with a diagnostic
rather than waiting longer. The two calls that wait on a condition are
`settle` and `confirm`, and both take their bound from the caller as a
script element, for the reason §9.6 gives; `boot` and `step_pass` wait on
no condition, each completing by its own definition. Application-supplied
effects sit outside that quantifier, as they sit outside INV-T4's
determinism scope — an effect that sleeps times its own test.

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
whose whole job is to supply turns, under a caller-stated budget and
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
| `try_confirm` | misuse | legal | misuse |

The third row's condition is §9.6's grant lifecycle, which is where
those two calls' legality is stated in full: `grant` is misuse at an
run the kernel's bookkeeping does not hold, and `settle` is
misuse while a grant is outstanding.

`try_confirm` is an observation in receiver and effect — it drives
nothing and consumes nothing — but its legality follows the *grant*
lifecycle rather than the ledgers': a token exists only in the
running state, so it is misuse outside it, and misuse again on a
token this driver's gate does not hold.

Misuse **fails the test** rather than returning an error, in the
store's own style (§5.3), and the observation calls stay callable
throughout, as `state` and `finish` stay callable in the store's quit
state. The two error types are not misuse: `NotReady` reports a
production fact (§9.5) and `GrantOutstanding` a script-order fact
(§9.6), and both leave the driver untouched.

### 9.4 Naming a producer run

A `RunName` names **one run** — one start — for every producer kind
alike, and the driver introduces **no second identity model**.

The reason it is per run rather than per identity is that a logical
identity does not pick out a run. A `CancelInFlight` supersession
frees the identity slot for a successor *immediately* at the
revocation's application point, while the revoked run may still send
late; both are inert with respect to the successor, but both exist
(RFC 0014 §3.1's no-stale-resurrection clause, carrying RFC 0003
INV-8's token discipline). A subscription restarted under the same
`SubscriptionId` is the same case. Naming runs by `CommandId` or
`SubscriptionId` alone would leave a grant unable to say *which* of
the two it means, and a ledger unable to record which one sent — so
the name is minted per start.

That is the test-surface counterpart of a discipline the kernel
already keeps: the kernel distinguishes those same two runs by its
own per-run token (INV-8), and a `RunName` is the driver's way of
referring to the same distinction. It is not a second kernel
identity, and the kernel's own identity model is untouched —
RFC 0005's `CommandId` and `SubscriptionId` for the kinds that have
one, kernel-side scope membership without a logical key for the ones
that do not (RFC 0013 §9's third resolution).

Three rules, and they now hold for every kind rather than for
anonymous runs alone:

- **Minted from an observation.** The driver mints a `RunName` when
  it observes a run start, and reports it in that step's `started`
  list — the only place one is minted. A run is nameable only after
  the kernel has started it; there is no way to name one in advance,
  and nothing about the run is chosen by the test.
- **It reaches no kernel identity surface.** A `RunName` is not a key
  and participates in no keyed semantics: no keyed capacity, no move
  into or out of a gauge count — each kind stays counted as its own
  kind (RFC 0014 §9 row 9) — and no admission, cancellation, or
  teardown decision reads it. For a keyed or subscription run it
  carries the identity the kernel already holds, readable through
  `kind`, and adds nothing to it; for an anonymous run it adds no key
  where the kernel has none, so the auto-keying RFC 0013 §10 rejected
  stays rejected.
- **Its only uses are naming.** It names a run to `grant` and tags
  ledger records (§9.6). The send gate `grant` releases at is a
  kernel-side seam, RFC 0014 §7.2's second driving differential, not
  a driver-side queue; what crosses the grant boundary into the
  kernel is the run the kernel already holds, which the driver
  resolves the name to there. The name itself stops at that boundary.

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
immediate release. `grant(run)` arms a release at one run; the
returned `GrantToken` correlates to the commit that release produces,
and `confirm` consumes it once that send's acceptance — the post-send
acknowledgement — is confirmed.

**What the gate covers**, stated here because the rest of this section
quantifies over it: **every** send a producer run makes, on either lane.
RFC 0014 §3.1 splits producer output in two — message output on the data
lane, and the one producer output that does not travel it, a
producer-originated quit, on the control lane — and the gate holds both.
Three things follow. §9.5's bootstrap claim that no producer output
reaches *either* lane before a grant is complete rather than
data-lane-only, because a producer-originated quit is gated too. A
producer's quit is scriptable exactly like its messages — the pass-unit
series RFC 0014 §13.1 names for both quit semantics is driven by granting
the quit's own send. And `accepted` records accepted producer sends from
both lanes, each record carrying the lane alongside the run that sent it,
so a test can tell a released quit from a released message.

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

**Which intent a grant releases.** The next one at that run that
no grant has released yet: if an intent is already waiting at the
gate when `grant` is called, that is the one; if none is, the grant
stays armed and releases the first to arrive after it. A grant is
therefore issuable ahead of the producer that will satisfy it, which
is what lets a script fix an order before the run reaches its send
point.

Three rules carry INV-RC14's enqueue-order guarantee onto the
surface:

- **One outstanding grant, driver-wide.** At most one grant is
  outstanding across the whole driver, not one per run: while a
  token is unconfirmed, `grant` returns `Err(GrantOutstanding)` at
  issue time whatever run it names. The sequential handshake
  *grant → enqueue-acceptance confirmed → next grant* is therefore
  the only way two releases can be ordered at all, and raw grant
  order is not expressible — neither `grant(A); grant(B)` across two
  producers, which is the model RFC 0014 §11 excludes, nor two
  releases at one run. A per-run rule would admit the first of
  those, so the rule is driver-wide.
- **Detached.** The token is `'static`: it borrows neither the driver
  nor the script, and it carries its correlation with it rather than
  as a loan against the driver. That is what RFC 0014 §13.3's
  **driver progress** condition needs — a grant whose resolution
  requires the kernel to drain the lane its send waits on cannot be
  resolved without stepping, so the test must be able to hold the
  token *across* `step_pass` calls, and every `TestDriver` driving
  method takes `&mut self`. A borrowing token could not survive one
  of those calls, which is precisely the shape that cannot satisfy
  the condition. `confirm` is the in-place form for the case that needs
  no pass; like every wait here it is bounded and fails rather than
  hangs. Where a drain is needed the resolution happens inside a
  `step_pass` instead, which is the same route by that other name.
- **The guarantee starts at the gate.** Scripted enqueue order is a
  claim about sends the gate has released and confirmed, never about
  the order producers reached the gate in.

RFC 0014 §13.3's **ack correlation** condition reads, in full: "at most
one outstanding grant per origin, or an explicit correlation of each
grant to its exact commit; the next grant to an origin only after the
previous acceptance." The driver-wide rule above satisfies the first
disjunct strictly — one outstanding grant driver-wide implies at most one
per origin however coarsely "origin" is read, this section's runs being
the finest such grouping — and `GrantToken` supplies the second, being
correlated to the commit its release produces. The trailing clause holds
as written wherever a grant resolves by acceptance: no grant at any run
is admitted while a commit is still uncorrelated. Where a grant resolves
as `Confirmed::Reclaimed` there is no commit to correlate, so the clause
has nothing to range over, and **that reading is fixed in §9.12**, which
resolves RFC 0014 §13.3's bounded half: the next grant follows the
previous grant's resolution either way, and a reclaimed one leaves no
commit to correlate. This section itself neither narrows nor widens
that condition, stating no reading beyond the acceptance case its
letter covers.

**Grant lifecycle, in full.** A grant at a run the kernel's
bookkeeping does not currently hold — a run never started, or one
whose exit a pass has already reflected — is misuse and fails the
test: it is a script error the kernel cannot produce an outcome for,
and an error return would let a test go on scripting against a run
that is gone. `settle` is misuse while a grant is outstanding,
stranded or not, for the reason its own paragraph gives. `confirm`
and `grant` after termination are misuse under §9.3's state table,
like every other driving call.

**A grant ends in one of exactly two states, and `confirm` reports
which.** The subject is the grant, not any particular send, because a
grant can be armed at a run that never presents one. Either the
send this grant released **gets into the lane** — it is admitted, and
the guaranteed sequence gains its entry — or **this grant will never
put anything into the lane**. Two facts establish the second, and
either suffices: the send it released ended without getting in (its
producer reclaimed at an await point, or stopped after observing
closure — RFC 0014 §6.1), or the granted run's exit is reflected
in the kernel's bookkeeping and this grant released no send at all,
so there is nothing left that could arrive. The outcome is what this
section pins; RFC 0014 §10's reservation-and-commit accounting is one
informative way to reach it, not the contract.

Those two outcomes are disjoint and exhaustive: there is no third *end*
for a grant — though one may stay unresolved for as long as the lane
makes a send wait, which is why `confirm` carries a budget rather than a
promise (below). `confirm` drives until one of the two is reached and
returns `Confirmed::Accepted` or `Confirmed::Reclaimed`. Both clear the
outstanding grant, so `grant` and `settle` are legal again after either.

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
cancellation RFC 0014 §6.1 and RFC 0011 §4.4 make contract — a grant
outstanding at that run still lands in one of the two states
above. *When* an abort falls relative to an in-flight send is
mechanism, and this surface does not depend on it: either the send
got in before its producer ended, or it did not and nothing more will
follow it.

The disjunction is `confirm`'s *completion* condition, not a promise
that one of its arms arrives inside the budget. `confirm` begins no
pass, and two of the ways a grant ends need one, so in both the test
steps first and confirms after:

- **A commit that needs a drain.** Where the send waits on lane
  capacity, only a `step_pass` can drain the lane ahead of it (the
  RFC 0014 §13.3 driver-progress form).
- **The second reclaiming fact.** A run's exit reaches the
  kernel's bookkeeping at stage 1 of a pass (RFC 0014 §3.5), so a
  grant armed at a run that has exited but whose exit no pass has
  reflected cannot reach that fact inside `confirm` at all: the test
  interposes a `step_pass(WakeSource::ProducerExit)` and confirms
  after it.

In both, a `confirm` issued too early exhausts its budget and fails
like every other wait here, and the fix is the same and needs no
change to this contract: the token is detached, so it survives the
step that makes the resolution reachable.

This is a sanctioned observation of a state the *kernel* produced,
and it is deliberately not the same thing as a **stranded token** — a
token dropped without `confirm`, which leaves its grant outstanding
so that every later `grant` returns `Err(GrantOutstanding)` and every
`settle` is misuse until the test ends. That one is a test-author
error with no kernel state behind it, and it stays recorded as the
misuse pattern that error most often means. One boundary on the
reclaimed resolution is worth stating, because the case that first
comes to mind is not an instance of it: where **termination** is what
ends the run, the driver is in its terminated state (§9.3) and
`confirm` is misuse there, so a shutdown-time reclamation is not
something this resolution reports. What earns the resolution its
place is that the state is reachable at all while the driver still
runs, which is the argument §9.11's *Unresolvable grant* model
carries.

**`settle` is the call that contracts turns.** Some runs finish without
ever presenting a send-intent — a cleanup finalizer, whose `Output = ()`
closes the message path outright (RFC 0014 §4.4), a future that completes
with no message, a subscription run stopping after its last output. No
grant releases them, and no pass *guarantees* them anything: a pass turns
the executor and may advance a runnable producer incidentally at an await
point, but it promises no turns at all — a pass that never awaits yields
none. That is the gap `settle` closes, and its whole content is the three
things a by-product cannot offer: turns as the *purpose* of the call, and
a budget and completion condition the caller states. It begins no pass
and releases no send-intent, and an exit it lets a run reach becomes
visible the way every exit does, at the exit-reflection stage of the next
`step_pass(WakeSource::ProducerExit)`.

**What a turn is, normatively — and it is defined by construction.**
One **turn** is the driver task spawning a fresh no-op task onto its
own executor and awaiting that task's completion, suspending until it
resolves. Nothing but public primitives: a spawn and a join. The
definition is a construction rather than a property because the
property one would rather state — that every task ready at the yield
gets the executor first — is not something the executor's public
contract offers. There is no ready-set observation, and a bare yield
may re-poll the yielding task immediately; the only way to *assert*
that property would be to instrument the scheduler, which RFC 0014
§7.2 forbids outright ("production scheduling stays uninstrumented").
Spawning one ordinary task is not instrumentation — it adds no hook
and reads no scheduler state.

What the construction buys is the thing the property was wanted for:
two conforming drivers build the same turn, so on the same executor
they produce the same observation sequence, and a test cannot pass
under one and fail under the other.

*Informative, not contract:* on the current-thread executor of §9.8's
verified range, a FIFO ready queue means a turn constructed this way
does in practice let the tasks ready at the spawn run before the join
resolves. That is an observation about that executor, not a promise
this section makes, and nothing outside the verified range is claimed
at all. The normative content stays what the construction pins, and
RFC 0011 §2.3's unspecified producer scheduling is untouched: a turn
is a unit of *opportunity*, not of progress. It says nothing about
how far any task runs, in what order tasks are picked, or whether
one completes.

**Both waiting calls take their budget from the caller.** `settle`
and `confirm` each take `max_turns`, spend at most that many turns,
and fail the test with the count consumed when they run out. The
reason is the one the turn definition serves: a driver-chosen budget
is a mechanism two implementations may pick differently, and a test
that passes under a bound of three and fails under a bound of one
would make conformance depend on it. That is as true of `confirm` as
of `settle` — a producer granted a release may take several turns to
reach its send, so a bound of one can report exhaustion where a bound
of three reports `Accepted`, on the same finite execution. Naming
both budgets at the call site puts them in the script (§9.8) instead.

What differs between the two is only the completion condition, not
who owns the budget: `settle`'s is the caller's predicate, and
`confirm`'s is the gate's — the grant resolving one way or the other
(§9.6's two states).

**Both check that condition before the first turn**, so a call that
has nothing to wait for spends nothing. A grant already resolved —
by a `step_pass` that drained the lane ahead of its send, say —
returns from `confirm` at once, and `confirm(0, token)` is therefore
the way a test asserts exactly that: it succeeds on an already
resolved grant and fails on exhaustion otherwise. `settle(0, until)`
reads the same way against its predicate. Leaving this open would
have split conformance twice over — on whether `confirm(0, token)`
can succeed at all, and on whether the extra turn a resolved grant
did not need moves the intent ledger or carries a run to an exit.

The predicate is supplied rather than fixed because the obvious fixed
condition, "until the executor is idle", is not a thing this contract
can observe: idleness is exposed by no surface here, and
implementations disagree about what it means, so a `settle` defined
that way would differ between two conforming kernels. A predicate
evaluated at the boundaries above is deterministic instead.

What a predicate can see is what the test can see, which is
ordinarily its own application-side instrumentation: a cleanup
finalizer that sets a flag, a mock source that records its stop. It
is deliberately *not* a run's exit as the driver knows it — an exit
reaches the driver only at a pass's stage 1 (§9.6's completion
condition above), which `settle` does not run.

Since turns are not selective (§9.3), what `settle` does to the two
ledgers is stated exactly rather than denied. It **initiates no append to
the guaranteed sequence**, and that holds structurally rather than by
intent: `settle` is misuse while a grant is outstanding, no send is
released except through an outstanding grant, and a grant stays
outstanding until it resolves — so during a legal `settle` there is no
armed gate and no released send still in flight, and nothing can get into
the lane. The **intent ledger may gain entries**, on the other hand, from
any producer the turns advance to a send point — as it may during any of
the other three driving calls (§9.3). That is what a non-guaranteed
pre-gate ledger is for, and a test reading `intents` after a `settle` is
reading exactly the kind of record this section declines to guarantee.

Two ledgers divide at the send gate. `accepted` records the sends
admitted past it, each record carrying the run that sent it and the lane,
in gate order: the guaranteed observation sequence INV-RC14 scopes, which
RFC 0014 §7.2 begins at the gate for exactly this reason. Admitted is not
delivered — a record says an item passed the gate and says nothing about
whether `update` ever saw it, which is why a revoked run's committed send
belongs in it. That order is the *driver's*, established by the sequence
of grants, and cross-lane it is nobody's claim about production: RFC 0014
§3.3 declines to order a run's own control-lane quit against its earlier
data-lane output at all, so a reading that puts one before the other is
the citation rule's ordinary case (§9.9). `intents` records send-intents
before the gate, tagged the same way: pre-gate records, deliberately
outside the guarantee. A test may read `intents` to see that a producer
reached the gate; it may not derive an order or a completeness claim from
them.

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

The future it polls comes from the production entry point, not from
this section: the test calls `ProgramRuntime::run` itself and pins
the future that call returns, which is the "polls the production
driving future directly" of RFC 0014 §7.2. No `TestDriver` hands one
out, and none could — §9.2's manual-effect-polling rule turns on
exactly that.

What the probe supplies is a waker and a poll, and nothing else. It
polls that future directly; it scripts nothing inside the kernel,
adds no branch, and is neither a third runtime seam nor a second
driver. Its whole surface is that poll and a count of the wake-ups
its waker has received.

**Which source armed and which woke is established by the script,
not reported by the probe.** RFC 0014 §7.2 says the probe observes
whether the loop parks, which sources it armed, and which arrival
wakes it — and those are observations, not accessors. Each is
established the way RFC 0014 §13.1 already requires its three series
to be built, "scripted from a genuinely parked kernel with no other
work pending":

- **That it parked**, in two stages, over what this execution
  actually has to observe: a re-poll returns `Pending`, the wake
  count does not move, and the application's own instrumentation
  stays silent — no journal entry, no `view` call. Then, after the
  arrival, the count moves exactly once. The ledgers are not among
  these witnesses and could not be: a probe series polls the future
  from `ProgramRuntime::run` and no `TestDriver` is in play, so
  neither ledger exists in that execution.
- **Which source**, by construction: the script arranges the arrival
  of one source and no other, so the wake it observes can have come
  from nothing else. That the kernel had armed that source is what
  the wake witnesses.

An accessor form is not available, and the reason is worth recording
because it is a fact about the kernel rather than a choice here. The
park registers **one** waker across both lanes and the exit
notifications, and a wake carries no tag saying which of them fired;
recovering a per-source answer would take per-source instrumentation
inside the kernel — a branch — and RFC 0014 §7.2 says of this
instrument that it "scripts nothing inside the kernel, adds no
branch". An `armed`/`woken_by` pair would have been that branch.

This is a surface *reduction*, and it costs INV-RC16 nothing. The
invariant's enforcement was never these accessors: its arming half is
structural at the park site — no finite test proves a registration
for a source it did not exercise — and its behavioral half is the
three series, one per armed source, which the construction above
carries exactly as before.

**Its evidence scope is INV-RC16's arming and wake claims, and
nothing else.** A `ParkProbe` observation is never evidence for
INV-RC13's same-topology claim, for INV-RC14's scripted determinism,
for RFC 0014 §3.5's pass stage order, or for production pass
initiation. RFC 0014 §13.1 names the three series it carries; every
other series is pass-unit driven.

### 9.8 Determinism, scoped

A **script** is the ordered sequence of driving calls, with each call's
own arguments — `boot`, each `step_pass`'s `WakeSource`, each `grant`'s
run and its paired `confirm` with that call's `max_turns`, and **each
`settle`'s predicate and `max_turns`** — together with the
application-side inputs and readiness (§9.2). `boot` is listed for
completeness rather than as a choice: §9.3's state table admits it in
exactly one position, so it is not a free variable.

These arguments are free variables and are named as such, because
they are. A budget is one: the same call at the same position
resolves or exhausts depending on the number given, so two runs
differing only in a `max_turns` are two scripts. A predicate is
another: `settle(n, || true)` and `settle(n, || done())`
placed identically take different numbers of turns, and turns are
observable — they can grow the intent ledger (§9.3) and carry a run
to an exit that a later `step_pass(WakeSource::ProducerExit)` has
something to reflect. Two runs differing only in a predicate are
therefore two scripts, and calling them one would have put INV-RC14's
guarantee over a set that does not determine its own observation
sequence.

Stated honestly against RFC 0014 §7.2's tuple: that tuple is inputs,
readiness, arbitration choices, and grants, and this section already
went past it when it added `settle` to the driving vocabulary — a
call §7.2 does not name. The predicate and budget ride that same
extension rather than a second one. What the extension does not do is
weaken §7.2: every element of its tuple is still a script element
here, the guarantee is still one observation sequence per script, and
the additions are constrained by the same rule as the rest — a
`settle` contributes no entry to the guaranteed sequence (§9.6).

The determinism this preserves is worth deriving rather than
asserting. Under a fixed script, the application-side state evolves
deterministically: its inputs and readiness are script elements, and
the driving calls that advance it are fixed in order and arguments. A
predicate is a function of that state, evaluated at the turn
boundaries §9.6 defines. So the sequence of predicate evaluations —
and with it the number of turns each `settle` takes — is a function
of the script, not a further degree of freedom, which is exactly what
naming the predicate as a script element buys.

For a deterministic application, one script yields one observation
sequence across repeated runs, because the driver introduces no
nondeterminism of its own (INV-RC14). As in INV-T4, the claim scopes
to what the mechanism contributes: an application whose own reduction
is nondeterministic is outside it.

Two bounds on that claim, both RFC 0014 §7.2's and neither weakened
here:

- **Enqueue order is guaranteed only through the handshake** — grant,
  confirmed acceptance, next grant (§9.6). Raw grant order guarantees
  nothing, and is not expressible.
- **The verified range is a current-thread executor**, on either lane
  mode: unbounded as this section states it, and bounded as §9.12
  records after that extension's verification pass. The claim is
  scoped to that executor.

**The executor-independent extension stays open.** Extending the
determinism claim past a current-thread executor needs its own
verification pass and the two protocol conditions RFC 0014 §13.3
names: **driver progress** — the driver stays steppable while a
grant's acceptance is outstanding, so a capacity-blocked send cannot
deadlock the handshake — and **ack correlation** — at most one
outstanding grant per origin, *or* an explicit correlation of each
grant to its exact commit, with the next grant to an origin only
after the previous acceptance. §9.6 quotes that second condition in
full and answers its letter for the case that letter covers, a grant
resolving by acceptance; how the trailing clause reads where a grant
resolves with no commit at all is part of what §13.3's own resolution
fixes, and §9.6 settles nothing about it. This section's surface is
shaped to satisfy both conditions as far as that letter reaches
(§9.6's detached token and its driver-wide admission rule), but a
shape is not a verification. That verification has since run for one
of the two extensions §13.3 names, and §9.12 records it: the claim
now reaches **bounded lanes** on a current-thread executor, while
**executor-independent scheduling** keeps the verified range above
and stays open. §9.12 also fixes the trailing clause's reading for a
grant that resolves with no commit, which is the part §9.6 deferred.

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
  reached inside a `step_pass`, where it needs the kernel to drain the
  lane the send waits on (the RFC 0014 §13.3 driver-progress form); or a
  `Confirmed::Reclaimed` resolution, which appends nothing because no
  commit occurred. Of those three, one always needs a pass before the
  `confirm` that reports it — the commit that needs a drain — and
  another needs one when its establishing fact is a run's exit,
  which reaches the bookkeeping only at a pass's stage 1 (§9.6). The
  entry, where there is one, becomes evidence when the pass-unit step
  that delivers it consumes it; the handshake itself witnesses nothing
  about production. `settle` contributes nothing to that sequence — it
  initiates no append to it (§9.6), and the exit it lets a run reach is
  evidence only once a `step_pass(WakeSource::ProducerExit)` reflects
  it. Any pre-gate record these calls produce is outside the guaranteed
  sequence by construction, which is what makes the intent ledger
  inadmissible here rather than merely unreliable. None of the three is
  a second evidence surface beside pass-unit driving, and none is a
  stage of a pass: they leave the stage order untouched, which is why
  they do not fall under the probe exclusion below.
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
structural review re-runs at the store's intake site now that the
parts carry them.

The rest of the extension was a **named delegation**, recorded here
rather than drafted here, and its owner was the change that landed the
kernel-side lowering — the store's half landing in that same change and
not before it, so this document and the kernel never stated different
lowering semantics at once. What that change had to fix, in full:

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

Writing those edits before the kernel landed would have stated a
contract the crate did not implement, which is why they were delegated
rather than made when this section was written.

**Discharged.** The kernel-side lowering landed and all three are
made. A teardown entry selects the store's pending leaves by scope
prefix, over every kind, as §5.1 now states. A cleanup registration
arms against its scope and is run by the teardown that selects it: the
store spawns no task, so it starts the finalizer where the contract
says it starts and reports what it observed — an unfired registration
and an unfinished cleanup run are both leak classes in §6, since a
hook's external side effects are its whole purpose and a script that
ends without them ended somewhere it did not say. Termination remains
not a teardown: an observed quit discards unfired registrations with
everything else the shutdown discards. The INV-11 and INV-14 readings
are re-stated as historical throughout, in the places this list
enumerated.

### 9.11 Coverage, models, and excluded claims

**Surface–invariant coverage.** This section introduces no invariant.
The driving contract's invariants are RFC 0014's, and every element
of §9.3's block maps to one of them, walked in order:

- `TestDriver::new` and §9.2's absent constructors → INV-RC13, whose
  declared structural half is exactly this API-surface review; the
  `TestDriver` driving methods' uniform `&mut self` receivers and
  its sole ownership of its kernel instance additionally hold RFC 0011
  INV-LC9's exclusivity property, which that RFC's §6 requires any
  step-style surface to preserve.
- `step_pass`'s unconditional pre-pass turn → INV-RC13, as the third
  driving differential §9.2 records; it is inside the same
  API-surface review, and the structural fact it rests on is that the
  pass implementation takes no argument distinguishing its caller.
- `boot`, `step_pass`, `WakeSource`, and `NotReady` → INV-RC13's
  behavioral half, which runs through pass-unit steps against the
  production seams; `boot`'s whole-bootstrap granularity additionally
  serves INV-RC11's init-quit row (§9.5, §9.9).
- `grant`, `GrantToken`, `GrantOutstanding`, and `confirm` with its
  `max_turns` → INV-RC14, whose structural half is that the raw-grant
  shape is unrepresentable — which is what the driver-wide outstanding
  rule delivers (§9.6). `Confirmed` maps to INV-RC14 through both arms:
  it reports which of the two states a grant ended in, and only the one
  that got a send into the lane appends to the gate-scoped sequence. It
  maps to INV-RC5 through *neither* — strict revocation is a
  delivery-side property, so neither ledger witnesses it. Its behavioral
  rows observe that `update` never runs for the revoked item, which the
  reducer under test records for itself; the dequeue that drops it does
  no `update` work at all (RFC 0014 §4.3), which is why there is nothing
  for a record of this section's to hold.
- `try_confirm` → INV-RC14, negatively: it reports the gate's
  terminal without driving, so it can neither append to the
  guaranteed sequence nor consume a grant, and it exists because a
  bounded-lane series needs to distinguish "not resolved yet" from
  "resolved and not yet reported" without spending a turn (§9.12).
- `on_worker_threads` → INV-RC13 for the topology it constructs, and
  to no determinism claim at all: it drives outside §9.8's verified
  range by design (§9.12).
- `settle` and its completion predicate → INV-RC13: it drives the
  production executor and adds no seam, so it is covered by the same
  API-surface review. It reaches INV-RC14's observation sequence only
  negatively, by initiating no append to it — a property §9.6 makes
  structural through the rule that `settle` is misuse while a grant
  is outstanding. Its predicate and budget are free variables of a
  script, named as such in §9.8, and its turn is defined normatively
  in §9.6 so that two conforming drivers cannot disagree on what one
  call does.
- `accepted`, `intents`, `AcceptanceLedger`, `IntentLedger`,
  `SendRecord`, `Lane` → INV-RC14's gate-scoped observation
  sequence, whose pre-gate exclusion is what the second ledger keeps
  separate. The read surface — a length and an ordered walk over
  records that name their run and lane — is what makes that sequence
  assertable at all; without it the invariant would have a contract
  and no instrument.
- `ParkProbe`, its `poll`, and its wake count → INV-RC16, whose
  behavioral rows sit on that probe. The arming and wake observations
  RFC 0014 §7.2 names are established by how a series is scripted
  rather than by any accessor, for a reason about the kernel rather
  than a choice here (§9.7).
- `StepReport::terminated` → INV-RC11 (the production result
  contract, RFC 0011 INV-LC5's, preserved).
- `RunName`, `RunKind`, and `StepReport::started` → no invariant of
  their own: the identity models are RFC 0005's, and the name's
  confinement falls inside INV-RC13's structural review, which walks
  the API for surfaces reaching the kernel. Naming per run rather
  than per identity is what lets INV-RC14's sequence stay assertable
  across a supersession, where one identity has two runs (§9.4).

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
  content (send events by run, not messages) together with §9.9's
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
  is that it initiates no append to the guaranteed sequence. §9.6's
  turn definition closes it from the other side, and by construction
  rather than by promise: a turn is a spawn and a join, which gives
  the driver no per-task control to be selective *with*.
- *Identity as run* — naming runs by `CommandId` or `SubscriptionId`
  alone passes every single-run test and then silently conflates a
  superseded run with its successor: the identity slot is free for
  the successor at the revocation's application point while the old
  run can still send late (RFC 0014 §3.1), so a grant could not say
  which it meant and a ledger record could not say which had sent.
  Excluded by §9.4's per-run naming, which mints one name per start
  and carries the logical identity inside it rather than as it.
- *Unresolvable grant* — a surface whose grant clears only on a
  commit leaves the driver permanently unusable in two reachable
  cases, neither of them a test-author error: a released send that
  ends without getting into the lane, and a grant armed at a run
  that exits before presenting a send at all — the second reachable
  by granting at a run whose exit `settle` has induced but no pass
  has yet reflected. Either way `confirm` would exhaust its budget,
  `settle` would stay barred, and every later `grant` would be
  refused. Excluded by §9.6's two states, which are stated over the
  *grant* rather than over a send and so cover both; a budget
  exhausted before either arrives still fails the test, and the
  stranded token stays a dead end because it *is* a test-author
  error.
- *Acceptance read as delivery* — reading an entry in `accepted` as
  proof that `update` saw the item. A revoked run's send can commit
  and be recorded, and is then dequeued with no `update` work at all
  (RFC 0014 §4.3). The accessor's name carries the distinction now
  rather than leaning on prose, and §9.6's ledger paragraph scopes
  the record to admission besides; §9.11's mapping puts INV-RC5's
  checks on the pass rather than on either ledger.

**Excluded claims**, per the checklist's minimal-contract item: no
INV-T-numbered restatement of INV-RC13, INV-RC14, or INV-RC16 is added —
a second statement of an invariant owned elsewhere is one a later
amendment can drift from, and the surface above maps to the originals
instead; no exhaustiveness or leak-check rule is stated for the driver,
because what becomes of undelivered output is the kernel's own revocation
and termination contract (RFC 0014 §3.1, RFC 0011 §4.4) and a store-style
pending set does not exist here; no correspondence between `started`'s
order and a command's declaration order is claimed, that lowering order
being RFC 0014 §3.4's to state; and no bounded-lane determinism claim is
made (§9.8). **No per-source arming or wake accessor is offered on
`ParkProbe`**: the park registers one waker and a wake carries no tag, so
reporting which source fired would take a branch inside the kernel that
RFC 0014 §7.2 forbids this instrument — the observation is
script-established instead, and INV-RC16's enforcement is unchanged by
the reduction (§9.7). **No render-observation surface is offered
either**: the driver owns its terminal and never hands it back, and
`StepReport` carries `started` and `terminated` and nothing about frames,
so no call here reports that a render happened or what it drew. Render
evidence is application-side, where a `view` under test can record its
own calls, and that is where INV-RC10's redraw row is read — its own
enforcement line calls for "a scripted flood with an interposed probe and
a pending redraw", and the interposed probe is that application-side
instrument, not a driver method. This is deliberate negative space rather
than an omission: a frame observation on the driver would be one more
thing the surface reports about a pass, beside the differential
RFC 0014 §7.2 confines it to.

Two elements were suspected of redundancy and kept, neither implied
by its suspected survivor. `confirm` against `step_pass`, which also drives
the executor: stepping runs a whole pass, so confirming an acceptance
through a step would put a pass between two grants and change the
very enqueue order the handshake exists to script. And `settle`
against `confirm`, which also drives the executor without a pass:
`confirm` requires a token to consume, so a test whose only
outstanding work is a run that never sends has no token to confirm
and no way to reach that run at all.

### 9.12 Bounded-lane determinism: verified, and what stays open

RFC 0014 §13.3 asks for two extensions of §9.8's determinism claim,
bounded lanes and executor-independent scheduling, each gated on its
own verification pass and on that section's two protocol conditions.
One of the two has been verified; the other has not, and they are
recorded separately because they are separate claims.

**Bounded lanes: verified, and the claim extends.** For a
deterministic application on a **current-thread executor**, one
script yields one observation sequence with the data lane bounded,
exactly as §9.8 states it for the unbounded case. Both of §13.3's
conditions are met by execution rather than by shape:

The pass itself is a replay row: a one-slot lane and two keyed
producers, scripted so that every send but the first finds the lane
full, replayed for one observation sequence per script and again for
the reversed script. Two faces are what make it a claim rather than a
description — a run that ignored the handshake and drained in arrival
order would produce the same sequence for one script alone. The
capacity mechanism sits inside the replayed region: each wait is
resolved by the pass that drains the item ahead of it, so what
replays is the bounded behaviour and not an unbounded script over a
lane that merely happens to be bounded (which is the neighbouring
headroom row's job).

- **Driver progress** — every grant after the first is issued onto a
  full lane and acknowledged only after the `step_pass` that drains
  the item ahead of it, with the token held across that step. That is
  the steppability the condition asks for, and it is inside the
  claim rather than beside it. Three further rows drive the blocked
  send's other faces: a release onto a full lane that does not
  commit, an acknowledgement that arrives only after a dequeue frees
  capacity, and a revocation that frees no capacity for a waiting
  send.
- **Ack correlation** — the token outstanding across that step is the
  only grant outstanding anywhere on the driver, which is the
  driver-wide rule of §9.6 doing the correlating, and `try_confirm`
  between the release and the drain separates "the dequeue freed the
  slot" from "the dequeue committed the send". Without that
  separation a release that would have committed anyway reads the
  same as one the dequeue released, and the row would witness
  nothing.

**Executor independence: still open.** The multi-worker constructor
(§9.3) drives what the current-thread range excludes, and what it
establishes is deliberately narrower than determinism: the handshake
holds there, but no one-script-one-sequence claim is made for it, and
none is derivable from those runs. §13.3's second extension therefore
stays open, with §9.8's verified range unchanged for it.

**The trailing clause, for a grant that resolves with no commit.**
§13.3's ack-correlation condition ends "the next grant to an origin
only after the previous acceptance", and §9.6 answered its letter
only where a grant resolves *by* acceptance, leaving the reclaimed
case to this resolution. It reads: **the next grant follows the
previous grant's resolution, whichever of the two it is.** Where the
resolution is `Confirmed::Reclaimed` there is no commit to correlate,
so the correlation obligation is discharged vacuously — there is
nothing outstanding for a later grant to be confused with, which is
the whole of what the clause protects. Reading it instead as
requiring an acceptance would make a reclaimed grant unfollowable and
strand the driver, which is the state §9.11's *Unresolvable grant*
model excludes. RFC 0014 §7.2 states the same condition and closes it
with a citation to §13.3, so that citation now reaches this reading
and the two documents say one thing.

**What this does not extend.** Bounded-lane *revocation* remains what
RFC 0014 §13.1's series of that name witnesses — INV-RC5 under a
bounded lane — and nothing here widens it. The determinism claim's
other bounds are untouched: enqueue order is still guaranteed only
through the handshake (§9.8), and the guaranteed sequence still
begins at the send gate (§9.6).

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
- RFC 0003 — command cancellation: its §4.2 occupancy accounting, its
  §5.1 explicit-cancels-before-keyed-spawn ordering, and INV-1,
  INV-3, INV-4, INV-5, INV-6, INV-7, INV-9, INV-10 (cited in §§4.1,
  4.2, 5.1, 5.3 of this document). Two of the invariants this document
  once read parity off are superseded and are cited historically where
  they appear: INV-11's batch child-key folding (RFC 0014 §3.4 — each
  child keeps its own key) and INV-14's shared-first pull (RFC 0014
  §3.2 — one FIFO lane, no second class to prefer). Neither changes what
  the store does; both change what a test scripted over it may be cited
  as evidence of.
- RFC 0005 — structural lifecycle identity: `SubscriptionId`, the
  declared-set semantics `subscription_ids` observes; with `CommandId`,
  the two identity types §9.4's run names carry.
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
  of auto-keying, which §9.4's run naming honors.
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
