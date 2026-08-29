//! The fixtures every conformance series shares.
//!
//! Three rules shape everything here.
//!
//! - **Application-side observation only.** The driver reports no state, no
//!   frame, and no delivery transcript (RFC 0008 §9.11), so what a series
//!   asserts about `update`, `view`, and `subscriptions` is recorded by the
//!   program under test, in [`Journal`]. What a series asserts about a
//!   producer's own progress — a run reaching its end, a subscription
//!   source starting or stopping — is recorded by the effect or the source,
//!   in a [`Beacon`]. Both are the "test's own application-side
//!   instrumentation" a `settle` predicate is meant to read (RFC 0008 §9.6).
//! - **Bounded waiting, never timed.** Nothing here sleeps, arms a timer, or
//!   reads a wall clock. A wait is bounded either by a counted number of
//!   executor turns or scheduler yields, failing the test on its bound, or —
//!   where the waiting side is a drop on a worker thread, which can count no
//!   turns — by a release a script has no way to omit, because the type that
//!   drives it owns it ([`GatedDriver`]). What both forms rule out is the
//!   same pair: a wait that ends on a clock, and a wait that need not end at
//!   all. A blocking wait on a worker also has to announce itself with
//!   [`block_in_place`], or the executor it is blocking stops being able to
//!   turn.
//! - **One turn, one construction.** A turn is a spawn of a fresh no-op task
//!   onto the executor plus a join on it (RFC 0008 §9.6), which is what
//!   [`turn`] does and what [`TestDriver::settle`] does inside the driver.
//!
//! [`TestDriver::settle`]: crate::testing::driver::TestDriver::settle
//! [`block_in_place`]: tokio::task::block_in_place

use std::any::Any;
use std::collections::{HashMap, VecDeque};
use std::future::{Future, pending, ready};
use std::marker::PhantomData;
use std::mem;
use std::num::NonZeroUsize;
use std::ops::{Deref, DerefMut};
use std::panic::{self, AssertUnwindSafe};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::task::Poll;
use std::thread;
use std::thread::{Result as ThreadResult, ThreadId};

use futures::stream::{self, BoxStream, StreamExt};
use ratatui::backend::{Backend, TestBackend};
use ratatui::{Frame, Terminal};
use tokio::runtime::{Handle, RuntimeFlavor};
use tokio::sync::Notify;
use tokio::task::{self, yield_now};

use crate::command::effect_command::EffectCommand;
use crate::command::{Action, Command};
use crate::kernel::Kernel;
use crate::kernel::arbiter::WakeSource;
use crate::kernel::lane::GateMode;
use crate::reducer::{Exit, Program, Reducer};
use crate::runtime::config::RuntimeConfig;
use crate::runtime::load::LoadObserver;
use crate::subscription::mock::MockSource;
use crate::subscription::{Subscription, SubscriptionSource};
use crate::test_support::{FailingBackend, hook_guard};
use crate::testing::driver::{Confirmed, ParkProbe, RunName, StepReport, TestDriver};

/// The turn budget the series hand [`TestDriver::settle`] and
/// [`TestDriver::confirm`].
///
/// Both budgets are elements of a script (RFC 0008 §9.8), so they are named
/// at the call site rather than chosen by the driver. Every condition the
/// series wait on is one a couple of turns establish; the margin is there so
/// a failure reads as "the script never reaches this" rather than "the
/// budget was tight".
///
/// [`TestDriver::settle`]: crate::testing::driver::TestDriver::settle
/// [`TestDriver::confirm`]: crate::testing::driver::TestDriver::confirm
pub const TEST_TURNS: usize = 16;

/// The turn budget the multi-worker series hand [`TestDriver::confirm`].
///
/// Larger than [`TEST_TURNS`] and for a different reason. On the
/// current-thread executor a turn drains the ready queue, so a tight budget
/// catches a script that never releases what it granted. Beside worker
/// threads a turn is a rendezvous with another thread instead, and how many
/// of them pass before a worker picks a granted producer up is the
/// operating system's business — so the budget here is a hang guard whose
/// value is mechanism, and nothing these series claim rests on it. What
/// they claim rests on the handshake ([`MidBatchHandshake`]), which no
/// number of turns can reorder.
///
/// [`TestDriver::confirm`]: crate::testing::driver::TestDriver::confirm
pub const THREADED_TURNS: usize = 4096;

/// How many scheduler yields the reducer's half of a
/// [`MidBatchHandshake`] spends waiting for the gated producer's commit.
///
/// A hang guard whose value is mechanism, in the same style as
/// [`THREADED_TURNS`]: the ordering the handshake establishes does not
/// depend on how many yields it took to observe the signal, and the bound
/// only decides whether a producer that never commits fails the test or
/// stops it.
const HANDSHAKE_YIELDS: usize = 1_000_000;

/// How many turns a series hands the executor before asserting that
/// something has **not** happened.
///
/// Not a correctness condition anywhere it is used: *any* number of turns has
/// to leave a parked loop's waker unsignalled, and *any* number has to leave a
/// held quiescence unfinished. This is the number the series spend
/// establishing that, and it is what separates such an assertion from a race
/// with whatever would have made it false — read too early, "it has not
/// happened yet" and "it does not happen" are the same reading.
pub const NEGATIVE_TURNS: usize = 8;

/// An application-side counter a producer, a source, or a drop glue marks.
///
/// This is the only thing a `settle` predicate reads, and the only thing a
/// park witness uses to detect an arrival: the driver's own view of a run's
/// exit reaches a test at a pass's first stage and nowhere else, and a
/// `ParkProbe` series has no driver at all.
#[derive(Clone, Debug, Default)]
pub struct Beacon(Arc<AtomicUsize>);

impl Beacon {
    /// Records one occurrence.
    pub fn mark(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }

    /// How many have been recorded.
    pub fn marks(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }

    /// Whether any has.
    pub fn marked(&self) -> bool {
        self.marks() > 0
    }
}

/// Marks its beacon when dropped — the shape a cleanup finalizer, a
/// reclaimed task body, and a stopped subscription source all share.
#[derive(Debug)]
pub struct DropMark(Beacon);

impl DropMark {
    /// A guard that will mark `beacon` when it is dropped.
    pub const fn new(beacon: Beacon) -> Self {
        Self(beacon)
    }
}

impl Drop for DropMark {
    fn drop(&mut self) {
        self.0.mark();
    }
}

/// A gate a stopped subscription run's **quiescence** holds at, so a script
/// can keep a run stop-requested and not-yet-quiesced across a whole pass.
///
/// Why this exists: a stop's abort resolves within one executor turn on the
/// current-thread executor, so there the only pass in which a stopped run is
/// still unquiesced is the pass that issued the stop — the window RFC 0012
/// INV-SE4's mandated sequence needs is not constructible. Holding the
/// *dismantling* itself is what opens that window, and it is only safe to
/// hold beside worker threads: the abort drops the run's stream on a worker,
/// so blocking there costs the driving thread nothing and the driver stays
/// steppable throughout.
///
/// Like [`MidBatchHandshake`], the ordering this establishes is the
/// application's own, not the scheduler's, and a series using it cites no
/// part of INV-RC14's determinism claim (RFC 0008 §9.8's verified range is
/// current-thread).
#[derive(Clone, Debug)]
pub struct QuiescenceGate {
    open: Arc<(Mutex<bool>, Condvar)>,
    entered: Beacon,
    /// The thread that will release this gate: the one it was constructed
    /// on, which [`threaded_driver_with`] asserts is also the one building
    /// the driver, and [`GatedDriver`]'s `!Send` keeps as the one dropping
    /// it. See [`QuiescenceGate::wait`].
    releaser: ThreadId,
}

impl Default for QuiescenceGate {
    fn default() -> Self {
        Self {
            open: Arc::default(),
            entered: Beacon::default(),
            releaser: thread::current().id(),
        }
    }
}

impl QuiescenceGate {
    /// Releases the held run, which then quiesces.
    ///
    /// Idempotent, and it *stores* the release rather than signalling it: a
    /// hold that has not begun yet reads the flag and never waits at all, so
    /// no ordering between this and the dismantling is load-bearing.
    pub fn open(&self) {
        let (open, released) = &*self.open;
        *open.lock().unwrap_or_else(PoisonError::into_inner) = true;
        released.notify_all();
    }

    /// Whether the held run's dismantling has begun — the abort has landed
    /// on a worker and the run is now stop-requested with its quiescence
    /// pending, which is the state a script waits for before opening the
    /// window's second half.
    pub fn entered(&self) -> bool {
        self.entered.marked()
    }

    /// Hold side: block until the gate is open, unless blocking here would
    /// be blocking the only thread that can open it.
    ///
    /// That first clause is the whole of what keeps an unbounded wait from
    /// being an unbounded risk. The hold is correct because the abort drops
    /// the run's stream on a *worker*, never on the script's own thread — a
    /// premise this fixture does not control, since it is the kernel that
    /// decides where a cancelled task's future is dropped. Were that to
    /// change, a hold on the script's thread would block the release it is
    /// waiting for, and no arm of the caller's `block_in_place` check would
    /// notice: a multi-thread runtime's `block_on` thread reports
    /// `MultiThread` and runs the closure inline.
    ///
    /// So the premise is checked rather than assumed. Reached on the
    /// releasing thread, the hold declines to wait, the run quiesces at once,
    /// and the window's own assertion — "A is stop-requested and has not
    /// quiesced, which is the state the sequence needs" — fails in the file
    /// that states it. A named failure, where the alternative is a job that
    /// times out with no test named at all.
    ///
    /// The trailing `drop` buys no contention back — it is the last statement
    /// in the function, so the guard would fall at the same point without it.
    /// It is there because `clippy::significant_drop_tightening` asks for it
    /// and the lint job runs with `-D warnings`; removing it fails that job
    /// rather than changing when anything is unlocked.
    fn wait(&self) {
        if thread::current().id() == self.releaser {
            return;
        }
        let (open, released) = &*self.open;
        let mut open = open.lock().unwrap_or_else(PoisonError::into_inner);
        while !*open {
            open = released.wait(open).unwrap_or_else(PoisonError::into_inner);
        }
        drop(open);
    }
}

/// The guard a probe source's stream holds, whose drop is that run's
/// quiescence as the application sees it.
///
/// Two shapes because two windows: the ordinary one reports the quiescence
/// and returns, and the gated one holds the dismantling open until the
/// script releases it.
#[derive(Debug)]
enum Quiescence {
    Immediate(DropMark),
    Gated(GatedQuiescence),
}

/// The guard whose drop is a gated run's quiescence.
///
/// The two marks are made either side of the hold, and that is the whole
/// point: `entered` says the dismantling started, `quiesced` says it
/// finished, and between them the run is in the state the window needs.
#[derive(Debug)]
struct GatedQuiescence {
    gate: QuiescenceGate,
    quiesced: Beacon,
}

impl Drop for GatedQuiescence {
    /// Held with a blocking wait rather than a waker or a deadline: a drop
    /// cannot await, and a deadline would be a clock read.
    ///
    /// What ends the wait is structural rather than counted, and the script
    /// is not asked to remember any of it: the driver that runs the script
    /// holds the gates the script declared and opens them in its own
    /// [`Drop`], which runs before the field holding the executor does
    /// ([`GatedDriver`]). So the gate is opened on every path out — the
    /// ordinary one at the end of the window, and an unwinding one, where the
    /// release still lands before the shutdown that would join this thread.
    ///
    /// A counted bound was the alternative and it is the wrong shape here:
    /// the count would be spent concurrently with the script's own progress
    /// through the window, so the two race, and the hold releasing early
    /// fails a premise the kernel had nothing to do with (issue #300).
    ///
    /// Blocking a worker thread is what makes the window constructible at all:
    /// the abort drops the run's stream on a worker, so the driving thread
    /// stays steppable throughout. But blocking one *without telling the
    /// scheduler* does not leave it steppable, and a second worker does not
    /// save it. A worker blocked inside a task is indistinguishable from a
    /// worker running a long one, so the scheduler counts it as active and
    /// need not wake its parked neighbour for the task
    /// [`TestDriver::turn`] injects — which strands that task, and with it the
    /// `settle` whose turn budget was supposed to be this test's bound.
    /// Reproduced at 2 hangs in 800 runs at 16x parallelism, with the driving
    /// thread parked in `block_on`, one worker here, and the other parked in
    /// the IO driver.
    ///
    /// [`block_in_place`] is the announcement that makes the hold safe: it
    /// hands this worker's core to another thread for the duration, so the
    /// executor keeps turning while this thread sits in the gate. It is only
    /// valid on a multi-thread runtime's worker, and this drop also runs off
    /// one — during a shutdown that tears the run down from elsewhere — so
    /// the flavour is checked rather than assumed.
    ///
    /// The two cases that must *not* announce are named, and everything else
    /// announces: [`RuntimeFlavor`] is `#[non_exhaustive]` and the dependency
    /// is a caret requirement, so a flavour added later arrives here without
    /// this file changing. Sent down the plain-wait path it would block a
    /// worker without telling anyone, which is the hang above, silent and
    /// unnamed; sent down this one, the worst it can do is panic where it is
    /// misused, in a drop that is not unwinding, which says what happened.
    /// The hazard `block_in_place` still has and this does not cover is a
    /// `LocalSet` on a multi-thread runtime, where it panics too — there is
    /// no `LocalSet` anywhere in this crate, and a check for one is not
    /// something the runtime exposes.
    ///
    /// [`block_in_place`]: tokio::task::block_in_place
    /// [`TestDriver::turn`]: crate::testing::driver::TestDriver
    fn drop(&mut self) {
        self.gate.entered.mark();
        match Handle::try_current().map(|handle| handle.runtime_flavor()) {
            Err(_) | Ok(RuntimeFlavor::CurrentThread) => self.gate.wait(),
            Ok(RuntimeFlavor::MultiThread | _) => task::block_in_place(|| self.gate.wait()),
        }
        self.quiesced.mark();
    }
}

/// A permit-storing release signal for producer bodies.
///
/// [`open`](Latch::open) banks the permit whether or not the body has
/// reached its wait, so no scheduling order is load-bearing.
#[derive(Clone, Debug, Default)]
pub struct Latch(Arc<Notify>);

impl Latch {
    /// Releases the producer holding at this latch.
    pub fn open(&self) {
        self.0.notify_one();
    }

    /// Producer side: holds until the test opens the latch.
    pub async fn wait(&self) {
        self.0.notified().await;
    }
}

/// One call the kernel made into the program under test.
///
/// These four are the whole application surface a pass touches, so this is
/// what pins delivery, rendering, and re-evaluation without any probe inside
/// the kernel — and it is the *only* render observation there is, the driver
/// owning its terminal and reporting nothing about frames (RFC 0008 §9.11).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Call {
    /// `init` ran.
    Init,
    /// `update` ran for this message.
    Reduce(u8),
    /// `view` ran — one render.
    View,
    /// `subscriptions` ran — one re-evaluation.
    Subscriptions,
    /// The mid-batch handshake observed the gated producer's commit, from
    /// inside the `update` that opened it (see [`MidBatchHandshake`]).
    ///
    /// Recorded on the driving thread by the reducer, between the `Reduce`
    /// that opened the gate and the next one, which is what makes "the
    /// arrival landed inside the batch" a literal reading of the journal
    /// rather than an inference.
    Committed,
}

/// The application's own record of what the kernel asked it to do.
///
/// The renders are logged twice over: once as a [`Call::View`] in the call
/// sequence, which is what orders a render against the batch and the
/// re-evaluation around it, and once as the state that render observed —
/// the last message `update` had been invoked with when `view` ran. The
/// second log is what makes "the render observed the pass's current state"
/// (RFC 0011 INV-LC2) readable, and it is deliberately not a `Call`
/// variant: folding the state into the sequence would make every ordering
/// assertion in the suite state-sensitive.
#[derive(Clone, Debug, Default)]
pub struct Journal {
    calls: Arc<Mutex<Vec<Call>>>,
    renders: Arc<Mutex<Vec<Option<u8>>>>,
}

impl Journal {
    /// Appends one call.
    fn record(&self, call: Call) {
        self.calls_mut().push(call);
    }

    /// Appends the state one render observed: the last message delivered
    /// before it, or `None` where no message had been.
    fn render(&self, observed: Option<u8>) {
        self.renders_mut().push(observed);
    }

    /// What each render observed, in render order.
    pub fn rendered(&self) -> Vec<Option<u8>> {
        self.renders_mut().clone()
    }

    /// Every call, in order.
    pub fn calls(&self) -> Vec<Call> {
        self.calls_mut().clone()
    }

    /// The messages `update` was invoked with, in order.
    pub fn reduced(&self) -> Vec<u8> {
        self.calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Reduce(message) => Some(message),
                Call::Init | Call::View | Call::Subscriptions | Call::Committed => None,
            })
            .collect()
    }

    /// How many renders `view` has performed.
    pub fn views(&self) -> usize {
        self.count(&Call::View)
    }

    /// How many re-evaluations `subscriptions` has performed.
    pub fn evaluations(&self) -> usize {
        self.count(&Call::Subscriptions)
    }

    /// How many calls of one shape were made.
    fn count(&self, wanted: &Call) -> usize {
        self.calls().iter().filter(|call| *call == wanted).count()
    }

    /// The lock, recovered rather than propagated: a `view` or an `update`
    /// that panics mid-record would otherwise turn every later read into a
    /// poison error in place of the test's own assertion, and the data is an
    /// append-only list with no cross-record invariant.
    fn calls_mut(&self) -> MutexGuard<'_, Vec<Call>> {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The render log's lock, recovered for the same reason.
    fn renders_mut(&self) -> MutexGuard<'_, Vec<Option<u8>>> {
        self.renders.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A subscription source that reports its own admission and quiescence.
///
/// Both facts are application-side by construction: the admission mark is
/// made by the spawner the kernel invokes at the admission site, and the
/// quiescence mark by the drop glue of the stream that spawner returned. A
/// series therefore reads "this source started" and "this source's run is
/// gone" without a driver accessor for either.
#[derive(Clone, Debug)]
pub struct ProbeSource {
    key: &'static str,
    values: Vec<u8>,
    ends: bool,
    fault: Option<Fault>,
    /// Where this source's quiescence is held, for the one series that needs
    /// a stopped run to stay unquiesced across a pass.
    gate: Option<QuiescenceGate>,
    admissions: Beacon,
    quiescences: Beacon,
}

/// Where a faulty [`ProbeSource`] panics — the two sites RFC 0011 keeps
/// apart, because they land in different panic classes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Fault {
    /// In the spawner, which the kernel invokes at the admission site on the
    /// **driving task** — so the unwind is fail-fast (INV-LC6).
    Spawner,
    /// In the stream, polled inside the **runtime-owned** forwarder task —
    /// so the panic is contained (INV-LC8).
    Stream,
}

impl ProbeSource {
    /// A source that emits nothing and never ends, so its run lives until
    /// something stops it.
    pub fn silent(key: &'static str) -> Self {
        Self {
            key,
            values: Vec::new(),
            ends: false,
            fault: None,
            gate: None,
            admissions: Beacon::default(),
            quiescences: Beacon::default(),
        }
    }

    /// A silent source whose run's **quiescence** holds at `gate` once
    /// something stops it, so a script can drive a whole pass while the stop
    /// is outstanding (RFC 0012 INV-SE4's mandated window).
    pub fn gated(key: &'static str, gate: QuiescenceGate) -> Self {
        Self {
            gate: Some(gate),
            ..Self::silent(key)
        }
    }

    /// A source that emits `values` and then parks, so its output can be
    /// scripted without its run ending.
    pub fn sending(key: &'static str, values: impl IntoIterator<Item = u8>) -> Self {
        Self {
            values: values.into_iter().collect(),
            ..Self::silent(key)
        }
    }

    /// A source that ends on its own without emitting — a natural finish.
    pub fn finishing(key: &'static str) -> Self {
        Self {
            ends: true,
            ..Self::silent(key)
        }
    }

    /// A source whose lazy constructor panics, at the reconcile that admits
    /// it — application code on the driving task, so fail-fast
    /// (RFC 0011 INV-LC6).
    pub fn unbuildable(key: &'static str) -> Self {
        Self {
            fault: Some(Fault::Spawner),
            ..Self::silent(key)
        }
    }

    /// A source that builds and then panics while the forwarder task polls
    /// it — inside a runtime-owned task, so contained
    /// (RFC 0011 INV-LC8).
    pub fn exploding(key: &'static str) -> Self {
        Self {
            fault: Some(Fault::Stream),
            ..Self::silent(key)
        }
    }

    /// How many times this source has been admitted (its spawner invoked).
    pub fn admissions(&self) -> usize {
        self.admissions.marks()
    }

    /// How many times a run of this source has quiesced (its stream
    /// dropped, or ended).
    pub fn quiescences(&self) -> usize {
        self.quiescences.marks()
    }
}

impl SubscriptionSource for ProbeSource {
    type Output = u8;
    type Key = &'static str;

    /// The spawner. Admission is marked *before* either fault fires, because
    /// the count is of spawner invocations (RFC 0012 INV-SE1) and the kernel
    /// did invoke it — an unbuildable source's admission is a real
    /// admission that then unwound.
    #[expect(
        clippy::panic,
        reason = "the two panic classes under test are real panics, at the two sites RFC 0011 \
                  keeps apart"
    )]
    fn stream(&self) -> BoxStream<'static, u8> {
        self.admissions.mark();
        assert!(
            self.fault != Some(Fault::Spawner),
            "subscription source constructor panic under the fail-fast class"
        );
        if self.fault == Some(Fault::Stream) {
            let guard = DropMark::new(self.quiescences.clone());
            return Box::pin(stream::once(async move {
                let _quiesced = guard;
                panic!("subscription forwarder panic under the containment class")
            }));
        }
        let guard = self.gate.clone().map_or_else(
            || Quiescence::Immediate(DropMark::new(self.quiescences.clone())),
            |gate| {
                Quiescence::Gated(GatedQuiescence {
                    gate,
                    quiesced: self.quiescences.clone(),
                })
            },
        );
        let state = (self.values.clone().into_iter(), guard, self.ends);
        Box::pin(stream::unfold(
            state,
            |(mut values, guard, ends)| async move {
                if let Some(value) = values.next() {
                    return Some((value, (values, guard, ends)));
                }
                if ends {
                    return None;
                }
                pending().await
            },
        ))
    }

    fn key(&self) -> &'static str {
        self.key
    }
}

/// The mid-batch commit handshake: it lets a producer's send commit
/// **during** an in-progress input batch, deterministically.
///
/// A pass is a synchronous region — RFC 0014 §3.5's four stages run without
/// the driving task yielding — so the only way a producer commits inside one
/// is for the producer to run on a thread the pass is not occupying. That is
/// what [`threaded_driver_with`] supplies, and it is the whole of what the
/// worker threads are for: **the determinism is this handshake's, never the
/// scheduler's.**
///
/// The two halves rendezvous like this. The reducer, running on the driving
/// thread inside the batch, calls [`open_and_await_commit`] — it opens the
/// gate and then blocks until the producer reports its commit. The producer,
/// running on a worker thread, busy-yields on the gate flag, and once it
/// opens performs its real send and signals. No waker is involved on the
/// producer's side, so no scheduling order is load-bearing; neither side can
/// pass the other.
///
/// The reducer's wait is bounded without a clock, by a count: it hands the OS
/// scheduler a counted number of yields and polls for the signal between
/// them, failing on the bound rather than blocking forever. A blocking `recv`
/// would have been simpler and is what the shape suggests, but a producer
/// that never commits would then hang the suite instead of failing it, and a
/// deadline is the only way to bound *that* block — a clock read, which
/// RFC 0014 §6.3 and this crate's disallowed-method list both rule out.
///
/// Which is why this wait is counted and [`QuiescenceGate`]'s is not, though
/// both are hang guards over a rendezvous. There the party that must release
/// the hold is the script itself, so the release can be made structural and
/// unforgettable — [`GatedDriver`] owns it and drops it before its executor.
/// Here it is the *producer* that must signal, from inside a run the script
/// does not own and cannot finish on its behalf: a body that parks before its
/// send holds its sender alive, so even `Disconnected` never arrives. Nothing
/// the script can hold releases this one, so a count is what is left.
///
/// [`open_and_await_commit`]: MidBatchHandshake::open_and_await_commit
pub struct MidBatchHandshake {
    open: Arc<AtomicBool>,
    committed: Receiver<()>,
    reclaimed: Beacon,
}

/// The producer half of a [`MidBatchHandshake`], handed to the effect that
/// commits inside the batch.
pub struct MidBatchGate {
    open: Arc<AtomicBool>,
    committed: Sender<()>,
    reclaimed: Beacon,
}

impl MidBatchHandshake {
    /// A fresh handshake and the gate its producer holds.
    pub fn new() -> (Self, MidBatchGate) {
        let open = Arc::new(AtomicBool::new(false));
        let (committed, received) = mpsc::channel();
        let reclaimed = Beacon::default();
        (
            Self {
                open: Arc::clone(&open),
                committed: received,
                reclaimed: reclaimed.clone(),
            },
            MidBatchGate {
                open,
                committed,
                reclaimed,
            },
        )
    }

    /// The beacon the gated producer's body marks when it is dropped.
    ///
    /// The application-side view of that run's quiescence, for a `settle`
    /// predicate: the driver's own view of an exit reaches a test only at a
    /// pass's first stage.
    pub fn reclaimed(&self) -> Beacon {
        self.reclaimed.clone()
    }

    /// Reducer side: open the producer's gate, then wait for its commit.
    ///
    /// Returns only once the producer's send has returned, so everything the
    /// reducer does after this — including the command it returns — is
    /// strictly after the commit, and everything the batch does before it is
    /// strictly before.
    ///
    /// The wait is a counted number of scheduler yields with a poll for the
    /// signal between them, so it ends either at the commit or at its bound,
    /// and never on a clock. The bound's value is mechanism, like every
    /// other hang guard here: the *ordering* this establishes does not
    /// depend on how many yields it took to observe.
    ///
    /// # Panics
    ///
    /// Panics when the bound is spent with no signal, which means the gated
    /// producer never reached its send.
    #[expect(
        clippy::panic,
        reason = "an exhausted bound fails the test, which is what a panic is here"
    )]
    pub fn open_and_await_commit(&self) {
        self.open.store(true, Ordering::SeqCst);
        for _ in 0..HANDSHAKE_YIELDS {
            match self.committed.try_recv() {
                Ok(()) => return,
                Err(TryRecvError::Empty) => thread::yield_now(),
                Err(TryRecvError::Disconnected) => {
                    panic!("the gated producer's run ended before it reached its send")
                }
            }
        }
        panic!(
            "the mid-batch handshake was not answered in {HANDSHAKE_YIELDS} scheduler yields: \
             the gated producer never committed while the batch was running"
        );
    }
}

/// A named, state-gated subscription declaration.
///
/// The scripted program declares a feed exactly while its name is in the
/// state's wanted set, which is how a series scripts a removal, a
/// replacement, and a re-declaration without writing a reducer of its own.
pub struct Feed {
    name: &'static str,
    source: ProbeSource,
    scope: Option<&'static str>,
}

impl Feed {
    /// A feed declaring `source` under its own key as its name.
    pub fn new(source: ProbeSource) -> Self {
        Self {
            name: source.key,
            source,
            scope: None,
        }
    }

    /// The same feed, declared under a composition boundary — so a prefix
    /// teardown selects its run beside the command runs declared there
    /// (RFC 0014 §4.1).
    pub const fn under(mut self, scope: &'static str) -> Self {
        self.scope = Some(scope);
        self
    }

    /// This feed's declaration, as the program returns it.
    fn declare(&self) -> Subscription<u8> {
        let subscription = Subscription::new(self.source.clone());
        match self.scope {
            Some(scope) => subscription.scoped(scope),
            None => subscription,
        }
    }
}

/// Which of the two `subscriptions` call sites a script panics at
/// (RFC 0011 INV-LC6 keeps them apart: one is reached from `boot`, the other
/// from a pass's frame stage).
///
/// Stated over the *state* rather than over a call count, so the declaration
/// function stays pure — the same state gives the same outcome, which is
/// what RFC 0012 INV-SE6 requires of it even in a fixture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeclarationPanic {
    /// The bootstrap reconcile, before any message has been delivered.
    Bootstrap,
    /// The steady re-evaluation of a state this message was delivered into.
    After(u8),
}

impl DeclarationPanic {
    /// Whether this site is the one being called, given the last message
    /// `update` was invoked with.
    const fn fires(self, last: Option<u8>) -> bool {
        match (self, last) {
            (Self::Bootstrap, None) => true,
            (Self::After(message), Some(delivered)) => message == delivered,
            (Self::Bootstrap, Some(_)) | (Self::After(_), None) => false,
        }
    }
}

/// What the scripted program is told to do, handed over at `init`.
pub struct Script {
    /// The command `init` returns unchanged.
    init: Command<u8>,
    /// The commands `reduce` returns, in order; exhausted, it returns
    /// [`Command::none`].
    replies: VecDeque<Command<u8>>,
    /// Sources declared unconditionally, for series that need a running
    /// subscription and nothing more.
    mocks: Vec<MockSource<u8>>,
    /// Named declarations, gated on the wanted set.
    feeds: Vec<Feed>,
    /// The names declared before any message arrives.
    wanted: Vec<&'static str>,
    /// The wanted set each message installs, applied before that message's
    /// reply is taken.
    redeclare: HashMap<u8, Vec<&'static str>>,
    /// The message whose `update` panics — the driving-task panic class.
    panic_on: Option<u8>,
    /// Whether `view` panics — the same class, at the render call site.
    panic_in_view: bool,
    /// Which `subscriptions` call site panics, if either.
    panic_in_subscriptions: Option<DeclarationPanic>,
    /// The message whose `update` runs the mid-batch handshake, and the
    /// handshake it runs.
    handshake: Option<(u8, MidBatchHandshake)>,
}

impl Script {
    /// A script whose `init` returns `init` and whose `reduce` returns
    /// nothing.
    pub fn new(init: impl Into<Command<u8>>) -> Self {
        Self {
            init: init.into(),
            replies: VecDeque::new(),
            mocks: Vec::new(),
            feeds: Vec::new(),
            wanted: Vec::new(),
            redeclare: HashMap::new(),
            panic_on: None,
            panic_in_view: false,
            panic_in_subscriptions: None,
            handshake: None,
        }
    }

    /// Rejects this script at a current-thread constructor.
    ///
    /// There the abort drops a run's stream on the driving thread, which is
    /// the gate's own releaser, so the hold declines and the window it exists
    /// to open is never open — a row claiming a state it never reached, and
    /// passing. Every constructor that builds a current-thread executor has
    /// to say this, so it lives here rather than at one of them: the strength
    /// of the rule is whichever sibling forgets it.
    fn assert_ungated(&self) {
        assert!(
            self.gates().is_empty(),
            "a gated source needs `threaded_driver_with`: on the current-thread executor its \
             hold runs on the driving thread and declines, so the window it exists to open is \
             never open"
        );
    }

    /// Every [`QuiescenceGate`] a declared source of this script holds.
    ///
    /// What [`threaded_driver_with`] releases, taken from the script itself
    /// so that it is the gate the source will actually hold at rather than
    /// one named again alongside it.
    fn gates(&self) -> Vec<QuiescenceGate> {
        self.feeds
            .iter()
            .filter_map(|feed| feed.source.gate.clone())
            .collect()
    }

    /// The commands `reduce` returns, one per delivered message in order.
    #[must_use]
    pub fn replying(mut self, replies: impl IntoIterator<Item = impl Into<Command<u8>>>) -> Self {
        self.replies = replies.into_iter().map(Into::into).collect();
        self
    }

    /// Sources the program declares unconditionally.
    #[must_use]
    pub fn declaring(mut self, mocks: Vec<MockSource<u8>>) -> Self {
        self.mocks = mocks;
        self
    }

    /// Named feeds, all of them declared to begin with.
    #[must_use]
    pub fn feeding(mut self, feeds: impl IntoIterator<Item = Feed>) -> Self {
        self.feeds = feeds.into_iter().collect();
        self.wanted = self.feeds.iter().map(|feed| feed.name).collect();
        self
    }

    /// The subset of the feeds declared before any message arrives.
    #[must_use]
    pub fn wanting(mut self, wanted: impl IntoIterator<Item = &'static str>) -> Self {
        self.wanted = wanted.into_iter().collect();
        self
    }

    /// The wanted set `message` installs when it is delivered.
    #[must_use]
    pub fn redeclaring(
        mut self,
        message: u8,
        wanted: impl IntoIterator<Item = &'static str>,
    ) -> Self {
        self.redeclare.insert(message, wanted.into_iter().collect());
        self
    }

    /// Makes `update` panic when `message` is delivered — an application
    /// panic on the driving task, which is fail-fast rather than contained
    /// (RFC 0011 INV-LC8).
    #[must_use]
    pub const fn panicking_on(mut self, message: u8) -> Self {
        self.panic_on = Some(message);
        self
    }

    /// Makes `view` panic — the same fail-fast class at the render call
    /// site (RFC 0011 INV-LC6).
    #[must_use]
    pub const fn panicking_in_view(mut self) -> Self {
        self.panic_in_view = true;
        self
    }

    /// Makes `subscriptions` panic at the bootstrap call site: the initial
    /// reconcile, before any message has been delivered.
    #[must_use]
    pub const fn panicking_in_subscriptions_at_bootstrap(mut self) -> Self {
        self.panic_in_subscriptions = Some(DeclarationPanic::Bootstrap);
        self
    }

    /// Makes `subscriptions` panic at the steady call site: the
    /// re-evaluation of a state `message` has been delivered into.
    #[must_use]
    pub const fn panicking_in_subscriptions_after(mut self, message: u8) -> Self {
        self.panic_in_subscriptions = Some(DeclarationPanic::After(message));
        self
    }

    /// Runs `handshake` from the `update` for `message`, so a gated
    /// producer's send commits between that message and the batch's next
    /// one.
    #[must_use]
    pub fn handshaking_on(mut self, message: u8, handshake: MidBatchHandshake) -> Self {
        self.handshake = Some((message, handshake));
        self
    }
}

/// The scripted program's state: the script, minus what `init` consumed.
pub struct State {
    replies: VecDeque<Command<u8>>,
    mocks: Vec<MockSource<u8>>,
    feeds: Vec<Feed>,
    wanted: Vec<&'static str>,
    redeclare: HashMap<u8, Vec<&'static str>>,
    panic_on: Option<u8>,
    panic_in_view: bool,
    panic_in_subscriptions: Option<DeclarationPanic>,
    handshake: Option<(u8, MidBatchHandshake)>,
    /// The last message `update` was invoked with — the state a render and a
    /// re-evaluation observe, which is what makes "this pass's current
    /// state" readable from the application side (RFC 0011 INV-LC2).
    last: Option<u8>,
}

/// A program that replies from its script and records every call the kernel
/// makes into it.
pub struct Scripted {
    journal: Journal,
}

impl Reducer for Scripted {
    type State = State;
    type Message = u8;

    fn reduce(&self, state: &mut State, message: u8) -> Command<u8> {
        self.journal.record(Call::Reduce(message));
        assert!(
            state.panic_on != Some(message),
            "application panic under the fail-fast class"
        );
        state.last = Some(message);
        if let Some(wanted) = state.redeclare.get(&message) {
            state.wanted.clone_from(wanted);
        }
        // Before the reply is taken, so the command this `update` returns —
        // a teardown, say — applies strictly after the gated commit.
        if let Some((gated, handshake)) = state.handshake.as_ref()
            && *gated == message
        {
            handshake.open_and_await_commit();
            self.journal.record(Call::Committed);
        }
        state.replies.pop_front().unwrap_or_else(Command::none)
    }

    fn subscriptions(&self, state: &State) -> Vec<Subscription<u8>> {
        self.journal.record(Call::Subscriptions);
        assert!(
            !state
                .panic_in_subscriptions
                .is_some_and(|site| site.fires(state.last)),
            "application panic in `subscriptions` under the fail-fast class"
        );
        state
            .mocks
            .iter()
            .map(|mock| Subscription::new(mock.clone()))
            .chain(
                state
                    .feeds
                    .iter()
                    .filter(|feed| state.wanted.contains(&feed.name))
                    .map(Feed::declare),
            )
            .collect()
    }
}

impl Program for Scripted {
    type Flags = Script;

    fn init(&self, flags: Script) -> (State, Command<u8>) {
        self.journal.record(Call::Init);
        let Script {
            init,
            replies,
            mocks,
            feeds,
            wanted,
            redeclare,
            panic_on,
            panic_in_view,
            panic_in_subscriptions,
            handshake,
        } = flags;
        (
            State {
                replies,
                mocks,
                feeds,
                wanted,
                redeclare,
                panic_on,
                panic_in_view,
                panic_in_subscriptions,
                handshake,
                last: None,
            },
            init,
        )
    }

    fn view(&self, state: &State, _frame: &mut Frame<'_>) {
        self.journal.record(Call::View);
        self.journal.render(state.last);
        assert!(
            !state.panic_in_view,
            "application panic in `view` under the fail-fast class"
        );
    }
}

/// The default configuration the series construct through.
pub fn config() -> RuntimeConfig {
    RuntimeConfig::new()
}

/// A non-zero count, for the two configured bounds.
pub fn cap(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).expect("non-zero")
}

/// A real terminal over the test backend, so `Program::view` runs exactly as
/// it does in production.
pub fn terminal() -> Terminal<TestBackend> {
    Terminal::new(TestBackend::new(8, 2)).expect("the test backend never fails")
}

/// A driver over the scripted program, plus the journal it records into.
pub fn driver(script: Script) -> (TestDriver<Scripted, TestBackend>, Journal) {
    driver_with(script, config())
}

/// The same, under a chosen configuration.
///
/// Rejects a script with a gated source. Holding a dismantling is only
/// constructible beside worker threads — here the abort drops the run's
/// stream on the driving thread, which is the gate's own releaser, so the
/// hold declines and the window silently never opens. That is a row claiming
/// to have tested a state it never reached, so it is a failure at the
/// constructor instead.
pub fn driver_with(
    script: Script,
    config: RuntimeConfig,
) -> (TestDriver<Scripted, TestBackend>, Journal) {
    script.assert_ungated();
    let journal = Journal::default();
    let program = Scripted {
        journal: journal.clone(),
    };
    (
        TestDriver::new(program, script, config, terminal()),
        journal,
    )
}

/// A driver on a multi-worker executor, for the series whose producer has
/// to commit while a pass is running.
///
/// Two workers beside the driving thread, which is what the mid-batch
/// handshake needs: the driving thread blocks inside `update` while the
/// gated producer runs. The executor is the only difference — same
/// production construction path, same two seams, same pass-unit stepping —
/// and the determinism of these series is the handshake's, not the
/// scheduler's, so they cite no part of INV-RC14's scripted-determinism
/// claim (RFC 0008 §9.8's verified range is current-thread).
pub fn threaded_driver_with(script: Script, config: RuntimeConfig) -> (GatedDriver, Journal) {
    let journal = Journal::default();
    let program = Scripted {
        journal: journal.clone(),
    };
    // Read off the script rather than passed in beside it, which is the only
    // way the gate this releases is *the* gate the script's source holds. A
    // separate argument can be the wrong one, or absent, and both compile.
    let gates = script.gates();
    // `wait`'s premise check guards the thread a gate names as its releaser,
    // and a gate names the thread it was constructed on. That is only the
    // thread that will actually release it while the two are the same thread,
    // which they are here and at every call site — so it is asserted rather
    // than assumed. Built somewhere else, the check would guard a thread that
    // releases nothing while the real driving thread blocked on the gate, and
    // the failure would be the hang this whole arrangement exists to remove.
    // `GatedDriver` is not `Send`, so this thread is also the one that drops
    // it, which is what closes the gap between the two.
    for gate in &gates {
        assert_eq!(
            thread::current().id(),
            gate.releaser,
            "a gated script has to be driven from the thread its gate was constructed on: that \
             thread is the one whose hold declines to wait, and the one this driver's drop \
             releases from"
        );
    }
    (
        GatedDriver {
            gates,
            driver: TestDriver::on_worker_threads(program, script, config, terminal(), cap(2)),
            not_send: PhantomData,
        },
        journal,
    )
}

/// A [`TestDriver`] that opens its script's [`QuiescenceGate`]s before it
/// shuts its executor down.
///
/// This is the only multi-worker driver there is, and that is the point.
/// A held quiescence with nothing to release it is a hang rather than a
/// failure, so nothing about the release is left for a series to remember:
/// there is no second constructor to reach for, no guard to bind, no
/// position to get right, and no gate argument that can disagree with the
/// one [`ProbeSource::gated`] handed the script. A script with no gated
/// source carries no gates and this costs it an empty vector.
///
/// It is a plain deref to the driver otherwise: the release is all this
/// adds.
///
/// Deliberately not [`Send`]. A gate's premise check names the thread it was
/// constructed on, [`threaded_driver_with`] asserts that this is the thread
/// building the driver, and staying on that thread is what makes it also the
/// thread that drops it — so the thread whose hold declines to wait and the
/// thread that performs the release are the same one, rather than two that
/// happen to coincide. Moved across threads, a hold reaching the real driving
/// thread would block it short of this drop, and the suite would hang with no
/// test named.
pub struct GatedDriver {
    gates: Vec<QuiescenceGate>,
    driver: TestDriver<Scripted, TestBackend>,
    /// The `!Send` marker the paragraph above is about, and nothing else.
    not_send: PhantomData<*const ()>,
}

impl Drop for GatedDriver {
    /// Opens the gates before the driver goes.
    ///
    /// `Drop::drop` runs before any field is dropped, which is what makes
    /// the ordering a property of the language rather than of the field
    /// order below it: reordering, renaming or adding a field cannot put the
    /// executor's shutdown — and its join on the thread a hold is running on
    /// — in front of the release any more.
    fn drop(&mut self) {
        for gate in &self.gates {
            gate.open();
        }
    }
}

impl Deref for GatedDriver {
    type Target = TestDriver<Scripted, TestBackend>;

    fn deref(&self) -> &Self::Target {
        &self.driver
    }
}

impl DerefMut for GatedDriver {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.driver
    }
}

/// A driver whose terminal fails its `healthy_draws + 1`-th draw.
///
/// A render failure is the one termination cause no application can
/// produce, so driving it takes a failing backend rather than a hook inside
/// the kernel — which is what keeps `Program::view` and the terminal the
/// only render owners (RFC 0011 INV-LC5).
pub fn failing_driver(
    script: Script,
    healthy_draws: usize,
) -> (TestDriver<Scripted, FailingBackend>, Journal) {
    script.assert_ungated();
    let journal = Journal::default();
    let program = Scripted {
        journal: journal.clone(),
    };
    let terminal = Terminal::new(FailingBackend::new(8, 2, healthy_draws))
        .expect("sizing the failing backend never fails");
    (
        TestDriver::new(program, script, config(), terminal),
        journal,
    )
}

/// A kernel over the `ParkProbe` series' program: the probe
/// drives the production loop itself, so it takes a kernel rather than a
/// driver, and the gate is production's immediate one.
pub fn park_kernel(script: Script) -> (Kernel<Scripted>, Journal) {
    script.assert_ungated();
    let journal = Journal::default();
    let program = Scripted {
        journal: journal.clone(),
    };
    let kernel = Kernel::new(
        program,
        script,
        &config(),
        GateMode::Immediate,
        LoadObserver::new(),
    );
    (kernel, journal)
}

/// Releases one of `run`'s send-intents and confirms it reached the lane.
///
/// The whole handshake in one statement, because every script uses it the
/// same way: `grant` opens the route by which an entry is appended to the
/// guaranteed sequence, `confirm` reports the commit, and only then is a
/// further grant admitted anywhere on the driver.
pub fn accept<P: Program, B: Backend>(driver: &mut TestDriver<P, B>, run: RunName) {
    accept_within(driver, run, TEST_TURNS);
}

/// Steps a pass begun by `source`, spending turns while that source is not
/// yet ready, on a stated budget.
///
/// **For the multi-worker series only, and it weakens nothing.** Each
/// attempt is a whole pass in the fixed stage order or nothing at all — a
/// refused `step_pass` drives the kernel not at all — so this is pass-unit
/// driving with a bounded wait in front of it, not a stage probe. What makes
/// it necessary is a fact about worker threads rather than about the kernel:
/// a run's exit becomes observable when a worker finishes reaping its task,
/// and no application-side signal can be ordered against that, so the
/// current-thread series' pattern — settle on the application's own mark,
/// then step — has a gap there that no script can close.
///
/// The turn between attempts is spent through [`TestDriver::settle`], the
/// driver's own budgeted waiting primitive, so nothing here reaches past the
/// published surface.
///
/// # Preconditions
///
/// With [`WakeSource::ProducerExit`], readiness is satisfied by *any*
/// observable exit or quiescence, not one the caller has in mind — the
/// report carries no run identity to check against. A call is
/// deterministic only when the script has exactly one exit-capable run
/// outstanding; with more, the step can be admitted by the wrong one while
/// the awaited run is still unreaped, and the caller's next read is back
/// on worker timing. State the census at the call site.
///
/// The retry path also inherits [`TestDriver::settle`]'s misuse condition:
/// do not call with a grant outstanding, banked or not. An attempt that
/// finds the source already ready returns without settling, so the misuse
/// would surface only on the runs that wait — a timing-dependent panic,
/// the class this helper exists to remove.
///
/// # Panics
///
/// Panics when `max_turns` is spent with the source still not ready, and —
/// through the retry path's [`TestDriver::settle`] — when called while a
/// grant is outstanding.
///
/// [`TestDriver::settle`]: crate::testing::driver::TestDriver::settle
#[expect(
    clippy::panic,
    reason = "an exhausted bound fails the test, which is what a panic is here"
)]
pub fn step_when_ready<P: Program, B: Backend>(
    driver: &mut TestDriver<P, B>,
    source: WakeSource,
    max_turns: usize,
) -> StepReport<B::Error> {
    for _ in 0..max_turns {
        if let Ok(stepped) = driver.step_pass(source) {
            return stepped;
        }
        // Exactly one turn: the predicate is false on its first reading and
        // true on the one after the turn.
        let mut spent = false;
        driver.settle(1, || mem::replace(&mut spent, true));
    }
    panic!("bounded step exhausted: {source:?} was still not ready after {max_turns} turns");
}

/// [`accept`] under a stated budget, for the multi-worker series
/// ([`THREADED_TURNS`]).
pub fn accept_within<P: Program, B: Backend>(
    driver: &mut TestDriver<P, B>,
    run: RunName,
    max_turns: usize,
) {
    let token = driver.grant(run).expect("no other grant is outstanding");
    assert_eq!(
        driver.confirm(max_turns, token),
        Confirmed::Accepted,
        "the released send committed into its lane"
    );
}

/// An effect that never produces and never ends, so a run exists to name and
/// no output arrives unbidden.
pub fn silent_effect() -> EffectCommand<u8> {
    Command::stream(stream::pending())
}

/// An effect that sends each of `messages` and then ends.
pub fn sending_effect<I>(messages: I) -> EffectCommand<u8>
where
    I: IntoIterator<Item = u8>,
    I::IntoIter: Send + 'static,
{
    Command::stream(stream::iter(messages))
}

/// An effect that sends each of `messages` and then parks forever, so its
/// run outlives its own output and no exit accompanies the sends.
pub fn parking_effect<I>(messages: I) -> EffectCommand<u8>
where
    I: IntoIterator<Item = u8>,
    I::IntoIter: Send + 'static,
{
    Command::stream(stream::iter(messages).chain(stream::pending()))
}

/// An effect that marks `beacon` and then ends, sending nothing — the shape
/// a cleanup finalizer has, and the one a `settle` predicate watches for.
pub fn marking_effect(beacon: Beacon) -> EffectCommand<u8> {
    Command::stream(stream::unfold(Some(beacon), |state| async move {
        state?.mark();
        None::<(u8, Option<Beacon>)>
    }))
}

/// An effect that parks forever holding a drop-marking guard: owned work
/// whose reclamation is observable from the application side.
///
/// The guard is built before the returned future, so a run reclaimed before
/// its first poll still marks.
pub fn holding_effect(beacon: Beacon) -> EffectCommand<u8> {
    let guard = DropMark::new(beacon);
    Command::stream(stream::unfold(guard, |guard| async move {
        pending::<()>().await;
        Some((0_u8, guard))
    }))
}

/// An effect that sends each of `messages` and then panics — the contained
/// producer-panic class (RFC 0011 INV-LC8).
#[expect(
    clippy::panic,
    reason = "the panic class under test is a real producer panic"
)]
pub fn panicking_effect<I>(messages: I, panicked: Beacon) -> EffectCommand<u8>
where
    I: IntoIterator<Item = u8>,
    I::IntoIter: Send + 'static,
{
    Command::stream(stream::iter(messages).chain(stream::once(async move {
        panicked.mark();
        panic!("producer panic under containment test");
    })))
}

/// An effect that sends each of `messages`, marks `done`, and then ends —
/// a run whose natural finish is observable from the application side, so a
/// `settle` predicate can wait for it.
pub fn finishing_effect(messages: Vec<u8>, done: Beacon) -> EffectCommand<u8> {
    Command::stream(
        stream::iter(messages).chain(stream::unfold(Some(done), |state| async move {
            state?.mark();
            None::<(u8, Option<Beacon>)>
        })),
    )
}

/// A producer-originated quit: the run emits one quit on the control lane
/// and then ends (RFC 0014 §3.3).
pub fn quitting_effect() -> EffectCommand<u8> {
    Command::actions(stream::once(async { Action::Quit }))
}

/// An effect that sends each of `messages`, then holds at `gate` until the
/// reducer opens it, then commits one producer-originated quit, signals the
/// handshake, and parks forever.
///
/// The hold is a busy-yield rather than a waker wait, so nothing about the
/// executor's wake ordering is load-bearing: the producer proceeds exactly
/// when the flag says so. The signal is emitted from the poll *after* the
/// quit's send returned, so it reports a commit rather than an intent — the
/// reducer that is blocked on it resumes only once the quit is in the
/// control lane.
pub fn gated_quitting_effect(messages: Vec<u8>, gate: MidBatchGate) -> EffectCommand<u8> {
    let MidBatchGate {
        open,
        committed,
        reclaimed,
    } = gate;
    let guard = DropMark::new(reclaimed);
    Command::actions(
        stream::iter(messages.into_iter().map(Action::Message))
            .chain(stream::once(async move {
                while !open.load(Ordering::SeqCst) {
                    yield_now().await;
                }
                Action::Quit
            }))
            .chain(stream::once(async move {
                // Held for the park below, so dropping this run — which is
                // what a revocation does — marks the handshake's beacon.
                let _reclaimed = guard;
                // A closed receiver means the reducer that opened this gate
                // is gone, which is the test failing elsewhere; there is
                // nothing useful to do about it from a producer task.
                let _sent = committed.send(());
                pending().await
            })),
    )
}

/// A producer-originated quit whose run then parks forever, so the quit's
/// arrival is the only thing that happens and no task exit can stand in for
/// it. This is the park series' quitter.
///
/// `sent` is marked on the poll that follows the quit's send, which is the
/// poll the producer body reaches only once that send committed — the
/// application-side handshake a park witness synchronizes on.
pub fn parked_quitting_effect(latch: Latch, sent: Beacon) -> EffectCommand<u8> {
    Command::actions(
        stream::once(async move {
            latch.wait().await;
            Action::Quit
        })
        .chain(stream::once(async move {
            sent.mark();
            pending().await
        })),
    )
}

/// An effect that holds at `latch`, then sends each of `messages`, marks
/// `sent`, and parks forever.
///
/// The mark is made on the poll after the last send returned, so it is the
/// application-side signal that the message is in the lane — and the run
/// never ends, so no producer exit accompanies the arrival.
pub fn latched_effect(latch: Latch, messages: Vec<u8>, sent: Beacon) -> EffectCommand<u8> {
    Command::stream(stream::unfold(
        (Some(latch), messages.into_iter(), sent),
        |(latch, mut messages, sent)| async move {
            if let Some(latch) = &latch {
                latch.wait().await;
            }
            if let Some(message) = messages.next() {
                return Some((message, (None, messages, sent)));
            }
            sent.mark();
            pending().await
        },
    ))
}

/// Polls `future` once with the probe's waker.
///
/// The two-stage park witness (RFC 0008 §9.7), synchronous half: a further
/// poll suspends again having run no pass — the application's own
/// instrumentation stays silent, no journal entry and no `view` call — and
/// no wake source is signalled, so by the `Future` contract the loop cannot
/// resume until one does. Returns the wake count the park starts from.
pub fn assert_parked<E, F>(
    probe: &ParkProbe,
    future: Pin<&mut F>,
    journal: &Journal,
    what: &str,
) -> usize
where
    F: Future<Output = Result<Exit, E>>,
{
    let before = probe.wakes();
    let calls = journal.calls();
    assert!(
        probe.poll(future).is_pending(),
        "the loop is suspended: {what}"
    );
    assert_eq!(
        journal.calls(),
        calls,
        "re-polling ran no pass — no delivery, no render, no re-evaluation — so the suspension \
         is a park and not a gap between passes: {what}"
    );
    assert_eq!(
        probe.wakes(),
        before,
        "no wake source is signalled while parked: {what}"
    );
    before
}

/// The same witness hardened with executor turns: every other runnable task,
/// and any wake a self-re-arming loop deferred, gets to run — and the loop's
/// waker must still be unsignalled afterwards.
///
/// That is what separates a park from a loop that yields and re-arms itself
/// every turn. Usable only where the awaited arrival is not itself produced
/// by those turns.
pub async fn assert_parked_across_turns<E, F>(
    probe: &ParkProbe,
    mut future: Pin<&mut F>,
    journal: &Journal,
    what: &str,
) -> usize
where
    F: Future<Output = Result<Exit, E>>,
{
    let before = assert_parked(probe, future.as_mut(), journal, what);
    for _ in 0..NEGATIVE_TURNS {
        turn().await;
    }
    assert_eq!(
        probe.wakes(),
        before,
        "executor turns signalled no wake source, so the loop is parked on a waker rather than \
         re-arming itself: {what}"
    );
    assert_parked(probe, future, journal, what)
}

/// Polls to completion, handing the executor a turn between polls so the
/// runtime-owned tasks the kernel waits on can run. The bound converts a
/// would-be hang into a failed assertion.
#[expect(
    clippy::panic,
    reason = "an exhausted bound fails the test, which is what a panic is here"
)]
pub async fn drive_to_ready<E, F>(probe: &ParkProbe, mut future: Pin<&mut F>) -> Result<Exit, E>
where
    F: Future<Output = Result<Exit, E>>,
{
    for _ in 0..TEST_TURNS {
        if let Poll::Ready(output) = probe.poll(future.as_mut()) {
            return output;
        }
        turn().await;
    }
    panic!("bounded drive exhausted: the loop did not finish in {TEST_TURNS} turns");
}

/// Turns the executor until `condition` holds, on the same bound every other
/// wait here uses.
///
/// The `ParkProbe` series' counterpart to [`TestDriver::settle`], which they
/// cannot call: there is no driver in a probe series (RFC 0008 §9.6).
///
/// [`TestDriver::settle`]: crate::testing::driver::TestDriver::settle
#[expect(
    clippy::panic,
    reason = "an exhausted bound fails the test, which is what a panic is here"
)]
pub async fn settle_until(mut condition: impl FnMut() -> bool, what: &str) {
    for _ in 0..=TEST_TURNS {
        if condition() {
            return;
        }
        turn().await;
    }
    panic!("bounded settle exhausted after {TEST_TURNS} turns: {what}");
}

/// Runs `body` with the process panic hook silenced, serialized against
/// every other hook test, and reports how it ended.
///
/// The two panic classes are the only series that need this: one drives a
/// real producer panic and the other a real application panic, and both
/// would otherwise print a backtrace from a passing test and race the
/// recording hook tests (docs/testing.md, "Process-Global Panic Hook
/// Tests"). The guard recovers from poisoning, so a test that panics on
/// purpose fails only itself.
pub fn silently<T>(body: impl FnOnce() -> T) -> ThreadResult<T> {
    let _guard = hook_guard();
    let previous = panic::take_hook();
    panic::set_hook(Box::new(|_info| {}));
    let outcome = panic::catch_unwind(AssertUnwindSafe(body));
    panic::set_hook(previous);
    outcome
}

/// Hands the driver exactly `turns` turns, and asserts nothing of its own.
///
/// What a series spends before claiming something has **not** happened, so
/// whatever would have made the claim false has had its chance to. Written
/// once because the shape has a trap in it: `settle`'s budget and the count
/// being spent are different quantities, and spelling the second as
/// [`NEGATIVE_TURNS`] inside a `settle` bounded by [`TEST_TURNS`] couples two
/// constants documented as independent — raise the first past the second and
/// the rows fail on the driver's exhausted-budget message, which names
/// neither the constant that moved nor the row that moved it.
///
/// # Panics
///
/// Through [`TestDriver::settle`], which is what spends the turns: outside
/// the running state, and while a grant is outstanding. The second is worth
/// reading as this helper's own precondition rather than as something
/// inherited by accident. A released send still in flight is exactly what
/// makes an append to the guaranteed sequence possible during these turns
/// (RFC 0008 §9.6) — so with a grant outstanding, "it has not happened" is
/// not a claim any number of turns can establish, and a series that wants to
/// spend them there is asking a question the turns cannot answer rather than
/// reaching for the wrong helper.
///
/// [`TestDriver::settle`]: crate::testing::driver::TestDriver::settle
pub fn spend_turns<P: Program, B: Backend>(driver: &mut TestDriver<P, B>, turns: usize) {
    let mut spent = 0_usize;
    driver.settle(turns + 1, || {
        spent += 1;
        spent > turns
    });
}

/// What a caught panic's payload says, for a failure that happened on a
/// thread the reporting one is only watching.
///
/// `panic!` produces a `&str` for a literal and a `String` for a format, and
/// an assertion produces the second; nothing here produces anything else, so
/// the third arm is a description rather than a message.
fn panic_text(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a payload that is neither &str nor String".to_owned())
}

/// Which of two things a watcher's exhausted bound caught, given whether the
/// watched script reached the drop it is about.
///
/// A separate decision from the bound itself, because the bound covers a
/// whole script and only part of it is the claim: reaching the drop and the
/// drop returning are different failures, and reporting the second for the
/// first names a cause that is not implicated.
const fn bound_verdict(reached_drop: bool) -> &'static str {
    if reached_drop {
        "the driver's drop did not return: its shutdown joined the worker running the hold \
         without the release having happened first"
    } else {
        "the script did not reach its drop: something before it outran this bound, and the \
         release is not implicated"
    }
}

/// One executor turn, by the construction RFC 0008 §9.6 pins: spawn a fresh
/// no-op task and await it. Public primitives only, and no instrumentation
/// of the scheduler.
///
/// # Panics
///
/// Panics when the no-op task did not complete, which a task that neither
/// panics nor is aborted cannot do.
pub async fn turn() {
    tokio::spawn(ready(()))
        .await
        .expect("a no-op task neither panics nor is aborted");
}

#[cfg(test)]
mod tests {
    use super::*;

    use tokio::runtime::Builder;

    // A watcher's bound covers a whole script, so its verdict has to say
    // which part of it ran out — the claim is the drop, and everything before
    // it is a different failure wearing the same timeout.
    #[test]
    fn an_exhausted_bound_names_the_half_it_caught() {
        assert!(
            bound_verdict(true).starts_with("the driver's drop did not return"),
            "reaching the drop makes the release the thing that did not happen"
        );
        assert!(
            bound_verdict(false).contains("the release is not implicated"),
            "and not reaching it makes the release the thing that cannot be blamed"
        );
    }

    // The three shapes a caught payload arrives in, since the watcher rows
    // report one from a thread they cannot otherwise ask.
    #[test]
    fn a_caught_payload_reads_as_its_message() {
        assert_eq!(panic_text(&"from a literal"), "from a literal");
        assert_eq!(
            panic_text(&"from a format".to_owned()),
            "from a format",
            "an assertion's payload is a `String`, not a `&str`"
        );
        assert_eq!(
            panic_text(&7_u8),
            "a payload that is neither &str nor String",
            "and anything else is described rather than dropped"
        );
    }

    /// How many scheduler yields a row here spends waiting for a detached
    /// hold to finish.
    ///
    /// A hang guard, so the two costs it trades between are not symmetric and
    /// the value is not a midpoint.
    ///
    /// Passing costs nothing whatever the value is: the loops exit at the
    /// first mark, so nothing here is spent on a bound that is never reached.
    /// What has to fit under it is the longest *stall* — the gap between two
    /// progress marks — not a whole script, which is what keeps the quantity
    /// small enough to survive the conversion below. Worst observed, by
    /// caller and platform:
    ///
    /// |                              | macOS, 10 cores | Linux CI, 4 cores |
    /// | ---                          | ---             | ---               |
    /// | a bare `drop` on a thread    | (not sampled)   |             1913 |
    /// | a whole driver script's drop |           6664 |             3511 |
    ///
    /// The Linux column is 100 suite runs at 4x oversubscription and no
    /// failures; the macOS column is 11 runs at default parallelism, ten of
    /// them between 68 and 373 and one at 6664 with a build running beside
    /// them. That single outlier is the number the margin is taken against,
    /// because a thirtyfold tail is the thing a bound has to survive.
    ///
    /// A count is not a portable unit, which is why both columns exist: a
    /// `thread::yield_now()` yields the scheduling quantum on Darwin and
    /// returns almost immediately on Linux when the peer is already running.
    /// Sampling one platform and calling the ratio a margin is how this
    /// constant was first set, and it was wrong by an order of magnitude.
    ///
    /// Failing is the only thing a larger value delays, and it is measurably
    /// expensive: a yield costs about 154 µs in a debug build *with a blocked
    /// peer*, which is the failing path's own condition, so
    /// [`HANDSHAKE_YIELDS`] would report a regression 154 seconds later — a
    /// bound that slow reads as the hang it replaces.
    ///
    /// So the value sits far above the worst stall and far below that:
    /// `262_144` is about 39x over 6664 and 75x over 3511, for a failure
    /// reported in around 45 seconds. A bound that merely clears the observed
    /// maximum is how the budget this change removed came to fail in the
    /// first place.
    const HOLD_YIELDS: usize = 262_144;

    // The gate stores its release rather than signalling it, which is what
    // makes the order between a script's `open` and the dismantling it holds
    // not load-bearing: a hold that begins after the release reads the flag
    // and returns without waiting for anything.
    //
    // Held on a spawned thread, because on the releasing thread the hold
    // declines to wait at all and this would pass without touching the flag
    // — the row below is the one that covers that.
    //
    // The thread is detached and the result read from the beacon, on a
    // counted bound, rather than joined. A `join` would be the one wait in
    // this file with nothing to end it: the hold it waits on is the fixture's
    // own, with no `GatedDriver` behind it to release it, so a regression in
    // `open` or `wait` would hang the suite here with no test named — which
    // is the failure mode this whole file is arranged against.
    #[test]
    #[expect(
        clippy::panic,
        reason = "an exhausted bound fails the test, which is what a panic is here"
    )]
    fn a_hold_begun_after_its_release_waits_for_nothing() {
        let gate = QuiescenceGate::default();
        let quiesced = Beacon::default();
        gate.open();

        let held = GatedQuiescence {
            gate: gate.clone(),
            quiesced: quiesced.clone(),
        };
        thread::spawn(move || drop(held));

        for _ in 0..HOLD_YIELDS {
            if quiesced.marked() {
                assert!(
                    gate.entered(),
                    "the dismantling recorded itself on the way in"
                );
                return;
            }
            thread::yield_now();
        }
        panic!(
            "the hold did not finish in {HOLD_YIELDS} scheduler yields, with its gate open before \
             it began: it waited for a release that had already been stored"
        );
    }

    // Reached on the thread that has to release it, a hold declines to wait
    // rather than blocking the release it is waiting for.
    //
    // The premise this stands in for is the kernel's: the abort drops a
    // cancelled run's stream on a worker, never on the script's own thread.
    // This fixture does not control that, so it checks it instead of
    // assuming it — and the check has to be this way round, because a hold
    // that did block here would take the whole suite with it and name
    // nothing. Here the run quiesces at once and the window's own assertion
    // is what fails.
    #[test]
    fn a_hold_reached_on_the_releasing_thread_declines_to_wait() {
        let gate = QuiescenceGate::default();
        let quiesced = Beacon::default();

        drop(GatedQuiescence {
            gate: gate.clone(),
            quiesced: quiesced.clone(),
        });

        assert!(
            quiesced.marked(),
            "the hold returned on the releasing thread, with the gate never opened"
        );
        assert!(
            !*gate.open.0.lock().unwrap_or_else(PoisonError::into_inner),
            "and it returned by declining rather than by finding a release"
        );
    }

    // The same check, reached the way the fixture reaches it.
    //
    // The row above drops the hold on a bare test thread, so
    // `Handle::try_current` fails and the plain-wait arm runs. A script's
    // releasing thread is inside a multi-thread runtime's `block_on`, which
    // reports `MultiThread` and takes the `block_in_place` arm instead — the
    // arm the premise check has to hold on, and the one where getting it
    // wrong costs the most: a hold that blocked here would block the release,
    // and `block_in_place` is what keeps the check's own call legal on this
    // thread rather than a panic out of a drop.
    #[test]
    fn a_hold_reached_inside_the_runtime_declines_on_the_releasing_thread() {
        let gate = QuiescenceGate::default();
        let quiesced = Beacon::default();
        let runtime = Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a two-worker runtime");

        runtime.block_on(async {
            drop(GatedQuiescence {
                gate: gate.clone(),
                quiesced: quiesced.clone(),
            });
        });

        assert!(
            quiesced.marked(),
            "the hold returned on the releasing thread, from inside the runtime, with the gate \
             never opened"
        );
        assert!(
            !*gate.open.0.lock().unwrap_or_else(PoisonError::into_inner),
            "and it returned by declining rather than by finding a release"
        );
    }

    // Dropping the driver releases a hold that is *still blocking* — the
    // guarantee a window that fails part-way through rests on, which is that
    // the release lands before the executor's shutdown joins the thread the
    // hold is on.
    //
    // Booting and stopping the run first is the whole point. Without it no
    // hold ever blocks, and the row degrades into checking that a drop flips
    // a boolean — which would keep passing with `gate.open()` moved back out
    // of `Drop::drop` into a field-order dependency, while a failing window
    // went back to hanging.
    //
    // Run on a spawned thread and watched from here on a counted bound,
    // because a regression is a hang rather than a wrong answer, and
    // `GatedDriver` is `!Send`: the driver has to live entirely on the thread
    // that builds it, so the whole script goes there and only beacons come
    // back.
    //
    // Two beacons, because "the thread ended" and "the thread got there" are
    // different questions and only one of them is the bound's. `exited` is
    // marked from a `DropMark`, so an unwind marks it too — otherwise a panic
    // anywhere in the script would sail past the mark and be reported as the
    // release having gone missing, which is a diagnosis of something else
    // entirely. `arrived` is marked on the last line, so a script that
    // panicked is a failed assertion here rather than a pass.
    //
    // And the panic is caught rather than left to the hook, because on a
    // spawned thread the hook is the only route a payload has to the log, and
    // `silently` replaces it process-wide for as long as a panic-class row is
    // running. Reported from here, the message arrives whatever hook happens
    // to be installed — an assertion that promises to name a cause has to be
    // holding the cause.
    #[test]
    #[expect(
        clippy::panic,
        reason = "an exhausted bound fails the test, which is what a panic is here"
    )]
    fn dropping_a_driver_releases_a_hold_that_is_still_blocking() {
        let exited = Beacon::default();
        let arrived = Beacon::default();
        let at_drop = Beacon::default();
        let (watch_exited, watch_arrived) = (exited.clone(), arrived.clone());
        let watch_at_drop = at_drop.clone();
        let progress = Beacon::default();
        let watch_progress = progress.clone();
        let panicked = Arc::new(Mutex::new(Option::<String>::None));
        let reported = Arc::clone(&panicked);

        thread::spawn(move || {
            let _exited = DropMark::new(exited);
            progress.mark();
            let script = AssertUnwindSafe(|| {
                let gate = QuiescenceGate::default();
                let first = ProbeSource::gated("a", gate.clone());
                let superseded = ProbeSource::silent("b");
                let (mut driver, _journal) = threaded_driver_with(
                    Script::new(parking_effect([1]))
                        .feeding([Feed::new(first.clone()), Feed::new(superseded)])
                        .wanting(["a"])
                        .redeclaring(1, ["b"]),
                    config().batch_max_messages(cap(1)),
                );
                progress.mark();
                let trigger = driver.boot().started[0].clone();
                progress.mark();
                accept_within(&mut driver, trigger, THREADED_TURNS);
                progress.mark();
                driver
                    .step_pass(WakeSource::Data)
                    .expect("the trigger is in the lane");
                progress.mark();
                // A's dismantling is now held on a worker, and stays held: this
                // thread never opens the gate itself.
                driver.settle(THREADED_TURNS, || gate.entered());
                progress.mark();

                // The row's own premise, asserted rather than assumed. Without it,
                // a kernel that dropped a cancelled run's stream on the driving
                // thread would have `wait` decline, the hold would finish during
                // the `settle` above, and this row would go back to observing
                // nothing but a boolean while still passing.
                //
                // The turns first are what make it a check rather than a coin
                // toss. `entered` and `quiesced` are marked either side of the
                // hold, so observing the first says nothing yet about the second:
                // a hold that declined marks both back to back and this thread can
                // look in between. Measured with `wait` forced to decline, the
                // bare check caught it 2 times in 5; with these turns spent
                // first, 10 in 10.
                spend_turns(&mut driver, NEGATIVE_TURNS);
                progress.mark();
                assert_eq!(
                    first.quiescences(),
                    0,
                    "the hold is still blocking, which is what makes the drop below a release rather \
                 than a formality"
                );

                at_drop.mark();
                drop(driver);
            });
            if let Err(payload) = panic::catch_unwind(script) {
                *panicked.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(panic_text(&*payload));
                return;
            }
            arrived.mark();
        });

        let mut seen = watch_progress.marks();
        let mut stalled = 0_usize;
        while stalled < HOLD_YIELDS {
            if watch_exited.marked() {
                let cause = reported
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
                assert!(
                    cause.is_none(),
                    "the script thread panicked: {}",
                    cause.unwrap_or_default()
                );
                assert!(
                    watch_arrived.marked(),
                    "the script thread ended before its last line without panicking"
                );
                return;
            }
            // The bound is on a *stall*, not on the script: every phase marks
            // `progress`, and observing a mark resets the count. A yield is
            // worth wildly different amounts of time on different platforms —
            // and on the same one under different load — so a count that has
            // to cover a whole script is a wall-clock bound wearing a
            // portable-looking unit. Covering the longest gap between two
            // marks is a quantity that stays small everywhere.
            let marks = watch_progress.marks();
            if marks == seen {
                stalled += 1;
            } else {
                seen = marks;
                stalled = 0;
            }
            thread::yield_now();
        }
        // Which of the two the stall caught, rather than asserting the
        // interesting one: reaching the drop and the drop returning are
        // different failures, and the second is the row's claim.
        let cause = bound_verdict(watch_at_drop.marked());
        panic!("{cause} ({HOLD_YIELDS} scheduler yields without progress)");
    }
}
