# RFC pre-review checklist

Run this before requesting review on a new RFC or an amendment to an
existing one. It exists because contract text fails review in predictable
ways: the checklist internalizes the reviewer's method — *construct an
implementation or execution that satisfies everything stated but violates
the intent, and cut the claims that pin no intent at all* — so those
findings surface before review instead of during it. It distills
the review of the RFC 0006 open-question-3 amendment (PR #211),
where three of five passes were preventable in hindsight.

This is a living document, and it prunes as well as grows. When a review
pass finds a defect class this checklist should have caught, add an item
(or a prompt under an existing item) in the same PR that fixes the
finding — but when the new prompt only restates a principle an existing
item already carries, fold it in rather than adding a peer, and when
several *From PR* examples have collapsed to one internalized principle,
keep the one or two that still teach the pattern and retire the rest. An
item earns its length by catching what its shorter form would miss;
length that no longer does is a defect class this checklist should catch
in its own text.

## 1. Quantifiers against the inventory

For every universal claim — *each*, *every*, *all*, *never*, *always*,
*bounded by* — enumerate the concrete set it quantifies over and check
each member. If the RFC has an inventory table (channels, producers,
tasks, send sites), walk every row; if it does not, the claim probably
needs one. Name the exceptions in the claim itself rather than leaving
them implied elsewhere in the document.

*From PR #211:* R1 said "each runtime-owned channel" while the same RFC's
§1.1 table listed the quit channel as unbounded by design.

A one-row implicature is the same defect in reverse: a claim true of one
inventory row used to characterize the whole aggregate it feeds. *From
PR #214:* "every user input travels the shared channel" (true of the
terminal-event row) was used to cast shared traffic as "exactly the
user's controls" — but subscription output and unkeyed command output
feed the same channel, and the argument built on the implicature (keyed
results deferred only behind user actions) was false. Check the converse
direction of every such claim against every row that feeds the same
aggregate.

Measurement and scenario tables are inventories too. An acceptance
condition that quantifies over "each scenario" is checked against every
row of the scenario table, including control rows whose intended behavior
differs from what the condition demands. *From PR #215:* INV-L4's
acceptance conditions quantified over "each `quit_*` scenario" — a set
that includes the keyed control `quit_keyed_backlog_50k`, whose ~1.3s
full-drain delivery is intended behavior — so the stated criterion could
never pass.

An execution profile is an inventory too. A claim that a scenario,
test, or check is *part of* a profile — a CI job, a smoke selector, a
feature-gated run — is checked by expanding the profile's actual member
set (the selector's argument list, the job's invocation) and finding
the named element in it. Being compiled is not being run: a scenario
built into the binary a profile invokes is compiled under that profile
without executing, so a rationale that needs the scenario to *run*
cannot lean on a profile that merely builds it. *From PR #221:* eight
`keyed_isolation` keys were justified as "cheap
enough for the smoke profile's build check" while the `--smoke`
selector's set never contained the scenario — and if the appeal was to
compilation instead, key count never affected build cost at all.

A definition by cases is an inventory too: enumerate the space the
definition partitions and check that the named cases cover it, and
treat an exception clause as quantified over every member of the
inventory it exempts. *From PR #240:* "deliverable — either
immediately, or because a test-controlled source has made it ready"
had no case for a self-waking future (neither immediate nor
externally completed), and a post-quit no-poll exception named only
the finish/drop checks while send/receive were also polling sites
that could reach a leaf on their way to failing.

A universal claim about executions quantifies over the behavior other
parties supply. A harness, runtime, or wrapper cannot promise a
property of the whole execution when part of the execution is the
caller's own code; scope the claim to what the mechanism itself
contributes. *From PR #240:* a determinism invariant promised
identical transcripts across "two executions of one test program" —
refuted by any application whose own `update` is nondeterministic —
and had to be re-scoped to "the store introduces no nondeterminism of
its own", with transcript identity conditional on a deterministic
application.

An enforcement instrument's member list is an inventory too. A
deny-list (a lint configuration, a grep pattern, a CI selector) that
claims to close a definition is derived by walking the surface the
definition quantifies over, never by listing the members the codebase
happens to use today; scope the derivation claim to the sub-surfaces
actually walked and name the delegated remainder. Closure is checked
per obtainment route: banning a type's constructor does not close the
type while a constant or a conversion still yields values whose reads
bypass the list. *From PR #242:* a three-method disallowed list
presented as enforcing "every time read" missed `UNIX_EPOCH.elapsed()`
(a wall-clock now-read with no `now` call), the `std` timed waits
(`park_timeout`, `Condvar::wait_timeout`, `recv_timeout`), and `std`
`Instant`s obtained through the virtual clock's `into_std` conversion;
the fix text then claimed the list was "derived by walking `std`'s
surface" while `std::net`'s nameable timed I/O was still absent from
it.

A universal negative over a set that contains the rule's own
instrument is checked against that member first. A contract that
elevates one sanctioned channel and then states "no member of the set
does X" quantifies over the sanctioned channel too, and either carves
it out explicitly or is refuted by it. *From PR #242:* "the crate has
no time-reading dependency" was refuted by the bench harness in
dev-dependencies, and its replacement "no dependency serves library
code as a time source" by the executor clock itself — the very
dependency the RFC pins as the single time source.

## 2. Adversarial counterexample per invariant

For each requirement and invariant, deliberately construct at least one
implementation or execution that passes every stated test yet violates
the intent. If one exists, strengthen the text or scope the claim; either
way, record the models considered and why each is excluded (an
"Adversarial models considered" note, inline or as a section).

Prompts that have already paid off:

- A pooled or shared-resource implementation behind per-unit interfaces
  (a global permit pool passing per-channel capacity checks).
- A scheduler adversary: unbiased `select!` tie-breaks, batching windows,
  poll ordering.
- A lifecycle edge: a task that terminates while its effects (queued
  messages, held permits) outlive it.
- A bounded test against an unbounded parameter: any finite scenario that
  saturates `j` units is passed by a pool larger than `j × capacity`.
- An admission-window adversary: with bounded channels, the interval
  between a consumer's pull and a blocked producer's re-send leaves the
  channel momentarily empty, so orderings impossible under unbounded
  sends become legal, compliant executions — an acceptance criterion
  derived from unbounded behavior may falsely flag them (from PR #213: a
  capacity-1 run delivers a keyed quit before the remaining producer
  backlog while violating nothing).
- A minimal-effort adversary against acceptance criteria and definitions
  of done, not only invariants: the implementation that does the named
  thing once, with arbitrary values, or only on the member the test
  happens to touch (from PR #215: an observability definition of done
  that asserted "each event fires with its required fields" — satisfied
  by emitting every event once with wrong values).
- An acceptance criterion whose parameters the measured implementation
  itself chooses: deferring a run's configuration, load, or trial counts
  to implementation time lets the implementer pick the values their code
  passes under, and an unscoped wall-clock threshold lets any
  sufficiently fast machine pass it (from PR #217: the bounded
  acceptance run deferred capacity/depth/trial count to implementation
  time, and the p99 ≤ 1 ms condition named no machine).
- A coincidental property of the current mechanism, silently depended on
  by other components because the RFC never named it as contract: check
  every consumer of the property under review, not only the ones the
  invariant text already lists, by grepping for the thing a from-scratch
  reimplementation would be free to drop. A change that satisfies every
  stated invariant can still break such a consumer if the coincidence,
  not the invariant, was what it actually relied on (from the RFC 0006
  §4.4 gauge-event seq amendment: an earlier attempt to move gauge-event
  emission off the lock satisfied the stated value-fidelity invariant but
  broke `benches/runtime_load.rs`'s teardown barrier and RFC 0007 §5.2's
  current-value read, both of which relied on arrival order coinciding
  with lock-serialized dispatch — a coincidence the contract had never
  pinned, undetected until grepped for and named explicitly as the `seq`
  field).
- A joint-satisfiability walk, dual to the per-invariant adversary:
  sketch one implementation that satisfies every invariant
  *simultaneously*, against the production seams as they exist in the
  code today. Two invariants can each be individually checkable while
  no implementation can honor both; and when the only satisfying
  implementation requires changing existing code first, that change is
  recorded as a named prerequisite under item 5's delegation rule, not
  assumed (from PR #240: one invariant required consuming the runtime's
  decomposition boundary while another required a per-leaf delivery
  order that boundary had already folded away into an unordered
  select — jointly unimplementable until a parts-type refactor was
  named as a prerequisite in the RFC body).
- A repeat-until check without a budget: any check that re-runs an
  operation until a condition holds needs its budget stated, or an
  adversarial input that never satisfies the condition (an
  always-self-waking, never-ready stream) runs it forever — and even
  when every real input terminates, an unstated budget lets two
  compliant implementations disagree on observable outcomes (from PR
  #240: the deliverability check had no poll budget until "each check
  polls each leaf its scan reaches exactly once, with wake-ups not
  honored within the call" was pinned).
- A behavioral test an invariant *prescribes*, checked against the other
  invariants and contracts — not only against the property it proves. A
  mandated test is itself an execution; run the joint-satisfiability walk
  on it (from PR #246: INV-T8's retention test scripted
  `send(Start); send(Cancel)` and then asserted the message was still
  deliverable, which INV-T7 and §5.1 make impossible — a cancelled
  occupant's output is gone; the retention test had to use a
  non-cancelling `send`, and the cancel case moved to INV-T7).
- A normative contract stated as *observable properties*, not as the
  implementation mechanism that happens to produce them. A clause phrased
  over a specific call (`.skip(1)`, an `interval` handle) is satisfiable
  by keeping that mechanism while breaking intent, and is falsely refuted
  by a conforming reimplementation that drops it (from PR #246: the
  `Timer` non-catch-up contract was reworded from "`.skip(1)` absorbs the
  elapsed deadline" to a `next_deadline`-relative property so a
  from-scratch timer still conforms).
- A multi-clause contract over one stateful object, checked where its
  clauses interact and with the *resulting* state pinned, not only the
  immediate output. Read each clause against the boundary input the
  others produce (from PR #246: a `Timer` contract said "a gap shorter
  than one interval yields no tick" and, separately, "the next deadline
  is the anchor-phase boundary after a miss" — at 3.5 ms on a 1 ms timer
  that boundary is 4 ms, 0.5 ms later, so the clauses disagreed on
  whether it fires; and "return one tick after a miss" left the post-miss
  cadence unpinned. Restating the whole contract deadline-relative fixed
  both).
- An ambient-facility adversary: a contract that requires some shared
  facility's *absence* (no executor, no reactor, no ambient clock) is
  checked against being invoked while that facility is already present
  and running, not only against the harness's own internal state
  machine — the facility being absent by default in a plain unit test
  is not the same as the contract *forbidding* its presence, and
  nothing stops a caller from providing it anyway. (From PR #247: RFC
  0008's "polling a time-dependent leaf fails the test" assumed the
  reactor's genuine absence without forbidding an already-entered Tokio
  runtime; entered, the same poll returns `Pending` instead of
  panicking, and the outcome becomes timed by the ambient runtime and
  real wall-clock progress instead of the documented fail-fast — fixed
  by adding a construction-time check that rejects the ambient facility
  outright rather than assuming it away.)

## 3. Enforcement class, declared up front

Every invariant states how it is checked at the point it is introduced,
using the established classes:

- **structural** — code review of specific sites (construction, send,
  spawn), for properties that finite behavioral tests cannot fully prove
  — a test can fail to refute such a property, but passing it is not
  proof.
- **behavioral** — a test at the narrowest layer that proves the
  contract (see [docs/testing.md](../testing.md)): unit or property
  tests for pure transition logic, internal state, and edge cases
  needing private access; bench or integration scenarios for end-to-end
  runtime behavior and public-API contracts. Either way, with a stated
  pass/fail criterion.
- **statistical** — trials with defined measurement conditions: count,
  load profile, percentile threshold, and the environment the numbers
  are defined on — the reference machine, the status of runs on other
  machines, and what CI does or does not gate. A wall-clock threshold
  with no environment scope is not yet a criterion: it is trivially
  passable on a fast machine and spuriously failable on a slow one.
  When the run's remaining parameters (configuration under test, load
  depths, trial counts) are not fixed in the same document, record who
  fixes them and that the acceptance run waits — parameters chosen at
  implementation time by the party being measured make the criterion
  self-certifying (from PR #217, where the checklist's own list of
  measurement conditions ended at "percentile threshold" and the text
  satisfied it while remaining unreproducible).

Ask explicitly: *can a behavioral test distinguish a compliant
implementation from a non-compliant one?* If not, the primary check is
structural and any scenario is a regression check — say so, and do not
present the scenario as proof.

Whatever the class, name the production seam the check goes through: the
concrete construction/send/spawn sites for a structural review, or, for a
behavioral test, the production code path it exercises or the transition
logic it shares with production. A test that exercises a parallel model
of the mechanism proves nothing about the runtime. And when the invariant
quantifies over several seams — *every* pull point, *every* send site —
the check covers each member: a non-compliant implementation can add its
bypass on exactly the seam the single test does not touch (from PR #213:
an "every `AppInputs` pull point" invariant tested only on
`try_next_ready` misses a quit-specific bypass in `poll_next`).

## 4. Code claims verified against code

Every sentence of the form "X holds because the code does Y" gets a fresh
read of the relevant code before the text is pushed — including the
concurrency schedule (who runs on which task, what happens between send
and receive). This applies with double force to text added while
responding to review: patch text tends to get less verification than the
original draft, and it is where false justifications creep in.

*From PR #211:* "quit occupancy is bounded by producer count" — false,
because a task terminates after `send` while its signal stays queued —
and a misdescription of `StreamMap::poll_next` as draining all ready
receivers.

The document's own derivations are citations too: when a clause credits
a mechanism with resolving a failure mode, re-read the section that
analyzed that mechanism — the analysis may already refute the credit.
*From PR #214:* §4.7 justified declining a fairness policy partly
because bounded mode "exists to exit" the overload regime, while §4.3
had already established that bounded capacity leaves shared readiness —
and with it keyed starvation — intact at any capacity; backpressure
bounds memory and shared latency, not keyed liveness.

A failure-mode claim about a mechanism — panics, blocks, hangs,
silently degrades — is checked against any execution model the same
document already pins for that mechanism, not asserted as a
free-standing plausibility judgment. *From PR #247:* a rationale
paragraph asserted that polling a still-pending time-dependent leaf
inside an ambient executor "never completes" — a hang — without
rereading the same document's own already-normative poll budget (every
check polls each reached leaf at most once and returns immediately, so
the store cannot block by construction); the actual defect was
nondeterministic later-delivery (a leaf turning up genuinely
deliverable on a later poll once real wall-clock time elapses), not
blocking, and the fix text changed accordingly.

Invariant citations are code claims too: "X is already carried by RFC
N's INV-M" gets a fresh read of INV-M's exact statement, and an umbrella
clause ("all RFC N invariants hold") covers only what RFC N actually
states. *From PR #213:* a resolution leaned on "INV-L5 carries RFC 0003"
for delivery-FIFO and quit precedence, but RFC 0003's INV-9 defines only
post-dispatch suppression and INV-14 only same-pull-point ordering — the
properties had to be pinned as new invariants with their own checks.

Negative citations are code claims too. A sentence claiming another
document *leaves a behavior open* — "RFC N does not guarantee X", "the
outcome is scheduling-dependent", "this depends on reap timing" — is
verified by searching that document for the passage that would pin the
behavior, never by failing to recall one; and re-reading an invariant's
*title* is not a fresh read of its statement. *From PR #240:*
review-fix text described a keyed-admission window as
reap-timing-dependent in the runtime, while RFC 0003's pre-spawn
reconciliation and the exact wording of INV-6/INV-7 pin a deterministic
release — the citation pass behind the patch had verified the invariant
labels, not the release semantics the paragraph leaned on, so the
document contradicted the contract it claimed parity with.

Measurement citations are code claims too, and they prove only what they
measured. Two classes from PR #220:

- An observed average or p50 supports an estimate, never an *at most*.
  Deriving a worst-case bound as `n × average cost` is invalid — the
  tail is unbounded by the mean — so either present the product as an
  estimate on the measured workload, or define and measure the
  worst-case service time the bound actually needs. (From PR #220: a
  capacity recommendation claimed "adds at most ~27ms" from the
  harness's average drain rate.)
- A value presented as measurement-driven cites a measurement that can
  *distinguish* the chosen value from the alternatives it rejects; a
  workload indifferent between the candidates sizes nothing. Either add
  the discriminating measurement, or state the value as a convention
  with its trade-off spelled out — not as measurement-derived. (From PR
  #220: `keyed_channel_capacity = 16` was justified by a scenario whose
  25ms-cadence probe never buffers two messages and thus cannot
  distinguish 16 from 1.)

Operational absolutes are code claims about a whole mechanism. A
normative *only*, *never*, or *cannot fail* over a workflow, harness,
or run profile is verified by enumerating every branch and every
failure exit of the mechanism it quantifies over — timeout guards,
exit-code paths, and abort conditions included, not only the assertions
the sentence has in mind. And when the absolute restates another
document's rule, it is checked against that rule's exact scope, which
the restatement may silently narrow or widen. *From PR #221:* "a slow
CI machine cannot fail the profile on speed" missed
the harness's per-scenario `max_wall` timeout-failure exit, and "full
scenarios run only on the reference machine" narrowed RFC 0006's
scoping, which admits full runs on any machine and reserves only
acceptance force to the reference machine.

A capability absolute about the enforcement tooling itself is the same
class: *the lint cannot name X* is verified against the tool's actual
expressiveness before X is delegated to a weaker check, and that
expressiveness includes the tool's escape hatches for its own error
modes — a suppression flag can keep an entry mechanical that a first
reading would delegate. *From PR #242:* socket timeout methods
described as something "the lint cannot name" are ordinary
`disallowed-methods` paths. *Corrected in PR #244:* that same round's
"only the platform-gated `std::os` siblings genuinely needed the review
half" was itself too weak — `disallowed-methods` takes a per-entry
`allow-invalid = true` that suppresses the unresolvable-path
diagnostic, so those siblings are mechanical entries too, enforced on
every CI target where their path resolves (the Linux gate covers
`std::os::unix`). The genuinely-delegated remainder shrinks to targets
CI never lints — an empty set today. Verify a capability absolute
against the tool's suppression and configuration surface, not only its
default diagnostic, before writing any review half at all; and name the
delegated remainder even when it is currently empty (item 1).

A behavioral claim about a dependency is a code claim, verified against
that dependency's documented semantics — thresholds, margins, and
"best-effort" hedges included — not its assumed behavior. *From PR #246:*
a `Timer` contract asserted no catch-up burst "because
`MissedTickBehavior::Skip` skips missed ticks", but Tokio's Skip engages
only once a tick is late past a fixed 5 ms margin, so sub-margin
intervals replayed a burst; and "ready on the next poll after `advance`"
was stronger than Tokio's `advance`, which documents that it does not
wait for the sleeps it moves past.

A "matches the runtime" or parity claim separates outcome-parity from
mechanism-parity, and the runtime step it names is verified to exist.
*From PR #246:* "the store skips the poll, matching the runtime, which
aborts the keyed task without sampling it" was false — the runtime
samples receiver facts before every `Spawn` regardless of policy; the
store's skip was justified instead by the sample not changing the
outcome. Separately, `redraw_requested` reading the init directive was
described as "what the runtime would read" although the runtime never
consults the init redraw directive at all.

A term naming a code moment — "construction", "start", "first poll",
"anchor" — is pinned to the exact site when two candidate sites differ
observably. *From PR #246:* "the timer's construction-time anchor" was
ambiguous between `Timer::new` (which only stores the interval) and
`Timer::stream()` (which builds the `interval()`); advancing time between
them changes the first deadline, so the text was fixed to
"stream-construction anchor" and given a test that advances time across
the gap.

"Behavior-preserving", "no-op", or "internal only" is an operational
absolute about the whole change: one observable difference refutes it,
and once refuted the header's `CHANGELOG` and `Scope` must record the
change (item 7). *From PR #246:* the `Timer` non-catch-up fix was an
observable behavior change for short-interval timers, so `CHANGELOG:
none` / "behavior-preserving" became `CHANGELOG: Fixed`, with the fix
added to `Scope` and its own implementation section.

## 5. Normative force and readiness

An RFC or amendment that gates implementation carries no soft spots in
its normative sections. Three scans, all from the first review round of
PR #215:

- **Hedge scan.** Grep the normative sections for *should*, *may*, *or
  similar*, *at least*, *for example*, *left to a separate task*, *when
  the implementation lands*, *at implementation time*. Each
  hit is tightened into a requirement, moved into explicitly
  non-normative rationale, or delegated — and a delegation is recorded
  in the RFC body as a named prerequisite (which task owns it, what it
  must fix, and that implementation waits on it), never left as an
  aside. (§3.2/§4.1 held the `RuntimeConfig` API at "for example" / "at
  least" with the owning task recorded nowhere as a gate; §4.4 specified
  observability as "or similar" / "should" with no definition of done.)

  Scope this to the claim itself: within a labeled invariant or requirement
  bullet (`- **INV-Lx**:` / `- **Rx**:`), write the opening sentence as the
  complete, hedge-free claim, and carry qualification, rationale, and
  resolved history in the sentences that follow within the same bullet.
  When triaging a hit, a hedge in that opening sentence is a finding; a
  hedge later in the same bullet usually is not — confirm it is doing
  rationale work, not smuggling an unresolved qualification into the claim,
  before waving it through.

  Deferral-to-implementation vocabulary is exempt from that scoping:
  *when the implementation lands*, *at implementation time*, and kin are
  findings wherever they appear — a matrix row, a consequence bullet,
  rationale — whenever what they defer is a parameter of an acceptance
  criterion, because they hand the decision to the party the criterion
  exists to judge. Distinguish deferring a *measured value* (filling a
  cell later is fine) from deferring the *conditions it is measured
  under* (a finding). (From PR #217: the deferrals sat in §5.1 rows and
  a §4.7 consequence bullet, outside every opening claim the scoped
  scan covers.)
- **No pending choices inside invariants.** An invariant that still
  contains a decision to make — "resolving this needs either (a) … or
  (b) …", "the remaining step" — is not yet an invariant. Either resolve
  the choice or state what resolves it and that implementation waits.
  (INV-L4 shipped as a two-way choice in an RFC otherwise presented as
  implementation-ready.)
- **Surface–invariant coverage.** Every element of contract surface the
  RFC introduces — configuration field, emitted event, public behavior —
  maps to at least one invariant with a declared enforcement class
  (item 3). Walk the surface list and name the invariant for each; an
  element whose semantics are defined nowhere (`batch_max_messages` had
  neither counting semantics nor a corresponding invariant) is a finding
  you can file yourself. A named invariant is not yet coverage: walk the
  mapping the other direction too, from each cited invariant back to its
  own exact statement (item 4's invariant-citation rule applies here),
  and confirm its stated scope actually reaches the surface element —
  not merely that the label exists and sounds adjacent. (From PR #247:
  `subscription_ids` was mapped to INV-T3 for coverage ("a pure
  observation ... covered by INV-T3"), but INV-T3's own text scopes it
  to `Command` decomposition, which subscriptions never pass through —
  the mapping named a real invariant that did not, in fact, cover the
  claim, and the fix added a dedicated invariant instead of stretching
  INV-T3's label over an unrelated surface.)

## 6. Re-derive, don't patch

When a review finding changes a clause's premises, scope, invariants, or
proof method — regardless of the severity label it carries — treat it as
"the clause's derivation is broken", not "one sentence is wrong": rewrite
the clause from its premises (the inventory, the requirements it serves),
then run items 1–5 on the result as if it were new text. Severity is
about impact, not about how deep the fix must go: PR #211's proof-method
gap (a scenario presented as proof of pool absence) was filed as a P2 and
still required re-deriving the invariant's entire enforcement story.
Sentence-level patches under review pressure are how one finding becomes
a chain of findings.

Patch text written in response to review is the highest-risk text this
item covers, and the checklist applies to it in full. *From PR #215:*
all three second-round findings — an acceptance condition quantified
over an unchecked scenario inventory, a definition of done a wrong-value
implementation passes, and a stale open-question description of a
decision the same amendment had made — were introduced by first-round
patch text and sit squarely in classes items 1, 2, and 7 already name.

Make the re-check mechanical rather than remembered: for every
review-fix commit, write out its changed-claims list — each claim the
fix adds, strengthens, or rewords — and run items 1–5 on exactly that
list as if it were new RFC text, before pushing. The list is the
enforcement; without it the pass silently shrinks to the sentences the
finding pointed at, and a small patch is precisely the patch whose
re-check gets skipped. *From PR #221:* two of the three findings sat
in claims PR #220's review-fix commit had added or reshaped
— the reference-machine-exclusivity sentence, and the rewritten
pass/fail derivation that carried "cannot fail on speed" into new
surroundings — and both would have appeared on that commit's
changed-claims list.

## 7. Mechanical pass

- Cross-references: open-question numbers point at their resolutions;
  preamble/decision-scope status agrees with the body; the amendment
  header line is updated.
- Citations of another document's invariants name things that actually
  exist there, and a corrected claim is corrected everywhere: grep for
  the old term across the RFC, its references section, and the index
  (from PR #213: "RFC 0003's FIFO" survived in R5, §4.3, the
  open-question text, and the references after the body had already
  conceded RFC 0003 states no FIFO invariant).
- A corrected claim's stale restatements rarely share its wording: grep
  for the claim's *subject* vocabulary across the whole document, not
  only for the sentence that was rewritten (from PR #240: rewriting the
  polling-site inventory left "It does not poll effects; polling
  happens in `receive*` calls" standing in the API-semantics section —
  found by grepping "poll", invisible to a grep for the rewritten
  sentence).
- A guarantee you deliberately *weakened or negated* is re-checked
  across every object that could re-assert it, by the *concept* removed
  rather than the old wording: the stale restatement is usually a
  positive claim in a different section that shares no vocabulary with
  the sentence you changed (from PR #246, twice: after §3.2 stopped
  fixing "which poll observes readiness", INV-C2 still said "ready on the
  next poll after one that does", and later the `Timer` contract
  re-fixed it as "a poll observing now ≥ next_deadline yields a tick" —
  neither reachable by grepping the §3.2 sentence).
- A decision that changes observable behavior is reflected in the header
  block: the `Scope` list, the `CHANGELOG` line, and any
  "behavior-preserving" / "no new behavior" claim in the summary or
  invariants. Grep the header for `CHANGELOG:` and "behavior-preserving"
  whenever a decision's behavior changes (from PR #246: a
  behavior-changing timer fix shipped for two review rounds with
  `CHANGELOG: none` and the fix absent from `Scope`).
- Resolving a decision is a corrected claim: grep for the decision's own
  vocabulary — its option labels ("(a)/(b)"), *pending*, *remaining
  step*, *the input for the choice* — across the findings, every open
  question's resolution text, and the amendment header (from PR #215:
  open question 8 still described F6 as the input to a choice the same
  amendment had already made).
- The PR title, body, and stated review focus match the final RFC text
  after the last fix commit: a claim corrected during review is
  corrected in the PR description too, and rationale or conclusions the
  fixes removed from the RFC do not survive in the body. Re-read the
  description at the end of each review round, not only when opening
  the PR.
- A present-tense claim about repository state — a lint entry, an
  allow attribute, a CI job — is checked against the repository as it
  exists; an artifact a named prerequisite will create is stated as
  landing with that prerequisite (from PR #242: "it carries an
  explicit lint allow at its measurement sites" described an allow the
  RFC's own migration prerequisite had yet to add, inside an inventory
  framed as "at this RFC's writing").
- Superseding another document's recorded expectation is a corrected
  claim even when the expectation was non-normative: when a decision
  replaces a sketch or shape another RFC recorded, name the
  supersession where the decision lands, so the later amendment
  reconciles against the decision rather than against two silently
  divergent texts (from PR #242: the no-clock-handle design
  contradicted RFC 0008 §7's "store-held clock handle" sketch until
  §5.1 named the supersession).
- `typos` and `git diff --check` are clean.
- English only (repository artifact).

## 8. Minimal contract

Items 1–7 all pull one way — toward a more defensible contract. Most name
a way some claim is too weak, too broad, unproven, or inconsistent, and
the fix adds, tightens, or corrects it; almost none asks whether a claim
should exist at all. So an RFC that runs the whole checklist converges on
*airtight* without ever converging on *small*. This item is the
counterweight, and it runs item 2's adversary in reverse: instead of the
implementation that satisfies the text while violating intent, construct
the *smaller contract* that pins the same intent, and record the reduction
the way item 2 records the models it excludes.

- For each invariant, requirement, and element of contract surface, ask
  what the rest of the contract fails to pin once it is deleted. If
  nothing, delete it: a claim the rest of the contract already implies is
  not a weaker claim to strengthen, it is one to remove.
- Two claims both correct where one is a special case of the other
  collapse to the general one. A redundant invariant is two statements a
  later amendment can drift apart — item 7's stale-restatement defect,
  introduced on purpose.
- A surface element whose only justification is that some implementation
  might want it (a configuration field, an emitted event with no
  invariant that needs its value) is not yet a contract; defer it to the
  RFC that needs it rather than pinning it now.
- Record the reduction as an *excluded claims* note (item 2's form),
  scoped to the claims this pass acted on, not to every clause that
  survives: each claim it drops, paired with the surviving claim that
  implies it — *INV-Z dropped; implied by INV-Y* — and each clause it
  suspected of redundancy but kept, with why the survivor does *not*
  imply it. A reader re-deriving from premises under item 6 then sees the
  dropped claim it would otherwise re-add, and sees the kept one already
  ruled non-redundant.
