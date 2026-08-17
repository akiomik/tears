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

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::{Notify, mpsc};

use crate::runtime::channel;
use crate::testing::driver::{AcceptanceRecorder, Confirmed, IntentRecorder};

use super::accounting::{PendingCounter, PendingReservation};

/// Opaque per-run identity. Every envelope carries the token of the run
/// that produced it, which is what makes the delivery decision a lookup on
/// the producing run's liveness (RFC 0014 §3.1).
pub type RunToken = u64;

/// Which of the two lanes carried a send.
///
/// The kernel owns exactly these two and no third (INV-RC15), so this enum
/// is the whole routing vocabulary: nothing per run, per key, or per class
/// exists to name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Lane {
    /// The FIFO data lane every producer's message output shares.
    Data,
    /// The unbounded control lane, carrying producer-originated quits and
    /// nothing else.
    Control,
}

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

/// Refusal from [`SendGate::issue_grant`]: a grant is already outstanding on
/// this gate.
///
/// The rule is **driver-wide**, not per-origin: while one grant is
/// unresolved, a grant at *any* origin is refused. A per-origin rule would
/// admit `grant(a); grant(b)` — two releases ordered by nothing but the
/// order they were asked for — which is precisely the raw grant order
/// RFC 0014 §11 excludes.
#[derive(Debug, Eq, PartialEq)]
pub struct GrantOutstanding;

/// A send against a lane whose receiver is gone. Producers translate this
/// into ending their run (the send-stop policy).
#[derive(Debug, Eq, PartialEq)]
pub struct SendClosed;

/// The send gate: one seam covering **every** producer send on **both**
/// lanes, a producer-originated quit included.
///
/// Production installs it in [`Immediate`](GateMode::Immediate) mode, where
/// it holds no state and adds no wait. Scripted, it holds at most one
/// outstanding grant for the whole kernel, and that single slot is what
/// carries the two conditions RFC 0014 §13.3 names:
///
/// - **ack correlation** — one outstanding grant driver-wide implies at most
///   one per origin, and the grant's own sequence number correlates it to
///   the release it produced, so a resolution can never be some other
///   release's.
/// - **driver progress** — resolution is *observed*, never awaited here.
///   [`take_resolution`](Self::take_resolution) reports the terminal a
///   released send has already reached and reports its absence otherwise, so
///   the caller keeps its turns and can step the kernel to drain the lane a
///   capacity-blocked send is waiting on.
///
/// A released send has exactly two terminals — commit, or its reservation
/// released without committing — and both are recorded here, which is what
/// makes [`Confirmed`]'s two arms observable rather than inferred.
#[derive(Debug)]
pub struct SendGate {
    mode: GateMode,
    state: Mutex<GateState>,
    /// Woken when a grant is armed. Producers parked in
    /// [`acquire`](SendGate::acquire) re-check the slot on each wake, since
    /// a grant names one origin and wakes them all.
    released: Notify,
}

/// The gate's whole scripted state: a monotone grant sequence and the one
/// outstanding grant.
#[derive(Debug, Default)]
struct GateState {
    next_sequence: u64,
    outstanding: Option<Grant>,
}

/// One armed grant and, once its released send resolves, its terminal.
#[derive(Debug)]
struct Grant {
    sequence: u64,
    origin: RunToken,
    /// Whether the named producer has taken the release. Until it has, no
    /// send has been released and no terminal can be reported.
    taken: bool,
    /// The released send's terminal, once it has one.
    outcome: Option<Confirmed>,
}

impl SendGate {
    /// A gate in the given mode with no grant outstanding.
    pub fn new(mode: GateMode) -> Self {
        Self {
            mode,
            state: Mutex::new(GateState::default()),
            released: Notify::new(),
        }
    }

    /// The grant await, placed before the reservation. A producer aborted
    /// while parked here drops this future with no counter residue, so it
    /// never entered the accounting and needs no release.
    ///
    /// The re-check after enabling the notification is not optional: a grant
    /// armed between the check and the park would otherwise be lost, because
    /// a notification only reaches waiters already registered when it is
    /// sent.
    pub async fn acquire(&self, origin: RunToken) {
        if self.mode == GateMode::Immediate {
            return;
        }
        loop {
            if self.take_release(origin) {
                return;
            }
            let notified = self.released.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.take_release(origin) {
                return;
            }
            notified.await;
        }
    }

    /// Driver side: arms this gate's next grant at `origin` and returns its
    /// sequence number. Refuses while a grant is outstanding, whatever
    /// origin it names.
    pub fn issue_grant(&self, origin: RunToken) -> Result<u64, GrantOutstanding> {
        let sequence = {
            let mut state = self.lock();
            if state.outstanding.is_some() {
                return Err(GrantOutstanding);
            }
            state.next_sequence += 1;
            let sequence = state.next_sequence;
            state.outstanding = Some(Grant {
                sequence,
                origin,
                taken: false,
                outcome: None,
            });
            sequence
        };
        self.released.notify_waiters();
        Ok(sequence)
    }

    /// Driver side: the terminal the grant `sequence` released, once it has
    /// one, clearing the outstanding grant as it reports.
    ///
    /// `None` means the released send has not resolved yet — it may still be
    /// awaiting capacity — and the grant stays outstanding. This never
    /// blocks, so a caller that needs the kernel to drain first keeps its
    /// turns to do so.
    ///
    /// # Panics
    ///
    /// Panics when `sequence` is not the outstanding grant's. A token that
    /// names a grant this gate no longer holds is a script error with no
    /// kernel state behind it.
    pub fn take_resolution(&self, sequence: u64) -> Option<Confirmed> {
        let mut state = self.lock();
        let grant = state
            .outstanding
            .as_ref()
            .expect("a confirmed token names this gate's outstanding grant");
        assert_eq!(
            grant.sequence, sequence,
            "a grant is resolved through its own token"
        );
        let outcome = grant.outcome?;
        state.outstanding = None;
        drop(state);
        Some(outcome)
    }

    /// Whether a grant is outstanding — the predicate that makes a settle
    /// legal or misuse, and the one that makes an append to the guaranteed
    /// sequence structurally impossible during a legal settle.
    pub fn grant_outstanding(&self) -> bool {
        self.lock().outstanding.is_some()
    }

    /// Producer side: takes the release if the outstanding grant names
    /// `origin` and has not been taken.
    fn take_release(&self, origin: RunToken) -> bool {
        let mut state = self.lock();
        match state.outstanding.as_mut() {
            Some(grant) if grant.origin == origin && !grant.taken => {
                grant.taken = true;
                true
            }
            _ => false,
        }
    }

    /// Producer side: records the terminal a released send reached.
    ///
    /// Immediate mode releases nothing, so there is nothing to correlate and
    /// nothing is recorded — the production path pays no bookkeeping for a
    /// seam only the driver uses.
    fn resolve(&self, outcome: Confirmed) {
        if self.mode == GateMode::Immediate {
            return;
        }
        let mut state = self.lock();
        let grant = state
            .outstanding
            .as_mut()
            .expect("a released send resolves under its own grant");
        assert!(
            grant.taken,
            "a terminal is reported only for a release a producer took"
        );
        assert!(
            grant.outcome.is_none(),
            "a released send reaches exactly one terminal"
        );
        grant.outcome = Some(outcome);
        drop(state);
    }

    /// Recovers from a poisoned lock rather than propagating it:
    /// [`resolve`](Self::resolve) runs from a drop, which includes the
    /// unwind of a producer panicking mid-send, and a lock `expect` there
    /// would panic during unwinding and abort the process. The state is a
    /// sequence number beside one slot, so a panic between the two leaves it
    /// usable rather than corrupt.
    fn lock(&self) -> MutexGuard<'_, GateState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// RAII terminal for one *released* send: the gate's half of what the
/// reservation is to the counter.
///
/// Committing reports [`Confirmed::Accepted`]; every other end — an `Err`
/// return, an abort at the capacity wait inside the send, a panic unwinding
/// through it — drops this guard and reports [`Confirmed::Reclaimed`],
/// which is exactly the set of ends that leave the reservation released
/// without a commit.
struct ReleasedSend<'gate> {
    gate: &'gate SendGate,
    committed: bool,
}

impl<'gate> ReleasedSend<'gate> {
    /// Begins the terminal's lifetime, at the moment the gate released the
    /// send.
    const fn new(gate: &'gate SendGate) -> Self {
        Self {
            gate,
            committed: false,
        }
    }

    /// The send committed: the envelope is in its lane.
    fn commit(mut self) {
        self.committed = true;
        self.gate.resolve(Confirmed::Accepted);
    }
}

impl Drop for ReleasedSend<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.gate.resolve(Confirmed::Reclaimed);
        }
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
    intents: IntentRecorder,
    acceptances: AcceptanceRecorder,
}

impl<Msg: Send + 'static> IngressHandle<Msg> {
    /// Builds the handle for one run.
    pub const fn new(
        origin: RunToken,
        counter: Arc<PendingCounter>,
        gate: Arc<SendGate>,
        data: DataSender<Msg>,
        control: ControlSender<Msg>,
        intents: IntentRecorder,
        acceptances: AcceptanceRecorder,
    ) -> Self {
        Self {
            origin,
            counter,
            gate,
            data,
            control,
            intents,
            acceptances,
        }
    }

    /// The run this handle sends on behalf of.
    pub const fn origin(&self) -> RunToken {
        self.origin
    }

    /// Sends a message on the data lane.
    pub async fn send(&self, msg: Msg) -> Result<(), SendClosed> {
        self.send_payload(Lane::Data, Payload::Msg(msg)).await
    }

    /// Sends a quit on the control lane.
    ///
    /// Gated exactly like a message: the seam covers both lanes, so a
    /// producer's quit is scriptable the way its messages are (RFC 0008
    /// §9.6).
    pub async fn quit(&self) -> Result<(), SendClosed> {
        self.send_payload(Lane::Control, Payload::Quit).await
    }

    /// The one send path, shared by both lanes:
    /// intent, gate, reservation, real send, then commit on success or the
    /// reservation's own release on failure.
    ///
    /// The order is the whole protocol. The intent is recorded first,
    /// because a record of reaching the gate must survive never passing it.
    /// The gate wait precedes the reservation, so an abort while parked
    /// there leaves no counter residue. The reservation precedes the send,
    /// so the run's entry cannot be reclaimed between the send's acceptance
    /// and the accounting of it. And the commit precedes the acceptance
    /// record, which precedes the gate's terminal — so a driver that
    /// observes `Accepted` observes a ledger that already carries the entry.
    async fn send_payload(&self, lane: Lane, payload: Payload<Msg>) -> Result<(), SendClosed> {
        self.intents.record(self.origin, lane);
        self.gate.acquire(self.origin).await;
        let released = ReleasedSend::new(&self.gate);
        let reservation = PendingReservation::new(Arc::clone(&self.counter));
        let envelope = Envelope {
            origin: self.origin,
            payload,
        };
        let sent = match lane {
            Lane::Data => self.data.send(envelope).await.map_err(|_| SendClosed),
            Lane::Control => self.control.send(envelope).map_err(|_| SendClosed),
        };
        match sent {
            Ok(()) => {
                reservation.commit();
                self.acceptances.record(self.origin, lane);
                released.commit();
                Ok(())
            }
            // Both guards drop here, reservation first: the counter is
            // released, and the gate then reports that release as this
            // grant's terminal.
            Err(SendClosed) => Err(SendClosed),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::sync::Arc;

    use futures::poll;

    use super::{
        Confirmed, ControlReceiver, ControlSender, DataReceiver, DataSender, Envelope, GateMode,
        GrantOutstanding, IngressHandle, Lane, Payload, RunToken, SendClosed, SendGate,
        control_lane,
    };
    use crate::kernel::accounting::PendingCounter;
    use crate::runtime::channel;
    use crate::testing::driver::{AcceptanceRecorder, GatedSend, IntentRecorder};

    const ALICE: RunToken = 1;
    const BOB: RunToken = 2;

    /// One run's ingress wiring, kept together so a test can assert on the
    /// counter and on the receiving ends the handle sends into.
    struct Fixture {
        handle: IngressHandle<u32>,
        counter: Arc<PendingCounter>,
        gate: Arc<SendGate>,
        data_rx: DataReceiver<u32>,
        control_rx: ControlReceiver<u32>,
        intents: IntentRecorder,
        acceptances: AcceptanceRecorder,
        // The kernel's own sender clones, which a live kernel holds for its
        // whole lifetime: a test uses them to fill a bounded lane and to
        // mint a second run's handle on the same lanes.
        data_tx: DataSender<u32>,
        control_tx: ControlSender<u32>,
    }

    impl Fixture {
        fn new(mode: GateMode, capacity: Option<NonZeroUsize>) -> Self {
            let (data_tx, data_rx) = channel::channel::<Envelope<u32>>(capacity);
            let (control_tx, control_rx) = control_lane::<u32>();
            let counter = Arc::new(PendingCounter::default());
            let gate = Arc::new(SendGate::new(mode));
            let intents = IntentRecorder::default();
            let acceptances = AcceptanceRecorder::default();
            let handle = IngressHandle::new(
                ALICE,
                Arc::clone(&counter),
                Arc::clone(&gate),
                data_tx.clone(),
                control_tx.clone(),
                intents.clone(),
                acceptances.clone(),
            );
            Self {
                handle,
                counter,
                gate,
                data_rx,
                control_rx,
                intents,
                acceptances,
                data_tx,
                control_tx,
            }
        }

        fn immediate() -> Self {
            Self::new(GateMode::Immediate, None)
        }

        fn scripted() -> Self {
            Self::new(GateMode::Scripted, None)
        }

        /// A second run's handle on the same lanes and the same gate.
        fn handle_for(&self, origin: RunToken) -> IngressHandle<u32> {
            IngressHandle::new(
                origin,
                Arc::new(PendingCounter::default()),
                Arc::clone(&self.gate),
                self.data_tx.clone(),
                self.control_tx.clone(),
                self.intents.clone(),
                self.acceptances.clone(),
            )
        }
    }

    fn one() -> NonZeroUsize {
        NonZeroUsize::new(1).expect("one is non-zero")
    }

    fn record(origin: RunToken, lane: Lane) -> GatedSend {
        GatedSend { origin, lane }
    }

    // Production's gate is transparent: the send completes with no grant,
    // and the whole pinned order runs anyway.
    #[tokio::test]
    async fn an_immediate_gate_admits_a_send_with_no_grant() {
        let mut fixture = Fixture::immediate();

        fixture.handle.send(7).await.expect("the lane is open");

        let envelope = fixture.data_rx.try_recv().expect("the envelope is queued");
        assert_eq!(envelope.origin, ALICE, "the envelope carries its origin");
        assert!(
            matches!(envelope.payload, Payload::Msg(7)),
            "the payload is the message"
        );
        assert_eq!(
            fixture.counter.value(),
            1,
            "a committed send stays pending until the dequeue side decrements"
        );
        assert_eq!(
            fixture.intents.snapshot(),
            vec![record(ALICE, Lane::Data)],
            "the intent is recorded before the gate"
        );
        assert_eq!(
            fixture.acceptances.snapshot(),
            vec![record(ALICE, Lane::Data)],
            "the acceptance is recorded with its lane"
        );
    }

    // Rule 0: the gate wait precedes the reservation, so a producer parked
    // on a grant holds no reservation — and an abort there leaves nothing to
    // release.
    #[tokio::test]
    async fn a_send_parked_on_the_gate_holds_no_reservation() {
        let fixture = Fixture::scripted();

        {
            let send = fixture.handle.send(7);
            tokio::pin!(send);
            assert!(
                poll!(send.as_mut()).is_pending(),
                "an ungranted send does not proceed"
            );
            assert_eq!(
                fixture.counter.value(),
                0,
                "a send parked at the gate has reserved nothing"
            );
            assert_eq!(
                fixture.intents.count_for(ALICE),
                1,
                "the intent was recorded before the gate wait"
            );
            assert!(
                fixture.acceptances.snapshot().is_empty(),
                "nothing is accepted before the gate releases it"
            );
        }

        assert_eq!(
            fixture.counter.value(),
            0,
            "aborting at the gate leaves no counter residue"
        );
        assert!(
            !fixture.gate.grant_outstanding(),
            "a send that never took a release resolves no grant"
        );
    }

    // The scripted seam in full: the grant releases exactly the named
    // origin's next send, and the commit is the terminal the driver reads.
    #[tokio::test]
    async fn a_grant_releases_its_origin_and_resolves_accepted() {
        let mut fixture = Fixture::scripted();

        let sequence = fixture
            .gate
            .issue_grant(ALICE)
            .expect("the gate holds no other grant");
        fixture.handle.send(7).await.expect("the lane is open");

        assert_eq!(
            fixture.gate.take_resolution(sequence),
            Some(Confirmed::Accepted),
            "a committed send resolves its grant as accepted"
        );
        assert!(
            !fixture.gate.grant_outstanding(),
            "a resolved grant is cleared"
        );
        assert_eq!(
            fixture.acceptances.snapshot(),
            vec![record(ALICE, Lane::Data)],
            "acceptance appends to the guaranteed sequence"
        );
        assert!(
            fixture.data_rx.try_recv().is_ok(),
            "the envelope reached the lane"
        );
    }

    // The rule is driver-wide: a second grant is refused whatever origin it
    // names, so `grant(a); grant(b)` is not expressible.
    #[tokio::test]
    async fn a_second_grant_is_refused_at_every_origin() {
        let fixture = Fixture::scripted();

        let sequence = fixture.gate.issue_grant(ALICE).expect("the first grant");
        assert_eq!(
            fixture.gate.issue_grant(ALICE),
            Err(GrantOutstanding),
            "a second grant at the same origin is refused"
        );
        assert_eq!(
            fixture.gate.issue_grant(BOB),
            Err(GrantOutstanding),
            "and so is one at another origin"
        );

        fixture.handle.send(7).await.expect("the lane is open");
        assert_eq!(
            fixture.gate.take_resolution(sequence),
            Some(Confirmed::Accepted),
            "the first grant resolves"
        );
        assert!(
            fixture.gate.issue_grant(BOB).is_ok(),
            "the next grant is admitted once the previous one resolved"
        );
    }

    // A grant names one origin: another origin's send stays parked while it
    // is outstanding.
    #[tokio::test]
    async fn a_grant_releases_no_other_origin() {
        let fixture = Fixture::scripted();
        let other = fixture.handle_for(BOB);

        fixture.gate.issue_grant(ALICE).expect("the grant");

        let send = other.send(9);
        tokio::pin!(send);
        assert!(
            poll!(send.as_mut()).is_pending(),
            "a grant at one origin releases no other"
        );
    }

    // The other terminal: a released send whose reservation is released
    // without committing. A capacity-blocked send dropped mid-wait is the
    // shape a revoked blocked sender takes.
    #[tokio::test]
    async fn a_released_send_reclaimed_before_commit_resolves_reclaimed() {
        let fixture = Fixture::new(GateMode::Scripted, Some(one()));
        fixture
            .data_tx
            .send(Envelope {
                origin: BOB,
                payload: Payload::Msg(1),
            })
            .await
            .expect("the lane is open");

        let sequence = fixture.gate.issue_grant(ALICE).expect("the grant");
        {
            let send = fixture.handle.send(7);
            tokio::pin!(send);
            assert!(
                poll!(send.as_mut()).is_pending(),
                "a full lane holds the released send"
            );
            assert_eq!(
                fixture.counter.value(),
                1,
                "the released send holds its reservation while it waits"
            );
            assert_eq!(
                fixture.gate.take_resolution(sequence),
                None,
                "an unresolved send reports no terminal and keeps its grant"
            );
        }

        assert_eq!(
            fixture.counter.value(),
            0,
            "the dropped send releases its reservation"
        );
        assert_eq!(
            fixture.gate.take_resolution(sequence),
            Some(Confirmed::Reclaimed),
            "the release is the grant's terminal"
        );
        assert!(
            fixture.acceptances.snapshot().is_empty(),
            "a reclaimed send appends nothing to the guaranteed sequence"
        );
    }

    // Termination's immediate postcondition drops the receivers; an
    // in-flight send then fails, and failing is a release.
    #[tokio::test]
    async fn a_send_to_a_closed_lane_releases_its_reservation() {
        let fixture = Fixture::immediate();
        drop(fixture.data_rx);

        assert_eq!(
            fixture.handle.send(7).await,
            Err(SendClosed),
            "a send with no receiver fails"
        );
        assert_eq!(
            fixture.counter.value(),
            0,
            "a failed send releases its reservation"
        );
        assert!(
            fixture.acceptances.snapshot().is_empty(),
            "a failed send is not an acceptance"
        );
    }

    // The gate covers both lanes: a producer-originated quit is released by
    // a grant exactly as a message is, and its record carries its lane.
    #[tokio::test]
    async fn a_quit_takes_the_control_lane_and_is_gated_too() {
        let mut fixture = Fixture::scripted();

        {
            let quit = fixture.handle.quit();
            tokio::pin!(quit);
            assert!(
                poll!(quit.as_mut()).is_pending(),
                "an ungranted quit does not proceed either"
            );
        }

        let sequence = fixture.gate.issue_grant(ALICE).expect("the grant");
        fixture.handle.quit().await.expect("the lane is open");

        let envelope = fixture
            .control_rx
            .try_recv()
            .expect("the quit is on the control lane");
        assert_eq!(envelope.origin, ALICE, "the quit carries its origin");
        assert!(
            matches!(envelope.payload, Payload::Quit),
            "the payload is a quit"
        );
        assert!(
            fixture.data_rx.try_recv().is_err(),
            "a quit never takes the data lane"
        );
        assert_eq!(
            fixture.gate.take_resolution(sequence),
            Some(Confirmed::Accepted),
            "a released quit resolves like any other send"
        );
        assert_eq!(
            fixture.acceptances.snapshot(),
            vec![record(ALICE, Lane::Control)],
            "the acceptance record carries the control lane"
        );
        assert_eq!(
            fixture.intents.count_for(ALICE),
            2,
            "both quit attempts recorded an intent, the abandoned one included"
        );
    }
}
