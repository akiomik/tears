# RFC 0011: Runtime Lifecycle

- Status: Draft
- Target: 0.11.0 — one behavior change (construction no longer starts the
  init command's effect, §3.4); public signatures unchanged
- Scope: the runtime's steady-state phase order, the bootstrap contract,
  the termination model (controlled and abrupt routes, with two-stage
  postconditions), panic containment for runtime-owned tasks, and driver
  exclusivity
- Feature flag: none
- CHANGELOG: `Changed` — constructing a `Runtime` no longer starts the
  init command's effect; it starts inside `run()` (§3.4), observable only
  to code that constructs a runtime it does not run. The entry lands with
  the implementation.

## Summary

Every RFC so far assumes a runtime life cycle none of them states: that
rendering happens at frame granularity rather than per message, that a
frame pass puts its render ahead of its subscription reconciliation and
runs both against the current state, that quitting tears the runtime
down. This RFC is the owner of that contract. Five decisions:

1. **Steady-state phase order** (§2, INV-LC1/INV-LC2). Processing is
   organized into input batches and frame passes; their interleaving is
   unspecified (§2.3). A batch processes inputs one at a time (RFC 0003
   INV-10) and records its outcome as pending work; a frame pass
   consumes it in a fixed order — render first, then subscription
   re-evaluation — with both steps observing the pass's current state,
   so whenever a redraw was pending at a frame pass, subscriptions start
   against a state that same pass has just rendered.
2. **Bootstrap** (§3, INV-LC3/INV-LC4). Constructing a `Runtime` is
   inert: no runtime-owned task is spawned, no command effect is polled,
   no subscription source starts. Inside `run()`, the init command is
   dispatched before the initial subscription reconcile, and the first
   render starts out pending — eligible, not promised. `Application::new`
   is user code and runs at construction; this RFC claims nothing about
   its side effects, and a panic inside it happens before a runtime
   exists and is outside this contract.
3. **Termination** (§4, INV-LC5–INV-LC7). Termination has two routes —
   controlled (unkeyed quit, keyed quit, render error: the loop exits
   with a reason and the shutdown postconditions hold by `run()`'s
   return) and abrupt (drop of the `run` future, a panic unwinding
   through `run` from application code on the driving task, drop of a
   never-run runtime value: ownership teardown and cancellation
   requests complete synchronously by `Drop`; task futures follow on
   the quiescent stage). Both routes reach
   the same postconditions in two stages: an immediate stage at the
   terminating operation's completion, and a quiescent stage once the
   executor has processed the requested cancellations — task
   cancellation is a request, not an event that completes inline.
4. **Panic containment** (§5, INV-LC8). A panic inside a runtime-owned
   producer task — an unkeyed command task, a keyed command task, or a
   subscription forwarder — never terminates the application. A panic in
   the application's own code on the driving task is deliberately
   fail-fast (the abrupt route above).
5. **Driver exclusivity** (§6, INV-LC9). At most one owner drives a
   runtime instance at a time, and the state transitions —
   `update`, `view`, `subscriptions` — execute serially and
   non-reentrantly. Pinned as a property, not as the absence of an API:
   a future `step`/handle surface is additive exactly as long as it
   preserves it.

Mechanism — the task inventory, the keyed exit bookkeeping, the gauge
funnel, the single-task parking assumption — is recorded as informative
(§7) together with the premises other RFCs' invariants rest on.

## 1. Scope

### 1.1 In scope

- The steady-state phase order and its negative space (§2).
- The bootstrap contract: inert construction, in-`run()` ordering, first
  render eligibility, and what TestStore's construction maps (§3).
- The construction-dispatch behavior change, as a named deliverable
  (§3.4).
- The termination model: routes, causes, two-stage postconditions, and
  the render-error no-divergence analysis (§4).
- Panic containment for runtime-owned tasks (§5).
- Driver exclusivity and non-reentrancy (§6).
- The premises this contract's enforcement and other RFCs' invariants
  rest on, recorded as informative (§7).

### 1.2 Out of scope

- **Intra-batch semantics.** One-item drain is RFC 0003 INV-10, the
  micro-batch window is RFC 0003 §4.4, the count cap is RFC 0006
  INV-L12, shared-first pull is RFC 0003 INV-14. This RFC cites them
  and re-pins none of them.
- **Load control and delivery.** Channel capacities, backpressure,
  delivery ordering, and the load-observability schema are RFC 0006.
- **Subscription reconciliation semantics.** Which subscriptions run,
  restart, or stop is RFC 0005 and the `Application::subscriptions`
  documentation; this RFC pins only *when* reconciliation happens.
- **Graceful drain.** This RFC pins only that today's termination is
  the zero-grace degenerate form of a drain model (§4.5); a bounded
  grace period is future work with its own RFC.
- **Supervision surface.** Typed task-exit causes and any supervision
  event schema are future work. This RFC adds no new common diagnostic
  schema; diagnostic requirements existing owners state (RFC 0003,
  RFC 0006) stand unchanged (§5.1).
- **Quit vocabulary.** The shape of the public quit surface (whether
  quit remains an `Action` or becomes a `Command` constructor) is
  RFC 0002 §9's breaking-change track, orthogonal to the lifecycle
  semantics pinned here.
- **TestStore semantics.** RFC 0008 owns the store; §3.3 here only
  states which side of the bootstrap contract the store's construction
  maps.

## 2. Steady-state phase order

### 2.1 The cycle

After bootstrap (§3) and until termination (§4), the runtime interleaves
two kinds of passes, both executed on the driving task — which kind runs
next is §2.3's negative space, so no alternation or ratio between them
is guaranteed:

- **Input batch.** Inputs are processed one at a time: each message runs
  `update`, and the returned command is dispatched before the next input
  is pulled (RFC 0003 INV-10; window and cap per RFC 0003 §4.4 and
  RFC 0006 INV-L12). The batch records its outcome as pending work:
  redraw pending per RFC 0002's OR-fold over the batch, subscription
  dirtiness per RFC 0003 §4.4's at-least-one-`update` rule.
- **Frame pass.** Pending work is consumed at frame granularity and in a
  fixed order: the render step first (executed when a redraw is
  pending), then subscription re-evaluation (executed when subscriptions
  are dirty) — `subscriptions()` is called on the current state and the
  declared set is reconciled per RFC 0005.

Rendering and subscription re-evaluation are frame-phase activities:
neither runs inside an input batch, and one frame pass performs at most
one render and at most one re-evaluation (INV-LC1). This is what makes
render cost proportional to frames rather than messages (RFC 0002's
premise) and keeps reconciliation from running per message.

### 2.2 Render before subscription start

Within one frame pass the render step precedes subscription
re-evaluation, and a frame pass never begins subscription re-evaluation
while a redraw is pending (INV-LC2). Both steps of one pass observe the
same state — the runtime's current state when the pass begins; no batch
interleaves within a pass (§6). Consequently, whenever a redraw was
pending at a frame pass, the state whose subscriptions that pass starts
is a state the same pass has just rendered.

The contract is stated over the pass's current state, not over
individual states. Pending work does not queue per state, so a state
that requested a redraw is not itself promised a render: a later batch
can replace it before the next frame pass, and the pass then renders the
newer state — the earlier one is never drawn. (Adversarial model: batch
A requests a redraw, batch B runs `without_redraw` before the frame; the
redraw is still pending, and the pass renders the post-B state once
while the post-A state is never rendered — a compliant execution, and
the sequence INV-LC2's checks include for exactly this reason.) The
consequence is also scoped by suppression: a pass entered with no redraw
pending — every contributing batch opted out under RFC 0002's
`without_redraw` — re-evaluates subscriptions with no preceding render;
suppression suppresses the redraw, never the re-evaluation (RFC 0002
non-negotiable B). And a render step that fails is a controlled
termination (§4.1): re-evaluation never runs, which also satisfies
"never begins re-evaluation while a redraw is pending". (A second
adversarial model: an implementation that re-evaluates in one frame pass
and renders in the next would satisfy a bare intra-pass ordering claim
while starting subscriptions against an unrendered state — excluded by
the never-while-pending form.)

### 2.3 Negative space: arbitration

Which ready branch runs next — another input batch, a frame pass, or
quit delivery — is deliberately unspecified. No consumer may rely on a
particular interleaving of batches and frames, on an input observed
before a frame tick being processed before that frame renders, or on
any tie-break among simultaneously ready branches. This is the runtime
counterpart of RFC 0008 §4.2's citation rule (the store's canonical
order is not evidence of a runtime order). It is negative space, not
freedom from other contracts: the frame branch's pacing behavior and the
quit branch's latency acceptance are pinned by RFC 0006 (F5, INV-L4),
whose premises §7 records — an implementation that re-arbitrates these
branches is measured against those contracts, not against this section.

## 3. Bootstrap

### 3.1 Construction is inert

Constructing a `Runtime` (`Runtime::new` / `Runtime::with_config`)
spawns no runtime-owned task, polls no command effect, and starts no
subscription source (INV-LC3). A runtime that is constructed and never
run therefore executes none of the application's effects and none of its
subscription sources.

Two boundaries are explicit:

- `Application::new(flags)` runs during construction and is user code:
  it may perform side effects of its own, so this RFC claims "the
  *runtime* starts nothing", never "construction is side-effect-free".
- A panic inside `Application::new` unwinds out of the constructor
  before a `Runtime` value exists. There is nothing to clean up and no
  lifecycle to speak of; it is outside this contract entirely.

### 3.2 `run()` bootstrap order

Inside `run()`, before the steady-state loop:

1. **The init command is dispatched** — its cancel list is applied and
   its keyed identity, if any, is admitted (RFC 0003 §4.3), and its
   effect becomes eligible to run — before
2. **the initial subscription reconcile** starts any subscription
   source, and
3. **the first render starts out pending**: eligible for the first
   frame pass, unconditionally and independently of the init command's
   redraw directive, which the runtime never consults (the fact
   RFC 0008 §5.2 records).

The order pins intake, not execution (INV-LC4). Once the init effect's
task exists it may be polled immediately on another executor thread, so
no ordering is pinned among the init effect's first poll, an initial
subscription's first output, and the first render — bootstrap
arbitration is negative space exactly like §2.3's. In particular the
first render's *execution* is not promised: eligibility enters the same
arbitration, and an init-dispatched quit can win it, terminating the run
before any frame renders. What is promised is eligibility — a run that
reaches its first frame pass with the redraw still pending renders even
though no message has been processed.

### 3.3 What TestStore maps

`TestStore::new` maps the logical intake and accounting of this
bootstrap — it applies `Application::new` and enqueues the init command
with its metadata, cancel list, and keyed admission, exactly as RFC 0008
§3.2 states. It does not map the temporal side: runtime task start,
subscription start, and first render have no counterpart in the store,
and production offers no stable observable phase between a dispatch and
its effect's first poll for the store to map (the spawned task may be
polled immediately on another executor thread). A test must not read
store construction as evidence of production bootstrap *timing*; the
intake half is exactly what it is evidence of.

### 3.4 Deliverable: construction-dispatch removal

Today the constructor dispatches the init command itself
(`RuntimeCore::with_capacities`, `src/runtime/core.rs`), so a
constructed-but-never-run runtime spawns the init task and the init
effect runs — violating INV-LC3. The implementation moves that dispatch
into `run()`, ahead of the initial subscription reconcile, preserving
the init-before-subscriptions relative order §3.2 pins. Public
signatures are unchanged; the observable change is confined to code that
constructs a runtime without running it (or observes effect side effects
before `run()`), and carries this RFC's `Changed` entry.

## 4. Termination

Termination is not a single sequence. It has two routes, distinguished
by whether control is still inside the event loop; both converge on the
§4.4 postconditions.

### 4.1 Controlled termination

Causes: an unkeyed quit (dedicated-channel delivery), a keyed quit
(in-band delivery through its run's private channel), a render error.
The delivery semantics of the two quit forms are RFC 0003's and
RFC 0006's contract (INV-9, INV-L4, INV-L10/INV-L11) and are not
restated here; what this RFC pins is what happens next.

Contract (INV-LC5): every controlled cause exits the loop with its
reason; the immediate postcondition (§4.4) holds when `run()` returns,
and the return value classifies the reason — `Ok(())` for either quit
form, `Err` carrying the render error. `run` consumes the runtime, so
the terminating operation — the loop exit, any explicit shutdown, and
the value's teardown — completes inside the call; whether a given cause
reaches the postcondition through the explicit shutdown routine or
through the consumed value's drop is mechanism (§4.2).

### 4.2 Render error: no divergence on contract surface

Today the quit exits run the explicit shutdown while the render-error
exit returns early without it (`src/runtime.rs`). The contract question
is whether that difference is observable; it is not, on any pinned
surface: `run` takes the runtime by value, so the early return
drops the runtime inside the call, and the value's drop requests
cancellation of every runtime-owned task — the unkeyed and keyed task
sets abort on drop, and the subscription manager aborts its forwarders
on drop — before the caller can observe the `Err`. Both §4.4 stages are
therefore reached at the same boundaries as on the quit exits; the
producer gauges fire only on change (RFC 0006 §4.4), so deduplicated
teardown emissions do not distinguish the paths either, and what
remains distinct is unpinned diagnostics (§5.1). A caller cannot hold
the runtime after `run` returns, so the divergence a bypass could
otherwise expose — a kept-alive runtime whose subscription sources keep
being polled after an `Err` — is unconstructible. (Adversarial model
considered and excluded by the signature, `run(self, …)`.) Routing the render-error
exit through the same explicit shutdown routine is implementation
tidying, not a contract deliverable, and carries no CHANGELOG entry.

### 4.3 Abrupt termination

Causes: the `run` future is dropped (external cancellation — a caller's
`select!`/timeout); a panic unwinds through `run` from application code
invoked on the driving task — `update`, `view`, `subscriptions`
(called during bootstrap *and* on every dirty frame), or a declared
subscription's lazy source constructor, which runs inside the same
reconcile (all four sites are on the driving task, so all unwind
through `run`); the runtime value is dropped without ever being run —
once `run` is called the value is owned by the future, so a mid-run
drop *is* the run-future drop above.

Contract (INV-LC6): the terminating drop or unwind itself performs the
ownership teardown and the cancellation requests — the §4.4 immediate
postcondition holds when it completes, with no further call required
and no runtime component left waiting to notice termination later. The
synchronous half is exactly that: teardown of ownership and the abort
requests. The task futures themselves are dismantled by the executor
afterward, on the quiescent stage's schedule (§4.4) — synchrony is not
claimed for them. A panic in the application's transition
functions stays fail-fast: it propagates to `run()`'s caller (whether
the implementation lets it unwind directly or resumes it after cleanup —
open question 1) and is never converted into a continued run.

### 4.4 Postconditions: immediate and quiescent

Both routes reach the same two-stage postcondition; the stages are
distinct because the executor is a third party — aborting a task is a
cancellation *request*, and the task's future (with the RAII state it
holds, including gauge guards) is dropped on a later executor poll, not
inline. (The repository's own teardown tests already encode this: they
poll for gauges to settle after `run()` returns rather than asserting
zero immediately — `tests/observability.rs`.)

**Immediate postcondition** — holds at the terminating operation's
completion (`run()`'s return for controlled; completion of the drop or
unwind for abrupt):

1. No further transition: `update`, `view`, and `subscriptions` are
   never invoked again for this runtime, and no producer output —
   buffered or in flight — is ever delivered. Output undelivered at
   termination is discarded, never delivered late (the discard RFC 0006
   INV-L2 already carves out and RFC 0008 §5.3 mirrors).
2. Cancellation has been requested for every runtime-owned task. No
   further runtime action is needed to reach quiescence — only executor
   scheduling.

**Quiescent postcondition** — holds once the executor has processed the
requested cancellations: every runtime-owned task has terminated, and
every producer gauge (RFC 0006 §4.4) reads zero. How many scheduler
passes that takes is unpinned; a consumer awaits it with a bounded
settle loop (poll until the gauges read zero), never a fixed pass count
(INV-LC7).

Which channel endpoint closes at which of the two stages is mechanism,
as is whether the controlled causes share one shutdown routine and what
shutdown tracing is emitted (§5.1); the contract is the two stages
above.

Whether the *controlled* route additionally guarantees quiescence by
`run()`'s return — a join barrier before returning — is open question 2;
under either resolution the two postconditions above hold as stated.

### 4.5 Graceful drain: the zero-grace frame

Today's termination is immediate: cancellation is requested at the
terminating operation with no grace interval and no awaiting of
in-flight effects — the degenerate, zero-grace form of a drain model.
This RFC pins only that frame: a future graceful-drain feature is one
that inserts a bounded grace interval between the terminating operation
and the cancellation requests, and it must still reach both §4.4
postconditions; its design (what drains, what the bound is, how it is
configured) is future work, not settled here.

## 5. Panic containment

A panic inside a runtime-owned producer task — an unkeyed command task,
a keyed command task, or a subscription forwarder — does not terminate
the application (INV-LC8): the event loop keeps running, other
producers are not cancelled by it — their subsequent output remains
deliverable and is delivered under the ordinary delivery contracts
(RFC 0003/RFC 0006; no schedule or ordering claim is made) — and the
terminated producer's resources are released like any other task exit
(its gauge contribution falls, subject to §4.4's quiescence timing).
The containment property is pinned here for all three kinds; for the
keyed kind, RFC 0003 §5.5/§7.3 already record the catch-and-log
behavior, and that diagnostic requirement stays RFC 0003's (§5.1).

The complement is deliberate: a panic in the application's own code on
the driving task — `update`, `view`, `subscriptions`, a subscription's
source constructor — is the application's own bug and stays fail-fast
(§4.3); containment never extends to it.

### 5.1 Negative space: diagnostics and exit causes

This RFC pins no *new* common diagnostic schema for panics or
termination: no supervision-event surface, and no unified target,
wording, or payload across the three task kinds. Diagnostic
requirements other RFCs already state stand unchanged and are carved
out of this section, at the scope their owners actually state —
specifically, RFC 0003 §7.3 requires that keyed task panics are
logged, so the carve-out is that the keyed-panic log event fires, and
no more. The event's target, level, and message wording are not owner
contract: RFC 0003 §5.5 records the wording as mechanism, and the
existing test's target-and-level filter is a conformance check against
the current implementation, not a contract either RFC states. Any
preservation of those values is an implementation-side compatibility
baseline, not an RFC requirement — this RFC neither pins them nor
amends RFC 0003 to do so. RFC 0006 INV-L13's load-event schema is
likewise carved out, at its own stated scope. Beyond those owner-stated requirements, the tracing output
accompanying a contained panic or a termination is diagnostic, not
contract, and may not be matched on as a stable surface. A task's exit cause (stream end,
panic, closed channel, abort) is likewise not contract surface today:
this RFC neither exposes causes nor pins their absence, leaving a
future supervision surface free to expose them additively.

## 6. Driver exclusivity

At most one owner drives a runtime instance at a time, and the state
transitions — `update`, `view`, `subscriptions` — execute serially and
non-reentrantly: no transition begins before the previous one returns,
and none is invoked from inside another (INV-LC9).

This is pinned as a *property*, not as the absence of an API. Today it
is delivered by the single consuming `run(self)` entry point on one
driving task; a future `step`-style or handle-based driving surface is additive
exactly as long as the property is preserved, and a change that breaks
it — concurrent or reentrant transitions — is an amendment to this RFC
regardless of what API introduces it.

## 7. Premises and mechanism (informative)

Nothing in this section is contract. It records, first, the premises
that other RFCs' invariants rest on — so a lifecycle-motivated change to
any of them is recognized as touching those invariants, not as free
mechanism churn — and second, the mechanism inventory a reimplementation
is free to replace.

Premises:

- **Unbiased top-level `select!`.** The event loop's branch arbitration
  (§2.3) is an unbiased select. This is the premise of RFC 0006
  INV-L4's statistical formulation — F6's depth-independence was
  measured on it — so biasing the loop is INV-L4-amendment territory,
  not a free re-arbitration.
- **Frame-branch pacing and gating.** The frame branch is paced
  non-catch-up (missed frames are skipped, not replayed) and gated on
  pending work; RFC 0006 §1.1's scheduling facts and F5's frame-health
  finding assume both.
- **Always-armed quit branch.** The dedicated quit channel's branch is
  armed in every loop iteration — the premise of RFC 0006 R4/INV-L4.
- **Synchronous producer creation on the driving task.** Commands are
  dispatched before the next input is pulled and subscription
  reconciliation spawns forwarders inside the frame pass — the premise
  of RFC 0006 INV-L7/INV-L8's no-self-deadlock argument.
- **Single-task parking.** The frame scheduler parks by returning a
  never-ready future when no work is pending, which assumes the one
  driving task is woken through its channels and timers
  (`src/runtime/frame_scheduler.rs`). An external-driving design must
  revisit this assumption alongside INV-LC9.

Mechanism inventory (free to change while the contract holds): the
runtime owns three task kinds — unkeyed command tasks in a `JoinSet`
(`src/runtime/core.rs`), keyed command tasks with typed exit
bookkeeping (`src/runtime/keyed_commands.rs`), and subscription
forwarders (`src/subscription.rs`); pending work is two flags consumed
by the frame pass (`src/runtime/pending_work.rs`); gauge events flow
through the off-lock funnel RFC 0006 §4.4 specifies
(`src/runtime/load.rs`). None of these shapes is pinned here.

## 8. Invariants

Enforcement classes follow the pre-review checklist's definitions.

- **INV-LC1**: in steady state, rendering and subscription
  re-evaluation happen only in frame passes — never inside an input
  batch — and one frame pass performs at most one render and at most one
  subscription re-evaluation, consuming pending work recorded by
  preceding batches (§2.1). Behavioral, at the runtime layer (the
  layer's existing white-box pattern): tests drive the batch and
  frame-pass paths (`process_input_batch`, `process_frame_tick` in
  `src/runtime.rs`) with a recording application and assert that a batch
  processing several messages triggers no `view`/`subscriptions` call,
  and that the following frame pass performs each at most once.
- **INV-LC2**: within one frame pass the render step precedes
  subscription re-evaluation, a frame pass never begins subscription
  re-evaluation while a redraw is pending, and both steps observe the
  pass's current state — so whenever a redraw was pending at a pass,
  the subscriptions that pass starts are those of a state the same pass
  has just rendered (§2.2; the claim is per pass, not per individual
  state — no state is itself promised a render — and a pass with no
  redraw pending re-evaluates with no preceding render, by design).
  Behavioral, same seam as INV-LC1: a recording application whose batch
  marks both redraw and dirtiness asserts the
  `view`-before-`subscriptions` call order within the pass and that
  both calls observed the same state; the superseding sequence — a
  redraw-requesting batch, then a `without_redraw` batch, then the
  frame pass — asserts exactly one render, of the latest state, with
  the intermediate state never rendered; a `without_redraw`-only
  variant asserts re-evaluation still happens (RFC 0002 INV-4's
  separation, observed from the lifecycle side).
- **INV-LC3**: constructing a `Runtime` spawns no runtime-owned task,
  polls no command effect, and starts no subscription source; no claim
  is made about `Application::new`'s own side effects, and an
  `Application::new` panic is outside this contract (§3.1). Primary
  check structural — review of the construction path
  (`Runtime::new`/`with_config`, `RuntimeCore` construction in
  `src/runtime/core.rs`) for the absence of spawn and dispatch sites —
  because a behavioral test cannot prove the absence of a task that
  performs no observable work. Behavioral regression check: construct a
  runtime with an init effect and a subscription source that record
  execution, under a `tracing` recorder; drop it without running; assert
  neither ran and no producer-gauge event fired during construction.
- **INV-LC4**: inside `run()`, the init command is dispatched before the
  initial subscription reconcile starts any source, and the first render
  starts out pending — unconditionally, independent of the init
  command's redraw directive. Execution order beyond intake is not
  pinned: the init effect's first poll, initial subscription output, and
  the first render arbitrate freely, and the first render's execution is
  not promised (§3.2). Structural for the ordering half — review of
  `run()`'s bootstrap sequence — because production exposes no stable
  observable phase between dispatch and first poll for a behavioral test
  to anchor on (§3.3). Behavioral for the eligibility half, at the
  runtime layer: a freshly constructed runtime's first frame pass
  renders with no message processed.
- **INV-LC5**: each controlled cause — unkeyed quit, keyed quit, render
  error — exits the loop, and the §4.4 immediate postcondition holds
  when `run()` returns, with the return value classifying the reason
  (`Ok(())` for the quits, `Err` for the render error); whether a cause
  reaches the postcondition through the explicit shutdown routine or
  through the consumed runtime value's drop is mechanism (§4.1, §4.2).
  Behavioral, one row per cause at the integration layer, each row
  asserting the return classification, that no further
  `update`/`view`/`subscriptions` call is observed afterward, and —
  through the INV-LC7 settle loop — that producers wind down:
  - an unkeyed quit under running producers;
  - a keyed quit under running producers;
  - a render error (injected failing render), asserting the `Err`
    return plus the same settle-loop quiescence as the quit rows.
- **INV-LC6**: each abrupt cause — drop of the `run` future, a panic
  unwinding through `run` from application code on the driving task
  (`update`, `view`, `subscriptions` at either call site, or a
  subscription's lazy source constructor), drop of a never-run runtime
  value — performs the ownership teardown and cancellation requests
  synchronously during the drop or unwind, reaching the §4.4 immediate
  postcondition with no further call (task futures are dismantled
  afterward by the executor — the quiescent stage); a panic propagates
  to the caller (§4.3). Structural for the synchrony half: review of
  the `Drop` owners that carry the teardown — the task-set and manager
  structures whose drops issue the abort requests
  (`src/runtime/core.rs`, `src/runtime/keyed_commands.rs`,
  `src/subscription.rs`) — confirming every runtime-owned task is
  reachable from a structure the runtime value's drop or the unwind
  reaches, with no teardown step deferred to a later call or task.
  Behavioral at the integration layer, one row per quantified cause and
  call site; each row asserts that from the moment the drop or unwind
  completes — checked immediately, and re-checked across the INV-LC7
  settle loop's yields, not only after settling — no further
  transition, delivery, or source poll is observed (a settle-only check
  would pass an implementation that defers its cancellation requests by
  a scheduler pass, whose still-live producers keep polling sources
  during that window), plus the propagation assertion for the panic
  rows (the test harness catches the unwind). The rows run on a
  single-threaded test executor, so no producer poll is in flight
  across the drop itself and the no-further-poll assertion is
  deterministic:
  - the `run` future dropped mid-run (a caller `select!`/timeout);
  - a panic in `update`;
  - a panic in `view`;
  - a panic in `subscriptions` at the bootstrap call site (raised on
    its first call, before the loop);
  - a panic in `subscriptions` at the steady call site (raised only on
    a re-evaluation after a processed message);
  - a panic in a subscription's lazy source constructor (raised at the
    reconcile that starts it);
  - a never-run runtime value dropped — with §3.4 landed there is
    nothing to wind down, and the row asserts exactly that (no effect
    executed, no source started, no producer-gauge event), reusing
    INV-LC3's recorder setup.
- **INV-LC7**: after either route's terminating operation, once the
  executor has processed the requested cancellations, every
  runtime-owned task has terminated and every producer gauge reads
  zero; the number of scheduler passes to get there is unpinned, so the
  check — and any consumer — awaits quiescence with a bounded settle
  loop, never a fixed pass count (§4.4). Behavioral: the settle-loop
  assertion (the `tests/observability.rs` pattern) applied across the
  INV-LC5/INV-LC6 cause tests; an implementation that leaves a producer
  running or a gauge non-zero fails the loop's bound. (Adversarial
  model: asserting gauge zero immediately after `run()` returns —
  excluded by the two-stage split, since abort is a request and guard
  drops ride later polls.)
- **INV-LC8**: a panic inside a runtime-owned producer task (unkeyed
  command task, keyed command task, subscription forwarder) does not
  terminate the application: the loop keeps running, and other
  producers are not cancelled by it — their subsequent output remains
  deliverable and is delivered under the ordinary delivery contracts,
  with no schedule or ordering claim (§5). The containment property is
  pinned here for all three kinds; RFC 0003's keyed-panic requirement
  is that the log event fires (§7.3), not that the application
  continues, so the keyed row below is not redundant with its test, and
  no new diagnostic schema is pinned (§5.1).
  Behavioral at the integration layer, one row per task kind — a
  panicking unkeyed effect, a panicking keyed effect, and a
  subscription whose source constructor succeeds and whose stream
  panics while the forwarder task polls it (distinct from INV-LC6's
  constructor-panic row, which unwinds on the driving task) — each
  running alongside a surviving producer whose later messages must
  still arrive at `update`, with the run then quitting normally.
- **INV-LC9**: at most one owner drives a runtime instance at a time,
  and `update`/`view`/`subscriptions` execute serially and
  non-reentrantly; a future driving surface is additive iff it preserves
  this (§6). Structural: the property is delivered by construction
  (the consuming `run(self)` as the sole driving entry point,
  transitions invoked only from the driving task) and reviewed at those
  invocation sites — a behavioral test cannot prove the absence of a
  reentrant path.

Surface–invariant coverage: this RFC adds no public API. Its contract
surface is the phase order (INV-LC1/INV-LC2 and §2.3's negative space),
construction inertness and the §3.4 change (INV-LC3), the bootstrap
order and first-render eligibility (INV-LC4), the termination routes
with §4.2's no-divergence analysis (INV-LC5/INV-LC6), the two-stage
postconditions
(INV-LC7), panic containment with §5.1's negative space (INV-LC8), and
driver exclusivity (INV-LC9). The §3.3 TestStore-mapping paragraph is
scoping for RFC 0008's existing surface and carries no invariant here.

Excluded claims (minimal-contract pass): an update-before-dispatch
ordering invariant was dropped — RFC 0003 INV-10 already owns it, and
INV-LC1 cites rather than restates it; a batch-end flag-recording
invariant was dropped — RFC 0002 (redraw OR-fold) and RFC 0003 §4.4
(dirtiness rule) own the recording, and INV-LC1 pins only the
frame-granularity consumption they do not; a shutdown step list
(abort order, entry clearing) was dropped as mechanism — RFC 0003 §5.6
records it, and §4.4's postconditions pin everything a dependent may
rely on. INV-LC2 was kept despite overlapping INV-LC1 (both constrain
the frame pass) because INV-LC1 alone does not order the two activities
within the pass, and the never-while-pending clause is what excludes the
split-pass adversary.

## 9. Open questions

1. **Panic-reason preservation.** For the transition-panic route
   (§4.3), does the implementation catch at the `run` boundary, perform
   cleanup, and `resume_unwind` — keeping the panic payload while
   pulling cleanup onto the controlled path's code — or rely on
   `Drop`-based cleanup with the panic unwinding through untouched?
   Both satisfy INV-LC6 (ownership teardown and cancellation requests
   synchronous within the unwind, panic propagates); the
   catch-and-resume form is acceptable only as exactly that — fail-fast
   is not up for revision. Resolves at implementation
   design, in this RFC's body.
2. **Controlled-route quiescence.** Should the controlled route
   guarantee the quiescent postcondition by `run()`'s return — a join
   barrier on runtime-owned tasks before returning? If adopted, the
   controlled/abrupt asymmetry (controlled: quiescent at return;
   abrupt: cancellation requested, quiescence on executor progress)
   becomes contract and is stated here explicitly; if declined, both
   routes keep the uniform two-stage form of §4.4. INV-LC5/INV-LC7 hold
   under either resolution. Resolves at implementation design, in this
   RFC's body.

## 10. References

- RFC 0002 — redraw suppression: the redraw OR-fold INV-LC1 consumes;
  non-negotiable B and INV-4 (suppression never suppresses
  re-evaluation), the scoping fact behind INV-LC2; §9's quit-vocabulary
  track (out of scope here).
- RFC 0003 — command cancellation: INV-10 (one-item drain), §4.3
  (dispatch), §4.4 (batch window, dirtiness rule, keyed-quit exit),
  §5.5/§7.3 (keyed panic containment INV-LC8 restates), §5.6 (shutdown
  mechanism), INV-9/INV-14.
- RFC 0005 — structural lifecycle identity: the reconciliation contract
  whose *timing* §2 pins.
- RFC 0006 — runtime load control: INV-L4/R4/F5/F6 (the contracts §2.3's
  negative space defers to and §7's premises serve), INV-L7/INV-L8 (the
  no-self-deadlock argument premised on synchronous producer creation),
  INV-L2 (the shutdown discard §4.4 cites), §4.4 (the producer gauges
  INV-LC7 reads), INV-L10/INV-L11 (keyed-quit delivery).
- RFC 0008 — TestStore: §3.2 (construction semantics §3.3 scopes), §4.2
  (the citation rule §2.3 mirrors), §5.2 (the init-directive
  independence §3.2 cites), §5.3 (the shutdown-discard mirror).
- `src/runtime.rs` (`run`, `process_input_batch`, `process_frame_tick`),
  `src/runtime/core.rs` (construction, dispatch, shutdown),
  `src/runtime/keyed_commands.rs`, `src/subscription.rs` (the three
  task kinds and their panic capture), `src/runtime/pending_work.rs`,
  `src/runtime/frame_scheduler.rs` (pending-work flags and parking),
  `src/runtime/load.rs` (gauges).
- `tests/observability.rs` — the settle-loop pattern INV-LC7 adopts.
- `docs/rfcs/pre-review-checklist.md` — enforcement-class definitions.
