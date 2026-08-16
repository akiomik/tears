# RFC 0013: Scope Teardown

- Status: Accepted — co-designed with RFC 0014 (the reducer-first
  core), whose §4 states the kernel side of the same operation. The
  owner-document edits this contract requires land with that RFC's
  acceptance (§8); the implementation waits, with the rest of that
  kernel, on RFC 0014 §13.1's implementation-acceptance tier.
- Target: the 0.11.0 composition window (RFC 0010 §1.8). The teardown
  operation is additive public surface; the kernel it lands on carries
  RFC 0014's breaking changes, recorded there.
- Scope: prefix selection over scoped lifecycle identities across
  every run kind — keyed commands, anonymous effects, subscription
  runs — plus cleanup registrations; the public surface
  (`Command::teardown`) shared by the manual and composition layers;
  dispatch ordering; revocation strictness; totality and idempotence;
  scope reuse; subscription participation (immediate stop under
  RFC 0012's uniform barrier); cleanup-hook participation; what
  teardown cannot reach; cross-domain ordering; and the delegations
  this RFC inherits (RFC 0010 §5.3's `C-16`, register rows N30 and
  N40)
- Feature flag: none
- CHANGELOG: `Added` — `Command::teardown`, a prefix-teardown
  operation over scoped effect lifecycles (§3). Its cleanup companion
  `Command::on_teardown` is RFC 0014 §4.4's surface. Both land with
  the RFC 0014 implementation, after that RFC's §13.1 gate.

## Summary

RFC 0005 §4.5 defers six questions about a future prefix-teardown
operation, and RFC 0010 §5.3 delegates that operation — together with
runtime scope-tree ownership (N30) and the teardown half of child
appear/disappear teardown (N40) — to this RFC, which gates the
composition work. All six are resolved here, jointly with RFC 0014,
whose kernel supplies the delivery and tracking contract the
resolutions stand on:

1. **Selection is a complete prefix path over every run kind**
   (§3.1) — a teardown selects exactly the runs whose scope path
   begins with its prefix: keyed commands, anonymous effects, and
   subscription runs, plus the prefix's unfired cleanup
   registrations; matching a segment anywhere in a path is rejected
   (§10).
2. **One surface for both layers** (§3.2) — `Command::teardown(seg)`
   is the manual primitive, the composition combinators invoke the
   same constructor when draining their removal journals, and
   anchoring composes through `scoped` exactly as explicit cancel IDs
   do.
3. **Teardown applies in the cancel phase** (§3.3) — before every
   spawn of the same command, batch children included (they lower to
   independent keyed entries, RFC 0014 §3.4), so removing and
   reinserting a child in one update works at batch granularity.
4. **Strictness is revocation** (§3.4) — from the application point,
   no selected run's output is delivered: buffered before or sent
   after, message or producer quit, command or subscription output
   alike (RFC 0014 INV-RC5/INV-RC6).
5. **Total and idempotent** (§3.5), and **scope reuse observes
   nothing stale** (§3.6) — no scope-generation state exists; per-run
   tokens and the fresh-slot rule carry the property.
6. **Subscriptions participate by immediate stop** (§4) — the
   application point issues stop requests to the selected
   subscription runs and revokes them; declaration removal is paired
   structurally by the combinators; admission stays ordered by
   RFC 0012's uniform quiescence barrier.

The lookup strategy — scanning entries versus a secondary index,
RFC 0005 §4.5's second question — is mechanism, deliberately unpinned
(§3.7). The classes unreachable under RFC 0003's delivery topology —
unkeyed tasks, output already in the shared channel, batch-folded
child keys — are reachable on the RFC 0014 kernel through scope
tracking, revocation filtering, and multi-keyed lowering; what remains
unreachable is pinned in §3.8.

## 1. Context and constraints

### 1.1 Charter

RFC 0005 Phase B made child-instance identity expressible (`scoped`),
and its §4.5 states that scoping is not teardown: no ownership handle,
no prefix-cancellation command. It defers six decisions to this RFC,
each resolved in the section named:

1. whether selection matches one segment or a complete prefix path —
   a complete prefix path (§3.1);
2. whether lookup scans current entries or maintains a secondary
   index — mechanism, unpinned (§3.7);
3. ordering relative to same-update spawns and explicit ID cancels —
   the cancel phase, commuting with explicit cancels (§3.3);
4. treatment of running and finished-but-buffered command output —
   revocation: cancellation requested, nothing delivered (§3.4);
5. whether subscriptions participate and how declarative
   re-evaluation interacts with teardown — immediate stop, paired
   with declaration removal (§4); and
6. whether a later child reusing the same scope can observe stale
   teardown state — no, by construction (§3.6).

The one known client is TCA-parity collection composition: RFC 0014
§2.5's `for_each` combinator, whose removal journal merges one
`Command::teardown` per removed key into the parent's returned
command (RFC 0014 INV-RC3), automatically cancelling a removed child
instance's in-flight effects. Full effect parity in the composition
surface depends on this contract, which is why this RFC gates it.

### 1.2 Delegation inherited

From RFC 0010:

- **`C-16` (§5.3).** This RFC owns scope-tree ownership, teardown
  ordering, and the six questions above. It blocks composition: no;
  it gates the composition surface: yes — discharged by co-landing
  with RFC 0014 (its §8 (e)).
- **N30 (§10.1).** The runtime scope tree is owned here and resolved:
  the runtime tracks task-by-scope first-class, so anonymous (unkeyed)
  effects spawned through a composition boundary carry scope
  membership and are selectable — no logical key, no second identity
  model (§3.1, §3.7). In-stream structured concurrency
  (`FuturesUnordered` and kin inside one effect stream) stays
  expressible: its children live and die with the owning run, which
  is the selectable unit.
- **N40, teardown half (§10.1).** Child appear/disappear teardown is
  jointly owned with the composition surface: teardown ordering is
  this RFC's (§6), the combinators do not interfere (R3), and the
  cleanup window sits at the head of RFC 0012 §3's
  stop-requested→quiesced interval (§5) — shared with the
  graceful-drain delegation (RFC 0010 §10.1, N27), which owns any
  future bounded grace window.

### 1.3 Derivation base

This RFC derives from the following contracts; the amendments and
supersessions its design requires of them are enumerated in §8 and
ride RFC 0014 §9's register:

- **RFC 0014 §3's kernel delivery contract** — one origin-tagged data
  lane with revocation filtering at the delivery decision point, the
  control-lane and synchronous quit routes, and the fixed pass
  structure. It is the successor of RFC 0003's delivery topology
  (RFC 0014 §9, rows 1–3); the RFC 0003 discipline that survives on
  the kernel — the token rule (INV-8), one-item drain (INV-10), and
  §4.3's dispatch phase order — is cited here in that surviving form.
- **RFC 0005's identity laws** (INV-14–INV-21). Selection compares
  scope paths structurally; the path representation stays internal and
  no scope introspection becomes public (RFC 0005 §2.3). Teardown
  remains an explicit operation — dropping a scoped value still tears
  nothing down (RFC 0005 INV-21).
- **RFC 0012's boundary vocabulary and admission rules** (§3,
  INV-SE2–INV-SE5), which §4 consumes with one additive extension —
  the fourth stop cause — and **RFC 0011's phase order and
  termination postconditions** (INV-LC1, §4.4), unchanged.

## 2. Requirements

The teardown contract is reviewed against this list:

- **R1 — complete teardown.** One `Command::teardown` operation
  reaches every effect a child instance owns: keyed commands (running
  and finished-but-queued), anonymous effects spawned through the
  child's boundary, subscription runs, and the child's unfired cleanup
  registrations. An architecture in which no single operation can
  satisfy this is rejected.
- **R2 — composable anchor.** The teardown directive itself composes
  through `scoped`: a parent further scoped by its own parent targets
  the correct deeper prefix with no knowledge of its ancestors.
- **R3 — combinator non-interference.** Teardown ordering holds
  without the composition layer observing or awaiting quiescence: the
  combinators return commands and never block on the barrier
  (RFC 0012 §4.4, RFC 0014 §4.3).
- **R4 — same-update remove and reinsert.** Removing a child and
  reinserting one under the same scope value in one update works: the
  old instance's teardown neither kills nor suppresses the new
  instance's spawns. This holds at batch granularity — the multi-keyed
  lowering (RFC 0014 §3.4) puts the old instance's teardown and the
  new instance's fresh keyed spawns in one returned command — and the
  new instance's subscriptions are admitted at the next re-evaluation
  behind the old runs' quiescence: deferred by ordering, never lost
  (§4.3).
- **R5 — selection isolation.** Teardown of one scope selects,
  revokes, and suppresses nothing outside its selection — no other
  scope, no root-global slot, no subscription outside the prefix
  (INV-ST6). Subscription *admission* availability is deliberately not
  covered by this claim: under RFC 0012 §4's uniform barrier, a slow
  teardown-stopped subscription run defers every pending admission
  runtime-wide — an observable coupling between unrelated children,
  accepted as documented negative space (§4.3, RFC 0014 §5.1).
- **R6 — observability.** Teardown is observable through the existing
  producer gauges (RFC 0006 §4.4, under RFC 0014 §9 row 9's
  vocabulary: keyed runs, anonymous runs, subscriptions). No dedicated
  teardown event is pinned; an RFC that needs one adds it there.
- **R7 — two-layer test coverage.** The command half is
  contract-testable in the non-executing store: the store consumes the
  same lowered parts the kernel consumes — teardown entries and
  multi-keyed batch children included (RFC 0014 §7.1; the RFC 0008
  parity extension, §8) — and §7.2 requires the store-layer tests.
  The subscription and cleanup halves are execution and belong to the
  stage-3 `TestDriver` (RFC 0014 §7.2), driven pass-unit.
- **R8 — two layers, one surface.** The manual primitive and the
  composition machinery invoke the same `Command::teardown`
  constructor — no internal twin — so correctness of child teardown
  does not rest on hand-written anchors, and the machinery adds no
  reach the primitive lacks. The one-surface property is checked
  structurally (§7.2's origination review).
- **R9 — totality and idempotence.** Teardown is defined for every
  constructible prefix; zero matches is a no-op; reapplication is
  observationally a single application.

## 3. The teardown operation

### 3.1 Selection: complete prefix path, every run kind

A teardown operation carries a scope-path prefix of one or more
segments. It selects exactly the runs whose full identity's scope path
*begins with* that prefix — segment-by-segment equality (concrete type
and structural value, RFC 0005 §2.3) over the prefix's length,
starting at the path root — across every run kind:

- **keyed command runs**, whose scope path is the keyed ID's; the
  local key type and value never participate in selection;
- **anonymous (unkeyed) effect runs** spawned through a composition
  boundary, which carry scope membership under the kernel's
  first-class task-by-scope tracking (RFC 0014 INV-RC7) — no logical
  key and no second identity model; an anonymous run spawned with no
  boundary above it has an empty scope path and is never selected;
- **subscription runs**, by their full ID's scope path (§4);

plus the prefix's **unfired cleanup registrations**, which are
consumed and started (§5). Cleanup *runs* are the one run kind not
selected: a finalizer already started runs to completion under the
strict frame, and only termination cancels it (RFC 0014 §4.4).
Consequences, each a required test (§7.2):

- a run whose path equals the prefix (scope-only path plus local key)
  is selected;
- a run whose path is shorter than the prefix is not selected;
- a run containing the prefix's segments deeper in its path,
  reordered, or as a proper subset is not selected.

An empty prefix — teardown of everything — is not constructible: the
operation is built from at least one scope value. A root-level
cancel-all is excluded (§10).

### 3.2 Surface and anchoring

`Command::teardown(seg)` is the public constructor: it builds the
operation from one scope value. `scoped(s)` applied to a command
carrying a teardown entry prepends `s` to that entry's prefix, exactly
as it prepends to explicit cancel IDs (RFC 0005 §4.3; RFC 0005 INV-18
covers teardown prefixes and cleanup registrations — the coverage
clause RFC 0014 §9 row 6 carries). A teardown constructed at a
composition boundary therefore targets that boundary's subtree and
nothing above or beside it, and an aggregating parent needs no
knowledge of its own ancestors (R2).

The composition combinators invoke the same constructor: a removal
journal drain merges one `Command::teardown` per removed key into the
parent's returned command, qualified by the boundary's segment like
every other identity-bearing carrier (RFC 0014 §2.5, INV-RC2/INV-RC3).
There is no internal twin (R8). Lowering, selection, and application
are the kernel's.

### 3.3 Dispatch ordering: the cancel phase

Within one dispatched command, teardown entries apply in the same
phase as explicit cancels: after pre-spawn reconciliation and before
every spawn of the same command (RFC 0003 §4.3's phase order, extended
to batch children by RFC 0014 §3.4). Application is commutative with
the same command's explicit cancels — both are strict, idempotent
drops, so a prefix covering an explicitly cancelled ID is applying the
same removal twice.

A spawn from the same command whose full ID falls under a torn-down
prefix starts fresh under every `CancelPolicy`: the cancel phase has
already emptied the slot, so the spawn takes the `Absent → Start`
transition and `KeepInFlight` has nothing to suppress. Across
commands, RFC 0003 INV-10 still serializes: a teardown dispatched by
one message applies before the next input is pulled.

In `Command::batch`, each child's spawn key lowers to its own
independent keyed entry, and cancel and teardown entries from all
children apply in the one cancel phase that precedes every spawn from
the same command (RFC 0014 §3.4, INV-RC4). `scoped` applied to the
batch distributes over children, qualifying teardown prefixes along
with spawn keys, cancel IDs, and cleanup registrations. This is what
makes same-update remove-and-reinsert work at batch granularity (R4):
the combinator's journal yields the old instance's teardown and the
reinserted child's fresh keyed spawns in one returned command.
Cleanup registrations from the same command apply in the spawn
phase (§5).

### 3.4 Strictness: revocation

For each selected run, teardown is revocation at its application point
(RFC 0014 §3.1):

- if the run's task is still executing, its cancellation is
  requested — for a subscription run, a stop request in RFC 0012 §3's
  sense (§4); a run whose task has already exited has nothing to
  cancel, and only its queued output is affected; and
- from the application point, none of the run's output is handed to
  `update` — output queued before, output sent after, the current
  input batch's remainder included, for every queued item regardless
  of queue depth or the task's exit timing (the kernel carrier is
  RFC 0014 INV-RC5, and INV-RC6 for the subscription kind);
- a queued producer quit under the prefix does not quit the
  application: a producer quit travels the control lane and is applied
  only if its origin is still live (RFC 0014 §3.3) — cancel still
  beats a buffered quit.

Natural finish is not revocation: a run that finished on its own with
output still queued stays deliverable unless selected. The
cancellation-safety property extends to prefixes and to every producer
kind: a shared message (a navigation event removing a pane) whose
`update` returns a teardown suppresses the pane's
ready-but-undelivered results — keyed, anonymous, and subscription
output alike — before they can be delivered.

Strictness is the pinned frame in the same sense as RFC 0011 §4.5's
zero-grace frame for termination: a future graceful teardown is one
that inserts a bounded cleanup window between the teardown's
application and the cancellation requests, owned by the graceful-drain
delegation (RFC 0010 §10.1, N27), and it must preserve §3.6's
no-stale-observation property and reach the same end state.

### 3.5 Totality and idempotence

Teardown is defined for every constructible prefix. Zero matches is a
no-op. Reapplying a teardown with no intervening spawn under its
prefix is observationally identical to applying it once (R9) — a
second application selects the emptied region, takes the no-op path,
and re-fires no consumed cleanup hook (RFC 0014 INV-RC8). A spawn
between two applications is a new occupant, and the second application
legitimately selects it; that is two applications, not a failed
idempotence. This is strict-idempotence extended from one ID to a
selection across run kinds.

### 3.6 Scope reuse

After a teardown's application point, a run's identity slot is free
for a successor, and a spawn whose full ID falls under the torn-down
prefix observes a fresh `Absent` slot. Late task exits and late sends
from torn-down runs are inert with respect to the successor —
RFC 0003 INV-8's token discipline, preserved on the kernel (RFC 0014
§3.1's no-stale-resurrection clause). A later occupant of the same
scope observes no residue of the teardown beyond the ordinary
lifecycle rules — there is no scope-generation state to observe, and
none is introduced. The sixth deferred question is answered *no* by
construction for the strict frame; a graceful window re-poses it and
must preserve the answer (§3.4).

### 3.7 Mechanism left free

Whether the kernel finds matching runs by scanning its run bookkeeping
or by maintaining a prefix index is mechanism: both preserve every
invariant in §7, path representation is already internal (RFC 0005
§2.3), and no complexity bound is part of this contract. What is *not*
mechanism is the existence of first-class task-by-scope tracking: it
decides that anonymous runs are reachable at all (INV-ST1), so the
tracking's existence is contract while its shape — RFC 0014 §10's run
registry and cleanup ledger are the informative reference — is the
implementation's.

### 3.8 What teardown cannot reach

A teardown operation cannot reach two classes. The first is what has
already crossed the delivery decision point or left the runtime's
custody:

- **messages already delivered to `update`**, and state mutations
  already applied;
- **external side effects already performed** by any run;
- **input already consumed from an external resource** by a
  stop-requested, not-yet-quiesced source — revocation filters
  delivery, it cannot return what a source already took; ordering a
  successor behind that source's quiescence is the barrier's job
  (§4.3).

The second is input that carries no run origin: **key-addressed
external input** addressed to a removed child that has been
re-inserted under the same key reaches the new instance (RFC 0014
§2.5's routing boundary) — documented negative space of the
composition surface, unchanged by teardown.

The classes RFC 0003's topology left unreachable are not exceptions
here: anonymous runs are selected (§3.1), a revoked run's undelivered
data-lane output is retracted (§3.4), and batch children have their
own entries (§3.3) — each an affirmative, regression-tested reach
(§7.2).

## 4. Subscription participation: immediate stop

### 4.1 The stop, in RFC 0012's vocabulary

The teardown's application point issues stop requests to the selected
subscription runs and revokes them. In RFC 0012 §3's boundary
vocabulary: the application point is those runs' *stop requested*
boundary — teardown is a stop cause beside leaving the desired set,
shutdown, and abrupt teardown (the fourth-cause amendment to RFC 0012
§3, carried by RFC 0014 §9 row 5); *quiesced* follows on the
executor's schedule, and the quiescence of a teardown-stopped run
marks subscriptions dirty like any steady-state stop —
termination-driven quiescence stays excluded (RFC 0012 INV-SE5,
RFC 0014 §5.2).

Delivery from a selected subscription run ends at the application
point — revocation (§3.4), not quiescence: the run's undelivered
output, buffered or sent during the request→quiescence tail, is never
handed to `update` (RFC 0014 INV-RC6). What the stop cannot do is
un-consume input the run already read (§3.8).

### 4.2 Declaration pairing

The combinators remove the torn-down child's subscription declarations
in the same update whose journal drain issues the teardown (RFC 0014
INV-RC3), so the stop is never self-defeating. The manual primitive
applied to a *still-declared* subscription stops the run, and the next
re-evaluation restarts it — RFC 0005 INV-13's restart meaning,
untouched — which makes that use self-defeating by design; the
primitive is sound for subscriptions only when the caller also removes
the declarations, which the composition layer does structurally.

### 4.3 Admission under the uniform barrier

Successor admissions follow RFC 0012 §4 unchanged: an admission
executes only at a subscription re-evaluation (INV-SE5), ordered
behind the quiescence of every stop-requested subscription run —
teardown-stopped runs included (INV-SE3). A same-update reinsert's
subscriptions are therefore deferred, never lost: the next
re-evaluation admits them once the old runs quiesce (R4). A
re-evaluation that itself issues stop requests admits nothing in the
same pass (RFC 0014 §5.3).

The barrier's subjects are subscription runs only: a teardown-selected
command run neither joins the barrier nor triggers it, and neither
does a cleanup run — which a teardown never selects at all (§3.1),
and which is therefore outside the barrier for two reasons rather than
one. Neither kind polls an input source, so there is no stolen-input
hazard to close through them (RFC 0014 §5.1). The availability
coupling the uniform barrier carries — one
slow-quiescing teardown-stopped source defers unrelated children's
admissions runtime-wide — is accepted as documented negative space
(R5; RFC 0014 §5.1), and narrowing the barrier by declared conflict
domains stays rejected there.

## 5. Cleanup participation

`Command::on_teardown(effect)` registers a finalizer against the scope
at the call boundary, qualified by `scoped` and the combinators like
every other identity-bearing carrier (§3.2). The surface and the
finalizer contract — at most once, no runtime-visible output of any
kind, consumed-not-rerun, termination discards unfired registrations
and cancels running cleanup runs — are RFC 0014 §4.4's
(INV-RC8). What this RFC pins is
the teardown-side participation:

- a teardown **consumes** the prefix's unfired registrations at its
  application point and starts them there — the head of the
  stop-requested→quiesced interval the cleanup window sits in (§1.2),
  running concurrently with the torn-down runs' quiescence under the
  strict frame (§3.4);
- registration applies in the **spawn phase** of its command — after
  the same command's cancel phase — so a teardown-and-reregister
  command consumes the old occupant's hooks and leaves the new
  registration armed (RFC 0014 §3.4);
- re-applying a teardown re-fires nothing (§3.5); and
- cleanup runs produce no runtime-visible output at all — no message,
  no producer quit, no directive — join no barrier, and their
  quiescence marks no dirt (§4.3; RFC 0014 §4.4/INV-RC8, §5.1, §5.2).

## 6. Cross-domain ordering and termination

Teardown ordering across the domains is stated over observable
surfaces — delivery stop and admission — never over task-exit
instants: cancellation is a request, and task futures are dismantled
on the executor's schedule (RFC 0011 §4.4).

At the teardown's application point, in its command's cancel phase:
every selected run of every kind is revoked — its delivery to `update`
ends there (§3.4) — its cancellation or stop is requested, and the
prefix's unfired cleanup finalizers start (§5). After the point,
quiescence follows per run on the executor's schedule; a
teardown-stopped subscription run's quiescence marks subscriptions
dirty, and successor admissions are ordered behind quiescence at the
next re-evaluation (§4.3). The composition layer observes none of it:
combinators return commands and never await quiescence (R3). The
orphan measures for a torn-down scope's output — zero deliveries
after the application point, and the lane-split occupancy bounds over
the request→quiescence tail — are the kernel's, pinned at RFC 0014
§4.3.

Termination takes precedence: a teardown in flight when the runtime
terminates adds nothing to RFC 0011 §4.4's two postconditions,
termination-driven quiescence marks no dirt (RFC 0012 INV-SE5's
exclusion), and termination fires no cleanup hooks — it discards
unfired registrations and cancels running cleanup runs (RFC 0014
INV-RC8). This RFC adds no claim there; §7.2 carries a regression test
that the postconditions hold unchanged with a teardown in the final
update.

## 7. Invariants and contract tests

### 7.1 Invariants

Every invariant in this section is behavioral except where its own
statement declares otherwise (INV-ST7's absence half): unit and
property tests on the selection predicate, plus contract tests through
the production dispatch path on the RFC 0014 kernel — the store layer
for the command half, the stage-3 driver (pass-unit, RFC 0014 §7.2)
for execution. Async tests use deterministic synchronization, never
sleeps. Where an invariant's carrier is a kernel invariant, the
citation names it; the statement here is the teardown-side contract in
full.

- **INV-ST1: prefix selection.** A teardown selects exactly the runs
  whose scope path begins with its prefix — segment-by-segment
  type-and-value equality over the prefix's length from the path
  root — across every run kind: keyed command runs, anonymous runs
  spawned through a composition boundary, and subscription runs; plus
  the prefix's unfired cleanup registrations. Cleanup runs already
  started are not selected (§3.1). Runs with shorter paths
  (a root-spawned anonymous run's empty path included), reordered
  segments, subset segments, or the same segments under a different
  root are not selected. Local keys never participate in selection.
  The anonymous kind's reachability is carried by the kernel's scope
  tracking (RFC 0014 INV-RC7).
- **INV-ST2: anchored composition.** `scoped(s)` prepends `s` to every
  teardown prefix present at the call boundary, exactly as for
  explicit cancel IDs; the combinators apply the same qualification
  automatically (RFC 0014 INV-RC2), and cleanup registrations are
  qualified alike (RFC 0005 INV-18's coverage).
- **INV-ST3: cancel-phase application.** Teardown entries apply with
  the same command's explicit cancels — after pre-spawn
  reconciliation, before every spawn of the same command, batch
  children included (RFC 0014 §3.4) — and commute with them. A
  same-command spawn under a torn-down prefix starts fresh under every
  `CancelPolicy`.
- **INV-ST4: revocation per selected run.** Each selected run is
  revoked at the application point: its task's cancellation is
  requested if the task is still executing — a stop request in
  RFC 0012 §3's sense for subscription runs — and none of the run's
  output is handed to `update` afterwards:
  output queued before or sent after, message or producer quit,
  command or subscription output alike, for every queued item
  regardless of queue depth or task-exit timing (the kernel carriers
  are RFC 0014 INV-RC5 and INV-RC6). A queued producer quit under the
  prefix does not quit the application.
- **INV-ST5: total and idempotent.** Every constructible prefix is
  accepted; zero matches is a no-op; reapplication with no intervening
  spawn under the prefix is observationally a single application, and
  consumed cleanup hooks are not re-fired (RFC 0014 INV-RC8).
- **INV-ST6: selection isolation.** No run or registration outside the
  selection is affected: equal local IDs under other scopes,
  root-global slots, and subscriptions outside the prefix keep their
  replacement, suppression, delivery, and restart behavior.
  Subscription admission *availability* is carved out: the uniform
  barrier's runtime-wide deferral behind a teardown-stopped run is
  accepted, documented negative space (§4.3), not a violation of this
  invariant.
- **INV-ST7: reuse without stale observation.** After application, a
  spawn under the torn-down prefix observes a fresh slot; late task
  exits and late sends from torn-down runs are inert (§3.6); a later
  occupant observes no teardown residue beyond the ordinary lifecycle
  rules. Its two halves take different classes, for the reason
  RFC 0006 INV-L9 splits the same way — cited for its method, not as
  a live neighbour: that invariant is itself not preserved on this
  kernel (RFC 0006 §5.2). The **observable** half —
  fresh-slot spawn, inert late exit, inert late send — is
  **behavioral**: the fresh-start rows below, scripted per case. The
  **absence** half — no scope-generation state exists and none is
  introduced, so no residue can be observed at all — is
  **structural**, an inventory review of the runtime's per-scope
  state at the teardown application and spawn sites, because no finite
  set of fresh-start scripts proves it: an implementation that taints
  only the scopes a test never reuses passes every such script. The
  §7.3 *generation-tracking* adversary is excluded by that review,
  with the fresh-start rows as its regression neighbours.
- **INV-ST8: the unreached.** Teardown affects nothing already
  delivered to `update`, no state mutation already applied, and no
  external side effect already performed; it cannot un-consume input a
  stop-requested, not-yet-quiesced source has already read; and it
  does not re-route key-addressed external input (§3.8). The listed
  non-effects are regression-tested. Anonymous-run reachability,
  undelivered-output retraction, and batch-child selection are
  affirmative reach under INV-ST1/INV-ST4, not exceptions to this
  invariant.

### 7.2 Required tests

Selection unit tests: path equal to prefix; path shorter than prefix
(the empty anonymous root path included); prefix segments deeper,
reordered, or subset; equal local IDs under sibling scopes; local key
excluded from matching; constant-hash scope values still selected
structurally (RFC 0005's collision-safety discipline — INV-2 and the
Phase B constant-hash scope tests — applied to selection); selection
uniform across the three run kinds.

Kernel tests, through the production dispatch path (pass-unit driver
steps, RFC 0014 §7.2), on both lane modes where delivery is asserted —
the bounded-mode witness being RFC 0014 §13.1's
`bounded-lane revocation` series, which scripts a bounded lane under the
two
protocol conditions RFC 0014 §13.3 names and carries no bounded-lane
determinism claim beyond the enqueue order the grant handshake fixes:

- teardown of a prefix with one running and one finished-but-queued
  run revokes both and delivers neither — asserted as no `update`
  invocation, including for items already queued when the teardown
  applies;
- teardown entries contributed by batch children apply in the one
  cancel phase before every spawn of the same command, and `scoped` on
  the batch qualifies the children's prefixes (§3.3, INV-ST2);
- a queued producer quit under the prefix does not quit (origin
  revoked before the control-lane drain applies it);
- a same-command spawn under the torn-down prefix runs, under both
  `CancelInFlight` and `KeepInFlight`;
- teardown plus explicit cancel of a covered ID in one command behaves
  as teardown alone;
- zero-match teardown is a no-op; repeating a teardown changes nothing
  and re-fires no cleanup hook;
- runs under sibling scopes, a root-global slot, and a subscription
  outside the prefix are untouched through a teardown of one pane's
  prefix;
- a next-update spawn under the torn-down prefix starts fresh; a late
  task exit bearing a torn-down run's identity is inert — the test
  deterministically produces the torn-down run's exit and processes
  it, asserting the successor's run is untouched and still delivers;
- an anonymous effect spawned through the torn-down child's boundary
  is revoked and its undelivered output never reaches `update`
  (INV-ST1's anonymous kind; RFC 0014 INV-RC7);
- a subscription run under the prefix is stop-requested at the
  application point and its undelivered output is retracted — both
  asserted before any subscription re-evaluation runs, so an
  implementation that defers either to re-evaluation fails the test
  (§4.1; RFC 0014 INV-RC6);
- after the teardown-stopped run quiesces, subscriptions are marked
  dirty and the next re-evaluation admits the then-declared set — the
  reinserted child's subscriptions included — and admits nothing while
  the stopped run has not quiesced (§4.3, RFC 0012 INV-SE3/INV-SE5);
- a teardown-selected command run defers no subscription admission
  (barrier scope, RFC 0014 §5.1, INV-RC12);
- a message from a selected run already delivered to `update` before
  the teardown stays delivered, and the state transition it caused
  stays applied — no retroactive effect (INV-ST8);
- an external side effect a selected run performed before the
  teardown — observed through a test-provided instrument — is not
  undone or compensated (INV-ST8);
- consumed input is not returned — against a destructive,
  non-replaying test resource: a token a teardown-stopped source
  consumed before quiescing is not returned to the resource by the
  runtime, and the runtime does not itself redeliver it to the
  successor — the successor's own reads find the resource without it.
  A source's own replay semantics on reconnection are outside this
  test's subject: RFC 0012 §2's template deliberately pins neither
  the resource-acquisition point nor the items a source's stream
  yields, and a source that replays history conforms (INV-ST8);
- key-addressed external input arriving after a same-key
  remove-and-reinsert reaches the new instance (INV-ST8; RFC 0014
  §2.5's routing boundary);
- cleanup: a registration under the prefix starts at the application
  point; a teardown-and-reregister command consumes the old hooks and
  leaves the new registration armed (§5);
- teardown in the final update before quit: RFC 0011 §4.4's immediate
  postcondition holds at `run()`'s return, the quiescent postcondition
  is then reached under a bounded settle loop (RFC 0011 INV-LC7), and
  no cleanup hook fires from termination (§6).

Store tests, through the store's command intake (RFC 0008 INV-T3):
prefix selection over the pending set, multi-keyed batch children
included; strict drop of a selected occupant's queued output, a queued
quit included; teardown before the same command's spawn — the
reinserted child's leaf stays deliverable; root-path anonymous pending
output unaffected. A store that ignores teardown entries in the parts
fails these.

One structural check accompanies these tests, for R8's one-surface
property, which no behavioral test can prove — an internal twin
produces lowered entries identical to the public surface's. The
review walks the teardown *origination* routes: the public
`Command::teardown` constructor and the combinators' journal-drain
sites, confirming that every teardown operation originates in a call
to the public constructor — the journal drain included — and that no
route below the public surface originates one from a raw prefix.
Transformations of an already-originated operation are not
origination and stay free: `scoped`'s prefix qualification, the
aggregation of batch children's entries, and the lowering from
command metadata to the runtime parts all transform existing entries.
The combinator rows above are its regression neighbors, not its
proof.

### 7.3 Adversarial models considered

- *Superset canceller* — tearing down more than the selection passes
  any per-selection assertion; excluded by INV-ST6's sibling,
  root-global, and outside-subscription tests.
- *Segment-anywhere matcher* — passes prefix-equal cases; excluded by
  INV-ST1's reordered/deeper/subset tests.
- *Spawn-before-cancel implementation* — kills the same command's
  reinserted child; excluded by INV-ST3's same-command spawn test.
- *Private-twin constructor* — composition machinery that originates
  its teardown operations from raw prefixes through an internal
  constructor produces the same lowered entries and passes every
  behavioral test while violating R8; excluded by §7.2's structural
  origination review.
- *Filter-at-update and tombstone-expiry implementations* — deliver a
  revoked run's output into the batch and drop it there, or forget a
  revoked run after its task exits and deliver late-queued output;
  both are the kernel's adversaries (RFC 0014 §11), excluded by
  INV-RC5's no-`update`-invocation and late-exit checks, which
  INV-ST4's tests reuse.
- *Re-evaluation-deferred subscription stopper* — defers the stop
  request, the revocation, or both to the next re-evaluation instead
  of the application point; passes every declaration-removal test
  (the run stops eventually) while the removed child's source keeps
  delivering, or keeps consuming external input, across the gap;
  excluded by INV-ST4's subscription row, whose two assertions run
  before any re-evaluation.
- *Generation-tracking implementation* — suppresses or taints later
  same-scope spawns; excluded by INV-ST7's structural half — the
  per-scope state inventory — with its fresh-start rows as the
  behavioral neighbours.
- *No-op implementation* — trivially idempotent; excluded because
  INV-ST5's idempotence is asserted only after INV-ST4's effects are.
- *Joint-satisfiability walk* — one implementation satisfying all
  eight simultaneously against the RFC 0014 kernel seams: extend the
  lowered command parts' cancel set with prefix entries; in the
  dispatch's cancel phase, resolve each prefix against the run
  bookkeeping's scope paths (every kind), mark each selected run
  revoked, request its cancellation or stop, and consume-and-start the
  prefix's unfired cleanup registrations; in the spawn phase, spawn
  the command's children (batch children as independent entries) and
  arm its new cleanup registrations; the delivery decision point
  filters revoked origins (RFC 0014 INV-RC5) and the control drain
  discards revoked quits; quiescence of teardown-stopped subscription
  runs marks dirt and the barrier orders admissions (RFC 0012 §4). No
  lifecycle state beyond the kernel's revocation flag (RFC 0014 §10)
  is needed. The parts type has a second consumer: the store's command
  intake (RFC 0008 INV-T3) applies prefix entries to its pending
  output before the spawn decision — the §7.2 store tests pin exactly
  this, so a store that drops the prefix entries on the floor is
  excluded.

### 7.4 Excluded claims

- A batch-lowering invariant is not pinned here — RFC 0014 §3.4 and
  INV-RC4 own the multi-keyed lowering; INV-ST3 pins only the cancel
  phase's precedence over it.
- The retraction quantifiers — "every queued item, regardless of
  task-exit timing", on both lane modes — are not independently owned
  here: INV-ST4 states them with RFC 0014 INV-RC5/INV-RC6 as its
  carrier, and their behavioral checks live there.
- The cleanup finalizer contract (at most once, no runtime-visible
  output of any kind, termination discards) is not pinned here —
  RFC 0014 INV-RC8 owns it, the no-output clause structural-primary at
  the cleanup task's construction site; INV-ST1/INV-ST5 pin only its
  participation in selection and idempotence.
- Barrier scope and the stopping-pass defer are not pinned here —
  RFC 0012 §4 and RFC 0014 §5.1/§5.3 own them; §4.3 consumes them.
- A dedicated teardown observability event is deliberately absent
  (R6); the producer gauges already reflect teardown.
- INV-ST6 is kept beside INV-ST1 rather than collapsed into it:
  INV-ST1 pins the selection *set*, INV-ST6 pins that non-selection
  implies non-effect — a side-effecting matcher satisfies the first
  while violating the second.
- A termination-precedence invariant is deliberately absent: RFC 0011
  §4.4, RFC 0012 INV-SE5, and RFC 0014 INV-RC8 already pin the
  property (§6); this RFC carries only the regression test.

## 8. Cross-document sync

The owner-document edits this contract requires ride RFC 0014 §9's
supersession and amendment register, landing in its gated order after
that RFC's acceptance:

- RFC 0005 INV-18 (row 6): `scoped` coverage extends to teardown
  prefixes and cleanup registrations.
- RFC 0012 §3 (row 5): the fourth stop cause (teardown) with its dirt
  classification, and the stopping-pass defer clarification.
- RFC 0003 INV-11 / RFC 0005 INV-20 (row 3): the batch supersession —
  multi-keyed lowering replaces folding.
- RFC 0008 (row 11): the store parity extension to teardown entries
  and batch children, and the stage-3 driver amendment.

One edit is this RFC's own: RFC 0005 §4.5 records this contract as the
resolution of the six questions it deferred. RFC 0008 INV-T3
needs no text change — the store already consumes the shared parts
type — but its structural review re-runs at the store's intake once
the parts carry teardown entries (RFC 0014 §7.1).

## 9. Resolved questions

The four questions RFC 0014 §4 answers for this contract, numbered as
there, resolve in the body as follows:

1. **Public surface and owner.** `Command::teardown(seg)` is the
   manual primitive; the composition machinery invokes the same
   constructor — no internal twin (§3.2, R8).
2. **Subscription participation and admission coupling.** Immediate
   stop at the application point, paired with declaration removal by
   the combinators; the uniform barrier stays, its availability
   coupling accepted as documented negative space (§4).
3. **Scope tree and unkeyed tracking (N30).** The runtime tracks
   task-by-scope first-class; anonymous effects spawned through a
   composition boundary are selectable, with no second identity model
   and no prohibition on unkeyed child effects (§3.1, §3.7, §10).
4. **Shared-output retraction.** Yes: revocation filtering at the
   delivery decision point is contract, and the delivery window of a
   torn-down scope ends at the application point, not at quiescence
   (§3.4, §4.1).

## 10. Alternatives considered

### Segment-anywhere selection

Matching `PaneId(7)` at any path position reaches `[TabId(1),
PaneId(7)]` and `[TabId(3), PaneId(7)]` alike, violating instance
isolation (R5, RFC 0005 INV-19's discipline) with no client that
needs it. Rejected for complete-prefix selection.

### Drain-through teardown

Delivering a selected run's queued output before releasing it
contradicts the strict revocation family (RFC 0014 INV-RC5; RFC 0003's
strict-cancel intent) and would deliver messages addressed to a child
the state no longer contains. Rejected.

### Deferred subscription stop (declarative removal only)

Leaving subscription teardown to the next re-evaluation's stop
requests after declaration removal keeps RFC 0012 §3's three stop
causes untouched, but it opens a dispatch-to-re-evaluation gap in
which the removed child's source keeps running — still polling, still
consuming external input — and it gives the manual primitive no
subscription reach at all, splitting R1's "one operation" across two
mechanisms. Revoking at dispatch while deferring the stop request
closes only the delivery half of that gap: a still-polling removed
source can consume input its successor needs, the §4.3 hazard the
barrier exists to close. Rejected for the immediate stop paired with
declaration removal (§4).

### Auto-keying anonymous child effects

Closing the unkeyed gap by synthesizing keys moves anonymous child
effects into the keyed identity class — a second, implicit identity
model with the keyed capacity and gauge surfaces attached. Rejected
for kernel-side scope membership without a logical key (§3.1;
RFC 0014 §4).

### Prohibiting unkeyed child effects

Making anonymous effects unconstructible under a composition boundary
closes the same gap by fiat but breaks the child-as-standalone-app
symmetry: a reducer that runs standalone could no longer run composed
unchanged. Rejected (RFC 0014 §4).

### Scope generations

A generation counter per scope value could tag stale declarations and
output. It adds registry state and a second identity axis for a
property the strict frame already provides through per-run tokens and
the fresh-slot rule (§3.6). Rejected for the strict frame; a graceful
window must re-justify it if the preservation obligation (§3.4)
cannot be met otherwise.

### Policy-parameterized teardown

A grace/drain parameter on the operation multiplies the surface before
any requirement needs it; graceful behavior is the graceful-drain
delegation's window (§3.4). Rejected.

### Implicit teardown on drop or omission

Tearing down when a scoped value is dropped or a scoped command is
omitted contradicts RFC 0005 INV-21 and makes teardown unobservable in
the declaration. Rejected; teardown stays explicit — the combinators'
removal journals are explicit state operations (`remove`, `dismiss`,
occupied-slot replacement), not drop observation (RFC 0014 §2.5).

### Root cancel-all

An empty-prefix operation cancelling every run has no known client and
invites use as a shutdown mechanism, which RFC 0011 owns. Excluded by
construction (§3.1).

## 11. References

- RFC 0003: Command Cancellation — §4.3 dispatch phases, INV-8's token
  rule, INV-10; its delivery topology is superseded per RFC 0014 §9
- RFC 0005: Structural Lifecycle Identity — §2.3, §4.3–§4.5, INV-13,
  INV-14–INV-21
- RFC 0006: Runtime Load Control — §4.4 gauges; INV-L9, cited by
  INV-ST7 for its enforcement-class method (the invariant itself is
  superseded, its §5.2)
- RFC 0008: TestStore — INV-T3 (the parts intake), the stage-3
  amendment slot
- RFC 0010: Runtime Consolidation — §1.8, §5.3, §10.1 (N27, N30,
  N40)
- RFC 0011: Runtime Lifecycle — §2, §4.4, §4.5, INV-LC1, INV-LC7
- RFC 0012: Subscription Execution — §3, §4, §4.4, INV-SE2–INV-SE5
- RFC 0014: Reducer-First Core — §2.5, §3, §4, §5, §7, §9, §10,
  INV-RC2–INV-RC8, INV-RC12
- `src/runtime/keyed_commands.rs`, `src/subscription.rs` — the
  registry shapes RFC 0014 §9's supersessions replace
