//! The driving loop of the B-kernel prototype: one phase machine
//! (`Boot -> Steady -> Terminating -> Settled`), four branches behind the
//! Arbiter seam (Input / Control / `JoinExit` / Frame), synchronous quit
//! lowering at dispatch, delivery-side revocation filtering at dequeue,
//! the uniform subscription barrier with the stopping-pass defer rule,
//! and the two-stage termination postcondition.
//!
//! The branch executors are the single implementation shared by the
//! production loop (`run`, immediate gate, fixed-priority pick standing in
//! for the unbiased production arbiter) and by `TestDriver::step`
//! (scripted arbitration). Only the selection policy and the send-grant
//! policy differ (C-1's permitted differences).

use std::collections::{HashMap, VecDeque};
use std::fmt::Debug;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::mpsc;
use tokio::task::{Id as TaskId, JoinError, JoinSet};

use super::cmd::{
    CancelPolicy, Cmd, EffectBody, MockSource, Program, ScopePath, SpawnCmd, SubDecl, ViewSink,
};
use super::lane::{
    DataReceiver, DataSender, EffectCtx, Envelope, GateMode, IngressHandle, Ledger, OriginGate,
    Payload, PendingCounter, RunToken,
};
use super::registry::{Phase, RunEntry, RunKind, ScopeRegistry};

/// The kernel branch vocabulary (Arbiter seam selection set).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Branch {
    /// Micro-batch drain of the data lane.
    Input,
    /// Pass-boundary drain of the control lane.
    Control,
    /// Reflection of one observed task exit.
    JoinExit,
    /// Render (if redraw pending) then re-evaluation (if dirty).
    Frame,
}

/// Why the kernel terminated.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExitReason {
    /// Controlled quit (either physical route).
    Quit,
    /// Host render failure.
    RenderError,
}

/// Kernel phase (the only phase machine in the topology).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum KernelPhase {
    Boot,
    Steady,
    Terminating(ExitReason),
    Settled,
}

/// How one producer task ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExitOutcome {
    /// Ran to completion.
    Completed,
    /// Abort observed.
    Cancelled,
    /// Panic contained as a join error.
    Panicked,
}

/// Producer gauge family (quiescent-postcondition input).
#[derive(Debug, Default)]
pub struct Gauges {
    producers: AtomicU64,
}

impl Gauges {
    /// Live producer count.
    pub fn producers(&self) -> u64 {
        self.producers.load(Ordering::SeqCst)
    }
}

/// Harness-side gauge guard: decrements on drop, panic and abort paths
/// included.
struct GaugeGuard(Arc<Gauges>);

impl GaugeGuard {
    fn new(gauges: Arc<Gauges>) -> Self {
        gauges.producers.fetch_add(1, Ordering::SeqCst);
        Self(gauges)
    }
}

impl Drop for GaugeGuard {
    fn drop(&mut self) {
        self.0.producers.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Passive render service (drives nothing).
pub trait Host {
    /// Renders once; `Err` triggers controlled termination.
    fn render(&mut self, view: &mut dyn FnMut(&mut ViewSink)) -> Result<(), ()>;
}

/// In-memory host; can be scripted to fail at the nth render.
#[derive(Default)]
pub struct HeadlessHost {
    /// Number of renders performed.
    pub renders: usize,
    /// Fail at this (1-based) render count.
    pub fail_at: Option<usize>,
}

impl Host for HeadlessHost {
    fn render(&mut self, view: &mut dyn FnMut(&mut ViewSink)) -> Result<(), ()> {
        self.renders += 1;
        if self.fail_at == Some(self.renders) {
            return Err(());
        }
        let mut sink = ViewSink;
        view(&mut sink);
        Ok(())
    }
}

/// Lane and batch configuration.
#[derive(Clone, Copy, Debug)]
pub struct KernelConfig {
    /// Data lane capacity; `None` is unbounded.
    pub capacity: Option<usize>,
    /// Micro-batch cap for the Input branch.
    pub batch_cap: usize,
}

impl Default for KernelConfig {
    fn default() -> Self {
        Self {
            capacity: None,
            batch_cap: 16,
        }
    }
}

/// Settle report (quiescent postcondition evidence).
#[derive(Debug)]
pub struct ExitReport {
    /// Termination reason.
    pub reason: ExitReason,
    /// Tasks joined during settle.
    pub joined: usize,
    /// Whether the producer gauges read zero after the join drain.
    pub gauges_zero: bool,
}

/// The real forwarder body over a mock source: the single implementation
/// both admitted by `reconcile` and driven directly by the component-layer
/// closure-observation test (S6 arm b). A failed send (`Err` = lane
/// closed) ends the run autonomously — the send-stop policy.
pub fn forwarder_body<M: Send + Debug + 'static>(source: MockSource<M>) -> EffectBody<M> {
    Box::new(move |ctx| {
        Box::pin(async move {
            while let Some(item) = source.next().await {
                let describe = format!("{item:?}");
                if ctx.handle.send(&describe, item).await.is_err() {
                    break;
                }
            }
        })
    })
}

fn lower_into<M>(
    cmd: Cmd<M>,
    teardowns: &mut Vec<ScopePath>,
    spawns: &mut Vec<SpawnCmd<M>>,
    quit: &mut bool,
) {
    match cmd {
        Cmd::None => {}
        Cmd::Quit => *quit = true,
        Cmd::Batch(children) => {
            for child in children {
                lower_into(child, teardowns, spawns, quit);
            }
        }
        Cmd::Spawn(spawn) => spawns.push(spawn),
        Cmd::Teardown(path) => teardowns.push(path),
    }
}

/// The kernel: single driving task, two lanes, one `JoinSet`, one
/// authoritative registry.
pub struct Kernel<P: Program, H> {
    program: P,
    flags: Option<P::Flags>,
    state: Option<P::State>,
    registry: ScopeRegistry,
    join_set: JoinSet<()>,
    task_index: HashMap<TaskId, RunToken>,
    data_rx: Option<DataReceiver<P::Msg>>,
    data_tx: DataSender<P::Msg>,
    data_buf: VecDeque<Envelope<P::Msg>>,
    control_rx: Option<mpsc::UnboundedReceiver<Envelope<P::Msg>>>,
    control_tx: mpsc::UnboundedSender<Envelope<P::Msg>>,
    control_buf: VecDeque<Envelope<P::Msg>>,
    exit_buf: VecDeque<(RunToken, ExitOutcome)>,
    gauges: Arc<Gauges>,
    redraw_pending: bool,
    dirty: bool,
    phase: KernelPhase,
    batch_cap: usize,
    gate_mode: GateMode,
    host: H,
    next_token: RunToken,
    delivery: Ledger,
    intents: Ledger,
    settled: bool,
}

impl<P: Program, H: Host> Kernel<P, H> {
    /// Inert construction: no task, no poll, no `init` call (B-1).
    pub fn new(
        program: P,
        flags: P::Flags,
        config: KernelConfig,
        gate_mode: GateMode,
        host: H,
    ) -> Self {
        let (data_tx, data_rx) = config.capacity.map_or_else(
            || {
                let (tx, rx) = mpsc::unbounded_channel();
                (DataSender::Unbounded(tx), DataReceiver::Unbounded(rx))
            },
            |capacity| {
                let (tx, rx) = mpsc::channel(capacity);
                (DataSender::Bounded(tx), DataReceiver::Bounded(rx))
            },
        );
        let (control_tx, control_rx) = mpsc::unbounded_channel();
        Self {
            program,
            flags: Some(flags),
            state: None,
            registry: ScopeRegistry::default(),
            join_set: JoinSet::new(),
            task_index: HashMap::new(),
            data_rx: Some(data_rx),
            data_tx,
            data_buf: VecDeque::new(),
            control_rx: Some(control_rx),
            control_tx,
            control_buf: VecDeque::new(),
            exit_buf: VecDeque::new(),
            gauges: Arc::new(Gauges::default()),
            redraw_pending: false,
            dirty: false,
            phase: KernelPhase::Boot,
            batch_cap: config.batch_cap,
            gate_mode,
            host,
            next_token: 1,
            delivery: Ledger::default(),
            intents: Ledger::default(),
            settled: false,
        }
    }

    /// Bootstrap in the pinned order: init dispatch, initial reconcile,
    /// unconditional redraw. Returns the spawned producers.
    pub fn boot(&mut self) -> Vec<(RunToken, &'static str)> {
        assert!(self.phase == KernelPhase::Boot, "boot runs once");
        let first_token = self.next_token;
        let flags = self.flags.take().expect("boot consumes flags");
        let (state, init_cmd) = self.program.init(flags);
        self.state = Some(state);
        self.dispatch(init_cmd);
        if !self.terminating() {
            self.delivery.push("reconcile");
            self.reconcile();
        }
        self.redraw_pending = true;
        if !self.terminating() {
            self.phase = KernelPhase::Steady;
        }
        self.registry
            .iter()
            .filter(|e| e.token >= first_token)
            .map(|e| (e.token, e.label))
            .collect()
    }

    /// Booted state accessor.
    pub fn state(&self) -> &P::State {
        self.state.as_ref().expect("kernel booted")
    }

    /// Whether termination has been applied.
    pub fn terminating(&self) -> bool {
        matches!(
            self.phase,
            KernelPhase::Terminating(_) | KernelPhase::Settled
        )
    }

    /// Gauge family handle.
    pub fn gauges(&self) -> Arc<Gauges> {
        Arc::clone(&self.gauges)
    }

    /// Post-gate observation ledger.
    pub fn delivery(&self) -> Ledger {
        self.delivery.clone()
    }

    /// Pre-gate intent ledger (outside the guaranteed observation set).
    pub fn intents(&self) -> Ledger {
        self.intents.clone()
    }

    /// Registry probes for tests.
    pub fn registry(&self) -> &ScopeRegistry {
        &self.registry
    }

    /// Branch readiness. Nothing is ready outside Steady; `JoinExit`
    /// readiness may not be fabricated (it polls the real `JoinSet`).
    pub fn ready(&mut self, branch: Branch) -> bool {
        if self.phase != KernelPhase::Steady {
            return false;
        }
        match branch {
            Branch::Input => {
                !self.data_buf.is_empty() || self.data_rx.as_ref().is_some_and(|rx| rx.len() > 0)
            }
            Branch::Control => {
                !self.control_buf.is_empty()
                    || self.control_rx.as_ref().is_some_and(|rx| !rx.is_empty())
            }
            Branch::JoinExit => self.poll_exit(),
            Branch::Frame => self.redraw_pending || self.dirty,
        }
    }

    fn poll_exit(&mut self) -> bool {
        if !self.exit_buf.is_empty() {
            return true;
        }
        self.join_set.try_join_next_with_id().is_some_and(|result| {
            self.buffer_exit(result);
            true
        })
    }

    fn buffer_exit(&mut self, result: Result<(TaskId, ()), JoinError>) {
        let (id, outcome) = match result {
            Ok((id, ())) => (id, ExitOutcome::Completed),
            Err(err) => {
                let outcome = if err.is_panic() {
                    ExitOutcome::Panicked
                } else {
                    ExitOutcome::Cancelled
                };
                (err.id(), outcome)
            }
        };
        let token = self
            .task_index
            .remove(&id)
            .expect("joined task must be indexed");
        self.exit_buf.push_back((token, outcome));
    }

    /// The single branch executor shared by production and scripted
    /// driving.
    pub fn run_branch(&mut self, branch: Branch) {
        match branch {
            Branch::Input => self.input_batch(),
            Branch::Control => self.control_drain(),
            Branch::JoinExit => self.join_exit(),
            Branch::Frame => self.frame_step(),
        }
    }

    fn next_data(&mut self) -> Option<Envelope<P::Msg>> {
        if let Some(envelope) = self.data_buf.pop_front() {
            return Some(envelope);
        }
        self.data_rx.as_mut().and_then(DataReceiver::try_recv)
    }

    fn next_control(&mut self) -> Option<Envelope<P::Msg>> {
        if let Some(envelope) = self.control_buf.pop_front() {
            return Some(envelope);
        }
        self.control_rx.as_mut().and_then(|rx| rx.try_recv().ok())
    }

    fn input_batch(&mut self) {
        let mut applied = 0usize;
        for _ in 0..self.batch_cap {
            if self.terminating() {
                break;
            }
            let Some(envelope) = self.next_data() else {
                break;
            };
            let (label, revoked) = self.registry.on_dequeue(envelope.origin);
            if revoked {
                self.delivery.push(format!("filtered:{label}"));
                continue;
            }
            if let Payload::Msg(msg) = envelope.payload {
                self.delivery.push(format!("update:{msg:?}"));
                let state = self.state.as_mut().expect("kernel booted");
                let cmd = self.program.reduce(state, msg);
                applied += 1;
                self.dispatch(cmd);
            }
        }
        if applied > 0 {
            self.dirty = true;
            self.redraw_pending = true;
        }
    }

    fn control_drain(&mut self) {
        while !self.terminating() {
            let Some(envelope) = self.next_control() else {
                break;
            };
            let (label, revoked) = self.registry.on_dequeue(envelope.origin);
            if matches!(envelope.payload, Payload::Quit) {
                if revoked {
                    self.delivery.push(format!("filtered-quit:{label}"));
                } else {
                    self.delivery.push(format!("quit:{label}"));
                    self.apply_quit(ExitReason::Quit);
                }
            }
        }
    }

    fn join_exit(&mut self) {
        let Some((token, outcome)) = self.exit_buf.pop_front() else {
            return;
        };
        match self.registry.on_exit(token) {
            Some((label, revoked)) => {
                self.delivery.push(format!("exit:{label}:{outcome:?}"));
                if revoked && !self.terminating() {
                    self.dirty = true;
                }
            }
            None => self.delivery.push(format!("exit:unknown:t{token}")),
        }
    }

    fn frame_step(&mut self) {
        if self.redraw_pending {
            self.redraw_pending = false;
            let Self {
                host,
                program,
                state,
                ..
            } = self;
            let state = state.as_ref().expect("kernel booted");
            let outcome = host.render(&mut |sink| program.view(state, sink));
            self.delivery.push("render");
            if outcome.is_err() {
                self.apply_quit(ExitReason::RenderError);
                return;
            }
        }
        if !self.terminating() && self.dirty {
            self.dirty = false;
            self.delivery.push("reconcile");
            self.reconcile();
        }
    }

    fn dispatch(&mut self, cmd: Cmd<P::Msg>) {
        let mut teardowns = Vec::new();
        let mut spawns = Vec::new();
        let mut quit = false;
        lower_into(cmd, &mut teardowns, &mut spawns, &mut quit);
        for prefix in &teardowns {
            self.apply_teardown(prefix);
        }
        for spawn in spawns {
            self.apply_spawn(spawn);
        }
        if quit {
            self.apply_quit(ExitReason::Quit);
        }
    }

    fn apply_teardown(&mut self, prefix: &ScopePath) {
        self.delivery.push(format!("teardown:{}", prefix.display()));
        for token in self.registry.select_prefix(prefix) {
            let stopped = self.registry.stop_request(token);
            let label = self.registry.get(token).map_or("?", |e| e.label);
            if stopped {
                self.delivery.push(format!("stop:{label}"));
            } else {
                self.delivery.push(format!("revoke:{label}"));
            }
        }
    }

    fn apply_spawn(&mut self, spawn: SpawnCmd<P::Msg>) {
        if let Some(key) = spawn.key
            && let Some(occupant) = self.registry.keyed_occupant(&spawn.scope, key)
        {
            match spawn.policy {
                CancelPolicy::CancelInFlight => {
                    self.registry.stop_request(occupant);
                    self.delivery.push(format!("replace:{}", spawn.label));
                }
                CancelPolicy::KeepInFlight => {
                    self.delivery.push(format!("suppress:{}", spawn.label));
                    return;
                }
            }
        }
        let kind = spawn.key.map_or(RunKind::Anon, RunKind::Keyed);
        self.spawn_producer(spawn.label, kind, spawn.scope, spawn.body);
    }

    fn spawn_producer(
        &mut self,
        label: &'static str,
        kind: RunKind,
        scope: ScopePath,
        body: EffectBody<P::Msg>,
    ) -> RunToken {
        let token = self.next_token;
        self.next_token += 1;
        let counter = Arc::new(PendingCounter::default());
        let gate = Arc::new(OriginGate::new(self.gate_mode));
        let handle = IngressHandle::new(
            label,
            token,
            Arc::clone(&counter),
            Arc::clone(&gate),
            self.data_tx.clone(),
            self.control_tx.clone(),
            self.intents.clone(),
            self.delivery.clone(),
        );
        let guard = GaugeGuard::new(Arc::clone(&self.gauges));
        let fut = body(EffectCtx { handle });
        let abort = self.join_set.spawn(async move {
            let _guard = guard;
            fut.await;
        });
        self.task_index.insert(abort.id(), token);
        self.registry.insert(RunEntry {
            token,
            label,
            kind,
            scope,
            phase: Phase::Running,
            revoked: false,
            exited: false,
            counter,
            gate,
            abort,
        });
        self.delivery.push(format!("spawn:{label}:t{token}"));
        token
    }

    fn admit(&mut self, decl: &SubDecl<P::Msg>) {
        let body = forwarder_body(decl.source.clone());
        let key = decl.key;
        let scope = decl.scope.clone();
        self.spawn_producer(key, RunKind::Sub(key), scope, body);
        self.delivery.push(format!("admit:{key}"));
    }

    /// One subscription re-evaluation: stop phase, then admissions under
    /// the uniform barrier (subscription runs only) and the stopping-pass
    /// defer rule (a pass that issued any stop admits nothing).
    fn reconcile(&mut self) {
        let state = self.state.as_ref().expect("kernel booted");
        let desired = self.program.subscriptions(state);
        let desired_ids: Vec<(ScopePath, &'static str)> =
            desired.iter().map(SubDecl::full_id).collect();
        let mut stops = Vec::new();
        for entry in self.registry.iter() {
            if let RunKind::Sub(key) = entry.kind
                && entry.phase == Phase::Running
                && !desired_ids.contains(&(entry.scope.clone(), key))
            {
                stops.push((entry.token, entry.label));
            }
        }
        let issued = stops.len();
        for (token, label) in stops {
            self.registry.stop_request(token);
            self.delivery.push(format!("stop:{label}"));
        }
        if issued > 0 || self.registry.any_stopping_sub() {
            self.delivery.push("reconcile:deferred");
            return;
        }
        for decl in &desired {
            if !self.registry.sub_running(&decl.scope, decl.key) {
                self.admit(decl);
            }
        }
    }

    /// Controlled termination: phase transition plus the immediate
    /// postcondition, shared by every cause.
    fn apply_quit(&mut self, reason: ExitReason) {
        if self.terminating() {
            return;
        }
        self.phase = KernelPhase::Terminating(reason);
        self.delivery.push(format!("terminating:{reason:?}"));
        self.immediate_postcondition();
    }

    fn immediate_postcondition(&mut self) {
        self.data_rx = None;
        self.data_buf.clear();
        self.control_rx = None;
        self.control_buf.clear();
        self.registry.abort_all();
        self.delivery.push("immediate");
    }

    /// Quiescent postcondition: join the shared `JoinSet` empty, then
    /// read the gauge family (bounded settle — no fixed pass count).
    pub async fn settle(&mut self) -> ExitReport {
        let KernelPhase::Terminating(reason) = self.phase else {
            unreachable!("settle requires termination");
        };
        let mut joined = self.exit_buf.len();
        self.exit_buf.clear();
        while self.join_set.join_next().await.is_some() {
            joined += 1;
        }
        self.task_index.clear();
        let gauges_zero = self.gauges.producers() == 0;
        self.settled = true;
        self.phase = KernelPhase::Settled;
        self.delivery.push("settled");
        ExitReport {
            reason,
            joined,
            gauges_zero,
        }
    }

    /// Production loop: same branch executors, immediate gate expected,
    /// fixed-priority ready pick standing in for the unbiased arbiter
    /// (prototype simplification; the acceptance series drive scripted).
    pub async fn run(&mut self) -> ExitReport {
        let _ = self.boot();
        while !self.terminating() {
            let branch = self.park_next().await;
            self.run_branch(branch);
        }
        self.settle().await
    }

    fn pick_ready(&mut self) -> Option<Branch> {
        [
            Branch::Input,
            Branch::Control,
            Branch::JoinExit,
            Branch::Frame,
        ]
        .into_iter()
        .find(|&b| self.ready(b))
    }

    async fn park_next(&mut self) -> Branch {
        loop {
            if let Some(branch) = self.pick_ready() {
                return branch;
            }
            self.park_once().await;
        }
    }

    async fn park_once(&mut self) {
        enum Woke<M> {
            Data(Option<Envelope<M>>),
            Control(Option<Envelope<M>>),
            Exit(Option<Result<(TaskId, ()), JoinError>>),
        }
        let woke = {
            let data = self.data_rx.as_mut().expect("steady data lane");
            let control = self.control_rx.as_mut().expect("steady control lane");
            let join_set = &mut self.join_set;
            let has_tasks = !join_set.is_empty();
            tokio::select! {
                envelope = data.recv() => Woke::Data(envelope),
                envelope = control.recv() => Woke::Control(envelope),
                result = join_set.join_next_with_id(), if has_tasks => Woke::Exit(result),
            }
        };
        match woke {
            Woke::Data(Some(envelope)) => self.data_buf.push_back(envelope),
            Woke::Control(Some(envelope)) => self.control_buf.push_back(envelope),
            Woke::Exit(Some(result)) => self.buffer_exit(result),
            Woke::Data(None) | Woke::Control(None) | Woke::Exit(None) => {}
        }
    }
}

impl<P: Program, H> Drop for Kernel<P, H> {
    fn drop(&mut self) {
        if !self.settled {
            self.delivery.push("drop:immediate");
            self.registry.abort_all();
        }
    }
}
