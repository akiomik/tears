//! The two lanes and the enqueue-side protocol of the B kernel: the
//! per-origin pending counter with the saturation/poison rule (spec §2.1
//! rule 6), the RAII `PendingReservation` (rules 1-3), the `SendGate`
//! seam with immediate and scripted modes (§2.2 seam 2), and the
//! `IngressHandle` whose `send`/`quit` enforce the pinned order
//! intent -> gate -> reservation -> send -> commit (§2.1 rule 0).

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{Notify, Semaphore, mpsc};

/// Opaque per-run identity (the spec's `RunToken`; `ProducerId` is its
/// public form).
pub type RunToken = u64;

/// What an envelope carries: a message for the data lane, or a quit for
/// the control lane.
#[derive(Debug)]
pub enum Payload<M> {
    /// An application message.
    Msg(M),
    /// An effect/subscription-issued quit.
    Quit,
}

/// Origin-tagged carrier for both lanes.
#[derive(Debug)]
pub struct Envelope<M> {
    /// The producing run.
    pub origin: RunToken,
    /// The carried payload.
    pub payload: Payload<M>,
}

/// Per-origin committed-pending counter (spec §2.1). Saturating: reaching
/// `u32::MAX` freezes (poisons) the counter, which flips the entry's
/// removal condition to "termination only" — never toward stale delivery.
///
/// Single-atomic encoding: the poisoned state *is* the saturation value
/// (`count == u32::MAX`), so saturating, freezing, and the frozen check
/// are each one atomic transition. The prototype's first cut kept a
/// separate `poisoned` flag beside a `fetch_add`/`fetch_sub`; loom found
/// two interleaving counterexamples against it (a racing reserve stepping
/// past `u32::MAX`, and a racing decrement thawing a just-poisoned
/// counter) — see `counter_core` for the models. This encoding closes
/// both by construction.
#[derive(Debug, Default)]
pub struct PendingCounter {
    count: AtomicU32,
}

/// The saturation value doubles as the poisoned marker.
const POISONED: u32 = u32::MAX;

impl PendingCounter {
    /// Rule 1: reservation increment (saturating; reaching the ceiling
    /// poisons — rule 6).
    pub fn reserve(&self) {
        let mut current = self.count.load(Ordering::SeqCst);
        loop {
            if current == POISONED {
                return;
            }
            match self.count.compare_exchange(
                current,
                current + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return,
                Err(observed) => current = observed,
            }
        }
    }

    /// Rules 3 and 4: release (uncommitted drop) and dequeue share the
    /// same decrement; a poisoned counter is frozen and skips it.
    pub fn decrement(&self) {
        let mut current = self.count.load(Ordering::SeqCst);
        loop {
            if current == POISONED {
                return;
            }
            assert!(current > 0, "pending counter underflow");
            match self.count.compare_exchange(
                current,
                current - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return,
                Err(observed) => current = observed,
            }
        }
    }

    /// Current committed + reserved pending.
    pub fn value(&self) -> u32 {
        self.count.load(Ordering::SeqCst)
    }

    /// Whether the overflow rule froze this counter.
    pub fn is_poisoned(&self) -> bool {
        self.value() == POISONED
    }

    /// Test-side state injection for the overflow rule: pretends `value`
    /// commits are already pending. Not part of the modeled protocol.
    pub fn preset_for_test(&self, value: u32) {
        self.count.store(value, Ordering::SeqCst);
    }
}

/// RAII reservation (spec §2.1 rules 1-3): increments on construction;
/// dropping without `commit` releases; committing transfers the decrement
/// duty to the dequeue side.
pub struct PendingReservation {
    counter: Arc<PendingCounter>,
    committed: bool,
}

impl PendingReservation {
    /// Rule 1: reserve.
    pub fn new(counter: Arc<PendingCounter>) -> Self {
        counter.reserve();
        Self {
            counter,
            committed: false,
        }
    }

    /// Rule 2: the send succeeded; the envelope is in the lane.
    pub fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for PendingReservation {
    fn drop(&mut self) {
        if !self.committed {
            self.counter.decrement();
        }
    }
}

/// Gate policy: production grants immediately; the scripted mode waits
/// for a driver allowance (§2.2 seam 2).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GateMode {
    /// Production: first-poll ready, no behavior change.
    Immediate,
    /// `TestDriver`: allowances are granted one send at a time.
    Scripted,
}

/// Per-origin send gate + commit-ack channel. The commit counter is the
/// acceptance signal C-2 requires: `TestDriver::grant` returns only after
/// observing a commit past its snapshot.
#[derive(Debug)]
pub struct OriginGate {
    mode: GateMode,
    allowances: Semaphore,
    intents: AtomicU64,
    grants_issued: AtomicU64,
    commits: AtomicU64,
    commit_notify: Notify,
}

/// Refusal: this origin's previous grant has not been acknowledged (its
/// send has not committed), so a new grant may not be issued yet.
#[derive(Debug, PartialEq, Eq)]
pub struct GrantOutstanding;

impl OriginGate {
    /// A gate in the given mode with no banked allowances.
    pub fn new(mode: GateMode) -> Self {
        Self {
            mode,
            allowances: Semaphore::new(0),
            intents: AtomicU64::new(0),
            grants_issued: AtomicU64::new(0),
            commits: AtomicU64::new(0),
            commit_notify: Notify::new(),
        }
    }

    /// Records that the producer reached its send intent (pre-gate,
    /// intent-ledger side).
    pub fn note_intent(&self) {
        self.intents.fetch_add(1, Ordering::SeqCst);
    }

    /// How many send intents this origin has reached.
    pub fn intents(&self) -> u64 {
        self.intents.load(Ordering::SeqCst)
    }

    /// Rule 0: the grant await, placed before the reservation. Aborting a
    /// producer parked here drops the future without any counter residue.
    pub async fn acquire(&self) {
        match self.mode {
            GateMode::Immediate => {}
            GateMode::Scripted => {
                self.allowances
                    .acquire()
                    .await
                    .expect("gate semaphore closed")
                    .forget();
            }
        }
    }

    /// Commit count (each committed send of this origin, both lanes).
    pub fn commits(&self) -> u64 {
        self.commits.load(Ordering::SeqCst)
    }

    /// Producer side: the send committed (envelope accepted by the lane).
    pub fn signal_commit(&self) {
        self.commits.fetch_add(1, Ordering::SeqCst);
        self.commit_notify.notify_waiters();
    }

    /// Driver side: issues this origin's next grant — banks one
    /// allowance, wakes the producer, and returns the grant's sequence
    /// number. The nth grant corresponds to exactly the nth commit
    /// (scripted mode admits one send per allowance), so awaiting
    /// `commit_reached(seq)` is an exact acknowledgement, never a
    /// snapshot: a concurrently issued handle cannot complete on someone
    /// else's commit. Refuses (`GrantOutstanding`) while the previous
    /// grant's send has not committed — per-origin outstanding grants
    /// are capped at one, and the next grant can only be issued after the
    /// previous acceptance exists.
    pub fn issue_grant(&self) -> Result<u64, GrantOutstanding> {
        let issued = self.grants_issued.load(Ordering::SeqCst);
        if issued > self.commits() {
            return Err(GrantOutstanding);
        }
        let sequence = issued + 1;
        self.grants_issued.store(sequence, Ordering::SeqCst);
        self.allowances.add_permits(1);
        Ok(sequence)
    }

    /// Driver side: parks until the commit count reaches `sequence` — the
    /// acceptance confirmation half of the grant handshake.
    pub async fn commit_reached(&self, sequence: u64) {
        loop {
            if self.commits() >= sequence {
                return;
            }
            let notified = self.commit_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.commits() >= sequence {
                return;
            }
            notified.await;
        }
    }
}

/// Append-only observation ledger shared between the kernel and tests.
#[derive(Clone, Default, Debug)]
pub struct Ledger(Arc<Mutex<Vec<String>>>);

impl Ledger {
    /// Appends one entry.
    pub fn push(&self, entry: impl Into<String>) {
        self.0.lock().expect("ledger lock").push(entry.into());
    }

    /// Snapshot of all entries.
    pub fn snapshot(&self) -> Vec<String> {
        self.0.lock().expect("ledger lock").clone()
    }

    /// Whether an exact entry was recorded.
    pub fn contains(&self, entry: &str) -> bool {
        self.snapshot().iter().any(|e| e == entry)
    }

    /// How many times an exact entry was recorded.
    pub fn count(&self, entry: &str) -> usize {
        self.snapshot().iter().filter(|e| *e == entry).count()
    }
}

/// The data lane sender: bounded or unbounded per config (spec §4).
pub enum DataSender<M> {
    /// Bounded lane (capacity waits possible).
    Bounded(mpsc::Sender<Envelope<M>>),
    /// Unbounded lane (send is ready in the same poll).
    Unbounded(mpsc::UnboundedSender<Envelope<M>>),
}

impl<M> Clone for DataSender<M> {
    fn clone(&self) -> Self {
        match self {
            Self::Bounded(tx) => Self::Bounded(tx.clone()),
            Self::Unbounded(tx) => Self::Unbounded(tx.clone()),
        }
    }
}

impl<M> DataSender<M> {
    /// Real lane send; `Err` means the receiver side is gone.
    pub async fn send(&self, envelope: Envelope<M>) -> Result<(), ()> {
        match self {
            Self::Bounded(tx) => tx.send(envelope).await.map_err(|_| ()),
            Self::Unbounded(tx) => tx.send(envelope).map_err(|_| ()),
        }
    }
}

/// The data lane receiver counterpart.
pub enum DataReceiver<M> {
    /// Bounded lane.
    Bounded(mpsc::Receiver<Envelope<M>>),
    /// Unbounded lane.
    Unbounded(mpsc::UnboundedReceiver<Envelope<M>>),
}

impl<M> DataReceiver<M> {
    /// Non-blocking dequeue.
    pub fn try_recv(&mut self) -> Option<Envelope<M>> {
        match self {
            Self::Bounded(rx) => rx.try_recv().ok(),
            Self::Unbounded(rx) => rx.try_recv().ok(),
        }
    }

    /// Queued envelope count (readiness input).
    pub fn len(&self) -> usize {
        match self {
            Self::Bounded(rx) => rx.len(),
            Self::Unbounded(rx) => rx.len(),
        }
    }

    /// Blocking dequeue (production park).
    pub async fn recv(&mut self) -> Option<Envelope<M>> {
        match self {
            Self::Bounded(rx) => rx.recv().await,
            Self::Unbounded(rx) => rx.recv().await,
        }
    }
}

/// Error type for sends against a dropped lane (termination).
#[derive(Debug, PartialEq, Eq)]
pub struct SendClosed;

/// The per-run ingress surface handed to producer bodies. Both lanes go
/// through the same pinned order:
/// intent -> gate (rule 0) -> reservation (rule 1) -> real send ->
/// commit (rule 2) / release-on-failure (rule 3).
pub struct IngressHandle<M> {
    origin_label: &'static str,
    origin: RunToken,
    counter: Arc<PendingCounter>,
    gate: Arc<OriginGate>,
    data: DataSender<M>,
    control: mpsc::UnboundedSender<Envelope<M>>,
    intent_ledger: Ledger,
    delivery_ledger: Ledger,
}

impl<M: Send + 'static> IngressHandle<M> {
    /// Builds the handle for one run.
    #[expect(clippy::too_many_arguments, reason = "plain constructor wiring")]
    pub fn new(
        origin_label: &'static str,
        origin: RunToken,
        counter: Arc<PendingCounter>,
        gate: Arc<OriginGate>,
        data: DataSender<M>,
        control: mpsc::UnboundedSender<Envelope<M>>,
        intent_ledger: Ledger,
        delivery_ledger: Ledger,
    ) -> Self {
        Self {
            origin_label,
            origin,
            counter,
            gate,
            data,
            control,
            intent_ledger,
            delivery_ledger,
        }
    }

    async fn send_payload(
        &self,
        describe: &str,
        control: bool,
        payload: Payload<M>,
    ) -> Result<(), SendClosed> {
        self.intent_ledger
            .push(format!("intent:{}:{describe}", self.origin_label));
        self.gate.note_intent();
        self.gate.acquire().await;
        let reservation = PendingReservation::new(Arc::clone(&self.counter));
        let envelope = Envelope {
            origin: self.origin,
            payload,
        };
        let sent = if control {
            self.control.send(envelope).map_err(|_| ())
        } else {
            self.data.send(envelope).await
        };
        match sent {
            Ok(()) => {
                reservation.commit();
                self.gate.signal_commit();
                self.delivery_ledger
                    .push(format!("accept:{}:{describe}", self.origin_label));
                Ok(())
            }
            Err(()) => Err(SendClosed),
        }
    }

    /// Sends a message on the data lane.
    pub async fn send(&self, describe: &str, msg: M) -> Result<(), SendClosed> {
        self.send_payload(describe, false, Payload::Msg(msg)).await
    }

    /// Sends a quit on the control lane.
    pub async fn quit(&self) -> Result<(), SendClosed> {
        self.send_payload("quit", true, Payload::Quit).await
    }
}

/// The context producer bodies receive (currently just the handle).
pub struct EffectCtx<M> {
    /// This run's ingress surface.
    pub handle: IngressHandle<M>,
}
