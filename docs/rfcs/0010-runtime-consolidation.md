# RFC 0010: Runtime Consolidation

- Status: Accepted (2026-08-05)
- Target: 0.11.0 — the consolidation bundle: this RFC is Accepted only
  together with RFC 0011, RFC 0012, and the blocking amendments to
  existing RFCs (§1.8)
- Scope: the consolidation audit's method and acceptance gate (§1), the
  consolidated reference architecture as informative evidence (§2), the
  per-root consolidation verdicts (§3–§7), the reaffirmation-ledger and
  additivity-audit snapshots (§8–§9), and the delegation register (§10)
- Feature flag: none
- CHANGELOG: none of its own — every behavior change decided under this
  consolidation rides its owner RFC's entry (RFC 0011, RFC 0012,
  RFC 0006, RFC 0007)

## Summary

The runtime's contract corpus grew one RFC at a time; this RFC is the
consolidation pass over all of it, against one goal image: **the final
architecture with features removed**. It fixes the audit method and the
gate its own acceptance must pass (§1), records the consolidated
reference architecture that the owner contracts jointly describe (§2,
informative), states the consolidation verdicts per root judgment with
every contract body left in its owner RFC (§3–§7), snapshots the
evidence — the replayed reaffirmation ledger and the additivity audit
(§8–§9) — and registers what is delegated onward (§10).

## 1. Scope and method

### 1.1 Goal, baseline, and reference contract

The audit's goal image is a runtime whose full form is *the final
architecture with features removed*: every present feature must read as
a projection of one architecture, and every audited future feature must
land on that architecture without replacing it.

- **Baseline**: `main` at commit `daa8bd1` — the reproducibility anchor
  every implementation-fact citation in this consolidation is defined
  on.
- **Reference contract**: the combination of three layers — they
  overlap, so each is defined separately rather than as disjoint
  sets —
  1. the **existing contract corpus**: the bodies of RFCs 0001–0009 as
     they stand on this branch;
  2. the **semantics-bearing Draft overlay**: RFC 0006, RFC 0007,
     RFC 0011, and RFC 0012 — the documents whose amendments or new
     contracts change semantics, and which carried Draft status
     through the audit for exactly that reason (transitioned together
     at the bundle acceptance, per §1.8); and
  3. the **semantics-neutral amendments** to Implemented documents,
     which keep their Status — each file's own Status header is the
     source of truth.

  Audit verdicts are measured against this combined corpus, not
  against the baseline implementation alone.

### 1.2 Normativity classes

Each section of this RFC carries a declared class:

- **§1** is normative *for this RFC's own acceptance*: the method and
  gate below are what a reviewer holds this document to.
- **§2** is informative: consolidated evidence, never a contract
  (§2's own preamble states the consequence).
- **§3–§7** are verdict chapters: the normative content is each
  verdict and its recorded conditions; the contract bodies they rest
  on are owned by RFC 0011, RFC 0012, and the existing RFCs, and are
  cross-referenced, never restated normatively here.
- **§8–§9** are **snapshot evidence**: dated records of the ledger
  replay and the additivity audit as they stood at the baseline plus
  the drift scans. They are evidence that the gate was met, not a
  contract about the future; nothing in them constrains later work
  except through the verdicts §3–§7 state and the two structure
  judgments §8.1 records.
- **§10** is the delegation register.

Whether a cross-document glossary of shared vocabulary is adopted, and
whether the reaffirmation ledger is kept alive after this consolidation
or frozen as a snapshot, are both decided inside §8.

### 1.3 Two additivity axes

Every audited fixture is judged on at least two axes. A future feature
that adds an opt-in API without breaking callers can still fail the
audit if implementing it would replace the core's ownership, state
machines, or task topology — that is precisely not "the final
architecture with features removed".

| Axis | Pass | Fail examples |
| --- | --- | --- |
| **Public-contract additivity** | The opt-in API lands while existing user code and every existing invariant hold | adding a bound to `Message`; changing a default ordering; adding a required trait method |
| **Architecture additivity** | A variant, policy, or adapter lands on the existing owners, state machines, and execution paths | replacing the channel topology; a second runtime; re-migrating the identity model |

"It is an internal change, therefore additive" is not a pass on the
architecture axis. The boundary in the other direction is **owner and
information-flow preservation**: adding a private field, a variant to a
non-public enum, or swapping the implementation behind an existing seam
does not fail the axis — otherwise the audit would demand implementing
features in advance, which §1.6 forbids.

### 1.4 Interaction audit

Single-fixture verdicts are not sufficient: features that each land can
collide in combination (retry with cancellation, priority with
shared-first pull, replay with nondeterministic I/O). Auditing every
combination is impossible, so the interaction suite prioritizes:

- pairs that press the same seam in opposite directions;
- pairs that span three or more of the architecture's spines; and
- pairs that involve failure or shutdown.

For each spine the suite carries at least one ordinary addition, one
opposite-direction pressure, and one combination with an existing
feature. Interaction rows are checked by joint-satisfiability walks,
not by per-feature argument alone.

### 1.5 Removal projection

Additivity is also audited in the removal direction: the completed form
minus a feature must still be the small core. The projection applies to
removing **surfaces whose removal the audit admits** — a surface the
audit rules an unconditional constituent (or whose removal it excludes
by an `X` verdict) is outside the
projection's quantifier, and the exclusion is recorded on the audit
row, not assumed.

- Disabling sources, backends, or observability whose removal the
  audit admits must leave the same core owners and event topology.
- An unkeyed command must be the *same* execution path without identity
  and cancellation policy — not a second runtime.
- The TEA facade must be an adapter over a single-feature reducer — not
  a second core.
- A headless or minimal build, where the audit admits one, must be the
  feature-removed projection of the same phase machine — not a bundle
  of placeholder implementations and no-op branches.

If removing a feature reveals a different state machine, the feature's
presence is encoded as an architecture fork, and the fixture is a fail
candidate on the architecture axis.

### 1.6 Headroom discipline

Cataloguing a future feature is never grounds for adding its traits or
fields now. The audit checks headroom, not reserved seats:

- no information is discarded irreversibly;
- no public contract locks an owner in place that a future feature
  would need to move;
- policy-addition points are single and their defaults express the
  small core; and
- an explicitly rejected future actually buys its simplification.

If a fixture can pass only with an unused abstraction added today, its
verdict is not "lands additively" but "requires implementing part of
the feature now" — and it is recorded that way.

### 1.7 Verdict vocabulary and audit depth

The audit's population has three row kinds, each with a closed terminal
vocabulary:

- **Feature fixtures** — verdicts `A` / `C` / `B` / `X`:
  - **A — additive**: lands on both §1.3 axes.
  - **C — conditional**: lands provided a named seam or owner decision
    holds; the condition is recorded on the row.
  - **B — breaking / re-architecture**: requires changing an existing
    contract or the core topology.
  - **X — intentionally excluded**: an anti-catalog decision; the
    feature is deliberately not supported.
- **Interaction rows** — verdicts `pass` / `fail` / `excluded`. An
  interaction row may terminate as `excluded` only when a constituent
  fixture's `X` (or an anti-catalog decision's `adopted(X)`) is
  settled and the joint question thereby loses its subject; the row
  records `excluded_by=<fixture id>` — a single representative id,
  with further affected fixtures in the row's note when one decision
  spans several. Remaining-leg rule: when the interaction among the
  row's surviving constituents still has independent audit value, a
  new interaction row is filed append-only, or the surviving fixture
  row is sharpened to carry it — an exclusion never silently discards
  a live question.
- **Anti-catalog decisions** — verdicts `adopted(X)` / `rejected` /
  `delegated`.

`unknown` is never rounded into a terminal verdict; it is resolved.
Merged rows record their consolidation target (`resolved_via`) and
still carry their own terminal verdict — the target's verdict when the
target is a fixture, the derived verdict when it is a design decision.

Depth is deliberately uneven, so the audit both considers much and
converges: every row gets a one-line verdict; seam-distinct
representatives and every `B`/`C` candidate get an execution-path
sketch; the interaction suite gets joint-satisfiability walks.

### 1.8 Acceptance gate

`U = 0` alone would let the audit terminate while failed; this RFC is
Accepted only when all of the following hold:

- **`U = 0`, terminally.** Every row — excluded rows included — carries
  a terminal verdict from its kind's vocabulary (§1.7), with no
  provisional values (`U`, `blocked_by`) remaining. A feature row that
  is excluded must carry `X`; interaction `fail` count is zero; merged
  rows carry `resolved_via` plus their own terminal verdict.
- **No internal `C`.** No conditional verdict waits on a root judgment
  this consolidation itself decides. The only `C`s that may remain are
  delegations to external RFCs, and each records its owning task and
  two flags, defined separately: **`blocks composition`** — `yes`
  means the delegated contract document must be Accepted together with
  this RFC in the bundle below (the register currently marks none
  `yes`); and **`gates the composition RFC`** — `yes` means the
  delegated document must precede or accompany the *future composition
  RFC*: a scheduling constraint on that RFC, never a condition on this
  RFC's acceptance.
- **No unprocessed `B`.** Every in-scope `B` is dispatched one of three
  ways: (a) the affected root judgment is reopened; (b) the row is
  demoted to `X` with the reason and the simplification actually
  gained; or (c) it is delegated with its cost entered against a
  breaking budget.
- **`B`'s criterion** is "breaks the *post-consolidation* contract".
  Changes already carried on the 0.11.0 breaking budget (the
  composition work itself included) are baseline, not `B`.
- **Bundle acceptance.** This RFC is Accepted only together with the
  bundle its Target names — RFC 0011, RFC 0012, and the blocking
  amendments (the RFC 0006 and RFC 0007 Draft overlays) — and with any
  delegation's contract document marked `blocks composition = yes`
  (none at present, so the bundle is exactly the Target's list). If an
  amendment's semantics change during the bundle review, the change is
  a counterexample and returns to the audit before acceptance.

### 1.9 Counterexample grades and the reopen rule

A settled root judgment is reopened only by a counterexample of one of
four grades:

- **(i)** public-contract additivity fail — an existing contract or
  existing user code breaks;
- **(ii)** owner/topology break — an architecture-axis fail (§1.3);
- **(iii)** removal-projection fail (§1.5);
- **(iv)** a `B` verdict on an in-scope fixture.

Ergonomic and aesthetic concerns are recorded but never reopen a
judgment. Every return to a root judgment is journaled in one line —
which decision, changed from what to what, by which counterexample —
and a return that changes no decision is forbidden (no polish loops).
**Whatever its grade, a counterexample reopens only the §3–§7 root
judgments it affects** — any of them, the identity/composition and
subscription-execution roots included, and no others. Illustrations,
not a closed list: a policy-only counterexample reopens the
scheduling-policy root alone; a counterexample reaching the event
loop's phase selection or termination ordering is the lifecycle
root's; a root joins a reopen only when the counterexample affects its
own decisions. This keeps returns from over-widening.

### 1.10 Drift gates

Because the audit measures against the combined document corpus at a
fixed baseline (§1.1), two drift gates bracket this RFC — before
drafting and before the bundle acceptance — and each gate has two
parts:

1. **Baseline code-delta scan.** Changes on the upstream `main`
   branch since baseline `daa8bd1` to `docs/rfcs`, `src/runtime`,
   `src/command`, and `src/subscription` are scanned; the fixtures and
   ledger rows those changes affect are appended to the audit
   (append-only — no renumbering), and only the affected root
   judgments and audit rows are re-run. This bundle's own branch
   diffs are not this scan's subject: they enter the audit through the
   ledger replay itself.
2. **Claim scan.** Every document claim is checked against the code it
   describes — pre-implementation present tense, future forms for
   landed work, superseded shapes cited as current, and absolute
   claims later scoped by other documents.

The pre-drafting gate completed 2026-07-30/31. Part 1 scanned upstream
`main` over `daa8bd1..89ea872` and found the
baseline advance had touched only the dependency lockfile — zero
contact with the scanned paths, so no audit rows required re-running
from the code delta. Part 2 swept all eleven RFCs, the RFC index, and
the code documentation they cite (the `RuntimeConfig` rustdoc, the
load-harness comments), and its corrections ran to closure in three
classes:

- **document retensing** — completed-state and superseded-shape drift
  fixed in place per document, with residual instances surfaced by an
  independent two-reviewer cold read and swept section-whole;
- **implementation conformance** — the two identifier allocators made
  fail-before-reuse where document contracts pin non-reuse, and the
  two previously missing of the three guidance notes RFC 0007 §3.3
  records as landed added to the `RuntimeConfig` rustdoc; and
- **numeric-claim precision** — the constructor panic documentation
  freed of an off-by-one allocation count, and RFC 0006's wraparound
  absolute restated as a consequence of per-instance strict increase
  and then twice sharpened (same-instance scope; the equal-`seq`
  case).

The correction endpoint is commit `ab313b2`; at that endpoint the
claim scan records zero remaining findings. The pre-acceptance gate
ran **both parts** again over the final bundle (below), per §1.8.

The pre-acceptance gate completed 2026-08-05. Part 1 scanned upstream
`main` over `89ea872..5c05011` and found only another dependency
lockfile advance — zero contact with the scanned paths, so no audit
rows required re-running. Part 2 re-swept the same subject
vocabulary over the final bundle text and found zero new findings:
the status lines carry §1.1's Draft-overlay scheme, no completed-state
or superseded-shape claim has reappeared, and the code-state claims
verified at `ab313b2` still describe the unchanged code. Both drift
gates are closed: the code-side endpoint is upstream `5c05011`, and
the part-2 subject text is the bundle as of commit `a6ce2d1` (this
paragraph's anchor is recorded one commit later, as `ab313b2`'s was).

## 2. Reference architecture (informative)

**Nothing in this section is contract.** It is the consolidated sketch
the owner contracts jointly describe — evidence that they are
satisfiable by one architecture — and every normative statement it
summarizes lives in the RFC cited beside it. A conforming
reimplementation is measured against those RFCs, never against this
sketch.

### 2.1 Phase machine

```text
[construct]   Runtime::new / with_config
  App::new(flags) runs (user code, outside the lifecycle contract)
  -> (app, init command); construction is inert: no runtime-owned
  task, no effect poll, no source start        (RFC 0011 §3, INV-LC3)
      |
      v  run()
[bootstrap]                                     (RFC 0011 §3, INV-LC4)
  1. init command dispatched (before subscriptions)
  2. initial subscription reconcile
  3. first render made pending — eligible, not promised
  arbitration among init output, first subscription output, and the
  first render: unpinned
      |
      v
[steady loop]  select! over three branches (unbiased — premise of
               RFC 0006 INV-L4)
  A. input batch: first input -> update -> dispatch, one item at a
     time (RFC 0003 INV-10), within the 100 microsecond window
     (RFC 0003 §4.4) and the optional count cap (RFC 0006 INV-L12);
     redraw directives OR-fold into pending work (RFC 0002); a batch
     that ran update marks subscriptions dirty (RFC 0003 §4.4)
  B. frame pass (current state; RFC 0011 INV-LC1/INV-LC2): render if
     redraw pending, then — after a successful render; a render error
     terminates instead (RFC 0011 §2.2, §4.1) — subscription re-evaluation
     if dirty; both steps observe the same state; no individual state
     is promised a render
  C. dedicated unkeyed-quit branch (never-bounded channel —
     RFC 0006 R4/INV-L4)
  input delivery: one shared FIFO (subscription output, unkeyed
  command output, terminal events) plus per-run keyed private FIFOs;
  every pull point is shared-first (RFC 0003 INV-14)
  subscription dirtiness has two sources: a batch that ran update,
  and the quiescence of a task stopped by a steady-state
  re-evaluation — reaching an idle driver as a wake-capable input
  (RFC 0011 §2.1, RFC 0012 §4); termination-driven quiescence marks
  nothing
      |
      v
[termination]                                          (RFC 0011 §4)
  controlled: unkeyed quit / keyed quit / render error -> loop exit
    with reason; shutdown routing vs. value drop is mechanism
    (RFC 0011 §4.2's no-divergence analysis)
  abrupt: run-future drop / panic in update, view, subscriptions, or
    a lazy source constructor / never-run drop -> Drop-chain
    teardown; synchronous half = ownership teardown + cancellation
    requests only
  postconditions, both routes (INV-LC5–INV-LC7):
    immediate  = no further transitions or delivery; cancellation
                 requested for every runtime-owned task
    quiescent  = after the executor processes the cancellations:
                 all tasks terminated, producer gauges read zero
```

### 2.2 Owner table

Consolidated direction for the execution model: **one execution
mechanism, two delivery classes** — unkeyed and keyed commands share
one spawn path, one task-ownership set, and one exit-reap path,
differing in identity (none vs. `CommandId` + policy) and output
routing (shared FIFO vs. private channel). Per-id lifecycle entries
exist for keyed runs only: an anonymous (unkeyed) task holds no
per-id entry and is reaped exit-only from the shared task set. Rows
marked *mechanism* are informative implementation shape, free to
change under their cited contracts.

| Resource | Owner at baseline | Consolidated | Contract owner |
| --- | --- | --- | --- |
| app state / `update` / `view` / `subscriptions` | `RuntimeCore.app` | unchanged | phases: RFC 0011; `subscriptions()` purity: RFC 0012 INV-SE6 |
| shared message channel | `msg_tx` / `AppInputs.shared` | unchanged (delivery class 1) | RFC 0006 (capacity, losslessness); RFC 0003 INV-14 (pull order) |
| dedicated quit channel | `quit_tx`/`quit_rx`, plain unbounded | unchanged (never bounded) | RFC 0006 R4/INV-L4 |
| keyed private channels | one per `KeyedCommands` entry | unchanged (delivery class 2) | RFC 0003; RFC 0006 INV-L9/INV-L10 |
| unkeyed command task | `command_tasks: JoinSet<()>` | shared spawn path and typed-exit task set; anonymous tasks are exit-only — no per-id entry (*mechanism*) | behavior unchanged under RFC 0003 INV-1 |
| keyed task + lifecycle FSM | `KeyedCommands` (map + task set + run tokens) | single authoritative structure, O(1) lookup; no double bookkeeping (*mechanism*) | RFC 0003 INV-2–INV-16, unchanged |
| subscription forwarder | `SubscriptionManager.running` | unified task-policy wrapper; reconcile algorithm unchanged, admission per the quiescence barrier | RFC 0005 INV-8–INV-13; re-evaluation phase: RFC 0011; admission: RFC 0012 §4 |
| task body policy (panic capture, send handling, quit translation) | duplicated across three task kinds | the *definition site* is unified into a single owner module; the item vocabulary (the stream's output type), sink shape, quit translation, panic-log form, and completion reporting stay per kind (*mechanism*) | panic containment: RFC 0011 INV-LC8; keyed-panic log occurrence: RFC 0003 §7.3; send handling: RFC 0006 §4.3; quit translation: unkeyed — RFC 0006 R4/INV-L4, keyed — RFC 0003 INV-9 / RFC 0006 INV-L10/INV-L11 |
| frame ownership | `FrameScheduler` + `PendingWork` + runtime | unchanged; parking premise informative | RFC 0011 INV-LC1/INV-LC2 and §7's premises |
| gauges / load events | `LoadObserver` funnel; guard-based and count-based gauges | the gauge-transcript-identity gate passed (2026-07-28), so the **keyed** gauge moves to an entry-owned guard held by the keyed entry; the subscription, unkeyed-command, and blocked gauges keep their task-held guards (*mechanism*) | RFC 0006 INV-L13 (schema, `runtime_id`/`seq`) either way |
| time | `tokio::time`, single axis | unchanged | RFC 0009 |
| identity | `StructuralKey` / `ScopePath` | unchanged | RFC 0005 |

### 2.3 Removal projection

The §1.5 checks close on this architecture, within §1.5's own scope
rule — the minimal-profile leg is outside the projection's subject by
the audit's exclusion verdict: the observability layer, the test
surface, and the terminal driving surface are unconditional
constituents, so no minimal profile is offered and none is audited for
removability.

- **Unkeyed = the same spawn, ownership, and reap path minus
  identity.** The unified execution model runs an unkeyed command
  through the same spawn path, task-ownership set, and exit-reap path
  as a keyed one, with no identity, no cancellation policy, and no
  per-id lifecycle entry — no second runtime exists to remove.
- **Feature removal is degeneration, not forking.** Bounded mode off,
  keyed commands unused, or no subscriptions declared each degrade the
  same phase machine — channels unbounded, the per-id entry set empty
  with only exit-only anonymous tasks, reconciles empty — and no
  alternative state machine appears.
- **Composition shares the phase machine.** RFC 0011 states the phase
  machine over the `Application` boundary (`new`/`update`/`view`/
  `subscriptions`); a composition core implements that boundary as a
  single aggregate `Application` adapter rather than a second runtime,
  so the projection closes in the composition direction too — the
  reason the composition RFC is gated on this bundle.

### 2.4 Ledger replay summary (dated snapshot)

The reaffirmation ledger — every invariant, unnumbered normative
clause, negative-space statement, and implicit contract extracted from
RFCs 0001–0009 and from the runtime's previously unnumbered implicit
contracts, 531 rows — was replayed row by row against §1.1's
reference contract. As of 2026-07-31:

- **Reaffirmed 522 / redesigned 9 / delegated 0 / open counterexamples
  0.** Redesigned rows are exactly those whose semantics the bundle
  changes, each recording its destination (the bootstrap
  construction-dispatch change and termination model, RFC 0011; the
  admission-scope rescope of load control's INV-L8 and the gauge-schema
  `runtime_id` addition, RFC 0006; the `Copy`-derive removal,
  RFC 0007); reaffirmed rows record their preservation or
  canonicalization site.
- **Two counterexamples arose during consolidation and are resolved**,
  each with its one-line journal entry per §1.9: a grade-(ii) joint
  unsatisfiability between the supersession and admission-point rules
  and the lifecycle phase contract — a counterexample reaching phase
  selection (the dirty-source set), so per §1.9 it reopened the
  lifecycle root alone — resolved by unifying *deferred re-admission*
  on the next frame-pass re-evaluation (RFC 0012 §4 with RFC 0011
  §2.1's second dirty source), the bootstrap reconcile's immediate
  admissions kept; and a grade-(i) wording contradiction between
  load control's producer-admission invariant and the quiescence
  barrier — policy-only, so it reopened the scheduling-policy root
  alone — resolved by rescoping RFC 0006 INV-L8 to load-control
  non-interference. Neither reopened beyond its affected roots
  (§1.9's rule).

The full row-level snapshot, with verdicts and grounds, is §8's
content; this summary is the tally the §1.8 gate is measured on.

## 3. Execution-model consolidation

Owns the root judgments: `root-A1` (the execution model) and
`root-SCHED` (the scheduling policy). Each verdict chapter names the
root judgments it owns, so §1.9's membership test closes mechanically
over §3–§7's lists.

### 3.1 `root-A1` — verdict: redesign of the mechanism, reaffirmation of the delivery contract

The unkeyed and keyed command paths are unified into **one execution
mechanism with two preserved delivery classes**. Full unification —
making unkeyed an anonymous-key keyed command, private channel
included — was rejected: unkeyed output leaving the shared FIFO would
fall behind INV-14's shared-first pull, breaking RFC 0003 INV-1 (the
default path is unchanged) and RFC 0006's liveness split
(liveness-critical output belongs in unkeyed commands), and
shared-first is itself the physical cancel-before-delivery mechanism
(RFC 0006 §4.7). What unifies instead is everything that was
meaningless duplication: one spawn path branching only on the presence
of identity (`CommandId` + policy) — the §2.2 shape: one
task-ownership set with typed exits, one exit-reap path, per-id
lifecycle entries for keyed runs only with anonymous tasks exit-only,
O(1) bookkeeping under a single authoritative owner (no double
bookkeeping), the task-body policy's *definition site* in one owner
module with item vocabulary (the stream's output type), sink shape,
quit translation, panic-log form, and completion reporting per
kind, and the keyed gauge entry-owned after its transcript-identity
gate passed (§2.2). All of this is mechanism — informative here and in
§2 — and a staged spike of all four stages on a separate spike branch
demonstrated feasibility with every contract suite green and no
contract test rewritten (no grade-(i) counterexample); the spike's
adoption is decided together with this amendment bundle, not assumed
by it. Per-leaf
provenance headroom (a leaf-metadata seam) is recorded as future
room, deliberately unimplemented (§1.6).

**Conditions.** The send-on-closed-channel rule is normalized to
break-on-close for every task kind: for a bounded-mode sender blocked
in `send`, that is a conformance fix owed to RFC 0006 §4.3's existing
requirement; for the immediate- and unbounded-close observations it is
a uniform policy choice inside non-guaranteed territory, recorded as
such in the terminal-matrix tests rather than as owner contract.

**Contract impact.** None on the contract documents for the mechanism
itself: the behavior contracts ride RFC 0003 (INV-1, INV-2–INV-16)
and RFC 0006 (the two delivery classes, INV-L9/INV-L10) unchanged, and
the ledger replay reaffirmed every affected row (§2.4).

**Reopen targets.** A counterexample of any §1.9 grade that affects
`root-A1`'s decisions reopens it — a delivery-compatibility break
(grade (i)) or a removal-projection fail (grade (iii)) can reach this
root as readily as an owner/topology break (grade (ii)); none has
occurred.

**Return #3 (2026-08-13)**: a grade-(ii) owner/topology
counterexample — the architecture-selection comparison for the
composition core (decided by RFC 0014) selected a kernel whose
delivery topology replaces this root's preserved shape — reopened
`root-A1`: "one execution mechanism with two preserved delivery
classes" changes to one origin-tagged data lane with revocation
filtering at the delivery decision point, an `update`-returned quit
applied synchronously at its dispatch, and producer-originated quits
on a dedicated control lane; the superseding contract, with its
per-invariant successor correspondence for RFC 0003 and RFC 0006, is
RFC 0014 (§3.1, §3.3, §9), accepted at its staged spike's tier: the
owner-document edits that correspondence names are landed, and
implementation mainlining stays gated there (§13.1).

### 3.2 `root-SCHED` — verdict: reaffirmation, canonicalization, and two numberings

Shared-first pull is reaffirmed with **RFC 0003 INV-14 as its sole
canonical statement** — RFC 0006 INV-L11 is its keyed-quit
application, and RFC 0008's ordering text maps it under that RFC's
citation rule. The fairness question stays resolved against any
policy (RFC 0006 §4.7), reaffirmed. Bounded delivery mode stays
non-default, reaffirmed — RFC 0006 §3.1's position, with any future
default flip remaining that RFC's deliberate later decision, not
judged here. Two existing behaviors are
numbered without semantic change: bounded mode's narrowed cancellation
immediacy (RFC 0006 INV-L14) and the traffic-class negative space
(RFC 0006 INV-L15), the latter with internal class metadata explicitly
mechanism — pinned in neither direction, compatible with the
leaf-metadata headroom above. After return #2 (below), load control's
admission invariant is rescoped to non-interference (RFC 0006 INV-L8):
admissibility and admission timing belong to their owners.

**Conditions.** None remaining; reopening fairness is a new RFC
amending RFC 0003 INV-14 (RFC 0006 §4.7's reopening rule).

**Contract impact.** Carried by the RFC 0006 Draft overlay (§1.1):
the INV-L14/INV-L15 numberings and the INV-L8 rescope. The RFC 0003
cross-reference sync is semantics-neutral.

**Reopen targets.** Policy-only counterexamples reopen `root-SCHED`
alone. **Practiced return #2**: a grade-(i) wording contradiction —
the unscoped "never blocks, rejects, or defers producer admission"
against RFC 0012 §4's quiescence barrier — reopened `root-SCHED`
alone (policy-only; no owner/topology break), resolved by the INV-L8
rescope with the §4.5 derivations and enforcement text synced, and
closed. Recorded in the §1.9 journal.

**Return #4 (2026-08-13)**: the same architecture-selection
counterexample as `root-A1`'s return, grade (ii) with a grade-(i)
face — the selected kernel's single FIFO with dequeue-time
revocation filtering replaces shared-first pull's topology, and with
it INV-14's canonical statement, its keyed-quit application INV-L11,
and configured frame pacing (the RFC 0006 frame-branch facts,
RFC 0007 INV-C5, and the frame-rate configuration and constructor
surface) do not survive — reopened `root-SCHED`: the reaffirmation
and canonicalization of shared-first pull change to a supersession
that keeps starvation freedom and flood-proof rendering, records the
broad cancel-before-delivery opportunity as a deliberate property
loss, and replaces configured pacing with pass-bounded render
cadence; fairness stays resolved against any policy and bounded
delivery stays non-default under the successor; the superseding
contract is RFC 0014 (§3.1, §3.2, §3.5, §6.3, §9), accepted at its
staged spike's tier, with its owner-document edits landed and
implementation mainlining still gated there (§13.1).

The same counterexample carries one more grade-(i) face on this root:
the private keyed channels go with shared-first pull, and per-command
backpressure isolation goes with them — RFC 0006 INV-L9, the term
RFC 0006 INV-L1 sums per command, and RFC 0007's
`keyed_channel_capacity` surface. This root's ledger row for that
isolation, LG-0006-058, reads as reaffirmed; under the successor it is
superseded instead, since every producer awaiting capacity awaits the
one data lane's — one producer's occupancy couples capacity admission
for all producers, where the per-command channels coupled none. The
ledger stays the snapshot §8.1 adopts, unamended; this entry is where
the correction lives, and the superseding contract is RFC 0014 (§3.1,
§9, whose row for the delivery-class supersession carries it), landed
in the owner documents on that RFC's acceptance and gated there for
mainlining (§13.1).

**Return #8 (2026-08-16)**: one counterexample, returned on each root
it reaches — here, on `root-CMP` as return #9 (§5.3), and on
`root-D18` as return #10 (§6.2) — and correcting audit rows rather
than root judgments: no verdict of §3–§7 changes on any of the three,
and each entry says what it does change. Here it is LG-0006-047, the
documentation guidance this root owns and the ledger records as
reaffirmed: keying buys cancellability
at the cost of delivery deferral under load, so liveness-critical
output routes to unkeyed commands. Neither half survives the successor
this root's own return already records — one data lane defers no keyed
output behind a class of its own, and an unkeyed command buys no
priority to route toward — so that guidance is superseded together
with the delivery classes that carried it, and its cancellability
half survives only in the form RFC 0014 §3.3 states. RFC 0006 §5.2
carries the owner-side correspondence. The ledger stays the snapshot
§8.1 adopts, unamended; this entry is where the correction lives.

## 4. Lifecycle and termination

Owns the root judgment: `root-K51J46` (the lifecycle root).

### 4.1 Verdict: redesign — a new owner contract for the fourth spine

The lifecycle phase machine and termination model — previously implicit
in every RFC and owned by none — are redesigned into an owner
contract, **RFC 0011**, which this chapter records but does not
restate: steady-state phase order (frame-granularity rendering and
re-evaluation on the pass's current state — with a redraw pending the
render precedes re-evaluation, with none pending re-evaluation
proceeds with no preceding render: RFC 0011 §2.2's conditional form); the bootstrap contract with **inert construction** —
the constructor spawns no runtime-owned task, polls no effect, starts
no source, a 0.11.0 behavior change — and the in-`run()` order with
first-render eligibility rather than a render promise; subscription
dirtiness with two sources (a batch that ran `update`; the quiescence
of a steady-state-stopped task, reaching an idle driver as a
wake-capable input — the shape return #1 settled); termination as two
routes (controlled and abrupt) converging on two-stage postconditions
(immediate; quiescent); panic containment for runtime-owned tasks with
application code fail-fast; and driver exclusivity as a property.
Premises other RFCs' invariants rest on are recorded informatively in
RFC 0011 §7, and the normative content is restricted to observables.

**Conditions.** RFC 0011's two open questions — panic-reason
preservation and controlled-route quiescence — resolve at
implementation design in that RFC's body; both are resolutions its
invariants already hold under. Graceful drain is pinned only as the
zero-grace degenerate form; its substance is future work outside this
bundle (blocks composition: no).

**Contract impact.** RFC 0011 (Draft overlay) owns the contract body
and the construction-dispatch `Changed` entry; the second dirty
source's behavior change is carried by RFC 0012's `Changed` entry
(RFC 0011's header says so); the RFC 0003 §4.4 cross-reference and the
RFC 0008 §5.2 first-frame wording sync are semantics-neutral.

**Reopen targets.** Counterexamples reaching the event loop's phase
selection or termination ordering reopen `root-K51J46`. **Practiced
return #1**: a grade-(ii) joint unsatisfiability — supersession
(INV-SE4) × the admission-point rule (INV-SE5) × batch-only dirtiness
(INV-LC1) admitted no implementation — reached phase selection (the
dirty-source set), so it reopened `root-K51J46` alone, narrowly
scoped to that set; resolved by unifying deferred re-admission on the
next frame-pass re-evaluation and adding the quiescence dirty source
(RFC 0011 §2.1, RFC 0012 §4), and closed. Recorded in the §1.9
journal.

**Return #5 (2026-08-13)**: a grade-(i) counterexample from the
architecture-selection outcome (RFC 0014), narrowed by that RFC's
facade decision — `Runtime<App>`, its `run` signature, and the
result contract (either quit form returns `Ok(())`, a render error
returns `Err`) are preserved at the existing entry point, so
RFC 0011 INV-LC5's classification continues to hold there and the
reopen does not touch the run/result surface (the constructor change
rides the pacing supersession, `root-SCHED`'s return in §3.2) —
reopened `root-K51J46` for the bootstrap contract alone: an init
command whose `Command::quit()` part is present changes from one
legal outcome of bootstrap arbitration to deterministic synchronous
termination during the init dispatch, before the initial reconcile
and before any source starts — a change to RFC 0011 §3.2's intake
order; the added advanced entry point (`ProgramRuntime` with its
`Exit` result) is additive surface beside the preserved one,
recorded as no supersession; the amending contract is RFC 0014
(§2.3, §2.4, §6.2, §9), accepted at its staged spike's tier, with its
owner-document edits landed and implementation mainlining still gated
there (§13.1).

## 5. Identity and the composition axiom

Owns the root judgment: `root-CMP` (the identity/composition root).

### 5.1 Verdict: axiom adopted

**The composition layer owns the identity boundary.** The concrete
shape: a future composition core implements the existing `Application`
boundary as a **single aggregate `Application` adapter** — no second
runtime, no second identity model — sharing RFC 0011's phase machine
(§2.3's projection). Identity contract bodies stay in RFC 0005; the
axiom is this RFC's own verdict and creates no change to any current
contract document.

### 5.2 Requirements handed to the composition RFC

The composition RFC must satisfy, and is reviewed against, this list
(the audit's `C-15` register), stated here self-contained:

- **(a) Automatic scope application.** Scopes are applied to child
  instances by the composition machinery itself, structurally removing
  the manual-scoping footguns (a child's IDs unscoped or doubly
  scoped by hand).
- **(b) Identity-law preservation.** RFC 0005's scope laws
  (INV-14–INV-21) hold through the adapter unchanged.
- **(c) TestStore reuse.** The adapter is testable by the existing
  store (RFC 0008 §1.2's expectation) — no second harness.
- **(d) Phase-machine sharing.** The adapter runs under RFC 0011's
  phase machine; it introduces no second lifecycle.
- **(e) `cancel_scope` precedence.** A `cancel_scope` design (the
  RFC 0005 §4.5 questions) precedes or accompanies the composition
  RFC. RFC 0005 already records this relation — before or together
  with collection (`forEach`-style) composition — and this
  consolidation generalizes its object from that concrete client to
  the composition RFC as a whole.
- **(f) Quiescence-barrier non-interference.** The adapter aggregates
  child declarations into one desired set and neither observes nor
  awaits quiescence (RFC 0012 §4.4); a composition design that needs
  quiescence observation is a change to RFC 0012's contract and
  returns here as a counterexample.

### 5.3 Reaffirmation, delegation, contract impact, reopen targets

**Reaffirmed**: the public identity types stay non-unified —
`CommandId` and `SubscriptionId` remain distinct public types
(RFC 0005 INV-7). **Delegated**: `cancel_scope` (the audit's `C-16`
register) — owned by its own future RFC; **blocks composition: no**
(this consolidation only sets its direction, so the document does not
join this RFC's bundle);
**gates the composition RFC: yes** (it must precede or accompany that
RFC per (e) — a scheduling constraint on the composition RFC, §1.8's
second flag). **Contract impact**: none now; implementation lands in
the future composition and `cancel_scope` RFCs. **Reopen targets**:
counterexamples breaking the identity model or requiring its
re-migration reopen `root-CMP`; none has occurred.

**Return #6 (2026-08-13)**: a grade-(ii) owner/topology
counterexample — the architecture-selection comparison this axiom
handed to the composition work was run, and the Application-centric
shape lost it — reopened `root-CMP`: the concrete shape changes from
a composition core implementing the existing `Application` boundary
as a single aggregate `Application` adapter to a reducer-first core
— a `Program`/`Reducer` protocol over a runtime kernel — with
`Application` preserved as its single-feature adapter; the axiom's
substance survives in the successor (the composition layer owns the
identity boundary; no second runtime, no second identity model), and
the §5.2 requirements are discharged there item by item; the
superseding contract is RFC 0014 (§2, §8), accepted at its staged
spike's tier, with its owner-document edits landed and implementation
mainlining still gated there (§13.1).

**Return #9 (2026-08-16)**: the same counterexample as return #8
(§3.2), on the root that owns the cleanup-hook seam. What changes here
is the condition text N27 carries for that seam, not this root's
verdict: N27 describes a
cleanup window that defers admission through the subscription barrier
and is observable through the producer gauges, and the successor
denies both — a cleanup run neither joins the barrier nor triggers it
(RFC 0014 §5.1), and no gauge field counts one (RFC 0006 §5.2). The
axiom's successor keeps cleanup registration inside the composition
boundary, qualified like every other identity-bearing carrier
(RFC 0014 §4.4), so what the correction leaves open is an overlap
rather than a leak: a cleanup run for a torn-down instance may still
be running while that scope's successor instance starts. That overlap
is accepted negative space here, not a pinned exclusion; a design that
needs non-overlap states it as a condition on the graceful-drain and
cleanup-ordering work, where §10.1 already seats the drain slot. The
ledger stays the snapshot §8.1 adopts, unamended; this entry is where
the correction lives.

## 6. Effect and subscription execution boundary

Owns the root judgments: `root-B7` (the directive/effect axis
placement) and `root-D18` (subscription execution and the effect-DI
boundary).

### 6.1 `root-B7` — verdict: reaffirmation and canonicalization of the axis split

The Axis A/B scope split — output treatment as passive folded
directives vs. execution lifecycle on the effect — is reaffirmed with
**RFC 0002 §9 as its canonical statement**, and Axis B's settled
composition is canonicalized: `timeout`/`retry` are per-effect
effect/stream transformations (RFC 0004), and cancellation is command
lifecycle metadata lowered by the runtime (RFC 0003) — settled without
any new internal action variant, so the closedness position is
reaffirmed (new directives go to the directives side, per RFC 0002's
frame; the audit's `B-11` register). The `Action`-privatization
ordering constraint was met by completion.

**Conditions and delegations.** Axis A's terminal home (modifier vs. a
future update-outcome type; the audit's `B-9` register) and the
quit-directive surface question (`B-8`) are delegated to the
composition RFC under explicit preservation conditions: unkeyed
quit's backlog-independent dedicated-channel delivery (RFC 0006
R4/INV-L4), keyed quit's in-band cancellable
delivery (RFC 0003 INV-9; RFC 0006 INV-L10/INV-L11), and the
cancellation-metadata lowering path — no representation choice may
weaken any of the three. A re-evaluation-policy directive is delegated
to a future RFC 0002-extension RFC (the audit's `B-10` register;
blocks composition: no).

**Contract impact.** Semantics-neutral only: RFC 0002 §9's sync to the
settled forms. **Reopen targets**: axis-placement counterexamples
reopen `root-B7`; none has occurred.

**Return #7 (2026-08-13)**: a grade-(i) counterexample — the
selected kernel (RFC 0014) carries a producer-originated quit on a
control lane drained before each pass's input batch, which keeps
backlog independence and cancellability until application (through
origin revocation) but not RFC 0006 INV-L10's ordering property: a
keyed run's quit no longer waits behind the same run's earlier
output — reopened `root-B7`'s preservation conditions on the
quit-directive question: of the three, unkeyed quit's
backlog-independent delivery is met in strengthened form
(synchronous application at the returning dispatch) and the
cancellation-metadata lowering path holds, while keyed quit's
in-band ordering half is superseded with the loss recorded as
breaking; the successor statement is RFC 0014 (§3.3, §9), accepted at
its staged spike's tier, with its owner-document edits landed and
implementation mainlining still gated there (§13.1).

### 6.2 `root-D18` — verdict: redesign — a new owner contract for source execution

Subscription execution — the third piece of the Timer contract split
(identity RFC 0005, timing RFC 0009) — is redesigned into an owner
contract, **RFC 0012**: the source execution template (effect-free
declaration, spawner-at-admission start, forwarder-paced polling), the
three-boundary stop vocabulary with the uniform quiescence barrier and
its admission rules, `subscriptions()` purity as owner of record
(INV-SE6), the source-side injection contract with RFC 0008's
non-execution boundary preserved (a driving store is a future RFC 0008
amendment), source-internal state legalized **without** generalizing
update-side external mutation (RFC 0001 §5.5 stays that RFC's scoped
deviation), and the effect-DI negative space owned there (INV-SE8; the
time axis stays RFC 0009's). The behavior change is two-faced —
admission of new and restarted subscriptions waits for outstanding
stopped tasks' quiescence (pure additions with no outstanding stops
admit immediately as today), and re-evaluation gains a
message-independent trigger — carried by RFC 0012's `Changed` entry at
0.11.0.

**Conditions and delegations.** Restart *rate* control is delegated to
a future opt-in policy RFC under RFC 0012 §8's partitioned frame
(blocks composition: no): the barrier, phase placement, and
supersession rules are invariant under any policy; what may relax is
only the re-admission promptness of subscriptions the adopted policy
targets — pure first admission (bootstrap's initial admissions
included) and every promptness clause of subscriptions outside the
policy's target set are preserved under any policy — and only after
that RFC amends the re-admission promptness clauses in RFC 0012 and
RFC 0005 to policy-off scope. The stage-3
driving-store API is delegated to a future RFC 0008 amendment gated on
RFC 0012's acceptance (blocks composition: no).

**Contract impact.** RFC 0012 (Draft overlay) owns the contract body;
the RFC 0011 §2.1 dirty-source amendment is part of the same Draft
overlay; the `Application::subscriptions` rustdoc update is an
implementation deliverable RFC 0012 lists.

**Reopen targets.** Source-execution or effect-DI boundary
counterexamples reopen `root-D18`. Return #1 arose from RFC 0012's
admission rules but its counterexample affected the lifecycle root's
decision (the dirty-source set), so per §1.9 it reopened `root-K51J46`
and not `root-D18` — recorded here because this chapter's rules were
the trigger.

**Return #10 (2026-08-16)**: the same counterexample as return #8
(§3.2), on the root whose audit rows carry it. N27's sharpening and
INT-6's joint walk both rest on cleanup delay being visible through
this root's own machinery —
cleanup delay defers admission through the barrier and is observable
through the gauges, the same shape as a blocking source. The successor
denies both halves: a command or cleanup run neither joins the
subscription barrier nor triggers it (RFC 0014 §5.1), and no gauge
field counts a cleanup run (RFC 0006 §5.2). INT-6's terminal verdict
cannot stand as a pass — the joint walk it verified is unavailable on
the successor — so it re-terminates as `C`, on the condition N27
already names: the graceful-drain slot with its cleanup-hook seam
(§10.1). **This root's judgment is not reopened.** Source execution
stays the redesign RFC 0012 owns, and every rule of §4 there — the
barrier, its admission rules, the boundaries — is unamended by this
correction, which reaches a feature row and an interaction row and
nothing §6.2 decides. The ledger and the audit snapshot stay as
§8.1 adopts them, unamended; this entry is where the correction lives.

## 7. Public API boundary and contour

Owns the root judgment: `root-I40` (the `Message` boundary), together
with the independent leaf judgments this chapter records
(§7.2–§7.8). The remaining small-grain leaf items (the pending-work
test-access cleanup, the visibility lint-dodge inventory, and the
bulk reaffirmation checks) belong to §8's ledger frame and are
recorded there.

### 7.1 `root-I40` — verdict: freeze reaffirmed

`Message: Send + 'static` stays exactly as it is — RFC 0008 INV-T1
remains the canonical statement, and no breaking budget is consumed.
Pressure in the addition direction (features wanting stricter bounds,
e.g. serializable or comparable messages) is absorbed by API-local
opt-in generic bounds on the features that need them — additive, never
a bound on `Message` itself. Pressure in the removal direction
(`!Send` executors, single-threaded wasm) is intentionally excluded,
subordinate to the executor verdict (§7.2). No amendment anywhere.

### 7.2 Executor premise (leaf-K56)

**Verdict: reaffirmed, and the alternate-executor question is closed
as an adopted anti-catalog decision.** The crate offers no public
compatibility surface for alternate async executors, keeps its `Send`
bounds, and its implementation may depend on Tokio; Tokio's internal
types are not thereby contract (RFC 0011 §7 pins none of those
shapes). Consequently non-Tokio runtime support is intentionally
excluded; `!Send`-executor and wasm fixtures are excluded with it; and
the external-driving half of the replacement question stays
conditional on a future driving-surface RFC, which this exclusion does
not block. Trimming the enabled Tokio feature set is contract-neutral
build hygiene (§10 backlog).

### 7.3 Time axes (leaf-N57)

**Verdict: monotonic-only, excluded as a second axis.** The core's
one normative time axis is RFC 0009's virtualizable monotonic clock;
no wall-calendar axis is added and RFC 0009 needs no amendment (its
§1.2 already places calendar time out of scope with application-side
injection). Calendar and wall-clock needs — daily refetch, clock
widgets, timezone-jump tracking — are ordinary subscription sources
under RFC 0012 §2's template: nondeterministic inputs like any other
I/O, touching none of RFC 0009's determinism contracts. Excluded
scope: no second normative time axis in core, no clock-jump tracking
guarantee, no calendar scheduler as framework contract.

### 7.4 Resource envelope (leaf-K55)

**Verdict: the limitation is already canonical — cited, not
re-owned.** The resource envelope tears manages is the queue-slot
occupancy of tears-owned channels, exactly as RFC 0006
R1/INV-L1/§4.2/§4.5 state it: bounded mode does not bound process memory, and upstream
buffers, source-internal queues, identity counts, and producer counts
stay application-owned. No unified task/queue/memory/CPU budget is
provided, and no owner seats are reserved for future budgets (§1.6).
No `update`/`view` execution-time ceiling or preemption is provided
either: a synchronous transition that never returns never reaches a
point where the runtime could observe a deadline while serial
transitions (RFC 0011 INV-LC9) are preserved; observing overrun after
completion is the observability side's affair (§7.8).

### 7.5 `RuntimeConfig` derives (leaf-I41)

**Verdict: `Copy` is removed at 0.11.0; `Clone` stays; `FrameRate`
keeps `Copy`.** Carried by RFC 0007 (its §2.1, `Changed`
breaking entry, and four-part implementation deliverable). The
removal clears the type-level obstacle to future non-`Copy`
configuration fields (policy objects, callbacks) without settling any
such feature — those stay conditional on their own seams, exactly as
§1.6 requires.

### 7.6 Core / non-core boundary (leaf-G35)

**Verdict: the boundary is decided; no minimal-profile contract is
introduced.** Unconditional core — removable by no build profile: the
phase machine and runtime, the channel and load layer (INV-L13
emission included), identity (RFC 0005), the clock (RFC 0009),
TestStore with the unconditional `test-util` feature (the no-flag
decisions of RFC 0008 §3.3 and RFC 0009 §5.1 stand), the terminal
driving surface, the mock source (RFC 0012 §6's reference seam), and
the dependency-free signal/time sources. Non-core — feature-gateable:
sources conforming to RFC 0012 §2's template that carry additional
external dependencies (the HTTP module, WebSocket, TLS selection,
future sources) — gating them changes no contract surface because the
template is the contract. This is the audit exclusion §2.3 cites: no
build profile removing core constituents is offered. Feature-inventory
tidying within this boundary is contract-neutral cleanup (§10
backlog).

### 7.7 Terminal coupling (leaf-G36)

**Verdict: no backend abstraction is designed**, and the
terminal-specific contract surface is declared as a closed five-point
inventory: (1) `Application::view`'s `ratatui::Frame` parameter;
(2) `Runtime::run`'s `ratatui::Terminal<B: Backend>` argument —
already backend-generic, so half the separability exists today;
(3) the terminal-events driving source and its input-occupancy
signals; (4) the panic-hook terminal restore; (5) suspend/resume
terminal ownership. The phase machine itself is backend-independent,
and the separation seam for any future work is recorded: the render
step inside the frame pass, preserving pending-work flag consumption,
with `view`'s `Frame` surface included in the separation subject.
Sanitization and raw-escape policy for untrusted text is not owned by
the runtime — it belongs to the application, the widget layer, and the
backend.

### 7.8 Observability contour (leaf-I42)

**Verdict: the three load-event kinds stay contract; the one change
is the gauge instance field.** Downgrading the capacity-wait event was
rejected: RFC 0007 §5.2's `quit_overload` valid-trial predicate
consumes it normatively (at least two shared-channel capacity-wait
events in the 5ms window before the quit), so a best-effort emission would
leave a conforming RFC 0006 implementation unable to run RFC 0007's
acceptance rows. The single schema change — the gauge event's
`runtime_id` — is carried by the RFC 0006 amendment (INV-L13).
The consumer-side negative space is declared: tears provides no
slow-subscriber or panicking-subscriber isolation, no event-queue
bound or overflow policy, and no subscriber panic containment —
tracing-consumer responsibility; and no common redaction framework —
each content-bearing observation owner, the existing module tracing
included, owns its redact-or-omit policy. State-level inspection and
transcript instrumentation remain conditional on a future transcript
RFC (blocks composition: no); this chapter adds no observation
surface.

**§7 reopen targets.** Boundary or contour counterexamples reopen
`root-I40`; a counterexample against an individual leaf decision is
re-judged in this chapter under the same §1.9 grades. None has
occurred.

## 8. Reaffirmation ledger (snapshot)

The full row-level snapshot behind §2.4's tally — snapshot
evidence in the sense of §1.2, dated with §1.1's baseline and §1.10's
drift endpoints, not a contract that outlives its date. The population
is the 531 ledger rows extracted from RFCs 0001–0009 and the runtime's
previously unnumbered implicit contracts: 478 rows whose source
document is the statement's normative owner, and 53 pointer rows (a
document restating or importing another document's contract). Replay
outcome, as in §2.4: **reaffirmed 522 / redesigned 9 / delegated 0 /
open counterexamples 0**. Row notes write **Δ** as shorthand for the
branch delta against §1.1's baseline corpus — the Draft overlay plus
the semantics-neutral syncs; a row's note may scope Δ to the
sub-delta its replay round measured.

Table columns: **ID** (stable ledger id); **Source** (`owner` /
`pointer`); **Roots** (the root and leaf judgments the row
participates in — `root-*` per the §3–§7 chapters, `leaf-*` per §7's
sub-chapters and §8.2); **Class** (invariant number, unnumbered
norm, threshold, or negative space); **Statement** (condensed
restatement — the owner RFC text remains the sole normative form);
**Verdict** (`reaffirmed` / `redesigned`); **Grounds**
(`preserved_in=` the site where the reference contract keeps the
statement, or `changed_to=` the amendment that now owns it;
`resolved_via=` records a pointer row's resolution target); **Note**.

### 8.1 Ledger structure judgments

Two structural questions about the contract corpus, chartered to this
chapter by §1.2, are settled here rather than as root judgments:

- **Invariant ownership registry — adopted as a snapshot, not as a
  living document.** Each row below names its normative owner and
  marks pointer rows, which is exactly the ownership DAG the corpus
  previously lacked. That DAG is frozen here as evidence; it is not
  carried forward as a maintained registry. Owner RFCs remain the sole
  normative source after this RFC is accepted, and future changes flow
  through owner-RFC amendments as before. A living ledger would be a
  second normative surface that must be kept in sync with every
  amendment, and divergence between the two surfaces would be silent;
  the simplification adopted is that no such surface exists.
- **No standalone normative glossary.** The concepts that were defined
  in several documents (occupancy, ready/deliverable, deadline
  anchoring, batch) each have a single normative owner after
  consolidation, recorded per row below; the other definition sites
  are pointer rows. A standalone glossary would re-introduce a second
  definition site for every term. Informative rustdoc cross-references
  remain an implementation deliverable of the owner RFCs.

### 8.2 Code-level confirmations

Three code-level findings from the audit are recorded here; none
changes a contract.

- **`PendingWork` dual access paths (leaf-F29) — reaffirmed, with a
  cleanup backlog.** Production code reaches pending-work state only
  through the transition methods (the frame scheduler's delegation;
  the mark/take family). The parent-module tests still assign fields
  directly (11 direct assignments in `src/runtime.rs` tests; the
  `pub(super)` visibility exists for this access). Backlog, §10,
  non-gating: consolidate the test path onto transition methods or a
  test-only helper, privatize the fields, and sync the doc comments.
  No contract impact.
- **`pub` visibility lint accommodation (leaf-F32) — reaffirmed
  as-is.** The `redundant_pub_crate` accommodations have shrunk to two
  commented module groups (`channel.rs`, `load.rs`); reachability is
  capped at crate scope by the enclosing `runtime` module. A
  crate-wide `allow` is rejected — it would blind the lint elsewhere.
  The remaining two sites are expected to dissolve in §3's module
  reorganization; until then the commented state is accepted. No
  contract impact.
- **Small-item batch (leaf-I43) — reaffirmed.** (a) `Action`
  privatization is complete — the public surface is `Command`
  constructors only — and RFC 0002 §4/§9 state the completed form;
  (b) `Command::retry`'s inherent-name occupation stays as RFC 0004's
  accepted trade-off; (c) `Effect::map`'s `Arc<Mutex<F>>` is the
  mechanism that admits `Fn + Send` (non-`Sync`) mappers, pinned by a
  regression test — not removable; (d) timeout tie-ordering and its
  neighboring negative space verified preserved; (e) `subscriptions()`
  purity is owned by RFC 0012 §5 (INV-SE6).

### 8.3 Full ledger

| ID | Source | Roots | Class | Statement | Verdict | Grounds | Note |
|---|---|---|---|---|---|---|---|
| LG-0001-001 | owner | root-D18 | INV-1 | INV-1: a query already subscribed before the `invalidate()` call time `T` never loses an invalidation issued at `T` (presupposes the synchronous bump of §5.5) | reaffirmed | preserved_in=0001 §6 (§5.5) | Unaffected — cell contract premised on the synchronous bump (§5.5). 0012 §7 only reconfirms the §5.5 deviation as scoped, without touching INV-1's content; the Δ in 0011 §2.1 / 0002 §9 is also uninvolved |
| LG-0001-002 | owner | root-D18 | INV-2 | INV-2: single-flight is guaranteed by the cell's `in_flight_generation` contract (at most 1 concurrent in-flight per identity). Holds without relying on runtime dedup. Multiple `invalidate` calls during a fetch coalesce into one refetch. identity = (`client_id`, `TypeId`, `QueryKey`) | reaffirmed | preserved_in=0001 §6 (§5.4) | Unaffected — the row itself states non-reliance on runtime dedup. Even if the quiescence barrier introduced in 0012 §4 delays restart admission, single-flight still holds via the cell's in_flight_generation; verdict basis unchanged |
| LG-0001-003 | owner | root-D18 | INV-3 | INV-3: a fetch result is applied only when the target generation == `current_generation` | reaffirmed | preserved_in=0001 §6 (§5.4) | Unaffected — compare-and-commit is an intra-cell generation contract. Neither 0012 (execution contract), 0011 §2.1 (dirty source), nor 0002 §9 touches generation commit |
| LG-0001-004 | owner | root-D18 | INV-4a | INV-4a: subscribing after `invalidate` with data present (or after `stale_time` has elapsed) emits the `is_stale = true` data before refetching (stale-while-revalidate) | reaffirmed | preserved_in=0001 §6 (§5.2) | Unaffected — emit-then-refetch ordering contract at subscription start. 0012's admission changes can only shift when the subscription task starts; the SWR ordering contract itself is unchanged |
| LG-0001-005 | owner | root-D18 | INV-4b | INV-4b: subscribing after `invalidate` with no data is observed as Pending/Fetching with `is_stale = false`, and fetches while respecting the generation | reaffirmed | preserved_in=0001 §6 (§5.2) | Unaffected — the no-data branch's Pending/Fetching observation contract is an intra-cell state transition; no part of the Δ touches the transition rules |
| LG-0001-006 | owner | root-D18 | INV-5 | INV-5: if even one of `client_id` / `TypeId` / `QueryKey` differs, the subscription and cache slot are independent | reaffirmed | preserved_in=0001 §6 (§5.8) | Unaffected — subscription-identity / cache-slot separation is owned by 0001 §5.8. 0012 §1.2 explicitly declares identity (0005 vocabulary) out of scope; no change |
| LG-0001-007 | owner | root-D18 | INV-6 | INV-6: a cell with active subscribers is never GC'd, and its `data` does not expire via `cache_time` | reaffirmed | preserved_in=0001 §6 (§5.10) | Unaffected — cell liveness via subscriber ref count is a GC contract. 0012 §7 only turns source-internal state into clauses and does not concern the GC surface |
| LG-0001-008 | owner | root-D18 | INV-7 | INV-7: an inactive cell's data is reclaimed once `cache_time` has elapsed from the moment it became inactive. Holds via the automatic sweep on each fetch completion | reaffirmed | preserved_in=0001 §6 (§5.10) | Unaffected — inactive-origin cache_time reclamation and the automatic sweep are intra-cell contracts. 0012's start/stop/admission surface does not touch the sweep trigger |
| LG-0001-009 | owner | root-D18 | INV-8 | INV-8 (liveness): a fetch releases `in_flight_generation` whether it ends in success / error / discard, and after a bump the next fetch slot is always acquirable | reaffirmed | preserved_in=0001 §6 (§5.4,§5.6) | Unaffected — fetch-slot release liveness is an intra-cell contract. 0012's quiescence is at the subscription-task-termination layer, a separate layer from fetch slots; basis unchanged |
| LG-0001-010 | owner | root-D18 | INV-9 | INV-9 (post-error retry suppression): after a fetch fails at generation `G`, no refetch occurs while `current_generation` remains `G`. Refetch only via generation advance or clearing by success | reaffirmed | preserved_in=0001 §6 (§5.6) | Unaffected — last_error_generation is cell-internal state. Even though 0012 §7 makes internal state in general template-legal, this invariant's content and owner remain 0001 |
| LG-0001-011 | owner | root-D18 | INV-10 + threshold (ZERO boundary) | INV-10 (time-stale loop suppression): even with `stale_time = ZERO`, a wakeup caused by the cell's own fetch-completion send does not refetch the same generation for a time-stale reason. Time-stale fetches are limited to `InitialObserve` | reaffirmed | preserved_in=0001 §6 (§5.7) | Unaffected — suppressing wakeups from the cell's own fetch-completion send is a source-internal reconcile contract. The second dirty source in 0011 §2.1 concerns the runtime frame layer and is unrelated to cell wakeups |
| LG-0001-012 | owner | root-D18 | unnumbered norm (non-negotiable) | Non-negotiable (A): the state shape is rich from the start. Struct with private fields + accessor reads | reaffirmed | preserved_in=0001 §4 (A),§5.1 | Unaffected — the non-negotiable state-shape condition is a type-design norm. The Δ touches neither the QueryResult surface nor 0001 §4 |
| LG-0001-013 | owner | root-D18 | unnumbered norm (non-negotiable) | Non-negotiable (B): state is concentrated in the cell, and the subscription stream drives fetches via observe-and-reconcile. The cell is the sole source of truth | reaffirmed | preserved_in=0001 §4 (B),§5.3 | Unaffected — 0012 §7 cites 0001's cell as a precedent for source-internal state, legalizing it from exception to clause, but the ownership and content of observe-and-reconcile and the single source of truth remain 0001 §4(B),§5.3 |
| LG-0001-014 | owner | root-D18 | unnumbered norm | `QueryResult` fields are private; reads go through accessors. Public fields are forbidden because they conflict with future non-breaking overlay additions (§9) | reaffirmed | preserved_in=0001 §5.1 | Unaffected — private fields + accessors is an API-surface norm. 0012 adds no public API (opening of §11), so uninvolved |
| LG-0001-015 | owner | root-D18 | unnumbered norm | Public enums (`QueryStatus`, `FetchStatus`) are `#[non_exhaustive]` | reaffirmed | preserved_in=0001 §5.1 | Unaffected — no part of the Δ touches the #[non_exhaustive] norm for public enums |
| LG-0001-016 | owner | root-D18 | unnumbered norm | `QueryStatus::Error` is a state kind; the error value is accessed via `error()` (to allow `data = Some(previous), status = Error`) | reaffirmed | preserved_in=0001 §5.1 | Unaffected — treating Error as a state kind is type semantics specific to 0001, with no corresponding part in the 0012/0011/0002 Δ |
| LG-0001-017 | owner | root-D18 | unnumbered norm + negative space | `is_stale` is a snapshot value as of emit time. Even if `stale_time` elapses with no event, the snapshot is retained until the next reconcile (intentional and documented) | reaffirmed | preserved_in=0001 §5.1 | Unaffected — emit-time snapshot semantics is a cell contract. Also consistent with 0012 §2's tolerance of arbitrary poll pacing; content unchanged |
| LG-0001-018 | owner | root-D18 | threshold + unnumbered norm | `is_stale := data.is_some() && (data_generation < current_generation \|\| data_timestamp.elapsed() >= stale_time)`; the boundary is `>=` | reaffirmed | preserved_in=0001 §5.2 | Unaffected — the is_stale predicate and the >= boundary are the threshold definition of 0001 §5.2 and appear nowhere in the Δ |
| LG-0001-019 | owner | root-D18 | unnumbered norm | No-data states have `is_stale = false`. With no data, fetch as Pending/Fetching; the generation is used only for the commit race check | reaffirmed | preserved_in=0001 §5.2 | Unaffected — no-data is_stale=false is a corollary of the §5.2 predicate. Δ uninvolved |
| LG-0001-020 | owner | root-D18 | unnumbered norm | The stream never reads the watch payload directly. On every wakeup it re-reads the authoritative state under the cell mutex (watch is a pure notification) | reaffirmed | preserved_in=0001 §5.3 | Unaffected — the contract of treating watch as pure notification and re-reading authoritative state under the mutex is source-stream-internal behavior. 0012 §2 only cites poll-pacing tolerance as the default norm; the re-read contract remains owned by 0001 |
| LG-0001-021 | owner | root-D18 | unnumbered norm | The check-and-set of `in_flight_generation` happens under the cell state mutex | reaffirmed | preserved_in=0001 §5.4 | Unaffected — check-and-set under the mutex is a cell synchronization norm. The Δ does not touch cell synchronization |
| LG-0001-022 | owner | root-D18 | unnumbered norm | Loser streams do not fetch; they await the result via `watch.changed()`. A stream with `last_error_generation == current_generation` neither fetches nor waits | reaffirmed | preserved_in=0001 §5.4 (§5.6) | Unaffected — the loser stream's wait/no-wait branching is intra-cell arbitration. Δ uninvolved |
| LG-0001-023 | owner | root-D18 | unnumbered norm | Fetcher arbitration = the fetcher of the stream that first acquires the in-flight slot. Premise contract that requests with the same identity are equivalent | reaffirmed | preserved_in=0001 §5.4 (§5.8) | Unaffected — fetcher arbitration premised on same-identity equivalence remains a 0001 §5.8 premise. 0012 §1.2 explicitly puts identity out of scope |
| LG-0001-024 | owner | root-D18 | unnumbered norm | Discard branch: if `invalidate` during a fetch makes `G < current_generation`, discard the result, clear the slot, watch send, and re-fire reconcile at the latest generation | reaffirmed | preserved_in=0001 §5.4 | Unaffected — the discard branch is intra-cell generation-race handling, independent of 0012's quiescence/admission |
| LG-0001-025 | owner | root-D18 | unnumbered norm | Coalescing: multiple bumps within one in-flight window coalesce into a single refetch at the next latest generation, looping until acquired == current commits | reaffirmed | preserved_in=0001 §5.4 | Unaffected — coalescing of bumps within the in-flight window is an intra-cell contract. Δ uninvolved |
| LG-0001-026 | owner | root-D18 | unnumbered norm | `invalidate()` completes "bump + watch send" as a single synchronous operation. Returns no `Command`; updates directly via a `&self` method (an intentional deviation from TEA) | reaffirmed | preserved_in=0001 §5.5 | 0012 §7/INV-SE7 explicitly reconfirms this row's synchronous invalidate() as an intentional TEA deviation scoped to 0001 §5.5, and newly owns its non-generalization (no general license for update-side external mutation). The verdict remains reaffirmed unchanged, but 0012 §7's reconfirmation is added to the verdict basis |
| LG-0001-027 | owner | root-D18 | unnumbered norm | On fetch failure, the previous successful `data` is retained. Errors do not change freshness | reaffirmed | preserved_in=0001 §5.6 | Unaffected — retaining previous data on error and leaving freshness unchanged are cell state transitions with no corresponding part in the Δ |
| LG-0001-028 | owner | root-D18 | unnumbered norm + threshold | fetch predicate: `needs_fetch := (no_data \|\| generation_stale \|\| (time_stale && reason == InitialObserve)) && !has_in_flight && last_error_generation != current_generation` | reaffirmed | preserved_in=0001 §5.7 | Unaffected — the needs_fetch predicate is the threshold definition of 0001 §5.7. No document in the Δ touches the fetch predicate |
| LG-0001-029 | owner | root-D18 | negative space + unnumbered norm | `ReconcileReason` / `FetchDecisionInput` / `should_fetch` are `pub(super)` and not public API | reaffirmed | preserved_in=0001 §5.7 | Unaffected — the pub(super) negative space is crate-internal visibility. Consistent with 0012 also adding no public API; basis unchanged |
| LG-0001-030 | owner | root-D18 | unnumbered norm | Subscription identity always includes `TypeId` | reaffirmed | preserved_in=0001 §5.8 | Unaffected — subscription identity including TypeId is owned by 0001 §5.8. 0012 §1.2 explicitly puts identity out of scope; no change |
| LG-0001-031 | owner | root-D18 | unnumbered norm | Cell map key = (`TypeId`, `QueryKey`). `client_id` is not included (client separation via cell-map ownership). The subscription-identity side does include `client_id` | reaffirmed | preserved_in=0001 §5.8 | Unaffected — the cell-map key and client-separation design are on the identity surface; uninvolved per 0012 §1.2's out-of-scope declaration |
| LG-0001-032 | owner | root-D18 | unnumbered norm | `QueryKey(Arc<[QueryKeyPart]>)` uses structural comparison and cannot collide. `QueryKeyPart` is `#[non_exhaustive]` | reaffirmed | preserved_in=0001 §5.8 | Unaffected — QueryKey structural comparison and #[non_exhaustive] are identity-representation norms. Δ uninvolved |
| LG-0001-033 | owner | root-D18 | unnumbered norm | Cell updates are limited to atomic replace + `watch.send`; mutable references are never leaked | reaffirmed | preserved_in=0001 §5.9 | Unaffected — the atomic-replace + watch-send-only cell update norm remains owned by 0001. 0012 §7 only cites the cell as a precedent and does not change the update norm |
| LG-0001-034 | owner | root-D18 | unnumbered norm | Because `TypeId` is part of the map key, `Arc::downcast` always succeeds. Operations not needing `T` call `AnyCell` methods directly | reaffirmed | preserved_in=0001 §5.9 | Unaffected — always-successful downcast and direct AnyCell calls are type-erasure implementation norms. Δ uninvolved |
| LG-0001-035 | owner | root-D18 | unnumbered norm | An active cell's data is retained regardless of `cache_time`. `cache_time` and `stale_time` are independent; `cache_time < stale_time` is allowed | reaffirmed | preserved_in=0001 §5.10 | Unaffected — active-data retention and cache_time/stale_time independence are retention contracts with no corresponding part in the Δ |
| LG-0001-036 | owner | root-D18 | unnumbered norm | The `cache_time` timer starts when the cell becomes inactive (not from the fetch timestamp; a semantic break from 0.8.x) | reaffirmed | preserved_in=0001 §5.10,§8 | Unaffected — the recorded semantic break to an inactive_since origin is a migration fact specific to 0001. Δ uninvolved |
| LG-0001-037 | owner | root-D18 | unnumbered norm | Subscriber count is an explicit ref count via `CellSubscription` guards | reaffirmed | preserved_in=0001 §5.10 | Unaffected — the explicit ref count via CellSubscription guards is cell liveness management, untouched by the Δ |
| LG-0001-038 | owner | root-D18 | unnumbered norm | Cell creation and initial subscribe are atomic under the map shard lock (prevents concurrent GC from splitting one identity across 2 cells) | reaffirmed | preserved_in=0001 §5.10 | Unaffected — atomicity of cell creation + initial subscribe under the shard lock is a cell-map contract. Δ uninvolved |
| LG-0001-039 | owner | root-D18 | unnumbered norm | GC runs automatically on each fetch completion + manual trigger via `QueryClient::gc()` | reaffirmed | preserved_in=0001 §5.10 | Unaffected — the GC triggers (each fetch completion + manual) are intra-cell contracts, a layer apart from 0012's execution contract |
| LG-0001-040 | owner | root-D18 | unnumbered norm | Emission de-dup: the cell's `version: AtomicU64` and the subscriber-side `seen_version` suppress redundant emits. Genuine changes are never dropped | reaffirmed | preserved_in=0001 §5 (Emission de-dup) | Unaffected — emission de-dup via version/seen_version is a cell↔subscriber contract. The dirty-related Δ in 0011 §2.1 is at the runtime frame layer and unrelated |
| LG-0001-041 | owner | root-D18 | unnumbered norm | `stale_time` is retained in the 0.9.0 core. `QueryConfig` is non-breaking | reaffirmed | preserved_in=0001 §3 | Unaffected — retaining stale_time and keeping QueryConfig non-breaking are scope declarations. The Δ does not touch 0001's API/scope |
| LG-0001-042 | owner | root-D18 | unnumbered norm | Public read API retained: accessors and the `Query::new(key, fetcher, client)` shape are preserved | reaffirmed | preserved_in=0001 Summary,§8 | Unaffected — declaration retaining the public read API (accessors, Query::new). 0012 explicitly states public signatures unchanged; uninvolved |
| LG-0001-043 | owner | root-D18 | negative space | Timer-driven background revalidation (`refetchInterval` equivalent) is not implemented in 0.9.0 | reaffirmed | preserved_in=0001 §3,§10 | Unaffected — the negative space of no timer-driven revalidation stays with 0001. 0012 §9's effect-DI negative space is on the executor/DI axis, a different matter; 0009's time axis is also unchanged |
| LG-0001-044 | owner | root-D18 | negative space | Retry / backoff policy not implemented (future non-breaking addition possible on top of `last_error_generation`) | reaffirmed | preserved_in=0001 §3,§5.6,§10 | Unaffected — no retry/backoff for query fetches is 0001's negative space. What 0012 §8 delegates is subscription restart rate, a different target with a clear boundary |
| LG-0001-045 | owner | root-D18 | negative space | Prefetch / advanced cache control not implemented | reaffirmed | preserved_in=0001 §3,§10 | Unaffected — non-implementation of prefetch/advanced cache control is 0001 negative space with no corresponding surface in any document of the Δ |
| LG-0001-046 | owner | root-D18 | negative space | Optimistic updates are out of scope. Room for adding a provisional overlay is kept; in 0.9.0 the overlay is always empty | reaffirmed | preserved_in=0001 §3,§9 | Unaffected — optimistic updates out of scope and the empty-overlay allowance remain 0001 §9. Δ uninvolved |
| LG-0001-047 | owner | root-D18 | negative space | `fetch_status` transitions only between `Idle`/`Fetching` in core (the type is introduced ahead of need) | reaffirmed | preserved_in=0001 §5.1 | Unaffected — the Idle/Fetching-only transitions of fetch_status (type introduced ahead of need) are 0001 negative space. Δ uninvolved |
| LG-0001-048 | owner | root-D18 | negative space | Swapping the fetcher for the same identity is unsupported (change the request by changing the key) | reaffirmed | preserved_in=0001 §8 | Unaffected — no fetcher swapping is a corollary of identity keying. 0012 §1.2 puts identity out of scope, and the norm that changes go through key changes is also unchanged |
| LG-0001-049 | owner | root-D18 | unnumbered norm (semantic break) | `QueryState` is removed, replaced by the rich `QueryResult` | reaffirmed | preserved_in=0001 §8 (§5.1) | Unaffected — the QueryState removal → QueryResult replacement is a record of the 0.9.0 migration with no corresponding part in the Δ |
| LG-0001-050 | owner | root-D18 | unnumbered norm | That future features land as `Added` (non-breaking) is itself the confirmation condition that core (A)(B) hold. If a breaking change becomes necessary, the core has a gap (meta contract) | reaffirmed | preserved_in=0001 §10 | Unaffected — the "future features land as Added" meta contract remains 0001 §10. 0012 itself is non-breaking to the 0001 API (no public API additions) and is if anything a confirming example |
| LG-0001-051 | owner | root-D18 | unnumbered norm (enforcement declaration) | Test division of labor: synchronization-primitive core = loom (`loom-core` feature). Wiring/timing = tokio integration tests | reaffirmed | preserved_in=0001 §7 | Unaffected — the loom/tokio test division is 0001 §7's enforcement declaration. 0012 §11's enforcement class is for 0012's invariants and does not touch 0001's test strategy |
| LG-0002-001 | owner | root-B7 | INV-1 | INV-1 (default preserved): command processing in all existing constructors sets `needs_redraw = true`. Existing app behavior is unchanged | reaffirmed | preserved_in=0002 §6 (§2 (A)) | That the runtime does not consult the init command's redraw directive (0011 §3.2) is a pre-existing fact from 0008 §5.2 and does not contradict the constructor default contract |
| LG-0002-002 | owner | root-B7,root-K51J46 | INV-2 | INV-2 (opt-out suppresses redraw): if all processed messages in a batch returned `without_redraw()` commands, `needs_redraw` is not changed | reaffirmed | preserved_in=0002 §6 (§5.2) | Unaffected — the Δ's 0002 change is §9 (informative) only; INV-2's body §5.2/§6 is unchanged. After the 0011 §2.1 rewrite, the recording of redraw pending remains "RFC 0002's OR-fold"; the second dirty source is added only on the subscriptions side and does not touch the needs_redraw fold |
| LG-0002-003 | owner | root-B7,root-K51J46 | INV-3 | INV-3 (mixed batch redraws): a micro-batch containing even one redraw-ing command sets `needs_redraw = true` regardless of order | reaffirmed | preserved_in=0002 §6 (§5.2) | Unaffected — INV-3's order-independent OR is unchanged in §5.2/§6 (Δ is §9 only). The 0011 §2.1 addition is a second supply source of subscription dirtiness; the shape of consuming the intra-batch redraw OR-fold (INV-LC1) is also unchanged |
| LG-0002-004 | owner | root-B7,root-D18 | INV-4 | INV-4 (subscriptions unaffected): even a `without_redraw()` command sets `subscriptions_dirty = true`. Redraw suppression never suppresses subscription re-evaluation | reaffirmed | preserved_in=0002 §6 (§2 (B),§5.3) | Unaffected — the second dirty source (0011 §2.1) is additive and does not conditionalize INV-4's content that without_redraw does not suppress subscriptions_dirty. The rewritten 0011 §2.2, including the lifecycle-completion-only pass, explicitly cites "suppression is redraw-only; re-evaluation is never suppressed (0002 non-negotiable B)" |
| LG-0002-005 | owner | root-B7 | INV-5a | INV-5a (batch OR): `Command::batch([...]).redraw == any(child.redraw)`. Computed over children independently of the presence of `stream` | reaffirmed | preserved_in=0002 §6 (§5.4) | Unaffected — the batch OR is unchanged in §5.4. The rewritten §9 also spells out the contrast between Axis A's OR-fold and cancellation's own fold (0003 INV-11 — warn+discard/concat, not an OR-fold), which if anything strengthens the consistency basis |
| LG-0002-006 | owner | root-B7 | INV-5b | INV-5b (all-opted-out vs empty): `batch([])` has `redraw == true` (empty fallback), but all children opted out gives `redraw == false` (no silent revert to `true`) | reaffirmed | preserved_in=0002 §6 (§5.4) | - |
| LG-0002-007 | owner | root-B7 | INV-6 | INV-6 (map propagation): `map` preserves `redraw` on both branches (the `stream == None` branch must not reset to default) | reaffirmed | preserved_in=0002 §6 (§5.5) | Consistent with 0003 INV-12 (map preserves all directives) |
| LG-0002-008 | owner | root-B7 | INV-7 | INV-7 (recovery): suppression approximates "state changed". Even if opted out mistakenly, the view recovers on the next redraw-ing command | reaffirmed | preserved_in=0002 §6 | Unaffected — 0002 §6 is unchanged by the Δ. The adversarial model in 0011 §2.2 (superseding sequence) survives the rewrite; no change touches the recovery semantics via INV-3 |
| LG-0002-009 | owner | root-B7 | INV-8 | INV-8 (`is_none` / `redraw` independence): `is_none()` reflects only `stream`; `none().without_redraw()` is a valid combination | reaffirmed | preserved_in=0002 §6 (§5.6) | - |
| LG-0002-010 | owner | root-B7 | unnumbered norm (non-negotiable) | Non-negotiable (A): default behavior unchanged. Suppression is opt-out only | reaffirmed | preserved_in=0002 §2 (A) | - |
| LG-0002-011 | owner | root-B7,root-D18 | unnumbered norm (non-negotiable) | Non-negotiable (B): redraw suppression and subscription re-evaluation are separate concerns. This RFC gates only `needs_redraw`; `subscriptions_dirty` remains unconditional | reaffirmed | preserved_in=0002 §2 (B),§5.3 | §5.3's per-batch unconditional overclaim has been replaced by a defer to 0003 §4.4 (§5.3 retitled "Subscriptions stay independent of the directive"). Non-negotiable B (independence from the redraw directive) is preserved verbatim — this row's owner claim (0002 owns the unconditionality = directive independence) is unchanged. That the batch-side recording rule is owned by 0003 §4.4 matches the verdict on LG-0003-040 |
| LG-0002-012 | owner | root-B7 | unnumbered norm + negative space | `without_redraw()` is an optimization hint, not a guarantee. The runtime is free to redraw | reaffirmed | preserved_in=0002 §5.1 | First-render eligibility in 0011 §3.2 is consistent with "hint, not guarantee" |
| LG-0002-013 | owner | root-B7,root-A1 | unnumbered norm | A command's side effects still execute even with `without_redraw()` | reaffirmed | preserved_in=0002 §5.1 | - |
| LG-0002-014 | owner | root-B7 | unnumbered norm | Implementation invariant: `redraw` is tracked independently of `stream`. `stream.is_none()` short-circuit sites must pass the `redraw` bit through | reaffirmed | preserved_in=0002 §5.1 | - |
| LG-0002-015 | owner | root-B7 | unnumbered norm | Single default initialization point: `redraw: true` is centralized in a single private helper (makes the next Axis-A attribute addition a one-line change) | reaffirmed | preserved_in=0002 §5.1 | Unaffected — §5.1 is unchanged by the Δ. The rewritten §9 also keeps Axis A (passive field; future without_subscription_update) and "the minimal bool/enum attribute set describes Axis A only", so the benefit rationale that the single default initialization point makes the next Axis-A attribute addition a one-line change is unchanged. The reclassification of cancellable (lifecycle metadata within Axis B) is unrelated to Axis A's initialization point |
| LG-0002-016 | owner | root-B7 | unnumbered norm | The `redraw` field is private, default `true`; `without_redraw()` is a `#[must_use]` consuming builder. `Action` is unchanged | reaffirmed | preserved_in=0002 §5.1 | Unaffected — §5.1 (private field, default true, #[must_use] consuming builder) is unchanged by the Δ. In the rewritten §9, Action privatization remains a separate breaking scope, and the determination that cancellation needs no new variant, keeping the closed set (Message \| Quit), reinforces "Action is unchanged" |
| LG-0002-017 | owner | root-B7,root-K51J46 | unnumbered norm | Runtime gating: read via intra-batch OR before enqueue. Only the `needs_redraw = true` line is conditionalized | reaffirmed | preserved_in=0002 §5.2 | Unaffected — §5.2 is unchanged by the Δ. The rewritten 0011 §8 excluded claims still name 0002 as the recording owner of the redraw OR-fold (0012 owns only the lifecycle-completion-side recording rule), so the owner placement holds as-is |
| LG-0002-018 | owner | root-B7,root-K51J46 | unnumbered norm | `needs_redraw` is a persistent flag, cleared after render | reaffirmed | preserved_in=0002 §5.2 | Clear-after-render = consumption of pending work by the 0011 frame pass. The flag mechanism is treated as informative in 0011 §7 |
| LG-0002-019 | owner | root-B7 | unnumbered norm | `batch`: `redraw` = OR of children, computed before the stream filter. The empty fallback applies only with zero children | reaffirmed | preserved_in=0002 §5.4 | - |
| LG-0002-020 | owner | root-B7 | unnumbered norm | `map`: preserves `self.redraw` on both branches. `without_redraw().map(f)` and `map(f).without_redraw()` yield the same result | reaffirmed | preserved_in=0002 §5.5 | - |
| LG-0002-021 | owner | root-B7 | unnumbered norm | `is_none()` / `is_some()` remain stream-based and do not consider `redraw` (the 2 fields are independent by design) | reaffirmed | preserved_in=0002 §5.6 | - |
| LG-0002-022 | owner | root-B7 | unnumbered norm | New valid state: a command with `stream == None && redraw == false` carries a runtime directive even though `is_none() == true`. `is_none()` must not be treated as "droppable" | reaffirmed | preserved_in=0002 §5.6 | Preserved after 0003's cancels/directives extension as well (cancels are applied before the stream-less early return, LG-0003-029) |
| LG-0002-023 | owner | root-B7 | unnumbered norm | Doc updates are a mandatory PR deliverable: the `Command` type = "side-effect stream + runtime directives", etc. | reaffirmed | preserved_in=0002 §3,§5.6 | - |
| LG-0002-024 | owner | root-B7 | unnumbered norm (goal) | Goal: make the render count proportional to visible-changing messages | reaffirmed | preserved_in=0002 §3 | Unaffected — the rewritten 0011 §2.1 continues to cite "render cost proportional to frames (RFC 0002's premise)". The added message-independent frame pass does not run render unless redraw is pending, so the goal of render count proportional to visible changes is not broken |
| LG-0002-025 | owner | root-B7 | negative space | Partial memoization (Elm `Html.Lazy` equivalent) is out of scope. Whole-frame suppression only | reaffirmed | preserved_in=0002 §3,§9 | Negative space unchanged |
| LG-0002-026 | owner | root-B7,root-D18 | negative space | Subscription re-evaluation skip is out of scope. Possible in future as a separate opt-out (e.g. `without_subscription_update()`) | reaffirmed | preserved_in=0002 §3,§9 | Unaffected — the §9 rewrite is limited to the breaking track and Axis B sections; the additive section's text that subscription re-evaluation skip remains possible in future as a separate opt-out (without_subscription_update) and is out of scope per non-negotiable B is unchanged. The second trigger of 0012 §4.3 is a separate lineage from batch-derived dirtiness and does not conflict with the future separate opt-out allowance |
| LG-0002-027 | owner | root-B7,root-K51J46 | negative space | Adaptive / capped frame rate is out of scope | reaffirmed | preserved_in=0002 §3 | Negative space unchanged |
| LG-0002-028 | owner | root-B7 | negative space (rejection) | `Action::SkipRedraw` variant rejected (the flag is set synchronously / `Action` is drained asynchronously, so the timing does not line up) | reaffirmed | preserved_in=0002 §4,§8 | The rejection rationale (synchronous set / asynchronous drain timing mismatch) still holds after integration |
| LG-0002-029 | owner | root-B7 | negative space (rejection) | egui-style opt-in (default no redraw) rejected: the opt-out model degrades mistakes to "one extra redraw" (fail-safe) | reaffirmed | preserved_in=0002 §8 | - |
| LG-0002-030 | owner | root-B7 | negative space (rejection) | Changing `update`'s return type (a dedicated outcome type) rejected: `Command` already is the update-outcome/directive channel | reaffirmed | preserved_in=0002 §8 (§4) | - |
| LG-0002-031 | owner | root-B7 | negative space + forward-compatibility declaration | `with_redraw(bool)` / `silent()` etc. rejected. `redraw_if(bool)` can be added later additively | reaffirmed | preserved_in=0002 §5.1.1 | Forward-compatibility declaration (additive room for redraw_if) unchanged |
| LG-0002-032 | pointer | root-B7 | negative space + subordination declaration (to a future RFC) | Removing the public `Action` is a separate breaking scope, but it should land **before** directive-adding modifiers (e.g. `cancellable`) | reaffirmed | preserved_in=0002 §9; resolved_via=0002 §9 | The delegation target (the future Action privatization RFC) has already been executed; §4/§9 are synchronized to the completed state (the ordering constraint is satisfied; the preservation conditions — the two quit-delivery contracts plus the lowering path — are confirmed pinned in the current RFCs). Delegation → reaffirmation: §9 preserves the constraints as a completion record |
| LG-0002-033 | owner | root-B7,root-K51J46 | unnumbered norm (applicability condition) + threshold | Applicability condition: presupposes Tick suppression (because `Tick` re-sets every frame). Unconditional redraw from `Tick` would also nullify `should_process_frame` idle gating | reaffirmed | preserved_in=0002 §1.1 (§5.2) | should_process_frame idle gating is consistent with the frame-branch gating premise of 0011 §7 |
| LG-0002-034 | owner | root-B7 | threshold (estimate, non-acceptance) | Estimated effect (best case, non-binding). Motivating profile: `process_frame_tick` = 89% of the main thread | reaffirmed | preserved_in=0002 §1,§1.1 | Non-binding estimate. Preserved with no contractual force |
| LG-0002-035 | owner | root-B7 | unnumbered norm (release contract) | CHANGELOG declaration: one `Added` PR; existing invariants unchanged | reaffirmed | preserved_in=0002 header | Preserved as a release declaration (historical fact) |
| LG-0002-036 | owner | root-B7 | unnumbered norm (enforcement declaration) | Test division of labor: private-field algebra = white-box unit tests; user-observable contracts = runtime integration tests | reaffirmed | preserved_in=0002 §7 | Unaffected — 0002 §7 is unchanged by the Δ. The 0011 §8 INV-LC1 enforcement text added pending work from lifecycle completion, but the runtime-layer white-box pattern itself is retained, staying consistent with the white-box unit / runtime integration division |
| LG-0003-001 | owner | root-A1 | unnumbered norm A | Non-negotiable A: commands without cancellation metadata use the existing unkeyed path / shared channel; existing apps observe no new behavior | reaffirmed | preserved_in=0003 §1.2 | Even under the unified execution mechanism (model (b)), unkeyed takes the same path without identity/policy (§2.3); the observable behavior of existing apps is unchanged |
| LG-0003-002 | owner | root-A1 | unnumbered norm B (strict) | Non-negotiable B (strict): after cancel/supersede, none of that run's outputs (`Action::Message`, `Action::Quit`, already-buffered, and buffered after task completion) are ever delivered | reaffirmed | preserved_in=0003 §1.2 | Preserved under §2.2's keyed contract set (RFC 0003 INV-2–INV-16 unchanged) |
| LG-0003-003 | owner | root-A1 | unnumbered norm C | Non-negotiable C: correct by construction — no reliance on app-side stale checks, generation filters in update, or best-effort abort. The runtime owns and drops the sole receiver of stale output | reaffirmed | preserved_in=0003 §1.2 | The runtime-owned private-receiver drop structure is unchanged after integration |
| LG-0003-004 | owner | root-A1,root-K51J46 | unnumbered norm D | Non-negotiable D: no detached tasks or unbounded completed-task records. Keyed tasks are abortable by id/shutdown, completed tasks are reaped, and stale completions cannot mutate a new run | reaffirmed | preserved_in=0003 §1.2 | Mechanism note: unkeyed is also unified bookkeeping + JoinSet<TaskExit> (§2.2), but the semantics of no detached tasks / reap / stale non-mutation are preserved |
| LG-0003-005 | owner | root-B7 | unnumbered norm E | Non-negotiable E: `Action` stays closed. Cancellation is command state and adds no new `Action` variant. The effect stream yields only `Action::Message` / `Action::Quit` | reaffirmed | preserved_in=0003 §1.2 | Non-negotiable E (Action closed) preserved. Cancellation remains command state |
| LG-0003-006 | owner | root-A1 | negative space | No rollback of external work is guaranteed (no undoing of accepted HTTP, completed FS writes, etc.) | reaffirmed | preserved_in=0003 §1.3 | Negative space unchanged |
| LG-0003-007 | owner | root-A1 | negative space | No public cancellation handle is provided (cancellation is only a value returned by `update`) | reaffirmed | preserved_in=0003 §1.3 | Negative space unchanged (cancellation only via the update return value) |
| LG-0003-008 | owner | root-A1 | negative space | Per-child cancellation inside `Command::batch` is unsupported (future work) | reaffirmed | preserved_in=0003 §1.3 | Per-child cancellation remains future work |
| LG-0003-009 | owner | root-A1,root-D18 | negative space | No keyed-task registry sharing with `SubscriptionManager` (commands use a private channel + imperative replacement; subscriptions use declarative diff) | reaffirmed | preserved_in=0003 §1.3 | Unaffected — 0012 changes the subscription side's admission timing but keeps the declarative diff (reconcile) shape of the desired set, and does not share a registry with the commands' private channel + imperative replacement. No Δ touches the non-sharing negative space of 0003 §1.3 |
| LG-0003-010 | owner | root-A1 | negative space | `debounce` / `throttle` are out of scope for this RFC (require clock injection; to be built later on top of the keyed lifecycle) | reaffirmed | preserved_in=0003 §1.3 | debounce/throttle out of scope unchanged |
| LG-0003-011 | owner | root-A1,leaf-I43 | unnumbered norm | `CommandId` / `CancelPolicy` are exported only from `tears::command`, not from the crate root / prelude | reaffirmed | preserved_in=0003 §3 | Export scope unchanged |
| LG-0003-012 | owner | root-A1,root-CMP | unnumbered norm | `CommandId` is structural equality: holds an erased `Eq + Hash + Send + Sync + 'static` value, not a pre-hash surrogate. TypeId is part of equality (`new("search")` and `new(String::from("search"))` are distinct ids) | reaffirmed | preserved_in=0003 §3.1 | Structural equality and TypeId-inclusive identity unchanged |
| LG-0003-013 | owner | root-A1,root-CMP | unnumbered norm | A hash collision does not imply equality (equality is decided by actual comparison of the erased values) | reaffirmed | preserved_in=0003 §3.1 | hash collision ≠ equality unchanged |
| LG-0003-014 | owner | root-A1,root-CMP | negative space (no Debug guarantee) | `CommandId`'s `Debug` is a diagnostic representation of the erased type name only and is unstable. Values are not displayed; unequal same-type ids may share identical Debug output | reaffirmed | preserved_in=0003 §3.1 | Debug non-guarantee unchanged |
| LG-0003-015 | owner | root-A1 | unnumbered norm | `CancelPolicy::default()` is `CancelInFlight`; the enum is `#[non_exhaustive]` | reaffirmed | preserved_in=0003 §3.2 | default=CancelInFlight and #[non_exhaustive] unchanged |
| LG-0003-016 | owner | root-A1 | unnumbered norm | Occupancy is deliverability-based (running, or undelivered buffered output exists). Not `StreamMap` entry existence. Reconcile the id before the policy decision; do not treat a closed-empty entry as occupied | reaffirmed | preserved_in=0003 §3.2 | Deliverability-based occupancy is preserved under the unified bookkeeping as well (§2.2: 0003 FSM unchanged) |
| LG-0003-017 | owner | root-A1 | unnumbered norm premised on INV-4 | `Command::cancel(id)` is strict and idempotent, policy-independent (cancels `KeepInFlight` runs too); no-op if the id is absent | reaffirmed | preserved_in=0003 §3.3 | strict/idempotent/policy-independent unchanged |
| LG-0003-018 | pointer | root-A1,root-B7 | unnumbered norm | `Command::cancel(id)` requests a redraw by default (suppressible via `without_redraw()`) | reaffirmed | preserved_in=0003 §3.3; resolved_via=LG-0002-016 | Pointer row (the owner of the redraw default/without_redraw is the 0002-side owner row). Semantics unchanged |
| LG-0003-019 | owner | root-A1 | unnumbered norm / negative space | `Command::none().cancellable(id)` is inert (no stream to spawn). A pure cancel must be `Command::cancel(id)` | reaffirmed | preserved_in=0003 §3.3 | Inert cancellable(id) unchanged |
| LG-0003-020 | owner | root-A1 | unnumbered norm | Keyed output goes through a runtime-owned private receiver. The task wrapper owns the sole sender; user code never sees the sender | reaffirmed | preserved_in=0003 §4.1 | The private-receiver ownership structure is preserved |
| LG-0003-021 | owner | root-A1 | unnumbered norm | `CommandReceiver` yields `ReceiverEvent::Closed` (not `None`) when closed+empty, then parks pending until the runtime removes it (avoids StreamMap silent removal) | reaffirmed | preserved_in=0003 §4.1 | `ReceiverEvent::Closed` park behavior unchanged (mechanism unification does not touch FSM semantics) |
| LG-0003-022 | owner | root-A1 | unnumbered norm | `ReceiverFacts` (`sender_closed`, `buffered`) is confined to the keyed-command module. Sampled immediately after an Output pull; the lifecycle transition is applied before the payload is returned | reaffirmed | preserved_in=0003 §4.1 | Module-internal confinement of `ReceiverFacts` and sampling order unchanged |
| LG-0003-023 | owner | root-A1 | implementation norm for INV-7 | `sender_closed && buffered == 0` → release the id before `update()` returns same-id work; `sender_closed && buffered > 0` → transition to `Draining` | reaffirmed | preserved_in=0003 §4.1 | Preserved as the INV-7 implementation rule |
| LG-0003-024 | owner | root-A1 | unnumbered norm | `Draining` is non-empty by invariant (closed+empty means `Absent`) | reaffirmed | preserved_in=0003 §4.2 | `Draining` non-emptiness invariant preserved |
| LG-0003-025 | owner | root-A1 | unnumbered norm (transition table) | Transition table (Absent/Running/Draining × Spawn/Cancel/Output/TaskExit/Closed → `LifecycleDecision`) as given in the §4.2 table | reaffirmed | preserved_in=0003 §4.2 | Transition-table semantics unchanged. Note: the O(1) move to a single authoritative structure is mechanism-only (no-double-bookkeeping gate) |
| LG-0003-026 | owner | root-A1 | unnumbered norm | The transition function is total over TaskExit input (late completion is normal under cancellation) | reaffirmed | preserved_in=0003 §4.2 | TaskExit totality preserved |
| LG-0003-027 | owner | root-A1 | unnumbered norm | Before the Spawn decision, reap all completed keyed tasks and sample the target id's receiver facts once. The window during panic logging where TaskExit is not yet published is also closed by the targeted snapshot | reaffirmed | preserved_in=0003 §4.2 | Closure of the panic window via reap + targeted snapshot preserved |
| LG-0003-028 | owner | root-A1 | INV-7 reinforcement | `KeepInFlight` consults reconciled state. Spawns a same-id retry only when the previous receiver is proven sender-closed and empty. Never infers a "final result" from a single mid-stream delivery on an open stream | reaffirmed | preserved_in=0003 §4.2 | `KeepInFlight` reconciled-state consultation preserved |
| LG-0003-029 | owner | root-A1 | unnumbered norm | Enqueue order: `reconcile_keyed_available()` → apply all `cancels` → stream-less early return → unkeyed/keyed spawn. Cancels are applied even for cancel-only commands | reaffirmed | preserved_in=0003 §4.3 | Enqueue order (cancels→spawn) unchanged. The bootstrap init-dispatch move (0011 §3.4) does not touch per-command order |
| LG-0003-030 | owner | root-A1 | unnumbered norm (performance characteristic) | Enqueue-time reconciliation drains only ready `JoinSet` exits; it does not scan all live keyed receivers | reaffirmed | preserved_in=0003 §4.3 | Drain-only-ready-exits performance property preserved under the unified structure as well (consistent with §2.2 O(1) move) |
| LG-0003-031 | owner | root-SCHED,root-A1 | unnumbered norm | `AppInputs` owns the shared receiver and keyed merge; the wait path (`poll_next`) and batch path (`try_next_ready`) share the same shared-first priority | reaffirmed | preserved_in=0003 §4.4 | `AppInputs` mux ownership and shared-first priority shared by both paths unchanged |
| LG-0003-032 | owner | root-SCHED | INV-14 supplement | Shared-first bias is confined inside `AppInputs`. The top-level `select!` keeps Tokio's normal fairness across app input / frame tick / quit | reaffirmed | preserved_in=0003 §4.4 | Unbiased top-level select is also recorded as an 0011 §7 premise (informative); semantics unchanged |
| LG-0003-033 | owner | root-SCHED | negative space | The shared-first guarantee is local to the pull point. A shared message arriving after a keyed item was pulled does not retroactively precede it | reaffirmed | preserved_in=0003 §4.4 | Pull-point locality negative space unchanged |
| LG-0003-034 | owner | root-SCHED | negative space | A run of ready shared inputs can delay keyed output indefinitely (bounded fairness not guaranteed; accepted tradeoff) | reaffirmed | preserved_in=0003 §4.4 | Absence of bounded fairness unchanged |
| LG-0003-035 | owner | root-SCHED,root-A1 | threshold | The micro-batch window keeps the existing 100 microseconds | reaffirmed | preserved_in=0003 §4.4 | 100µs window preserved in the §2.1 steady loop (cap is 0006 INV-L12) |
| LG-0003-036 | owner | root-K51J46,root-A1 | unnumbered norm | `process_input_batch` returns `BatchOutcome::Quit` only for keyed `Quit`; otherwise `Continue` | reaffirmed | preserved_in=0003 §4.4 | `BatchOutcome::Quit` semantics unchanged. Authority from loop exit onward is 0011 §4 (0003 (as amended) cross-reference) |
| LG-0003-037 | owner | root-K51J46,root-A1 | unnumbered norm | `ReceiverEvent::Closed` does not call `update()`; keyed `Quit` also does not call `update()` and exits via the same shutdown path as `quit_rx` | reaffirmed | preserved_in=0003 §4.4 | Delivery contract for the two quit paths stays owned by 0003/0006; authority for the converged shutdown/termination is 0011 §4 (0003 (as amended)). Semantics unchanged |
| LG-0003-038 | owner | root-A1,root-SCHED | INV-10 / threshold (one-item) | A command returned by `update()` is dispatched before the next app input is pulled (one-item drain; prefetch forbidden) | reaffirmed | preserved_in=0003 §4.4 | Not affected — 0011 §1.2 cites one-item drain (INV-10) cite-only with no re-pin, and the 0011 §2.1 addition concerns only the subscription-dirtiness side. Owner and content of dispatch-before-next-pull unchanged |
| LG-0003-039 | owner | root-A1 | unnumbered norm | Keyed message delivery is recorded before `update()` (so a sender-closed empty receiver can release its id before a same-id retry) | reaffirmed | preserved_in=0003 §4.4 | Delivery-recording timing preserved |
| LG-0003-040 | owner | root-A1,root-D18 | unnumbered norm | `subscriptions_dirty` is set only when `update()` actually ran within the batch. `Closed` and keyed `Quit` alone do not mark dirty | reaffirmed | preserved_in=0003 §4.4 | 0011 §2.1 splits the dirty source into two lines (input batch ∪ quiescence of removal/replacement from steady-state re-evaluation), so this row's "only" is read as narrowed to exclusivity within the first line = in-batch triggers. 0003 §4.4 is preserved as owner of the batch-side recording rule (0011 §2.1 explicitly names it "the RFC 0003 §4.4 rule"); the second line's recording rule is owned by 0012. That `Closed`/keyed `Quit` alone do not mark dirty is unchanged, and termination quiescence does not mark dirty (explicit exclusion in 0011 §2.1) |
| LG-0003-041 | owner | root-A1 | unnumbered norm / threshold (at most once) | `keyed.try_next_ready()` polls the StreamMap at most once, treats `Poll::Pending` as "not ready", and never awaits | reaffirmed | preserved_in=0003 §4.4 | At-most-once poll and never-await preserved |
| LG-0003-042 | owner | root-A1 | unnumbered norm | `KeyedPoll`: `Quiescent` (reconciliation complete, no entries) / `PendingWithWakeSource` (waker registered). An empty manager never returns `PendingWithWakeSource` | reaffirmed | preserved_in=0003 §4.4 | Two-valued `KeyedPoll` contract preserved |
| LG-0003-043 | owner | root-A1,root-K51J46 | INV-16 table | `poll_next` result table: shared closed + keyed `Quiescent` → `Ready(None)` within the same poll (never returns an unwakeable `Pending` first) | reaffirmed | preserved_in=0003 §4.4 | INV-16 `poll_next` result table preserved |
| LG-0003-044 | owner | root-A1 | unnumbered norm | All existing constructors use `CommandCancellation::default()`; repeated `cancellable`/`cancellable_with` is last-call-wins while explicit cancels and directives are retained | reaffirmed | preserved_in=0003 §5.1 | Default metadata and last-call-wins preserved |
| LG-0003-045 | owner | root-A1,root-CMP | INV-12 | `map` preserves `key`, `policy`, `cancels`, and runtime directives in full | reaffirmed | preserved_in=0003 §5.1 | INV-12 `map` preservation rule unchanged |
| LG-0003-046 | owner | root-A1,root-CMP,leaf-I42 | INV-11 | `batch` folds directives and unions child `cancels`; child keys are ignored with a warning-level tracing event; the batch's own key applies to the batch task | reaffirmed | preserved_in=0003 §5.1 | INV-11 fold/union/warning unchanged (tracing is an always-on dependency) |
| LG-0003-047 | owner | root-A1,root-CMP | unnumbered norm | `timeout` preserves `key`/`policy`/`cancels`/directives and changes only the wrapped effect. `.timeout().cancellable()` and the reverse order are equivalent. `retry`/`retry_if` carry the default (empty) metadata from `Command::future` | reaffirmed | preserved_in=0003 §5.1 | Timeout metadata preservation and retry default metadata unchanged (for verification responsibility see LG-0003-075) |
| LG-0003-048 | owner | root-A1 | unnumbered norm (docs) | Public docs for `cancellable`/`cancellable_with` must state the batch boundary (not preserved on children) | reaffirmed | preserved_in=0003 §5.1 | Docs requirement (state the batch boundary) unchanged |
| LG-0003-049 | owner | root-A1 | unnumbered norm | Fixed cancellation application order: within one command, explicit cancels → its own keyed spawn (`batch(vec![cancel(id), work]).cancellable(id)` starts work after the old run is dropped) | reaffirmed | preserved_in=0003 §5.1 | Fixed explicit-cancels→self-spawn order preserved |
| LG-0003-050 | owner | root-A1 | unnumbered norm | The `entries` map is the single source of truth for deliverability (absent key = `Absent`) | reaffirmed | preserved_in=0003 §5.4 | Single source of truth preserved under the unified structure (no double bookkeeping is the spike gate) |
| LG-0003-051 | owner | root-A1 | INV-8 | A task exit mutates state only on `RunToken` match `(id, token)`; stale exits are ignored | reaffirmed | preserved_in=0003 §5.4 | INV-8 `RunToken` gating preserved |
| LG-0003-052 | owner | root-A1,root-K51J46 | unnumbered norm | `KeyedCommands` is not a terminating `Stream` (no entries is reusable quiescence). The only terminating application-input stream is `AppInputs` | reaffirmed | preserved_in=0003 §5.4 | Non-terminating quiescence contract preserved |
| LG-0003-053 | owner | root-A1 | unnumbered norm | The StreamMap remove/update/reinsert pattern is confined inside `KeyedCommands`; identity transitions do not churn the StreamMap | reaffirmed | preserved_in=0003 §5.4 | Internal confinement and no-churn observable properties unchanged. Note: the StreamMap mechanism itself may be replaced by the unified structure (informative) |
| LG-0003-054 | owner | root-A1 | INV-15 | `LifecycleDecision` is a closed decision type applied by a single exhaustive applier. Invalid logical-state/physical-action combinations are unrepresentable | reaffirmed | preserved_in=0003 §5.4 | INV-15 closed decision type preserved |
| LG-0003-055 | owner | root-A1 | unnumbered norm | Keyed runs use a `JoinSet<TaskExit>` separate from unkeyed `command_tasks`; spawn = fresh RunToken → private unbounded channel → task spawn + AbortHandle → entry insert | reaffirmed | preserved_in=0003 §5.5 | Spawn semantics (RunToken→private channel→AbortHandle→entry) preserved. Note: "separate JoinSet from unkeyed" is a mechanism description and may change under unified bookkeeping — observable contract unchanged |
| LG-0003-056 | owner | root-K51J46,root-A1,leaf-I42 | unnumbered norm | Task body: catches unwind and logs `"keyed command task panicked"`; Message forwarding stops on receiver drop; the Quit send result is ignored and the stream is not polled afterwards; returns `TaskExit{id,token}` on normal or panic completion | reaffirmed | preserved_in=0003 §5.5 | Task body unified under a single task_policy owner (mechanism). The log-emission requirement is 0003 §7.3, containment is 0011 INV-LC8, wording is mechanism (0011 §5.1) |
| LG-0003-057 | owner | root-A1,root-K51J46 | unnumbered norm | The sender is dropped when the wrapper exits → receiver closure = task-body termination. `JoinError::cancelled` from an aborted task carries no TaskExit and is ignored | reaffirmed | preserved_in=0003 §5.5 | Sender-drop=closure and ignoring cancelled `JoinError` preserved |
| LG-0003-058 | owner | root-K51J46,root-A1 | unnumbered norm | Shutdown: subscriptions/unkeyed abort as today + abort all keyed tasks + clear keyed entries. Adopting `JoinSet` also aborts on drop (avoids bare `JoinHandle` detach) | reaffirmed | preserved_in=0003 §5.6 | Shutdown procedure stays recorded as mechanism in 0003 §5.6; authority for the termination contract (two-stage postcondition) is 0011 §4/INV-LC5–7 (0011 §8 excluded claims). Semantics unchanged |
| LG-0003-059 | owner | root-A1 | INV-1 | INV-1: commands without cancellation metadata take the same shared `msg_tx`/`quit_tx` path as today | reaffirmed | preserved_in=0003 §6 | INV-1: observable behavior unchanged even under the unified execution mechanism (§2.2 owner table) |
| LG-0003-060 | owner | root-A1 | INV-2 | INV-2: at most one lifecycle-owned receiver per `CommandId`. Supersede/cancel drops the old receiver before the successor becomes deliverable | reaffirmed | preserved_in=0003 §6 | INV-2 preserved |
| LG-0003-061 | owner | root-A1 | INV-3 | INV-3: strict latest-wins — after a `CancelInFlight` supersede, the old run's message/quit does not affect the app even if already buffered | reaffirmed | preserved_in=0003 §6 | INV-3 strict latest-wins preserved |
| LG-0003-062 | owner | root-A1 | INV-4 | INV-4: explicit cancel is strict and idempotent — drops buffered output + aborts the running task; repeated application = one application | reaffirmed | preserved_in=0003 §6 | INV-4 preserved |
| LG-0003-063 | owner | root-A1,root-B7 | INV-5 | INV-5: `KeepInFlight` drops only the new stream. The dropped arrival's redraw directive and explicit cancel list have already been processed | reaffirmed | preserved_in=0003 §6 | INV-5 preserved (redraw/cancel separation consistent with 0002) |
| LG-0003-064 | owner | root-A1 | INV-6 | INV-6: finished-but-buffered output is still cancellable — the id stays occupied as long as buffered output remains, even after the task exited | reaffirmed | preserved_in=0003 §6 | INV-6 preserved |
| LG-0003-065 | owner | root-A1 | INV-7 | INV-7: a sender-closed empty receiver releases its id before a retry; a still-open stream stays occupied even after one delivery | reaffirmed | preserved_in=0003 §6 | INV-7 preserved |
| LG-0003-066 | owner | root-A1 | INV-8 | INV-8: stale task exits are inert — a `TaskExit` with mismatched `(id, token)` can neither remove nor mutate current state | reaffirmed | preserved_in=0003 §6 | INV-8 preserved |
| LG-0003-067 | owner | root-A1,root-K51J46 | INV-9 | INV-9: buffered quit is also cancellable — a cancelled/superseded keyed `Action::Quit` does not terminate the app; a live keyed `Quit` does | reaffirmed | preserved_in=0003 §6 | INV-9 preserved. Keyed-quit delivery contract is 0003/0006; loop exit from there onward is authoritative in 0011 §4.1 (0003 (as amended)) — semantics unchanged |
| LG-0003-068 | owner | root-A1,root-SCHED | INV-10 | INV-10: one-item drain — a command returned by one message is dispatched before the next shared/keyed item is pulled | reaffirmed | preserved_in=0003 §6 | Not affected — no Δ to INV-10 itself (0003 §6). 0011 §2.1 only cites INV-10, and the 0012 admission change concerns subscription re-evaluation timing and does not touch drain order |
| LG-0003-069 | owner | root-A1,root-CMP,leaf-I42 | INV-11 | INV-11: batch folding — cancels/directives are folded, child keys ignored with a warning, the batch-level key applies to the batch task | reaffirmed | preserved_in=0003 §6 | INV-11 preserved |
| LG-0003-070 | owner | root-A1,root-CMP | INV-12 | INV-12: map propagation — `Command::map` preserves key, policy, cancel list, and directives | reaffirmed | preserved_in=0003 §6 | INV-12 preserved |
| LG-0003-071 | owner | root-A1 | INV-13 | INV-13: bounded keyed bookkeeping — completed keyed tasks are reaped, closed empty entries are explicitly removed, and no auxiliary state survives after an entry disappears | reaffirmed | preserved_in=0003 §6 | INV-13 preserved (bounded-bookkeeping semantics unchanged even under unified bookkeeping) |
| LG-0003-072 | owner | root-SCHED | INV-14 (+ negative space) | INV-14: shared-first app-input scheduling (shared preferred at the same pull point). Bounded fairness between shared and keyed is not provided | reaffirmed | preserved_in=0003 §6 | INV-14 preserved — authority for shared-first pull is 0003 INV-14 (§2.1) |
| LG-0003-073 | owner | root-A1 | INV-15 | INV-15: closed lifecycle action space — invalid action combinations are unrepresentable | reaffirmed | preserved_in=0003 §6 | INV-15 preserved |
| LG-0003-074 | owner | root-A1,root-K51J46 | INV-16 | INV-16: pending is future-wakeable — every `Poll::Pending` from `AppInputs` has a waker registered; shared closed + `Quiescent` yields `Ready(None)` in the same poll | reaffirmed | preserved_in=0003 §6 | INV-16 preserved |
| LG-0003-075 | pointer | root-A1 | cross-RFC relation | Verification responsibility for the RFC 0004 §4.2 forward-integration contracts (cancel before timeout suppresses the timeout message; supersede during retry backoff suppresses the final message; `KeepInFlight` prevents spawning a second retrying command under an occupied id) lies with this RFC ("pinned here, not there") | reaffirmed | preserved_in=0003 §7.3; resolved_via=0004 §4.2 | Pointer row. Owner of the forward-integration contracts is 0004 §4.2; verification responsibility is 0003 ("pinned here, not there") — this placement is unchanged |
| LG-0003-076 | owner | root-A1 | enforcement provision | §7.1: white-box unit tests in `src/command.rs` (equality, default, last-call-wins, cancel, map, batch) | reaffirmed | preserved_in=0003 §7.1 | Enforcement provision unchanged |
| LG-0003-077 | owner | root-A1 | enforcement provision | §7.2: proptests call the production pure `lifecycle_transition` itself. Testing a parallel separate model or an independent state-effect table is forbidden; deterministic tests exercise every decision variant | reaffirmed | preserved_in=0003 §7.2 | Direct-call requirement on the production pure `lifecycle_transition` preserved under the unified structure as well (FSM unchanged) |
| LG-0003-078 | owner | root-A1 | enforcement provision | §7.3: runtime contract tests use deterministic synchronization (`Notify`, oneshot, paused time) and never sleep | reaffirmed | preserved_in=0003 §7.3 | Deterministic-synchronization provision unchanged |
| LG-0003-079 | owner | root-A1 | code seam provision | Implementation seams: `src/command/cancellation.rs` (new), `src/runtime/keyed_commands.rs` (new), `RuntimeCommandParts` extension, `AppInputs` extension | reaffirmed | preserved_in=0003 §9 | Seam-description row. Note: with the unified execution mechanism (§3.1) the seams may shift to task_policy etc. (mechanism only, informative) — no observable contract |
| LG-0003-080 | owner | root-A1 | precondition | Prerequisite refactors already on main: `AppInputs` mux ownership, `Command::into_runtime_parts()` lowering, `process_input_batch` rename, Effect leaf refactor (PR #137) | reaffirmed | preserved_in=0003 §9 | Factual record that the prerequisite refactors are on main; unchanged |
| LG-0004-001 | owner | root-A1 | unnumbered norm (meta) | Normative boundary: §1.3, §§2–4, Appendix A, and the "Contract" column of Appendix B are normative; Appendix C is non-normative and the T/R contracts take precedence | reaffirmed | preserved_in=0004 opening (Normative boundary) | - |
| LG-0004-002 | owner | root-A1,root-B7 | unnumbered norm (constraint 1) | timeout/retry are effect-local: no new `RuntimeDirectives` fields, `Action` variants, or runtime keyed state are added | reaffirmed | preserved_in=0004 §1.3 | Effect-locality preserved in the §2.2 owner table as well (runtime keyed state stays owned by 0003) |
| LG-0004-003 | owner | root-A1 | unnumbered norm (constraint 2) | Leaf identity: timeout wraps each leaf individually and preserves leaf count and order | reaffirmed | preserved_in=0004 §1.3 | - |
| LG-0004-004 | owner | root-A1 | unnumbered norm (constraint 2) | timeout must not preclude future per-leaf cancellation metadata (beyond the RFC 0003 top-level keyed model) | reaffirmed | preserved_in=0004 §1.3 | Reserved headroom for per-leaf cancellation remains unconsumed after 0003 (as amended) |
| LG-0004-005 | owner | root-A1 | unnumbered norm (constraint 3) | `batch([a.timeout(1s), b.timeout(2s)])` times out per child independently | reaffirmed | preserved_in=0004 §1.3/§2.3 | - |
| LG-0004-006 | owner | leaf-I43 | unnumbered norm (constraint 4) | Non-panicking constructors: invalid public input is handled via `Option`/builder (the crate's `panic = "warn"` policy) | reaffirmed | preserved_in=0004 §1.3 | - |
| LG-0004-007 | owner | leaf-I43 | unnumbered norm (constraint 5) | Retry support types are exported only from `tears::command`, not placed in the crate root or prelude | reaffirmed | preserved_in=0004 §1.3/§3.5 | - |
| LG-0004-008 | owner | leaf-I43 | unnumbered norm (constraint 6) | No new dependencies: implemented with existing `tokio`/`futures`/`tokio-stream` only | reaffirmed | preserved_in=0004 §1.3 | - |
| LG-0004-009 | owner | root-A1,root-D18 | unnumbered norm (constraint 7) | All T/R contracts must be pinned by deterministic tests (time-dependent tests use the Tokio paused clock) | reaffirmed | preserved_in=0004 §1.3/Appendix B | Not affected — 0012 §9 newly owns the non-time-axis effect-DI negative space, but states the time axis (rejection of a clock abstraction, paused-clock premise) remains with 0009 unchanged, and 0012 §1.2 also places timing out of scope. The basis for the deterministic-test obligation (constraint 7) is unchanged |
| LG-0004-010 | owner | root-A1,root-B7 | INV (T1) | T1: timeout changes only the effect and preserves runtime directives | reaffirmed | preserved_in=0004 §2.3/B.1 | - |
| LG-0004-011 | owner | root-A1 | INV (T2) | T2: the deadline starts at the leaf's first poll (not at the `.timeout()` call) | reaffirmed | preserved_in=0004 §2.2/B.1 | - |
| LG-0004-012 | owner | root-A1 | INV (T3) | T3: a single `.timeout()` call emits at most one timeout message across all wrapped leaves (`FnOnce` may consume move-only state) | reaffirmed | preserved_in=0004 §2.2/B.1 | - |
| LG-0004-013 | owner | root-A1 | INV (T4) | T4: completion before the deadline does not consume `on_timeout`; when completion and the deadline are observed simultaneously, termination wins | reaffirmed | preserved_in=0004 §2.2/B.1 | - |
| LG-0004-014 | owner | root-A1,root-K51J46 | INV (T5) | T5: messages before the deadline pass through; a `Quit` before the deadline is delivered, closes the leaf, and does not consume `on_timeout`; in the runtime a delivered `Quit` stops sibling delivery | reaffirmed | preserved_in=0004 §2.2/B.1 | The runtime-side "delivered `Quit` stops sibling delivery" is consistent with 0011 §4.1 controlled termination; unchanged |
| LG-0004-015 | owner | root-A1 | INV (T6) + negative space (no rollback) | T6: a timed-out inner stream is dropped and never polled again; rollback is not promised | reaffirmed | preserved_in=0004 §2.2/B.1 | - |
| LG-0004-016 | owner | root-A1,root-B7 | INV (T7) | T7: timeout on a stream-less command is inert and preserves `is_none()` (directives also preserved) | reaffirmed | preserved_in=0004 §2.3/B.1 | - |
| LG-0004-017 | owner | root-A1 | INV (T8) | T8: `map`/`batch` preserve leaf count, order, and placement semantics; child timeouts remain independent | reaffirmed | preserved_in=0004 §2.3/B.1 | - |
| LG-0004-018 | owner | root-A1 | INV (T9) + negative space (f1/f2 choice unspecified) | T9: nested timeouts emit only from the earlier one on a given leaf; simultaneous deadlines emit exactly one (which one is unspecified); different leaves of a batch may emit from different calls | reaffirmed | preserved_in=0004 §2.4/B.1 | - |
| LG-0004-019 | owner | root-A1 | INV (T10) + negative space | T10: deadline/item tie ordering is unspecified and tests admit both outcomes (`Item` = `Message` and `Quit`); termination (`None`) always wins | reaffirmed | preserved_in=0004 §2.2/§2.4/B.1 | - |
| LG-0004-020 | owner | root-A1 | INV (T10) | T10 (bounded progress): a poll with the deadline ready passes through at most one simultaneously-ready inner item, with the terminal transition no later than the next poll; polls after terminal output return `None` | reaffirmed | preserved_in=0004 §2.2/B.1 | - |
| LG-0004-021 | owner | root-A1 | threshold (within T10) | T10: `Duration::ZERO` does not panic | reaffirmed | preserved_in=0004 B.1 | - |
| LG-0004-022 | owner | root-A1 | negative space | The exact ordering between a ready inner item and the deadline is otherwise outside the contract | reaffirmed | preserved_in=0004 §2.2 end | - |
| LG-0004-023 | owner | root-A1 | unnumbered norm (semantic distinction) | timeout is an overall deadline, not per-item inactivity (`tokio_stream::StreamExt::timeout`) | reaffirmed | preserved_in=0004 §2.1/§5.1 | - |
| LG-0004-024 | owner | leaf-I43 | unnumbered norm | The `on_timeout` bound exposes no public `Sync` requirement (same as `Command::map`) | reaffirmed | preserved_in=0004 §2.5 | The implementation example (Arc<Mutex<Option<F>>>) remains non-normative in Appendix C |
| LG-0004-025 | owner | root-A1 | INV (R1) + threshold (1-based/inclusive) | R1: `max_attempts` includes the first execution; when processing completes, exactly one final message is emitted | reaffirmed | preserved_in=0004 §3.4/B.2 | - |
| LG-0004-026 | owner | root-A1 | INV (R2) | R2: failure on the final attempt is always `Exhausted`; `should_retry` is not called on that attempt | reaffirmed | preserved_in=0004 §3.4/B.2 | - |
| LG-0004-027 | owner | root-A1 | INV (R3) | R3: `StoppedByPredicate` occurs only while attempts remain | reaffirmed | preserved_in=0004 §3.4/B.2 | - |
| LG-0004-028 | owner | root-A1 | INV (R4) | R4: `None` means no delay; `Fixed` waits the same delay after each retryable failure, before the next attempt | reaffirmed | preserved_in=0004 §3.4/B.2 | - |
| LG-0004-029 | owner | root-A1 | INV (R5) | R5: `RetryError` retains `last_error` and exposes it via `Display` and `Error::source()` | reaffirmed | preserved_in=0004 A.2/B.2 | - |
| LG-0004-030 | owner | root-A1 | INV (R6) | R6: `new` defaults to no backoff; the fixed-backoff builder preserves `max_attempts`; zero attempts is unrepresentable via `NonZeroUsize` | reaffirmed | preserved_in=0004 §3.3/A.3/B.2 | - |
| LG-0004-031 | owner | root-A1 | INV (R7) | R7: a retry command is observationally identical to a single-leaf future command under `map`/`without_redraw`/`batch` | reaffirmed | preserved_in=0004 §4.1/B.2 | - |
| LG-0004-032 | owner | root-A1 | INV (R8) | R8: `should_retry` can hold local state via its `FnMut` bound | reaffirmed | preserved_in=0004 §3.5/B.2 | - |
| LG-0004-033 | owner | leaf-I43 | unnumbered norm + threshold | `RetryContext::attempt()` is 1-based; any future 0-based exponent conversion stays inside helpers and does not change the public context | reaffirmed | preserved_in=0004 §3.4 | - |
| LG-0004-034 | owner | root-A1 | unnumbered norm | If the task is aborted before processing completes, no final message is produced | reaffirmed | preserved_in=0004 §3.4 | Preserved under the 0011 INV-LC5/LC6 abort requirements as well (dual grounding with the 0003 private-receiver drop unchanged) |
| LG-0004-035 | owner | leaf-I43 | unnumbered norm (must) | Public rustdoc must warn that the operation may run up to `policy.max_attempts()` times (non-idempotent side effects may occur multiple times) | reaffirmed | preserved_in=0004 §3.2 | - |
| LG-0004-036 | owner | leaf-I43 | unnumbered norm (must) | Public rustdoc must state the argument reading order: configuration → operation → message conversion | reaffirmed | preserved_in=0004 §3.5 | - |
| LG-0004-037 | owner | leaf-I43 | unnumbered norm (API shape) | The argument order `policy, operation, f` is fixed as the first deliberate deviation from the effectful-payload-first convention | reaffirmed | preserved_in=0004 §3.5 | - |
| LG-0004-038 | owner | leaf-I43 | unnumbered norm (namespace contract) | `Command::retry` permanently occupies the inherent constructor name; future modifiers must use different names such as `with_retry`/`retry_with` | reaffirmed | preserved_in=0004 §3.5 | - |
| LG-0004-039 | owner | leaf-I43 | unnumbered norm (compatibility) | Adding retry types to the prelude/root later is non-breaking; removing them after addition is breaking | reaffirmed | preserved_in=0004 §3.5/§5.2 | - |
| LG-0004-040 | owner | leaf-I43 | INV (Appendix A as a whole) | Appendix A type definitions, derives, `#[non_exhaustive]` placement, and signatures are normative | reaffirmed | preserved_in=0004 A.1/A.2 | - |
| LG-0004-041 | owner | leaf-I43 | unnumbered norm | Return-self builders get `#[must_use]` with a message; the other candidates get bare `#[must_use]` | reaffirmed | preserved_in=0004 A.2 | - |
| LG-0004-042 | owner | leaf-I43 | unnumbered norm | `RetryContext::new` is `pub(crate)`: retry contexts cannot be constructed outside the crate | reaffirmed | preserved_in=0004 A.2 | - |
| LG-0004-043 | owner | leaf-I43 | unnumbered norm | `RetryPolicy::new(NonZeroUsize)` is infallible and the sole constructor; no fallible `try_new` is provided | reaffirmed | preserved_in=0004 A.3 | - |
| LG-0004-044 | owner | leaf-I43 | unnumbered norm (must) | `RetryBackoff` is `#[non_exhaustive]` at both the enum level and the `Fixed` variant level; rustdoc must state that downstream matches require a wildcard arm | reaffirmed | preserved_in=0004 A.3 | - |
| LG-0004-045 | owner | leaf-I43 | unnumbered norm (compatibility) | The `const fn` builder constrains future backoff fields to values without drop glue; representations requiring `Drop` need a separate API | reaffirmed | preserved_in=0004 A.3 | - |
| LG-0004-046 | owner | leaf-I43 | unnumbered norm (compatibility) | Future fields must preserve the derives: `RetryBackoff` is `Clone+Debug+Eq+PartialEq`, `RetryContext` additionally `Copy`; raw `f64` cannot be stored in an `Eq` field | reaffirmed | preserved_in=0004 A.3 | - |
| LG-0004-047 | owner | root-A1 | negative space (feature boundary) | Per-attempt deadlines are `tokio::time::timeout` inside the operation; the outer `.timeout()` is an overall deadline over the whole retry (per-attempt timeout is not provided by this API) | reaffirmed | preserved_in=0004 §3.4 | - |
| LG-0004-048 | pointer | root-A1 | unnumbered norm (forward integration) | `timeout` preserves cancellation metadata the same way it preserves directives (after 0003 is implemented) | reaffirmed | preserved_in=0004 §4.2; resolved_via=LG-0003-047 | The metadata preservation contract of 0003 §5.1 is unchanged under the 0003 (as amended) as well |
| LG-0004-049 | pointer | root-A1 | unnumbered norm (forward integration) | Both `.timeout(...).cancellable(id)` and `.cancellable(id).timeout(...)` preserve the key | reaffirmed | preserved_in=0004 §4.2; resolved_via=LG-0003-047 | Key preservation for both orderings is explicitly pinned in 0003 §5.1 |
| LG-0004-050 | pointer | root-A1 | unnumbered norm (forward integration) | Retry constructors carry `Command::future`'s default cancellation metadata | reaffirmed | preserved_in=0004 §4.2; resolved_via=LG-0003-047 | The default (empty) metadata clause for retry/retry_if exists on the owner side |
| LG-0004-051 | pointer | root-A1 | unnumbered norm (forward integration) | For a keyed command, cancel/supersede suppresses the pending timeout message and the retry final message via private-receiver drop | reaffirmed | preserved_in=0004 §4.2; resolved_via=LG-0003-002 | Non-negotiable B (no delivery of any output after cancel/supersede) is the owner contract; the private-receiver drop mechanism is also unchanged |
| LG-0004-052 | pointer | root-A1 | unnumbered norm (forward integration) | `Command::cancel(id).timeout(...)` remains stream-less; the timeout is inert and the explicit cancellation remains | reaffirmed | preserved_in=0004 §4.2; resolved_via=LG-0003-047 | The owner of stream-less inertness is T7; survival of explicit cancels is the 0003 §5.1 preservation clause |
| LG-0004-053 | pointer | root-A1 | unnumbered norm (delegation of the verification obligation) | RFC 0003 runtime tests must additionally cover cancellation before the timeout, supersession during retry backoff, and suppression of a second retry command under the same key via `KeepInFlight` | reaffirmed | preserved_in=0004 §4.2; resolved_via=0003 §7.3 | The verification obligation is received in 0003 §7.3 as "pinned here, not there" (matching LG-0003-075 on both sides) |
| LG-0004-054 | owner | root-A1 | negative space (out of scope) | Keyed cancellation, debounce, throttle, clock DI, observability metrics, and stream re-subscription retry are out of scope for this RFC | reaffirmed | preserved_in=0004 §1.4/§5.2 | Ownership of debounce/throttle stays with the 0003 keyed lifecycle (unchanged under the reference architecture (§2) as well) |
| LG-0004-055 | owner | root-A1,root-K51J46 | unnumbered norm (verification) | Runtime smoke test: timeout/retry final messages reach `update`, and pending timeouts/backoffs do not detach on shutdown | reaffirmed | preserved_in=0004 B.3 | Unifying unkeyed bookkeeping under JoinSet<TaskExit> is mechanism (informative); shutdown abort behavior is unchanged (0011 INV-LC5–LC7) |
| LG-0005-001 | owner | root-CMP | unnumbered norm | Non-negotiable A: hash collisions must not make unequal logical keys equal (hash is bucket selection only; full equality is structural value comparison) | reaffirmed | preserved_in=0005 §1.4 | - |
| LG-0005-002 | owner | root-CMP,root-D18 | unnumbered norm | Non-negotiable B: concrete source types are a framework-owned namespace. A source returns only its associated `Key` and cannot choose or substitute the namespace | reaffirmed | preserved_in=0005 §1.4 | Unaffected — namespace ownership and ID construction via Key are identity contract. 0012 §1.2 explicitly places identity (structural ID/scope/dedup) out of scope, and the Δ does not touch the 0005 text (confirmed by diff stat) |
| LG-0005-003 | owner | root-CMP | unnumbered norm | Non-negotiable C: the Rust type of a logical key is part of identity. Type erasure must not introduce cross-type equality | reaffirmed | preserved_in=0005 §1.4 | - |
| LG-0005-004 | pointer | root-A1,root-CMP | unnumbered norm | Non-negotiable D: existing unscoped command semantics unchanged | reaffirmed | preserved_in=0005 §1.4; resolved_via=0003 §6 | pointer: the owner of existing unscoped command semantics is 0003 (equivalence definition imported from LG-0003-012). The architecture does not touch identity/scope |
| LG-0005-005 | owner | root-CMP | unnumbered norm | Non-negotiable E: scoping is structural and hierarchical. Segments are not flattened into a precomputed hash; segment type, value, boundary, and order all participate in full equality | reaffirmed | preserved_in=0005 §1.4 | - |
| LG-0005-006 | owner | root-CMP | unnumbered norm | Non-negotiable F: a scope is an identity qualifier, not bulk teardown (it does not cancel, does not create a runtime registry entry, and does not provide select-all-descendants operations) | reaffirmed | preserved_in=0005 §1.4 | - |
| LG-0005-007 | pointer | root-A1,root-CMP | unnumbered norm | Non-negotiable G: `Command::batch` granularity stays explicit. Scoping does not solve per-effect batching | reaffirmed | preserved_in=0005 §1.4; resolved_via=LG-0003-069 | pointer: the owner of batch granularity is 0003 INV-11 (§2.2 keeps 0003 INV-2 through INV-16 unchanged) |
| LG-0005-008 | owner | root-CMP | unnumbered norm | Non-negotiable H: correctness is not conditioned on benchmark results. Never fall back to collision-unsafe equality because of benchmark results | reaffirmed | preserved_in=0005 §1.4 | Includes the §7 norm that benchmarks are non-conditioning. Correctness-first is maintained under the reference architecture (§2) as well |
| LG-0005-009 | owner | root-CMP | unnumbered norm | Subscription full identity = ScopePath + SourceType + LogicalKeyType + LogicalKeyValue; Command full identity = ScopePath + LogicalKeyType + LogicalKeyValue | reaffirmed | preserved_in=0005 §2.1 | §2.2 owner table: "identity: StructuralKey/ScopePath unchanged" |
| LG-0005-010 | owner | root-CMP | unnumbered norm | All IDs built via the Phase A API, and all commands not passing through `scoped`, have an empty ScopePath | reaffirmed | preserved_in=0005 §2.1 | - |
| LG-0005-011 | owner | root-CMP | unnumbered norm | Value comparison in local equality is performed only after the key types (TypeId) match | reaffirmed | preserved_in=0005 §2.2 | - |
| LG-0005-012 | owner | root-CMP | negative space | The framework does not compensate for Eq/Hash law violations in user-defined key/scope types — same trust boundary as HashMap keys | reaffirmed | preserved_in=0005 §2.2 | - |
| LG-0005-013 | owner | root-CMP | unnumbered norm | Scope segment equality = both concrete type and value match; paths are compared element-wise in outer-to-inner order | reaffirmed | preserved_in=0005 §2.3 | - |
| LG-0005-014 | owner | root-CMP | unnumbered norm + negative space | Path internal representation is free, but replacing segment boundaries with a digest is forbidden; path representation and scope introspection are not public | reaffirmed | preserved_in=0005 §2.3 | - |
| LG-0005-015 | pointer | root-A1,root-CMP | unnumbered norm | Unscoped command IDs remain root-global lifecycle slots. Reusing the same full ID is an intentional opt-in to the existing replacement / `KeepInFlight` / explicit-cancel behavior | reaffirmed | preserved_in=0005 §2.4; resolved_via=0003 §4.2 | pointer: the owner of replacement/KeepInFlight/explicit-cancel is 0003 (FSM unchanged under the reference architecture (§2)) |
| LG-0005-016 | owner | root-CMP,root-D18 | negative space | Subscriptions do not provide fan-out via duplicate declarations (equal full IDs are first-wins plus a diagnostic) | reaffirmed | preserved_in=0005 §2.4 | Unaffected — no fan-out (first-wins for equal full IDs) is unchanged; 0012 §1.2 explicitly states dedup belongs to RFC 0005. 0012's admission rules cover only the execution timing after the desired set is determined and do not touch the dedup decision |
| LG-0005-017 | owner | root-CMP | unnumbered norm (should) | Shared subscriptions should be hoisted to a common parent and mapped once; independent child instances should use separate scopes (recommended norm) | reaffirmed | preserved_in=0005 §2.4 | The should-level (recommended) norm is also unchanged |
| LG-0005-018 | owner | root-CMP,root-D18 | unnumbered norm | Public constructors of `SubscriptionId` are removed. ID construction is done by `Subscription::new<Source>` from `source.key()` (namespace cannot be forged) | reaffirmed | preserved_in=0005 §3.1 | Unaffected — privatizing the SubscriptionId construction path is identity surface. 0012 keeps public signatures unchanged (header) and identity out of scope (§1.2), and does not mention the construction API |
| LG-0005-019 | owner | root-D18,root-CMP | threshold/unnumbered norm | `SubscriptionSource::Key` bounds = `Eq + Hash + Send + Sync + 'static` | reaffirmed | preserved_in=0005 §3.1 | Unaffected — Key's trait bounds are a type-boundary contract. 0012 constrains only the execution side of sources (start/poll/stop) and does not touch SubscriptionSource's associated type bounds |
| LG-0005-020 | owner | root-D18,root-CMP | unnumbered norm | `key()` returns an owned value. `Key: Clone` is not required. One Key type per source implementation | reaffirmed | preserved_in=0005 §3.1 | Unaffected — owned return of key(), no Clone requirement, and one Key type per source are identity API shape, covered by 0012 §1.2's out-of-scope statement. No such text in the Δ |
| LG-0005-021 | owner | root-D18,root-CMP | unnumbered norm | `key()` must be stable across evaluations: for the same logical identity, even a new source value created by re-evaluation must return an equal key. Must be stated in rustdoc | reaffirmed | preserved_in=0005 §3.1 | Unaffected — the cross-evaluation key stability norm stays in 0005 §3.1. 0012 INV-SE6 (purity: same state → same identities per RFC 0005) merely reinforces the same premise from the subscription-declaration side and does not move ownership of the stability contract |
| LG-0005-022 | owner | root-D18 | negative space | An unstable key silently aborts+respawns the subscription every dirty frame with no diagnostic — not enforceable by the type system and not detected | reaffirmed | preserved_in=0005 §3.1 | Unaffected — the undetected, no-diagnostic negative space is unchanged. Under 0012 §4, respawn admission moves across quiescence to the next frame pass, and since quiescence marks dirty (INV-SE5), unstable-key churn can persist independent of messages; but this row's contract itself — "not enforceable by types, not detected" — remains as in 0005 §3.1 |
| LG-0005-023 | owner | root-CMP | INV-7 (refined in §3.1) | `SubscriptionId` keeps `Send + Sync + UnwindSafe + RefUnwindSafe`. No unwind-safety bounds are added to key types | reaffirmed | preserved_in=0005 §3.1 | The reference architecture (§2)'s task_policy unification is on the task-body side and does not affect SubscriptionId's unwind-safety bounds |
| LG-0005-024 | owner | root-CMP,leaf-I43 | unnumbered norm | `SubscriptionId` remains re-exported at the same canonical crate-root path as before | reaffirmed | preserved_in=0005 §3.1 | - |
| LG-0005-025 | owner | root-D18,root-CMP | unnumbered norm + negative space | `dyn SubscriptionSource` requires specifying `Key = K` (breaking) | reaffirmed | preserved_in=0005 §3.1 | Unaffected — the required Key = K specification for dyn is a type-surface change already fulfilled in 0.10.0. 0012 does not change identity or public signatures (§1.2, header) |
| LG-0005-026 | owner | root-D18 | unnumbered norm (should) | Built-in sources' `Key` is part of the public trait surface. Opaque tokens are a last resort | reaffirmed | preserved_in=0005 §3.1 | Unaffected — the public-surface policy for built-in Keys (opaque tokens as last resort) is on the identity side. 0012 §6.1's injection contract requires only template conformance and does not touch Key publicity |
| LG-0005-027 | owner | root-CMP | unnumbered norm | `SubscriptionId` is `Clone` and not `Copy` (intentional breaking change in 0.10.0) | reaffirmed | preserved_in=0005 §3.2 | - |
| LG-0005-028 | owner | root-CMP,leaf-I43 | negative space | No promises of `Arc` usage, one allocation per constructor, pointer-sized storage, or O(1) clone | reaffirmed | preserved_in=0005 §3.2 | - |
| LG-0005-029 | owner | root-CMP | unnumbered norm | `SubscriptionId::of::<T>(u64)` is removed with no deprecated bridge | reaffirmed | preserved_in=0005 §3.3 | - |
| LG-0005-030 | owner | root-D18,root-CMP | unnumbered norm | Built-in sources must migrate from `DefaultHasher::finish()` to the original logical components. Returning digest values as `type Key = u64` is forbidden | reaffirmed | preserved_in=0005 §3.3 | Unaffected — the digest-value-Key prohibition and migration to logical components are identity-construction norms. 0012 does not touch how identifiers are constructed at all (§1.2 states it only uses 0005's vocabulary) |
| LG-0005-031 | owner | root-CMP | negative space + unnumbered norm | `Debug` is not required on keys. Identity is defined by Eq/Hash, not Debug | reaffirmed | preserved_in=0005 §3.4 | - |
| LG-0005-032 | owner | root-CMP,leaf-I42 | INV-11 (refined in §3.5) | Ignoring a duplicate full ID must be observable via a warning-level tracing event (target `tears::subscription`). Key values are not exposed by default | reaffirmed | preserved_in=0005 §3.5 | The §2.2 owner table's keyed-panic log attribution (RFC 0003 §7.3) covers task-body panic logs, which is distinct from this row's manager-side duplicate warning (target tears::subscription) — no conflict |
| LG-0005-033 | owner | leaf-I42,root-CMP | negative space | Diagnostic wording, fields, event counts, and rate-limiting are not stable API | reaffirmed | preserved_in=0005 §3.5 | - |
| LG-0005-034 | owner | root-CMP,leaf-I42 | unnumbered norm | Unequal keys that merely hash-collide are not duplicates, and the collision must not be a reason to warn | reaffirmed | preserved_in=0005 §3.5 | - |
| LG-0005-035 | owner | root-CMP | unnumbered norm | `scoped<Scope>` bounds = `Eq + Hash + Send + Sync + 'static`. The methods live on `Subscription`/`Command` | reaffirmed | preserved_in=0005 §4.1 | - |
| LG-0005-036 | owner | root-CMP | negative space | No public `ScopeId`/`ScopePath` or common `LifecycleId` is added | reaffirmed | preserved_in=0005 §4.1 | - |
| LG-0005-037 | owner | root-CMP | unnumbered norm | `Subscription::scoped(scope)` prepends one segment to the current scope path and preserves source namespace, local key, spawner, and message type | reaffirmed | preserved_in=0005 §4.2 | - |
| LG-0005-038 | owner | root-CMP | unnumbered norm | If a future reducer API automates scope application, it must still preserve this RFC's identity laws | reaffirmed | preserved_in=0005 §4.2 | The identity-law constraint on future reducer automation is preserved under §2.3 as well (single aggregate adapter, no second runtime) |
| LG-0005-039 | owner | root-CMP,root-B7 | unnumbered norm | `Command::scoped` does not change the effect stream, message mapping, redraw directive, timeout/retry wrappers, cancellation policy, or output | reaffirmed | preserved_in=0005 §4.3 | Non-modification of the redraw directive is consistent with the reference architecture (§2)'s unchanged OR-fold (owned by 0002) |
| LG-0005-040 | owner | root-CMP | unnumbered norm | `Command::none().scoped(scope)` is lifecycle-inert | reaffirmed | preserved_in=0005 §4.3 | - |
| LG-0005-041 | owner | root-CMP | unnumbered norm | `scoped` is a boundary operation on the lifecycle metadata present at call time, not a persistent mode. A `cancellable` after `scoped` installs a root-global spawn key (last-call-wins); explicit cancels already scoped earlier remain scoped | reaffirmed | preserved_in=0005 §4.3 | - |
| LG-0005-042 | owner | root-CMP | unnumbered norm with enforcement designation | rustdoc warnings about the ordering footgun are required in all 3 places — `Command::scoped`/`cancellable`/`cancellable_with` — with examples of both orderings | reaffirmed | preserved_in=0005 §4.3 | - |
| LG-0005-043 | owner | root-CMP | negative space | No diagnostic is emitted when `cancellable` comes after `scoped` (intentional: the runtime cannot distinguish legitimate composition from a mistake) | reaffirmed | preserved_in=0005 §4.3 | - |
| LG-0005-044 | pointer | root-A1,root-CMP | negative space | Even when scoped, child spawn keys are not preserved across `Command::batch` (scoped explicit cancels are folded and preserved) | reaffirmed | preserved_in=0005 §4.4; resolved_via=LG-0003-069 | pointer: the owner of the batch boundary is 0003 INV-11 |
| LG-0005-045 | owner | root-CMP,root-A1 | unnumbered norm | `scoped` on a batch result scopes the folded explicit cancels and the top-level key present at that point | reaffirmed | preserved_in=0005 §4.4 | - |
| LG-0005-046 | pointer | root-A1,root-CMP | deferred | Per-effect keyed child preservation is deferred work belonging to RFC 0003 and is not implicitly added by this RFC | reaffirmed | preserved_in=0005 §4.4; resolved_via=LG-0003-008 | pointer (deferred): per-effect keyed child preservation is 0003's future-work ownership. The reference architecture (§2) does not implicitly add it — deferral preserved |
| LG-0005-047 | owner | root-CMP | deferred | The 6 open questions for `cancel_scope` (segment/prefix selection, scan vs secondary index, ordering within the same update, buffered output handling, subscription participation and declarative re-evaluation, stale teardown observation) are deferred to a separate RFC | reaffirmed | preserved_in=0005 §4.5 | The deferral declaration of the 6 cancel_scope questions is preserved as-is = reaffirmation chosen (delegation not adopted; consistent with the C-16 delegation (§5.3)) |
| LG-0005-048 | owner | root-CMP | deferred | Structural paths are forward-compatible with prefix teardown, but that API/behavior is not pre-approved | reaffirmed | preserved_in=0005 §4.5 | Same as above (deferral preserved = reaffirmed, delegation not adopted). Forward-compat of structural paths is consistent with §2.3's removal projection, and no API/behavior is pre-approved |
| LG-0005-049 | owner | root-CMP | deferred | TCA-parity forEach reducer composition takes prefix teardown as a prerequisite requirement, and teardown should be scheduled as that prerequisite | reaffirmed | preserved_in=0005 §4.5 | Same as above (deferral preserved = reaffirmed, delegation not adopted). §2.3's aggregate adapter direction does not change the scheduling constraint that teardown is a prerequisite for forEach reducers |
| LG-0005-050 | owner | root-CMP | negative space | Manual scoping does not make incorrect composition unrepresentable: omitting a scope, reusing the same scope value, and attaching a local key after the boundary all compile and can alias lifecycle slots | reaffirmed | preserved_in=0005 §4.6 | - |
| LG-0005-051 | owner | root-CMP,root-A1 | negative space | Command aliasing is indistinguishable from RFC 0003's intentional root-global shared slots; replacement, suppression, and mutual cancellation can be silent (cannot warn) | reaffirmed | preserved_in=0005 §4.6 | - |
| LG-0005-052 | owner | root-CMP | deferred | Construction-time correctness (automatic application of child instance scopes) is deferred to a future composition layer that owns the boundary. Phase B is a manual primitive, not a final guarantee | reaffirmed | preserved_in=0005 §4.6 | Deferral (construction-time correctness to a future composition layer) preserved. §2.3 only plans an adapter on 0011's Application boundary and does not pre-empt type-level enforcement |
| LG-0005-053 | owner | root-CMP,root-D18 | unnumbered norm | The blanket `impl<A: SubscriptionSource> From<A> for Subscription` is preserved | reaffirmed | preserved_in=0005 §5.1 | Unaffected — preserving the blanket From impl is declaration API shape. 0012 §2 only stipulates the execution boundary ("declaration is effect-free") and does not touch the existence of the conversion impl |
| LG-0005-054 | owner | root-CMP,root-D18 | unnumbered norm | All built-in sources, examples, doctests, benchmarks, and tests must migrate within the same 0.10.0 change. The changelog must document before/after for custom sources and the loss of `Copy` | reaffirmed | preserved_in=0005 §5.1 | Unaffected — migration within the same 0.10.0 change plus the changelog documentation is a fulfilled delivery norm. 0012's Changed covers 0.11.0 admission/trigger behavior and does not retroactively affect this migration record |
| LG-0005-055 | owner | root-CMP | negative space | Phase B is additive. Unscoped keeps Phase A / RFC 0003 behavior with an empty scope path. No automatic scoping by closure type, message mapper, vector position, or memory address | reaffirmed | preserved_in=0005 §5.2 | - |
| LG-0005-056 | owner | root-CMP | unnumbered norm | Deferring Phase B requires no additional breaking changes | reaffirmed | preserved_in=0005 §5.2 | - |
| LG-0005-057 | owner | root-CMP | unnumbered norm (process) | Tracking must state phase status explicitly (e.g. `Partially Implemented (Phase A)`) | reaffirmed | preserved_in=0005 §5.3 | process norm (phase status notation) |
| LG-0005-058 | owner | root-CMP | INV-1 | INV-1: subscription IDs are equal only when source type, logical-key type, logical-key value, and scope path are all equal | reaffirmed | preserved_in=0005 §6.1 | - |
| LG-0005-059 | owner | root-CMP | INV-2 | INV-2: unequal logical keys remain unequal even if they feed identical bytes or constants to every `Hasher` | reaffirmed | preserved_in=0005 §6.1 | - |
| LG-0005-060 | owner | root-CMP,root-D18 | INV-3 | INV-3: `Subscription::new<Source>` builds the ID from `TypeId::of::<Source>()` and `Source::Key`. Equal keys on different concrete source types are distinct IDs by construction | reaffirmed | preserved_in=0005 §6.1 | Unaffected — INV-3's ID construction rule (TypeId + Key) is the core of identity. 0012 §1.2 places structural IDs out of scope, and §13's references cover only INV-12/INV-13/§3.5, leaving INV-3 unchanged |
| LG-0005-061 | owner | root-CMP | INV-4 | INV-4: keys of different concrete Rust types are distinct IDs even if they look the same | reaffirmed | preserved_in=0005 §6.1 | - |
| LG-0005-062 | owner | root-CMP | INV-5 | INV-5: under the Eq/Hash laws, equal IDs have equal hashes. Hash equality does not imply ID equality | reaffirmed | preserved_in=0005 §6.1 | - |
| LG-0005-063 | owner | root-CMP | INV-6 | INV-6: IDs do not borrow non-`'static` source state (owned erasure) | reaffirmed | preserved_in=0005 §6.1 | - |
| LG-0005-064 | owner | root-CMP | INV-7 | INV-7: `CommandId` and `SubscriptionId` remain separate public types even if they share internal machinery. `SubscriptionId` keeps `Send + Sync + UnwindSafe + RefUnwindSafe`; only `Copy` is removed | reaffirmed | preserved_in=0005 §6.1 | Even with the reference architecture (§2)'s unified entry bookkeeping, the public type separation (CommandId/SubscriptionId) and the bounds are unchanged |
| LG-0005-065 | owner | root-CMP,root-D18 | INV-8 | INV-8: two unequal desired IDs that hash-collide can be run/stopped/restarted independently | reaffirmed | preserved_in=0005 §6.2 | Unaffected — collision-independent reconciliation (independent decisions on which IDs to run/stop/restart) is unchanged. 0012 §4's uniform barrier only constrains admission execution timing uniformly across all subscriptions without undermining the substance of INV-8, and INV-SE2 makes non-interference with continuing subscriptions explicit. The authoritative source for the re-evaluation phase remains 0011 §2 |
| LG-0005-066 | owner | root-CMP,root-D18 | INV-9 | INV-9: hash-colliding subscriptions each keep their own spawn closure and message mapping | reaffirmed | preserved_in=0005 §6.2 | Unaffected — per-subscription retention of spawn closure/message mapping under collision is an identity-association contract. Under 0012 INV-SE4 as well, superseded generations' spawners are discarded un-invoked, which does not touch the association for live subscriptions |
| LG-0005-067 | owner | root-CMP,root-D18 | INV-10 | INV-10: equal full IDs within the same desired set keep the first declaration in input order (first-wins, deterministic) | reaffirmed | preserved_in=0005 §6.2 | Unaffected — first-wins dedup is explicitly left owned by 0005 per 0012 §1.2. 0012's admission rules newly constrain only the execution timing for the post-dedup desired set |
| LG-0005-068 | owner | root-CMP,leaf-I42 | INV-11 | INV-11: duplicate ignoring is observable at warning level and does not require `Debug` on key values | reaffirmed | preserved_in=0005 §6.2 | Same as above |
| LG-0005-069 | owner | root-CMP,root-D18 | INV-12 | INV-12: structural ID comparison does not invoke the stream spawner of a discarded duplicate and does not regenerate the stream of a continuing subscription (lazy spawn) | reaffirmed | preserved_in=0005 §6.2 | Unaffected — the never-invoked guarantee (duplicate discard, no regeneration for continuing IDs) is left in place by 0012 §2, INV-SE1, and the §11 excluded claims, which explicitly cite it as owned by RFC 0005 INV-12. What 0012 newly owns is only the invoked-at-admission timing side; the division of the start boundary (0005 = never-invoked / 0012 = admission timing) is explicit in the text |
| LG-0005-070 | pointer | root-D18,root-CMP | INV-13 | INV-13: restart contract unchanged — a subscription that has finished and remains desired under the same full ID restarts on the next re-evaluation | reaffirmed | preserved_in=0005 §6.2; resolved_via=0012 §4.3 | resolved_via is 0012 §4.3. INV-13 semantics are explicitly preserved by 0012 §1.2/§4.3 (a finished task is by definition quiesced, so a pure restart is not delayed). Only when the same re-evaluation also stops running tasks does admission move to the next frame pass after quiescence (INV-SE3/SE5), and via the second dirty source (0011 §2.1) a restart can occur on a frame pass without a message — the wording "restarts on the next re-evaluation" holds on either path. The judgment that hash-skip restart suppression is an implementation bug with the contract side unchanged (separate track) is also maintained |
| LG-0005-071 | owner | root-CMP | INV-14 | INV-14: scope segments participate in equality by both type and value, and the framework's scope node kind is distinguished from all user local-key shapes | reaffirmed | preserved_in=0005 §6.3 | - |
| LG-0005-072 | owner | root-CMP | INV-15 | INV-15: reversing scope application reverses the path. Full identity differs only when the reversed structural segment sequence differs from the original | reaffirmed | preserved_in=0005 §6.3 | - |
| LG-0005-073 | owner | root-CMP,root-A1 | INV-16 | INV-16: equal local IDs under unequal scope paths do not alias in either the subscription or the command registry | reaffirmed | preserved_in=0005 §6.3 | - |
| LG-0005-074 | owner | root-CMP | INV-17 | INV-17: `Subscription::map` / `Command::map` preserve full identity | reaffirmed | preserved_in=0005 §6.3 | - |
| LG-0005-075 | owner | root-CMP | INV-18 | INV-18: `Command::scoped` qualifies both the keyed spawn ID and all explicit cancel IDs present at the call boundary | reaffirmed | preserved_in=0005 §6.3 | - |
| LG-0005-076 | owner | root-CMP,root-A1 | INV-19 | INV-19: cancel/replace of a full ID under one scope does not affect an equal local ID under a different scope | reaffirmed | preserved_in=0005 §6.3 | - |
| LG-0005-077 | pointer | root-A1,root-CMP | INV-20 | INV-20: scoping does not bypass RFC 0003's batch boundary | reaffirmed | preserved_in=0005 §6.3; resolved_via=LG-0003-069 | pointer: the owner of the batch boundary is 0003 INV-11 (unchanged under the reference architecture (§2)) |
| LG-0005-078 | owner | root-CMP | INV-21 | INV-21: dropping the return value of `scoped` or omitting a scoped command does not issue prefix cancellation beyond the existing per-ID rules | reaffirmed | preserved_in=0005 §6.3 | - |
| LG-0005-079 | owner | root-D18 | unnumbered norm (test policy) | Tests involving async tasks use deterministic synchronization, not sleeps | reaffirmed | preserved_in=0005 §6.4 | Unaffected — the deterministic-synchronization-not-sleep test policy is unchanged, and 0012's enforcement has the same shape (INV-SE3 drives the quiescence gap deterministically with a single-threaded test executor). Ownership of the policy remains 0005 §6.4 |
| LG-0005-080 | owner | root-CMP | enforcement/threshold | Phase A benchmark migration must construct IDs through the same framework path as real sources. Record before/after time and, where possible, allocation counts | reaffirmed | preserved_in=0005 §7 | - |
| LG-0005-081 | owner | root-CMP | unnumbered norm | When allocation dominates, permissible follow-ups must preserve all identity invariants and stay internal-only. Reintroducing a pre-hashed equality surrogate is not permitted | reaffirmed | preserved_in=0005 §7 | - |
| LG-0005-082 | owner | root-CMP | threshold/enforcement | Phase B must add a scoped steady-state benchmark before implementation completion is declared. Path representation stays private | reaffirmed | preserved_in=0005 §7 | §7 is synchronized with the fulfilled state — the requirement that Phase B add a scoped benchmark before declaring implementation complete is recorded as landed via `subscription_reconcile_steady_scoped` (benches/subscription.rs; comparing an unscoped baseline / single boundary / nested path). Path representation also remains private (the 0/1/4 segments are bench-internal). The row's threshold/enforcement content and verdict are unchanged |
| LG-0005-083 | owner | root-CMP | negative space (scope exclusion) | Non-goals: adding `Reducer`/`Store`/lens/optics, deciding automatic derivation of reducer child scopes, merge/fan-out of duplicate subscription mappings, and unifying the command/subscription managers | reaffirmed | preserved_in=0005 §10 | reducer/Store remains delegated to a future RFC (§2.3 does not pre-approve it either) |
| LG-0005-084 | owner | root-A1,root-D18,root-SCHED | negative space (scope exclusion) | Non-goals: changes to command occupancy, output suppression, or cancellation policy; subscription restart backoff / safety fuse; adding runtime channel bounds / backpressure / load-control | reaffirmed | preserved_in=0005 §10 | Unaffected — the restart backoff/safety fuse non-goal is preserved. 0012 §8 newly fixes the delegation frame (rate policy is owned by a future opt-in RFC; admitting before quiescence is not possible), which is consistent with and does not contradict 0005 §10's scope exclusion. The owners of occupancy (0003) and channel bounds/backpressure (0006) are also unchanged per 0012 §1.2 |
| LG-0005-085 | owner | root-CMP | negative space (scope exclusion) | Non-goals: exposing scope path or erased-key internals | reaffirmed | preserved_in=0005 §10 | - |
| LG-0006-001 | owner | root-SCHED,leaf-K55 | unnumbered norm (R1) | R1: buffers of app-facing runtime-owned channels (shared + each keyed) can be bounded per-channel via configuration. Total buffer = `app_channel_capacity + m × keyed_channel_capacity` | reaffirmed | preserved_in=0006 §1.2/§4.1 | The behavioral anchor test for R1 is attributed to 0003 INV-7, not 0005 (corrected in RFC 0006's current text; the test exists and the contract content is unchanged — id release for a sender-closed empty receiver is owned by 0003 INV-7). R1's own requirement (backlog-independent delivery of unkeyed quit) and this row's verdict are unchanged |
| LG-0006-002 | owner | root-K51J46,root-SCHED | negative space | R1 negative: the dedicated quit channel is outside R1 — it is never bounded and no occupancy claim is made (the R4 unbounded exception) | reaffirmed | preserved_in=0006 §1.2/§4.1 | The quit channel's unbounded exception (R4) remains owned by 0006 and unchanged even with 0011 §4 as the termination authority |
| LG-0006-003 | owner | leaf-K55,root-SCHED | negative space | R1 negative: not a bound on total pending-work memory — a blocked producer holds 1 in-flight message outside the channel, and the producer count (including m) is an application-owned contract input | reaffirmed | preserved_in=0006 §1.2/§4.5 | - |
| LG-0006-004 | pointer | leaf-K55,root-CMP | unnumbered norm | An "active CommandId" includes finished-but-undrained runs (counted in m until drained and the id released) | reaffirmed | preserved_in=0006 §1.2; resolved_via=LG-0003-064 | The owner of occupancy = deliverability is 0003 INV-6 (the release side is INV-7). The anchor test exists in src/runtime/keyed_commands.rs |
| LG-0006-005 | owner | root-SCHED | unnumbered norm (R2) | R2: a configured bound does not silently drop messages by default — the default overload response is backpressure | reaffirmed | preserved_in=0006 §1.2 | Enforced via INV-L2 |
| LG-0006-006 | owner | root-SCHED,leaf-I42 | unnumbered norm (R3) | R3: input-to-screen latency must be observable, and under bounded configuration it is bounded by queue capacity (premised on a bounded number of producers) | reaffirmed | preserved_in=0006 §1.2 | Enforced via INV-L3 |
| LG-0006-007 | owner | root-K51J46 | unnumbered norm (R4) | R4: a quit signal that has entered the dedicated quit channel is delivered with latency independent of the app backlog — the dedicated channel and the always-armed select branch must be maintained | reaffirmed | preserved_in=0006 §1.2 | 0011 §4 is the authority for the termination phase, but the 0006-owned quit delivery contract (dedicated channel + always-armed branch) is semantically unchanged |
| LG-0006-008 | owner | root-K51J46,root-A1 | unnumbered norm | Unkeyed `Action::Quit` (including user-initiated quit returned by update) is always sent directly to the dedicated quit channel and never passes through the shared channel (both modes) | reaffirmed | preserved_in=0006 §4.2 | Direct dispatch of unkeyed quit to the dedicated channel matches §2.1 steady loop C. Consistent with 0011 §4 controlled termination |
| LG-0006-009 | owner | root-K51J46,root-SCHED | negative space | R4 negative: the time for a task to "reach" a quit that sits behind a preceding `Action::Message` in the same stream is outside R4 — in bounded mode the preceding send may wait on shared capacity | reaffirmed | preserved_in=0006 §1.2/§4.2 | - |
| LG-0006-010 | pointer | root-A1,root-SCHED | unnumbered norm (R5) | R5: RFC 0003's cancellation/ordering invariants (cancel-before-delivery, INV-14, INV-10) may only be preserved or explicitly amended | reaffirmed | preserved_in=0006 §1.2; resolved_via=0003 §5 | R5's "preserve or explicitly amend only" branch is consistent with the 0003 (as amended) |
| LG-0006-011 | owner | root-K51J46,root-A1 | negative space | Pin of the unkeyed quit asymmetry: for unkeyed `[Message, Quit]`, the quit may be observed first (it travels a separate channel) — pre-existing behavior, outside all ordering claims of this RFC | reaffirmed | preserved_in=0006 §1.2/§4.2 | The unkeyed [Message, Quit] asymmetry pin remains outside ordering claims. 0011 §4 may be cross-referenced |
| LG-0006-012 | owner | root-SCHED,leaf-I41 | unnumbered norm (R6) | R6: default (unconfigured) behavior of existing apps does not change in 0.10.0 | reaffirmed | preserved_in=0006 §1.2 | Enforced via INV-L6 |
| LG-0006-013 | owner | root-SCHED,leaf-I41 | unnumbered norm | Release gate verdict: this RFC adds no breaking change to 0.10.0. Defaults are unbounded, the sender never waits, and there is no message loss before shutdown | reaffirmed | preserved_in=0006 §3.1/§3.3 | - |
| LG-0006-014 | owner | leaf-I41 | unnumbered norm | Opt-in is additive: new `Runtime::with_config(flags, config)`; `Runtime::new` is unchanged and equivalent to the default configuration | reaffirmed | preserved_in=0006 §3.2 | - |
| LG-0006-015 | owner | root-SCHED,leaf-I41 | negative space | negative: the current public API does not document unbounded buffering as a guarantee — offering a bounded mode does not contradict the published contract | reaffirmed | preserved_in=0006 §3.2 | - |
| LG-0006-016 | owner | leaf-I41,root-SCHED | unnumbered norm | `app_channel_capacity: Option<NonZeroUsize>` — `None` (default) = unbounded shared channel, `Some(n)` = bound | reaffirmed | preserved_in=0006 §4.1 | - |
| LG-0006-017 | owner | leaf-I41,root-SCHED | unnumbered norm | `keyed_channel_capacity: Option<NonZeroUsize>` — likewise for each keyed private channel | reaffirmed | preserved_in=0006 §4.1 | - |
| LG-0006-018 | owner | root-A1 | INV-L12 (defined in §4.1) | The counted unit of `batch_max_messages` is "pulled input" — the initiating input counts as the first, and `ReceiverEvent::Closed` is also counted. A batch ends at whichever of count / 100µs / exhaustion / quit comes first | reaffirmed | preserved_in=0006 §4.1/INV-L12 | The counted unit (pulled input, `Closed` counted) matches the batch window of §2.1 steady loop A |
| LG-0006-019 | owner | root-SCHED,root-D18 | unnumbered norm + negative space | Backpressure onto a subscription propagates by the forwarding task awaiting capacity and not polling the source stream. Buffering/shedding for un-pausable upstreams is source-level policy, outside the runtime's responsibility | reaffirmed | preserved_in=0006 §4.2 | Not affected — 0012 §2 (Poll) only cites the forwarder pace and the 0006 §4.2 backpressure (no polling while awaiting capacity), stating "No delivery guarantee is added or restated here", and its excluded claims explicitly drop delivery invariants. It also does not touch the attribution of buffer/shed for un-pausable upstreams as source-level policy |
| LG-0006-020 | owner | root-SCHED,root-D18 | unnumbered norm | Terminal input is treated the same as subscriptions — under overload, earlier inputs queue on the terminal side | reaffirmed | preserved_in=0006 §4.2 | Not affected — the subscription-equivalent treatment of terminal input stays as in 0006 §4.2 with no relevant change in the delta. 0012 §4.1 cites the terminal as an example of the stolen-input hazard but adds no queueing/delivery contract |
| LG-0006-021 | owner | root-SCHED | unnumbered norm | Command tasks (keyed/unkeyed) await capacity before the next send. No trait/type changes | reaffirmed | preserved_in=0006 §4.2/§3.2 | The await-capacity-before-send semantics are unchanged even under unified entry bookkeeping (model (b)) |
| LG-0006-022 | owner | root-SCHED,root-D18 | INV-L2 + negative space | Delivery in bounded mode is lossless until shutdown — the runtime does not drop messages to relieve pressure. Lossy strategies are a source-level layer | reaffirmed | preserved_in=0006 §4.2/INV-L2 | Not affected — 0012 §1.2 states that delivery (including losslessness and ordering) is owned by 0006 and only cites it. No delta touches the content of INV-L2 or the exclusion clause for the 0003-side overrides |
| LG-0006-023 | owner | root-A1 | unnumbered norm + negative space | Per-source in-order delivery is unchanged, scoped per source class (unkeyed/subscription use the shared FIFO; each keyed run has a private FIFO). RFC 0003 does not spell out the FIFO invariant | reaffirmed | preserved_in=0006 §4.3 | The two delivery classes (shared FIFO + the set of keyed private FIFOs) are preserved in §2.1/§2 |
| LG-0006-024 | owner | root-SCHED | negative space | INV-14 shared-first pull is unchanged, and bounded mode adds no fairness guarantee whatsoever — keyed starvation (F4) is not bounded by capacity | reaffirmed | preserved_in=0006 §4.3/§4.7 | Shared-first pull is canonically owned by 0003 INV-14 (§2.1). The no-added-fairness position is also pinned as invariant INV-L15 |
| LG-0006-025 | owner | root-SCHED,root-K51J46 | unnumbered norm | A keyed producer blocked on a full private channel is aborted by cancellation exactly like a running one | reaffirmed | preserved_in=0006 §4.3 | - |
| LG-0006-026 | owner | root-SCHED,root-K51J46 | negative space | Pin of the weakened practical cancellation effectiveness in bounded mode: a cancel input awaiting admission is not "ready" inside the channel, so keyed output may be delivered first — not an INV-14 violation, but the practical reach of RFC 0003's "prompt cancellation" is weaker than under the unbounded default | reaffirmed | preserved_in=0006 INV-L14 | Now numbered (verdict per §3.2). The 0006 §4.3 body only adds "Pinned as INV-L14"; semantics unchanged |
| LG-0006-027 | pointer | root-B7,root-D18 | unnumbered norm | Redraw suppression (RFC 0002) and subscription re-evaluation gating are unchanged | reaffirmed | preserved_in=0006 §4.3; resolved_via=LG-0002-011 | The 0006 §4.3 text (bounded mode leaves redraw suppression and re-evaluation gating unchanged and operates downstream of input delivery) is intact and the affirmation stands, but "semantics unchanged" no longer holds unqualified — 0011 §2.1/0012 §4 split the gating trigger into two paths, and message-independent re-evaluation was added as a behavior change (Changed in 0012). The 0002 redraw gate and the unconditionality of `subscriptions_dirty` (resolved_via target LG-0002-011) are preserved, and since the second dirty source is independent of input delivery, the §4.3 claim itself still holds |
| LG-0006-028 | owner | root-K51J46,root-SCHED | unnumbered norm | shutdown: bounded channels are closed the same way as unbounded ones; blocked senders observe closed and their tasks terminate | reaffirmed | preserved_in=0006 §4.3 | The current unkeyed implementation is non-conformant with bounded-blocked-close — a conformance fix toward the break is required (§3.1). The immediate-close/unbounded-close break is not an owner contract but a unified policy in non-guaranteed territory (executed in a separate cell) |
| LG-0006-029 | owner | leaf-I42 | unnumbered norm | Initial-implementation load observability is `tracing` only (a profiling-hook counter is a future additive) | reaffirmed | preserved_in=0006 §4.4 | - |
| LG-0006-030 | owner | leaf-I42 | INV-L13 (schema) | batch event: target `tears::runtime::load`, level `trace`, once per completed micro-batch (no emission for a quit-terminated batch). Fields: `pulled`/`updated`/`shared_pending`. Subsumes and relocates the existing "processed message batch" trace | reaffirmed | preserved_in=0006 §4.4/INV-L13 | - |
| LG-0006-031 | owner | leaf-I42 | INV-L13 (schema) + negative space | capacity-wait event: bounded-only, level `debug`, once at acceptance time for each send that waited for capacity. Fields `channel`/`wait_us`. Per-send emission is deliberate — collapsing it later into a periodic aggregate is breaking; adding an aggregate alongside is additive | reaffirmed | preserved_in=0006 §4.4/INV-L13 | - |
| LG-0006-032 | owner | leaf-I42 | INV-L13 (schema) | producer gauges: level `debug`, emitted on every counted-value change. Fields `seq`/`subscriptions`/`unkeyed_commands`/`keyed_commands`/`blocked`. `seq` is a per-runtime monotone u64; current value = the value of the event with the max `seq` | redesigned | changed_to=0006 §4.4/INV-L13 | The gauge schema changed by schema amendment — a `runtime_id` field is added (process-local opaque u64, never reused, magnitude carries no meaning, structural requirement to fail before wrap); `seq` is a strict increase per `runtime_id` (initial value, contiguity, and counter ownership shape are not pinned — both per-instance and process-global conform); current value is partition-then-max-seq. The prior premise that the schema semantics were unchanged no longer holds, hence redesign. Preserved: level `debug`, emission on every counted-value change, the existing 4 counts, and the option B max-seq consumption principle |
| LG-0006-033 | owner | leaf-I42 | negative space | gauge negative: **gauge-event arrival order is not a contract** — consumers must order by `seq` | redesigned | changed_to=0006 §4.4 | A partition constraint was added to the consumer ordering rule — "order by seq" becomes "order by seq within the same runtime_id" (cross-instance seq comparison is explicitly meaningless). The negative-space point itself (arrival order is not a contract) is unchanged (preserved part), but the normative text's semantics were extended to multi-instance, hence redesign |
| LG-0006-034 | owner | leaf-I42 | unnumbered norm + negative space | Gauge reentrancy safety: a nested gauge change caused by a subscriber is delivered as an event with its own `seq` and does not nest inside tracing dispatch | redesigned | changed_to=0006 §4.4 | The reentrant nested gauge event gained an "under the same runtime_id" constraint (per-instance seq ordering). Delivery as an own-`seq` event and no nesting inside tracing dispatch are preserved. Redesign due to the extension of the normative text |
| LG-0006-035 | owner | leaf-I42 | negative space | negative: a per-keyed-channel occupancy gauge is deliberately outside the minimal schema. Adding one is additive | reaffirmed | preserved_in=0006 §4.4 | - |
| LG-0006-036 | owner | leaf-I42 | enforcement contract | Definition of done for observability: verify each event at the narrowest deterministic layer with a tracing subscriber plus value asserts | reaffirmed | preserved_in=0006 §4.4 (DoD) | The verification methodology (each event verified at the narrowest deterministic layer with a tracing subscriber + value asserts) is unchanged, while the gauge-layer DoD row gained an instance half (distinct ids across 2 runtimes, partition-then-max-seq reconstruction, fail on omit/sharing/reuse). Affirmed because the methodology's semantics are unchanged (the added assertions accompany the redesigns of 032/062) |
| LG-0006-037 | owner | root-SCHED,leaf-K55 | unnumbered norm (OQ3 rejected) | No admission limit: the producer-count premise remains application-owned; the runtime does not enforce it but makes it observable | reaffirmed | preserved_in=0006 §4.5 | The 0012 §4 quiescence barrier is a lifecycle ordering constraint, not a count-based admission limit or a load policy, so the substance of the OQ3 rejection (no admission limit; producer-count premise application-owned; runtime makes it observable via gauges without enforcing) is unchanged. 0012 §2 likewise cites the same-shaped treatment of making blocking-source stalls observable via gauges, which is consistent. However, this note's INV-L8-based rationale must be reread following the resolution of the LG-0006-057 counterexample (INV-L8 rescope) |
| LG-0006-038 | owner | root-SCHED | unnumbered norm (OQ3 rejected) | keyed channels are bounded **per command**; no shared permit pool | reaffirmed | preserved_in=0006 §4.5 | Via INV-L9 |
| LG-0006-039 | owner | leaf-K55,root-SCHED | unnumbered norm (doc) | Obligation to document the anti-pattern: spawning a command per message converts, under bounded overload, into a blocked-producer backlog and is not bounded by any capacity | reaffirmed | preserved_in=0006 §4.5 | The documentation obligation lands in the 0007 document (via OQ1) — it remains a 0006-owned norm, not a delegation |
| LG-0006-040 | owner | root-K51J46 | unnumbered norm (OQ7 rejected) | Rejection of rerouting keyed `Action::Quit` (both shapes): it stays in the private channel — delivery order is itself the cancellation semantics | reaffirmed | preserved_in=0006 §4.6 | The keyed quit staying in the private channel (OQ7 rejection) remains unchanged as a 0006/0003-owned delivery contract even under the 0011 §4 termination canon (anchored on 0003 INV-9) |
| LG-0006-041 | owner | root-K51J46,root-SCHED | negative space | Bounded mode preserves the contract (routing/order) but not the numbers: delivering a buffered keyed quit is legal even while a producer backlog remains — keyed-quit latency never becomes an acceptance bound nor a reroute detector | reaffirmed | preserved_in=0006 §4.6/§5.1 | - |
| LG-0006-042 | owner | root-SCHED | unnumbered norm (OQ6 rejected) | No fairness policy (and none will be added under this contract); INV-14 is maintained in both modes — a keyed-delivery latency bound is not a goal. Reopening requires a new RFC amending INV-14 | reaffirmed | preserved_in=0006 §4.7/INV-L15 | INV-L15 canonizes the OQ6 resolution in invariant form (numbering only; semantics unchanged). The reopening condition (a new RFC amending INV-14) is also preserved |
| LG-0006-043 | owner | root-SCHED | negative space | Explicitly forgone cost: keyed liveness under sustained shared readiness (both modes). A deliberate trade of cancellation correctness > keyed liveness | reaffirmed | preserved_in=0006 §4.7 | - |
| LG-0006-044 | owner | root-SCHED | negative space | keyed-to-keyed arbitration is unspecified — `StreamMap` poll merely returns one ready element from a randomized start position | reaffirmed | preserved_in=0006 §4.7 | - |
| LG-0006-045 | owner | root-SCHED | negative space | The keyed-probe scenario is permanently measurement-only | reaffirmed | preserved_in=0006 §4.7/§5.1 | - |
| LG-0006-046 | owner | root-SCHED,root-A1 | negative space | `batch_max_messages` is not a fairness knob and never will be | reaffirmed | preserved_in=0006 §4.7 | - |
| LG-0006-047 | owner | root-SCHED | unnumbered norm (doc) | doc guidance: keying buys cancellability at the cost of delivery deferral under load; route liveness-critical output to unkeyed commands | reaffirmed | preserved_in=0006 §4.6/§4.7 | - |
| LG-0006-048 | owner | root-SCHED,leaf-K55 | INV-L1 | INV-L1: with `app_channel_capacity = n` the shared channel holds at most n messages. Conceptual total = shared capacity + blocked producers + Σ(per-command keyed capacity); the quit channel is excluded | reaffirmed | preserved_in=0006 INV-L1 | - |
| LG-0006-049 | owner | root-SCHED | INV-L2 | INV-L2: bounded mode does not drop messages to relieve backpressure. RFC 0003's cancellation drop and shutdown discard remain as exceptions | reaffirmed | preserved_in=0006 INV-L2 | - |
| LG-0006-050 | owner | root-SCHED | INV-L3 + negative space | INV-L3: drain-side waiting for a shared accepted message is bounded by the drain time of one full queue. Admission waiting is at most (k+1) drain-equivalents via FIFO permits; end-to-end ≤ (k+1)+n — holds only when k is bounded. No bound of this kind exists for keyed delivery | reaffirmed | preserved_in=0006 INV-L3 | - |
| LG-0006-051 | owner | root-K51J46 | INV-L4 + threshold | INV-L4: a quit in the dedicated quit channel is delivered with latency independent of the app-channel backlog (statistical formulation (b)). acceptance: 4 scenarios, ≥200 trials each, reference machine, **quit→delivered p99 ≤ 1 ms**, both modes. quit→delivered is the delivery instant per tracing event timestamps | reaffirmed | preserved_in=0006 INV-L4 | The termination-phase canon is 0011 §4; the statistical formulation and threshold of quit→delivered are 0006-owned and unchanged |
| LG-0006-052 | owner | root-K51J46 | unnumbered norm | INV-L4 fallback: deterministic quit priority (option (a)) is a recorded fallback — adopting it is an amendment of this invariant | reaffirmed | preserved_in=0006 §5 (INV-L4) | - |
| LG-0006-053 | owner | root-K51J46 | negative space | INV-L4 scope negative: `quit_keyed_backlog_50k` is out of scope. Quit requests inside command streams are also out of scope | reaffirmed | preserved_in=0006 §5 (INV-L4) | - |
| LG-0006-054 | pointer | root-A1,root-SCHED | INV-L5 | INV-L5: **all invariants** of RFC 0003 hold unchanged in bounded mode as well (import of all 0003 invariants). However, INV-L10/L11 are not carried by INV-L5 alone (because 0003 leaves them unwritten) | reaffirmed | preserved_in=0006 INV-L5; resolved_via=0003 §5 | The point that INV-L10/L11 are not carried by INV-L5 (unwritten in 0003) is also preserved |
| LG-0006-055 | owner | root-SCHED,leaf-I41 | INV-L6 + negative space | INV-L6: the default configuration (all `None`) reproduces current behavior — the check is structural (the `None` path constructs the same `mpsc::unbounded_channel` as today). Substituting a bounded channel with huge capacity is not acceptable | reaffirmed | preserved_in=0006 INV-L6 | Structural-only is made explicit in the §5 exception list — this row's check means were identical from the start; semantics unchanged |
| LG-0006-056 | owner | root-A1,root-SCHED | INV-L7 | INV-L7: the event loop task never performs an awaiting `send` into a channel it drains itself (not type-system enforced) | reaffirmed | preserved_in=0006 INV-L7 | - |
| LG-0006-057 | owner | root-SCHED,leaf-K55 | INV-L8 | INV-L8: the runtime does not block/reject/defer producer admission — every subscription subject to reconciliation is started, and commands are dispatched synchronously and unconditionally | redesigned | changed_to=0006 INV-L8 | The counterexample of grade (i) (the unqualified no-defer clause vs. the defer of the 0012 §4 barrier) is resolved by amendment — INV-L8 is rescoped to prohibit additional block/reject/defer by load control (configuration, channel occupancy, producer-count pressure) for producers that are admissible under the other owner contracts. Preserved: admissibility = 0005; admission timing = 0012 §4 (added delay by a future rate policy sits in the 0012 §8 delegation slot); immediate command dispatch = 0003 INV-10. Redesign under the 1 ID = 1 verdict rule because the semantics were narrowed from the unqualified form. Resolved within the root-SCHED narrow scope |
| LG-0006-058 | owner | root-SCHED | INV-L9 | INV-L9: keyed backpressure is isolated per command — a global permit pool could satisfy INV-L1 while violating this, so the isolation pin is this invariant | reaffirmed | preserved_in=0006 INV-L9 | - |
| LG-0006-059 | owner | root-K51J46 | INV-L10 | INV-L10: a keyed run's `Action::Quit` is sent to the private channel and delivered only after that run's earlier outputs — a keyed quit does not overtake its own run's output. Unkeyed commands are out of scope | reaffirmed | preserved_in=0006 INV-L10 | Even under the 0011 §4 termination canon, the keyed quit's within-run ordering contract remains 0006-owned and unchanged (the basis on which 0003 INV-9 holds) |
| LG-0006-060 | pointer | root-K51J46,root-SCHED | INV-L11 + negative space | INV-L11: at every `AppInputs` pull point, ready shared input is delivered before ready keyed quit — no quit-specific bypass for keyed quit. Only inputs ready inside the channel are in scope (bounded admission window) | reaffirmed | preserved_in=0006 INV-L11; resolved_via=LG-0003-072 | The quit-application form of 0003 INV-14. The verification requirement on both pull paths is also preserved |
| LG-0006-061 | owner | root-A1 | INV-L12 | INV-L12: with `Some(n)` one micro-batch pulls at most n inputs (the initiating input counts as the first; `Closed` is also counted). Composes with the existing termination conditions (100µs/exhaustion/quit) | reaffirmed | preserved_in=0006 INV-L12 | - |
| LG-0006-062 | owner | leaf-I42 | INV-L13 | INV-L13: events are emitted per the §4.4 schema and field values keep their meaning. Rename/drop/repurpose is an RFC amendment, not an implementation detail | redesigned | changed_to=0006 INV-L13 | INV-L13 itself changed — `runtime_id` joins the schema, and the field-meaning list and enforcement check column were extended. The norm "rename/drop/repurpose is an RFC amendment, not an implementation detail" is preserved, and this change went through exactly that amendment path (a demonstration of the norm). Redesign, updating the old note that assumed an unchanged schema |
| LG-0006-063 | owner | root-SCHED | enforcement contract | Each invariant has a bench regression scenario or a unit/runtime-layer/integration test | reaffirmed | preserved_in=0006 §5 | The structural-only exceptions (INV-L6/L7/L8/L14/L15) are made explicit with rationale — each invariant's enforcement means remain as already fixed per row; semantics unchanged |
| LG-0006-064 | pointer | root-SCHED,leaf-I41 | unnumbered norm | Reproducibility rule 1: the parameters of the bounded run are pinned by RFC 0007 before the implementation PR | reaffirmed | preserved_in=0006 §5.1; resolved_via=0007 §3.1/§5.1 | The pin location for the bounded run parameters is 0007 |
| LG-0006-065 | owner | root-SCHED | unnumbered norm + threshold (measurement basis) | Reproducibility rule 2: every latency acceptance criterion is scoped only to the reference machine (Apple M1 Max 10 cores, rustc 1.97.0; baseline measured 2026-07-17). Replacing the machine requires re-measuring the unbounded baseline plus an amendment to this section | reaffirmed | preserved_in=0006 §5.1 | - |
| LG-0006-066 | owner | root-SCHED | negative space | Reproducibility rule 3: CI gates no latency criterion whatsoever | reaffirmed | preserved_in=0006 §5.1 | - |
| LG-0006-067 | owner | root-SCHED,leaf-K55 | unnumbered norm (measurement contract) | Depth definition: `produced - processed`. Observable bound = `capacity + concurrent producers` | reaffirmed | preserved_in=0006 §5.1 | - |
| LG-0006-068 | owner | root-SCHED | unnumbered norm (scenario contract) | `keyed_isolation` scenario: admission only, delivery excluded; a regression check, not a proof of pool absence | reaffirmed | preserved_in=0006 §5.1 | - |
| LG-0006-069 | owner | root-K51J46,root-SCHED | negative space + threshold | F7's full-drain p50 (≈1.30s @ ~50k) is a regression check **for the unbounded default only** | reaffirmed | preserved_in=0006 §5.1 | - |
| LG-0006-070 | owner | root-SCHED,root-K51J46 | threshold (recorded acceptance) | Measured acceptance results (2026-07-24, reference machine): depth 1025; no drops in any burst/overload case; update p99 flattens at 28.0ms; quit rows p99 0.61/0.91/0.92/0.65ms; `keyed_isolation`: 17 admits per key, 0 keyed deliveries, concurrent shared occupancy 1025 | reaffirmed | preserved_in=0006 §5.1 | The recorded acceptance values are preserved in §5.1. Status read Draft while the amendment was in progress, but the record's semantics are unchanged |
| LG-0006-071 | owner | root-SCHED,root-K51J46 | negative space | Measurement-only rows: bounded `keyed_overload` keyed p50 13.0s, `quit_keyed_bounded` p50 13.07s — recorded, not gated | reaffirmed | preserved_in=0006 §5.1 | - |
| LG-0006-072 | owner | root-D18,leaf-I41 | negative space (rejection) | OQ5 rejection content: restart-rate control stays a subscription-level policy — `RuntimeConfig` has no restart-rate field and reserves no name for one | reaffirmed | preserved_in=0006 §6 | Not affected — 0012 §8, in fixing the restart-rate delegation slot, cites and preserves the OQ5 rejection (rate is subscription-level; `RuntimeConfig` has no field; 0007 §4) as a standing position. The added slot (a rate policy may delay admission past quiescence but never advance it) does not contradict the rejection and reserves no field |
| LG-0006-073 | pointer | leaf-I41 | unnumbered norm (delegation) | OQ1/OQ2: recommended default values are in RFC 0007 §3.1 | reaffirmed | preserved_in=0006 §6; resolved_via=0007 §3.1 | The canon for the recommended default values is 0007 §3.1 |
| LG-0006-074 | owner | root-CMP,root-D18 | unnumbered norm | `BenchSubscriptionManager::new` is bench-internals gated, `#[doc(hidden)]`, and outside semver | reaffirmed | preserved_in=0006 §3.2 | Not affected — 0012 changes the manager's admission timing, but no delta touches the 0006 §3.2 declaration that `BenchSubscriptionManager::new` is bench-internals gated, `#[doc(hidden)]`, and outside semver |
| LG-0007-001 | pointer | root-SCHED | unnumbered norm (meta, subordination) | Subordination declaration: this RFC decides only matters delegated by RFC 0006; load-control semantics, the backpressure contract, and all INV-L are incorporated by reference and no clause of this RFC can revise them; on conflict RFC 0006 wins and the conflict is a defect of this RFC | reaffirmed | preserved_in=0007 Decision scope; resolved_via=0006 §4/§5 (INV-L1–L15) | The 0006 delta's addition of INV-L14/L15 does not change the subordination relation (reference consistency only) |
| LG-0007-002 | pointer | root-SCHED,root-A1,root-K51J46 | unnumbered norm (boundary) | Clauses outside the delegation (the semantics of the 3 controls, RFC 0006 §4.1/INV-L12, and the acceptance *criterion*) remain in RFC 0006 | reaffirmed | preserved_in=0007 §1; resolved_via=0006 §4.1/§5.1 | - |
| LG-0007-003 | owner | leaf-I41 | unnumbered norm (API shape) | `RuntimeConfig` has private fields and derives `Clone, Copy, Debug, Eq, PartialEq`; its fields are frame_rate + the 3 controls (`Option<NonZeroUsize>`) only | redesigned | changed_to=0007 §2.1 | Adoption of the leaf-I41 verdict — the `Copy` derive on `RuntimeConfig` is removed in 0.11.0 (Clone/Debug/Eq/PartialEq kept; `FrameRate` keeps `Copy`). §2.1 Derives/misuse-guard is re-derived (4 deliverables enumerated). Redesign under the 1 ID = 1 verdict rule because this row's "Copy derive" content changed semantically. Private fields, the other derives, and the field set are preserved |
| LG-0007-004 | pointer | root-SCHED | unnumbered norm (reference incorporation) | A configuration with no load controls set exactly reproduces the unbounded delivery mode | reaffirmed | preserved_in=0007 §2.1/§2.2; resolved_via=LG-0006-055 | The structural check of INV-L6 is preserved in the §2.2 owner table (channel construction unchanged) |
| LG-0007-005 | owner | leaf-I41 | unnumbered norm + negative space (no Default) | `RuntimeConfig::new(frame_rate)` is the only constructor; there is deliberately no `Default` impl (the crate has no default frame rate; adding one later is additive) | reaffirmed | preserved_in=0007 §2.1 | - |
| LG-0007-006 | owner | leaf-I41 | INV-C1 | INV-C1: `Runtime::new` is a literal delegation to `Self::with_config(flags, RuntimeConfig::new(frame_rate))`; there is exactly one construction path | reaffirmed | preserved_in=0007 §2.2/§7 | Literal delegation and the single construction path are unchanged even after the removal of construction dispatch in 0011 §3.4 (an implementation-seam change only) |
| LG-0007-007 | owner | leaf-I41 | INV-C2 | INV-C2: `new` keeps the given frame rate and leaves the 3 controls unset; each setter sets only its own field | reaffirmed | preserved_in=0007 §2.1/§7 | - |
| LG-0007-008 | owner | leaf-I41 | INV-C3 | INV-C3: no construction or setter can produce an invalid config, and none returns `Result` or panics | reaffirmed | preserved_in=0007 §2.1/§7 | - |
| LG-0007-009 | owner | leaf-I41 | INV-C4 | INV-C4: the public surface is only the frame rate + the 3 controls of RFC 0006 §4.1 (no restart-rate field); `tears::RuntimeConfig` is reachable, but not via `tears::prelude::*` | reaffirmed | preserved_in=0007 §2.1/§2.3/§4/§7 | - |
| LG-0007-010 | owner | leaf-I41 | INV-C5 | INV-C5: `with_config` builds the `FrameScheduler` from `config.frame_rate`; it never silently creates a scheduler at a rate different from the caller-supplied value | reaffirmed | preserved_in=0007 §2.2/§7 | - |
| LG-0007-011 | owner | leaf-I41 | INV-C6 | INV-C6: `RuntimeConfig::new` + the 3 setters + `Runtime::with_config` are `#[must_use]` (setters with an explanatory message) | reaffirmed | preserved_in=0007 §2.1/§7 | - |
| LG-0007-012 | owner | leaf-I41 | unnumbered norm (compatibility acceptance) | The `Copy` derive is intentional; adding a non-`Copy` field later is breaking, and that cost is recorded as knowingly accepted | redesigned | changed_to=0007 §2.1 | Adoption of the leaf-I41 verdict overturns this row's content — the `Copy` derive is removed in 0.11.0, so the recorded acceptance (non-`Copy` field additions being breaking, the cost knowingly accepted) is dissolved by the removal rather than preserved. Redesign under the 1 ID = 1 verdict rule because this row's compatibility-acceptance content changed semantically |
| LG-0007-013 | owner | leaf-I41 | unnumbered norm (compatibility) | Private fields make later field additions additive without `#[non_exhaustive]`; getters are not provided initially and can be added later additively | reaffirmed | preserved_in=0007 §2.1 | - |
| LG-0007-014 | owner | leaf-I41 | unnumbered norm | `Runtime::new`'s signature and semantics are unchanged; no other constructors are added and no existing signature changes | reaffirmed | preserved_in=0007 §2.2 | 0011 INV-LC3 (making construction inert) is a 0011-owned bootstrap change (U2); this row's contract surface (signature, constructor set, construction path) is preserved |
| LG-0007-015 | owner | leaf-I41 | unnumbered norm | Each constructor receives the frame rate exactly once | reaffirmed | preserved_in=0007 §2.2 | - |
| LG-0007-016 | owner | leaf-I41 | unnumbered norm (placement) | Module placement is `src/runtime/config.rs` (`pub mod config`) | reaffirmed | preserved_in=0007 §2.3 | - |
| LG-0007-017 | owner | leaf-I42 | unnumbered norm | The bench harness builds bounded runs via the public surface only; no config-related items are added to `bench-internals` | reaffirmed | preserved_in=0007 §2.3 | - |
| LG-0007-018 | pointer | root-SCHED | unnumbered norm (reference) | The runtime's own defaults leave all 3 controls unset (unbounded) | reaffirmed | preserved_in=0007 §3 opening; resolved_via=LG-0006-055 | - |
| LG-0007-019 | owner | root-SCHED | negative space (non-contract declaration) | Recommended values are guidance, not contract: no invariant tests the values, and changing them is a documentation change | reaffirmed | preserved_in=0007 §3 opening | - |
| LG-0007-020 | owner | root-SCHED | unnumbered norm (must) | Exception: the app-channel sizing rule (§3.1) must, as long as it is published, stay consistent with RFC 0006's measurements; the keyed-side rule carries no such obligation | reaffirmed | preserved_in=0007 §3 opening | - |
| LG-0007-021 | owner | root-SCHED | threshold (recommended value, non-contract) | Recommended `app_channel_capacity = 1024`: a measurement-informed margin choice, not a uniquely pinned value; 512/2048 are also defensible | reaffirmed | preserved_in=0007 §3.1 | - |
| LG-0007-022 | owner | root-SCHED | negative space | The app-channel latency estimate is an estimate, not a worst-case bound | reaffirmed | preserved_in=0007 §3.1 | - |
| LG-0007-023 | owner | root-SCHED | threshold (recommended value, non-contract) | Recommended `keyed_channel_capacity = 16`: a policy value not derived from measurement | reaffirmed | preserved_in=0007 §3.1 | - |
| LG-0007-024 | pointer | root-SCHED | negative space (explicit non-guarantee) | keyed capacity buys neither delivery-latency guarantees nor keyed liveness — sizing it for latency guarantees is a category error | reaffirmed | preserved_in=0007 §3.1; resolved_via=LG-0006-050 | The 0006 delta's INV-L14/L15 reinforce this non-guarantee with numbering (semantics unchanged) |
| LG-0007-025 | pointer | root-SCHED | negative space (upper bound on claim strength) | The §4.3 rationale supports only an existence claim (the admission window is scheduling-dependent and cuts both ways) | reaffirmed | preserved_in=0007 §3.1; resolved_via=0006 §4.3 | - |
| LG-0007-026 | owner | root-SCHED | threshold (recommended, non-contract) + negative space | Recommended `batch_max_messages`: unset — no non-`None` default is recommended; the count cap is documented only as a diagnostic knob | reaffirmed | preserved_in=0007 §3.1 | - |
| LG-0007-027 | owner | leaf-I41 | unnumbered norm (document placement) | rustdoc placement: sizing rules and recommended values live on each setter itself; no separate guide document is added | reaffirmed | preserved_in=0007 §3.2 | - |
| LG-0007-028 | pointer | root-SCHED | unnumbered norm (fulfillment of the documentation obligation) | The 3 guidance notes of RFC 0006 §§4.5/4.6/4.7 land in the same rustdoc | reaffirmed | preserved_in=0007 §3.3; resolved_via=0006 §6 OQ1 (§§4.5–4.7) | Because the delegating row LG-0006-073 is kind=pointer, this resolves via the RFC § |
| LG-0007-029 | owner | leaf-I41 | unnumbered norm + negative space | restart-rate control consumes no `RuntimeConfig` surface: no field and no name reservation | reaffirmed | preserved_in=0007 §4 | Consistent on both faces with the 0006 OQ5 rejection |
| LG-0007-030 | pointer | leaf-I42 | unnumbered norm (reference restatement) | All latency criteria are scoped to the RFC 0006 §2 reference machine; CI gates no latency criterion | reaffirmed | preserved_in=0007 §5 opening; resolved_via=LG-0006-065 | - |
| LG-0007-031 | pointer | root-SCHED,leaf-I42 | threshold (acceptance parameter) | There is exactly one bounded acceptance configuration: 60 FPS / 1024 / 16 / `batch_max_messages = None`, fixed before implementation | reaffirmed | preserved_in=0007 §5.1; resolved_via=0006 §5.1 | - |
| LG-0007-032 | pointer | root-SCHED | unnumbered norm (exclusion rationale) | `batch_max_messages` is `None` in the matrix: its contract is checked at the unit layer via INV-L12 | reaffirmed | preserved_in=0007 §5.1; resolved_via=LG-0006-061 | - |
| LG-0007-033 | owner | leaf-I42 | negative space | A second small-capacity configuration is deliberately not added | reaffirmed | preserved_in=0007 §5.1 | - |
| LG-0007-034 | owner | leaf-I42 | unnumbered norm (measurement definition) | Non-conflation of the two depth quantities: shared-channel occupancy ≤ capacity; observed depth ≤ `capacity + concurrent producers` | reaffirmed | preserved_in=0007 §5.2 | - |
| LG-0007-035 | owner | leaf-I42,root-K51J46 | unnumbered norm (redefinition) | Unbounded `quit_backlog_*` rows have no bounded counterparts; bounded quit rows take blocked-producer count and channel-full churn as independent variables | reaffirmed | preserved_in=0007 §5.2 | - |
| LG-0007-036 | pointer | root-K51J46 | threshold (acceptance) | `quit_idle`: 200 trials, quit→delivered p99 ≤ 1 ms | reaffirmed | preserved_in=0007 §5.2; resolved_via=LG-0006-051 | - |
| LG-0007-037 | pointer | root-K51J46 | threshold (acceptance) | `quit_blocked_1`: 200 valid trials satisfying `blocked == 1` at the quit instant, p99 ≤ 1 ms | reaffirmed | preserved_in=0007 §5.2; resolved_via=LG-0006-051 | - |
| LG-0007-038 | pointer | root-K51J46 | threshold (acceptance) | `quit_blocked_64`: `blocked == 64`, 200 trials, p99 ≤ 1 ms; depth is read as `capacity + producers` (1088) | reaffirmed | preserved_in=0007 §5.2; resolved_via=LG-0006-051 | - |
| LG-0007-039 | pointer | root-K51J46 | threshold (acceptance) | `quit_overload`: ≥ 2 capacity-wait events within the 5ms window immediately before quit, 200 trials, p99 ≤ 1 ms | reaffirmed | preserved_in=0007 §5.2; resolved_via=LG-0006-051 | - |
| LG-0007-040 | pointer | root-K51J46 | threshold + negative space (no bound) | `quit_keyed_bounded`: `blocked >= 1`, 20 trials, no acceptance bound | reaffirmed | preserved_in=0007 §5.2; resolved_via=LG-0006-053 | The absence of a bound explicitly matches the recorded legality of admission-window delivery |
| LG-0007-041 | owner | leaf-I42 | unnumbered norm (must) | Predicates must be checked by gauge/event observation at the quit instant; a barrier is never a substitute for observation | reaffirmed | preserved_in=0007 §5.2 note | - |
| LG-0007-042 | owner | leaf-I42 | negative space | `blocked == N` is the entirety of the precondition; a simultaneous raw-occupancy claim is deliberately not included | reaffirmed | preserved_in=0007 §5.2 note | - |
| LG-0007-043 | pointer | leaf-I42 | unnumbered norm (consumption rule for dependency contracts) | Gauge consumption must use "current value = the value of the event with the max `seq`" and must not trust arrival order | reaffirmed | preserved_in=0007 §5.2 note; resolved_via=LG-0006-032 | The consumer wording is synchronized in the same bundle — a general consumer takes max-seq after partitioning by runtime_id; this harness, with serial execution + a teardown barrier, always has exactly 1 active partition, so the scalar high-water is its degenerate form. No semantic change to the max-seq consumption predicate itself (confirmed by acceptance verification). Affirmation kept; the redesign is carried by the resolved_via target 032 |
| LG-0007-044 | owner | leaf-I42 | unnumbered norm (verdict rule) | Three-way classification of attempt outcomes: valid trial / predicate miss (the only retryable one) / quit-contract failure (immediate fail, never retried) | reaffirmed | preserved_in=0007 §5.2 | - |
| LG-0007-045 | owner | leaf-I42 | threshold | attempt cap: each predicated row gets at most `10 × trials` attempts | reaffirmed | preserved_in=0007 §5.2 | - |
| LG-0007-046 | owner | leaf-I42 | negative space | The cap guarantees termination only, not a wall-clock ceiling | reaffirmed | preserved_in=0007 §5.2 | - |
| LG-0007-047 | owner | leaf-I42 | unnumbered norm (reporting rule) | The 3 terminal outcomes are reported as separate classes and never conflated; a row's count is the number of valid trials | reaffirmed | preserved_in=0007 §5.2 | - |
| LG-0007-048 | pointer | root-SCHED | unnumbered norm + negative space | `overload`/`burst_200k`/`keyed_overload`: re-run under the §5.1 configuration; criteria are RFC 0006 §5.1 verbatim; observed-depth upper bound is `1024 + 1` | reaffirmed | preserved_in=0007 §5.3; resolved_via=0006 §5.1 | - |
| LG-0007-049 | pointer | root-SCHED | unnumbered norm + negative space | `steady_20k`/`steady_200k` are not bounded rows: they run under the default configuration, with structural identity verified by code inspection | reaffirmed | preserved_in=0007 §5.3; resolved_via=LG-0006-055 | - |
| LG-0007-050 | pointer | root-SCHED | threshold (acceptance parameter) | `keyed_isolation`: concurrent verification of 8-key saturation + a 9th probe key + a shared probe | reaffirmed | preserved_in=0007 §5.3; resolved_via=0006 §5.1 | - |
| LG-0007-051 | owner | leaf-I42,root-SCHED | threshold (acceptance) | `keyed_isolation` gate: shared occupancy in the simultaneous sample is exactly `capacity + 1` (not the historical peak) | reaffirmed | preserved_in=0007 §5.3 | - |
| LG-0007-052 | owner | leaf-I42,root-SCHED | threshold (acceptance) | Additional `keyed_isolation` gate: not a single keyed message is delivered to `update`, and each keyed channel's yield count is exactly `capacity + 1` | reaffirmed | preserved_in=0007 §5.3 | - |
| LG-0007-053 | owner | leaf-I42 | negative space | `keyed_isolation` is not included in the §6 smoke profile | reaffirmed | preserved_in=0007 §5.3 | - |
| LG-0007-054 | owner | leaf-I42 | unnumbered norm | CI runs the smoke profile in place of the full-scenario run; acceptance force is on the reference machine only | reaffirmed | preserved_in=0007 §6 | - |
| LG-0007-055 | owner | leaf-I42 | unnumbered norm (invocation form) | Smoke invocation: `--smoke` argument; CI invokes via `just bench-smoke`, identical to the local invocation | reaffirmed | preserved_in=0007 §6 | - |
| LG-0007-056 | owner | leaf-I42 | threshold (smoke parameter) | Smoke scenario set: `steady_20k` shortened to 0.5s + a 20k bounded burst + `quit_idle_bounded`/`quit_blocked_1` at 5 valid trials each (50-attempt cap) | reaffirmed | preserved_in=0007 §6 | - |
| LG-0007-057 | owner | leaf-I42,root-SCHED | unnumbered norm (gate definition) | Gate for the draining scenario: `Msg::Load` seq values cover `0..total` exactly once each and in order (strictly-increasing-by-one) — refuting drop/duplicate/reorder/lost tail all at once | reaffirmed | preserved_in=0007 §6 | The rationale that a gap surfaces a 0006 INV-L2 violation is likewise unchanged |
| LG-0007-058 | owner | leaf-I42 | negative space (record of the rejected form) | A total-only assertion is deliberately rejected (it would let drop+duplicate pass) | reaffirmed | preserved_in=0007 §6 | - |
| LG-0007-059 | owner | leaf-I42,root-K51J46 | negative space (rationale for the absence of a gate) | Quit scenarios assert completion only (shutdown discard and illegal drop are indistinguishable at the observation point) | reaffirmed | preserved_in=0007 §6 | - |
| LG-0007-060 | owner | leaf-I42 | negative space | The smoke profile carries no latency assertion whatsoever | reaffirmed | preserved_in=0007 §6 | - |
| LG-0007-061 | owner | leaf-I42 | threshold + unnumbered norm (implementation obligation) | Every smoke scenario has a per-scenario `max_wall` = 30 s completion guard | reaffirmed | preserved_in=0007 §6 | - |
| LG-0007-062 | owner | leaf-I42 | unnumbered norm | `quit_blocked_1` carries a second fail condition, the attempt cap; the 3 failure classes remain distinguishable even in smoke | reaffirmed | preserved_in=0007 §6 | - |
| LG-0007-063 | owner | leaf-I42 | negative space (denial of status) | Smoke is neither an acceptance run nor a regression baseline, and its numbers are recorded nowhere | reaffirmed | preserved_in=0007 §6 | - |
| LG-0007-064 | owner | leaf-I41,leaf-I42 | unnumbered norm (meta) | No open questions; deliberate exclusions (future getter, aggregate capacity-wait event, restart-rate design) are already assigned to follow-ups or future RFCs | reaffirmed | preserved_in=0007 §8 | - |
| LG-0007-065 | owner | leaf-I41 | unnumbered norm (meta) | Enforcement classes follow the pre-review checklist definitions | reaffirmed | preserved_in=0007 §7 opening | - |
| LG-0007-066 | owner | leaf-I41,root-SCHED | unnumbered norm (coverage declaration) | Surface–invariant coverage table (struct+new → C2/C3/C6 etc.); the runtime semantics of the load controls are owned by RFC 0006 INV-L1/L3/L6/L12 and this RFC does not duplicate them | reaffirmed | preserved_in=0007 §7 end | The 0006 (as amended) addition of INV-L14/L15 does not change this row's coverage claim — L14/L15 pin permitted execution and vocabulary and do not own the runtime semantics of the load-control fields |
| LG-0008-001 | owner | root-I40 | INV-T1 | The `Application` definition is invariant in this RFC: `type Message: Send + 'static` unchanged, no new bounds. Any future RFC adding a bound is obligated to declare it as a breaking change with a migration path | reaffirmed | preserved_in=0008 §2.1 | - |
| LG-0008-002 | owner | root-D18 | INV-T2 | TestStore bound placement: `Debug` on the store itself, `PartialEq` only on the equality-assert methods, `Clone` nowhere | reaffirmed | preserved_in=0008 §2.1 | Unaffected — the Δ (new 0012, informative sync of 0002 §9, 0011 §2.1/§7) does not touch TestStore's trait-bound placement (Debug/PartialEq/Clone); 0012 §6.2 also states TestStore's public API is unchanged |
| LG-0008-003 | owner | root-D18 | unnumbered norm | `receive_matching(predicate)` is kept permanently as a bound-free escape hatch — `PartialEq` is never made load-bearing | reaffirmed | preserved_in=0008 §2.1 | Unaffected — the bound-free receive_matching escape hatch is a norm internal to the store API, with no contact with any contract surface in the Δ (subscription execution, dirty source, 0002 §9) |
| LG-0008-004 | owner | root-D18,root-A1 | INV-T3 | TestStore consumes commands at the same decomposition boundary as the runtime (`into_runtime_parts` → `RuntimeCommandParts`) and does not re-derive directives, cancellation, or effects in parallel | reaffirmed | preserved_in=0008 §4.1 | Unaffected — the command decomposition boundary (into_runtime_parts → RuntimeCommandParts) is a 0003/0009-side contract; the Δ diff covers only 0002/0011/0012/README — 0003 is unchanged |
| LG-0008-005 | owner | root-A1,root-D18 | unnumbered norm (premise) | Premise (landed): `RuntimeCommandParts` carries leaves unfolded and in declaration order; each consumer folds/drives at its own consumption site (the runtime uses `fold_leaves`, the store keeps them separate) | reaffirmed | preserved_in=0008 §4.1 | Unaffected — the landed premise that RuntimeCommandParts carries leaves unfolded and in declaration order concerns the command path; the Δ covers only the subscription execution contract and the dirty source |
| LG-0008-006 | owner | root-D18 | INV-T4 | Determinism: `send`/`advance`/`receive*` are synchronous, polling only on a fixed budget → two executions yield identical state transitions and delivery sequences (when update is deterministic) | reaffirmed | preserved_in=0008 §4.1 | Unaffected — INV-T4 determinism rests on the synchrony of send/advance/receive and the fixed poll budget, and does not cover subscription execution. The purity canonicalization (0012 §5) changes the citation target on the INV-T11 side (LG-0008-029), not this row |
| LG-0008-007 | owner | root-D18 | unnumbered norm | Poll budget: polls run only inside `receive*`, `finish`, the drop check, keyed-intake reconciliation, and the `advance` anchoring scan. A bare `send` polls no leaf at all | reaffirmed | preserved_in=0008 §4.1 | Unaffected — the enumeration of poll sites is a store-internal budget norm; nothing in the Δ touches the poll budget |
| LG-0008-008 | owner | root-D18 | unnumbered norm | Each check polls every scan-reached leaf exactly once and does not honor wakers within the call | reaffirmed | preserved_in=0008 §4.1 | Unaffected — exactly-once polling and non-honoring of wakers are store-internal norms supporting INV-T4, with no contact with the Δ |
| LG-0008-009 | owner | root-D18,root-K51J46 | unnumbered norm | The quit-state check precedes all poll sites | reaffirmed | preserved_in=0008 §4.1 | Unaffected — the quit-state check preceding all poll sites corresponds to 0006 INV-L2; the 0002 §9 Δ explicitly makes preservation of the two quit-delivery contracts (0006 R4/INV-L4, 0003 INV-9/0006 INV-L10) a condition of privatization, so the premise is unchanged |
| LG-0008-010 | owner | root-D18 | unnumbered norm | Stage 2 changes only where polls happen — the budget is unchanged | reaffirmed | preserved_in=0008 §4.1 | Unaffected — stage 2 changing only the location of polls depends on 0009 §3.2, and 0009 is not in the Δ |
| LG-0008-011 | owner | root-D18 | INV-T5 | Within one leaf: messages are delivered in stream order | reaffirmed | preserved_in=0008 §4.2 | Unaffected — in-leaf stream-order delivery is a store-specific contract mentioned by no document in the Δ |
| LG-0008-012 | owner | root-D18,root-A1 | INV-T6 | Cross-leaf canonical order: delivery proceeds from the earliest-enqueued deliverable leaf. This order is a TestStore contract only and must not be cited as a runtime ordering guarantee (citation rule) | reaffirmed | preserved_in=0008 §4.2 | Unaffected — 0011 §2.3 (the runtime counterpart of the citation rule) is outside the Δ diff and textually unchanged (the changes are to §2.1/§2.2/§7/INV-LC1). The second dirty source is a runtime-side re-evaluation trigger and does not touch the store's canonical order or its citation ban |
| LG-0008-013 | owner | root-D18 | unnumbered norm | Time-made-ready leaves merge into the same order at their enqueue position | reaffirmed | preserved_in=0008 §4.2 | Unaffected — the merge of time-made-ready leaves at their enqueue position depends on 0009 §3.2/§3.4; 0009 is unchanged |
| LG-0008-014 | owner | root-D18 | unnumbered norm | Scan semantics: `receive*` stops at the first deliverable; the `advance` anchoring scan never early-stops | reaffirmed | preserved_in=0008 §4.2 | Unaffected — the first-deliverable stop of receive* and the non-early-stopping advance anchoring scan are store-internal semantics unrelated to the Δ |
| LG-0008-015 | pointer | root-A1,root-D18 | INV-T7 | Cancellation parity, 6 behaviors: RFC 0003's INV-3,4,5,6,7,9 hold on the store's pending output | reaffirmed | preserved_in=0008 §5.1; resolved_via=0003 §6 | Unaffected — a store mapping of 0003 INV-3,4,5,6,7,9; 0003 is unchanged in the Δ (the 0002 §9 Δ contains only an informative note framing cancellation as having needed no new variant; the 0003 FSM/INVs are unchanged) |
| LG-0008-016 | owner | root-D18,root-A1 | unnumbered norm | Keyed-intake reconciliation issues only `KeepInFlight`; `CancelInFlight` polls zero (an escape hatch that can cancel an unpollable occupant) | reaffirmed | preserved_in=0008 §5.1 | Unaffected — KeepInFlight-only issuance and zero-poll CancelInFlight concern outcome agreement with 0003 §4.2; 0003 is unchanged |
| LG-0008-017 | pointer | root-A1,root-D18 | unnumbered norm | Occupancy accounting: an id is occupied for as long as the current run can produce output, and released upon observed exhaustion of all leaves; buffered output occupies and counts toward exhaustiveness | reaffirmed | preserved_in=0008 §5.1; resolved_via=0003 §6 | Unaffected — the owner of occupancy accounting, 0003 INV-6/INV-7, is unchanged in the Δ |
| LG-0008-018 | pointer | root-A1,root-D18 | unnumbered norm | Batch child-key folding needs no restatement in the store (because it consumes real `Command`s) | reaffirmed | preserved_in=0008 §5.1; resolved_via=LG-0003-069 | Unaffected — the owner of batch child-key folding, 0003 INV-11, is unchanged in the Δ, and the argument that it holds by construction through real `Command` consumption is also unchanged |
| LG-0008-019 | owner | root-D18,root-A1 | negative space | Negative space: the stale-exit token (INV-8) and bounded bookkeeping (INV-13) are unmodeled; the `KeepInFlight`-discard assert in the reconciliation window is a store choice, not a runtime guarantee | reaffirmed | preserved_in=0008 §5.1 | Unaffected — the unmodeled targets (0003 INV-8/INV-13) are outside the Δ. The INV-13 here is 0003's bounded bookkeeping, distinct from the 0005 INV-13 (restart semantics) whose preservation 0012 §4.3 states — no room for confusion |
| LG-0008-020 | pointer | root-B7,root-D18,root-K51J46 | unnumbered norm | `redraw_requested()` is the folded directive of the most recent step; before the first step it returns the init command's directive, but this is not a prediction of the runtime's first render (the runtime does not read init's directive and always renders the first frame) | reaffirmed | preserved_in=0008 §5.2; resolved_via=LG-0002-017 (0002 directive owner) and 0011 §3.2/INV-LC4 (production contract for the initial render) | Unaffected — the 0002 §9 Δ is an informative reorganization premised on Axis B/privatization; directive ownership (LG-0002-017) is unchanged, and 0011 §3.2/INV-LC4 are outside the Δ diff and textually unchanged — no change to the store-side observable contract |
| LG-0008-021 | owner | root-K51J46,root-D18 | unnumbered norm | Quit is asserted only via `receive_quit`; residual output after an observed quit is legitimately discarded; a suppressed quit is not "observed" | reaffirmed | preserved_in=0008 §5.3 | Unaffected — 0011 §4.4 is unchanged in the Δ. 0012 INV-SE5 states that quiescence at termination marks no dirt, which in fact presupposes the survival of the §4.4 postcondition; preservation of the two quit-delivery contracts is also stated by the 0002 §9 Δ |
| LG-0008-022 | owner | root-D18 | INV-T8 | Exhaustiveness: each leak class fails at its designated site (`finish`/drop). `send` is not a leak-check site | reaffirmed | preserved_in=0008 §6 | Unaffected — the per-leak-class fail-site designation is store accounting; nothing in the Δ touches exhaustiveness |
| LG-0008-023 | owner | root-D18,root-SCHED | unnumbered norm | `send` has no exhaustiveness precondition — keyed-output precedence is the runtime's shared-first parity, while unkeyed precedence is TestStore's own linearization and is no evidence about the runtime | reaffirmed | preserved_in=0008 §6 | Unaffected — the absence of an exhaustiveness precondition on send and shared-first parity (0003 INV-14) depend only on contracts outside the Δ |
| LG-0008-024 | owner | root-D18 | unnumbered norm | `new` enqueues the init command the same way as a step, and init output is subject to the same accounting | reaffirmed | preserved_in=0008 §3.2 | Unaffected — 0011 §3.3 (intake/accounting mapping) and §3.4 (removal of construction-time dispatch) are outside the Δ diff and textually unchanged; the observable surface of init enqueue and shared accounting is unchanged |
| LG-0008-025 | owner | root-D18 | negative space | Exhaustive-only is the sole mode; a lenient mode is deliberately not designed | reaffirmed | preserved_in=0008 §6 | Unaffected — the exhaustive-only single-mode negative space is touched by no document in the Δ |
| LG-0008-026 | owner | root-K51J46,root-D18 | INV-T9 | Quit terminality: after `receive_quit`, `send`/`advance`/`receive*` fail without polling; `finish`/drop pass without polling | reaffirmed | preserved_in=0008 §5.3 | Unaffected — quit terminality corresponds to 0006 INV-L2/0003 INV-9; both RFCs are outside the Δ |
| LG-0008-027 | owner | root-D18 | INV-T10 | Ambient-runtime rejection: `TestStore::new` checks `Handle::try_current()` before doing any work and panics immediately if it succeeds | reaffirmed | preserved_in=0008 §4.3 | Unaffected — INV-T10's single construction-time check remains consistent with 0011 §3.3, which is unchanged in the Δ; 0012 does not mention store construction |
| LG-0008-028 | owner | root-D18 | negative space | Limit of INV-T10: the check happens only once, at construction | reaffirmed | preserved_in=0008 §4.3 | Unaffected — the statement that the check happens only once at construction has no contact with the Δ |
| LG-0008-029 | owner | root-D18,root-CMP | INV-T11 | `subscription_ids` returns the RFC 0005 §3.5 first-occurrence-stable dedup and calls no source's `stream()`; the return value is the input to reconciliation, not a prediction of the spawn set; no warning event fires either | reaffirmed | preserved_in=0008 §3.2 | Verdict unchanged (upheld), but the supporting citation moves — the purity premise of INV-T11 determinism is canonicalized from rustdoc-only to 0012 §5 (INV-SE6); 0012 §5 names INV-T11 as the store-side consumer of purity, and §6.2 states INV-T11 is preserved unchanged. Because the 0012 §4 quiescence barrier can delay admission, the clause "the return value is the input to reconciliation, not a prediction of the spawn set" is all the more load-bearing (even under the barrier, only the declared set is observed — unchanged) |
| LG-0008-030 | owner | root-D18 | INV-T12 | Controlled-context ownership and explicit-only time: `new` constructs and owns the context; time moves only via `advance` and by exactly `duration` → RFC 0009 §3.2 auto-advance never fires from the store's own operations | reaffirmed | preserved_in=0008 §4.3 | Unaffected — controlled-context construction ownership and explicit-only time depend on 0009 contracts (§3.2, INV-C2/C3); 0009 is outside the Δ, and 0011 §3.3 is likewise unchanged |
| LG-0008-031 | owner | root-D18,leaf-K56 | unnumbered norm | `advance`'s timer-driver barrier: after moving the clock, drive only until the timer driver has processed the reached instants; the barrier is executor progress, not leaf polling | reaffirmed | preserved_in=0008 §3.2 | Unaffected — the timer-driver barrier resolves the 0009 §3.2 readiness-point question; 0009 is unchanged |
| LG-0008-032 | owner | root-D18,root-A1 | INV-T13 | Anchoring: `advance` polls all pending leaves exactly once in enqueue order before moving the clock → a leaf's first poll is always at enqueue-time virtual now, and the timeout deadline = enqueue-now + declared duration (independent of scan order) | reaffirmed | preserved_in=0008 §3.2 | Unaffected — anchoring (the 0004 first-poll-anchor mapping, resolved by 0009 §5.1) depends only on contracts outside the Δ |
| LG-0008-033 | owner | root-D18 | unnumbered norm | `advance(Duration::ZERO)` is legal and anchors only | reaffirmed | preserved_in=0008 §3.2 | Unaffected — the legality of advance(ZERO) (anchor only) is store-specific and unrelated to the Δ |
| LG-0008-034 | owner | root-D18,root-K51J46 | unnumbered norm/threshold | `advance` fails in quit state; `duration` overflow panics | reaffirmed | preserved_in=0008 §3.2 | Unaffected — advance failing in quit state and the duration-overflow panic are store norms in the INV-T9 family, with no contact with the Δ |
| LG-0008-035 | owner | root-D18 | unnumbered norm | `receive*` diagnostic contract: a mismatch names the actual value via `Debug`; quit is exclusive to `receive_quit`; nothing-deliverable distinguishes 2 states | reaffirmed | preserved_in=0008 §3.2 | Unaffected — the receive* diagnostic contract (naming via Debug, 2-state distinction) is store-specific and mentioned by no document in the Δ |
| LG-0008-036 | owner | root-D18,root-K51J46 | unnumbered norm | `state`/`redraw_requested`/`subscription_ids`/`finish` remain callable even in quit state | reaffirmed | preserved_in=0008 §3.2 | Unaffected — the callable set in quit state is a 0008 §3.2 carve-out. 0012 INV-SE5's "no subscriptions calls after termination" is a runtime-side contract, the store does not run the runtime (0012 §6.2 preserved), and INV-SE6 purity permits calls at any frequency and any time, so there is no conflict |
| LG-0008-037 | owner | root-D18 | unnumbered norm | The drop check is identical to `finish`, but is skipped while panicking (avoiding double panic) | reaffirmed | preserved_in=0008 §3.2 | Unaffected — skipping the drop check while panicking (double-panic avoidance) is a store implementation norm unrelated to the Δ |
| LG-0008-038 | owner | root-D18 | unnumbered norm | Placement: the single path `tears::testing::TestStore`; no re-export, not in the prelude, no feature flag | reaffirmed | preserved_in=0008 §3.3 | Unaffected — the single-path, no-re-export, no-feature-flag placement is also consistent with 0012 §6.2's "TestStore public API unchanged"; no change |
| LG-0008-039 | owner | root-D18,leaf-K56 | negative space | §4.3 negative space: tasks spawned from within a leaf poll are outside the pending set; clock-manipulation effects void the contract; `tokio::time::pause` and nested-runtime blocking are loud failures | reaffirmed | preserved_in=0008 §4.3 | Unaffected — the §4.3 negative space (out-of-pending-set spawns, clock-manipulation voiding, pause/nested-runtime loud failure) is on the time axis; 0012 §9 also states the time axis stays with 0009, unchanged |
| LG-0008-040 | owner | root-D18,leaf-K56 | unnumbered norm + negative space | I/O leaves: because the context is time-only, a leaf requiring the I/O reactor hits a missing-reactor panic on its first scan poll; for merely-pending leaves, `finish` is the only accountability | reaffirmed | preserved_in=0008 §4.3 | Unaffected — the missing-reactor panic for I/O leaves is a consequence of the time-only context, with no contact with the Δ |
| LG-0008-041 | owner | root-D18 | negative space | Out of scope: no execution of subscription sources (start/poll/restart) in either stage 1 or stage 2; the resolution is not Clock DI but an independent subscription-execution design | reaffirmed | preserved_in=0008 §1.2 | Basis updated — the "independent subscription-execution design" this row points to has been drafted as RFC 0012, and 0012 §1.2/§6.2 state the TestStore non-execution contract (0008 §1.2, INV-T11) is preserved as stated. A stage-3 driving store is left to a future 0008 amendment (0012 §6.2, §12 open question 2), so this row's out-of-scope declaration (no execution in stages 1 and 2; a design separate from Clock DI) stands as-is |
| LG-0008-042 | owner | root-D18,root-SCHED | negative space | Out of scope: runtime integration contracts (channel capacity, backpressure, batching, scheduling) — passing TestStore is evidence of nothing (citation ban stated explicitly) | reaffirmed | preserved_in=0008 §1.2 | Unaffected — the citation ban (0008 §1.2) and its 0011 §2.3 counterpart are unchanged in the Δ. The second dirty source (0011 §2.1) is new runtime-scheduling behavior, but the store does not run the runtime, so "TestStore passage is no evidence of runtime integration" is if anything reinforced — no contradiction |
| LG-0008-043 | owner | root-D18,root-CMP | negative space | Out of scope: reducer composition — a future composition API's adapter should reuse this store, but that is that RFC's obligation | reaffirmed | preserved_in=0008 §1.2 | Unaffected — the store-reuse obligation for composition adapters stays with the future RFC. 0012 §1.2 also scopes composition out, and §4.4 only states the barrier's composition transparency; this row's delegation structure is unchanged |
| LG-0008-044 | pointer | root-D18 | unnumbered norm | Dependency direction: stage 2 consumes RFC 0009's contracts and resolves its §5.1's 3 design inputs; ownership of the Clock contract lies with 0009 (inversion forbidden) | reaffirmed | preserved_in=0008 §7; resolved_via=0009 §5.1 | Unaffected — the dependency direction (Clock contract owned by 0009; inversion forbidden) is unchanged. 0012 §9 likewise states the time axis remains 0009-owned and cites it only to mark the boundary, creating no inversion |
| LG-0008-045 | pointer | root-D18,root-A1 | negative space | Excluded claims: no same-poll readiness guarantee; no store-side restatement of RFC 0004 semantics (delegated to INV-C3 transparency) | reaffirmed | preserved_in=0008 §7; resolved_via=LG-0009-012 | Unaffected — the same-poll-readiness exclusion and the delegation of 0004 semantics to INV-C3 transparency are on the 0009 side; 0009 is outside the Δ |
| LG-0008-046 | pointer | root-D18,leaf-K56 | unnumbered norm | `test-util` is part of the crate's unconditional `tokio` dependency features; the load-path regression check is held by RFC 0009 INV-C4 | reaffirmed | preserved_in=0008 §7; resolved_via=LG-0009-025 | Unaffected — the unconditional tokio features for test-util and the load-path regression check are on the 0009 §5.1/INV-C4 side, outside the Δ |
| LG-0008-047 | owner | root-D18 | negative space | Open question 1: the `receive_unordered` helper is undesigned (additive either way) | reaffirmed | preserved_in=0008 §9 | Unaffected — the receive_unordered open question stays in 0008 §9. The stage-3 amendment (0012 §6.2/§12) is a different future surface (subscription driving) and neither closes nor changes this open question |
| LG-0009-001 | owner | root-D18 | INV-C1 | Single time source: all time reads in library code go through the virtualizable clock (`tokio::time`); banned std entry points appear nowhere except the bench exception | reaffirmed | preserved_in=0009 §3.1 | Unaffected — INV-C1's single time source is a time-axis contract. The Δ's 0012 §9 newly owns only non-time effect DI and states the time axis remains 0009's, unchanged (0012 §1.2). The 0009 text is unchanged. |
| LG-0009-002 | owner | root-D18 | unnumbered norm | The banned inventory is an exhaustive derivation over 3 classes (now-read / real-time wait / timed-wait construction) | reaffirmed | preserved_in=0009 §3.1 | Unaffected — the exhaustive 3-class derivation of the banned inventory is a time-axis norm enforced via clippy.toml. The Δ (new 0012, 0002 §9 sync, 0011 §2.1) does not touch 0009 §3.1. |
| LG-0009-003 | owner | root-D18,leaf-K56 | unnumbered norm | For platform-gated rows, the Linux CI lint gate is the actual enforcement | reaffirmed | preserved_in=0009 §3.1 | Unaffected — the Linux CI lint gate for platform-gated rows describes the enforcement mechanism and does not intersect the Δ's contract-changing surfaces (admission barrier, dirty source, effect-DI negative space). |
| LG-0009-004 | owner | root-D18 | unnumbered norm | `Duration` values are plain data; pure arithmetic is permitted; untimed blocking is permitted | reaffirmed | preserved_in=0009 §3.1 | Unaffected — Duration-as-plain-data and permitted pure arithmetic are unnumbered norms internal to the time axis. 0012 §9 covers only non-time effect DI and does not touch this row. |
| LG-0009-005 | owner | root-D18,leaf-K56 | unnumbered norm | Dependency scoping: `tokio::time` is the only sanctioned time source; another dependency becoming a time source is a reviewable event | reaffirmed | preserved_in=0009 §3.1 | Unaffected — the tokio::time-only-sanctioned dependency scoping is on the time axis. 0012 §9 only marks the boundary ("RFC 0009 owns that axis still"); the owner of time-source scoping remains 0009. |
| LG-0009-006 | pointer | root-D18,root-SCHED | threshold/exception | The single deliberate exception: real wall-clock measurement in `benches/runtime_load.rs` (explicit lint allow, limited to that site) | reaffirmed | preserved_in=0009 §3.1; resolved_via=0006 §2 | Unaffected — the bench real-wall-clock exception is grounded in 0006's requirement for real measurement. The Δ does not change 0006; the pointer target and the basis of the verdict are unchanged. |
| LG-0009-007 | owner | root-D18,leaf-I42 | unnumbered norm | Observability-only reads follow the same rule — no exempt category; tests are under the same rule too | reaffirmed | preserved_in=0009 §3.1 | Unaffected — the same-rule treatment of observability-only reads (no exempt category) is a scoping norm of INV-C1. The 0006 load-event fields are also outside the Δ; the basis is unchanged. |
| LG-0009-008 | owner | root-D18 | INV-C2 | Advancement: in a controlled context, virtual time advances only via (a) explicit advance and (b) auto-advance when the executor is idle; a non-idling controller observes only its own advances | reaffirmed | preserved_in=0009 §3.2 | Unaffected — INV-C2 advancement is the controlled context's time contract. 0012 does not touch time control at all (§1.2 explicitly scopes Timing out), and the consumer side, 0008 §4.1/INV-T12, is outside the Δ. |
| LG-0009-009 | owner | root-D18,leaf-K56 | negative space/unnumbered norm | Auto-advance clause (b) is a record of the executor's documented behavior, not a grant by this contract (citation rule) | reaffirmed | preserved_in=0009 §3.2 | Unaffected — the citation rule for auto-advance clause (b) is a notational norm internal to 0009 §3.2 and intersects no surface of the Δ. |
| LG-0009-010 | owner | root-D18 | unnumbered norm | No early firing: while virtual now is before the deadline, no time-gated behavior fires | reaffirmed | preserved_in=0009 §3.2 | Unaffected — no-early-firing is the firing contract for time-gated behavior. 0012 §2 adds no timing claims (delivery/pacing only cites 0006), so the basis is unchanged. |
| LG-0009-011 | pointer | root-D18 | unnumbered norm + negative space | Unpinned part of readiness: which poll observes readiness, and whether there is a barrier, this RFC does not pin — stage 2 fixes it | reaffirmed | preserved_in=0009 §3.2; resolved_via=LG-0008-031 | Unaffected — the which-poll/barrier fixing of readiness is owned by stage 2 (0008 §3.2). 0012 §6.2 states the 0008 contracts are "preserved as stated"; the pointer remains valid. |
| LG-0009-012 | owner | root-D18,root-A1 | INV-C3 | Transparency: RFC 0004's timeout/retry semantics and `Timer` semantics hold identically under the virtual clock (the contracts are inequality-shaped, so a paused run is itself evidence of the contract) | reaffirmed | preserved_in=0009 §3.2 | Unaffected — INV-C3 transparency (0004 timeout/retry + Timer semantics) is a time-axis contract. 0012 §1.2 states "Timer's tick semantics are RFC 0009"; the owner is unchanged. |
| LG-0009-013 | owner | root-D18 | unnumbered norm (within INV-C3) | The `Timer` anchor is the stream's first poll (neither `new` nor `stream()`); time elapsed before the first poll counts toward nothing, and the initial `next_deadline` = first_poll + interval | reaffirmed | preserved_in=0009 §4.2 | Unaffected — Timer anchor = stream first poll is consistent with 0012 §2: the resource-acquisition point is not pinned, and first-poll acquisition is conforming (signal-source example). Admission delay from the quiescence barrier only delays the first poll (= the anchor) and does not contradict the anchor contract. |
| LG-0009-014 | owner | root-D18 | unnumbered norm (within INV-C3) | `Timer` non-catch-up: when now >= `next_deadline`, exactly one tick is deliverable regardless of how many boundaries elapsed (no burst) | reaffirmed | preserved_in=0009 §4.2 | Unaffected — non-catch-up (exactly one tick) is precisely tolerance of arbitrary poll pacing, and aligns exactly with 0012 §2's forwarder pace (which cites the 0006 §3.2 SubscriptionSource norm). |
| LG-0009-015 | owner | root-D18 | unnumbered norm (within INV-C3) | `Timer` post-miss cadence: on tick delivery, `next_deadline` moves to the first anchor-phase boundary strictly after current now (phase preserved) | reaffirmed | preserved_in=0009 §4.2 | Unaffected — post-miss cadence (phase preservation) is a count-and-cadence contract that holds even under forwarder-paced delayed polling. 0012 §2 adds no delivery/timing claims, so the basis is unchanged. |
| LG-0009-016 | owner | root-D18 | unnumbered norm | The `Timer` contract is specified as observable properties — mechanism-independent, free to reimplement; it is a count-and-cadence claim, not a which-poll claim | reaffirmed | preserved_in=0009 §4.2 | Unaffected — the observable-properties specification that makes no which-poll claim is mutually presupposing with 0012 §2's forwarder-paced arbitrary polling; consistent, no double ownership. |
| LG-0009-017 | owner | root-D18 | unnumbered norm (deliverable) | §4.2 is a behavior-changing fix: the catch-up burst is abolished — a `CHANGELOG: Fixed` entry is mandatory | reaffirmed | preserved_in=0009 §4.2 | Unaffected — the CHANGELOG: Fixed for the catch-up-burst abolition is 0009's own deliverable. The Δ does not touch 0009's deliverables (0012's Changed entry concerns admission/trigger, a separate matter). |
| LG-0009-018 | owner | root-D18 | unnumbered norm | The existing wide-margin real-time `Timer` tests are demoted to non-normative smoke checks | reaffirmed | preserved_in=0009 §4.2 | Unaffected — the non-normative demotion of the wide-margin real-time tests is a testing norm internal to 0009 §3.4/§4.2 and does not intersect the Δ. |
| LG-0009-019 | owner | root-D18 | INV-C4 | Production neutrality: the production runtime does not pause, construct, or configure the clock; no public type mentions time control; unpaused time reads are observationally identical to the platform monotonic clock | reaffirmed | preserved_in=0009 §3.3 | Unaffected — INV-C4 production neutrality is an absence contract for the time-control surface. 0012 adds no public API (INV-SE8 also names tests/api_surface.rs as the regression neighbor) and does not conflict with neutrality. |
| LG-0009-020 | owner | leaf-K56,root-D18 | unnumbered norm | Pause is possible only on a single-thread runtime — production's multi-thread configuration is structurally unpausable | reaffirmed | preserved_in=0009 §3.3 | Unaffected — the single-thread restriction on pause is a norm grounded in the executor's nature, independent of the subscription execution contract (0012). |
| LG-0009-021 | owner | root-D18 | unnumbered norm (premise) | §4.1 HTTP migration (landed): cell.rs/query.rs moved from `std::time::Instant` to the virtualizable `Instant`; behavior-preserving when unpaused | reaffirmed | preserved_in=0009 §4.1 | Unaffected — the HTTP time-source migration is a landed premise. The Δ does not touch the time sources of http/cell.rs or query.rs; the unpaused behavior-preserving basis is unchanged. |
| LG-0009-022 | owner | root-D18 | unnumbered norm (decision) | No clock abstraction: no `Clock` trait, no injected clock value, no clock parameter — the time axis is the single axis of the executor clock; resolves RFC 0004 §1.4's clock-DI deferral | reaffirmed | preserved_in=0009 §2 | Unaffected — the ownership split of no-clock-abstraction is confirmed by the 0012 §9 text: "RFC 0009 rejected a clock abstraction for the time axis and owns that axis still — this section neither restates nor extends that rejection". The split — time axis = 0009 §2, non-time axis = 0012 §9 (INV-SE8) — is consistent, no double ownership. Resolution of the 0004 §1.4 deferral also stays with 0009. |
| LG-0009-023 | owner | root-D18 | negative space | Rejected alternatives (recorded): `Arc<dyn Clock>` via Flags / drive-time resolution / per-source injection / TCA-style dependency registry | reaffirmed | preserved_in=0009 §2.2 | Unaffected — the §2.2 rejected alternatives (including the TCA-style dependency registry) are rejections recorded in the time-axis context. 0012 §9's rejection of a non-time DI registry self-identifies as "a new decision of this RFC" and cites 0009 only as a boundary marker, neither restating nor extending it — no contradiction (0009 §2.2's non-time-goes-through-Flags remark is rationale, not an ownership claim). |
| LG-0009-024 | owner | root-D18 | negative space | §3.4 negative space: equal-deadline firing order is unspecified (consumers needing an order linearize on their own — TestStore §4.2 is the worked example); no bound is pinned on real-time lateness; cross-context separation — no contract spans contexts | reaffirmed | preserved_in=0009 §3.4 | Unaffected — the §3.4 negative space (equal-deadline order, lateness bound, cross-context separation) is entirely on the time axis and does not overlap 0012 §9's non-time negative space. |
| LG-0009-025 | owner | root-D18,leaf-K56 | unnumbered norm | §5.1 `test-util` decision: moves to unconditional `tokio` dependency features together with the first in-crate consumer (TestStore stage 2); no feature flag on tears itself | reaffirmed | preserved_in=0009 §5.1 | Unaffected — the unconditional-dependency-feature decision for test-util is a time-axis decision on the Cargo surface. The Δ touches neither Cargo.toml nor 0008 stage 2. |
| LG-0009-026 | pointer | root-D18 | unnumbered norm/negative space | §5.1 Timer's stage-2 exclusion: `Timer` is a subscription source and does not enter the store's pending command set; delivery would require a full SubscriptionManager-equivalent design | reaffirmed | preserved_in=0009 §5.1; resolved_via=LG-0008-041 | Unaffected — the rationale for Timer's stage-2 exclusion (subscription execution lies outside the store) is maintained: 0012 §6.2 states the 0008 §1.2 non-execution contract is "unchanged and deliberately preserved". The execution contract needed for delivery is now owned by 0012, but stage-3 driving remains a future 0008 amendment; the exclusion itself and the pointer are unchanged. |
| LG-0009-027 | pointer | root-D18 | unnumbered norm | §5.1 records the 3 design inputs for stage 2 only — resolution belongs to RFC 0008 | reaffirmed | preserved_in=0009 §5.1; resolved_via=0008 §7 | Unaffected — the resolution target of the 3 design inputs, 0008 §7, is unchanged in the Δ, and 0009's record-only positioning is likewise unchanged. |
| LG-0009-028 | owner | root-D18 | unnumbered norm | This RFC provides no clock value — what the store holds is a controlled time context | reaffirmed | preserved_in=0009 §5.1 | Unaffected — the no-clock-value provision (the store holds a controlled time context) is the consumption premise of 0008 INV-T12. 0012 §6.2 states 0008 is unchanged; the basis is unchanged. |
| LG-0009-029 | owner | leaf-N57,root-D18 | negative space | §1.2 out of scope: calendar time (no `SystemTime` reads exist; date rendering is injected by the app itself via Flags) / randomness and jitter (future backoff-policy RFC) / TestStore's time-API shape / debounce/throttle and restart-rate semantics / real-time accuracy bounds | reaffirmed | preserved_in=0009 §1.2 | Unaffected — the out-of-scope enumeration remains accurate after the Δ: restart-rate is still delegated to a future opt-in RFC in 0012 §8 (the same standing position as 0007 §4/0006 open question 5), so 0009's deferral target has not disappeared. Calendar time via self-injected Flags is likewise cited by 0012 §9 as the same convention (a docs/examples concept, not an API surface) — consistent. |
| LG-0009-030 | owner | root-D18 | unnumbered norm (explicit non-contract) | The §5.5 rustdoc recipe is documentation, not contract surface (2 sites: the `tears::testing` module doc + the `QueryConfig` rustdoc) | reaffirmed | preserved_in=0009 §5.5 | Unaffected — the non-contractual nature of the rustdoc recipe (a documentation deliverable) intersects none of the Δ's contract changes. |
| LG-0009-031 | owner | root-D18,leaf-K56 | unnumbered norm (docs) | Auto-advance vs real-I/O caveat (docs): awaiting real network I/O on a paused runtime auto-advances before the I/O completes | reaffirmed | preserved_in=0009 §5.5 | Unaffected — the auto-advance vs real-I/O caveat is a docs-only warning, unrelated to 0012's admission/trigger changes. |
| LG-0009-032 | owner | root-D18 | negative space | No open questions — the nearest candidates are recorded in the §3.4 negative space and remain unpinned until a named consumer states a need | reaffirmed | preserved_in=0009 §7 | Unaffected — 0009 §7 remains "None" after the Δ (text unchanged, confirmed by diff). Drafting 0012 added no open question to 0009, and the structure of recording into the §3.4 negative space is unchanged. |
| LG-U01 | owner | root-K51J46 | unnumbered norm (implicit, no owner) | Lifecycle phase order: update → command enqueue → (end of batch) → render → subscription re-evaluation. "A new state's subscriptions start only after that state has been rendered once" | reaffirmed | preserved_in=0011 §2 (INV-LC1/INV-LC2) | The preserved_in target, 0011 §2 itself, is revised by the Δ — §2.1 adds a second dirty source (quiescence of tasks stopped by steady-state re-evaluation), and §2.2's no-render re-evaluation case analysis gains "a pass whose dirt was marked only by lifecycle completion", making a message-independent frame pass possible. However, the phase order (batch → frame pass; render precedes re-evaluation; the same pass observes the same current state) and the shape of INV-LC1/LC2 are unchanged, and the never-while-pending form — "if a redraw is pending, subscriptions start against the state that same pass rendered" — is preserved. The verdict stands, with the basis updated to the revised §2 |
| LG-U02 | owner | root-K51J46 | unnumbered norm (implicit, no owner) | Bootstrap's first observable states: the init command is spawned at construction, initial subscriptions at the start of `run`, first render after that (the relative order is unspecified and not pinned even as negative space) | redesigned | changed_to=0011 §3 (INV-LC3/LC4) | Construction-time spawn abolished (budgeted for 0.11.0). The init-first relative order is preserved |
| LG-U03 | owner | root-K51J46 | unnumbered norm (implicit, no owner) | Termination model: a unified contract across quit / render error / panic / drop for cleanup, termination reason, treatment of buffered output, and grace period | redesigned | changed_to=0011 §4 (INV-LC5-LC7) | New constraints: the 2-path contract + 2-stage postcondition. Preserved parts: the existing shutdown descriptions in 0003 §5.6 / 0006 §4.3 |
| LG-U08 | owner | root-D18 | unnumbered norm (rustdoc only, no RFC owner) | `subscriptions()` is a pure function of state (no side effects; identical declared set for identical state) | reaffirmed | preserved_in=0012 §5 (INV-SE6) | The previously deferred canonicalization is resolved by the drafting of 0012. §5 INV-SE6 becomes the owner of record for purity (identical declared set for identical state; no side effects; no reads of external mutable state; independence from call count/timing), with a deliverable updating the rustdoc to cite this RFC. 0011 §2's dirty-frame gating and 0008 INV-T11 are named as existing consumers; the row's content is preserved as-is |
| LG-U10 | owner | root-K51J46 | unnumbered norm (implementation only) | Unkeyed/subscription remainder of panic isolation: panics in unkeyed command tasks and subscription forwarders are caught and logged and do not bring down the app (the keyed portion is already owned by 0003 §5.5) | reaffirmed | preserved_in=0011 §5 (INV-LC8) | Implementation-only → canonicalized. Log emission for the keyed portion is split with 0003 §7.3 |

## 9. Additivity audit (snapshot)

The full fixture-manifest snapshot — every feature fixture,
interaction row, and anti-catalog decision the audit tracked, with
terminal verdicts under §1.7's vocabulary — snapshot evidence in the
sense of §1.2. Population: 115 rows in three kinds across five id
series. Provenance: the N series is the divergence sweep of the
framework-comparison and future-direction notes; the INT series is
the interaction suite §1.4 composes from the audited fixtures; the AC
series (including the AC-E sub-series of previously settled
rejections) is the anti-catalog candidate list; the P and S series
are the pre-existing feature backlog; the TestStore issue list and
the TCA-comparison import questions complete the fifth series. The
population was frozen before the root judgments; ids are stable,
gaps are never re-numbered, and the population may be extended only
append-only (the pre-drafting drift scan required none).

Tallies (machine-verified): feature fixtures **A 35 / C 40 / X 13**;
anti-catalog decisions **adopted(X) 15**; interaction rows **pass 7 /
excluded 5 / fail 0**. No provisional values remain — `unknown` = 0
and `blocked_by` = 0 — so the fixture-side conditions of §1.8's gate
are met. Every remaining `C` names its owning task and both delegation
fields on its row; every row is `blocks composition = no`, and every
row is `gates the composition RFC = no` except N30 and N40, whose
owning document is the `cancel_scope` RFC and which therefore inherit
that delegation's `gates = yes` precedence constraint (§5.3 and §10).
Feature rows with
`status = excluded` carry `X`, recording the exclusion's
simplification; interaction rows record `excluded_by=<fixture id>`.

### 9.1 Feature fixtures — N series

| ID | Fixture | Status | Verdict | Note |
|---|---|---|---|---|
| N1 | High-frequency input coalescing | active | **C** | Condition: terminal-input-side coalescing = design of an opt-in RuntimeConfig policy (leaf-I41's `Copy` constraint lifted by the RFC 0007 `Copy`-derive removal amendment (§2.4) — non-`Copy` policy fields are also non-breaking). Non-terminal sources are additive via 0012 §2/§7 (coalesce in internal state). Indiscriminate shedding remains not provided per INV-L15. owner: input-coalescing policy RFC (RuntimeConfig) — blocks composition = no / gates the composition RFC = no |
| N2 | Input protocol extensions | active | **A** | Absorbed by the single 0012 §2 source template (a new protocol = adding a source implementation; owner/information flow unchanged) |
| N3 | Terminal mode control | active | **A** | Confirms B-11: new directives are additive on the `RuntimeDirectives` side; `Action` closed set (0003 non-negotiable E) preserved |
| N4 | suspend/resume + handoff | active | **C** | Condition: leaf-G36 (terminal ownership seam) + root-B7 (closedness of the suspend entry point) + **root-D18 (quiesce→restart contract for the terminal source — an abort request alone is insufficient because a one-beat-late poll may read input intended for the external process, so quiescence confirmation is required; separate from liveness backpressure of non-terminal producers)**. The lifecycle side is carried by 0011. Design obligation: the degraded select must keep the quit branch armed. owner: terminal suspend/resume RFC — blocks composition = no / gates the composition RFC = no |
| N5 | Rich output (OSC52/images) | active | **X** | Per leaf-G36: delegated to ratatui/backend (contour declaration sharing the same root as AC4). The framework adds no new contract to the render path |
| N6 | Process-based sources | active | **A** | Conforms to the 0012 §2 template (another source of the same shape as P7/P17) |
| N7 | Signal subscription | active | **C** | Condition: leaf-G36 (include the driving surface's exclusively-held signal inventory in the contour declaration). The source execution side is carried by 0012 §2 (implementation has the handler installed by first poll). owner: driving surface RFC (contour inventory) — blocks composition = no / gates the composition RFC = no |
| N8 | Command priority | excluded | **X** | INV-L15 (0006, as amended) canonicalizes non-provision of per-class priority/fairness/shedding — changing this requires an INV-14/L15 amendment. Concurrency limiting is a separate seam on the N31/source side. INT-1 terminates as excluded via this row (§9.2) |
| N9 | Intermediate progress reporting | active | **A** | Effects are already streams — carried as-is by the current delivery contract (owner/information flow unchanged) |
| N10 | Cross-feature shared state | active | **C** | Condition: composition RFC (expressed via parent state + routing — C-15). A dedicated shared-state mechanism is not provided — 0012 §7 limitation (public-handle mutation from update is not generalized; G-34/invalidate() remains the limited deviation of 0001 §5.5). owner: composition RFC — blocks composition = no / gates the composition RFC = no |
| N11 | Reducer middleware | active | **A** | Expressible under the current contract as a wrapper delegating to `Application` (function composition of update) — no runtime hook needed; owner/information flow unchanged. Library canonicalization is an optional extension of the composition RFC |
| N12 | Replay / time-travel | active | **A** | Confirms the root-I40 freeze: `Message: Send + 'static` unchanged; record/replay bounds are API-local opt-in (in-memory restoration = `Clone`, persistence = `Serialize + Deserialize` — both message and state). **INT-4 re-judged to excluded_by=N56 (uniform rule, same root as N17) — the 4 sharpening points are retained by this row**: canonical form = state-snapshot restoration (does not require update determinism — consistent with 0008 INV-T4; the only thing normatively reconstructible from restored state is subscriptions() = INV-SE6; view is rustdoc advice, so render reproduction is not claimed) / 2 optional modes = message re-application (deterministic apps only) and view re-execution (deterministic-view apps only) / external side effects, lifecycle trace, and render results under non-execution modes belong to N47 / the transcript is not an ordering contract |
| N13 | Property-based testing | active | **A** | Same judgment as N12 (API-local `Arbitrary` boundary). A real client of the 0008/0009 determinism contracts |
| N14 | Dev inspector | active | **C** | Per leaf-I42: condition = future design of a state/inspector observation surface (co-located with the N47 transcript RFC). leaf-I42's contract surface (the 3 INV-L13 kinds + instance field) is load observation only and does not satisfy the fixture's state-observation requirement. owner: state-observation RFC (seated with the N47 transcript RFC) — blocks composition = no / gates the composition RFC = no |
| N15 | Headless runtime | active | **C** | Condition: leaf-G36's backend/terminal separation seam. The 0011 phase machine is preserved as a render degeneration (consistent with §1.5's removal projection). owner: driving surface RFC (backend/terminal separation) — blocks composition = no / gates the composition RFC = no |
| N16 | Multiple runtime instances | active | **A** | Condition resolved: adding runtime_id to the gauge schema is drafted as the RFC 0006 gauge-schema amendment (§2.4) — the reference contract (§1.1) includes the bundle amendments, so the remaining observability condition (instance correlation) is satisfied. Instance-owned nature of core resources is confirmed; INV-LC9 preserved. Per-instance reconstruction is possible via partition→max-seq (batch/capacity-wait remain uncorrelatable — as contracted) |
| N17 | wasm / web backend | active | **X** | Consequence of adopting leaf-K56/AC3 + the root-I40 freeze — `Send` bound maintained and no alternate-executor compatibility surface provided, so the bound-removal direction is excluded |
| N18 | State persistence | active | **A** | Same judgment as N12 (`Serialize` is a local boundary on the opt-in feature side). Saving after the 0011 termination postcondition + INV-LC3 inert construction makes restoration contractually clean too (consistent with N26) |
| N19 | Crash recovery | active | **A** | 0011 §4.3/INV-LC6 already contract unwind-synchronous teardown + panic propagation. Terminal restoration is existing opt-in; display is additive via caller-side catch |
| N20 | Runtime configuration changes | active | **C** | Condition: design of a runtime-config-update/driver seam — the `Copy` removal (leaf-I41 adopted; the RFC 0007 `Copy`-derive removal amendment, §2.4) only removes the type-level obstacle; information flow, synchronization, and a public control surface for post-startup changes are undesigned. owner: runtime-config update RFC — blocks composition = no / gates the composition RFC = no |
| N21 | async/fallible init | active | **A** | 0011 §3 expresses bootstrap without a second runtime (minimal additive condition satisfied). Carried via a Loading state + init-originated quit. INT-8 passed with no new conditions; its single first-paint-policy sharpening point is recorded on N22 |
| N22 | first-paint / ready barrier | active | **C** | Condition: a home for the opt-in first-paint policy (candidates: a RuntimeDirectives directive — additive slot established in §6.1 (B-11) / leaf-I41's RuntimeConfig / Axis A home — composition RFC per B-9). An opt-in addition point preserving the INV-LC4 default is unimplemented (§1.6). **INT-8 sharpening**: the policy gates render output only and does not suppress reconcile/re-evaluation/admission (0012 §4) — avoids deadlock when the paint-wait condition is subscription-derived. owner: first-paint policy RFC — blocks composition = no / gates the composition RFC = no |
| N23 | Graceful shutdown + deadline | active | **C** | Condition: graceful-drain RFC (P5/P6 delegation slot). 0011 §4.5 already fixes the insertion point and postcondition preservation as a frame. owner: graceful-drain RFC — blocks composition = no / gates the composition RFC = no. INT-2 passed with 3 sharpening points |
| N24 | Terminal reconnection / backend swap | active | **C** | Condition: N49-family driving surface + leaf-G36 backend seam. Within the run() path this is impossible due to INV-LC5 + run(self) consume. owner: driving surface RFC — blocks composition = no / gates the composition RFC = no |
| N25 | Supervision policy | active | **C** | Condition: a future supervision/rate RFC (adjacent to the 0012 §8 delegation slot, co-located with S8a). Cause vocabulary (0011 §4 / 0012 §3) is preserved, so "no policy = current behavior" holds. owner: supervision/rate RFC (S8a co-located) — blocks composition = no / gates the composition RFC = no. **INT-3 sharpening**: that RFC has a design obligation to choose exactly one of (a) passive rate limiting (with explicit statement that automatic retry is not guaranteed) or (b) ownership of an opt-in third trigger for backoff-expiry dirty+wake (0011 §2.1 opt-in extension + conformance to the wake contract). **Premise common to both forms**: accompanied by an amendment restricting the **re-admission half** of the immediacy clauses in 0005 INV-13 / 0012 INV-SE3, INV-SE5 (next-pass), and §4.3 to policy-off only (0012 §8 = its preserved/relaxable partition with the re-admission-only scope — the relaxation covers only re-admission of policy-targeted subscriptions (finished restart + replacement); pure first admission and non-policy-targeted subscriptions are preserved) |
| N26 | App restart after failure | active | **A** | 0011 termination postcondition + INV-LC3 (inert construction) make sequential restart contractually clean. Snapshots are additive via user-driven persistence |
| N27 | Resource finalizer / async cleanup | active | **C** | Condition: an RFC realizing the 0011 §4.5 drain slot + a cleanup hook seam (linked to the `cancel_scope` delegation, §5.3). No reserved seat for async cleanup (§1.6). **INT-6 sharpening**: the cleanup window can be placed inside the 0012 §3 stop-requested→quiesced interval (abort-only is not pinned) — cleanup delay defers admission via the barrier and is observable via gauge (same shape as a blocking source). owner: graceful-drain RFC — blocks composition = no / gates the composition RFC = no |
| N28 | External shutdown handle + state recovery | active | **A** | An opt-in handle to a dedicated quit channel + a run variant are additive preserving INV-LC5/LC9 (conditions made explicit in 0011 §6). Keep the form that exposes no bare sender |
| N29 | CPU-bound / blocking command | active | **A** | Expressible under the current stream contract. Non-guaranteed stopping of detached work is consistent with AC6/K-55; quiescence covers runtime-owned work only. **INT-2 has passed** |
| N30 | Structured child tasks | active | **C** | Condition: cancel_scope RFC (delegated per §5.3, pre-bound by 0005 §4.5). In-stream structured concurrency (`FuturesUnordered` etc.) is expressible under the current A-1 topology; the runtime scope tree is owned by that RFC. owner: cancel_scope RFC — blocks composition = no / gates the composition RFC = yes (inherits the C-16 cancel_scope delegation's 0005 §4.5 precedence constraint) |
| N31 | Effect algebra | active | **A** | sequence/race/limit can be layered as modifiers under root-B7's Axis B canonical form (effect/stream transformation — same shape as 0004). A-1 unified topology (single task_policy) unchanged; no separate executor/FSM needed. **INT-11 pass** — 2 sharpening points (metadata fold is INV-11-shaped uniformly across combinators / pinning the win condition belongs to the combinator RFC) |
| N32 | Streaming effect + backpressure | active | **A** | Backpressure already applies via the 0006 bounded contract. Progress coalescing is source-side policy (consistent with INV-L15) |
| N33 | External message sender bridge | active | **A** | Carried by the existing contract as a subscription source (turning a receiver into a stream). `Send` bounds suffice for cross-thread use — no renegotiation of root-I40 needed |
| N34 | Transactional / compensating effect | active | **A** | Rollback/undo messages are an app pattern expressible under the current stream contract. The framework does not promise exactly-once (boundary confirmation consistent with AC6/K-55) |
| N35 | Non-pausable push sources | active | **A** | Carried by 0012 §2 (tolerance to arbitrary poll pacing) + §7 (drop/coalesce policy in source-internal state) + the source-specific delegation seam of 0006 §4.2 (owner/information flow unchanged) |
| N36 | Fallible subscription startup | active | **C** | Condition: an opt-in observation surface for startup-failure cause belongs to the future supervision RFC (same slot as N25). The current form is carried by 0012 §2/§3 (failure = a source that quiesces immediately; cause unobserved). owner: supervision RFC — blocks composition = no / gates the composition RFC = no |
| N37 | Live source reconfiguration | active | **A** | New identity = replacement of the 0005 structural ID (under the 0012 §4 barrier). The same-lifecycle form is limited to a concrete shape on the existing information flow: configuration changes are sent via existing effect paths (e.g. Command) to a control channel the source itself owns (e.g. a receiver taken at construction) and applied within the source boundary (INV-SE7) — the general form of directly mutating a public handle from update is forbidden by 0012 §7, and under the same ID there is no spawner re-execution (0005 INV-12), so changes to declaration arguments do not reach the existing stream |
| N38 | Multiplexed shared source | active | **A** | Expressed via 0012 §7 shared state inside the source boundary (the 0001 cell is precedent). The runtime's 1 ID = 1 forwarding task is unchanged; per-subscriber lifetimes are separated via 0005 identity |
| N39 | Source health / liveness event | active | **A** | Additive in-band (a health variant in the source stream's item type). The tracing path is an optional instrument under the own-instrument carve-out (INV-L15), subordinate to leaf-I42's schema judgment but verdict unchanged. No new control plane needed (B-11 closedness preserved, §6.1) |
| N40 | Child appear/disappear teardown | active | **C** | Condition: cancel_scope RFC (§5.3) + composition RFC (C-15 (a) automatic scope application). Consistency with the 0012 §4 barrier is already required by C-15 (f). **INT-6 sharpening**: teardown ordering is owned by the cancel_scope RFC and the adapter does not interfere (C-15 (f)); the cleanup window is within the 0012 §3 interval (shared with N27). owner: cancel_scope RFC + composition RFC — blocks composition = no / gates the composition RFC = yes (inherits the C-16 cancel_scope delegation's 0005 §4.5 precedence constraint) |
| N41 | Scoped dependency lifetime | active | **C** | Condition: composition RFC (C-15 (a) automatic scope application). Root-only Flags DI + 0012 §7 source-internal state is the current form (per 0012 §9, no dedicated DI surface is added). owner: composition RFC — blocks composition = no / gates the composition RFC = no |
| N42 | Shared background service | active | **C** | Condition: composition RFC (lift into the parent scope — C-15 (a)/(f)). Expresses the reverse-direction lifetime with the 0012 execution contract unchanged. owner: composition RFC — blocks composition = no / gates the composition RFC = no |
| N43 | Dynamic feature enable | active | **C** | Condition: composition RFC (enum/dynamic reducer composition). The source side is already carried by INV-SE6 (declared set = pure function of state) + 0012 §4 admission. owner: composition RFC — blocks composition = no / gates the composition RFC = no |
| N44 | Cross-feature routing / broadcast | active | **C** | Condition: composition RFC (canonical home of routing/broadcast — C-15). The receptacle for forms reducible neither to N11 (wrapper) nor to N10 (parent state). owner: composition RFC — blocks composition = no / gates the composition RFC = no |
| N45 | Fault injection | active | **C** | Condition: injection at runtime-internal seams (send failure, full queue) is subordinate to the N48 conformance kit slot (leaf-G36 remainder). The source-seam portion is carried by 0012 §6 (mock source = template-conformant). owner: conformance kit RFC (N48 frame) — blocks composition = no / gates the composition RFC = no |
| N46 | Deterministic interleaving | active | **C** | Condition: design of a harness-side arbitration injection seam (§1.6) + retention of the 0008 §4.2 citation rule. Scheduler injection into production would revisit the unbiased premise and is not possible. owner: test-harness arbitration RFC — blocks composition = no / gates the composition RFC = no |
| N47 | Full-runtime transcript | active | **C** | Per leaf-I42: condition = a dedicated transcript RFC (design of the phase-machine-hook instrumentation seam). leaf-I42's stable surface is load-only and cannot compose this. The receptacle for the render/lifecycle reproduction sent over by INT-4. owner: transcript RFC — blocks composition = no / gates the composition RFC = no |
| N48 | Extension conformance kit | active | **C** | Decomposes within one row given leaf-G36: a source/backend conformance kit is additively constructible from the 0012 §6 template + render-step seam (G36 inventory). Condition = only the future-runtime-driver-conformance half (awaiting the N49 driving surface RFC). owner: driving surface RFC — blocks composition = no / gates the composition RFC = no |
| N49 | Host-driven step / poll | active | **C** | Condition: driving surface RFC. Non-reentrancy is already satisfied in advance by 0011 INV-LC9/§6 — remaining conditions are the step mappings of: the pacing/latency contract (INV-L4's step mapping is dominated by host cadence and cannot be discharged by construction), the parking premise, and **the bootstrap/termination paths (INV-LC4 intake point, INV-LC5/LC6 cause quantification)**. owner: driving surface RFC — blocks composition = no / gates the composition RFC = no |
| N50 | Minimal feature build | active | **X** | Per leaf-G35: a **minimal-profile contract for removing core components** (phase machine / channel and load layer + INV-L13 emission / identity / clock / TestStore + test-util / terminal driving surface / mock, signal, and time sources) **is not provided** (no renegotiation of the no-flag Accepted decision; no reserved seat under §1.6's headroom discipline). Feature-off for HTTP/WS remains possible as today; the phase-machine preservation axis is verified (the leaf-G36 portion is satisfied by the settled render-step seam) |
| N51 | Slow/reentrant observability consumer | active | **X** | Per leaf-I42: **tears does not provide** isolation of slow/panicking consumers, queue caps, overflow/backpressure, or panic containment — these are the responsibility of the tracing subscriber/exporter side. Implementation fact: dispatch is outside the lock but runs synchronously on the producer/drainer thread (load.rs:392), so a slow subscriber can stall the hot path; a panic persists until drainer state recovery. E-25's funnel redesign is limited to structural simplification preserving the synchronous dispatch semantics |
| N52 | Secrets / PII redaction | active | **C** | Per leaf-I42: **no common redaction framework is provided**. Condition = each content-bearing owner owns its redact/omit policy — the N12 persistence, N47 transcript, and N14 inspector RFCs, plus existing module tracing (subscription_id: subscription.rs:215 / keyed id: keyed_commands.rs:285 / WebSocket URL path: websocket.rs:252). The INV-L13 normative surface is content-free but does not cover the content-bearing tracing outside it, hence C rather than X. owner: each content-bearing owner RFC (N12 / N47 / N14 / module tracing docs) — blocks composition = no / gates the composition RFC = no |
| N53 | Per-tenant resource budget | active | **X** | Per the leaf-K55 judgment: no integrated task/queue/memory/CPU budget is provided (envelope canon = citation of 0006 R1/INV-L1/§4.5). Isolation of multiple runtimes is a process/host responsibility. N16's instance correlation belongs to the leaf-I42 side |
| N54 | System sleep / long suspension | active | **C** | Condition: a production suspend-time contract (0009 is only transparency to the platform monotonic; wall-time delay bounds and clock advancement during suspend are not guaranteed — either fill this in or pin it as negative space) + S8a (post-resume restart storm). The derivation on the timer non-catch-up side holds. Divergence is leaf-N57. owner: suspend-time contract RFC (with the S8a rate RFC) — blocks composition = no / gates the composition RFC = no |
| N55 | One state, multiple frontends | active | **C** | Condition ("backend abstraction" withdrawn as contradicting leaf-G36's non-design judgment): the existing `Runtime::run<B: Backend>` generic + render-step seam + the leaf-I42 observation surface / composition root-CMP seam. Input multiplexing goes via the N33 path; INV-LC9 is preserved even with multiple frontends. owner: composition RFC — blocks composition = no / gates the composition RFC = no |
| N56 | Native `!Send` executor | active | **X** | Same judgment as N17 (consequence of leaf-K56/AC3). INT-4 (d) holds independently of this consequence |
| N57 | Wall-clock / calendar time | active | **X** | Per leaf-N57: the core's normative time axis is only 0009's single monotonic (no amendment needed — 0009 §1.2 already places calendar time out of scope). Calendar needs are carried as an ordinary source (0012 §2). No second time axis and no clock-jump-tracking guarantee are provided |
| N58 | Untrusted content rendering (escape injection) | active | **X** | Per leaf-G36: tears does **not own** sanitization of untrusted text or a raw escape policy — the scope is fixed as an application/widget (ratatui)/backend responsibility. The raw passthrough surface (N5) is also X, so no new conflict surface arises |
| N59 | Time budget for slow reducer/view | active | **X** | leaf-K55 verdict: no execution-time cap or preemption for update/view — an infinite loop in a synchronous update never returns to a deadline observation point, so a hard budget cannot be enforced while preserving serial transitions (AC5). Post-completion overrun observation belongs to the P8/leaf-I42 side |

### 9.2 Interaction rows — INT series

| ID | Fixture | Status | Verdict | Note |
|---|---|---|---|---|
| INT-1 | priority × keyed cancellation × shared-first | active | **excluded** | excluded_by=N8 (priority not provided under INV-L15 — the breaking term vanishes; same shape as INT-10). shared-first + cancel-before-delivery already holds under the current contract (confirmed by replay). Reopen the joint question only if an INV-14/L15 amendment introduces priority |
| INT-2 | graceful shutdown × non-cooperative CPU work | active | **pass** | Joint walk verified — the key is the runtime-owned-only quantification in 0011 §4.4. No new conditions; 3 sharpening points into N23's drain RFC condition (quantification inheritance / single discard rule / D caps only the graceful wait — teardown duration, process exit, and detached work are out of scope) |
| INT-3 | supervision × backoff × startup failure | active | **pass** | Joint walk verified. Natural-termination quiescence does not mark dirty, so a crash-loop cannot self-drive message-less re-evaluation, but **cadence is proportional to input frequency (unbounded)** — storm bounding holds via S8a's passive rate limit. Guaranteeing automatic retry requires S8a to own backoff-expiry dirty+wake as an opt-in third trigger (opt-in extension of 0011 §2.1 + conformance to the wake contract). Sharpening → N25/S8a conditions |
| INT-4 | replay × local execution × arbitration | active | **excluded** | excluded_by=N56 (representative ID — §1.7's singular excluded_by form; N17 is the same-rooted leaf-K56 verdict). Under the unified rule, the local execution leg is X, so the joint question loses its subject. The N12 transcript-ordering analysis (snapshot as source of truth, no ordering-contract claim, bound independence) is retained as sharpening on the N12 row |
| INT-5 | multiple runtime × shared service × tenant budget | active | **excluded** | excluded_by=N53 (unified rule — the budget leg is X, so the three-way joint question loses its subject). The remaining N16 × N42 process-global inventory is kept for reference; if independent audit value arises, file a new INT append-only |
| INT-6 | child disappear × async cleanup × cancel_scope | active | **pass** | Joint walk verified — 0012 §3 does not pin the stop mechanism (the cleanup window can sit within the stop-requested→quiesced interval); cleanup delay is gauge-observable via the barrier (same shape as a blocking source); C-15 (f) keeps the adapter non-interfering. 2 sharpening points into the N27/N40 conditions |
| INT-7 | non-pausable source × high-frequency input × bounded mode | active | **pass** | Joint walk verified — internal buffers are outside the envelope (declared negative space of leaf-K55/0006 R1; bounded ≠ process-memory bounded); the source-internal drop policy (0012 §7) is the only place for a memory bound. N1's coalesce is an independent layer with no interference |
| INT-8 | async init × first paint × initial subscription | active | **pass** | Joint walk verified — placeable on the single phase machine via 0011 §3 bootstrap ordering + 0012 §4 (bootstrap admits immediately with no outstanding stop; the Loading→Ready swap is exactly an A→B→C sequence). No new conditions; 1 sharpening point into N22 (policy gates render output only) |
| INT-9 | host-driven loop × backend reconnect | active | **pass** | Joint walk verified — placeable on the single phase machine: non-reentrancy is structurally preserved by `&mut` exclusivity on the step surface; the source of truth for reconnect is 0012 §4 (quiesce → re-evaluate → admit, no new mechanism); for wake, the driver-non-fixed wording of 0011 §7 generalizes to the host. No new conditions; 5 sharpening points into the shared N49/N24 condition (driving surface RFC): flag consumption while detached / wake transcription to the host / restore on detach / reconnect = 0012 as source of truth / pacing preserved |
| INT-10 | inspector × slow observer × redaction | active | **excluded** | excluded_by=N51 (leaf-I42 being settled removes the blocker, and with N51 = X the three-way combination is intentionally not guaranteed). Confirmed out-of-scope: isolation = not provided (N51 X) / redaction = N52 C (owned by each owner, no unified guarantee) / observability = only the 3 INV-L13 kinds + instance field are normative — the settled state is that no unified contract guarantees all three |
| INT-11 | race × timeout/retry × cancellation | active | **pass** | Joint walk verified — the race is intra-stream; metadata attaches to a single composed command, and staleness suppression at the combinator layer (loser drop) and the 0003 layer (keyed delivery lifecycle) do not collide. No new conditions; 2 sharpening points into N31 |
| INT-12 | minimal build × TEA facade × composition core | active | **excluded** | excluded_by=N50 (unified rule — the prerequisite removal set is X, so the question loses its subject; a reduced version scoped to HTTP/WS/TLS is a separate fixture). The reduced projection is recorded via the removal projection (§2.3, §1.5) and the N50 row's preservation axes |

### 9.3 Anti-catalog decisions — AC series

| ID | Fixture | Status | Verdict | Note |
|---|---|---|---|---|
| AC1 | No hot code reload | active | **adopted(X)** | Hot reload not provided — not creating reserved seams (type erasure, reentrancy boundaries) preserves the simplicity of the phase machine / single state owner. Dynamic enable within a static set is split out to N43 (C) |
| AC2 | No multi-process / distributed state synchronization | active | **adopted(X)** | Out-of-responsibility declaration — same axis as AC7 (no actor-ization). Multiple runtime instances may coexist (N16 = A; 0006 requires a distinct runtime_id per runtime in the process). The negative space is that **no cross-instance / cross-process synchronization or consistency protocol is provided** — each runtime's state remains its own single owner's |
| AC3 | No support for non-tokio async runtimes | active | **adopted(X)** | leaf-K56 verdict: no public compatibility surface for alternate executors; `Send` bounds are kept and the current implementation may depend on Tokio (Tokio-internal types are not made contractual — consistent with the shape non-pin of 0011 §7). The P12 promotion verdict is completed on the P12 row |
| AC4 | No bundled general-purpose widget library | active | **adopted(X)** | leaf-G35/G36 settled: widgets are delegated to the ratatui ecosystem; a contour declaration that tears focuses on the runtime contract — same root as the X of N5/N58 |
| AC5 | No parallel execution of reducer/update | active | **adopted(X)** | 0011 INV-LC9 pins serial, non-reentrant execution as the source of truth — declaring parallel update unsupported is what actually yields the single-state-owner simplification |
| AC6 | No exactly-once / auto-rollback guarantee | active | **adopted(X)** | The A verdicts of N34 / P15 already hold inside this negative space (no exactly-once execution, no automatic rollback, and no atomicity between external effects and message delivery — retries may run non-idempotent external effects more than once, RFC 0004 §3.2). Adding such guarantees would require a full redesign of 0012's execution contract, hence adopted as negative space |
| AC7 | No general-purpose actor system | active | **adopted(X)** | 0011 §5 settles the supervision contour as "up to panic isolation; typed cause is non-contractual (only the room is preserved)" — actor-ization is a non-goal |
| AC8 | No public scheduler plugin API | active | **adopted(X)** | Per the root-SCHED confirmation (INV-14 unification, fairness rejected, INV-L15), the scheduler is a fixed mechanism — keeping plugins non-public preserves the simplicity of the statistical contract |
| AC9 | No dynamic native plugins / stable ABI | active | **adopted(X)** | Stable ABI not provided. Static enum composition is split out to N43 (C: composition RFC) — this row is the negative-space declaration for the dynamic-loading side |
| AC10 | No `no_std` / bare-metal | active | **adopted(X)** | Consequence of N50 = X (reduction is limited to within `std`) + leaf-K56 (tokio assumed) — being able to assume `std`/tokio underpins the simplicity of the subscription execution system |
| AC-E1 | Direct import of SwiftUI-style observation | active | **adopted(X)** |  |
| AC-E2 | macro-heavy DSL | active | **adopted(X)** |  |
| AC-E3 | Timer jitter (P14) | active | **adopted(X)** |  |
| AC-E4 | Dual runtimes (TEA/TCA side by side) | active | **adopted(X)** |  |
| AC-E5 | Direct import of the TCA dependency registry | active | **adopted(X)** |  |

### 9.4 Feature fixtures — P series (P4 was never assigned)

| ID | Fixture | Status | Verdict | Note |
|---|---|---|---|---|
| P0 | HTTP refetchInterval | active | **A** | The declarative interval on the query side is expressible with 0009 Timer + 0001 cell (a feature inside the HTTP module). Distinct target from S8a (subscription restart rate, delegated to 0012 §8) |
| P1 | HTTP retry/backoff policy | active | **A** | Additive via 0004's effect-local retry/backoff (Axis B stream transformation as source of truth) + 0001 query integration. Distinct target from S8a; only terminology is aligned. The randomness (jitter) reproducibility verdict lands here as the recipient of the 0009 §1.2 delegation (routing from TS-5) |
| P2 | debounce/throttle wrapper | active | **A** | Additive as a stream wrapper (Axis B transformation / 0012 §2 template combinator) — consistent with implementation progress on track B |
| P3 | HTTP prefetch / refetch(key) | active | **A** | Expressible as additional methods on `QueryClient`'s cell operations (0001 §5). Owner / information flow unchanged |
| P5 | runtime lifecycle events | active | **A** | **Scope fixed: notification-only at phase boundaries (excludes final update delivery, drain, and control hooks — consistent with the assumption of additive events via default methods / a separate trait)**. 0011 canonicalizes the phase/termination vocabulary; §5.1 reserves future additive extension. A form that includes delivery is treated as a separate fixture on the N23/P6 drain-condition (C) side |
| P6 | WebSocket graceful shutdown | active | **C** | Condition: graceful-drain RFC (same condition and same frame as N23). owner: graceful-drain RFC — blocks composition = no / gates the composition RFC = no |
| P7 | fs watcher subscription | active | **A** | Same verdict as N6 (conforms to the 0012 §2 template) |
| P8 | performance profiling hooks | active | **C** | leaf-I42 settled: condition = confirm whether profiling requirements are met by the 3 INV-L13 kinds + instance field (any missing hooks sit with the N47 transcript RFC — leaf-I42 adds no surface). owner: N47 transcript RFC — blocks composition = no / gates the composition RFC = no |
| P9 | lazy subscription init | active | **A** | 0012 §2 already canonicalizes the lazy spawner (declaration has no effect; execution begins at start) + non-pinning of the resource-acquisition point — already contract |
| P10 | subscription batching strategies | active | **C** | Condition = a future batching/load-control RFC (via an RFC 0006 INV-L12 amendment, re-judged under the root-SCHED reopening rule). Current batch semantics (INV-L12, 100µs window) have 0006 as source of truth. owner: batching/load-control RFC — blocks composition = no / gates the composition RFC = no |
| P11 | command queue optimization | active | **A** | The unified model (b) includes O(1) bookkeeping. Optimization is mechanism under preservation of the observable contract. Order-changing optimizations hit the INV-L15 wall |
| P12 | custom runtime support | active | **C** | Decomposition terminates at the leaf-K56 verdict (consistent with §7.2's driving-right decomposition): the executor-replacement side = X via AC3 (no public compatibility surface); the external-driving side = subordinate to N49's driving surface condition (AC3 does not block it). owner: driving surface RFC — blocks composition = no / gates the composition RFC = no |
| P13 | lens/optics investigation | active | **C** | Condition: composition RFC (source of truth for scope/lens). Example-level work can proceed additively in advance. owner: composition RFC — blocks composition = no / gates the composition RFC = no |
| P14 | Timer jitter | excluded | **X** | Terminated by AC-E3 adopted(X) — the simplification of not providing a jitter-injection surface. Exclusion scope: determinism of timer delivery stays with 0009 §2 as source of truth |
| P15 | HTTP optimistic update | active | **A** | Same verdict as N34 (rollback-message pattern, within the boundary of no exactly-once promise) |
| P16 | Investigation into removing the ratatui dependency | active | **X** | leaf-G36 settled: the removal work is a non-goal — `run<B: Backend>` is already generic, so separability is preserved; the investigation is complete with the 5-item contamination inventory and the fixed render-step seam location (folded into the contour declaration of not designing a backend abstraction) |
| P17 | sqlx as a subscription | active | **A** | Same verdict as N6 (conforms to the 0012 §2 template) |
| P18 | features cleanup | active | **A** | leaf-G35 settled: feature cleanup within the §7.6 demarcation is contract-neutral cleanup (no profile contract is introduced, so the contract surface is unchanged; per existing feature semver practice). §10 backlog |

### 9.5 Feature fixtures — S / TestStore / TCA series

| ID | Fixture | Status | Verdict | Note |
|---|---|---|---|---|
| S8a | subscription restart rate control | active | **C** | Condition: delegation to an external RFC (a future opt-in rate policy RFC — 0012 §8 owns the frame; added delay is already spelled out as outside INV-L8). Owned task: draft the S8a RFC. owner: S8a rate policy RFC — blocks composition = no / gates the composition RFC = no. The randomness (backoff jitter) reproducibility verdict also belongs to that RFC (0009 §1.2 delegation, routing from TS-5). **INT-3 sharpening**: (a) if only a passive rate limit, state explicitly that automatic retry is not guaranteed / (b) if automatic retry is promised, own backoff-expiry dirty+wake as an opt-in third trigger (including an opt-in extension amendment to the closed set of 0011 §2.1) — an either-or design obligation. **Premise common to both forms**: an amendment restricting the **re-admission half** of the immediacy in 0005 INV-13 / 0012 INV-SE3, INV-SE5 (next-pass), and §4.3 to policy-off only (0012 §8 = its preserved/relaxable partition with the re-admission-only scope — the relaxation covers only policy-targeted re-admission (finished restart + explicit replacement); pure first admission (including bootstrap) and non-policy-targeted cases are preserved. Delay cannot be pushed outside the re-evaluation point). Storm bounding holds under (a) |
| TS-1 | Cannot drive subscription-originated input | active | **C** | The foundational half (source-side injection contract) is already drafted in 0012 §6; condition for the remaining half = the RFC 0008 stage-3 driving-store amendment (the delegation frame of 0012 §6.2). owner: RFC 0008 stage-3 amendment — blocks composition = no / gates the composition RFC = no |
| TS-2 | No seam for swapping effect I/O | active | **A** | Expressible via the existing Flags/environment path — subscriptions use source-side injection (0012 §6); no non-time effect-DI surface is created (0012 §9/INV-SE8 negative space). 0012 §9 itself defines the remainder as a docs concern, not API surface — docs/example work for the Flags convention goes to the §10 non-gate follow-up. **TS-5 backlink**: the docs condition for app-level clock injection (Flags convention) is also closed in the same §10 follow-up (recipient of the residual routing from TS-5 X) |
| TS-3 | HTTP Query cache behavior outside verification scope | active | **C** | Condition = the RFC 0008 stage-3 driving-store amendment only (the Flags side is already satisfied by TS-2 = A). owner: RFC 0008 stage-3 amendment — blocks composition = no / gates the composition RFC = no |
| TS-4 | No view/render assertions | active | **A** | Independent additive helper — addable on the existing render-step seam (leaf-G36 settled) and requires no contract change |
| TS-5 | App-level clock/randomness DI not in place | excluded | **X** | Explicit exclusion scope: **no general-purpose app-level Clock/Rng DI surface is provided** — DI for the non-time effect-execution surface is 0012 §9/INV-SE8 (not extended to the time axis); a time-axis Clock abstraction is already rejected in 0009 §2. Explicit routing of the remainder: (1) docs for app-level clock injection (Flags convention) = the §10 non-gate follow-up recorded on TS-2 (= A); (2) whether a calendar/wall-clock axis is needed = leaf-N57 (N57 row); (3) policy-local randomness reproducibility (retry/restart jitter) = judged on the P1 (0004 path) / S8a (rate RFC) side, per the 0009 §1.2 delegation |
| TS-6 | Subscription lifecycle not verifiable | active | **C** | Same condition as TS-1 — condition = the RFC 0008 stage-3 driving-store amendment (0012 §6 + 0012 §6.2). owner: RFC 0008 stage-3 amendment — blocks composition = no / gates the composition RFC = no |
| TCA-1 | Where the quit directive lives | merged | **C** | resolved_via: B-8. root-B7, root-A1. Derived from the B-8 verdict (delegation of quit preservation to the composition RFC under 3 conditions — §6.1). owner: composition RFC — blocks composition = no / gates the composition RFC = no |
| TCA-2 | Timing of Outcome exposure | merged | **C** | resolved_via: B-9. root-B7. Derived from the B-9 verdict (delegation of the Axis A terminal home to the composition RFC). owner: composition RFC — blocks composition = no / gates the composition RFC = no |
| TCA-3 | subscription re-eval policy directive | merged | **C** | resolved_via: B-10. root-B7. Derived from the B-10 verdict (delegation to an RFC 0002 extension RFC). owner: RFC 0002 extension — blocks composition = no / gates the composition RFC = no |
| TCA-4 | navigation / presentation state | active | **C** | Condition: composition RFC. Adjacent to N40/N43 but not merged. owner: composition RFC — blocks composition = no / gates the composition RFC = no |

## 10. Delegations and follow-up work

This chapter is normative (§1.2): it is the delegation register, the
anti-catalog adoptions, the non-gating follow-ups, and this RFC's
Implemented criterion.

### 10.1 Delegation register

Every audit row that terminated `C` names its owning task and both
§1.8 flags on its own row (§9). This register is the canonical
grouping of those owners into delegated documents — where §9 rows
name the same slot under slightly different working titles, the entry
below is the canonical name; each row is listed once per owning
entry, and the multi-owner rows N40 and N54 appear once in each of
their two entries. Per §1.8,
**every entry is `blocks composition = no`**: none of these documents
joins this RFC's acceptance bundle (the bundle is exactly the
Target's list). The only scheduling constraint is `cancel_scope`'s
`gates the composition RFC = yes`. The bundle's own documents
(RFC 0011, RFC 0012, and the RFC 0006/0007 amendments) are acceptance
material under §1.8, not register entries.

| Delegated document | Audit rows | Gates | Scope handed over |
|---|---|---|---|
| composition RFC — RFC 0014, Accepted; delegation discharged | N10, N41, N42, N43, N44, N55, P13, TCA-1, TCA-2, TCA-4, N40 (automatic-scope-application half) | no (vacuous — it is itself the gated document) | the §5.2 requirement set (a)–(f); parent-state/routing composition, scope application, the scope/lens canon, enum reducer composition, routing/broadcast, navigation/presentation state, multi-frontend seams; Axis A terminal home and quit surface (`B-8`/`B-9`, §6.1) |
| `cancel_scope` RFC (`C-16`, §5.3) — RFC 0013, Accepted; delegation discharged | N30, N40 (teardown half; jointly with the composition RFC) | **yes** — must precede or accompany the composition RFC (RFC 0005 §4.5); met by co-design, both accepted together | scope-tree ownership, teardown ordering, the six deferred `cancel_scope` questions (RFC 0005 §4.5) |
| driving surface RFC (with its subordinate conformance kit) | N7, N15, N24, N45, N48, N49, P12 | no | external driving of the runtime: pacing/latency step mapping, parking premise, bootstrap/termination intake, backend/terminal separation, the contour declaration's inventory of signals the driving surface exclusively holds (POSIX signal handling), source/backend conformance kit |
| RFC 0008 stage-3 driving-store amendment | TS-1, TS-3, TS-6 | no | driving-store test surface for subscription-fed input, lifecycle verification, and HTTP query-cache coverage (RFC 0012 §6.2's slot) |
| graceful-drain RFC | N23, N27, P6 | no | the RFC 0011 §4.5 drain slot: deadline-bounded shutdown, and what remains of the cleanup-hook seam now that RFC 0014 §4.4 takes the hook itself (registration, at-most-once firing at the teardown application point, termination discarding unfired hooks) — the drain window around it, the ordering of a cleanup against its scope's successor (§6.2's return), and any bound on cleanup completion |
| supervision/rate RFC (the S8a slot) | N25, N36, S8a; the restart-storm half of N54 | no | opt-in restart-rate policy in RFC 0012 §8's delegation slot (one of the two recorded forms, with the immediacy-amendment precondition), startup-failure cause observation |
| suspend-time contract RFC | N54 (production half) | no | production suspend-time behavior: fill or pin as negative space what RFC 0009 leaves open beyond monotonic passthrough |
| batching/load-control RFC | P10 | no | batching strategies beyond the current batch semantics, via RFC 0006 INV-L12 amendment; reopens `root-SCHED` per §1.9 |
| state-observation / transcript RFC (the N47 slot) | N14, N47, P8 | no | full-runtime transcript (phase-machine-hook seam), state/inspector observation surface, profiling hooks beyond INV-L13 |
| RFC 0002 extension RFC (`B-10`, §6.1) | TCA-3 | no | subscription re-evaluation-policy directive |
| RuntimeConfig input-coalescing policy RFC | N1 | no | opt-in terminal-input coalescing policy |
| terminal suspend/resume RFC | N4 | no | suspend/resume over the settled quiesce→restart, terminal-ownership, and suspend-entry closedness seams |
| runtime-config update RFC | N20 | no | post-startup configuration change: information flow, synchronization, public control surface |
| first-paint policy RFC | N22 | no | opt-in first-paint gating (home candidates recorded on the row; INV-LC4 default preserved) |
| test-harness arbitration RFC | N46 | no | harness-side deterministic-interleaving injection seam (production scheduler stays uninstrumented) |
| content-bearing owner RFCs (meta-entry) | N52 | no | redaction/omission policy, owned per content-bearing surface (the transcript and inspector documents above, any future persistence surface for N12's opt-in bounds, and module tracing docs) |

Name normalization: §9's working titles map onto these entries as
follows — N45's "conformance kit RFC (N48 frame)" and N48's "driving
surface RFC" are the driving-surface entry; N25's "supervision/rate
RFC (S8a co-located)", N36's "supervision RFC", and S8a's "S8a rate
policy RFC" are the single S8a-slot entry; N54's owner splits between
the suspend-time entry and the S8a-slot entry as its row records;
N14's "state-observation RFC (seated with the N47 transcript RFC)"
is the N47-slot entry. The per-row flags in §9 are unchanged by this
grouping.

### 10.2 Anti-catalog adoptions

The fifteen `adopted(X)` decisions are normative negative space: each
names the simplification it buys, and reversing one is a contract
change on the owner surface it protects (reopening per §1.9 where a
root is named).

- **AC1 — no hot code reload.** No reserved type-erasure or re-entry
  seams; the phase machine and the single state owner stay
  monomorphic.
- **AC2 — no multi-process / distributed state sync.** No
  cross-instance or cross-process synchronization or consistency
  protocol is provided; multiple runtime instances may coexist
  (N16; RFC 0006 requires a distinct `runtime_id` per runtime), and
  each runtime's state remains its own single owner's.
- **AC3 — no non-Tokio executor surface.** No public compatibility
  surface for alternate executors; `Send` bounds are kept; the
  implementation may depend on Tokio, whose internal shapes stay
  unpinned.
- **AC4 — no bundled widget library.** Widgets stay with the ratatui
  ecosystem; tears remains a runtime-contract crate.
- **AC5 — no parallel `update`.** Serial, non-reentrant update
  (RFC 0011 INV-LC9) keeps the single state owner.
- **AC6 — no exactly-once execution, no automatic rollback, and no
  atomicity between external effects and message delivery.** Retries
  may run non-idempotent external effects more than once (RFC 0004
  §3.2), while a retry chain still yields at most one final message.
- **AC7 — no general actor system.** The supervision contour is
  capped at panic isolation; typed causes stay non-contractual.
- **AC8 — no scheduler plugin API.** The scheduler is a fixed
  mechanism; its statistical contracts stay simple.
- **AC9 — no dynamic native plugins / stable ABI.** No dynamic
  loading surface; composition stays static (the enum path is the
  composition RFC's, §10.1).
- **AC10 — no `no_std` / bare-metal.** The `std` + Tokio precondition
  is preserved; build reductions stay within `std` (N50).
- **AC-E1 — no SwiftUI-style observation import.** No
  observation/dependency-tracking layer; invalidation flows through
  explicit messages and the declared-set re-evaluation (whose purity
  RFC 0012 INV-SE6 pins), with RFC 0001 §5.5's `invalidate()` as the
  sole recorded deviation.
- **AC-E2 — no macro-heavy DSL.** The public API stays plain Rust
  traits and types; there is no macro surface to version.
- **AC-E3 — no timer-jitter injection surface.** Timer delivery
  determinism stays RFC 0009's; P14 terminates on this decision.
- **AC-E4 — no dual runtime.** One runtime semantics; the
  TCA-comparison imports (the TCA rows) enter only through the
  audited delegation slots above.
- **AC-E5 — no ambient dependency registry.** DI stays the Flags /
  environment path plus source-side injection (RFC 0012 §6).

### 10.3 Non-gating follow-ups

None of these conditions acceptance; they are tracked here so the
Implemented criterion (§10.4) can close over them.

- Pending-work test-path consolidation (§8.2): move the test path
  onto transition methods or a test-only helper, privatize the
  fields, sync the doc comments.
- Tokio feature-set trim (§7.2) and feature-inventory tidying (§7.6).
- Flags-convention documentation and example, including app-level
  clock injection — the docs remainder TS-2 records and TS-5 routes.
- Informative rustdoc cross-references from owner RFCs for the
  shared-vocabulary concepts (§8.1).

### 10.4 Breaking budget, and from Accepted to Implemented

Breaking budget: no delegated `B` entered a breaking budget — §1.8's
third dispatch arm was never exercised. The bundle's own breaking
changes are already carried on the 0.11.0 budget as baseline (§1.8),
enumerated in §2.4.

This RFC becomes Implemented when the work §10 enumerates is closed:

- each §10.1 entry's document is Accepted or explicitly withdrawn,
  with the disposition of its audit rows recorded;
- code conformance to the redesigned contracts (the bundle's
  amendments) has merged to main on the 0.11.0 breaking budget
  (§1.8);
- the mechanism unification §3.1 adopts has landed on main and
  passed §3.1's preservation criterion (contract suites green, no
  contract test rewritten, no grade-(i) counterexample) — the
  `root-A1` rows §8 reaffirms hold on the unified mechanism, not
  merely on the pre-unification paths;
- the simplifications promised by `X` and `adopted(X)` verdicts are
  applied, and no reserved seam excluded by §10.2 has been
  reintroduced;
- the §10.3 follow-ups are done.

## 11. References

- RFC 0001 — HTTP module redesign: §5.5, the scoped update-side
  deviation §6.2 keeps scoped.
- RFC 0002 — redraw suppression: the redraw OR-fold and directive
  independence §2.1 summarizes; §9, the axis canon §6.1 reaffirms.
- RFC 0003 — command cancellation: INV-1/INV-2–INV-16, INV-9, INV-10,
  INV-14, §4.4, §7.3.
- RFC 0004 — command timeout and retry: the per-effect transformation
  form §6.1 canonicalizes.
- RFC 0005 — structural lifecycle identity: INV-7, INV-8–INV-13,
  INV-14–INV-21, §4.5, and the identity surface of §2.2.
- RFC 0006 — runtime load control: R4, §4.3, §4.5, §4.7, INV-L4,
  INV-L8, INV-L9/INV-L10/INV-L11, INV-L12, INV-L13,
  INV-L14/INV-L15.
- RFC 0007 — RuntimeConfig: §2.1 (the derive decision §7.5 records)
  and §5.2 (the valid-trial predicate behind §7.8).
- RFC 0008 — TestStore: INV-T1 (the `Message` canon §7.1 keeps),
  §1.2, §3.3, §5.2.
- RFC 0009 — Clock DI: the single time axis; §1.2 and §5.1.
- RFC 0011 — runtime lifecycle: §2, §3, §4, §7, INV-LC1–INV-LC9.
- RFC 0012 — subscription execution: §2, §4, §5 (INV-SE6), §6, §8,
  §9 (INV-SE8).
- RFC 0014 — reducer-first core: §2, §3, §6, §8, §9 (the supersede
  destination the §1.9 return entries in §3–§7 name).
- `docs/rfcs/pre-review-checklist.md` — the review method §1 builds
  its gate on.
