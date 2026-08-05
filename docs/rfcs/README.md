# RFCs

Design contracts, invariants, and decision records for tears. When code
and an accepted RFC disagree, the RFC states the intended contract; fix
one or the other explicitly.

## What an RFC pins

An RFC fixes the *observable contract* — the guarantees and properties a
caller, subscriber, or composed feature can depend on — not the *mechanism*
that implements it. A lock, a snapshot, a data structure, a task layout is
out of scope; the property it exists to provide (a value is never lost, an
emission cannot deadlock a handler, an order is or is not guaranteed) is in
scope. The test for a candidate sentence: *could a conforming
reimplementation, reading only the RFC, pick a different mechanism and still
preserve everything a dependent relies on?* If yes, the sentence is
mechanism — leave it to the code and its comments. If a from-scratch
implementation could instead satisfy every stated word yet break something a
dependent depends on, the missing property belongs in the RFC.

Three corollaries:

- Pin a property's negative space too. When a guarantee holds only under
  conditions the current mechanism happens to cover — a value stays correct
  only because a snapshot is taken under a lock — state both what is
  guaranteed and what is not (e.g. per-event fidelity, but not cross-producer
  ordering), so a later mechanism change is measured against the contract
  rather than silently narrowing it.
- A property and its mechanism can carry different enforcement classes. The
  observable property may be behavioral where its scenarios are deterministic
  and structural where they are not (a concurrency-only guarantee reviewed at
  the seam that provides it); classify each part rather than the guarantee as
  a whole. See the [pre-review checklist](pre-review-checklist.md) items 2–3.
- A non-functional guarantee is pinned as an observable threshold, never as
  the mechanism that meets it. Latency, throughput, and memory footprint are
  mechanism-sensitive by nature, so state the bound a dependent may rely on —
  an acceptance criterion such as quit→delivered p99 ≤ 1 ms (RFC 0006 INV-L4)
  — and leave the channel capacity, queue shape, or task layout that reaches
  it to the code. The reimplementation test is unchanged: the number is the
  contract; the structure that meets it is not.

## Process

- Each RFC carries its own `Status` in its header — it is not duplicated
  here, so the file is always the source of truth.
- An RFC's body states the current contract, not its history. Resolutions
  of open questions and later amendments are edited into the body in
  place; the change history lives in Git, not in a header log. Dates
  appear in the body only where they aid reproducibility (the measurement
  reference date), never as a changelog.
- After an RFC is Accepted, any amendment to the contract it states is a
  reviewed change — treat it like a new RFC for review purposes.
- Before requesting review on a new RFC or an amendment, run the
  [pre-review checklist](pre-review-checklist.md).

## Index

| RFC | Scope |
| --- | --- |
| [0001](0001-http-module-redesign.md) | HTTP module redesign |
| [0002](0002-redraw-suppression.md) | Redraw suppression (message / redraw separation) |
| [0003](0003-command-cancellation.md) | Command cancellation, keyed delivery, delivery-order invariants |
| [0004](0004-command-timeout-retry.md) | Command timeout and retry |
| [0005](0005-structural-lifecycle-identity.md) | Structural lifecycle identity and composition scope |
| [0006](0006-runtime-load-control.md) | Runtime load control, backpressure, latency guarantees |
| [0007](0007-runtime-config.md) | RuntimeConfig public API, load-control acceptance parameters |
| [0008](0008-teststore.md) | TestStore: deterministic update/effect testing, `Message` boundary |
| [0009](0009-clock-di.md) | Clock DI: single-time-source rule, virtual-clock determinism contract |
| [0010](0010-runtime-consolidation.md) | Runtime consolidation: audit method and gate, reference architecture, verdicts |
| [0011](0011-runtime-lifecycle.md) | Runtime lifecycle: phase order, bootstrap, termination model |
| [0012](0012-subscription-execution.md) | Subscription execution: source template, quiescence barrier, effect-DI negative space |
