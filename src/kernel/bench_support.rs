//! Bench-only handles for the load-acceptance re-derivation RFC 0014 §13.5
//! owes RFC 0006 (§5.2's named prerequisite for INV-L4).
//!
//! The kernel is `pub(crate)`, and `benches/` compiles as separate crates that
//! see the public API only, so a bench cannot construct a kernel, a registry,
//! or a cleanup ledger at all. This module is the same bench-only escape the
//! crate already uses twice — `BenchSubscriptionManager` for the subscription
//! hot path and the `LoadObserver` re-export for the gauge bench — applied to
//! the kernel: gated behind `bench-internals`, which is not part of the public
//! API and carries no semver guarantee, and `#[doc(hidden)]` at its re-export.
//!
//! Three handles, one per measured object:
//!
//! - [`BenchKernel`] runs the **production driving loop** — `Kernel::drive`
//!   and `Kernel::settle`, unchanged — over an [`Application`]-adapted
//!   program. It splits `Kernel::run`'s two halves so a harness can timestamp
//!   between them; it adds no loop, no branch, and no stage of its own, which
//!   is what keeps the numbers a measurement *of production* rather than of a
//!   bench-shaped imitation.
//! - [`RegistryScan`] holds a populated [`ScopeRegistry`] and exposes the four
//!   linear walks a dispatch performs over it — `keyed_occupant`,
//!   `sub_running`, `any_stopping_sub`, and `select_prefix`. Their cost in the
//!   number of live runs is mechanism (RFC 0013 §3.7) that
//!   `ScopeRegistry::select_prefix`'s own doc defers to this measurement.
//! - [`CleanupLedgerScan`] does the same for `CleanupLedger::take_under`.
//!
//! Nothing here is reachable from a normal build: the module itself is behind
//! the feature gate, so a default `cargo build` does not compile it.

use std::future::pending;
use std::future::ready;
use std::sync::Arc;

use futures::stream::{self, BoxStream, StreamExt};
use ratatui::Terminal;
use ratatui::backend::Backend;
use tokio::task::JoinSet;

use crate::application::Application;
use crate::command::{Action, CleanupRegistration, Command, CommandId};
use crate::reducer::adapter::AppProgram;
use crate::runtime::config::RuntimeConfig;
use crate::runtime::load::LoadObserver;
use crate::structural_key::ScopePath;
use crate::subscription::{Subscription, SubscriptionId, SubscriptionSource};

use super::Kernel;
use super::accounting::PendingCounter;
use super::cleanup::CleanupLedger;
use super::lane::{GateMode, RunToken, SendGate};
use super::registry::{Phase, RunEntry, RunKind, ScopeRegistry};

/// A command whose spawned run emits one **producer-originated** quit on the
/// control lane and then ends (RFC 0014 §3.3).
///
/// `on_emit` runs on the producer task in the poll that yields the quit,
/// immediately before the harness hands it to the control lane — the send-side
/// instant a latency measurement starts from. `Command::quit` is the other
/// route and is not this one: it builds the immediate-quit carrier, applied
/// synchronously at its dispatch with no lane involved, so it cannot measure
/// the lane this invariant is about.
pub fn producer_quit<Msg, F>(on_emit: F) -> Command<Msg>
where
    Msg: Send + 'static,
    F: FnOnce() + Send + 'static,
{
    Command::actions(stream::once(async move {
        on_emit();
        Action::Quit
    }))
}

/// The production kernel, driven by the production loop.
pub struct BenchKernel<A: Application> {
    kernel: Kernel<AppProgram<A>>,
}

impl<A: Application> BenchKernel<A> {
    /// Builds an inert kernel over `A`, in the production send-gate mode.
    ///
    /// Only [`RuntimeConfig`]'s two surviving controls reach it, through
    /// `RuntimeConfig::kernel_controls` — the data lane's capacity (unset =
    /// unbounded lane mode) and the input batch's count cap.
    pub fn new(flags: A::Flags, config: &RuntimeConfig) -> Self {
        Self {
            kernel: Kernel::new(
                AppProgram::new(),
                flags,
                config,
                GateMode::Immediate,
                LoadObserver::new(),
            ),
        }
    }

    /// The production driving loop up to termination: `boot`, then passes over
    /// the shared stage implementation, parking when nothing has work.
    ///
    /// This is `Kernel::drive` itself. It returns once the quit has been
    /// applied *and* its immediate postcondition has run (the receivers
    /// dropped, the buffers cleared, every run revoked and aborted), which is
    /// what a caller timestamping this return is measuring.
    ///
    /// # Errors
    ///
    /// Returns the backend's error when a render failed (RFC 0011 INV-LC5).
    pub async fn drive<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> Result<(), B::Error> {
        self.kernel.drive(terminal).await
    }

    /// The quiescent postcondition, returning how many runtime-owned tasks it
    /// accounted for.
    pub async fn settle(&mut self) -> usize {
        self.kernel.settle().await.joined
    }
}

/// A bench-only subscription source with a caller-chosen key whose stream
/// never yields, so the run it starts stays parked until it is aborted.
struct ParkedSource(u64);

impl SubscriptionSource for ParkedSource {
    type Output = ();
    type Key = u64;

    fn stream(&self) -> BoxStream<'static, Self::Output> {
        stream::pending::<()>().boxed()
    }

    fn key(&self) -> Self::Key {
        self.0
    }
}

/// A subscription identity with the given key, for populating a registry.
fn subscription_id(key: u64) -> SubscriptionId {
    Subscription::new(ParkedSource(key)).id().clone()
}

/// The scope a populated run at `index` is attributed to: `["pane", index]`,
/// root-first. A `["pane"]` prefix therefore selects every one of them, and a
/// prefix rooted at any other segment selects none — the two ends of
/// `select_prefix`'s result size, walked identically.
fn run_scope(index: usize) -> ScopePath {
    ScopePath::empty().prefixed(index).prefixed("pane")
}

/// A populated [`ScopeRegistry`] and the four linear walks over it.
///
/// The runs are real entries with real abort handles: the join set below owns
/// one parked task per entry, because `RunEntry::abort` has no constructible
/// stand-in. Building one therefore requires a tokio runtime context.
pub struct RegistryScan {
    registry: ScopeRegistry,
    /// Owns the parked tasks the entries' abort handles point into. Dropped
    /// with the probe, which aborts them.
    _tasks: JoinSet<()>,
    hit_key: CommandId,
    miss_key: CommandId,
    hit_sub: SubscriptionId,
    miss_sub: SubscriptionId,
    prefix_all: ScopePath,
    prefix_none: ScopePath,
}

impl RegistryScan {
    /// A registry holding `runs` live entries, cycling the three producer
    /// kinds so every walk's predicate meets all of them.
    ///
    /// Every entry is `Running`, unrevoked, and unexited — the steady state
    /// each walk actually runs in. The `hit` identities name the **last**
    /// entry of their kind in token order, so a hitting lookup still traverses
    /// the whole map and the hit/miss pair differs only in the terminating
    /// `find`.
    ///
    /// # Panics
    ///
    /// Panics outside a tokio runtime context, which spawning the entries'
    /// tasks requires.
    #[must_use]
    pub fn with_runs(runs: usize) -> Self {
        let mut tasks = JoinSet::new();
        let mut registry = ScopeRegistry::new(LoadObserver::new());
        let gate = Arc::new(SendGate::new(GateMode::Immediate));
        let mut hit_key = CommandId::new(("bench-keyed", 0_u64));
        let mut hit_sub = subscription_id(0);
        for index in 0..runs {
            let abort = tasks.spawn(pending());
            let kind = match index % 3 {
                0 => {
                    let id = CommandId::new(("bench-keyed", index as u64));
                    hit_key = id.clone();
                    RunKind::Keyed(id)
                }
                1 => RunKind::Anon,
                _ => {
                    let id = subscription_id(index as u64);
                    hit_sub = id.clone();
                    RunKind::Sub(id)
                }
            };
            registry.insert(RunEntry {
                token: index as RunToken + 1,
                kind,
                scope: run_scope(index),
                phase: Phase::Running,
                revoked: false,
                exited: false,
                counter: Arc::new(PendingCounter::default()),
                gate: Arc::clone(&gate),
                abort,
            });
        }
        Self {
            registry,
            _tasks: tasks,
            hit_key,
            // Shares the entries' key *shape*, differing only in the last
            // field: a miss key of a different shape would be rejected on the
            // first component and measure a cheaper comparison than the walk
            // actually performs.
            miss_key: CommandId::new(("bench-keyed", u64::MAX)),
            hit_sub,
            miss_sub: subscription_id(u64::MAX),
            prefix_all: ScopePath::empty().prefixed("pane"),
            prefix_none: ScopePath::empty().prefixed("absent"),
        }
    }

    /// How many entries the registry holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.registry.len()
    }

    /// Whether the registry is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.registry.len() == 0
    }

    /// `keyed_occupant` at an id the last keyed entry holds.
    #[must_use]
    pub fn keyed_occupant_hit(&self) -> bool {
        self.registry.keyed_occupant(&self.hit_key).is_some()
    }

    /// `keyed_occupant` at an id no entry holds — the exhaustive walk.
    #[must_use]
    pub fn keyed_occupant_miss(&self) -> bool {
        self.registry.keyed_occupant(&self.miss_key).is_some()
    }

    /// `sub_running` at the last subscription entry's identity.
    #[must_use]
    pub fn sub_running_hit(&self) -> bool {
        self.registry.sub_running(&self.hit_sub)
    }

    /// `sub_running` at an identity no entry holds — the exhaustive walk, and
    /// the one an admission actually performs per declaration.
    #[must_use]
    pub fn sub_running_miss(&self) -> bool {
        self.registry.sub_running(&self.miss_sub)
    }

    /// The uniform barrier predicate. No entry is `Stopping`, so this is
    /// always the exhaustive walk — the steady-state case a re-evaluation
    /// pays.
    #[must_use]
    pub fn any_stopping_sub(&self) -> bool {
        self.registry.any_stopping_sub()
    }

    /// `select_prefix` at a prefix every run lies under.
    #[must_use]
    pub fn select_prefix_all(&self) -> usize {
        self.registry.select_prefix(&self.prefix_all).len()
    }

    /// `select_prefix` at a prefix no run lies under.
    #[must_use]
    pub fn select_prefix_none(&self) -> usize {
        self.registry.select_prefix(&self.prefix_none).len()
    }
}

/// A populated [`CleanupLedger`] and its one selecting operation.
pub struct CleanupLedgerScan {
    ledger: CleanupLedger,
    armed: usize,
    prefix_all: ScopePath,
    prefix_none: ScopePath,
}

impl CleanupLedgerScan {
    /// A ledger holding `registrations` armed finalizers, each anchored under
    /// the `["pane"]` prefix.
    #[must_use]
    pub fn with_registrations(registrations: usize) -> Self {
        let mut probe = Self {
            ledger: CleanupLedger::new(),
            armed: registrations,
            prefix_all: ScopePath::empty().prefixed("pane"),
            prefix_none: ScopePath::empty().prefixed("absent"),
        };
        probe.refill();
        probe
    }

    /// Re-arms the ledger to its populated size, for an iteration whose
    /// `take_under` consumed it.
    pub fn refill(&mut self) {
        self.ledger.discard_all();
        for index in 0..self.armed {
            let mut registration = CleanupRegistration::new(ready(()));
            registration.scope = run_scope(index);
            self.ledger.register(registration);
        }
    }

    /// How many registrations are armed.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.ledger.len()
    }

    /// Whether nothing is armed.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.ledger.is_empty()
    }

    /// `take_under` at a prefix every registration lies under — the partition
    /// plus the consuming move. Leaves the ledger empty.
    pub fn take_under_all(&mut self) -> usize {
        self.ledger.take_under(&self.prefix_all).len()
    }

    /// `take_under` at a prefix no registration lies under — the partition
    /// alone. Leaves the ledger populated, so it repeats without a refill.
    pub fn take_under_none(&mut self) -> usize {
        self.ledger.take_under(&self.prefix_none).len()
    }
}
