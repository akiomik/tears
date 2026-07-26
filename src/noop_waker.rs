//! Shared poll-once-with-noop-waker support.
//!
//! Several call sites need to poll a future or stream exactly once, without an
//! executor, to check whether it is ready right now: a wake-up scheduled
//! during that single poll is deliberately not honored, since nothing is
//! listening for it.

use std::task::Context;

use futures::task::noop_waker_ref;

/// A [`Context`] whose waker discards every wake-up. Building one costs
/// nothing (the waker is a static no-op vtable), so callers construct a fresh
/// one per poll rather than holding it across calls.
pub fn noop_context() -> Context<'static> {
    Context::from_waker(noop_waker_ref())
}
