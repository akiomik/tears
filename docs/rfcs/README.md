# RFCs

Design contracts, invariants, and decision records for tears. When code
and an accepted RFC disagree, the RFC states the intended contract; fix
one or the other explicitly.

## Process

- Each RFC carries its own `Status` in its header — it is not duplicated
  here, so the file is always the source of truth.
- Resolutions of an RFC's open questions land as amendments to the RFC
  body itself (recorded in the header's `Amendments` line), not as
  separate documents.
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
