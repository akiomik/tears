//! The B-kernel prototype's twelve acceptance series, plus the
//! claim-specific probes (reservation protocol, gate-wait abort, counter
//! poisoning, barrier scope, bounded commit-ack evaluation), the INV-RC9
//! bound rows, and a production-loop smoke test. All tests are
//! deterministic on the current-thread test executor: no sleeps, no
//! timers; progress is driven by grant handshakes and bounded condition
//! settles.
//!
//! Evidence classification — the twelve series split by instrument, and
//! each series belongs to exactly one:
//!
//! 1. **Pass-unit driving (9 series)**: the pack §H-5 set (S1-S8) plus
//!    "bounded-lane revocation". One driver step executes one whole pass
//!    in the normative §3.5 stage order through
//!    `TestDriver::step_pass`. This is the steady-state evidence
//!    surface.
//! 2. **Park-boundary probing (3 series)**: "parked control-quit wake",
//!    "parked subscription-quiescence wake" and "parked data-lane wake"
//!    drive the production loop (`Kernel::run`) by hand through
//!    `ParkProbe`. A pass-unit driver cannot produce this evidence at
//!    all: its step *is* a pass and it scripts pass initiation, which is
//!    exactly the mechanism under test. This surface is scoped to the
//!    park/wake arming rows and carries no other claim.
//!
//! Outside both: tests prefixed `whitebox_` drive single stages through
//! `TestDriver::step`, bypassing the pinned stage order — they probe
//! stage mechanisms in mid-pass windows unreachable by pass driving and
//! are outside the C-5 / INV-RC13 evidence surface.

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc as std_mpsc};
use std::task::{Context, Poll, Wake, Waker};

use futures::FutureExt;
use futures::future::pending;
use tokio::sync::{Notify, mpsc};
use tokio::task::yield_now;

use super::cmd::{
    CancelPolicy, Cmd, EffectBody, MockSource, Program, Reducer, ScopePath, ScopedChild, SpawnCmd,
    SubDecl, ViewSink,
};
use super::driver::{StepError, TestDriver, settle_until};
use super::kernel::{Branch, ExitReason, HeadlessHost, Kernel, KernelConfig, forwarder_body};
use super::lane::{
    DataSender, EffectCtx, GateMode, GrantOutstanding, IngressHandle, Ledger, OriginGate,
    PendingCounter,
};
use super::registry::Phase;
use crate::test_support::with_silent_panic_hook;

// --- shared scaffolding ---------------------------------------------------

/// Application state for the scripted test app.
#[derive(Default)]
struct AppState {
    /// Every message the reducer applied, in order.
    applied: Vec<u32>,
    /// Currently desired subscription keys.
    want: BTreeSet<&'static str>,
}

/// Reducer closure type for the scripted app.
type OnMsg = Box<dyn Fn(&mut AppState, u32) -> Cmd<u32> + Send + Sync>;

/// A closure-scripted application: `on_msg` runs after the message is
/// recorded; subscriptions are derived from `state.want` over `sources`.
struct ScriptApp {
    on_msg: OnMsg,
    sources: BTreeMap<&'static str, MockSource<u32>>,
}

impl Reducer for ScriptApp {
    type State = AppState;
    type Msg = u32;

    fn reduce(&self, state: &mut AppState, msg: u32) -> Cmd<u32> {
        state.applied.push(msg);
        (self.on_msg)(state, msg)
    }

    fn subscriptions(&self, state: &AppState) -> Vec<SubDecl<u32>> {
        state
            .want
            .iter()
            .filter_map(|key| {
                self.sources
                    .get(key)
                    .map(|source| SubDecl::new(key, source))
            })
            .collect()
    }
}

impl Program for ScriptApp {
    type Flags = (AppState, Cmd<u32>);

    fn init(&self, flags: Self::Flags) -> (AppState, Cmd<u32>) {
        flags
    }

    fn view(&self, _state: &AppState, _sink: &mut ViewSink) {}
}

fn want(keys: &[&'static str]) -> AppState {
    AppState {
        applied: Vec::new(),
        want: keys.iter().copied().collect(),
    }
}

fn body<F, Fut>(f: F) -> EffectBody<u32>
where
    F: FnOnce(EffectCtx<u32>) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    Box::new(move |ctx| Box::pin(f(ctx)))
}

fn anon(label: &'static str, effect: EffectBody<u32>) -> Cmd<u32> {
    Cmd::Spawn(SpawnCmd {
        label,
        scope: ScopePath::root(),
        key: None,
        policy: CancelPolicy::CancelInFlight,
        body: effect,
    })
}

/// Sends each value once (each send waits for its grant), then parks.
fn send_then_park(label: &'static str, values: Vec<u32>) -> Cmd<u32> {
    anon(
        label,
        body(move |ctx| async move {
            for value in values {
                if ctx.handle.send(&value.to_string(), value).await.is_err() {
                    return;
                }
            }
            pending::<()>().await;
        }),
    )
}

/// Sends each value once, then finishes naturally.
fn send_then_finish(label: &'static str, values: Vec<u32>) -> Cmd<u32> {
    anon(
        label,
        body(move |ctx| async move {
            for value in values {
                if ctx.handle.send(&value.to_string(), value).await.is_err() {
                    return;
                }
            }
        }),
    )
}

/// Sets its flag when the effect body is destroyed — the task-reclamation
/// probe for the termination series.
struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// A producer that parks forever holding a drop flag: alive owned work.
/// The guard is constructed at spawn (not at first poll), so reclamation
/// of a never-polled task is still observable.
fn parked_with_flag(label: &'static str, flag: Arc<AtomicBool>) -> Cmd<u32> {
    anon(
        label,
        body(move |_ctx| {
            let guard = DropFlag(flag);
            async move {
                let _guard = guard;
                pending::<()>().await;
            }
        }),
    )
}

/// The mid-batch commit handshake: lets a producer's send commit
/// *during* an in-progress input batch, deterministically.
///
/// The reducer (on the driving worker) opens `open_flag` and then blocks
/// on the std channel until the producer signals that its send
/// committed; the producer (on the second worker) busy-yields on
/// `open_flag` — no waker is involved, so tokio's non-stealable LIFO
/// slot cannot strand it behind the blocked driving worker — then
/// performs its real send and signals. Requires a 2-worker runtime;
/// determinism comes from this application-side synchronization, not
/// from the executor schedule.
struct MidBatchHandshake {
    open_flag: Arc<AtomicBool>,
    committed_rx: Mutex<std_mpsc::Receiver<()>>,
}

impl MidBatchHandshake {
    fn new() -> (Self, Arc<AtomicBool>, std_mpsc::Sender<()>) {
        let open_flag = Arc::new(AtomicBool::new(false));
        let (committed_tx, committed_rx) = std_mpsc::channel();
        let handshake = Self {
            open_flag: Arc::clone(&open_flag),
            committed_rx: Mutex::new(committed_rx),
        };
        (handshake, open_flag, committed_tx)
    }

    /// Reducer side: open the producer's gate, then wait for its commit.
    fn open_and_await_commit(&self) {
        self.open_flag.store(true, Ordering::SeqCst);
        self.committed_rx
            .lock()
            .expect("handshake receiver")
            .recv()
            .expect("the producer commits while the batch is in progress");
    }
}

/// A producer that busy-yields until its application-side flag opens,
/// commits one control-lane quit, signals the handshake, then parks.
fn gated_quitter(
    label: &'static str,
    scope: ScopePath,
    open_flag: Arc<AtomicBool>,
    committed_tx: std_mpsc::Sender<()>,
) -> Cmd<u32> {
    Cmd::Spawn(SpawnCmd {
        label,
        scope,
        key: None,
        policy: CancelPolicy::CancelInFlight,
        body: body(move |ctx| async move {
            while !open_flag.load(Ordering::SeqCst) {
                yield_now().await;
            }
            let _ = ctx.handle.quit().await;
            let _ = committed_tx.send(());
            pending::<()>().await;
        }),
    })
}

fn position_of(entries: &[String], prefix: &str) -> usize {
    entries
        .iter()
        .position(|e| e.starts_with(prefix))
        .unwrap_or_else(|| {
            unreachable!("expected an entry starting with {prefix:?} in {entries:?}")
        })
}

fn driver(app: ScriptApp, state: AppState, init: Cmd<u32>) -> TestDriver<ScriptApp> {
    TestDriver::new(app, (state, init), KernelConfig::default())
}

fn no_subs() -> BTreeMap<&'static str, MockSource<u32>> {
    BTreeMap::new()
}

fn inert(_state: &mut AppState, _msg: u32) -> Cmd<u32> {
    Cmd::None
}

// --- S1: cancel vs buffered output ---------------------------------------

/// S1 — cancel beats the buffered *message*: after the teardown pass
/// revokes the origin, its already-buffered data envelope is filtered at
/// dequeue, the Draining tombstone outlives it until that dequeue zeroes
/// the committed pending, and no stale delivery reaches the reducer.
/// (The buffered-quit half is witnessed same-topology by
/// `s1_buffered_quit_is_discarded_when_its_origin_is_torn_down_mid_batch`;
/// the drain-side filter mechanism is additionally pinned in isolation
/// by the `whitebox_` probe, and the "already-buffered live quit wins
/// over a later cancel trigger" rule by the INV-RC9 pass-start row.)
#[tokio::test]
async fn s1_cancel_beats_buffered_message_and_reclaims_the_tombstone() {
    let input = MockSource::default();
    let app = ScriptApp {
        on_msg: Box::new(|_, msg| match msg {
            1 => Cmd::Spawn(SpawnCmd {
                label: "fx",
                scope: ScopePath::seg("pane"),
                key: Some("k"),
                policy: CancelPolicy::CancelInFlight,
                body: body(|ctx| async move {
                    let _ = ctx.handle.send("10", 10).await;
                    pending::<()>().await;
                }),
            }),
            2 => Cmd::Teardown(ScopePath::seg("pane")),
            3 => Cmd::Quit,
            _ => Cmd::None,
        }),
        sources: BTreeMap::from([("input", input.clone())]),
    };
    let mut driver = TestDriver::new(
        app,
        (want(&["input"]), Cmd::None),
        KernelConfig {
            batch_cap: 1,
            ..KernelConfig::default()
        },
    );
    let boot = driver.boot();
    assert_eq!(
        boot.producers.first().map(|(label, _)| *label),
        Some("input"),
        "boot reports the spawned producers"
    );
    let input_id = driver.producer("input");

    input.push(1);
    driver.grant(input_id).await;
    driver.step_pass(Branch::Input).expect("spawn pass");
    let fx = driver.producer("fx");

    // The cancel trigger enqueues *before* fx's output: FIFO carries
    // [input:2, fx:10] into the next passes (cap 1).
    input.push(2);
    driver.grant(input_id).await;
    driver.grant(fx).await;
    driver.step_pass(Branch::Input).expect("teardown pass");
    assert!(
        driver.delivery().contains("stop:fx"),
        "teardown stops the child run"
    );
    assert_eq!(
        driver.entry_phase("fx"),
        Some((Phase::Stopping, true)),
        "fx is revoked before its buffered output drains"
    );

    driver.await_exit_ready().await;
    driver.step_pass(Branch::JoinExit).expect("filter pass");
    assert_eq!(
        driver.entry_phase("fx"),
        None,
        "tombstone reclaimed at the dequeue that zeroed committed pending"
    );
    let entries = driver.delivery().snapshot();
    assert!(
        position_of(&entries, "exit:fx") < position_of(&entries, "filtered:fx"),
        "the Draining tombstone outlived its buffered envelope: {entries:?}"
    );
    assert!(
        driver.delivery().contains("filtered:fx"),
        "buffered message dropped"
    );
    assert!(
        !driver.delivery().contains("update:10"),
        "no stale delivery"
    );
    assert_eq!(driver.registry_len(), 1, "only the input forwarder remains");

    input.push(3);
    driver.grant(input_id).await;
    driver.step_pass(Branch::Input).expect("quit pass");
    let report = driver.settle().await;
    assert_eq!(report.reason, ExitReason::Quit, "controlled quit");
    assert!(report.gauges_zero, "quiescent postcondition");
    assert_eq!(
        driver.state().applied,
        vec![1, 2, 3],
        "only live deliveries"
    );
}

/// S1 (buffered-quit half, same-topology witness) — a quit commits
/// mid-batch and the *same batch's* next input tears its origin down:
/// commit -> revoke -> drain, witnessed on the ledger. The next pass's
/// control drain discards the buffered quit (origin revoked) and the
/// runtime keeps delivering. Runs on two workers with the mid-batch
/// handshake; determinism is by application-side synchronization.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s1_buffered_quit_is_discarded_when_its_origin_is_torn_down_mid_batch() {
    let (handshake, open_flag, committed_tx) = MidBatchHandshake::new();
    let app = ScriptApp {
        on_msg: Box::new(move |_, msg| {
            if msg == 5 {
                // The quit commits here, mid-batch; the teardown this
                // update returns then revokes its origin — with the quit
                // still buffered on the control lane.
                handshake.open_and_await_commit();
                Cmd::Teardown(ScopePath::seg("quit"))
            } else {
                Cmd::None
            }
        }),
        sources: no_subs(),
    };
    let init = Cmd::Batch(vec![
        send_then_park("feed", vec![5, 6, 7]),
        gated_quitter("quitter", ScopePath::seg("quit"), open_flag, committed_tx),
    ]);
    let mut driver = TestDriver::new(
        app,
        (want(&[]), init),
        KernelConfig {
            batch_cap: 2,
            ..KernelConfig::default()
        },
    );
    driver.boot();
    driver.await_intents("feed", 1).await;
    let feed = driver.producer("feed");
    driver.grant(feed).await;
    driver.grant(feed).await;
    let quitter = driver.producer("quitter");
    let ack = driver
        .grant_handle(quitter)
        .expect("bank the quit allowance");

    driver
        .step_pass(Branch::Input)
        .expect("commit, then revoke, within one batch");
    ack.await;
    let entries = driver.delivery().snapshot();
    assert!(
        position_of(&entries, "accept:quitter:quit") < position_of(&entries, "stop:quitter"),
        "the quit was buffered before its origin was revoked: {entries:?}"
    );
    assert_eq!(
        driver.state().applied,
        vec![5, 6],
        "the batch finished its remainder"
    );
    assert_eq!(
        driver.entry_phase("quitter"),
        Some((Phase::Stopping, true)),
        "origin revoked with the quit still buffered"
    );

    driver.await_exit_ready().await;
    driver
        .step_pass(Branch::JoinExit)
        .expect("the next pass drains and filters");
    assert!(
        driver.delivery().contains("filtered-quit:quitter"),
        "the buffered quit is discarded, not applied"
    );
    assert!(
        !driver.delivery().contains("quit:quitter"),
        "a revoked quit never terminates"
    );
    assert!(
        !driver.delivery().contains("terminating:Quit"),
        "the runtime keeps running"
    );

    driver.grant(feed).await;
    driver.step_pass(Branch::Input).expect("delivery continues");
    assert_eq!(
        driver.state().applied,
        vec![5, 6, 7],
        "cancel beat the buffered quit and delivery continued"
    );
}

// --- S2: stop/restart safety window ---------------------------------------

/// White-box probe (stage-granular `step`; outside the evidence
/// surface) — the reconcile that issues a stop admits nothing even when
/// the stopped task has already exited with the exit *unreflected*.
/// Pass driving cannot reach this window (the §3.5 exit-reflection
/// stage precedes the frame stage in every pass), but in production an
/// exit can become observable mid-pass, after the exit stage already
/// ran — the stopping-pass defer discipline covers exactly that
/// residue, and this probe pins it against the reconcile mechanism in
/// isolation.
#[tokio::test]
async fn whitebox_stop_issuing_reconcile_defers_when_the_exit_is_unreflected() {
    let input = MockSource::default();
    let a_src = MockSource::default();
    let b_src = MockSource::default();
    let app = ScriptApp {
        on_msg: Box::new(|state, msg| {
            if msg == 1 {
                state.want.remove("suba");
                state.want.insert("subb");
            }
            Cmd::None
        }),
        sources: BTreeMap::from([
            ("input", input.clone()),
            ("suba", a_src.clone()),
            ("subb", b_src.clone()),
        ]),
    };
    let mut driver = driver(app, want(&["input", "suba"]), Cmd::None);
    driver.boot();
    let input_id = driver.producer("input");

    // suba's forwarder exits naturally, but the exit stays unobserved.
    a_src.close();
    driver.await_exit_ready().await;
    assert_eq!(
        driver.entry_phase("suba"),
        Some((Phase::Running, false)),
        "exit is not reflected outside the JoinExit branch"
    );

    input.push(1);
    driver.grant(input_id).await;
    driver.step(Branch::Input).expect("deliver swap");
    driver.step(Branch::Frame).expect("stop-issuing pass");
    let delivery = driver.delivery();
    assert!(delivery.contains("stop:suba"), "stop issued");
    assert!(delivery.contains("reconcile:deferred"), "pass defers");
    assert!(
        !delivery.contains("admit:subb"),
        "no admission in the stop pass"
    );

    driver.step(Branch::JoinExit).expect("reflect suba exit");
    driver.step(Branch::Frame).expect("post-quiescence pass");
    let entries = driver.delivery().snapshot();
    assert!(
        position_of(&entries, "exit:suba") < position_of(&entries, "admit:subb"),
        "successor admitted only after quiescence: {entries:?}"
    );
}

/// S2 — removing and re-adding the same identity: the successor waits
/// out the predecessor's quiescence (the safe window), then restarts
/// fresh. Driven pass by pass; both triggers are pre-committed so the
/// stop pass and the barrier pass run back to back with the predecessor
/// still unquiesced.
#[tokio::test]
async fn s2_same_identity_successor_waits_for_stop_quiescence() {
    let input = MockSource::default();
    let a_src = MockSource::default();
    let app = ScriptApp {
        on_msg: Box::new(|state, msg| {
            match msg {
                1 => {
                    state.want.remove("suba");
                }
                2 => {
                    state.want.insert("suba");
                }
                _ => {}
            }
            Cmd::None
        }),
        sources: BTreeMap::from([("input", input.clone()), ("suba", a_src.clone())]),
    };
    let mut driver = TestDriver::new(
        app,
        (want(&["input", "suba"]), Cmd::None),
        KernelConfig {
            batch_cap: 1,
            ..KernelConfig::default()
        },
    );
    driver.boot();
    let input_id = driver.producer("input");

    // Pre-commit both triggers so the two passes run back to back with
    // no yield in between: the predecessor stays unquiesced across them.
    input.push(1);
    driver.grant(input_id).await;
    input.push(2);
    driver.grant(input_id).await;

    driver.step_pass(Branch::Input).expect("stop pass");
    assert!(driver.delivery().contains("stop:suba"), "stop issued");
    assert_eq!(
        driver.delivery().count("reconcile:deferred"),
        1,
        "the stop-issuing pass defers"
    );
    driver.step_pass(Branch::Input).expect("barrier pass");
    assert_eq!(
        driver.delivery().count("admit:suba"),
        1,
        "successor not admitted while the predecessor is stopping"
    );
    assert_eq!(
        driver.delivery().count("reconcile:deferred"),
        2,
        "both passes deferred"
    );

    driver.await_exit_ready().await;
    driver.step_pass(Branch::JoinExit).expect("admission pass");
    assert_eq!(driver.delivery().count("admit:suba"), 2, "fresh restart");
    let entries = driver.delivery().snapshot();
    assert!(
        position_of(&entries, "exit:suba")
            < entries
                .iter()
                .rposition(|e| e == "admit:suba")
                .expect("second admission present"),
        "restart strictly after quiescence: {entries:?}"
    );
}

/// White-box probe (stage-granular `step`; outside the evidence
/// surface) — the control drain's revocation filter in isolation: a
/// buffered quit whose origin was revoked between its commit and the
/// drain is discarded, not applied. The same-topology witness of this
/// window is `s1_buffered_quit_is_discarded_when_its_origin_is_torn_down_mid_batch`
/// (two-worker mid-batch handshake); this probe keeps the drain
/// mechanism pinned on the current-thread domain without the
/// multi-worker construction.
#[tokio::test]
async fn whitebox_revoked_origin_quit_is_filtered_at_the_control_drain() {
    let input = MockSource::default();
    let app = ScriptApp {
        on_msg: Box::new(|_, msg| match msg {
            1 => Cmd::Spawn(SpawnCmd {
                label: "fx",
                scope: ScopePath::seg("pane"),
                key: Some("k"),
                policy: CancelPolicy::CancelInFlight,
                body: body(|ctx| async move {
                    let _ = ctx.handle.quit().await;
                    pending::<()>().await;
                }),
            }),
            2 => Cmd::Teardown(ScopePath::seg("pane")),
            _ => Cmd::None,
        }),
        sources: BTreeMap::from([("input", input.clone())]),
    };
    let mut driver = driver(app, want(&["input"]), Cmd::None);
    driver.boot();
    let input_id = driver.producer("input");

    input.push(1);
    driver.grant(input_id).await;
    driver.step(Branch::Input).expect("spawn fx");
    let fx = driver.producer("fx");
    driver.grant(fx).await;

    // Revoke fx via the input stage alone, bypassing the pass's control
    // drain (the white-box window).
    input.push(2);
    driver.grant(input_id).await;
    driver.step(Branch::Input).expect("revoke fx");
    driver
        .step(Branch::Control)
        .expect("drain the buffered quit");
    assert!(
        driver.delivery().contains("filtered-quit:fx"),
        "the revoked origin's quit is discarded"
    );
    assert!(
        !driver.delivery().contains("quit:fx"),
        "a revoked quit never terminates"
    );
    assert!(
        !driver.delivery().contains("terminating:Quit"),
        "the kernel keeps running"
    );
}

// --- S3: simultaneous readiness, both faces --------------------------------

#[expect(
    clippy::future_not_send,
    reason = "current-thread test helper; the driver never crosses threads"
)]
async fn s3_producer_face(first: &str, second: &str) -> Vec<String> {
    let app = ScriptApp {
        on_msg: Box::new(inert),
        sources: no_subs(),
    };
    let init = Cmd::Batch(vec![
        send_then_park("pa", vec![100]),
        send_then_park("pb", vec![200]),
    ]);
    let mut driver = driver(app, want(&[]), init);
    driver.boot();
    driver.await_intents("pa", 1).await;
    driver.await_intents("pb", 1).await;

    let first_id = driver.producer(first);
    let second_id = driver.producer(second);
    driver.grant(first_id).await;
    driver.grant(second_id).await;
    driver
        .step_pass(Branch::Input)
        .expect("drain both in one pass");
    driver.delivery().snapshot()
}

/// S3 (producer face) — with both producers parked at their send intent,
/// the sequential grant handshake alone fixes the enqueue order; each
/// script replays to an identical observation sequence.
#[tokio::test]
async fn s3_grant_handshake_scripts_the_enqueue_order_of_ready_producers() {
    let ab_one = s3_producer_face("pa", "pb").await;
    let ab_two = s3_producer_face("pa", "pb").await;
    assert_eq!(ab_one, ab_two, "replay identical (pa before pb)");
    let ba_one = s3_producer_face("pb", "pa").await;
    let ba_two = s3_producer_face("pb", "pa").await;
    assert_eq!(ba_one, ba_two, "replay identical (pb before pa)");

    assert!(
        position_of(&ab_one, "accept:pa:100") < position_of(&ab_one, "accept:pb:200"),
        "script order pa->pb on the acceptance ledger"
    );
    assert!(
        position_of(&ab_one, "update:100") < position_of(&ab_one, "update:200"),
        "delivery follows the scripted enqueue order"
    );
    assert!(
        position_of(&ba_one, "update:200") < position_of(&ba_one, "update:100"),
        "reversed script reverses delivery"
    );
    assert_ne!(ab_one, ba_one, "the two scripts are distinguishable");
}

#[expect(
    clippy::future_not_send,
    reason = "current-thread test helper; the driver never crosses threads"
)]
async fn s3_initiation_run(initiate_with_join: bool) -> Vec<String> {
    let app = ScriptApp {
        on_msg: Box::new(inert),
        sources: no_subs(),
    };
    let init = Cmd::Batch(vec![
        send_then_finish("pf", vec![1]),
        send_then_park("pg", vec![]),
    ]);
    let mut driver = driver(app, want(&[]), init);
    driver.boot();
    driver
        .step_pass(Branch::Frame)
        .expect("consume the boot redraw");
    assert!(
        !driver.ready(Branch::Input),
        "parked before the wake: no data"
    );
    assert!(
        !driver.ready(Branch::Control),
        "parked before the wake: no control"
    );
    assert!(
        !driver.ready(Branch::Frame),
        "parked before the wake: no frame work"
    );

    driver.await_intents("pf", 1).await;
    let pf = driver.producer("pf");
    driver.grant(pf).await;
    driver.await_exit_ready().await;
    assert!(driver.ready(Branch::Input), "data wake source ready");
    let initiation = if initiate_with_join {
        Branch::JoinExit
    } else {
        Branch::Input
    };
    driver
        .step_pass(initiation)
        .expect("the scripted initiation starts the pass");
    driver.delivery().snapshot()
}

/// S3 (pass-initiation seam) — with two wake sources simultaneously
/// ready on a parked kernel, the script picks which one initiates the
/// next pass; each script replays identically, and — because the pass
/// itself is the fixed §3.5 pipeline — the initiation choice does not
/// fork the observable sequence.
#[tokio::test]
async fn s3_pass_initiation_is_scriptable_and_replays_identically() {
    let join_one = s3_initiation_run(true).await;
    let join_two = s3_initiation_run(true).await;
    assert_eq!(
        join_one, join_two,
        "replay identical (join-exit initiation)"
    );
    let input_one = s3_initiation_run(false).await;
    let input_two = s3_initiation_run(false).await;
    assert_eq!(input_one, input_two, "replay identical (input initiation)");
    assert_eq!(
        join_one, input_one,
        "the fixed pass pipeline makes the initiation choice observably inconsequential"
    );
}

// --- S4: the two quit semantics -------------------------------------------

/// S4 — a reduce-returned quit is applied synchronously at dispatch: the
/// rest of the already-dequeued batch is never reduced, and no further
/// step is accepted.
#[tokio::test]
async fn s4_reduce_quit_is_synchronous_and_stops_the_batch() {
    let input = MockSource::default();
    let app = ScriptApp {
        on_msg: Box::new(|_, msg| if msg == 7 { Cmd::Quit } else { Cmd::None }),
        sources: BTreeMap::from([("input", input.clone())]),
    };
    let mut driver = driver(app, want(&["input"]), Cmd::None);
    driver.boot();
    let input_id = driver.producer("input");
    for value in [1, 7, 2] {
        input.push(value);
        driver.grant(input_id).await;
    }

    driver
        .step_pass(Branch::Input)
        .expect("batch with quit inside");
    assert_eq!(
        driver.state().applied,
        vec![1, 7],
        "messages after the quit are never applied"
    );
    assert!(driver.delivery().contains("terminating:Quit"), "sync quit");
    assert_eq!(
        driver.step_pass(Branch::Input),
        Err(StepError::Terminated),
        "no further application calls after termination"
    );
    let report = driver.settle().await;
    assert!(report.gauges_zero, "quiescent postcondition");
}

/// S4 — an effect-issued quit travels the control lane and is observed
/// without draining the data backlog: the pass's mandatory control drain
/// precedes its input batch, so a quit ready at pass start wins
/// (backlog independence, INV-RC9).
#[tokio::test]
async fn s4_control_quit_is_observed_independently_of_data_backlog() {
    let app = ScriptApp {
        on_msg: Box::new(inert),
        sources: no_subs(),
    };
    let init = Cmd::Batch(vec![
        send_then_park("noise", vec![100, 200]),
        anon(
            "quitter",
            body(|ctx| async move {
                let _ = ctx.handle.quit().await;
                pending::<()>().await;
            }),
        ),
    ]);
    let mut driver = driver(app, want(&[]), init);
    driver.boot();
    driver.await_intents("noise", 1).await;
    let noise = driver.producer("noise");
    driver.grant(noise).await;
    driver.grant(noise).await;
    driver.await_intents("quitter", 1).await;
    let quitter = driver.producer("quitter");
    driver.grant(quitter).await;

    driver
        .step_pass(Branch::Control)
        .expect("control drains first");
    assert!(
        driver.delivery().contains("quit:quitter"),
        "live-origin quit"
    );
    assert!(
        driver.state().applied.is_empty(),
        "quit observed without draining the data backlog"
    );
    let report = driver.settle().await;
    assert_eq!(report.reason, ExitReason::Quit, "controlled quit");
    assert!(report.gauges_zero, "backlogged producers reclaimed");
}

// --- S5: the two panic classes --------------------------------------------

#[expect(clippy::panic, reason = "S5 needs a real producer panic")]
fn panicking_after_send(label: &'static str) -> Cmd<u32> {
    anon(
        label,
        body(|ctx| async move {
            let _ = ctx.handle.send("10", 10).await;
            panic!("producer panic under containment test");
        }),
    )
}

/// S5 — a producer panic is contained as a join error: its committed
/// output still delivers (live Draining), the entry is reclaimed by the
/// unified rule, and other producers keep delivering.
#[tokio::test]
async fn s5_producer_panic_is_contained_and_delivery_continues() {
    // Deliberate-panic test: serialize with the process-global
    // panic-hook tests and silence the intentional panic output
    // (docs/testing.md "Process-Global Panic Hook Tests").
    with_silent_panic_hook(async {
        let app = ScriptApp {
            on_msg: Box::new(inert),
            sources: no_subs(),
        };
        let init = Cmd::Batch(vec![
            panicking_after_send("pan"),
            send_then_park("healthy", vec![20]),
        ]);
        let mut driver = driver(app, want(&[]), init);
        driver.boot();
        driver.await_intents("pan", 1).await;
        let pan = driver.producer("pan");
        let healthy = driver.producer("healthy");
        driver.grant(pan).await;

        driver.await_exit_ready().await;
        driver
            .step_pass(Branch::JoinExit)
            .expect("containment pass");
        assert!(
            driver.delivery().contains("exit:pan:Panicked"),
            "panic observed as a join error"
        );
        let entries = driver.delivery().snapshot();
        assert!(
            position_of(&entries, "exit:pan:Panicked") < position_of(&entries, "update:10"),
            "the committed output delivers through the live tombstone: {entries:?}"
        );
        assert_eq!(driver.entry_phase("pan"), None, "entry reclaimed");
        driver.grant(healthy).await;
        driver
            .step_pass(Branch::Input)
            .expect("other producers unaffected");
        assert_eq!(driver.state().applied, vec![10, 20], "loop continued");
        assert!(
            !driver.delivery().contains("terminating:Quit"),
            "containment: no termination"
        );
    })
    .await;
}

/// S5 — a panic in application code on the driving task is fail-fast: it
/// escapes the step (never converted into a continuation), and dropping
/// the kernel reclaims all producers.
fn panicking_reducer(_state: &mut AppState, msg: u32) -> Cmd<u32> {
    assert!(msg != 13, "application panic under fail-fast test");
    Cmd::None
}

#[tokio::test]
async fn s5_driving_side_app_panic_fails_fast_and_producers_are_reclaimed() {
    // Deliberate-panic test: serialize with the process-global
    // panic-hook tests and silence the intentional panic output
    // (docs/testing.md "Process-Global Panic Hook Tests").
    with_silent_panic_hook(async {
        let input = MockSource::default();
        let flag = Arc::new(AtomicBool::new(false));
        let app = ScriptApp {
            on_msg: Box::new(panicking_reducer),
            sources: BTreeMap::from([("input", input.clone())]),
        };
        let init = parked_with_flag("parked", Arc::clone(&flag));
        let mut driver = driver(app, want(&["input"]), init);
        driver.boot();
        let input_id = driver.producer("input");
        input.push(13);
        driver.grant(input_id).await;

        let unwound = catch_unwind(AssertUnwindSafe(|| driver.step_pass(Branch::Input)));
        assert!(unwound.is_err(), "the panic escapes the driving pass");

        let gauges = driver.gauges();
        let delivery = driver.delivery();
        drop(driver);
        settle_until(
            || gauges.producers() == 0 && flag.load(Ordering::SeqCst),
            "kernel drop reclaims all producers",
        )
        .await;
        assert!(
            delivery.contains("drop:immediate"),
            "drop postcondition ran"
        );
    })
    .await;
}

// --- S6: send failure (shutdown-scoped) ------------------------------------

/// S6 arm (a), full topology — a producer blocked in a bounded send holds
/// a reservation; termination drops the lanes, the blocked send's future
/// is dropped, and the RAII release leaves no counter residue while the
/// two-stage postcondition reclaims everything.
#[tokio::test]
async fn s6_blocked_send_is_released_and_reclaimed_by_termination() {
    let app = ScriptApp {
        on_msg: Box::new(inert),
        sources: no_subs(),
    };
    let init = Cmd::Batch(vec![
        send_then_park("flood", vec![1, 2, 3]),
        anon(
            "quitter",
            body(|ctx| async move {
                let _ = ctx.handle.quit().await;
                pending::<()>().await;
            }),
        ),
    ]);
    let mut driver = TestDriver::new(
        app,
        (want(&[]), init),
        KernelConfig {
            capacity: Some(1),
            ..KernelConfig::default()
        },
    );
    driver.boot();
    driver.await_intents("flood", 1).await;
    let flood = driver.producer("flood");
    driver.grant(flood).await;

    // Second send: granted but the lane is full — the producer parks
    // inside the real send holding its reservation.
    let flood_counter = driver.counter_of("flood");
    let mut blocked_ack = driver.grant_handle(flood).expect("second grant issues");
    settle_until(
        || flood_counter.value() == 2,
        "second send holds a reservation while waiting for capacity",
    )
    .await;
    assert!(
        (&mut blocked_ack).now_or_never().is_none(),
        "no acceptance while the send waits for capacity"
    );

    driver.await_intents("quitter", 1).await;
    let quitter = driver.producer("quitter");
    driver.grant(quitter).await;
    driver
        .step_pass(Branch::Control)
        .expect("terminate under blocked send");

    let report = driver.settle().await;
    assert!(report.gauges_zero, "all producers reclaimed");
    assert_eq!(
        flood_counter.value(),
        1,
        "blocked reservation released; only the undrained committed envelope remains"
    );
    assert!(
        (&mut blocked_ack).now_or_never().is_none(),
        "the blocked grant never acknowledges a send that never committed"
    );
    assert!(
        !driver.delivery().contains("accept:flood:2"),
        "no false acceptance"
    );
}

/// S6 arm (b), component layer — the real forwarder body over a real lane
/// observes closure (`Err`), exits autonomously, and its reservation is
/// released.
#[tokio::test]
async fn s6_component_forwarder_observes_closure_and_exits() {
    let (data_tx, data_rx) = mpsc::channel(1);
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let counter = Arc::new(PendingCounter::default());
    let gate = Arc::new(OriginGate::new(GateMode::Immediate));
    let handle = IngressHandle::new(
        "fwd",
        1,
        Arc::clone(&counter),
        Arc::clone(&gate),
        DataSender::Bounded(data_tx),
        control_tx,
        Ledger::default(),
        Ledger::default(),
    );
    let source: MockSource<u32> = MockSource::default();
    source.push(42);

    drop(data_rx);
    drop(control_rx);
    let task = tokio::spawn(forwarder_body(source)(EffectCtx { handle }));
    task.await.expect("forwarder exits cleanly, not by panic");
    assert_eq!(counter.value(), 0, "failed send released its reservation");
    assert_eq!(gate.commits(), 0, "closure is never counted as acceptance");
}

// --- S7: idle wake ---------------------------------------------------------

/// S7 — natural finish alone does not wake the parked loop (RFC 0012 /
/// spec §3 dirt sources); the still-declared finished source restarts on
/// the next message-driven re-evaluation.
#[tokio::test]
async fn s7_natural_finish_alone_does_not_wake_the_frame_pass() {
    let input = MockSource::default();
    let fin = MockSource::default();
    let app = ScriptApp {
        on_msg: Box::new(inert),
        sources: BTreeMap::from([("input", input.clone()), ("fin", fin.clone())]),
    };
    let mut driver = driver(app, want(&["input", "fin"]), Cmd::None);
    driver.boot();
    let input_id = driver.producer("input");
    driver
        .step_pass(Branch::Frame)
        .expect("consume the boot redraw");
    assert_eq!(
        driver.delivery().count("reconcile"),
        1,
        "boot reconcile only"
    );

    fin.close();
    driver.await_exit_ready().await;
    driver
        .step_pass(Branch::JoinExit)
        .expect("natural-finish pass");
    assert_eq!(
        driver.delivery().count("reconcile"),
        1,
        "a natural finish triggers no re-evaluation"
    );
    assert_eq!(
        driver.step_pass(Branch::Frame),
        Err(StepError::NotReady(Branch::Frame)),
        "natural finish leaves the loop parked"
    );

    input.push(5);
    driver.grant(input_id).await;
    driver
        .step_pass(Branch::Input)
        .expect("message-driven re-evaluation");
    assert_eq!(
        driver.delivery().count("admit:fin"),
        2,
        "still-declared finished source restarts at the next re-evaluation"
    );
}

/// S7 — the quiescence of a stopped run wakes the frame pass with no
/// message in between (the message-independent re-evaluation trigger).
#[tokio::test]
async fn s7_stop_quiescence_wakes_the_frame_pass_without_a_message() {
    let input = MockSource::default();
    let a_src = MockSource::default();
    let b_src = MockSource::default();
    let app = ScriptApp {
        on_msg: Box::new(|state, msg| {
            if msg == 1 {
                state.want.remove("suba");
                state.want.insert("subb");
            }
            Cmd::None
        }),
        sources: BTreeMap::from([
            ("input", input.clone()),
            ("suba", a_src.clone()),
            ("subb", b_src.clone()),
        ]),
    };
    let mut driver = driver(app, want(&["input", "suba"]), Cmd::None);
    driver.boot();
    let input_id = driver.producer("input");
    driver
        .step_pass(Branch::Frame)
        .expect("consume the boot redraw");

    input.push(1);
    driver.grant(input_id).await;
    driver.step_pass(Branch::Input).expect("stop pass");
    assert_eq!(
        driver.step_pass(Branch::Frame),
        Err(StepError::NotReady(Branch::Frame)),
        "nothing left to run: the loop is parked"
    );

    driver.await_exit_ready().await;
    driver
        .step_pass(Branch::JoinExit)
        .expect("idle wake pass: exit reflection dirties, the same pass re-evaluates");
    let entries = driver.delivery().snapshot();
    let wake = position_of(&entries, "exit:suba");
    let admit = position_of(&entries, "admit:subb");
    assert!(wake < admit, "admission follows the wake: {entries:?}");
    assert!(
        !entries[wake..admit]
            .iter()
            .any(|e| e.starts_with("update:")),
        "no message between quiescence and the re-evaluation: {entries:?}"
    );
}

// --- S8: termination under owned work --------------------------------------

struct OwnedWork {
    driver: TestDriver<ScriptApp>,
    input: MockSource<u32>,
    flag: Arc<AtomicBool>,
}

fn owned_work(host: HeadlessHost) -> OwnedWork {
    let input = MockSource::default();
    let flag = Arc::new(AtomicBool::new(false));
    let app = ScriptApp {
        on_msg: Box::new(|_, msg| if msg == 7 { Cmd::Quit } else { Cmd::None }),
        sources: BTreeMap::from([("input", input.clone())]),
    };
    let init = Cmd::Batch(vec![
        parked_with_flag("parked", Arc::clone(&flag)),
        Cmd::Spawn(SpawnCmd {
            label: "fetch",
            scope: ScopePath::seg("work"),
            key: Some("k"),
            policy: CancelPolicy::CancelInFlight,
            body: body(|_ctx| pending::<()>()),
        }),
        anon(
            "quitter",
            body(|ctx| async move {
                let _ = ctx.handle.quit().await;
                pending::<()>().await;
            }),
        ),
    ]);
    let driver =
        TestDriver::with_host(app, (want(&["input"]), init), KernelConfig::default(), host);
    OwnedWork {
        driver,
        input,
        flag,
    }
}

async fn assert_reclaimed(work: &mut OwnedWork, reason: ExitReason) {
    assert_eq!(
        work.driver.step_pass(Branch::Input),
        Err(StepError::Terminated),
        "no application calls after termination"
    );
    assert!(
        work.driver.delivery().contains("immediate"),
        "stage one ran"
    );
    let report = work.driver.settle().await;
    assert_eq!(
        report.reason, reason,
        "cause routed through the shared path"
    );
    assert!(report.gauges_zero, "gauges all zero");
    assert!(
        work.flag.load(Ordering::SeqCst),
        "parked owned work was dropped"
    );
}

/// S8 — reduce-returned quit under owned work: both postconditions hold
/// and the queued backlog is discarded, not applied.
#[tokio::test]
async fn s8_reduce_quit_reclaims_owned_work_and_discards_backlog() {
    let mut work = owned_work(HeadlessHost::default());
    work.driver.boot();
    let input_id = work.driver.producer("input");
    for value in [5, 7, 6] {
        work.input.push(value);
        work.driver.grant(input_id).await;
    }
    work.driver
        .step_pass(Branch::Input)
        .expect("quit mid-batch");
    assert_eq!(work.driver.state().applied, vec![5, 7], "backlog discarded");
    assert_reclaimed(&mut work, ExitReason::Quit).await;
}

/// S8 — control-lane quit under owned work.
#[tokio::test]
async fn s8_control_quit_reclaims_owned_work() {
    let mut work = owned_work(HeadlessHost::default());
    work.driver.boot();
    let input_id = work.driver.producer("input");
    work.input.push(5);
    work.driver.grant(input_id).await;
    work.driver.await_intents("quitter", 1).await;
    let quitter = work.driver.producer("quitter");
    work.driver.grant(quitter).await;
    work.driver
        .step_pass(Branch::Control)
        .expect("control quit");
    assert!(work.driver.state().applied.is_empty(), "backlog untouched");
    assert_reclaimed(&mut work, ExitReason::Quit).await;
}

/// S8 — host render failure routes through the same termination.
#[tokio::test]
async fn s8_render_error_reclaims_owned_work() {
    let mut work = owned_work(HeadlessHost {
        fail_at: Some(1),
        ..HeadlessHost::default()
    });
    work.driver.boot();
    work.driver
        .step_pass(Branch::Frame)
        .expect("failing render");
    assert_reclaimed(&mut work, ExitReason::RenderError).await;
}

/// S8 — dropping the run future (the driver) reclaims owned work through
/// the kernel's drop postcondition.
#[tokio::test]
async fn s8_run_future_drop_reclaims_owned_work() {
    let mut work = owned_work(HeadlessHost::default());
    work.driver.boot();
    let gauges = work.driver.gauges();
    let delivery = work.driver.delivery();
    let flag = Arc::clone(&work.flag);
    drop(work);
    settle_until(
        || gauges.producers() == 0 && flag.load(Ordering::SeqCst),
        "drop reclaims all owned work",
    )
    .await;
    assert!(
        delivery.contains("drop:immediate"),
        "drop postcondition ran"
    );
}

// --- park/wake witnesses (the `ParkProbe` surface) --------------------------

/// A permit-storing release signal for producer bodies: `open` banks the
/// permit whether or not the body has reached its wait, so no scheduling
/// order is load-bearing.
#[derive(Clone, Default)]
struct Latch(Arc<Notify>);

impl Latch {
    /// Releases the producer holding at this latch.
    fn open(&self) {
        self.0.notify_one();
    }

    /// Producer side: holds until the test opens the latch.
    async fn wait(&self) {
        self.0.notified().await;
    }
}

/// A producer that holds at its latch, commits exactly one control-lane
/// quit when released, and then parks forever — the wake
/// counterexample's own producer ("emits `Quit`, then blocks, never
/// exits"), so no task exit can stand in for the wake under test.
fn latched_quitter(label: &'static str, latch: Latch) -> Cmd<u32> {
    anon(
        label,
        body(move |ctx| async move {
            latch.wait().await;
            let _ = ctx.handle.quit().await;
            pending::<()>().await;
        }),
    )
}

/// A producer that holds at its latch, then sends each value once and
/// parks — a test-timed data-lane arrival for the production loop whose
/// origin never exits.
fn latched_sender(label: &'static str, latch: Latch, values: Vec<u32>) -> Cmd<u32> {
    anon(
        label,
        body(move |ctx| async move {
            latch.wait().await;
            for value in values {
                if ctx.handle.send(&value.to_string(), value).await.is_err() {
                    return;
                }
            }
            pending::<()>().await;
        }),
    )
}

/// Counts the signals the kernel's own wake arming produces.
#[derive(Default, Debug)]
struct WakeCount(AtomicUsize);

impl Wake for WakeCount {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// Hand-driving probe for the production loop's park boundary — the
/// second named evidence surface, scoped to the park/wake arming rows
/// and carrying no other claim. It owns the *only* waker `Kernel::run`
/// is ever polled with and it is the only thing that ever polls that
/// future, so the counter reads exactly the wake sources the kernel
/// armed and "still parked" becomes observable instead of inferred. A
/// kernel that arms wakers on the data lane and the join set only
/// leaves the counter at its parked value when a quit arrives on the
/// control lane.
///
/// Why the pass-unit surface cannot carry these three series: one step
/// of that driver *is* one pass, and it scripts which source initiates
/// the pass — the very mechanism the park/wake rows constrain. A
/// scripted driver never parks, so a parked kernel is unreachable from
/// it and the wake-source set is unobservable through it.
struct ParkProbe {
    count: Arc<WakeCount>,
    waker: Waker,
}

impl ParkProbe {
    /// A fresh probe with no signals recorded.
    fn new() -> Self {
        let count = Arc::new(WakeCount::default());
        let waker = Waker::from(Arc::clone(&count));
        Self { count, waker }
    }

    /// Signals this waker has received so far.
    fn wakes(&self) -> usize {
        self.count.0.load(Ordering::SeqCst)
    }

    /// One poll with the probe's waker.
    fn poll<F: Future>(&self, fut: Pin<&mut F>) -> Poll<F::Output> {
        fut.poll(&mut Context::from_waker(&self.waker))
    }

    /// Establishes that the loop is **parked** and not merely between
    /// passes: a further poll suspends again having run no pass (the
    /// delivery ledger is unchanged), and no wake source is signalled —
    /// so by the `Future` contract the loop cannot resume until some
    /// source signals this waker. Returns the wake count the park starts
    /// from.
    fn assert_parked<F: Future>(&self, fut: Pin<&mut F>, delivery: &Ledger, what: &str) -> usize {
        let before = self.wakes();
        let ledger = delivery.snapshot();
        assert!(self.poll(fut).is_pending(), "the loop is suspended: {what}");
        assert_eq!(
            delivery.snapshot(),
            ledger,
            "re-polling ran no pass, so the suspension is a park and not a \
             gap between passes: {what}"
        );
        assert_eq!(
            self.wakes(),
            before,
            "no wake source is signalled while parked: {what}"
        );
        before
    }

    /// The park assertion hardened with executor turns: the runtime is
    /// handed turns so that every other runnable task, and any wake a
    /// self-re-arming loop deferred, gets to run — and the loop's waker
    /// must *still* be unsignalled afterwards. This is what separates a
    /// park from a loop that yields and re-arms itself every turn: the
    /// yielding impostor accumulates signals here, a parked loop
    /// accumulates none. Usable only where the awaited event is not
    /// itself produced by those turns.
    async fn assert_parked_across_turns<F: Future>(
        &self,
        mut fut: Pin<&mut F>,
        delivery: &Ledger,
        what: &str,
    ) -> usize {
        let before = self.assert_parked(fut.as_mut(), delivery, what);
        // The turn count is not a correctness condition: any number of
        // executor turns has to leave the waker unsignalled.
        for _ in 0..8 {
            yield_now().await;
        }
        assert_eq!(
            self.wakes(),
            before,
            "executor turns signalled no wake source, so the loop is parked \
             on a waker rather than re-arming itself: {what}"
        );
        self.assert_parked(fut.as_mut(), delivery, what)
    }

    /// Polls to completion, handing the executor a turn between polls so
    /// the runtime-owned tasks the kernel waits on can run. The
    /// iteration bound converts a would-be hang into a failed assertion.
    async fn drive_to_ready<F: Future>(&self, mut fut: Pin<&mut F>, what: &str) -> F::Output {
        for _ in 0..10_000 {
            if let Poll::Ready(output) = self.poll(fut.as_mut()) {
                return output;
            }
            yield_now().await;
        }
        unreachable!("bounded drive exhausted: {what}")
    }
}

/// "parked control-quit wake" — the control-lane arm of the park/wake
/// contract, on the production loop. The loop is polled by hand with a
/// counting waker on the current-thread executor, so runtime turns
/// happen only where this test yields. After the boot pass it suspends
/// with **zero** signals recorded and re-polling runs no pass: it is
/// genuinely waiting on a waker, not sitting in the gap between two
/// passes. A producer-originated quit then commits on the control lane
/// while nothing else changes — no input, no task exit (the quitter
/// parks forever), no pending frame work — and that arrival **alone**
/// signals the waker; one further poll runs the woken pass, whose
/// mandatory control drain applies the quit.
///
/// The wake counterexample this excludes: a kernel registering wakers on
/// the data lane and the join set only satisfies "a quit is applied at
/// the first control drain at or after its arrival" vacuously, because
/// no pass ever begins. Its counter never leaves the parked value and
/// this witness fails at the wake assertion.
#[tokio::test(flavor = "current_thread")]
async fn parked_control_quit_wake() {
    let latch = Latch::default();
    let app = ScriptApp {
        on_msg: Box::new(inert),
        sources: no_subs(),
    };
    let mut kernel = Kernel::new(
        app,
        (want(&[]), latched_quitter("quitter", latch.clone())),
        KernelConfig::default(),
        GateMode::Immediate,
        HeadlessHost::default(),
    );
    let delivery = kernel.delivery();
    let probe = ParkProbe::new();
    let mut run = Box::pin(kernel.run());

    assert!(
        probe.poll(run.as_mut()).is_pending(),
        "the production loop suspends after the boot pass"
    );
    assert!(delivery.contains("render"), "the boot pass rendered");
    let parked_at = probe
        .assert_parked_across_turns(run.as_mut(), &delivery, "after the boot pass")
        .await;
    assert_eq!(parked_at, 0, "nothing has woken the loop yet");

    // The one event: a producer-originated quit reaches the control
    // lane. `accept:quitter:quit` is that send's enqueue acceptance —
    // the handshake this witness synchronizes on.
    latch.open();
    settle_until(
        || delivery.contains("accept:quitter:quit"),
        "the quit is accepted by the control lane",
    )
    .await;
    let arrival = delivery.snapshot();
    assert!(
        !arrival.iter().any(|e| e.starts_with("update:")),
        "no input accompanied the quit: {arrival:?}"
    );
    assert_eq!(
        probe.wakes(),
        parked_at + 1,
        "the control-lane arrival alone woke the parked loop, with exactly \
         one signal"
    );

    assert!(
        probe.poll(run.as_mut()).is_pending(),
        "the woken pass terminates and the loop moves to settle"
    );
    assert!(
        delivery.contains("quit:quitter"),
        "the woken pass applied the arrived quit at its control drain"
    );
    assert!(
        delivery.contains("terminating:Quit"),
        "controlled termination"
    );
    let report = probe
        .drive_to_ready(run.as_mut(), "the terminated loop settles")
        .await;
    assert_eq!(report.reason, ExitReason::Quit, "controlled quit");
    assert!(report.gauges_zero, "the quitter was reclaimed at settle");
}

/// "parked subscription-quiescence wake" — the
/// subscription-quiescence arm of the park/wake contract, on the
/// production loop and under the same hand-driven park witness. One
/// message flips the declaration, so the pass stops `suba` and, being a
/// stopping pass, admits nothing; the loop then parks with the stop
/// outstanding. The stopped run's quiescence — observed independently
/// through the producer gauge, which the run's task drops on
/// reclamation — is then the one event, and it alone signals the waker.
/// One further poll runs the woken pass: its exit reflection marks
/// subscriptions dirty and the same pass's frame stage re-evaluates and
/// admits the successor, with no message in between.
#[tokio::test(flavor = "current_thread")]
async fn parked_subscription_quiescence_wake() {
    let latch = Latch::default();
    let a_src = MockSource::default();
    let b_src = MockSource::default();
    let app = ScriptApp {
        on_msg: Box::new(|state, msg| {
            if msg == 1 {
                state.want.remove("suba");
                state.want.insert("subb");
            }
            Cmd::None
        }),
        sources: BTreeMap::from([("suba", a_src.clone()), ("subb", b_src.clone())]),
    };
    let mut kernel = Kernel::new(
        app,
        (
            want(&["suba"]),
            latched_sender("trigger", latch.clone(), vec![1]),
        ),
        KernelConfig::default(),
        GateMode::Immediate,
        HeadlessHost::default(),
    );
    let delivery = kernel.delivery();
    let gauges = kernel.gauges();
    let probe = ParkProbe::new();
    let mut run = Box::pin(kernel.run());

    assert!(
        probe.poll(run.as_mut()).is_pending(),
        "the production loop suspends after the boot pass"
    );
    assert!(
        delivery.contains("admit:suba"),
        "boot admitted the declared subscription"
    );
    probe
        .assert_parked_across_turns(run.as_mut(), &delivery, "after the boot pass")
        .await;
    assert_eq!(
        gauges.producers(),
        2,
        "the trigger and suba are the live producers"
    );

    latch.open();
    settle_until(
        || delivery.contains("accept:trigger:1"),
        "the trigger's message is accepted by the data lane",
    )
    .await;
    assert!(
        probe.poll(run.as_mut()).is_pending(),
        "the message-woken pass runs the re-evaluation"
    );
    assert!(
        delivery.contains("stop:suba"),
        "the re-evaluation stopped suba"
    );
    assert!(
        delivery.contains("reconcile:deferred"),
        "the stopping pass admits nothing"
    );
    assert!(
        !delivery.contains("admit:subb"),
        "the successor waits behind the barrier"
    );
    // This park carries the synchronous assertion only: the executor
    // turns that would falsify a yielding impostor are the very turns
    // that reclaim the stopped run, so they cannot be spent before the
    // awaited event. The exact-signal assertion below covers that gap
    // instead — an impostor re-arming itself on every turn accumulates
    // more than the one signal the quiescence produces.
    let parked_at = probe.assert_parked(run.as_mut(), &delivery, "with the stop outstanding");

    // The one event: the stop-requested subscription run quiesces. No
    // input, no control traffic, no pending frame work.
    settle_until(
        || gauges.producers() == 1,
        "the stopped subscription run quiesces",
    )
    .await;
    assert_eq!(
        probe.wakes(),
        parked_at + 1,
        "the quiescence notification alone woke the parked loop, with \
         exactly one signal"
    );

    assert!(
        probe.poll(run.as_mut()).is_pending(),
        "the woken pass reflects the exit and re-evaluates"
    );
    let entries = delivery.snapshot();
    let wake = position_of(&entries, "exit:suba");
    let admit = position_of(&entries, "admit:subb");
    assert!(wake < admit, "the admission follows the wake: {entries:?}");
    assert!(
        !entries[wake..admit]
            .iter()
            .any(|e| e.starts_with("update:")),
        "no message between the quiescence and the re-evaluation: {entries:?}"
    );
    probe.assert_parked(run.as_mut(), &delivery, "after the woken pass");

    drop(run);
    assert_eq!(
        kernel.state().applied,
        vec![1],
        "one message drove the whole witness"
    );
}

/// "parked data-lane wake" — the data-lane arm of the park/wake
/// contract, the third and last wake source, on the production loop
/// under the same hand-driven park witness. The other two series reach a
/// data-lane arrival only in passing; this one is the behavioral row for
/// it.
///
/// The producer is a latched sender that **parks forever after its
/// send**, so its task never exits: no producer-exit notification can
/// stand in for the wake being measured. After the boot pass the loop is
/// parked under both proofs (re-poll runs no pass; executor turns
/// signal nothing). Releasing the latch makes the data-lane acceptance
/// the *only* event — asserted literally, as the one entry the delivery
/// ledger gains — and the waker takes **exactly one** signal from it,
/// before any poll. The next poll then runs the woken pass and the
/// message reaches the reducer: `update:5` on the ledger and the value
/// in the application's own applied list.
#[tokio::test(flavor = "current_thread")]
async fn parked_data_lane_wake() {
    let latch = Latch::default();
    let app = ScriptApp {
        on_msg: Box::new(inert),
        sources: no_subs(),
    };
    let mut kernel = Kernel::new(
        app,
        (want(&[]), latched_sender("feed", latch.clone(), vec![5])),
        KernelConfig::default(),
        GateMode::Immediate,
        HeadlessHost::default(),
    );
    let delivery = kernel.delivery();
    let gauges = kernel.gauges();
    let probe = ParkProbe::new();
    let mut run = Box::pin(kernel.run());

    assert!(
        probe.poll(run.as_mut()).is_pending(),
        "the production loop suspends after the boot pass"
    );
    assert!(delivery.contains("render"), "the boot pass rendered");
    let parked_at = probe
        .assert_parked_across_turns(run.as_mut(), &delivery, "after the boot pass")
        .await;
    assert_eq!(parked_at, 0, "nothing has woken the loop yet");
    assert_eq!(gauges.producers(), 1, "the feed producer is the only run");
    let parked_ledger = delivery.snapshot();

    // The one event: a message reaches the data lane. `accept:feed:5` is
    // that send's enqueue acceptance — the handshake this witness
    // synchronizes on, and the only entry the ledger gains.
    latch.open();
    settle_until(
        || delivery.contains("accept:feed:5"),
        "the message is accepted by the data lane",
    )
    .await;
    let arrival = delivery.snapshot();
    let since_park: Vec<&str> = arrival[parked_ledger.len()..]
        .iter()
        .map(String::as_str)
        .collect();
    assert_eq!(
        since_park,
        vec!["accept:feed:5"],
        "the data-lane acceptance is the only thing that happened: {arrival:?}"
    );
    assert_eq!(
        gauges.producers(),
        1,
        "the sender is still live: no task exit stands in for this wake"
    );
    assert_eq!(
        probe.wakes(),
        parked_at + 1,
        "the data-lane arrival alone woke the parked loop, with exactly \
         one signal"
    );

    assert!(
        probe.poll(run.as_mut()).is_pending(),
        "the woken pass runs the input batch"
    );
    assert!(
        delivery.contains("update:5"),
        "the woken pass delivered the message to the reducer"
    );
    assert_eq!(
        gauges.producers(),
        1,
        "the sender outlives its own delivery"
    );
    probe.assert_parked(run.as_mut(), &delivery, "after the woken pass");

    drop(run);
    assert_eq!(
        kernel.state().applied,
        vec![5],
        "the reducer applied the woken pass's message"
    );
}

// --- claim probes ----------------------------------------------------------

/// §2.1 rule 0 — the grant await precedes the reservation: a producer
/// aborted while parked at the gate holds nothing, so the counter shows
/// no residue and the entry is removed without Draining.
#[tokio::test]
async fn gate_wait_abort_leaves_no_reservation_residue() {
    let input = MockSource::default();
    let app = ScriptApp {
        on_msg: Box::new(|_, msg| {
            if msg == 2 {
                Cmd::Teardown(ScopePath::seg("pane"))
            } else {
                Cmd::None
            }
        }),
        sources: BTreeMap::from([("input", input.clone())]),
    };
    let init = Cmd::Spawn(SpawnCmd {
        label: "gw",
        scope: ScopePath::seg("pane"),
        key: None,
        policy: CancelPolicy::CancelInFlight,
        body: body(|ctx| async move {
            let _ = ctx.handle.send("1", 1).await;
        }),
    });
    let mut driver = driver(app, want(&["input"]), init);
    driver.boot();
    let input_id = driver.producer("input");
    driver.await_intents("gw", 1).await;
    let counter = driver.counter_of("gw");
    assert_eq!(
        counter.value(),
        0,
        "gate first: no reservation while waiting"
    );

    input.push(2);
    driver.grant(input_id).await;
    driver
        .step_pass(Branch::Input)
        .expect("teardown the gate waiter");
    driver.await_exit_ready().await;
    driver
        .step_pass(Branch::JoinExit)
        .expect("observe the abort");
    assert_eq!(driver.entry_phase("gw"), None, "removed without Draining");
    assert_eq!(counter.value(), 0, "no counter residue from the gate wait");
    assert!(
        driver.intents().contains("intent:gw:1"),
        "the pre-gate intent was recorded on the intent ledger"
    );
    assert!(
        !driver.delivery().contains("accept:gw:1"),
        "nothing enqueued"
    );
}

/// §2.1 rule 6 — reaching saturation poisons the counter: decrements
/// freeze, the entry is never removed by the steady-state rules, and only
/// termination reclaims it; delivery is unaffected (never stale).
#[tokio::test]
async fn saturated_counter_poisons_and_defers_removal_to_termination() {
    let input = MockSource::default();
    let app = ScriptApp {
        on_msg: Box::new(|_, msg| if msg == 7 { Cmd::Quit } else { Cmd::None }),
        sources: BTreeMap::from([("input", input.clone())]),
    };
    let init = send_then_finish("px", vec![1, 2]);
    let mut driver = driver(app, want(&["input"]), init);
    driver.boot();
    let input_id = driver.producer("input");
    driver.await_intents("px", 1).await;
    let counter = driver.counter_of("px");
    counter.preset_for_test(u32::MAX - 1);

    let px = driver.producer("px");
    driver.grant(px).await;
    assert!(counter.is_poisoned(), "saturation freezes the counter");
    driver.grant(px).await;

    driver.await_exit_ready().await;
    driver
        .step_pass(Branch::JoinExit)
        .expect("poisoned pass: exit reflected, then deliveries proceed");
    assert_eq!(driver.state().applied, vec![1, 2], "no stale side effects");
    assert_eq!(
        driver.entry_phase("px"),
        Some((Phase::Draining, false)),
        "steady-state removal is disabled for a poisoned counter"
    );

    input.push(7);
    driver.grant(input_id).await;
    driver.step_pass(Branch::Input).expect("terminate");
    let report = driver.settle().await;
    assert!(
        report.gauges_zero,
        "termination reclaims the poisoned entry"
    );
}

/// Uniform barrier scope — a stopping *command* run never defers
/// subscription admission; a stopping *subscription* run defers every
/// admission, related or not, until its quiescence.
#[tokio::test]
async fn barrier_covers_subscription_runs_only_and_is_runtime_wide() {
    let input = MockSource::default();
    let a_src = MockSource::default();
    let b_src = MockSource::default();
    let app = ScriptApp {
        on_msg: Box::new(|state, msg| match msg {
            1 => Cmd::Spawn(SpawnCmd {
                label: "fetch",
                scope: ScopePath::seg("work"),
                key: Some("k"),
                policy: CancelPolicy::CancelInFlight,
                body: body(|_ctx| pending::<()>()),
            }),
            2 => {
                state.want.insert("suba");
                Cmd::Teardown(ScopePath::seg("work"))
            }
            3 => {
                state.want.remove("suba");
                state.want.insert("subb");
                Cmd::None
            }
            _ => Cmd::None,
        }),
        sources: BTreeMap::from([
            ("input", input.clone()),
            ("suba", a_src.clone()),
            ("subb", b_src.clone()),
        ]),
    };
    let mut driver = driver(app, want(&["input"]), Cmd::None);
    driver.boot();
    let input_id = driver.producer("input");

    input.push(1);
    driver.grant(input_id).await;
    driver
        .step_pass(Branch::Input)
        .expect("spawn the command run");
    input.push(2);
    driver.grant(input_id).await;
    driver
        .step_pass(Branch::Input)
        .expect("teardown + declare suba in one pass");
    assert_eq!(
        driver.entry_phase("fetch"),
        Some((Phase::Stopping, true)),
        "command run is stopping, unquiesced"
    );
    assert!(
        driver.delivery().contains("admit:suba"),
        "a stopping command run does not defer subscription admission"
    );

    input.push(3);
    driver.grant(input_id).await;
    driver.step_pass(Branch::Input).expect("stop pass defers");
    assert!(
        !driver.delivery().contains("admit:subb"),
        "a stopping subscription run defers unrelated admission"
    );

    for _ in 0..4 {
        if driver.delivery().contains("admit:subb") {
            break;
        }
        driver.await_exit_ready().await;
        driver
            .step_pass(Branch::JoinExit)
            .expect("reflect exits and re-evaluate");
    }
    let entries = driver.delivery().snapshot();
    assert!(
        position_of(&entries, "exit:suba") < position_of(&entries, "admit:subb"),
        "admission resumes only after the subscription quiesced: {entries:?}"
    );
}

// Scoped-child fixture: a root reducer with its own input subscription, a
// child reducer with a keyed effect and a state-gated subscription, glued
// by the one-level `ScopedChild` combinator under the `child` segment.
#[derive(Default)]
struct ParentState {
    child: ChildState,
    applied: Vec<u32>,
}

#[derive(Default)]
struct ChildState {
    active: bool,
}

struct ChildCore {
    src: MockSource<u32>,
}

impl Reducer for ChildCore {
    type State = ChildState;
    type Msg = u32;
    fn reduce(&self, state: &mut ChildState, msg: u32) -> Cmd<u32> {
        if msg == 5 {
            state.active = true;
            Cmd::Spawn(SpawnCmd {
                label: "child-fx",
                scope: ScopePath::root(),
                key: Some("ck"),
                policy: CancelPolicy::CancelInFlight,
                body: body(|_ctx| pending::<()>()),
            })
        } else {
            Cmd::None
        }
    }
    fn subscriptions(&self, state: &ChildState) -> Vec<SubDecl<u32>> {
        if state.active {
            vec![SubDecl::new("child-sub", &self.src)]
        } else {
            Vec::new()
        }
    }
}

struct WithInput {
    input: MockSource<u32>,
}

impl Reducer for WithInput {
    type State = ParentState;
    type Msg = u32;
    fn reduce(&self, state: &mut ParentState, msg: u32) -> Cmd<u32> {
        state.applied.push(msg);
        if msg == 6 {
            state.child.active = false;
            Cmd::Teardown(ScopePath::seg("child"))
        } else {
            Cmd::None
        }
    }
    fn subscriptions(&self, _state: &ParentState) -> Vec<SubDecl<u32>> {
        vec![SubDecl::new("input", &self.input)]
    }
}

struct ComposedApp(ScopedChild<WithInput, ChildCore>);

impl Reducer for ComposedApp {
    type State = ParentState;
    type Msg = u32;
    fn reduce(&self, state: &mut ParentState, msg: u32) -> Cmd<u32> {
        self.0.reduce(state, msg)
    }
    fn subscriptions(&self, state: &ParentState) -> Vec<SubDecl<u32>> {
        self.0.subscriptions(state)
    }
}

impl Program for ComposedApp {
    type Flags = ();
    fn init(&self, (): ()) -> (ParentState, Cmd<u32>) {
        (ParentState::default(), Cmd::None)
    }
    fn view(&self, _state: &ParentState, _sink: &mut ViewSink) {}
}

/// Parent-child (one level) — a teardown of the child boundary selects
/// exactly the child's runs (keyed effect + subscription), leaves root
/// runs untouched, and the paired declaration removal keeps the successor
/// from restarting.
#[tokio::test]
async fn scoped_child_teardown_selects_only_child_runs() {
    let input = MockSource::default();
    let child_src = MockSource::default();
    let app = ComposedApp(ScopedChild {
        parent: WithInput {
            input: input.clone(),
        },
        child: ChildCore {
            src: child_src.clone(),
        },
        seg: "child",
        lens: |s| &mut s.child,
        lens_ref: |s| &s.child,
        route: |m| *m == 5,
    });
    let mut driver = TestDriver::new(app, (), KernelConfig::default());
    driver.boot();
    let input_id = driver.producer("input");

    input.push(5);
    driver.grant(input_id).await;
    driver
        .step_pass(Branch::Input)
        .expect("child spawn via routing; the same pass admits the scoped sub");
    assert!(
        driver.delivery().contains("admit:child-sub"),
        "child sub up"
    );

    input.push(6);
    driver.grant(input_id).await;
    driver
        .step_pass(Branch::Input)
        .expect("parent tears the child down");
    let delivery = driver.delivery();
    assert!(
        delivery.contains("stop:child-fx"),
        "child keyed run selected"
    );
    assert!(delivery.contains("stop:child-sub"), "child sub selected");
    assert_eq!(
        driver.entry_phase("input"),
        Some((Phase::Running, false)),
        "root-scoped run untouched (selection isolation)"
    );

    for _ in 0..2 {
        if driver.registry_len() == 1 {
            break;
        }
        driver.await_exit_ready().await;
        driver
            .step_pass(Branch::JoinExit)
            .expect("reflect child exits and re-evaluate");
    }
    assert_eq!(driver.registry_len(), 1, "both child runs reclaimed");
    assert_eq!(
        driver.delivery().count("admit:child-sub"),
        1,
        "declaration removal pairs with teardown: no restart"
    );
}

/// Construction is inert (B-1): no task, no gauge movement, nothing
/// observable before boot; never-run drop leaves both postconditions
/// vacuously true.
#[tokio::test]
async fn construction_is_inert_and_never_run_drop_is_clean() {
    let app = ScriptApp {
        on_msg: Box::new(inert),
        sources: no_subs(),
    };
    let driver = driver(app, want(&[]), send_then_park("never", vec![1]));
    let gauges = driver.gauges();
    let delivery = driver.delivery();
    assert_eq!(gauges.producers(), 0, "no producer before boot");
    assert!(delivery.snapshot().is_empty(), "no observation before boot");
    drop(driver);
    assert_eq!(gauges.producers(), 0, "never-run drop spawns nothing");
}

/// Dirt boundary (RFC 0014 §5.1) — the exit of a *stopped command* run
/// never wakes the frame pass: only the quiescence of a stopped
/// subscription run marks dirt (the positive case is pinned by S7 and the
/// barrier test; the natural-finish case by the S7 contrast test).
#[tokio::test]
async fn stopped_command_run_exit_does_not_wake_the_frame_pass() {
    let input = MockSource::default();
    let app = ScriptApp {
        on_msg: Box::new(|_, msg| match msg {
            1 => Cmd::Spawn(SpawnCmd {
                label: "fetch",
                scope: ScopePath::seg("work"),
                key: Some("k"),
                policy: CancelPolicy::CancelInFlight,
                body: body(|_ctx| pending::<()>()),
            }),
            2 => Cmd::Teardown(ScopePath::seg("work")),
            _ => Cmd::None,
        }),
        sources: BTreeMap::from([("input", input.clone())]),
    };
    let mut driver = driver(app, want(&["input"]), Cmd::None);
    driver.boot();
    let input_id = driver.producer("input");
    driver
        .step_pass(Branch::Frame)
        .expect("consume the boot redraw");

    input.push(1);
    driver.grant(input_id).await;
    driver
        .step_pass(Branch::Input)
        .expect("spawn the command run");
    input.push(2);
    driver.grant(input_id).await;
    driver
        .step_pass(Branch::Input)
        .expect("stop the command run");
    assert_eq!(
        driver.step_pass(Branch::Frame),
        Err(StepError::NotReady(Branch::Frame)),
        "nothing pending before the exit"
    );
    let reconciles = driver.delivery().count("reconcile");

    driver.await_exit_ready().await;
    driver
        .step_pass(Branch::JoinExit)
        .expect("observe the stopped command exit");
    assert!(
        driver.delivery().contains("exit:fetch:Cancelled"),
        "the stopped command run exited"
    );
    assert_eq!(
        driver.step_pass(Branch::Frame),
        Err(StepError::NotReady(Branch::Frame)),
        "a stopped command run's quiescence leaves no dirt"
    );
    assert_eq!(
        driver.delivery().count("reconcile"),
        reconciles,
        "no re-evaluation was triggered by the command exit"
    );
}

/// Keyed replacement policies — `CancelInFlight` revokes the occupant and
/// starts fresh; `KeepInFlight` suppresses the new spawn; a Stopping
/// occupant does not hold the slot (fresh-slot rule).
#[tokio::test]
async fn keyed_slot_policies_replace_or_suppress() {
    let input = MockSource::default();
    let app = ScriptApp {
        on_msg: Box::new(|_, msg| {
            let policy = if msg == 2 {
                CancelPolicy::KeepInFlight
            } else {
                CancelPolicy::CancelInFlight
            };
            match msg {
                1..=3 => Cmd::Spawn(SpawnCmd {
                    label: "fetch",
                    scope: ScopePath::root(),
                    key: Some("k"),
                    policy,
                    body: body(|_ctx| pending::<()>()),
                }),
                _ => Cmd::None,
            }
        }),
        sources: BTreeMap::from([("input", input.clone())]),
    };
    let mut driver = driver(app, want(&["input"]), Cmd::None);
    driver.boot();
    let input_id = driver.producer("input");

    input.push(1);
    driver.grant(input_id).await;
    driver.step_pass(Branch::Input).expect("first keyed spawn");
    assert_eq!(driver.delivery().count("spawn:fetch:t2"), 1, "occupant up");

    input.push(2);
    driver.grant(input_id).await;
    driver
        .step_pass(Branch::Input)
        .expect("keep-in-flight spawn");
    assert!(
        driver.delivery().contains("suppress:fetch"),
        "KeepInFlight suppresses the new spawn"
    );
    assert_eq!(
        driver.entry_phase("fetch"),
        Some((Phase::Running, false)),
        "the occupant is untouched"
    );

    input.push(3);
    driver.grant(input_id).await;
    driver
        .step_pass(Branch::Input)
        .expect("cancel-in-flight spawn");
    assert!(
        driver.delivery().contains("replace:fetch"),
        "CancelInFlight revokes the occupant"
    );
    assert_eq!(
        driver.entry_phase("fetch"),
        Some((Phase::Running, false)),
        "the successor holds the slot fresh"
    );
    let stopping = driver.delivery().count("stop:fetch");
    assert_eq!(stopping, 0, "replacement is logged as replace, not stop");
}

/// Grant sequencing (the bounded-API redesign) — per-origin outstanding
/// grants are capped at one, each acknowledgement waits for its own
/// grant's exact commit, and the next grant is only issuable after the
/// previous acceptance exists: two handles can never alias one commit.
#[tokio::test]
async fn grant_handles_do_not_alias_acceptances() {
    let app = ScriptApp {
        on_msg: Box::new(inert),
        sources: no_subs(),
    };
    let init = send_then_park("pa", vec![1, 2]);
    let mut driver = driver(app, want(&[]), init);
    driver.boot();
    driver.await_intents("pa", 1).await;
    let pa = driver.producer("pa");

    let first = driver.grant_handle(pa).expect("first grant issues");
    assert!(
        matches!(driver.grant_handle(pa), Err(GrantOutstanding)),
        "a second grant is refused while the first is unacknowledged"
    );
    first.await;
    assert_eq!(
        driver.delivery().count("accept:pa:1"),
        1,
        "first acceptance"
    );

    let mut second = driver
        .grant_handle(pa)
        .expect("the next grant issues once the acceptance exists");
    assert!(
        (&mut second).now_or_never().is_none(),
        "the second acknowledgement waits for its own commit, not the first"
    );
    second.await;
    assert_eq!(
        driver.delivery().count("accept:pa:2"),
        1,
        "second acceptance"
    );
    driver.step_pass(Branch::Input).expect("both deliveries");
    assert_eq!(driver.state().applied, vec![1, 2], "grant order held");
}

// --- bounded commit-ack evaluation -----------------------------------------

#[expect(
    clippy::future_not_send,
    reason = "current-thread test helper; the driver never crosses threads"
)]
async fn bounded_headroom_run(first: &str, second: &str) -> Vec<String> {
    let app = ScriptApp {
        on_msg: Box::new(inert),
        sources: no_subs(),
    };
    let init = Cmd::Batch(vec![
        send_then_park("pa", vec![100]),
        send_then_park("pb", vec![200]),
    ]);
    let mut driver = TestDriver::new(
        app,
        (want(&[]), init),
        KernelConfig {
            capacity: Some(8),
            ..KernelConfig::default()
        },
    );
    driver.boot();
    driver.await_intents("pa", 1).await;
    driver.await_intents("pb", 1).await;
    let first_id = driver.producer(first);
    let second_id = driver.producer(second);
    driver.grant(first_id).await;
    driver.grant(second_id).await;
    driver
        .step_pass(Branch::Input)
        .expect("drain both in one pass");
    driver.delivery().snapshot()
}

/// Bounded evaluation (1) — with capacity headroom, the commit-ack
/// handshake behaves exactly as in the unbounded series: script-ordered,
/// replay-identical.
#[tokio::test]
async fn bounded_headroom_keeps_the_grant_handshake_deterministic() {
    let ab_one = bounded_headroom_run("pa", "pb").await;
    let ab_two = bounded_headroom_run("pa", "pb").await;
    assert_eq!(ab_one, ab_two, "replay identical under a bounded lane");
    let ba = bounded_headroom_run("pb", "pa").await;
    assert!(
        position_of(&ab_one, "update:100") < position_of(&ab_one, "update:200"),
        "script order holds under a bounded lane"
    );
    assert!(
        position_of(&ba, "update:200") < position_of(&ba, "update:100"),
        "reversed script reverses delivery under a bounded lane"
    );
}

#[expect(
    clippy::future_not_send,
    reason = "current-thread test helper; the driver never crosses threads"
)]
async fn bounded_full_lane_run() -> Vec<String> {
    let app = ScriptApp {
        on_msg: Box::new(inert),
        sources: no_subs(),
    };
    let init = Cmd::Batch(vec![
        send_then_finish("pa", vec![1]),
        send_then_finish("pb", vec![2]),
    ]);
    let mut driver = TestDriver::new(
        app,
        (want(&[]), init),
        KernelConfig {
            capacity: Some(1),
            ..KernelConfig::default()
        },
    );
    driver.boot();
    driver.await_intents("pa", 1).await;
    driver.await_intents("pb", 1).await;
    let pa = driver.producer("pa");
    let pb = driver.producer("pb");
    driver.grant(pa).await;

    let pb_counter = driver.counter_of("pb");
    let mut ack = driver.grant_handle(pb).expect("pb grant issues");
    settle_until(
        || pb_counter.value() == 1,
        "pb parks inside the real send holding its reservation",
    )
    .await;
    assert!(
        (&mut ack).now_or_never().is_none(),
        "no acceptance before capacity frees"
    );

    driver
        .step_pass(Branch::Input)
        .expect("drain one; capacity frees");
    ack.await;
    driver.step_pass(Branch::Input).expect("drain the second");
    driver.delivery().snapshot()
}

/// Bounded evaluation (2) — with the lane full, the grant acknowledges
/// only after the real send commits (capacity freed by a dequeue): the
/// acceptance signal stays truthful and the interleaving is
/// deterministic, but the spec's exclusive-borrow `grant(&mut self)`
/// cannot express this scenario (it would deadlock) — recorded as an API
/// finding for the C-2 bounded extension.
#[tokio::test]
async fn bounded_full_lane_grant_acks_only_after_real_acceptance() {
    let one = bounded_full_lane_run().await;
    let two = bounded_full_lane_run().await;
    assert_eq!(one, two, "replay identical under a full bounded lane");
    assert!(
        position_of(&one, "update:1") < position_of(&one, "accept:pb:2"),
        "the second acceptance happens only after the first dequeue: {one:?}"
    );
}

/// "bounded-lane revocation" — strict revocation witnessed on the
/// **bounded** lane mode, driven pass-unit. The buffered acceptance is
/// established by the commit-ack handshake: `grant` returns only once
/// the real bounded `send().await` has been accepted into the lane's
/// buffer, and the two-slot lane is exactly full at that point, which a
/// third producer's send confirms by parking on the missing capacity
/// (no acceptance, reservation held). The teardown pass then revokes the
/// buffered item's origin; the revoked envelope keeps holding its lane
/// slot — capacity is reclaimed by the delivery-side dequeue, not by the
/// revocation — and when the drain reaches it the item is filtered:
/// **no `update` invocation**, while the live origin's item queued
/// behind it still delivers. Every step is a handshake or a condition
/// settle; no yield count or sleep is a correctness condition.
#[tokio::test]
async fn bounded_lane_revocation() {
    let input = MockSource::default();
    let app = ScriptApp {
        on_msg: Box::new(|_, msg| match msg {
            1 => Cmd::Spawn(SpawnCmd {
                label: "fx",
                scope: ScopePath::seg("pane"),
                key: Some("k"),
                policy: CancelPolicy::CancelInFlight,
                body: body(|ctx| async move {
                    let _ = ctx.handle.send("10", 10).await;
                    pending::<()>().await;
                }),
            }),
            2 => Cmd::Teardown(ScopePath::seg("pane")),
            3 => Cmd::Quit,
            _ => Cmd::None,
        }),
        sources: BTreeMap::from([("input", input.clone())]),
    };
    let mut driver = TestDriver::new(
        app,
        (want(&["input"]), send_then_park("pb", vec![20])),
        KernelConfig {
            capacity: Some(2),
            batch_cap: 1,
        },
    );
    driver.boot();
    let input_id = driver.producer("input");
    let pb = driver.producer("pb");

    input.push(1);
    driver.grant(input_id).await;
    driver.step_pass(Branch::Input).expect("spawn pass");
    let fx = driver.producer("fx");

    // The cancel trigger enqueues *before* fx's output: FIFO carries
    // [input:2, fx:10] into the next passes (cap 1). Both grants return
    // only after their bounded send was accepted into the lane, so the
    // two-slot lane is exactly full here.
    input.push(2);
    driver.grant(input_id).await;
    driver.grant(fx).await;
    assert_eq!(
        driver.counter_of("fx").value(),
        1,
        "fx's item is committed into the bounded lane"
    );

    // Capacity is genuinely exhausted: a third producer's send parks
    // inside the real bounded send holding its reservation, and its
    // grant does not acknowledge.
    let pb_counter = driver.counter_of("pb");
    let mut pb_ack = driver.grant_handle(pb).expect("pb's grant issues");
    settle_until(
        || pb_counter.value() == 1,
        "pb parks inside the real send holding its reservation",
    )
    .await;
    assert!(
        (&mut pb_ack).now_or_never().is_none(),
        "the bounded lane is full: no acceptance for pb"
    );

    driver.step_pass(Branch::Input).expect("teardown pass");
    assert!(
        driver.delivery().contains("stop:fx"),
        "the teardown revoked the buffered item's origin"
    );
    assert_eq!(
        driver.entry_phase("fx"),
        Some((Phase::Stopping, true)),
        "fx is revoked with its item still buffered"
    );
    pb_ack.await;
    assert_eq!(
        driver.counter_of("fx").value(),
        1,
        "the revoked item still holds its lane slot: revocation frees no capacity"
    );

    driver.await_exit_ready().await;
    driver.step_pass(Branch::JoinExit).expect("filter pass");
    let entries = driver.delivery().snapshot();
    assert!(
        entries.iter().any(|e| e == "filtered:fx"),
        "the buffered item is filtered at dequeue: {entries:?}"
    );
    assert!(
        !entries.iter().any(|e| e == "update:10"),
        "no update invocation for the revoked origin: {entries:?}"
    );
    assert_eq!(
        driver.entry_phase("fx"),
        None,
        "the tombstone is reclaimed by the filtering dequeue"
    );

    driver
        .step_pass(Branch::Input)
        .expect("the live origin's item queued behind still delivers");
    input.push(3);
    driver.grant(input_id).await;
    driver.step_pass(Branch::Input).expect("quit pass");
    let report = driver.settle().await;
    assert_eq!(report.reason, ExitReason::Quit, "controlled quit");
    assert!(report.gauges_zero, "quiescent postcondition");
    assert_eq!(
        driver.state().applied,
        vec![1, 2, 20, 3],
        "only live deliveries, on a bounded lane"
    );
}

// --- INV-RC9 bound rows (fixed-pass bounds under flood) ---------------------

/// INV-RC9 pass-start row — a quit already arrived at pass start wins
/// over a full ready input batch: the mandatory control drain precedes
/// the input stage, so the quit applies with **zero** inputs processed —
/// even though the first queued input would have torn the quitter's
/// scope down ("an input that could have cancelled the quit's origin
/// never precedes it").
#[tokio::test]
async fn quit_ready_at_pass_start_wins_over_ready_input() {
    let app = ScriptApp {
        on_msg: Box::new(|_, msg| {
            if msg == 9 {
                Cmd::Teardown(ScopePath::seg("quit"))
            } else {
                Cmd::None
            }
        }),
        sources: no_subs(),
    };
    let init = Cmd::Batch(vec![
        send_then_park("flood", vec![9, 1, 2, 3, 4]),
        Cmd::Spawn(SpawnCmd {
            label: "quitter",
            scope: ScopePath::seg("quit"),
            key: None,
            policy: CancelPolicy::CancelInFlight,
            body: body(|ctx| async move {
                let _ = ctx.handle.quit().await;
                pending::<()>().await;
            }),
        }),
    ]);
    let mut driver = TestDriver::new(
        app,
        (want(&[]), init),
        KernelConfig {
            batch_cap: 2,
            ..KernelConfig::default()
        },
    );
    driver.boot();
    driver.await_intents("flood", 1).await;
    let flood = driver.producer("flood");
    for _ in 0..5 {
        driver.grant(flood).await;
    }
    driver.await_intents("quitter", 1).await;
    let quitter = driver.producer("quitter");
    driver.grant(quitter).await;

    driver.step_pass(Branch::Input).expect("one fixed pass");
    assert!(
        driver.state().applied.is_empty(),
        "the quit wins at pass start: zero inputs processed"
    );
    assert!(
        driver.delivery().contains("quit:quitter"),
        "applied at the first control drain at or after arrival"
    );
    assert!(driver.delivery().contains("terminating:Quit"), "terminated");
    let report = driver.settle().await;
    assert!(report.gauges_zero, "flood reclaimed");
}

/// INV-RC9 mid-batch row — the quit's send commits *during* the
/// in-progress input batch (a true mid-pass arrival, witnessed on the
/// ledger between update:1 and update:2): only the in-progress batch's
/// remainder (<= cap) precedes it, and the quit is applied at the next
/// pass's control drain — no further batch starts. Runs on two workers
/// with the mid-batch handshake; determinism is by application-side
/// synchronization.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quit_arriving_mid_batch_applies_at_the_next_pass_start() {
    let (handshake, open_flag, committed_tx) = MidBatchHandshake::new();
    let app = ScriptApp {
        on_msg: Box::new(move |_, msg| {
            if msg == 1 {
                handshake.open_and_await_commit();
            }
            Cmd::None
        }),
        sources: no_subs(),
    };
    let init = Cmd::Batch(vec![
        send_then_park("flood", vec![1, 2, 3, 4]),
        gated_quitter("quitter", ScopePath::root(), open_flag, committed_tx),
    ]);
    let mut driver = TestDriver::new(
        app,
        (want(&[]), init),
        KernelConfig {
            batch_cap: 2,
            ..KernelConfig::default()
        },
    );
    driver.boot();
    driver.await_intents("flood", 1).await;
    let flood = driver.producer("flood");
    for _ in 0..4 {
        driver.grant(flood).await;
    }
    let quitter = driver.producer("quitter");
    let ack = driver
        .grant_handle(quitter)
        .expect("bank the quit allowance");

    driver
        .step_pass(Branch::Input)
        .expect("the in-progress batch");
    ack.await;
    let entries = driver.delivery().snapshot();
    assert!(
        position_of(&entries, "update:1") < position_of(&entries, "accept:quitter:quit"),
        "the quit arrived after the batch started: {entries:?}"
    );
    assert!(
        position_of(&entries, "accept:quitter:quit") < position_of(&entries, "update:2"),
        "the quit arrived mid-batch, before the batch's remainder: {entries:?}"
    );
    assert_eq!(
        driver.state().applied,
        vec![1, 2],
        "only the in-progress batch's remainder precedes the quit"
    );

    driver.step_pass(Branch::Control).expect("the next pass");
    assert!(
        driver.delivery().contains("quit:quitter"),
        "applied at the first control drain at or after arrival"
    );
    assert!(
        !driver.delivery().contains("update:3"),
        "the next batch never starts"
    );
    let report = driver.settle().await;
    assert!(report.gauges_zero, "flood reclaimed");
}

/// INV-RC9 render row — the redraw a batch raises renders before the
/// next batch starts: with cap 2 and four queued messages, each fixed
/// pass ends in exactly one render, so the flood cannot suppress
/// rendering.
#[tokio::test]
async fn flood_cannot_suppress_render_between_batches() {
    let app = ScriptApp {
        on_msg: Box::new(inert),
        sources: no_subs(),
    };
    let init = send_then_park("flood", vec![1, 2, 3, 4]);
    let mut driver = TestDriver::new(
        app,
        (want(&[]), init),
        KernelConfig {
            batch_cap: 2,
            ..KernelConfig::default()
        },
    );
    driver.boot();
    driver.await_intents("flood", 1).await;
    let flood = driver.producer("flood");
    for _ in 0..4 {
        driver.grant(flood).await;
    }

    driver.step_pass(Branch::Input).expect("first fixed pass");
    driver.step_pass(Branch::Input).expect("second fixed pass");
    let entries = driver.delivery().snapshot();
    assert_eq!(driver.state().applied, vec![1, 2, 3, 4], "both batches ran");
    assert_eq!(
        entries.iter().filter(|e| *e == "render").count(),
        2,
        "one render per pass: {entries:?}"
    );
    let second_batch = position_of(&entries, "update:3");
    assert!(
        entries[position_of(&entries, "update:2")..second_batch]
            .iter()
            .any(|e| e == "render"),
        "the first batch's redraw renders before the next batch: {entries:?}"
    );
}

// --- production loop smoke --------------------------------------------------

/// The production loop (immediate gate, same branch executors) runs the
/// same program to a controlled quit — the same-implementation half of
/// the same-topology claim, smoke-tested.
#[tokio::test]
async fn production_loop_runs_the_same_executors_to_quit() {
    let input = MockSource::default();
    input.push(1);
    input.push(2);
    input.push(7);
    let app = ScriptApp {
        on_msg: Box::new(|_, msg| if msg == 7 { Cmd::Quit } else { Cmd::None }),
        sources: BTreeMap::from([("input", input.clone())]),
    };
    let mut kernel = Kernel::new(
        app,
        (want(&["input"]), Cmd::None),
        KernelConfig::default(),
        GateMode::Immediate,
        HeadlessHost::default(),
    );
    let report = kernel.run().await;
    assert_eq!(
        kernel.state().applied,
        vec![1, 2, 7],
        "all messages applied"
    );
    assert_eq!(report.reason, ExitReason::Quit, "controlled quit");
    assert_eq!(report.joined, 1, "the input forwarder was joined at settle");
    assert!(report.gauges_zero, "quiescent postcondition");
}
