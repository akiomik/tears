# RFC 0010: Runtime Consolidation

- Status: Draft
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
     contracts change semantics, and which are Draft for exactly that
     reason; and
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
  except through the verdicts §3–§7 state.
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
runs **both parts** again over the final bundle before the
bundle acceptance of §1.8.

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

- **Reaffirmed 523 / redesigned 8 / delegated 0 / open counterexamples
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
(RFC 0005 INV-7). **Delegated**: `cancel_scope` — owned by its own
future RFC; **blocks composition: no** (this consolidation only sets
its direction, so the document does not join this RFC's bundle);
**gates the composition RFC: yes** (it must precede or accompany that
RFC per (e) — a scheduling constraint on the composition RFC, §1.8's
second flag). **Contract impact**: none now; implementation lands in
the future composition and `cancel_scope` RFCs. **Reopen targets**:
counterexamples breaking the identity model or requiring its
re-migration reopen `root-CMP`; none has occurred.

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
frame). The `Action`-privatization ordering constraint was met by
completion.

**Conditions and delegations.** Axis A's terminal home (modifier vs. a
future update-outcome type) and the quit-directive surface question
are delegated to the composition RFC under explicit preservation
conditions: unkeyed quit's backlog-independent dedicated-channel
delivery (RFC 0006 R4/INV-L4), keyed quit's in-band cancellable
delivery (RFC 0003 INV-9; RFC 0006 INV-L10/INV-L11), and the
cancellation-metadata lowering path — no representation choice may
weaken any of the three. A re-evaluation-policy directive is delegated
to a future RFC 0002-extension RFC (blocks composition: no).

**Contract impact.** Semantics-neutral only: RFC 0002 §9's sync to the
settled forms. **Reopen targets**: axis-placement counterexamples
reopen `root-B7`; none has occurred.

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
build hygiene (§8 backlog).

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
keeps `Copy`.** Carried by the RFC 0007 Draft (its §2.1, `Changed`
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
tidying within this boundary is contract-neutral cleanup (§8
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
`runtime_id` — is carried by the RFC 0006 Draft amendment (INV-L13).
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

*Stub — the full row-level snapshot of §2.4's replayed ledger, with
per-row verdicts and typed grounds; also decides the glossary and
living-ledger questions (§1.2).*

## 9. Additivity audit (snapshot)

*Stub — the full fixture-manifest snapshot: every feature, interaction,
and anti-catalog row with its terminal verdict under §1.7's vocabulary,
demonstrating §1.8's gate.*

## 10. Delegations and follow-up work

*Stub — the delegation register: each delegated item with its owning
task and its two flags (`blocks composition`; `gates the composition
RFC` — §1.8's definitions), and the post-bundle follow-ups.*

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
- `docs/rfcs/pre-review-checklist.md` — the review method §1 builds
  its gate on.
