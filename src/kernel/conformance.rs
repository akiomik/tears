//! The kernel conformance suite.
//!
//! The series live here rather than in `tests/` because they drive the
//! kernel directly through the stage-3 driver, which is crate-private until
//! the switch. Each series file is named for the property it holds, and the
//! shared fixtures — the counter program, the mock source, gated effects,
//! and the bounded settle helpers — live in `support`.
//!
//! Two rules bind every file here:
//!
//! - **No sleep, no timer, no wall clock.** Waiting is bounded yielding, so
//!   a would-be hang becomes a failed assertion rather than a slow test.
//! - **The evidence surface is pass-unit driving.** Stage-granular probes
//!   bypass the fixed stage order, so tests that use them are white-box
//!   probes of one stage's mechanism, named with a `whitebox_` prefix and
//!   excluded from the same-topology acceptance evidence (RFC 0014 §7.2).
