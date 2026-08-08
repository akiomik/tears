# RFC 0013: Scope Teardown

- Status: Draft — the command-registry half (§3) and the cross-domain
  ordering frame (§5) are complete and reviewable; the public surface,
  subscription participation, and scope-tree ownership are co-designed
  with the composition RFC (RFC 0010 §5.2 (a)/(e)) and stand as open
  questions (§8). Implementation waits on their resolution.
- Target: the 0.11.0 composition window (RFC 0010 §1.8). The operation
  is additive; no existing contract's behavior changes.
- Scope: prefix selection over scoped lifecycle identities, the
  teardown operation's dispatch ordering, strict treatment of running
  and buffered output, totality and idempotence, scope reuse, the
  operation's negative space (what it cannot reach), requirements on
  the joint command/subscription/cleanup teardown, and the delegations
  this RFC inherits (RFC 0010 §5.3's `C-16`, register rows N30 and
  N40)
- Feature flag: none
- CHANGELOG: `Added` — a prefix-teardown operation over scoped command
  lifecycles (§3). Lands with the implementation.

## Summary

RFC 0005 §4.5 defers six questions about a future prefix-teardown
operation (`Command::cancel_scope` in its sketch), and RFC 0010 §5.3
delegates that operation — together with runtime scope-tree ownership
(N30) and the teardown half of child appear/disappear teardown (N40) —
to this RFC, which must precede or accompany the composition RFC
(RFC 0010 §5.2 (e)). This draft resolves the command-registry half of
the six and records the co-designed remainder:

1. **Selection is a complete prefix path** (§3.1) — a teardown selects
   exactly the entries whose scope path begins with its prefix;
   matching a segment anywhere in a path is rejected (§9).
2. **Anchoring composes through `scoped`** (§3.2) — a teardown issued
   at a composition boundary targets that boundary's subtree and
   nothing above it.
3. **Teardown applies in the cancel phase** (§3.3) — with explicit
   cancels, before the same command's spawns, so removing and
   reinserting a child in one update works at single-command
   granularity.
4. **Strict, like explicit cancel** (§3.4) — running tasks are
   cancelled, buffered output including a buffered keyed quit is
   discarded, nothing selected delivers afterwards.
5. **Total and idempotent** (§3.5), and **scope reuse observes nothing
   stale** (§3.6) — no scope-generation state exists; per-run tokens
   and the fresh-slot rule carry the property.
6. **Subscription participation is co-designed** (§4) — the recorded
   baseline is declarative removal by the composition layer; an
   imperative stop is costed as an RFC 0012 amendment. Which one the
   contract adopts is open question 2.

The lookup strategy — scanning entries versus a secondary index,
RFC 0005 §4.5's second question — is mechanism, deliberately unpinned
(§3.7). What one teardown cannot reach — unkeyed tasks, shared-channel
output, batch-folded child keys — is pinned as negative space (§3.8)
and handed to the composition RFC as its completion obligation.

## 1. Context and constraints

### 1.1 Charter

RFC 0005 Phase B made child-instance identity expressible
(`scoped`), and its §4.5 states that scoping is not teardown: no
ownership handle, no prefix-cancellation command. It defers six
decisions to this RFC:

1. whether selection matches one segment or a complete prefix path
   (§3.1);
2. whether lookup scans current entries or maintains a secondary index
   (§3.7);
3. ordering relative to same-update spawns and explicit ID cancels
   (§3.3);
4. treatment of running and finished-but-buffered command output
   (§3.4);
5. whether subscriptions participate and how declarative re-evaluation
   interacts with teardown (§4); and
6. whether a later child reusing the same scope can observe stale
   teardown state (§3.6).

The one known client is TCA-parity collection composition (RFC 0005
§4.5): a `forEach`-style reducer must automatically cancel a removed
child instance's in-flight effects when the child leaves the
collection. Full effect parity in the composition RFC depends on
prefix teardown, which is why this RFC gates it.

### 1.2 Delegation inherited

From RFC 0010:

- **`C-16` (§5.3).** This RFC owns scope-tree ownership, teardown
  ordering, and the six questions above. It blocks composition: no;
  it gates the composition RFC: yes.
- **N30 (§10.1).** The runtime scope tree — whether the runtime tracks
  task ownership by scope — is owned here. In-stream structured
  concurrency (`FuturesUnordered` and kin inside one effect stream)
  stays expressible under the current topology and is not regulated.
- **N40, teardown half (§10.1).** Child appear/disappear teardown is
  jointly owned with the composition RFC: teardown ordering is this
  RFC's (§5), the adapter does not interfere (RFC 0010 §5.2 (f)), and
  the cleanup window sits inside RFC 0012 §3's stop-requested→quiesced
  interval — shared with the graceful-drain delegation (RFC 0010
  §10.1, N27).

### 1.3 Non-negotiables

This RFC derives from, and changes nothing in:

- **RFC 0003's delivery contract and lifecycle state machine** — the
  reference contract RFC 0010 §3.1 reaffirms. Teardown adds entries to
  the existing cancel phase (§3.3) and adds no transition, state, or
  decision variant.
- **RFC 0005's identity laws** (INV-14–INV-21). Selection compares
  scope paths structurally; the path representation stays internal and
  no scope introspection becomes public (RFC 0005 §2.3). Teardown
  remains an explicit operation — dropping a scoped value still tears
  nothing down (RFC 0005 INV-21).
- **RFC 0012's admission rules and composition transparency**
  (INV-SE2–INV-SE5, §4.4) and **RFC 0011's phase order and
  termination postconditions** (INV-LC1, §4.4).

These stand as reference contracts. If the composition co-design
supersedes any of them, the derivations in §3–§5 are re-run against
the successor text before this RFC leaves Draft.

## 2. Requirements

The joint teardown contract — this RFC plus the composition RFC's
obligations — is reviewed against this list:

- **R1 — complete teardown.** One scope-teardown operation reaches
  every effect a child instance owns: keyed commands (running and
  draining), subscriptions, and any future cleanup hooks. Under the
  §4.1 baseline the "one operation" is the composition-level teardown
  (declaration removal composed with the command-half operation); the
  manual primitive alone does not satisfy R1, and an architecture in
  which no operation can satisfy it is rejected.
- **R2 — composable anchor.** The teardown directive itself composes
  through `scoped`: a parent further scoped by its own parent targets
  the correct deeper prefix with no knowledge of its ancestors.
- **R3 — adapter non-interference.** Teardown ordering holds without
  the composition adapter observing or awaiting quiescence (RFC 0010
  §5.2 (f), RFC 0012 §4.4).
- **R4 — same-update remove and reinsert.** Removing a child and
  reinserting one under the same scope value in one update works: the
  old instance's teardown neither kills nor suppresses the new
  instance's spawns. This RFC delivers R4 at single-command
  granularity (§3.3); at batch granularity it additionally requires
  the multi-keyed lowering RFC 0005 §4.4 defers, a named prerequisite
  handed to the composition RFC (§3.8).
- **R5 — command-registry isolation.** In the command registry,
  teardown of one scope selects, cancels, and suppresses nothing
  outside its selection — no other scope, no root-global slot
  (INV-ST6). Subscription *admission* availability is deliberately not
  covered by this claim: under RFC 0012 §4's uniform barrier, a slow
  teardown-stopped task defers every pending admission runtime-wide —
  an observable coupling between unrelated children. Whether the joint
  contract accepts that coupling as documented negative space or
  narrows the barrier (an RFC 0012 §4 amendment) is part of open
  question 2.
- **R6 — observability.** Teardown is observable through the existing
  entry-count gauge (RFC 0006 §4.4). No dedicated teardown event is
  pinned; an RFC that needs one adds it there.
- **R7 — TestStore coverage.** The command half is contract-testable
  in TestStore: the store consumes the same lowered parts the runtime
  consumes (RFC 0008 INV-T3), so teardown entries reach its intake,
  and the store's cancellation parity (RFC 0008 INV-T7) extends to
  them — the §7 sync names the amendment and §6.2 requires the
  store-layer tests. The subscription half awaits the
  subscription-driving store RFC 0012 §6.2 places as a future RFC 0008
  amendment.
- **R8 — two layers.** The contract supports both a manual primitive
  and automatic application by the composition machinery (RFC 0010
  §5.2 (a)); correctness of child teardown does not rest on
  hand-written anchors.
- **R9 — totality and idempotence.** Teardown is defined for every
  constructible prefix; zero matches is a no-op; reapplication is
  observationally a single application.

## 3. Command-registry teardown

### 3.1 Selection: complete prefix path

A teardown operation carries a scope-path prefix of one or more
segments. It selects exactly the keyed command entries whose full
identity's scope path *begins with* that prefix: segment-by-segment
equality — concrete type and structural value, RFC 0005 §2.3 — over
the prefix's length, starting at the path root. Consequences, each a
required test (§6.2):

- an entry whose path equals the prefix (scope-only path plus local
  key) is selected; the local key type and value never participate in
  selection;
- an entry whose path is shorter than the prefix is not selected;
- an entry containing the prefix's segments deeper in its path,
  reordered, or as a proper subset is not selected.

An empty prefix — teardown of everything — is not constructible: the
operation is built from at least one scope value. A root-level
cancel-all is excluded (§9).

### 3.2 Anchoring and composition

`scoped(s)` applied to a command carrying a teardown entry prepends
`s` to that entry's prefix, exactly as it prepends to explicit cancel
IDs (RFC 0005 §4.3). A teardown constructed at a composition boundary
therefore targets that boundary's subtree and nothing above or beside
it, and an aggregating parent needs no knowledge of its own ancestors
(R2). RFC 0005 INV-18's coverage extends to teardown prefixes; the
one-clause amendment recording that lands with this RFC's acceptance
(§7).

### 3.3 Dispatch ordering: the cancel phase

Within one dispatched command, teardown entries apply in the same
phase as explicit cancels: after pre-spawn reconciliation and before
any spawn from the same command (RFC 0003 §4.3). Application is
commutative with the same command's explicit cancels — both are
strict, idempotent drops, so a prefix covering an explicitly cancelled
ID is applying the same removal twice.

A spawn from the same command whose full ID falls under a torn-down
prefix starts fresh under every `CancelPolicy`: the cancel phase has
already emptied the slot, so the spawn takes the `Absent → Start`
transition and `KeepInFlight` has nothing to suppress. This is what
makes same-update remove-and-reinsert work at single-command
granularity (R4). Across commands, RFC 0003 INV-10 already serializes:
a teardown dispatched by one message applies before the next app-input
is pulled.

In `Command::batch`, teardown entries fold with the explicit cancels
(RFC 0003 INV-11); child spawn keys remain ignored with the existing
warning. `scoped` applied to the batch qualifies folded teardown
prefixes along with the folded cancels (RFC 0005 §4.4).

### 3.4 Strictness

For each selected entry, teardown is observationally
`Command::cancel(id)` for that entry's full ID (RFC 0003 INV-4):

- a `Running` entry's task cancellation is requested and its receiver
  dropped;
- a `Draining` entry's buffered output is discarded;
- a buffered keyed `Action::Quit` under the prefix does not quit the
  application (RFC 0003 INV-9);
- no selected entry delivers output after the teardown's application
  point — buffered output is gone before the next app-input pull,
  exactly as for explicit cancels (RFC 0003 §4.4's no-prefetch rule).

The cancellation-safety property extends to prefixes: a shared message
(a navigation event removing a pane) whose `update` returns a teardown
suppresses the pane's ready-but-undelivered keyed results before they
can be pulled (RFC 0003 INV-14 and §4.4).

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
second application selects the emptied registry region and takes the
no-op path. A spawn between two applications is a new occupant, and
the second application legitimately selects it; that is two
applications, not a failed idempotence. This is RFC 0003 INV-4's
strict-idempotence extended from one ID to a selection.

### 3.6 Scope reuse

After a teardown's application point, a spawn whose full ID falls
under the torn-down prefix observes a fresh `Absent` slot, and late
task exits from torn-down runs are inert under RFC 0003 INV-8's token
rule. A later occupant of the same scope observes no residue of the
teardown beyond RFC 0003's ordinary lifecycle rules — there is no
scope-generation state to observe, and none is introduced. The
sixth deferred question is answered *no* by construction for the
strict frame; a graceful window re-poses it and must preserve the
answer (§3.4).

### 3.7 Mechanism left free

Whether the runtime finds matching entries by scanning the registry or
by maintaining a prefix index is mechanism: both preserve every
invariant in §6, path representation is already internal (RFC 0005
§2.3), and no complexity bound is part of this contract. The current
registries are flat maps keyed by full ID
(`src/runtime/keyed_commands.rs`, `src/subscription.rs`); a scan is
the obvious first implementation, and an index is a permitted internal
optimization. Whether the runtime additionally owns a *scope tree* —
first-class task ownership by scope, N30 — is not mechanism, because
it decides what unkeyed tasks the teardown can reach; that is open
question 3.

### 3.8 Negative space: the unreached

A teardown operation reaches only entries in the keyed command
registry. It does not reach:

- **(a) unkeyed tasks and their output.** A command without a spawn
  key has no lifecycle identity (RFC 0003 INV-1); its task is not
  selectable and its output travels the shared channel.
- **(b) output already in the shared channel.** Subscription output
  and unkeyed output carry no identity once delivered into the shared
  channel; nothing there is retractable, under this or any current
  contract.
- **(c) batch-folded child spawn keys.** `Command::batch` ignores
  child spawn keys (RFC 0003 INV-11), so a batched child's effects run
  inside the batch stream without their own entries; they are
  unreachable by a later teardown for the same reason they are
  unreachable by explicit cancel.

Completing (a)–(c) into R1's full child-instance teardown is the
composition RFC's obligation, under its automatic scope application
(RFC 0010 §5.2 (a)) together with the multi-keyed lowering RFC 0005
§4.4 defers — spawning independently keyed child effects from one
returned command. Until that lowering exists, R4 holds at
single-command granularity only, and the composition RFC cannot claim
full effect parity (§1.1).

## 4. Subscription participation (co-designed)

This section records the design space for open question 2; it is not
yet contract.

### 4.1 Baseline: declarative removal

The teardown operation does not touch the subscription registry.
Subscription teardown is achieved declaratively: the composition
machinery, applying scopes automatically (RFC 0010 §5.2 (a)), stops
declaring the removed child's subscriptions in the update that removes
the child; the next frame pass's re-evaluation issues the stop
requests (RFC 0012 INV-SE5), and the quiescence barrier orders
subsequent admissions (INV-SE3).

Properties of the baseline:

- No amendment to RFC 0012: the three stop causes of its §3 stand, and
  a still-declared subscription is never imperatively cancelled — so
  RFC 0005 INV-13's restart meaning is untouched.
- The cross-domain order is fixed (§5): command teardown at dispatch,
  subscription stops at the next re-evaluation.
- Cost: between the removing update and the source's stop, a running
  source keeps delivering into the shared channel. The delay is one
  frame pass, but the count of orphaned deliveries is bounded by no
  contract — input batches can process further messages before the
  frame pass (RFC 0003 §4.4), and delivery continues until the stopped
  task quiesces. In bounded delivery mode the buffered portion at any
  instant is capped by the shared capacity; the total is not.

### 4.2 Alternative: immediate stop, costed

A teardown that issues stop requests to the selected subscriptions at
its application point requires:

- a fourth stop cause in RFC 0012 §3 (today: left the desired set,
  shutdown, abrupt teardown) — an amendment to that RFC;
- a pinned dirt classification for the resulting quiescence (INV-SE5
  distinguishes steady-state stops, which mark dirt, from
  termination stops, which do not; a teardown stop is neither); and
- pairing with declaration removal in the same update — otherwise the
  still-declared subscription restarts at the next re-evaluation
  (RFC 0005 INV-13) and the stop is self-defeating. The operation is
  therefore sound only when invoked by machinery that also owns the
  declarations, not as a free-standing user primitive.

What it buys over the baseline: the stop *request* moves from the
next frame pass to the application point. It does not close the
orphan window there — a stop is a cancellation request (RFC 0012 §3),
an in-flight poll may still complete, and delivery continues until
the task quiesces, a request→quiescence tail whose delivery count no
contract bounds. Under both options the window ends at quiescence;
only delivery-side filtering (open question 4) could end it earlier.

### 4.3 Orphaned output

Under either option, output already in the shared channel is delivered
(§3.8 (b)). Routing or ignoring messages addressed to a
no-longer-present child is the composition RFC's obligation — its
parent mapping still receives them. Filtering at delivery would
require identifying each shared-channel item's origin, a delivery
topology this contract does not assume; whether it ever becomes one is
open question 4.

## 5. Cross-domain ordering and termination

Teardown ordering across the three domains is stated over observable
surfaces — delivery stop and admission — never over task-exit
instants: cancellation is a request, and task futures are dismantled
on the executor's schedule (RFC 0011 §4.4).

Under the §4.1 baseline the order is: command-half application at
dispatch (delivery from selected entries stops before the next
app-input pull, §3.4); subscription stop requests at the next frame
pass's re-evaluation, with admissions ordered behind quiescence
(RFC 0012 §4); and any future cleanup hooks inside RFC 0012 §3's
stop-requested→quiesced interval (§1.2). The adapter observes none of
it (R3). Adopting §4.2 moves the subscription stop *request* to the
command half's application point — the request only; the delivery
window still ends at quiescence (§4.2) — and the ordering statement
is re-derived then.

Termination takes precedence: a teardown in flight when the runtime
terminates adds nothing to RFC 0011 §4.4's two postconditions, and
termination-driven quiescence marks no dirt (RFC 0012 INV-SE5's
exclusion). This RFC adds no claim there; §6.2 carries a regression
test that the postconditions hold unchanged with a teardown in the
final update.

## 6. Invariants and contract tests

### 6.1 Invariants

All invariants in this section are behavioral: unit and property tests
on the selection predicate and the lifecycle transition, plus manager
and runtime contract tests through the production dispatch path,
extending RFC 0003 §7's suites. Async tests use deterministic
synchronization, never sleeps.

- **INV-ST1: prefix selection.** A teardown selects exactly the keyed
  command entries whose scope path begins with its prefix —
  segment-by-segment type-and-value equality over the prefix's length
  from the path root. Entries with shorter paths, reordered segments,
  subset segments, or the same segments under a different root are not
  selected. Local keys never participate in selection.
- **INV-ST2: anchored composition.** `scoped(s)` prepends `s` to every
  teardown prefix present at the call boundary, exactly as for
  explicit cancel IDs.
- **INV-ST3: cancel-phase application.** Teardown entries apply with
  the same command's explicit cancels — after pre-spawn
  reconciliation, before the same command's spawns — and commute with
  them. A same-command spawn under a torn-down prefix starts fresh
  under every `CancelPolicy`.
- **INV-ST4: strict per-entry semantics.** Each selected entry is
  cancelled as by `Command::cancel` of its full ID: running task
  cancellation requested, receiver dropped, buffered output — a
  buffered keyed quit included — discarded before the next app-input
  pull.
- **INV-ST5: total and idempotent.** Every constructible prefix is
  accepted; zero matches is a no-op; reapplication with no intervening
  spawn under the prefix is observationally a single application.
- **INV-ST6: selection isolation.** No entry outside the selection is
  affected: equal local IDs under other scopes and root-global slots
  keep their replacement, suppression, and delivery behavior.
- **INV-ST7: reuse without stale observation.** After application, a
  spawn under the torn-down prefix observes a fresh slot; late task
  exits from torn-down runs are inert (RFC 0003 INV-8); a later
  occupant observes no teardown residue beyond RFC 0003's ordinary
  rules.
- **INV-ST8: the unreached.** The §3 operation affects no unkeyed
  task, no output already in the shared channel, no batch-folded child
  effect (§3.8), and no entry of the subscription registry. The listed
  non-effects are regression-tested. Whether a *joint* teardown
  additionally issues subscription stops is open question 2; either
  resolution layers on this invariant rather than weakening it — the
  §4.2 alternative is a distinct stop operation with its own
  amendments, not a change to what the §3 operation reaches.

### 6.2 Required tests

Selection unit tests: path equal to prefix; path shorter than prefix;
prefix segments deeper, reordered, or subset; equal local IDs under
sibling scopes; local key excluded from matching; constant-hash scope
values still selected structurally (RFC 0005's collision-safety
discipline — INV-2 and the Phase B constant-hash scope tests —
applied to selection).

Manager and runtime tests, through the production dispatch path:

- teardown of a prefix with one `Running` and one `Draining` entry
  aborts the first, discards the second's buffer, and delivers
  neither — asserted on both app-input paths, the blocking `poll_next`
  path and the micro-batch `try_next_ready` path (RFC 0003 §4.4);
- a teardown entry contributed by a batched child folds into the batch
  and applies, and `scoped` on the batch qualifies the folded prefix
  (§3.3, INV-ST2);
- a buffered keyed quit under the prefix does not quit;
- a same-command spawn under the torn-down prefix runs, under both
  `CancelInFlight` and `KeepInFlight`;
- teardown plus explicit cancel of a covered ID in one command behaves
  as teardown alone;
- zero-match teardown is a no-op; repeating a teardown changes
  nothing;
- entries under sibling scopes and a root-global slot are untouched
  through a teardown of one pane's prefix;
- a next-update spawn under the torn-down prefix starts fresh; a
  `TaskExit` bearing a torn-down run's token is inert — the test
  deterministically produces the torn-down run's task exit (not merely
  an aborted, never-exiting pending task) and processes it, at the
  successor's pre-spawn reconciliation (RFC 0003 §4.2) or a later
  poll, asserting the successor's run is untouched and still delivers,
  so the token comparison itself is exercised;
- an unkeyed task spawned by the torn-down child keeps running and its
  output is delivered (INV-ST8 (a));
- a shared-channel message from the child delivered concurrently with
  the teardown still reaches `update` (INV-ST8 (b));
- a batch that folded a child's spawn key yields no entry a later
  teardown can select (INV-ST8 (c));
- teardown in the final update before quit: RFC 0011 §4.4's immediate
  postcondition holds at `run()`'s return, and the quiescent
  postcondition is then reached — every runtime-owned task terminated
  and the producer gauges settled to zero under a bounded settle loop
  (RFC 0011 INV-LC7), never a fixed pass count (§5).

TestStore tests, through the store's command intake (RFC 0008
INV-T3): prefix selection over the pending set; strict drop of a
selected occupant's buffered output, a buffered keyed quit included;
teardown before the same command's spawn — the reinserted child's
leaf stays deliverable; unkeyed pending output unaffected. A store
that ignores teardown entries in the parts fails these.

### 6.3 Adversarial models considered

- *Superset canceller* — tearing down more than the selection passes
  any per-selection assertion; excluded by INV-ST6's sibling and
  root-global tests.
- *Segment-anywhere matcher* — passes prefix-equal cases; excluded by
  INV-ST1's reordered/deeper/subset tests.
- *Spawn-before-cancel implementation* — kills the same command's
  reinserted child; excluded by INV-ST3's same-command spawn test.
- *Drain-through implementation* — delivers a `Draining` entry's
  buffer before releasing it; excluded by INV-ST4's non-delivery
  assertion.
- *Generation-tracking implementation* — suppresses or taints later
  same-scope spawns; excluded by INV-ST7's fresh-start tests.
- *No-op implementation* — trivially idempotent; excluded because
  INV-ST5's idempotence is asserted only after INV-ST4's effects are.
- *Joint-satisfiability walk* — one implementation satisfying all
  eight simultaneously against today's seams: extend the lowered
  command parts' cancel set with prefix entries; at enqueue, reconcile
  (RFC 0003 §4.3), collect matching IDs by internal path comparison
  over the flat registry, apply the existing `Cancel` transition to
  each, then spawn. No new lifecycle state, no transition change, no
  prerequisite refactor for the command half. The parts type has a
  second consumer: TestStore's command intake (RFC 0008 INV-T3)
  applies the parts' cancel set to its pending output before the
  keyed spawn decision (`src/testing.rs`); the walk extends to it with
  prefix entries applied at the same point — the §6.2 store tests pin
  exactly this, so a store that drops the prefix entries on the floor
  is excluded.

### 6.4 Excluded claims

- A batch-folding invariant is not pinned here; the contract lands as
  the RFC 0003 INV-11 amendment (§7), with §6.2's fold test as its
  check.
- A dedicated teardown observability event is deliberately absent
  (R6); the entry-count gauge already reflects teardown.
- INV-ST6 is kept beside INV-ST1 rather than collapsed into it:
  INV-ST1 pins the selection *set*, INV-ST6 pins that non-selection
  implies non-effect — a side-effecting matcher satisfies the first
  while violating the second.
- A termination-precedence invariant is deliberately absent: RFC 0011
  §4.4 and RFC 0012 INV-SE5 already pin the property (§5); this RFC
  carries only the regression test.

## 7. Cross-document sync at acceptance

Accepted, this RFC lands the following edits, per the README's
in-place amendment rule:

- RFC 0003 INV-11 (one clause): teardown entries fold with explicit
  cancels; child spawn keys remain ignored.
- RFC 0005 INV-18 (one clause): `scoped` qualifies teardown prefixes
  present at the call boundary.
- RFC 0005 §4.5: the deferral text points here as its resolution.
- RFC 0008 §1.1 and INV-T7: the cancellation-metadata coverage and
  the cancellation-parity list extend to teardown entries — prefix
  selection, strict drop, same-command teardown-then-spawn ordering,
  and unkeyed non-effect hold over the store's pending output as this
  RFC's §3 states them for the runtime's deliverable output. INV-T3
  needs no text change — the store already consumes the shared parts
  type — but its structural review re-runs at the store's intake site
  once the parts carry teardown entries.

## 8. Open questions

1. **Public surface and owner.** Where the manual primitive lives
   (a `Command` constructor is RFC 0005 §4.5's sketch), and whether
   the composition machinery invokes the same surface or an internal
   one. Resolved by co-design with the composition RFC (RFC 0010 §5.2
   (a)/(e)). Implementation waits on this.
2. **Subscription participation and admission coupling.** §4.1's
   declarative baseline versus §4.2's immediate stop with its RFC 0012
   amendment; and, under either, whether the joint contract accepts
   the uniform barrier's availability coupling — a slow
   teardown-stopped task defers unrelated children's admissions
   runtime-wide (R5) — as documented negative space, or narrows the
   barrier's granularity (an RFC 0012 §4 amendment). Resolved by the
   same co-design; adopting §4.2 or narrowing the barrier requires the
   amendments named.
3. **Scope tree and unkeyed tracking (N30).** How §3.8 (a) is closed:
   by the runtime (first-class task-by-scope tracking), by the
   composition layer auto-keying child effects (moving them from the
   shared to the keyed delivery class — the cost axis RFC 0010 §3.1's
   full-unification rejection names: shared-first ordering, the
   default path, the liveness split — plus the keyed capacity and
   gauge surfaces, RFC 0006), or by the composition surface making
   unkeyed child effects unconstructible, so nothing leaks. Leaving
   the leak open — composed children able to spawn unkeyed effects no
   teardown reaches — fails R1 and is not an admissible resolution.
   Resolved with the core direction the composition RFC rests on.
4. **Shared-output retraction.** Whether delivered-side filtering of a
   torn-down scope's shared-channel output ever becomes contract
   (§4.3). Depends on the same direction; if never, the routing
   obligation of §4.3 stands permanently.

## 9. Alternatives considered

### Segment-anywhere selection

Matching `PaneId(7)` at any path position reaches `[TabId(1),
PaneId(7)]` and `[TabId(3), PaneId(7)]` alike, violating instance
isolation (R5, RFC 0005 INV-19's discipline) with no client that
needs it. Rejected for complete-prefix selection.

### Drain-through teardown

Delivering buffered output before releasing selected entries
contradicts the strict-cancel family (RFC 0003 INV-3/INV-4/INV-6) and
would deliver messages addressed to a child the state no longer
contains — the §4.3 orphan problem, extended deliberately to the one
delivery class that can avoid it. Rejected.

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
the declaration. Rejected; teardown stays explicit.

### Root cancel-all

An empty-prefix operation cancelling every keyed entry has no known
client and invites use as a shutdown mechanism, which RFC 0011 owns.
Excluded by construction (§3.1).

## 10. References

- RFC 0003: Command Cancellation — state machine, cancel phase,
  INV-1/3/4/6/8/9/10/11/14
- RFC 0005: Structural Lifecycle Identity — §2.3, §4.3–§4.6, INV-13,
  INV-14–INV-21
- RFC 0006: Runtime Load Control — §4.4 gauges, delivery classes
- RFC 0008: TestStore — §1.1 cancellation coverage, §1.2 exclusions
- RFC 0010: Runtime Consolidation — §3.1, §5.2, §5.3, §10.1 (N27,
  N30, N40)
- RFC 0011: Runtime Lifecycle — §2, §4.4, §4.5
- RFC 0012: Subscription Execution — §3, §4, §6.2, INV-SE2–INV-SE5
- `src/runtime/keyed_commands.rs`, `src/subscription.rs` — current
  registry shapes
