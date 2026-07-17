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

## 3. Enforcement class, declared up front

Every invariant states how it is checked at the point it is introduced,
using the established classes:

- **structural** — code review of specific sites (construction, send,
  spawn); the property is not observable from behavior.
- **behavioral** — a bench or integration scenario with a stated
  pass/fail criterion.
- **statistical** — trials with defined measurement conditions (count,
  load profile, percentile threshold).

Ask explicitly: *can a behavioral test distinguish a compliant
implementation from a non-compliant one?* If not, the primary check is
structural and any scenario is a regression check — say so, and do not
present the scenario as proof.

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

## 5. Re-derive, don't patch

When review finds a substantive defect (P1) in a clause, treat it as "the
clause's derivation is broken", not "one sentence is wrong": rewrite the
clause from its premises (the inventory, the requirements it serves), then
run items 1–4 on the result as if it were new text. Sentence-level patches
under review pressure are how one finding becomes a chain of findings.

## 6. Mechanical pass

- Cross-references: open-question numbers point at their resolutions;
  preamble/decision-scope status agrees with the body; the amendment
  header line is updated.
- `typos` and `git diff --check` are clean.
- English only (repository artifact).
