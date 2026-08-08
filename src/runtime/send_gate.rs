//! The send-intent arbitration seam (RFC 0010 N46's harness-side
//! arbitration, applied to producer enqueue order).
//!
//! Every runtime-owned producer body — unkeyed command task, keyed command
//! task, subscription forwarder — passes through [`TaskSendGate::granted`]
//! immediately before delivering a message to its channel: the *send intent*
//! point. The production implementation grants immediately (`granted()` is
//! ready on its first poll — no suspension, no behavioral change); the
//! test-only scripted implementation can park a held producer at its intent
//! until the driving script grants it.
//!
//! This makes the enqueue order among simultaneously ready producers a
//! scripted decision by construction rather than an executor-scheduling
//! outcome: with several producers parked at their intents, only the granted
//! one can proceed, so no scheduling order — guaranteed or not — can reorder
//! the enqueues. The phase executors, bookkeeping, and delivery are
//! untouched; the production/test difference is confined to this seam.

#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
use scripted::ScriptedSends;

/// The runtime-wide gate root, cloned into every producer-spawning owner.
#[derive(Clone)]
pub enum SendGate {
    /// Production: every send intent is granted immediately.
    Immediate,
    /// Test-side scripted gating (driving surface only).
    #[cfg(test)]
    Scripted(Arc<ScriptedSends>),
}

impl SendGate {
    /// The production gate: all sends proceed immediately.
    pub const fn production() -> Self {
        Self::Immediate
    }

    /// Registers one producer task and returns its per-task gate handle.
    ///
    /// Registration happens at spawn, which is always synchronous on the
    /// driving task (command dispatch or subscription reconcile), so
    /// registration order — and with it each producer's index — is
    /// deterministic and known to a driving script.
    #[cfg_attr(
        not(test),
        expect(
            clippy::missing_const_for_fn,
            reason = "only the production variant exists without cfg(test); the scripted variant clones an Arc"
        )
    )]
    pub fn register_task(&self) -> TaskSendGate {
        match self {
            Self::Immediate => TaskSendGate::Immediate,
            #[cfg(test)]
            Self::Scripted(sends) => TaskSendGate::Scripted {
                index: sends.register(),
                sends: Arc::clone(sends),
            },
        }
    }
}

/// One producer task's handle onto the seam.
pub enum TaskSendGate {
    /// Production: ready on first poll.
    Immediate,
    /// Scripted: parks at the intent while this producer is held and has no
    /// unconsumed grant.
    #[cfg(test)]
    Scripted {
        index: usize,
        sends: Arc<ScriptedSends>,
    },
}

impl TaskSendGate {
    /// Resolves when this producer's current send intent is granted.
    #[cfg_attr(
        not(test),
        expect(
            clippy::unused_async,
            reason = "the production arm is synchronous; the seam's contract is an await point, which the scripted variant suspends on"
        )
    )]
    pub async fn granted(&self) {
        match self {
            Self::Immediate => {}
            #[cfg(test)]
            Self::Scripted { index, sends } => sends.granted(*index).await,
        }
    }
}

#[cfg(test)]
pub mod scripted {
    //! The scripted implementation and its test-held controller.

    use std::collections::HashMap;
    use std::future::poll_fn;
    use std::sync::{Arc, Mutex, MutexGuard};
    use std::task::{Poll, Waker};

    use super::SendGate;

    /// Creates a scripted gate and the controller that scripts it. Producers
    /// are ungated (immediate) until the controller holds them.
    pub fn scripted() -> (SendGate, SendGateController) {
        let sends = Arc::new(ScriptedSends::default());
        (
            SendGate::Scripted(Arc::clone(&sends)),
            SendGateController(sends),
        )
    }

    #[derive(Default)]
    struct ProducerState {
        /// Held producers park at their send intents until granted.
        held: bool,
        /// Unconsumed grants.
        allowance: usize,
        /// Grants consumed (each one is a completed passage of the seam;
        /// with an unbounded channel the send completes in the same poll).
        consumed: usize,
        /// Whether the producer is currently parked at a send intent.
        at_intent: bool,
        waker: Option<Waker>,
    }

    #[derive(Default)]
    struct State {
        registered: usize,
        producers: HashMap<usize, ProducerState>,
    }

    /// Shared state between the per-task gates and the controller.
    #[derive(Default)]
    pub struct ScriptedSends {
        state: Mutex<State>,
    }

    impl ScriptedSends {
        fn lock(&self) -> MutexGuard<'_, State> {
            self.state.lock().expect("send gate state lock")
        }

        pub(super) fn register(&self) -> usize {
            let mut state = self.lock();
            let index = state.registered;
            state.registered += 1;
            index
        }

        #[expect(
            clippy::significant_drop_tightening,
            reason = "the lock guard intentionally spans the whole poll decision"
        )]
        pub(super) async fn granted(&self, index: usize) {
            poll_fn(|cx| {
                let mut state = self.lock();
                let producer = state.producers.entry(index).or_default();
                if !producer.held {
                    return Poll::Ready(());
                }
                if producer.allowance > 0 {
                    producer.allowance -= 1;
                    producer.consumed += 1;
                    producer.at_intent = false;
                    producer.waker = None;
                    return Poll::Ready(());
                }
                producer.at_intent = true;
                producer.waker = Some(cx.waker().clone());
                Poll::Pending
            })
            .await;
        }
    }

    /// The driving script's end of the seam.
    pub struct SendGateController(Arc<ScriptedSends>);

    impl SendGateController {
        /// How many producer tasks have registered so far. Capturing this
        /// before a scripted spawn yields the index the next producer will
        /// receive.
        pub fn tasks(&self) -> usize {
            self.0.lock().registered
        }

        /// Holds a producer: its future send intents park until granted.
        pub fn hold(&self, index: usize) {
            self.0.lock().producers.entry(index).or_default().held = true;
        }

        /// Whether the producer is currently parked at a send intent.
        pub fn at_intent(&self, index: usize) -> bool {
            self.0
                .lock()
                .producers
                .get(&index)
                .is_some_and(|producer| producer.at_intent)
        }

        /// Grants one send to a held producer. Only granted producers can
        /// pass the seam, so grant order is enqueue order by construction.
        #[expect(
            clippy::significant_drop_tightening,
            reason = "the lock guard is scoped so the waker fires after it is released"
        )]
        pub fn grant(&self, index: usize) {
            let waker = {
                let mut state = self.0.lock();
                let producer = state.producers.entry(index).or_default();
                producer.allowance += 1;
                producer.waker.take()
            };
            if let Some(waker) = waker {
                waker.wake();
            }
        }

        /// How many grants the producer has consumed (completed seam
        /// passages).
        pub fn consumed(&self, index: usize) -> usize {
            self.0
                .lock()
                .producers
                .get(&index)
                .map_or(0, |producer| producer.consumed)
        }
    }
}
