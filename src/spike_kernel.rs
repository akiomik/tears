//! Minimal prototype of the candidate-B reducer-first runtime kernel
//! (`tmp/s10-gate-refarch-b.md`), built to verify the four unproven claims
//! named by the selection-gate verdict:
//!
//! 1. commit-ack handshake — `grant()` completes only after the granted
//!    send's reservation commit (the real `send().await == Ok`), so the
//!    C-2 sequential handshake is enforced by the API shape;
//! 2. reservation protocol (spec §2.1) — RAII `PendingReservation`,
//!    gate-before-reservation ordering, per-origin pending counters, the
//!    unified removal condition, and the saturation/poison overflow rule;
//! 3. revocation filtering — delivery-side dequeue filtering by the origin
//!    entry's revoked flag, with tombstone (Draining) retention tied to the
//!    committed pending count;
//! 4. `TestDriver` — same-topology deterministic driving of the real kernel
//!    (real `JoinSet` producers, real lanes, scripted Arbiter + `SendGate`).
//!
//! This is spike code: it is compiled only for tests, is not public API,
//! and deliberately implements only the slice of the design the eight
//! H-5 acceptance series and the four claims require. Composition is a
//! single parent-child scoping combinator; cleanup runs (`on_teardown`)
//! and the observability schema are out of scope.

pub mod cmd;
pub mod driver;
pub mod kernel;
pub mod lane;
pub mod registry;

#[cfg(test)]
mod series;
