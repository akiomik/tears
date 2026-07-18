# RFC pre-review checklist

Run this before requesting review on a new RFC or an amendment to an
existing one. It exists because contract text fails review in predictable
ways: the checklist internalizes the reviewer's method — *construct an
implementation or execution that satisfies everything stated but violates
the intent* — so those findings surface before review instead of during
it. It distills the review of the RFC 0006 open-question-3 amendment
(PR #211), where three of five passes were preventable in hindsight.

This is a living document: when a review pass finds a defect class this
checklist should have caught, add an item (or a prompt under an existing
item) in the same PR that fixes the finding.

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
- **statistical** — trials with defined measurement conditions (count,
  load profile, percentile threshold).

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

Invariant citations are code claims too: "X is already carried by RFC
N's INV-M" gets a fresh read of INV-M's exact statement, and an umbrella
clause ("all RFC N invariants hold") covers only what RFC N actually
states. *From PR #213:* a resolution leaned on "INV-L5 carries RFC 0003"
for delivery-FIFO and quit precedence, but RFC 0003's INV-9 defines only
post-dispatch suppression and INV-14 only same-pull-point ordering — the
properties had to be pinned as new invariants with their own checks.

## 5. Normative force and readiness

An RFC or amendment that gates implementation carries no soft spots in
its normative sections. Three scans, all from the first review round of
PR #215:

- **Hedge scan.** Grep the normative sections for *should*, *may*, *or
  similar*, *at least*, *for example*, *left to a separate task*. Each
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
  you can file yourself.

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
- Resolving a decision is a corrected claim: grep for the decision's own
  vocabulary — its option labels ("(a)/(b)"), *pending*, *remaining
  step*, *the input for the choice* — across the findings, every open
  question's resolution text, and the amendment header (from PR #215:
  open question 8 still described F6 as the input to a choice the same
  amendment had already made).
- `typos` and `git diff --check` are clean.
- English only (repository artifact).
