//! The two lanes and the enqueue-side protocol.
//!
//! Every producer *message* — keyed command output, anonymous command
//! output, subscription output — travels one origin-tagged FIFO data lane
//! (RFC 0014 §3.1). The one producer output that does not is a
//! producer-originated quit, which takes a dedicated control lane that is
//! **never bounded** (RFC 0014 §3.3, preserving RFC 0006 R4's backlog
//! independence for that route).
//!
//! [`IngressHandle`] is the only send surface a producer body gets, and it
//! pins one order for both lanes:
//!
//! ```text
//! intent -> gate -> reservation -> real send -> commit / release-on-failure
//! ```
//!
//! The gate wait sits *before* the reservation so that a producer aborted
//! while parked on a grant holds no reservation and needs no release: it
//! never entered the delivery accounting at all.
//!
//! ## Lane ownership
//!
//! The kernel holds a clone of both senders for its whole lifetime, and
//! [`IngressHandle`]s are minted from those clones. A receive on either lane
//! therefore cannot observe a closed channel while the kernel is alive; that
//! is a construction invariant, and the park path treats a violation of it
//! as a defect to park on rather than a condition to spin on (RFC 0014
//! §3.5's arming, §9 row 1).

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use tokio::sync::{Notify, Semaphore, mpsc};

use crate::runtime::channel;
use crate::testing::driver::{DeliveryLedger, IntentLedger};

use super::accounting::PendingCounter;

/// Opaque per-run identity. Every envelope carries the token of the run
/// that produced it, which is what makes the delivery decision a lookup on
/// the producing run's liveness (RFC 0014 §3.1).
pub type RunToken = u64;

/// What an envelope carries.
pub enum Payload<Msg> {
    /// An application message, translated from `Action::Message`.
    Msg(Msg),
    /// A producer-originated quit, translated from `Action::Quit`.
    Quit,
}

/// Origin-tagged carrier for both lanes.
pub struct Envelope<Msg> {
    /// The producing run.
    pub origin: RunToken,
    /// The carried payload.
    pub payload: Payload<Msg>,
}

/// The data lane's sending half: the runtime's own channel wrapper, so
/// bounded mode brings its capacity-wait event and `blocked` gauge with it
/// (RFC 0006 §4.4) under the `data` channel label.
pub type DataSender<Msg> = channel::Sender<Envelope<Msg>>;

/// The data lane's receiving half.
pub type DataReceiver<Msg> = channel::Receiver<Envelope<Msg>>;

/// The control lane's sending half. Always unbounded (RFC 0006 R4).
pub type ControlSender<Msg> = mpsc::UnboundedSender<Envelope<Msg>>;

/// The control lane's receiving half.
pub type ControlReceiver<Msg> = mpsc::UnboundedReceiver<Envelope<Msg>>;

/// Builds the control lane. Its unboundedness is not configurable: a quit
/// must never queue behind the data lane's backlog or its capacity waits
/// (RFC 0014 §3.3).
pub fn control_lane<Msg>() -> (ControlSender<Msg>, ControlReceiver<Msg>) {
    mpsc::unbounded_channel()
}

/// Gate policy. Two implementations, chosen by value rather than by trait
/// object: the seam is one branch in one function, and static dispatch keeps
/// the driving differential from taking the shape of a swappable component
/// (RFC 0014 §7.2).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GateMode {
    /// Production: a grant is available on the first poll, so the gate adds
    /// no behavior.
    Immediate,
    /// The driver's: allowances are issued one send at a time.
    Scripted,
}

/// Refusal from [`SendGate::issue_grant`]: this origin's previous grant has
/// not been acknowledged — its send has not committed — so a second
/// outstanding grant would break the one-grant-per-origin correspondence the
/// acknowledgement relies on.
#[derive(Debug, Eq, PartialEq)]
pub struct GrantOutstanding;

/// A send against a lane whose receiver is gone. Producers translate this
/// into ending their run (the send-stop policy).
#[derive(Debug, Eq, PartialEq)]
pub struct SendClosed;

/// Per-origin send gate and commit acknowledgement.
///
/// The commit counter is the acceptance signal: the *n*th grant of an origin
/// corresponds to exactly that origin's *n*th commit, because scripted mode
/// admits one send per allowance. Awaiting
/// [`commit_reached`](Self::commit_reached) for a grant's own sequence
/// number is therefore an exact acknowledgement and never a snapshot
/// comparison that a concurrently issued handle could satisfy.
pub struct SendGate {
    mode: GateMode,
    allowances: Semaphore,
    intents: AtomicU64,
    grants_issued: AtomicU64,
    commits: AtomicU64,
    commit_notify: Notify,
}

impl SendGate {
    /// A gate in the given mode with no banked allowances.
    pub fn new(_mode: GateMode) -> Self {
        todo!("gate construction")
    }

    /// Records that the producer reached its send intent, before the gate
    /// wait. Pre-gate observation only.
    pub fn note_intent(&self) {
        todo!("intent accounting")
    }

    /// How many send intents this origin has reached.
    pub fn intents(&self) -> u64 {
        todo!("intent accounting")
    }

    /// The grant await, placed before the reservation. A producer aborted
    /// while parked here drops this future with no counter residue.
    pub async fn acquire(&self) {
        todo!("gate wait")
    }

    /// Commit count: each committed send of this origin, on either lane.
    pub fn commits(&self) -> u64 {
        todo!("commit accounting")
    }

    /// Producer side: the send committed — the envelope is in the lane.
    pub fn signal_commit(&self) {
        todo!("commit accounting")
    }

    /// Driver side: issues this origin's next grant, returning its sequence
    /// number. Refuses while the previous grant's send has not committed, so
    /// per-origin outstanding grants are capped at one and the next grant
    /// exists only after the previous acceptance does.
    pub fn issue_grant(&self) -> Result<u64, GrantOutstanding> {
        todo!("grant handshake")
    }

    /// Driver side: parks until the commit count reaches `sequence` — the
    /// acceptance half of the handshake.
    ///
    /// The wait re-checks after enabling the notification and before
    /// awaiting it, so a commit landing between the check and the park is
    /// not lost.
    pub async fn commit_reached(&self, _sequence: u64) {
        todo!("grant handshake")
    }
}

/// The per-run ingress surface handed to a producer body.
///
/// Both lanes go through [`send_payload`](Self::send_payload)'s single
/// pinned order, so there is one place where the accounting rules apply and
/// no second send path for a producer kind to take.
pub struct IngressHandle<Msg> {
    origin: RunToken,
    counter: Arc<PendingCounter>,
    gate: Arc<SendGate>,
    data: DataSender<Msg>,
    control: ControlSender<Msg>,
    // Observation only, and deliberately not the `tears::runtime::load`
    // schema: the two ledgers record what a driving test asserts on, either
    // side of the gate. Holding them here rather than only in test builds
    // keeps the driven topology identical to the production one.
    intents: IntentLedger,
    delivery: DeliveryLedger,
}

impl<Msg: Send + 'static> IngressHandle<Msg> {
    /// Builds the handle for one run.
    pub fn new(
        _origin: RunToken,
        _counter: Arc<PendingCounter>,
        _gate: Arc<SendGate>,
        _data: DataSender<Msg>,
        _control: ControlSender<Msg>,
        _intents: IntentLedger,
        _delivery: DeliveryLedger,
    ) -> Self {
        todo!("ingress construction")
    }

    /// The run this handle sends on behalf of.
    pub const fn origin(&self) -> RunToken {
        self.origin
    }

    /// Sends a message on the data lane.
    pub async fn send(&self, _msg: Msg) -> Result<(), SendClosed> {
        todo!("data-lane send")
    }

    /// Sends a quit on the control lane.
    pub async fn quit(&self) -> Result<(), SendClosed> {
        todo!("control-lane send")
    }

    /// The one send path, shared by both lanes:
    /// intent, gate, reservation, real send, then commit on success or the
    /// reservation's own release on failure.
    async fn send_payload(&self, _control: bool, _payload: Payload<Msg>) -> Result<(), SendClosed> {
        todo!("pinned send order")
    }
}
