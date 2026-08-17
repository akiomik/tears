//! The reducer-first kernel.
//!
//! One driving task owns the application state and a run registry — one
//! entry per producer run, of every kind — with one join set for exit
//! observation, one origin-tagged data lane, and one control lane for
//! producer-originated quits (RFC 0014 §3.1, §3.3, §10).
//!
//! The kernel sits at the crate root rather than under `runtime` because it
//! is not a part of the old `Runtime`: it is the successor core, and
//! `runtime` keeps the entry facade, the construction-time configuration,
//! and the load observability.
//!
//! Two things this module deliberately does not have:
//!
//! - **No render abstraction.** The kernel borrows a
//!   `ratatui::Terminal<B>` directly for the pass stage that renders. A
//!   host trait would give a driver a way around `Program::view`, which is
//!   exactly the third driving difference INV-RC13 excludes; a render
//!   failure is driven by a failing backend instead, so the render owner
//!   stays one type.
//! - **No private kernel configuration type.** Construction reads the two
//!   surviving controls off [`RuntimeConfig`] and nothing else, which is
//!   what keeps the removed controls unreadable here rather than merely
//!   unread.
//!
//! Visibility note: this module and everything under it are crate-capped by
//! `lib.rs`, so items are declared `pub` rather than `pub(crate)` (the
//! convention `runtime::channel` documents).

// Scaffolding stage: the types and signatures below are fixed, the bodies
// are not written yet, and nothing constructs a kernel. Every item here is
// therefore unreachable, which is expected rather than informative; the
// allow comes off once the driving loop is wired.
#![allow(
    dead_code,
    reason = "kernel scaffolding: signatures land before their bodies and before any caller"
)]
// An unwritten body reads none of its receiver, which says nothing about the
// signature. The signatures here are the pinned ones.
#![allow(
    clippy::needless_pass_by_ref_mut,
    reason = "kernel scaffolding: `todo!()` bodies read nothing, so no `&mut` looks used yet"
)]

pub mod accounting;
#[cfg(all(feature = "loom-core", test))]
pub mod accounting_core;
pub mod arbiter;
#[cfg(test)]
mod conformance;
pub mod lane;
pub mod lowering;
pub mod pass;
pub mod producer;
pub mod registry;
pub mod teardown;

use std::collections::{HashMap, VecDeque};
use std::num::NonZeroUsize;

use ratatui::Terminal;
use ratatui::backend::Backend;
use tokio::task::{Id as TaskId, JoinSet};

use crate::command::{Command, SpawnEntry};
use crate::reducer::Program;
use crate::runtime::config::RuntimeConfig;
use crate::runtime::load::LoadObserver;
use crate::structural_key::ScopePath;
use crate::testing::driver::{DeliveryLedger, IntentLedger};

use lane::{
    ControlReceiver, ControlSender, DataReceiver, DataSender, Envelope, GateMode, RunToken,
};
use registry::{ExitOutcome, RunKind, ScopeRegistry};

/// Why the kernel terminated.
///
/// The render error's *value* is not carried here: it flows out through the
/// `Result` of the pass that produced it, so there is one owner for it and
/// the kernel stays independent of the backend type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExitReason {
    /// A controlled quit, of either physical route.
    Quit,
    /// A render failure.
    RenderError,
}

/// The kernel's phase — the only phase machine in the topology.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum KernelPhase {
    /// Constructed, not yet booted. Construction is inert (RFC 0011
    /// INV-LC3).
    Boot,
    /// Running steady-state passes.
    Steady,
    /// Termination applied, immediate postcondition done, settle pending.
    Terminating(ExitReason),
    /// Settled: the join set is drained.
    Settled,
}

/// What bootstrap started, in spawn order.
#[derive(Debug)]
pub struct BootReport {
    /// The producer runs the init dispatch and the initial reconcile
    /// spawned.
    pub producers: Vec<RunToken>,
}

/// The quiescent-postcondition evidence a settle produces (RFC 0011
/// INV-LC6, INV-LC7).
#[derive(Debug)]
pub struct ExitReport {
    /// Why the kernel terminated.
    pub reason: ExitReason,
    /// Tasks joined during settle.
    pub joined: usize,
    /// Whether the producer gauges read zero after the join drain.
    pub gauges_zero: bool,
}

/// The kernel: one driving task, two lanes, one join set, one authoritative
/// registry.
pub struct Kernel<P: Program> {
    program: P,
    flags: Option<P::Flags>,
    state: Option<P::State>,
    registry: ScopeRegistry,
    join_set: JoinSet<()>,
    task_index: HashMap<TaskId, RunToken>,
    // The receivers are dropped at the immediate postcondition, which is
    // what makes an in-flight send fail and end its producer; the senders
    // are held for the kernel's whole lifetime so a live kernel's `recv`
    // can never observe a closed lane (see `lane`'s ownership note).
    data_rx: Option<DataReceiver<P::Message>>,
    data_tx: DataSender<P::Message>,
    data_buf: VecDeque<Envelope<P::Message>>,
    control_rx: Option<ControlReceiver<P::Message>>,
    control_tx: ControlSender<P::Message>,
    control_buf: VecDeque<Envelope<P::Message>>,
    exit_buf: VecDeque<(RunToken, ExitOutcome)>,
    observer: LoadObserver,
    redraw_pending: bool,
    dirty: bool,
    phase: KernelPhase,
    batch_cap: NonZeroUsize,
    gate_mode: GateMode,
    next_token: RunToken,
    delivery: DeliveryLedger,
    intents: IntentLedger,
    settled: bool,
}

impl<P: Program> Kernel<P> {
    /// Inert construction: no task, no poll, no `init` call (RFC 0011
    /// INV-LC3).
    ///
    /// Only the two surviving controls are read from `config`
    /// ([`RuntimeConfig::kernel_controls`]): the data lane's capacity and
    /// the input batch's count cap. The frame rate and the per-command
    /// channel capacity have no successor here, and not reading them is
    /// enforced by that method's return type rather than by this function
    /// remembering to skip them.
    pub fn new(
        _program: P,
        _flags: P::Flags,
        _config: &RuntimeConfig,
        _gate_mode: GateMode,
        _observer: LoadObserver,
    ) -> Self {
        todo!("kernel construction")
    }

    /// Bootstrap in the pinned order: dispatch the init command, then the
    /// initial reconcile, then mark the unconditional first redraw.
    ///
    /// A quit dispatched by `init` short-circuits synchronously: the
    /// reconcile is skipped and the kernel never reaches steady state
    /// (RFC 0014 §6.2, amending RFC 0011's bootstrap).
    pub fn boot(&mut self) -> BootReport {
        todo!("bootstrap")
    }

    /// The booted state.
    pub const fn state(&self) -> &P::State {
        self.state.as_ref().expect("kernel booted")
    }

    /// Whether termination has been applied.
    pub const fn terminating(&self) -> bool {
        matches!(
            self.phase,
            KernelPhase::Terminating(_) | KernelPhase::Settled
        )
    }

    /// The run bookkeeping, for the driver's probes.
    pub const fn registry(&self) -> &ScopeRegistry {
        &self.registry
    }

    /// The load observer this kernel publishes gauges to.
    pub const fn observer(&self) -> &LoadObserver {
        &self.observer
    }

    /// How many runtime-owned tasks the kernel still owns.
    ///
    /// This is the settle loop's input (RFC 0011 INV-LC7), not part of any
    /// observability schema — the gauges are `tears::runtime::load`'s and
    /// keep their own vocabulary.
    pub fn owned_task_count(&self) -> usize {
        self.join_set.len()
    }

    /// The post-gate observation ledger.
    pub fn delivery(&self) -> DeliveryLedger {
        self.delivery.clone()
    }

    /// The pre-gate intent ledger.
    pub fn intents(&self) -> IntentLedger {
        self.intents.clone()
    }

    /// Applies one command: the cancel phase (explicit cancels and teardown
    /// prefixes), then the spawn phase in declaration order, then a
    /// synchronous quit if the command carried one (RFC 0014 §3.4).
    ///
    /// The quit is applied at the *completion* of the dispatch, so siblings
    /// already spawned by this same command exist and are then torn down by
    /// termination.
    pub fn dispatch(&mut self, _command: Command<P::Message>) {
        todo!("cancel phase, spawn phase, synchronous quit")
    }

    /// Applies one spawn entry, honoring the keyed slot policy: a
    /// `CancelInFlight` spawn stops the live occupant and starts fresh, a
    /// `KeepInFlight` spawn is discarded while the slot is occupied
    /// (RFC 0003's policy, on the new registry).
    fn apply_spawn(&mut self, _spawn: SpawnEntry<P::Message>) {
        todo!("keyed slot policy and spawn")
    }

    /// Starts one runtime-owned producer run and records its entry.
    fn spawn_producer(
        &mut self,
        _kind: RunKind,
        _scope: ScopePath,
        _body: producer::EffectBody<P::Message>,
    ) -> RunToken {
        todo!("producer spawn")
    }

    /// Controlled termination: the phase transition plus the immediate
    /// postcondition, shared by every cause (RFC 0011 §4.4).
    fn apply_quit(&mut self, _reason: ExitReason) {
        todo!("termination stage one")
    }

    /// The immediate postcondition: drop both receivers so in-flight sends
    /// fail, clear the buffers, and revoke and abort every run.
    fn immediate_postcondition(&mut self) {
        todo!("immediate postcondition")
    }

    /// The quiescent postcondition: join the shared join set empty, then
    /// read the gauges.
    ///
    /// Bounded by quiescence rather than by a fixed pass count, and it uses
    /// no clock (RFC 0011 INV-LC7).
    pub async fn settle(&mut self) -> ExitReport {
        todo!("termination stage two")
    }

    /// The production driving loop: fixed passes over the shared stage
    /// implementation, parking when no start has work.
    ///
    /// This is the only place the production path differs from the driver's:
    /// the driver replaces the park-and-initiate step with a scripted start
    /// and installs a scripted send gate. The pass itself is the same
    /// [`Kernel::pass_cycle`] call.
    pub async fn run<B: Backend>(
        &mut self,
        _terminal: &mut Terminal<B>,
    ) -> Result<ExitReport, B::Error> {
        todo!("production driving loop")
    }
}

impl<P: Program> Drop for Kernel<P> {
    /// A kernel dropped without settling still aborts what it owns, so no
    /// runtime-owned task outlives it.
    fn drop(&mut self) {
        if !self.settled {
            self.registry.abort_all();
        }
    }
}
