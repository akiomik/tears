//! The kernel conformance suite: RFC 0014 §13.1's twelve series.
//!
//! The series live here rather than in `tests/` because they drive the
//! kernel directly through the stage-3 driver, which is crate-private until
//! the switch. The shared fixtures — the scripted program, its journal, the
//! probe source, the gated effects, and the bounded waits — live in
//! [`support`], and the series files are named for the property they hold.
//!
//! Two rules bind every file here, and [`support`]'s own header states the
//! rest: no sleep, no timer, no wall clock; and pass-unit driving is the
//! evidence surface for everything the driver can reach (RFC 0014 §7.2).

pub mod delivery;
pub mod park;
pub mod quit;
pub mod support;
