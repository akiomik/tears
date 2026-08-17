//! The kernel conformance suite: RFC 0014 §13.1's twelve series.
//!
//! The series live here rather than in `tests/` because they drive the
//! kernel directly through the stage-3 driver, which is crate-private until
//! the switch. The shared fixtures — the scripted program, its journal, the
//! probe source, the gated effects, and the bounded waits — live in
//! [`support`], and the series files are named for the property they hold.
//!
//! # What produces evidence here
//!
//! **Pass-unit driving is the evidence surface for everything the driver can
//! reach** (RFC 0014 §7.2, RFC 0008 §9.9). Every steady-state claim below is
//! composed of `boot`, `step_pass`, `grant` → `confirm`, and `settle`: one
//! step is one whole pass through RFC 0014 §3.5's fixed stage order, and
//! `boot` is the whole production bootstrap in one call. No series runs a
//! stage in isolation — a stage-granular probe can fabricate a permuted
//! execution the fixed order forbids, so nothing observed through one would
//! be evidence for INV-RC13 — and this suite has none, so the `whitebox_`
//! prefix that would mark one appears nowhere in it.
//!
//! **The three `ParkProbe` series carry INV-RC16 and nothing else.** They
//! poll the production driving future by hand, with the probe's own waker,
//! and their observations are never evidence for the same-topology claim,
//! for scripted determinism, for the pass stage order, or for production
//! pass initiation (RFC 0008 §9.7).
//!
//! **The citation rule** (RFC 0008 §9.9): an order the driver establishes is
//! never evidence of a production order. That reaches the scripted sequence
//! of wake sources, the gate order the acceptance ledger records, and a
//! report's `started` order alike. Which source production picks among
//! several arrived at once stays unobserved here.
//!
//! **Where a claim is read.** Non-delivery is read from the application's
//! own record of what `update` was invoked with, never from a ledger: a
//! revoked run's committed send belongs in `accepted`, which records
//! admission and not delivery (RFC 0008 §9.6). Rendering is read from the
//! `view` calls the program under test counts, the driver owning its
//! terminal and reporting nothing about frames (RFC 0008 §9.11).
//!
//! **Waiting.** No series sleeps, arms a timer, or reads a wall clock. Every
//! wait is a bounded number of executor turns and fails the test on its
//! bound, and both budgets that a script owns — `settle`'s and `confirm`'s —
//! are named at the call site (RFC 0008 §9.6, §9.8).
//!
//! # Series and the invariant rows they carry
//!
//! The nine pass-unit series and the three probe series of RFC 0014 §13.1,
//! with the module each lives in and the §12 rows it witnesses.
//!
//! | Series (RFC 0014 §13.1) | Module | Rows |
//! | --- | --- | --- |
//! | `cancel vs buffered output` | [`delivery`] | INV-RC5 (message half, natural-finish control, late-task-exit adversary) |
//! | `simultaneous readiness` (both script faces) | [`delivery`] | INV-RC14 (producer face: the handshake is the only order; initiation face: one script, one sequence) |
//! | `both quit semantics` | [`quit`] | INV-RC9 (synchronous, pass-start, flooded lane), INV-RC11 (the init-quit row) |
//! | `both panic classes` | [`lifecycle`] | INV-RC11 via RFC 0011 INV-LC8 (contained producer panic, fail-fast application panic) |
//! | `shutdown-scoped send failure` | [`lifecycle`] | INV-RC11 via RFC 0011 §4.4's two stages (full topology), RFC 0014 §6.1's send-stop policy (component level) |
//! | `termination under owned work` | [`lifecycle`] | INV-RC11 over all four causes: `update` quit, producer quit, render failure, host-side drop |
//! | `stop/restart safe window` | [`lifecycle`] | INV-ST7's observable half, INV-RC12 (a) |
//! | `idle wake` | [`park`] | RFC 0014 §5.2's dirt sources, INV-RC12 (b) — the woken pass's exit reflection, not a row of INV-RC16 |
//! | `bounded-lane revocation` | [`bounded`] | INV-RC5's bounded-lane half, under RFC 0014 §13.3's two protocol conditions |
//! | `parked data-lane wake` | [`park`] | INV-RC16 |
//! | `parked control-quit wake` | [`park`] | INV-RC16 |
//! | `parked subscription-quiescence wake` | [`park`] | INV-RC16 |
//!
//! Two neighbours sit beside the twelve, in the same files and under the
//! same rules: [`delivery`] carries INV-RC10's flood rows (FIFO prefix,
//! backlog-proportional passes, a render between batches), and [`teardown`]
//! carries RFC 0013 §7.1's kernel rows — INV-ST1, INV-ST3, INV-ST5, INV-ST6,
//! and INV-ST7's observable half, with the kernel carriers INV-RC6 and
//! INV-RC7.
//!
//! # What this surface cannot construct
//!
//! Three §12 rows need a control-lane arrival that lands **strictly inside**
//! a pass, and no script here can produce one: RFC 0014 §3.5's four stages
//! run to completion without the driving task yielding, and the executor the
//! determinism claim is scoped to has one thread (RFC 0008 §9.8), so no
//! producer can commit while a pass is running. The rows are INV-RC9's
//! *mid-batch* case and its *cancel-beats-quit* case, and INV-RC5's
//! *buffered-quit* half. The window is empty here rather than unexercised —
//! it is reachable in production on a multi-worker executor, outside
//! INV-RC14's verified range. [`quit`] and [`delivery`] state the same limit
//! at the rows themselves and drive the constructible neighbours instead.

pub mod bounded;
pub mod delivery;
pub mod lifecycle;
pub mod park;
pub mod quit;
pub mod support;
pub mod teardown;
