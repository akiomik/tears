//! Producer task bodies and the policy every runtime-owned run shares.
//!
//! One harness for every kind — keyed command, anonymous command,
//! subscription forwarder — so task ownership, panic containment, gauge
//! accounting, and the send-stop policy have a single implementation and no
//! per-kind variant for a contract to slip through (RFC 0014 INV-RC1's
//! "effect-task ownership and bookkeeping" and "task body policy" rows).
//!
//! Policy, stated once:
//!
//! - Each run's whole body is wrapped so a panic is contained as a join
//!   error rather than reaching the process hook, keeping the terminal of a
//!   still-running application alone (RFC 0011 INV-LC8).
//! - A failed send ends the run. Failure means the lane's receiver is gone,
//!   which happens at termination's immediate postcondition, so continuing
//!   would only produce sends nothing can receive.
//! - `Action::Message` goes to the data lane and `Action::Quit` to the
//!   control lane (RFC 0014 §3.1, §3.3). That translation is the whole of
//!   what a command producer does with its stream.
//! - The gauge guard a run holds is dropped on every exit path, abort and
//!   panic included (RFC 0006 §4.4).

use futures::future::BoxFuture;
use futures::stream::BoxStream;

use crate::command::Action;
use crate::subscription::Subscription;

use super::lane::IngressHandle;

/// What a producer body receives.
pub struct EffectCtx<Msg> {
    /// This run's ingress surface — its only way to reach either lane.
    pub handle: IngressHandle<Msg>,
}

/// A producer body: given its context, the future the runtime-owned task
/// runs.
pub type EffectBody<Msg> = Box<dyn FnOnce(EffectCtx<Msg>) -> BoxFuture<'static, ()> + Send>;

/// The command producer body: relays one lowered effect stream, translating
/// each action onto its lane and ending the run on the first failed send.
pub fn command_body<Msg: Send + 'static>(
    _stream: BoxStream<'static, Action<Msg>>,
) -> EffectBody<Msg> {
    todo!("command producer body")
}

/// The subscription forwarder body.
///
/// The source's spawner is invoked exactly once, at admission, so a
/// subscription's stream is created once per run (RFC 0012 INV-SE1). The
/// same body is what a component-level test drives directly, so the
/// send-stop policy under test is the one production runs.
pub fn subscription_body<Msg: Send + 'static>(_subscription: Subscription<Msg>) -> EffectBody<Msg> {
    todo!("subscription forwarder body")
}
