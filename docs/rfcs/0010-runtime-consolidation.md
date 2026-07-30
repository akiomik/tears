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
- **Reference contract**: the union of three sets, each defined
  separately —
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
removing **surfaces the audit itself classifies as optional or
support** — a surface the audit rules an unconditional constituent (or
whose removal it excludes by an `X` verdict) is outside the
projection's quantifier, and the exclusion is recorded on the audit
row, not assumed.

- Disabling sources, backends, or observability the audit classes as
  optional must leave the same core owners and event topology.
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
  whether it blocks the composition RFC.
- **No unprocessed `B`.** Every in-scope `B` is dispatched one of three
  ways: (a) the affected root judgment is reopened; (b) the row is
  demoted to `X` with the reason and the simplification actually
  gained; or (c) it is delegated with its cost entered against a
  breaking budget.
- **`B`'s criterion** is "breaks the *post-consolidation* contract".
  Changes already carried on the 0.11.0 breaking budget (the
  composition work itself included) are baseline, not `B`.
- **Bundle acceptance.** This RFC is Accepted only simultaneously with
  every contract document marked `blocks composition = yes` in §10. If
  an amendment's semantics change during the bundle review, the change
  is a counterexample and returns to the audit before acceptance.

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

The pre-drafting gate completed 2026-07-30. Part 1 scanned upstream
`main` over `daa8bd1..89ea872` and found the
baseline advance had touched only the dependency lockfile — zero
contact with the scanned paths, so no audit rows required re-running
from the code delta. Part 2 swept all eleven RFCs, the RFC index, and
the code documentation they cite (the `RuntimeConfig` rustdoc, the
load-harness comments); findings were fixed in place per document,
then an independent cold-read rescan of the full corpus — split
across two reviewers — surfaced three residual instances, whose fixes
were verified by section-whole sweeps and a full re-read of the
affected RFC; the gate closed at zero remaining findings. The pre-acceptance gate runs **both
parts** again over the final bundle before the simultaneous acceptance
of §1.8.

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
     terminates instead (RFC 0011 §4.2) — subscription re-evaluation
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
| task body policy (panic capture, send handling, quit translation) | duplicated across three task kinds | the *definition site* is unified into a single owner module; the sink shape, quit translation, panic-log form, and completion reporting stay per kind (*mechanism*) | panic containment: RFC 0011 INV-LC8; keyed-panic log occurrence: RFC 0003 §7.3 |
| frame ownership | `FrameScheduler` + `PendingWork` + runtime | unchanged; parking premise informative | RFC 0011 INV-LC1/INV-LC2 and §7's premises |
| gauges / load events | `LoadObserver` funnel; guard-based and count-based gauges | the gauge-transcript-identity gate passed (2026-07-28), so the entry-owned RAII guard shape is the consolidated mechanism choice (*mechanism*) | RFC 0006 INV-L13 (schema, `runtime_id`/`seq`) either way |
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

*Stub — verdict chapter for unifying the unkeyed/keyed execution
mechanism — one spawn, task-ownership, and exit-reap path, per-id
lifecycle entries for keyed runs only — with the two delivery classes
preserved; contract impact rides RFC 0003 and RFC 0006.*

## 4. Lifecycle and termination

*Stub — verdict chapter for the lifecycle phase machine and the
two-route termination model; the contract body is owned by RFC 0011.*

## 5. Identity and the composition axiom

*Stub — verdict chapter for the composition-owns-the-identity-boundary
axiom and the requirements handed to the composition RFC; identity
contract bodies stay in RFC 0005.*

## 6. Effect and subscription execution boundary

*Stub — verdict chapter for source execution, the quiescence barrier,
and the effect-DI negative space; the contract body is owned by
RFC 0012.*

## 7. Public API boundary and contour

*Stub — verdict chapter for the `Message` bound freeze and the
leaf-level API decisions (RFC 0007's derive set, RFC 0006's gauge
instance field).*

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
task and blocks-composition flag, and the post-bundle follow-ups.*

## 11. References

- RFC 0002 — redraw suppression: the redraw OR-fold and directive
  independence §2.1 summarizes.
- RFC 0003 — command cancellation: INV-1/INV-2–INV-16, INV-10, INV-14,
  §4.4, §7.3.
- RFC 0005 — structural lifecycle identity: INV-8–INV-13 and the
  identity surface of §2.2.
- RFC 0006 — runtime load control: R4, INV-L4, INV-L8, INV-L9/INV-L10,
  INV-L12, INV-L13.
- RFC 0007 — RuntimeConfig: the derive-set decision §7 will record.
- RFC 0009 — Clock DI: the single time axis.
- RFC 0011 — runtime lifecycle: §2, §3, §4, INV-LC1–INV-LC9.
- RFC 0012 — subscription execution: §4, §5 (INV-SE6), §8, §9.
- `docs/rfcs/pre-review-checklist.md` — the review method §1 builds
  its gate on.
