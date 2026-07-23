# RFCs

Design contracts, invariants, and decision records for tears. When code
and an accepted RFC disagree, the RFC states the intended contract; fix
one or the other explicitly.

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
