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
//! - Each run's whole body is wrapped so a panic leaves the application
//!   running, is logged once, and surfaces as a join error. The process
//!   hook still runs and still delegates to whatever reporter it wrapped;
//!   what the wrapping changes is that it skips restoring the terminal of
//!   an application that is still drawing on it (RFC 0011 INV-LC8).
//! - A failed send ends the run. Failure means the lane's receiver is gone,
//!   which happens at termination's immediate postcondition, so continuing
//!   would only produce sends nothing can receive.
//! - `Action::Message` goes to the data lane and `Action::Quit` to the
//!   control lane (RFC 0014 §3.1, §3.3). That translation is the whole of
//!   what a command producer does with its stream.
//! - The gauge guard a run holds is dropped on every exit path, abort and
//!   panic included (RFC 0006 §4.4).

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, resume_unwind};
use std::sync::Arc;

use futures::future::BoxFuture;
use futures::stream::BoxStream;
use futures::{FutureExt, StreamExt};
use tokio::task::{Id as TaskId, JoinSet};

use crate::command::Action;
use crate::panic::contained_producer;
use crate::runtime::load::{GaugeGuard, LoadObserver};
use crate::structural_key::ScopePath;
use crate::testing::driver::{DeliveryLedger, IntentLedger};

use super::accounting::PendingCounter;
use super::lane::{ControlSender, DataSender, IngressHandle, RunToken, SendGate};
use super::registry::{Phase, RunEntry, RunKind};

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
///
/// A quit does not end the run; only a failed send does. What a quit does is
/// the kernel's: the control drain at the head of the next pass applies it,
/// and the immediate postcondition that follows aborts this run along with
/// every other. Ending the stream here would be this body deciding a
/// termination order it does not own, and RFC 0014 §3.3 pins no ordering
/// between a run's quit and its own data-lane output for it to preserve. The
/// superseded body did stop after a quit, so a stream that emits several can
/// now put several on the control lane — which is unbounded by contract
/// (RFC 0006 R4) and drained every pass, so the difference is transient
/// occupancy and not a delay.
pub fn command_body<Msg: Send + 'static>(
    stream: BoxStream<'static, Action<Msg>>,
) -> EffectBody<Msg> {
    Box::new(move |ctx| {
        Box::pin(async move {
            let mut stream = stream;
            while let Some(action) = stream.next().await {
                let sent = match action {
                    Action::Message(msg) => ctx.handle.send(msg).await,
                    Action::Quit => ctx.handle.quit().await,
                };
                if sent.is_err() {
                    break;
                }
            }
        })
    })
}

/// The subscription forwarder body.
///
/// It takes the source's stream rather than the `Subscription`, because the
/// spawner is invoked exactly once at *admission*, on the driving task
/// (RFC 0012 INV-SE1): a source constructor that panics has to unwind there
/// — where an application panic is fail-fast — rather than inside a
/// runtime-owned task, where it would be contained (RFC 0011 §4.3 lists the
/// lazy source constructor among the driving-task sites, and INV-LC8 keeps
/// its row distinct from the stream-panic row for exactly this reason).
///
/// The same body is what a component-level test drives directly, so the
/// send-stop policy under test is the one production runs.
///
/// **Prerequisite for the admission site.** `Subscription`'s spawner is
/// private to its own module, so nothing outside `crate::subscription` can
/// turn a `Subscription<Msg>` into the stream this takes. Admission needs
/// one crate-visible accessor there — `Subscription::id` is already
/// `pub(crate)`, its spawner is not — and that accessor is what fixes where
/// the spawner runs.
pub fn subscription_body<Msg: Send + 'static>(stream: BoxStream<'static, Msg>) -> EffectBody<Msg> {
    Box::new(move |ctx| {
        Box::pin(async move {
            let mut stream = stream;
            while let Some(msg) = stream.next().await {
                if ctx.handle.send(msg).await.is_err() {
                    break;
                }
            }
        })
    })
}

/// The gauge a run of this kind contributes to for its task's lifetime, if
/// any.
///
/// Keyed runs contribute none here, and that is not an omission: the
/// `keyed_commands` gauge is count-based and published by the registry's
/// membership mutation point, because a keyed entry's lifetime is the
/// runtime's rather than its task's — a draining entry outlives its task, so
/// no task-scoped guard could carry it.
fn gauge_guard(observer: &LoadObserver, kind: &RunKind) -> Option<GaugeGuard> {
    match kind {
        RunKind::Keyed(_) => None,
        RunKind::Anon => Some(observer.track_unkeyed_command()),
        RunKind::Sub(_) => Some(observer.track_subscription()),
    }
}

/// The spawn-side policy every runtime-owned producer run shares.
///
/// It borrows the kernel's spawn state rather than owning it: the join set,
/// the task index, and the two lane senders are the kernel's for its whole
/// lifetime, and this type is the one place that reads them together to
/// start a run. Every producer kind goes through
/// [`start`](ProducerHarness::start) and lands in the **one** join set, which
/// is what makes exit observation, abort, and the settle drain total over
/// kinds with no per-kind ownership path beside them.
pub struct ProducerHarness<'a, Msg: Send + 'static> {
    /// The single join set every producer kind is spawned into.
    pub join_set: &'a mut JoinSet<()>,
    /// Task-id to run-token index, so an exit observation names its run.
    pub task_index: &'a mut HashMap<TaskId, RunToken>,
    /// The data lane's sending half, cloned per run.
    pub data: &'a DataSender<Msg>,
    /// The control lane's sending half, cloned per run.
    pub control: &'a ControlSender<Msg>,
    /// The pre-gate observation ledger handed to the run's ingress.
    pub intents: &'a IntentLedger,
    /// The post-gate observation ledger handed to the run's ingress.
    pub delivery: &'a DeliveryLedger,
    /// The load observer this run's gauge guard reports to.
    pub observer: &'a LoadObserver,
    /// The gate the run's ingress waits on before each send.
    ///
    /// The kernel's one gate, cloned per run rather than built per run: the
    /// driver's "at most one outstanding grant" rule is driver-wide, which
    /// only a shared object can express (RFC 0008 §9.6).
    pub gate: &'a Arc<SendGate>,
}

impl<Msg: Send + 'static> ProducerHarness<'_, Msg> {
    /// Starts one run under `token` and returns its registry entry.
    ///
    /// The entry is returned rather than inserted so that the two
    /// authorities stay one each: this harness owns task startup, the
    /// registry owns bookkeeping, and the caller is the only place they
    /// meet.
    ///
    /// The body's `FnOnce` runs here, on the driving task, and only the
    /// future it returns is spawned — the same split the subscription
    /// forwarder relies on to keep a source constructor's panic on the
    /// driving task.
    pub fn start(
        &mut self,
        token: RunToken,
        kind: RunKind,
        scope: ScopePath,
        body: EffectBody<Msg>,
    ) -> RunEntry {
        let counter = Arc::new(PendingCounter::default());
        let gate = Arc::clone(self.gate);
        let handle = IngressHandle::new(
            token,
            Arc::clone(&counter),
            Arc::clone(&gate),
            self.data.clone(),
            self.control.clone(),
            self.intents.clone(),
            self.delivery.clone(),
        );
        let gauge = gauge_guard(self.observer, &kind);
        let run = body(EffectCtx { handle });

        let abort = self.join_set.spawn(async move {
            // Held for the whole body and dropped on every exit path —
            // completion, abort, and panic unwind alike (RFC 0006 §4.4).
            let _gauge = gauge;
            // `contained_producer` marks the poll so the panic hook skips
            // restoring the terminal of an application that keeps running
            // (RFC 0011 INV-LC8). The catch is what discharges the one
            // diagnostic obligation the successor still carries — RFC 0003
            // §7.3's keyed-panic log event, which RFC 0011 §5.1 carves out
            // of its diagnostic negative space — for every producer kind
            // rather than per kind.
            let outcome = AssertUnwindSafe(contained_producer(run))
                .catch_unwind()
                .await;
            if let Err(payload) = outcome {
                tracing::error!(
                    target: "tears::kernel",
                    panic = ?payload,
                    "producer task panicked"
                );
                // Re-raised so the exit still reaches the join set as a
                // panic rather than as an ordinary completion, which is what
                // the pass's exit reflection reads. `resume_unwind` does not
                // run the panic hook again, so the report above stays the
                // only one.
                resume_unwind(payload);
            }
        });
        self.task_index.insert(abort.id(), token);

        RunEntry {
            token,
            kind,
            scope,
            phase: Phase::Running,
            revoked: false,
            exited: false,
            counter,
            gate,
            abort,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::command::CommandId;
    use crate::subscription::Subscription;
    use crate::subscription::mock::MockSource;
    use crate::test_support::TraceRecorder;

    fn subscription_kind() -> RunKind {
        let subscription: Subscription<u8> = Subscription::new(MockSource::<u8>::new());
        RunKind::Sub(subscription.id().clone())
    }

    /// The four producer gauges as `tears::runtime::load` reports them over
    /// one run's guard: the values while it is held, then after it drops.
    fn gauge_trace(kind: &RunKind) -> Vec<(u64, u64, u64)> {
        let recorder = TraceRecorder::new().with_target("tears::runtime::load");
        let _guard = recorder.set_default();
        let observer = LoadObserver::new();

        drop(gauge_guard(&observer, kind));

        let subscriptions = recorder.u64_values("subscriptions");
        let unkeyed = recorder.u64_values("unkeyed_commands");
        let keyed = recorder.u64_values("keyed_commands");

        subscriptions
            .into_iter()
            .zip(unkeyed)
            .zip(keyed)
            .map(|((subscriptions, unkeyed), keyed)| (subscriptions, unkeyed, keyed))
            .collect()
    }

    #[test]
    fn an_anonymous_run_raises_and_lowers_the_unkeyed_command_gauge() {
        assert_eq!(gauge_trace(&RunKind::Anon), vec![(0, 1, 0), (0, 0, 0)]);
    }

    #[test]
    fn a_subscription_run_raises_and_lowers_the_subscription_gauge() {
        assert_eq!(
            gauge_trace(&subscription_kind()),
            vec![(1, 0, 0), (0, 0, 0)]
        );
    }

    #[test]
    fn a_keyed_run_moves_no_task_scoped_gauge() {
        assert!(
            gauge_trace(&RunKind::Keyed(CommandId::new("load"))).is_empty(),
            "the keyed gauge is count-based and published by the registry, so \
             starting a keyed run emits nothing here"
        );
    }
}
