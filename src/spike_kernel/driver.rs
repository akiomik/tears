//! Stage-3 driving surface: `TestDriver` drives the very same `Kernel`
//! (same construction path, real `JoinSet` producers, real lanes, same
//! branch executors, same termination) through the two scripted seams —
//! the Arbiter (`step` selects among *ready* branches only) and the
//! `SendGate` (`grant` completes only after observing the granted send's
//! reservation commit, i.e. the enqueue acceptance).
//!
//! Prototype deviations from the spec's API sketch, recorded for the
//! report: `boot`/`step` are synchronous (their branch executors have no
//! await points once readiness is established on the current-thread
//! executor), and `grant_handle` is an addition used by the bounded
//! commit-ack evaluation (the spec's `grant(&mut self)` shape cannot be
//! polled concurrently with `step`, which matters exactly when a bounded
//! send has to wait for capacity).

use std::sync::Arc;

use futures::future::BoxFuture;
use tokio::task::yield_now;

use super::cmd::Program;
use super::kernel::{Branch, ExitReport, Gauges, HeadlessHost, Kernel, KernelConfig};
use super::lane::{GateMode, GrantOutstanding, Ledger, OriginGate, PendingCounter, RunToken};
use super::registry::Phase;

/// Opaque public form of a run token.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ProducerId(RunToken);

/// Producers spawned during boot.
#[derive(Debug)]
pub struct BootReport {
    /// `(label, id)` in spawn order.
    pub producers: Vec<(&'static str, ProducerId)>,
}

/// Why a scripted step was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StepError {
    /// The named branch is not ready (readiness cannot be fabricated).
    NotReady(Branch),
    /// The kernel has terminated; only `settle` remains.
    Terminated,
}

/// Bounded, clock-free settle helper: yields until `condition` holds. The
/// iteration bound converts a would-be hang into a failed assertion.
pub async fn settle_until(mut condition: impl FnMut() -> bool, what: &str) {
    for _ in 0..10_000 {
        if condition() {
            return;
        }
        yield_now().await;
    }
    assert!(condition(), "bounded settle exhausted: {what}");
}

/// The same-topology scripted driver.
pub struct TestDriver<P: Program> {
    kernel: Kernel<P, HeadlessHost>,
}

impl<P: Program> TestDriver<P> {
    /// Inert construction through the production construction path, with
    /// the scripted gate installed.
    pub fn new(program: P, flags: P::Flags, config: KernelConfig) -> Self {
        Self::with_host(program, flags, config, HeadlessHost::default())
    }

    /// Same, with a scripted host (render-failure series).
    pub fn with_host(
        program: P,
        flags: P::Flags,
        config: KernelConfig,
        host: HeadlessHost,
    ) -> Self {
        Self {
            kernel: Kernel::new(program, flags, config, GateMode::Scripted, host),
        }
    }

    /// Drives the real bootstrap.
    pub fn boot(&mut self) -> BootReport {
        BootReport {
            producers: self
                .kernel
                .boot()
                .into_iter()
                .map(|(token, label)| (label, ProducerId(token)))
                .collect(),
        }
    }

    /// The live producer labelled `label`.
    pub fn producer(&self, label: &str) -> ProducerId {
        ProducerId(
            self.kernel
                .registry()
                .live_by_label(label)
                .expect("live producer with label")
                .token,
        )
    }

    fn gate(&self, origin: ProducerId) -> Arc<OriginGate> {
        self.kernel
            .registry()
            .get(origin.0)
            .map(|e| Arc::clone(&e.gate))
            .expect("granted origin must have a registry entry")
    }

    /// Sequential grant handshake: issues the origin's next grant and
    /// returns only after observing that grant's own commit (the enqueue
    /// acceptance) — C-2's grant -> acceptance -> next-grant.
    #[expect(
        clippy::needless_pass_by_ref_mut,
        reason = "spec API shape: grant borrows the driver exclusively, so \
                  raw overlapping grants are unconstructible"
    )]
    pub async fn grant(&mut self, origin: ProducerId) {
        let gate = self.gate(origin);
        let sequence = gate
            .issue_grant()
            .expect("the previous grant must be acknowledged before the next");
        gate.commit_reached(sequence).await;
    }

    /// The grant handshake as a detached acknowledgement future (does not
    /// borrow the driver), so a test can step the kernel while the ack is
    /// pending — the bounded commit-ack evaluation's shape; not part of
    /// the spec's API. Grant sequencing is enforced at issue time:
    /// per-origin outstanding grants are capped at one
    /// (`GrantOutstanding` otherwise), the returned future waits for this
    /// grant's exact commit (no snapshot aliasing), and the next grant
    /// can only be issued once that acceptance exists.
    pub fn grant_handle(
        &self,
        origin: ProducerId,
    ) -> Result<BoxFuture<'static, ()>, GrantOutstanding> {
        let gate = self.gate(origin);
        let sequence = gate.issue_grant()?;
        Ok(Box::pin(async move { gate.commit_reached(sequence).await }))
    }

    /// Scripted arbitration, stage-granular: runs one stage executor if
    /// and only if it is ready. This decomposes the fixed pass
    /// (RFC 0014 §3.5) for fine-grained assertion; the composed pass
    /// itself is driven by `step_pass` and pinned by the bound tests —
    /// stage orderings scripted here are driving decompositions, not
    /// production pass orders (C-2's citation rule applies).
    pub fn step(&mut self, branch: Branch) -> Result<(), StepError> {
        if self.kernel.terminating() {
            return Err(StepError::Terminated);
        }
        if !self.kernel.ready(branch) {
            return Err(StepError::NotReady(branch));
        }
        self.kernel.run_branch(branch);
        Ok(())
    }

    /// Runs one full fixed pass (exit reflection, then input batch with
    /// its always-finite count cap, then the mandatory control drain,
    /// then the frame step) through the same `pass_cycle` the production
    /// loop runs. `initiation` names the wake source starting the pass
    /// and must be ready — pass initiation is exactly the arbitration
    /// slot RFC 0014 §3.5 leaves scriptable.
    pub fn step_pass(&mut self, initiation: Branch) -> Result<(), StepError> {
        if self.kernel.terminating() {
            return Err(StepError::Terminated);
        }
        if !self.kernel.ready(initiation) {
            return Err(StepError::NotReady(initiation));
        }
        self.kernel.pass_cycle();
        Ok(())
    }

    /// The shared two-stage termination: settle to quiescence.
    pub async fn settle(&mut self) -> ExitReport {
        self.kernel.settle().await
    }

    /// Booted state.
    pub fn state(&self) -> &P::State {
        self.kernel.state()
    }

    /// Post-gate observation ledger.
    pub fn delivery(&self) -> Ledger {
        self.kernel.delivery()
    }

    /// Pre-gate intent ledger.
    pub fn intents(&self) -> Ledger {
        self.kernel.intents()
    }

    /// Gauge family handle (usable after the driver is dropped).
    pub fn gauges(&self) -> Arc<Gauges> {
        self.kernel.gauges()
    }

    /// Registry probes.
    pub fn registry_len(&self) -> usize {
        self.kernel.registry().len()
    }

    /// `(phase, revoked)` of the newest entry labelled `label`.
    pub fn entry_phase(&self, label: &str) -> Option<(Phase, bool)> {
        self.kernel
            .registry()
            .iter()
            .filter(|e| e.label == label)
            .map(|e| (e.phase, e.revoked))
            .next_back()
    }

    /// Pending-counter handle of the newest entry labelled `label`.
    pub fn counter_of(&self, label: &str) -> Arc<PendingCounter> {
        self.kernel
            .registry()
            .iter()
            .filter(|e| e.label == label)
            .map(|e| Arc::clone(&e.counter))
            .next_back()
            .expect("counter for label")
    }

    /// Whether `branch` is currently ready (no side effect beyond join
    /// buffering).
    pub fn ready(&mut self, branch: Branch) -> bool {
        self.kernel.ready(branch)
    }

    /// Yields until the producer labelled `label` has reached at least
    /// `at_least` send intents.
    #[expect(
        clippy::future_not_send,
        reason = "current-thread driving surface; the driver never crosses threads"
    )]
    pub async fn await_intents(&self, label: &str, at_least: u64) {
        let gate = self
            .kernel
            .registry()
            .iter()
            .filter(|e| e.label == label)
            .map(|e| Arc::clone(&e.gate))
            .next_back()
            .expect("gate for label");
        settle_until(
            || gate.intents() >= at_least,
            &format!("producer {label} reaches send intent"),
        )
        .await;
    }

    /// Yields until a real task exit is observable on the `JoinExit`
    /// branch.
    pub async fn await_exit_ready(&mut self) {
        for _ in 0..10_000 {
            if self.kernel.ready(Branch::JoinExit) {
                return;
            }
            yield_now().await;
        }
        assert!(
            self.kernel.ready(Branch::JoinExit),
            "bounded settle exhausted: task exit observable"
        );
    }
}
