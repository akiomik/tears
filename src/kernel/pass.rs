//! The processing pass: four fixed stages in fixed order, plus the park and
//! wake arming that keeps their bounds from holding vacuously.
//!
//! RFC 0014 §3.5 pins the order (normative, not mechanism):
//!
//! 1. **Exit reflection** — producer exits the executor has completed are
//!    reflected into the run bookkeeping: exit-observed lifecycle facts,
//!    entry retirement where the delivery accounting permits it, and
//!    subscription dirt per §5.2's sources. This stage processes no input
//!    and applies no quit. Its position decides which quiescence facts the
//!    same pass's control drain, admissions, and frame stage observe.
//! 2. **Control-lane drain** — every quit that has arrived is drained
//!    *before* this pass's input batch, and applied if its origin is live,
//!    discarded if revoked.
//! 3. **At most one input batch**, always count-bounded.
//! 4. **Frame step** — render if a redraw is pending, then re-evaluation if
//!    subscriptions are dirty.
//!
//! The stages are not independently arbitrated branches: no sequence of
//! ready inputs can defer any of them. Two bounds follow and are contract —
//! a producer quit is applied before any input batch that begins after its
//! arrival (INV-RC9), and a redraw a batch marks is rendered before the next
//! input batch begins (INV-RC10).
//!
//! [`Kernel::pass_cycle`] is a single implementation shared verbatim by the
//! production loop and by the driver's pass-unit stepping, which is what
//! makes the driving differential the two seams and nothing else.
//!
//! **No wall clock.** Nothing in this module reads a clock, arms a timer, or
//! sleeps: the batch is count-bounded, the frame is pass-bounded, and the
//! park waits on arrivals rather than on elapsed time (RFC 0014 §6.3).

use std::future::{pending, poll_fn};
use std::num::NonZeroUsize;

use ratatui::Terminal;
use ratatui::backend::Backend;
use tokio::task::{Id as TaskId, JoinError};

use super::arbiter::WakeSource;
use super::lane::{Envelope, Payload, RunToken};
use super::producer;
use super::registry::{Phase, RunKind};
use super::{ExitReason, Kernel, KernelPhase};
use crate::reducer::Program;
use crate::subscription::SubscriptionId;

/// The input-batch count cap used when `batch_max_messages` is unset.
///
/// What is contract is only that the cap is finite, so every batch ends
/// after a finite prefix of the ready input and the pass reaches its frame
/// stage (RFC 0014 §3.5's finite-prefix eventual progress). The wall-clock
/// batching window RFC 0006 INV-L6 defaulted to is superseded: the driving
/// loop reads no clock, and a time-windowed batch would make the pass bounds
/// wall-clock-relative. The value itself is mechanism.
pub const DEFAULT_BATCH_MAX_MESSAGES: NonZeroUsize = NonZeroUsize::new(1024).expect("non-zero");

/// What a park's single selection site received.
///
/// Private to the park: the woken value is *buffered* rather than consumed,
/// so this type exists only between the `select!` and the buffer it lands
/// in.
enum Woken<Msg> {
    /// A data-lane envelope, or the lane's closure.
    Data(Option<Envelope<Msg>>),
    /// A control-lane envelope, or the lane's closure.
    Control(Option<Envelope<Msg>>),
    /// One join-set exit, or an empty join set.
    Exit(Option<Result<(TaskId, ()), JoinError>>),
}

impl<P: Program> Kernel<P> {
    /// One pass in the normative stage order.
    ///
    /// A render failure terminates the kernel and returns the backend's
    /// error, which the caller classifies (RFC 0011 INV-LC5's `Err`).
    ///
    /// Each stage is guarded by the termination check rather than by a
    /// return from the previous one, so a quit applied *inside* a stage —
    /// an `update`-returned quit in the batch, a producer quit in the
    /// drain — stops the pass at the same boundary as one applied between
    /// stages.
    pub fn pass_cycle<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> Result<(), B::Error> {
        self.reflect_available_exits();
        if !self.terminating() {
            self.control_drain();
        }
        if !self.terminating() {
            self.input_batch();
        }
        if !self.terminating() {
            self.frame_step(terminal)?;
        }
        Ok(())
    }

    /// Stage 1: reflects every task exit currently observable.
    ///
    /// Every exit the executor has completed is reflected, not one: the
    /// stage's position is what decides which quiescence facts the rest of
    /// the pass observes, and reflecting only a prefix of them would leave
    /// that answer dependent on how many exits happened to land.
    fn reflect_available_exits(&mut self) {
        while self.poll_exit() {
            self.reflect_one_exit();
        }
    }

    /// Reflects one buffered exit into the run bookkeeping, marking
    /// subscription dirt per RFC 0014 §5.2.
    fn reflect_one_exit(&mut self) {
        let Some((token, _outcome)) = self.exit_buf.pop_front() else {
            return;
        };
        let Some(observed) = self.registry.on_exit(token) else {
            return;
        };
        // Dirt boundary (RFC 0014 §5.2): only the quiescence of a
        // *subscription* run that a steady-state stop revoked — a
        // re-evaluation's removal or replacement, or a scope teardown —
        // marks subscriptions dirty. A natural finish leaves no dirt (a
        // finished, still-declared subscription restarts at the next
        // re-evaluation), command and cleanup runs are not re-evaluation
        // subjects, and termination-driven quiescence is excluded by the
        // phase check: a termination-stopped run can only ever be observed
        // while the kernel is terminating, which is exactly why the stop
        // *cause* need not be stored on the entry.
        if observed.stopped && matches!(observed.kind, RunKind::Sub(_)) && !self.terminating() {
            self.dirty = true;
        }
    }

    /// Stage 2: drains the control lane, applying each quit whose origin is
    /// still live and discarding the rest (RFC 0014 §3.3).
    ///
    /// Drained to exhaustion, before this pass's input batch: a quit that
    /// has arrived when the pass begins is applied with **zero** further
    /// inputs processed (INV-RC9), and an input whose `update` would have
    /// cancelled the quit's origin never precedes it.
    fn control_drain(&mut self) {
        while !self.terminating() {
            let Some(envelope) = self.next_control() else {
                break;
            };
            // The dequeue is the delivery decision *and* the accounting's
            // rule-4 decrement, for a discarded envelope exactly as for an
            // applied one.
            let revoked = self.registry.on_dequeue(envelope.origin);
            if revoked {
                continue;
            }
            if matches!(envelope.payload, Payload::Quit) {
                self.apply_quit(ExitReason::Quit);
            }
        }
    }

    /// Stage 3: one count-bounded input batch. Every dequeue passes the
    /// delivery decision first, so a revoked origin's envelope is discarded
    /// without reaching `reduce` (RFC 0014 INV-RC5).
    ///
    /// The batch marks subscriptions dirty when it ran `update` at all —
    /// one of RFC 0014 §5.2's two dirt sources. It does *not* mark the
    /// redraw: that is the commands' own directive, OR-folded across the
    /// batch by [`Kernel::dispatch`] (RFC 0002's separation, which a
    /// `without_redraw` command relies on).
    fn input_batch(&mut self) {
        let mut applied = 0_usize;
        for _ in 0..self.batch_cap.get() {
            if self.terminating() {
                break;
            }
            let Some(envelope) = self.next_data() else {
                break;
            };
            let revoked = self.registry.on_dequeue(envelope.origin);
            if revoked {
                continue;
            }
            let Payload::Msg(message) = envelope.payload else {
                continue;
            };
            let state = self.state.as_mut().expect("kernel booted");
            let command = self.program.reduce(state, message);
            applied += 1;
            self.dispatch(command);
        }
        if applied > 0 {
            self.dirty = true;
        }
    }

    /// Stage 4: at most one render, then at most one re-evaluation, both on
    /// the pass's current state (RFC 0011 INV-LC1, INV-LC2).
    ///
    /// The render precedes the re-evaluation and the pass never begins a
    /// re-evaluation while a redraw is still pending, so the subscriptions
    /// this pass starts are those of a state this pass has just rendered.
    fn frame_step<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> Result<(), B::Error> {
        if self.redraw_pending {
            self.redraw_pending = false;
            let program = &self.program;
            let state = self.state.as_ref().expect("kernel booted");
            let rendered = terminal.draw(|frame| program.view(state, frame)).map(drop);
            if let Err(error) = rendered {
                // The failure's *value* leaves through the caller's
                // `Result`; the kernel keeps only the reason, so it stays
                // independent of the backend type (RFC 0011 INV-LC5).
                self.apply_quit(ExitReason::RenderError);
                return Err(error);
            }
        }
        if !self.terminating() && self.dirty {
            self.dirty = false;
            self.reconcile();
        }
        Ok(())
    }

    /// One subscription re-evaluation: the stop phase, then admissions under
    /// the uniform barrier (subscription runs only, RFC 0014 §5.1) and the
    /// stopping-pass defer rule — a pass that issued any stop admits nothing
    /// in that same pass (RFC 0014 §5.3).
    ///
    /// The defer is a single early return rather than a filtered admission
    /// loop, which is the structural half of INV-RC12 (c): a quiescence that
    /// happens while this call is still running has no admission site left
    /// to reach, because the path takes no second attempt after issuing its
    /// stops.
    ///
    /// Each admission invokes the declaration's spawner exactly once, here,
    /// on the driving task (RFC 0012 INV-SE1) — so a lazy source
    /// constructor that panics unwinds where an application panic is
    /// fail-fast rather than inside a runtime-owned task, where it would be
    /// contained (RFC 0011 INV-LC8).
    pub(super) fn reconcile(&mut self) {
        let declarations = {
            let state = self.state.as_ref().expect("kernel booted");
            self.program.subscriptions(state)
        };
        let declared: Vec<SubscriptionId> = declarations
            .iter()
            .map(|declaration| declaration.id().clone())
            .collect();

        // The stop phase: every running subscription run the current state
        // no longer declares. Read whole before any stop is issued, so the
        // selection cannot depend on the transitions it triggers.
        let stops: Vec<RunToken> = self
            .registry
            .iter()
            .filter(|entry| entry.phase == Phase::Running)
            .filter(|entry| matches!(&entry.kind, RunKind::Sub(id) if !declared.contains(id)))
            .map(|entry| entry.token)
            .collect();
        let issued = !stops.is_empty();
        for token in stops {
            self.registry.stop_request(token);
        }

        // The uniform barrier and the stopping-pass defer, in one condition:
        // a stop-requested, not-yet-quiesced subscription run anywhere defers
        // every admission runtime-wide, and this pass's own stops defer its
        // own admissions even where the entry is already a tombstone.
        if issued || self.registry.any_stopping_sub() {
            return;
        }

        for declaration in declarations {
            let id = declaration.id().clone();
            if self.registry.sub_running(&id) {
                continue;
            }
            // The run's scope attribution and its identity's scope are one
            // path: `Subscription::scoped` qualifies the identity, and the
            // run is attributed to the same boundary so a prefix teardown
            // selects it beside the command runs declared there
            // (RFC 0014 §4.1).
            let scope = id.scope().clone();
            let stream = declaration.into_stream();
            self.spawn_producer(RunKind::Sub(id), scope, producer::subscription_body(stream));
        }
    }

    /// Whether the named wake source has work for a pass.
    ///
    /// Readiness is read from the real lanes and the real join set, so the
    /// driver cannot fabricate it — a `ProducerExit` source in particular
    /// requires an actual task exit to be observable. Nothing is ready
    /// outside steady state: a kernel that has not booted has no pass to
    /// begin, and a terminating one has only its settle left.
    ///
    /// Each lane's readiness is "the park's buffer is non-empty **or** the
    /// lane has queued items", which is what keeps the one envelope a park
    /// received from being skipped by the pass it began.
    pub fn wake_source_ready(&mut self, source: WakeSource) -> bool {
        if self.phase != KernelPhase::Steady {
            return false;
        }
        match source {
            WakeSource::Data => {
                !self.data_buf.is_empty() || self.data_rx.as_ref().is_some_and(|rx| rx.len() > 0)
            }
            WakeSource::Control => {
                !self.control_buf.is_empty()
                    || self.control_rx.as_ref().is_some_and(|rx| !rx.is_empty())
            }
            WakeSource::ProducerExit => self.poll_exit(),
        }
    }

    /// Whether any wake source or a pending frame has work — the production
    /// loop's park condition, negated.
    ///
    /// Frame work joins the three wake sources here and only here: a pending
    /// redraw or subscription dirt is work to make progress on (so the
    /// kernel must not park on it), but nothing arrives to wake a kernel
    /// that parked on it, which is why it is no member of
    /// [`WakeSource`](super::arbiter::WakeSource).
    pub fn pass_work_ready(&mut self) -> bool {
        if self.phase != KernelPhase::Steady {
            return false;
        }
        self.redraw_pending
            || self.dirty
            || WakeSource::ALL
                .into_iter()
                .any(|source| self.wake_source_ready(source))
    }

    /// Parks until one of the three armed wake sources arrives (INV-RC16).
    ///
    /// This `select!` is the **single pass-initiation selection site**, and
    /// its choice among the ready branches is unbiased: `tokio::select!`
    /// polls its branches in a randomized order, and the kernel keeps no
    /// per-source priority, quota, or ordering state beside it (RFC 0014
    /// §3.5's structural enforcement).
    ///
    /// All three futures are constructed and polled within one `select!`,
    /// so a park registers the current waker with every armed source. The
    /// join-set branch is disabled while the set is empty — an empty set
    /// cannot produce an exit, and only the driving task spawns, so no task
    /// can appear while this future is parked.
    ///
    /// The woken item is *buffered*, not consumed: readiness is then
    /// "buffer non-empty or the lane has queued items", so the one envelope
    /// the park received is processed by the pass it began rather than
    /// skipped by it.
    ///
    /// A `None` from either lane is unreachable by construction — the
    /// kernel holds a clone of both senders for its whole lifetime — so
    /// that branch asserts in debug and otherwise parks forever rather than
    /// spinning. Parking is what RFC 0014 §9 row 1 puts in place of RFC 0003
    /// INV-16's `Ready(None)` half ("a live kernel with no work parks"), and
    /// it keeps a future change in lane ownership from turning a degenerate
    /// state into a busy loop.
    pub async fn park(&mut self) -> WakeSource {
        let woken = {
            let data = self
                .data_rx
                .as_mut()
                .expect("a live kernel holds its data lane");
            let control = self
                .control_rx
                .as_mut()
                .expect("a live kernel holds its control lane");
            let join_set = &mut self.join_set;
            let exits_armed = !join_set.is_empty();
            tokio::select! {
                envelope = poll_fn(|cx| data.poll_recv(cx)) => Woken::Data(envelope),
                envelope = control.recv() => Woken::Control(envelope),
                exit = join_set.join_next_with_id(), if exits_armed => Woken::Exit(exit),
            }
        };
        match woken {
            Woken::Data(Some(envelope)) => {
                self.data_buf.push_back(envelope);
                WakeSource::Data
            }
            Woken::Control(Some(envelope)) => {
                self.control_buf.push_back(envelope);
                WakeSource::Control
            }
            Woken::Exit(Some(exit)) => {
                self.buffer_exit(exit);
                WakeSource::ProducerExit
            }
            Woken::Data(None) | Woken::Control(None) | Woken::Exit(None) => {
                debug_assert!(
                    false,
                    "a live kernel holds both lane senders and arms the join set only when it \
                     is non-empty, so no armed source can complete with nothing"
                );
                pending().await
            }
        }
    }
}
