# RFC 0012: Subscription Execution

- Status: Draft
- Target: 0.11.0 — one behavior change (restart and replacement
  admission waits for the stopped task's quiescence, §4); public
  signatures unchanged
- Scope: the subscription source execution contract (start / poll /
  stop-and-restart), the three-boundary stop vocabulary with the
  uniform quiescence barrier and its admission rules, the
  `subscriptions()` purity contract, the source-side injection
  contract, source-internal state management, the restart-rate-control
  delegation frame, and the effect-DI negative space
- Feature flag: none
- CHANGELOG: `Changed` — a new or restarted subscription is admitted
  only after every previously stopped subscription task has quiesced
  (§4). Re-evaluations with no outstanding stopped task — pure
  additions, and restarts of already-finished subscriptions — admit
  immediately as today, and continuing subscriptions are unaffected.
  Lands with the implementation.

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
  INV-LC1/INV-LC2); this RFC's admission rules are constrained by that
  contract, not amendments to it.
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
  spawner; the spawner is what builds the source's stream. Source-side
  effects — opening a device, connecting, spawning I/O — belong to the
  spawner and therefore happen at *start*, when the manager admits the
  subscription and invokes the spawner: exactly once per admitted run
  (INV-SE1). Declaration is effect-free: returning a subscription from
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
- **Quiesced** — the task has terminated, as a confirmed fact rather
  than a pending request. After quiescence the source's stream is
  never polled again and the source value is dropped. (This is
  RFC 0011's request/quiescent two-stage model applied per
  subscription; RFC 0011 §4.4 states it for whole-runtime
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
  for A), then — before A quiesces — re-evaluate to `{C}`; when A
  quiesces, B's spawner must never be invoked, and C's subscription is
  what starts.
- **INV-SE5 — admission executes at a conforming point only.** An
  admission executes either (a) within the re-evaluation that issued
  the stops, by awaiting quiescence before admitting, or (b) at a
  subsequent frame-pass re-evaluation that reads the then-current
  state. Starting a source directly from a task-exit event is not a
  conforming shape: it would act on a desired set that no frame-pass
  re-evaluation has refreshed, re-opening the phase contract RFC 0011
  §2 pins (subscription re-evaluation is a frame-pass activity,
  INV-LC1). Both conforming points are compatible with RFC 0011: shape
  (a) suspends the frame pass as a whole (no batch interleaves within
  a pass), shape (b) is an ordinary re-evaluation.

### 4.3 Conformance and INV-13

The current manager does not conform: `SubscriptionManager::update`
(`src/subscription.rs`) aborts removed tasks and invokes new spawners
in the same synchronous pass, without awaiting the aborted tasks'
termination — stop requested and restart admitted are collapsed. The
conformance change is this RFC's `Changed` entry: what changes is the
*admission timing* of new and restarted subscriptions when stopped
tasks are still quiescing, nothing else.

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
whole injection contract: a test double (a mock source driven by the
test) is a conforming source, not a special mode. The concrete
`MockSource` API shape is an open question (§10).

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
  cancellation, then drive the executor and assert B starts; the
  pure-addition and finished-restart cases assert immediate admission.
- **INV-SE4**: only the newest desired set is admitted; a superseded
  generation's pending spawners are discarded un-invoked. Behavioral
  at the manager layer — the mandated sequence: `{A}` → `{B}` (stop
  requested for A) → before A quiesces, `{C}`; when A quiesces, B's
  spawner is never invoked and C starts. This sequence is a required
  test, not an example.
- **INV-SE5**: admissions execute only within the stopping
  re-evaluation (awaiting quiescence) or at a later frame-pass
  re-evaluation; no admission is triggered directly from a task-exit
  event. Structural: review of the admission call sites — the manager
  API's callers must be the reconcile path (`update_subscriptions` /
  bootstrap, `src/runtime.rs`) and no task-exit handler; a behavioral
  test cannot prove the absence of a bypass site.
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
   task has quiesced — awaiting join handles, a reap pass driven by
   the reconcile, or a completion notification — is an implementation
   choice with observable admission-latency implications but no
   contract difference under §4's rules. Resolves at implementation
   design, in this RFC's body.
2. **Mock source API.** The concrete shape of a test-driven conforming
   source (construction, item injection, completion control) — §6.1
   pins what it must satisfy, not what it looks like. Resolves with
   the injection deliverable or with RFC 0008's stage-3 amendment,
   whichever lands first.

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
  nonconformance), `src/application.rs` (the purity rustdoc INV-SE6
  canonicalizes), `src/runtime.rs` (`update_subscriptions`, the
  reconcile path INV-SE5 names), `tests/api_surface.rs` (INV-SE8's
  regression neighbor).
- `docs/rfcs/pre-review-checklist.md` — enforcement-class definitions.
