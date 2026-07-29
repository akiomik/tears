# RFC 0012: Subscription Execution

- Status: Draft
- Target: 0.11.0 — one behavior change with two faces (restart and
  replacement admission waits for the stopped task's quiescence, and
  subscription re-evaluation gains a message-independent trigger, §4);
  public signatures unchanged
- Scope: the subscription source execution contract (start / poll /
  stop-and-restart), the three-boundary stop vocabulary with the
  uniform quiescence barrier, its admission rules, and the
  message-independent re-evaluation trigger they introduce, the
  `subscriptions()` purity contract, the source-side injection
  contract, source-internal state management, the restart-rate-control
  delegation frame, and the effect-DI negative space
- Feature flag: none
- CHANGELOG: `Changed` — a new or restarted subscription is admitted
  only after every previously stopped subscription task has quiesced
  (§4). Re-evaluations with no outstanding stopped task — pure
  additions, and restarts of already-finished subscriptions — admit
  immediately as today, and continuing subscriptions are unaffected.
  Subscription re-evaluation also gains a message-independent trigger:
  the quiescence of a task stopped by re-evaluation marks
  subscriptions dirty, so `subscriptions()` can run — and a finished,
  still-declared subscription restart — on a frame pass with no new
  message (§4.3); the `Application::subscriptions` rustdoc is updated
  to match. Lands with the implementation.

## Summary

RFC 0005 owns a subscription's *identity* and RFC 0009 owns its
*timing*; nothing owns its *execution* — what starting, polling,
stopping, and restarting a source actually promise. This RFC is the
third piece of that split. Six decisions:

1. **Source execution template** (§2). A source starts by lazy spawner
   (side effects at start, never at declaration), is polled by its
   runtime-owned forwarder at forwarder pace (delivery and
   backpressure stay RFC 0006's), and stops through the §3
   boundaries.
2. **Three boundaries and a uniform quiescence barrier** (§3, §4).
   *Stop requested*, *quiesced*, and *restart admitted* are distinct
   contract moments, and admission is uniformly after quiescence: no
   new or restarted subscription starts while any stopped task has not
   yet quiesced, for every subscription alike, with four admission
   rules (INV-SE2–INV-SE5) closing the stale-generation races. The
   current manager does not conform — the conformance change is this
   RFC's `Changed` entry.
3. **`subscriptions()` purity** (§5, INV-SE6). The purity obligation,
   today stated only in `Application` rustdoc, gets an owner: the
   declared set is a pure function of application state. The runtime's
   dirty-frame re-evaluation gating (RFC 0011 §2) and TestStore's
   determinism (RFC 0008 INV-T11) already rest on it.
4. **Source-side injection only** (§6). The template is the injection
   surface: a mock source conforms by satisfying it. TestStore's public
   API is not touched — RFC 0008's non-execution contract (§1.2,
   INV-T11) is preserved as stated, and a subscription-driving store is
   a future RFC 0008 amendment layered on this contract.
5. **Source-internal state, and no more** (§7). Owning and managing
   mutable state *inside* a source's boundary is a legal template
   clause. Mutating external state from `update` through a public
   handle is not generalized: RFC 0001 §5.5's `invalidate()` stays that
   RFC's deliberate, limited deviation.
6. **Effect DI stays out of core** (§9). No effect-executor
   abstraction, DI registry, or injection surface enters the crate —
   a new negative-space decision owned here for the non-time axes;
   the time axis is RFC 0009's, unchanged.

Restart *rate* policy (backoff, minimum intervals) is deliberately not
designed here: this RFC owns the schedule discipline, a future opt-in
policy RFC owns rates (§8).

## 1. Scope

### 1.1 In scope

- The execution template — start, poll, stop/restart — and the
  boundary vocabulary (§2, §3).
- The uniform quiescence barrier and its four admission rules, with the
  conformance change they require (§4).
- The `subscriptions()` purity contract (§5).
- The source-side injection contract and the RFC 0008 boundary (§6).
- The source-internal state clause and its limit (§7).
- The restart-rate delegation frame (§8).
- The effect-DI negative space for non-time axes (§9).

### 1.2 Out of scope

- **Identity.** What makes two subscriptions the same — structural
  IDs, scopes, dedup — is RFC 0005; this RFC uses its vocabulary and
  changes none of it. INV-13's restart semantics are preserved (§4.3).
- **Timing.** Clock discipline and `Timer`'s tick semantics are
  RFC 0009.
- **Delivery.** Channel semantics, backpressure, losslessness, and
  ordering are RFC 0006; the template cites its §4.2 pacing clause and
  adds no delivery claim.
- **Lifecycle phases.** When re-evaluation runs is RFC 0011 (§2,
  INV-LC1/INV-LC2). This RFC contributes one thing to that contract —
  the subscription-lifecycle-completion dirty source RFC 0011 §2.1
  records — and its admission rules are otherwise constrained by it.
- **TestStore's public API.** Owned by RFC 0008; a stage-3 driving API
  is a future amendment there (§6.2).
- **Restart rate policy.** A future opt-in RFC (§8).
- **Composition.** An aggregating adapter's obligations are the
  composition RFC's; §4.4 states only that this contract is transparent
  to it.

## 2. Source execution template

A subscription source's execution life is three phases, each with its
own contract:

- **Start.** A subscription declares itself as an identity plus a lazy
  spawner; *start* is the boundary at which the manager invokes the
  spawner — exactly once per admitted run (INV-SE1) — the spawner
  builds the source's stream, and the forwarder is spawned. The
  source's execution therefore begins no earlier than start. Where a
  source acquires its resources is deliberately not pinned: at stream
  construction or in any later poll are both conforming — production
  sources do both (a WebSocket source connects inside its stream's
  polling, a signal source installs its handler at first poll;
  `src/subscription/websocket.rs`, `src/subscription/signal.rs`).
  Declaration is effect-free: returning a subscription from
  `subscriptions()` executes nothing, and identity comparison never
  invokes a spawner (RFC 0005 INV-12 owns the never-invoked cases —
  discarded duplicates and continuing IDs; this RFC owns the
  invoked-at-admission timing).
- **Poll.** From start until stop, the source's stream is polled by
  its runtime-owned forwarder task, which delivers each item into the
  shared channel. The pace is the forwarder's: under RFC 0006 §4.2's
  bounded mode a source is simply not polled while its forwarder
  awaits capacity, and a source must tolerate arbitrary poll pacing
  (already the `SubscriptionSource` norm RFC 0006 §3.2 relies on). No
  delivery guarantee is added or restated here.
- **Stop and restart.** Stopping and restarting traverse the §3
  boundaries under the §4 admission rules.

A conforming source yields to the executor: its stream does its work in
`poll` steps that return, rather than blocking its task indefinitely.
This is an application obligation of the same class as RFC 0006 §4.5's
producer-count premise — the runtime cannot cancel a task that never
reaches an await point, so a blocking source defers its own quiescence
and, through §4's barrier, subsequent admissions; the `subscriptions`
gauge (RFC 0006 §4.4) is what makes the stall observable.

## 3. Three boundaries

For a running subscription task, three moments are contractually
distinct, and naming them separately is what the rest of this RFC
builds on:

- **Stop requested** — the runtime has requested the task's
  cancellation: its ID left the desired set, the runtime is shutting
  down, or the run is being torn down abruptly. This is a request in
  exactly RFC 0011 §4.4's sense: cancellation is asked for, not yet
  observed complete, and a poll already in flight may still finish.
- **Quiesced** — the forwarder task has terminated, as a confirmed
  fact rather than a pending request. After quiescence the run-owned
  stream and execution state are no longer polled and have been
  dropped. The declaration-side `Source` value's own drop point is
  *not* pinned: today's spawner consumes it when the stream is built
  (`src/subscription/core.rs`), and a declaring layer must not rely on
  the value living until quiescence — or on any particular drop
  moment. (This is RFC 0011's request/quiescent two-stage model
  applied per subscription; RFC 0011 §4.4 states it for whole-runtime
  termination.)
- **Restart admitted** — a successor is started: the spawner of a
  newly desired or restarting subscription is invoked and its
  forwarder spawned.

The gap between the first two boundaries is real — an executor
processes cancellation on a later poll — and collapsing them is exactly
the defect §4 exists to prevent: a stop-requested task's one-beat-late
poll can still consume input from an external resource (a terminal, a
socket) while its successor is already reading the same resource.

## 4. The uniform quiescence barrier

### 4.1 The rule

Restart admission is after quiescence, uniformly: **no subscription is
admitted while any task whose stop has been requested has not yet
quiesced** — for every subscription, with no source-class distinctions.

Uniformity is load-bearing, not a simplification. A barrier scoped to
"handoff-prone" sources (terminals, sockets) would require the runtime
to classify sources — new contract surface this RFC declines to add —
and without classification the stolen-input hazard above cannot be
closed for the classes that need it. The uniform rule closes it as a
general property and, with it, removes the window in which an old task
and its replacement poll the same resource concurrently.

### 4.2 The four admission rules

- **INV-SE2 — continuing subscriptions are exempt.** An ID present in
  both the previous and the new desired set with a live task continues
  unchanged: no stop request, no quiescence wait, no readmission, and
  no spawner invocation (the never-recreated half is RFC 0005
  INV-12). The barrier never touches a subscription that is not being
  stopped or started.
- **INV-SE3 — no admission before outstanding stops quiesce.** A
  re-evaluation issues its stop requests first; no admission from its
  desired set executes until every stop-requested task — its own
  removals and any still-unquiesced stops from earlier
  re-evaluations — has quiesced. A re-evaluation with no outstanding
  stopped task (pure additions; restarts of already-finished tasks,
  which are quiesced by definition) admits immediately — the current
  synchronous behavior remains conforming there.
- **INV-SE4 — a newer re-evaluation supersedes pending admissions.**
  When a new re-evaluation arrives while admissions are pending on the
  barrier, the older generation's pending desired set and its
  un-invoked spawners are discarded; only the newest desired set is
  ever admitted. The mandated check (part of this invariant's
  enforcement): declare `{A}`, re-evaluate to `{B}` (stop requested
  for A), then — before A quiesces — re-evaluate to `{C}`; then A
  quiesces, marking subscriptions dirty (INV-SE5), and the next frame
  pass re-evaluates against the then-current state — `{C}` — and
  admits C. B's spawner is never invoked at any point in the
  sequence.
- **INV-SE5 — admission executes only at a subscription
  re-evaluation** — the bootstrap reconcile (RFC 0011 §3.2) or a
  frame-pass re-evaluation; deferred admissions are always the
  latter. The quiescence of a task stopped by a steady-state
  re-evaluation — removed or replaced out of the desired set — marks
  subscriptions dirty (the second dirty source RFC 0011 §2.1 records
  for this RFC), and marking that dirt wakes an idle runtime: a
  parked loop with no pending input is woken by the quiescence so the
  next frame pass can run — the wake is contract, the notification
  mechanism is unpinned. The next frame pass's re-evaluation, reading
  the then-current state, admits whatever that state declares, under
  INV-SE3's barrier. Termination-driven stops are outside this rule:
  quiescence during shutdown or abrupt teardown marks no dirt and
  triggers no re-evaluation — RFC 0011 §4.4's postconditions stand,
  and `subscriptions` is never invoked after termination. Two
  shapes are non-conforming: starting a source directly from a
  task-exit event (it would act on a desired set no frame-pass
  re-evaluation has refreshed, outside RFC 0011's phase contract —
  re-evaluation is a frame-pass activity, INV-LC1), and blocking the
  stopping re-evaluation to await quiescence inline before admitting
  (it suspends the loop across the quiescence gap, so no newer
  re-evaluation can arrive to supersede the pending set — INV-SE4's
  mandated sequence becomes unsatisfiable).

Joint satisfiability of INV-SE4, INV-SE5, and RFC 0011 INV-LC1, walked
against the amended contracts: a conforming implementation stops A,
admits nothing, and returns from the pass; A's quiescence only marks
dirt; the next frame pass runs the single re-evaluation INV-LC1
permits, reads the newest state, and admits from it — superseding is
automatic because no desired set is carried across passes, and every
admission is inside a frame-pass re-evaluation. All three hold on one
execution, including the INV-SE4 sequence.

### 4.3 Conformance and INV-13

The current manager does not conform: `SubscriptionManager::update`
(`src/subscription.rs`) aborts removed tasks and invokes new spawners
in the same synchronous pass, without awaiting the aborted tasks'
termination — stop requested and restart admitted are collapsed. The
conformance change is this RFC's `Changed` entry, and its scope is
two-fold, stated honestly: (1) *admission timing* — new and restarted
subscriptions wait for outstanding stopped tasks' quiescence; and
(2) *a new re-evaluation trigger* — that quiescence marks
subscriptions dirty, so `subscriptions()` can be re-evaluated on a
frame pass that no message preceded. An observable consequence of
(2): if subscription A's stream finishes naturally while stopped B is
still quiescing, B's quiescence dirties the next frame pass, whose
re-evaluation restarts the still-declared A with no new message having
been processed — where today A would wait for the next message. The
`Application::subscriptions` rustdoc currently pins the
single-trigger world ("re-evaluates subscriptions only after a
message is processed"; a finished source restarts "after the next
message" — `src/application.rs`); updating that rustdoc to the
two-trigger contract is an implementation deliverable of this RFC,
alongside the conformance change itself.

RFC 0005 INV-13 — a finished subscription that remains desired under
the same full ID restarts on the next re-evaluation — is preserved in
meaning: the restart still happens as a consequence of re-evaluation,
and a finished task is already quiesced, so a pure restart is not even
delayed. Only when the same re-evaluation also stops still-running
tasks does the restart's admission instant move to after their
quiescence.

### 4.4 Transparency to composition

The barrier is a runtime-side rule over the desired set, invisible to
whoever declares it. An aggregating adapter (a future composition
layer) merely merges child declarations into one desired set; nothing
in this contract requires — or offers — a declaring layer any way to
observe or await quiescence. A composition design that turns out to
need quiescence observation is a change to this contract, not a use of
it.

## 5. `subscriptions()` purity

**INV-SE6**: `Application::subscriptions` is a pure function of
application state — for a given state it returns the same declared set
(the same identities, per RFC 0005), executes no side effects, and
reads no external mutable state. The runtime may invoke it at any
re-evaluation frequency; an application must not rely on call count or
call timing for correctness.

This obligation exists today only as `Application` rustdoc
(`src/application.rs`); this RFC is its owner of record, and the
rustdoc carries the same contract, citing this RFC (a documentation
deliverable landing with the implementation). Two consumers already
lean on it: the runtime re-evaluates only on dirty frame passes
(RFC 0011 §2) — sound only if declarations depend on state alone — and
RFC 0008 INV-T11's `subscription_ids` determinism is the same
assumption observed from the store side.

## 6. Injection: the source side only

### 6.1 The template is the injection surface

Any source that satisfies §2 and §3 — effect-free declaration, side
effects at start via the lazy spawner, forwarder-paced polling,
quiescence on stop — can stand in for a production source under the
same identity rules (RFC 0005), with no runtime changes. That is the
whole injection contract: a test double is a conforming source, not a
special mode — and one exists: the public `MockSource`
(`src/subscription/mock.rs`, a cloneable broadcast-backed source with
`emit` and `receiver_count`) is the reference conforming seam. What
remains open is only its integration shape with a future RFC 0008
stage-3 driver (§12).

### 6.2 The RFC 0008 boundary

TestStore's contract is unchanged and deliberately preserved:
TestStore never starts, polls, or restarts a subscription source
(RFC 0008 §1.2), and `subscription_ids` observes the declared set only
(RFC 0008 INV-T11). Nothing in this RFC amends either statement. A
subscription-driving store — an opt-in stage-3 driver API — is a
future RFC 0008 amendment that would *consume* this RFC's execution
and injection contract; it is layered work, owned there, gated on this
RFC's acceptance.

## 7. Source-internal state

**INV-SE7**: a source may own and manage mutable state within its own
boundary — cells, caches, generation counters — as an ordinary part of
the template; RFC 0001's HTTP cell is the precedent, now legal by
clause rather than by exception. The limit is the boundary: this
clause does not generalize mutating external state from `update`
through a public handle. RFC 0001 §5.5's synchronous `invalidate()`
remains that RFC's deliberate, limited TEA deviation, adopted there
for correctness — it is reaffirmed as scoped to RFC 0001, and no
general license for update-side control-plane side effects is created
here. A future source that wants an `invalidate()`-shaped surface
makes its own case in its own RFC.

## 8. Restart rate control: delegated

This RFC owns the *schedule discipline* — the boundaries and admission
rules of §3/§4. Restart *rate* policy — backoff after failures,
minimum restart intervals, safety fuses — is an opt-in policy a future
RFC owns, consistent with the standing position that `RuntimeConfig`
carries no restart-rate field (RFC 0007 §4, RFC 0006 open question 5).
The delegation frame is fixed here: a rate policy may delay an
admission beyond quiescence; it may never admit before quiescence, and
it changes none of §4's rules.

## 9. Negative space: no effect DI in core

The crate introduces no effect-executor abstraction, no dependency
registry, and no injection surface in core for effect execution
(**INV-SE8**). This is a new decision of this RFC, covering the
non-time axes: RFC 0009 rejected a clock abstraction for the time axis
and owns that axis still — this section neither restates nor extends
that rejection, and cites it only to mark the boundary. Substituting
non-time I/O in tests and applications is served by two existing
instruments: the `Flags`/environment convention (application-level
dependency injection — a docs/examples concern, not API surface) and
this RFC's source-side injection (§6). An effect-DI proposal is a
change to this contract, measured against this section rather than
against a silent absence.

## 10. Premises and mechanism (informative)

Nothing here is contract. Mechanism: the manager tracks running
subscriptions by join handle and detects finished tasks via
`is_finished` (`src/subscription.rs`); how quiescence is observed —
joining handles, a reap pass, or a notification — is an implementation
choice (open question 1). The forwarder task's shape, its gauge guard,
and its panic capture are RFC 0011 §7's mechanism inventory. The
admission rules assume RFC 0011's phase contract (re-evaluation as a
frame-pass activity) and its request/quiescent two-stage model; they
are consumers of those contracts, not restatements.

## 11. Invariants

Enforcement classes follow the pre-review checklist's definitions.

- **INV-SE1**: a subscription's spawner is invoked exactly once per
  admitted run, at admission — never at declaration, never at identity
  comparison (the never-invoked cases are RFC 0005 INV-12's), and
  never before the §4 barrier admits the run. Behavioral at the
  manager layer (`SubscriptionManager::update`,
  `src/subscription.rs`): recording spawners assert one invocation per
  admitted run, zero before admission (a spawner pending on the
  barrier has not run), alongside RFC 0005's existing lazy-spawn
  suite.
- **INV-SE2**: a continuing ID — present in consecutive desired sets
  with a live task — is never stopped, never awaited, and never
  respawned by a re-evaluation, including one whose removed set is
  still quiescing. Behavioral at the manager layer: a re-evaluation
  that removes one subscription and keeps another asserts the kept
  task's handle is untouched and its source uninterrupted while the
  removed task quiesces.
- **INV-SE3**: no admission executes while any stop-requested task has
  not quiesced; a re-evaluation with no outstanding stopped task
  admits immediately. Behavioral at the manager layer on a
  single-threaded test executor, where the quiescence gap is
  deterministic: after a re-evaluation that stops A and adds B, assert
  B's spawner has not run before the executor processes A's
  cancellation, then drive the executor through A's quiescence and the
  next frame-pass re-evaluation and assert B is admitted there; the
  pure-addition and finished-restart cases assert immediate admission.
- **INV-SE4**: only the newest desired set is admitted; a superseded
  generation's pending spawners are discarded un-invoked. Behavioral
  at the manager layer — the mandated sequence: `{A}` → `{B}` (stop
  requested for A) → before A quiesces, `{C}` → A quiesces, marking
  subscriptions dirty → the next frame pass re-evaluates against the
  then-current state (`{C}`) and admits C; B's spawner is never
  invoked at any point. This sequence, including its post-quiescence
  frame-pass stage, is a required test, not an example.
- **INV-SE5**: admissions execute only at a subscription
  re-evaluation — the bootstrap reconcile (RFC 0011 §3.2) or a
  frame-pass re-evaluation — against the then-current state, and
  deferred admissions only at the latter; the quiescence of a task
  stopped by a steady-state re-evaluation marks subscriptions dirty,
  waking an idle runtime, and nothing more — termination-driven
  quiescence marks nothing (§4.2). No admission is
  triggered directly from a task-exit event, and the stopping
  re-evaluation does not block awaiting quiescence to admit inline
  (§4.2 — that shape makes INV-SE4's sequence unsatisfiable).
  Structural: review of the admission call sites — the manager admits
  only from the reconcile path (`update_subscriptions` / bootstrap,
  `src/runtime.rs`), the quiescence handler only marks dirt, and no
  await sits between a reconcile's stop requests and its return; a
  behavioral test cannot prove the absence of a bypass site, and the
  INV-SE4 sequence is the behavioral neighbor exercising the deferred
  flow end to end.

A runtime-level end-to-end gate accompanies INV-SE3's and INV-SE4's
manager-layer checks, because the manager layer alone cannot catch a
parked production loop: with the runtime idle — no pending input, the
frame branch parked — a stopped task's quiescence must wake the
runtime, whose next frame pass re-evaluates against the then-current
state and admits exactly what it declares (in the INV-SE4 sequence: C
alone, never B). The wake is INV-SE5's observable requirement; the
notification mechanism is free. This gate exists because an
implementation whose pending-work flag is set from another task
without a wake passes every manager-layer check while the production
loop parks forever — the current scheduler's parking comment records
exactly that hazard and demands an explicitly notified design when
pending work is marked off the driving task
(`src/runtime/frame_scheduler.rs`; RFC 0011 §7's parking premise).

- **INV-SE6**: `subscriptions()` purity (§5) — same state, same
  declared set; no side effects; no reads of external mutable state;
  no reliance on call count or timing. Structural: this is an
  obligation on application code, carried by the `Application`
  rustdoc citing this RFC; the crate-side check is review that runtime
  and store code depend only on what purity licenses (dirty-frame
  gating, RFC 0011 §2; declaration observation, RFC 0008 INV-T11) —
  a behavioral test cannot prove purity of arbitrary user code.
- **INV-SE7**: source-internal state is template-legal; update-side
  mutation of external state through a public handle is not
  generalized beyond RFC 0001 §5.5's scoped adoption (§7).
  Structural: a design-review rule for new sources and RFCs — a
  proposal relying on update-side external mutation cites and extends
  this clause explicitly rather than assuming a license.
- **INV-SE8**: no effect-executor abstraction, DI registry, or
  effect-injection surface exists in the crate's public API (§9).
  Structural: public-surface review, with the existing public-API
  check (`tests/api_surface.rs`, RFC 0007's instrument) as the
  regression neighbor showing no such item; negative space, so no
  behavioral scenario can prove the absence.

Surface–invariant coverage: this RFC adds no public API. Its contract
surface is the template's start discipline (INV-SE1), the barrier and
its admission rules (§4, INV-SE2–INV-SE5), purity (INV-SE6), the
internal-state clause and its limit (INV-SE7), and the effect-DI
negative space (INV-SE8). The three-boundary vocabulary (§3) is
definitional and carries no separate invariant: "a quiesced task is
never polled again" is the definition of quiesced, and the claims built
on it are INV-SE2–INV-SE5. The injection contract (§6.1) is the
template itself — a mock source is checked by the same invariants, not
by new ones — and §6.2 changes nothing in RFC 0008, so there is
nothing here to check.

Excluded claims (minimal-contract pass): a delivery invariant for
forwarded items was dropped — RFC 0006 owns delivery, and §2 only
cites its pacing clause; a "spawner not invoked for duplicates or
continuing IDs" invariant was dropped — RFC 0005 INV-12 owns it, and
INV-SE1 cites it; a per-subscription quiescence-follows-request
invariant was dropped — it is RFC 0011 §4.4's two-stage model, which
§3 applies rather than re-pins. INV-SE2 was kept despite following
from INV-SE3's "stop-requested" scoping because it is the clause that
makes the barrier's non-interference with healthy subscriptions
checkable on its own, and the A-removed-B-kept test is not implied by
INV-SE3's checks.

## 12. Open questions

1. **Quiescence observation.** How the manager observes that a stopped
   task has quiesced — awaiting join handles from a watcher, a
   completion notification from the forwarder, or polling handles — is
   an implementation choice with admission-latency implications; the
   contract constrains only its effect: quiescence marks subscriptions
   dirty (INV-SE5), so a shape that can observe quiescence only inside
   an already-triggered reconcile — never marking the dirt that would
   trigger one — does not conform. Resolves at implementation design,
   in this RFC's body.
2. **Mock-source integration.** A public `MockSource` already exists
   (`src/subscription/mock.rs`: construction, `emit`,
   `receiver_count`) and serves as §6.1's reference conforming seam.
   What remains open is only the stage-3 integration shape — how a
   driving TestStore consumes such a source — which RFC 0008's future
   amendment designs against this contract. Resolves there.

## 13. References

- RFC 0001 — HTTP module redesign: §5.5 (the scoped `invalidate()`
  deviation §7 reaffirms without generalizing); the cell as the
  source-internal-state precedent.
- RFC 0005 — structural lifecycle identity: INV-12 (lazy spawn — the
  never-invoked cases INV-SE1 cites), INV-13 (restart semantics §4.3
  preserves), §3.5 (identity and dedup).
- RFC 0006 — runtime load control: §4.2 (forwarder pacing and
  backpressure), §4.4 (the `subscriptions` gauge), §4.5 (the
  application-obligation class §2's yielding premise joins), open
  question 5 (restart rate stays subscription-level).
- RFC 0007 — RuntimeConfig: §4 (no restart-rate field — the standing
  position §8 keeps).
- RFC 0008 — TestStore: §1.2 (non-execution), INV-T11 (declaration
  observation) — both preserved unchanged; the future stage-3
  amendment §6.2 delegates.
- RFC 0009 — Clock DI: the time-axis rejection §9 distinguishes
  itself from.
- RFC 0011 — runtime lifecycle: §2/INV-LC1 (re-evaluation as a
  frame-pass activity — the constraint behind INV-SE5), §4.4 (the
  request/quiescent two-stage model §3 applies per subscription).
- `src/subscription.rs` (`SubscriptionManager::update`,
  `spawn_subscription` — the admission seam and the current
  nonconformance), `src/subscription/core.rs` (the spawner that
  consumes the `Source` at stream construction, §3),
  `src/subscription/websocket.rs` / `src/subscription/signal.rs`
  (poll-time resource acquisition, §2), `src/subscription/mock.rs`
  (the reference conforming seam, §6.1), `src/application.rs` (the
  purity rustdoc INV-SE6 canonicalizes), `src/runtime.rs`
  (`update_subscriptions`, the reconcile path INV-SE5 names),
  `tests/api_surface.rs` (INV-SE8's regression neighbor).
- `docs/rfcs/pre-review-checklist.md` — enforcement-class definitions.
