//! The pack §H-5 acceptance series (S1-S8) executed on the B-kernel
//! prototype, plus the claim-specific probes (reservation protocol,
//! gate-wait abort, counter poisoning, barrier scope, bounded commit-ack
//! evaluation) and a production-loop smoke test. All tests are
//! deterministic on the current-thread test executor: no sleeps, no
//! timers; progress is driven by grant handshakes and bounded yield
//! settles.

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::FutureExt;
use futures::future::pending;
use tokio::sync::mpsc;

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

/// S1 — after teardown revokes the origin, its already-buffered data
/// message and its buffered control-lane quit are both filtered at
/// dequeue; the tombstone survives as Draining until the last committed
/// envelope drains, and is reclaimed at that dequeue.
#[tokio::test]
async fn s1_cancel_beats_buffered_message_and_buffered_quit() {
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
                    let _ = ctx.handle.quit().await;
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
    driver.step(Branch::Input).expect("deliver spawn trigger");
    let fx = driver.producer("fx");

    // The cancel trigger enqueues *before* fx's output: FIFO then carries
    // [input:2, fx:10] on the data lane and [fx:quit] on the control lane.
    input.push(2);
    driver.grant(input_id).await;
    driver.grant(fx).await;
    driver.grant(fx).await;

    driver
        .step(Branch::Input)
        .expect("deliver teardown trigger");
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
    driver.step(Branch::JoinExit).expect("observe fx exit");
    assert_eq!(
        driver.entry_phase("fx"),
        Some((Phase::Draining, true)),
        "exited with committed pending stays as a Draining tombstone"
    );

    driver.step(Branch::Input).expect("drain buffered message");
    assert_eq!(
        driver.entry_phase("fx"),
        Some((Phase::Draining, true)),
        "tombstone survives while a committed envelope remains"
    );
    driver.step(Branch::Control).expect("drain buffered quit");
    assert_eq!(
        driver.entry_phase("fx"),
        None,
        "tombstone reclaimed at the dequeue that zeroed committed pending"
    );
    assert_eq!(driver.registry_len(), 1, "only the input forwarder remains");

    let delivery = driver.delivery();
    assert!(delivery.contains("filtered:fx"), "buffered message dropped");
    assert!(
        delivery.contains("filtered-quit:fx"),
        "buffered quit dropped"
    );
    assert!(!delivery.contains("update:10"), "no stale delivery");
    assert!(!delivery.contains("quit:fx"), "revoked quit does not quit");

    input.push(3);
    driver.grant(input_id).await;
    driver.step(Branch::Input).expect("deliver quit trigger");
    let report = driver.settle().await;
    assert_eq!(report.reason, ExitReason::Quit, "controlled quit");
    assert!(report.gauges_zero, "quiescent postcondition");
    assert_eq!(
        driver.state().applied,
        vec![1, 2, 3],
        "only live deliveries"
    );
}

// --- S2: stop/restart safety window ---------------------------------------

/// S2 — the re-evaluation that issues a stop admits nothing, even when
/// the stopped task has already exited (unobserved); the successor is
/// admitted only after the exit is reflected through `JoinExit`.
#[tokio::test]
async fn s2_stop_issuing_pass_defers_even_when_the_target_already_exited() {
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

/// S2 — removing and re-adding the same identity: the successor waits out
/// the predecessor's quiescence (the safe window), then restarts fresh.
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
    let mut driver = driver(app, want(&["input", "suba"]), Cmd::None);
    driver.boot();
    let input_id = driver.producer("input");

    input.push(1);
    driver.grant(input_id).await;
    driver.step(Branch::Input).expect("deliver removal");
    driver.step(Branch::Frame).expect("stop pass");
    assert!(driver.delivery().contains("stop:suba"), "stop issued");

    input.push(2);
    driver.grant(input_id).await;
    driver.step(Branch::Input).expect("deliver re-add");
    driver.step(Branch::Frame).expect("barrier pass");
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
    driver.step(Branch::JoinExit).expect("reflect quiescence");
    driver.step(Branch::Frame).expect("admission pass");
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
    driver.step(Branch::Input).expect("drain both");
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
async fn s3_branch_face(join_first: bool) -> Vec<String> {
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
    driver.await_intents("pf", 1).await;
    let pf = driver.producer("pf");
    driver.grant(pf).await;
    driver.await_exit_ready().await;

    assert!(driver.ready(Branch::Input), "data lane ready");
    assert!(driver.ready(Branch::JoinExit), "join exit ready");
    if join_first {
        driver.step(Branch::JoinExit).expect("scripted join first");
        driver.step(Branch::Input).expect("then input");
    } else {
        driver.step(Branch::Input).expect("scripted input first");
        driver.step(Branch::JoinExit).expect("then join");
    }
    driver.delivery().snapshot()
}

/// S3 (kernel-branch face) — with Input and `JoinExit` simultaneously
/// ready, the script picks the order; replays are identical and the two
/// scripts observably differ.
#[tokio::test]
async fn s3_scripted_arbitration_orders_ready_branches_and_replays() {
    let join_one = s3_branch_face(true).await;
    let join_two = s3_branch_face(true).await;
    assert_eq!(join_one, join_two, "replay identical (join first)");
    let input_one = s3_branch_face(false).await;
    let input_two = s3_branch_face(false).await;
    assert_eq!(input_one, input_two, "replay identical (input first)");

    assert!(
        position_of(&join_one, "exit:pf") < position_of(&join_one, "update:1"),
        "join-first script observes the exit first"
    );
    assert!(
        position_of(&input_one, "update:1") < position_of(&input_one, "exit:pf"),
        "input-first script delivers first"
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

    driver.step(Branch::Input).expect("batch with quit inside");
    assert_eq!(
        driver.state().applied,
        vec![1, 7],
        "messages after the quit are never applied"
    );
    assert!(driver.delivery().contains("terminating:Quit"), "sync quit");
    assert_eq!(
        driver.step(Branch::Input),
        Err(StepError::Terminated),
        "no further application calls after termination"
    );
    let report = driver.settle().await;
    assert!(report.gauges_zero, "quiescent postcondition");
}

/// S4 — an effect-issued quit travels the control lane and is observed
/// without draining the data backlog (backlog independence).
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

    driver.step(Branch::Control).expect("control drains first");
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
        .step(Branch::JoinExit)
        .expect("observe the panic exit");
    assert!(
        driver.delivery().contains("exit:pan:Panicked"),
        "panic observed as a join error"
    );
    assert_eq!(
        driver.entry_phase("pan"),
        Some((Phase::Draining, false)),
        "panic exit with committed pending is a live tombstone"
    );

    driver
        .step(Branch::Input)
        .expect("deliver the committed output");
    assert_eq!(driver.entry_phase("pan"), None, "entry reclaimed");
    driver.grant(healthy).await;
    driver
        .step(Branch::Input)
        .expect("other producers unaffected");
    assert_eq!(driver.state().applied, vec![10, 20], "loop continued");
    assert!(
        !driver.delivery().contains("terminating:Quit"),
        "containment: no termination"
    );
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

    let unwound = catch_unwind(AssertUnwindSafe(|| driver.step(Branch::Input)));
    assert!(unwound.is_err(), "the panic escapes the driving step");

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
        .step(Branch::Control)
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
    driver.step(Branch::Frame).expect("consume the boot redraw");
    assert_eq!(
        driver.delivery().count("reconcile"),
        1,
        "boot reconcile only"
    );

    fin.close();
    driver.await_exit_ready().await;
    driver
        .step(Branch::JoinExit)
        .expect("observe natural finish");
    assert_eq!(
        driver.step(Branch::Frame),
        Err(StepError::NotReady(Branch::Frame)),
        "natural finish leaves the loop parked"
    );
    assert_eq!(driver.delivery().count("reconcile"), 1, "no re-evaluation");

    input.push(5);
    driver.grant(input_id).await;
    driver.step(Branch::Input).expect("a message arrives");
    driver
        .step(Branch::Frame)
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
    driver.step(Branch::Frame).expect("consume the boot redraw");

    input.push(1);
    driver.grant(input_id).await;
    driver.step(Branch::Input).expect("deliver the swap");
    driver.step(Branch::Frame).expect("stop pass");
    assert_eq!(
        driver.step(Branch::Frame),
        Err(StepError::NotReady(Branch::Frame)),
        "nothing left to run: the loop is parked"
    );

    driver.await_exit_ready().await;
    driver.step(Branch::JoinExit).expect("quiescence wake");
    driver.step(Branch::Frame).expect("idle re-evaluation");
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
        work.driver.step(Branch::Input),
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
    work.driver.step(Branch::Input).expect("quit mid-batch");
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
    work.driver.step(Branch::Control).expect("control quit");
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
    work.driver.step(Branch::Frame).expect("failing render");
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
        .step(Branch::Input)
        .expect("teardown the gate waiter");
    driver.await_exit_ready().await;
    driver.step(Branch::JoinExit).expect("observe the abort");
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
    driver.step(Branch::JoinExit).expect("observe the exit");
    assert_eq!(
        driver.entry_phase("px"),
        Some((Phase::Draining, false)),
        "poisoned entry is retained past its exit"
    );
    driver
        .step(Branch::Input)
        .expect("deliveries proceed normally");
    assert_eq!(driver.state().applied, vec![1, 2], "no stale side effects");
    assert_eq!(
        driver.entry_phase("px"),
        Some((Phase::Draining, false)),
        "steady-state removal is disabled for a poisoned counter"
    );

    input.push(7);
    driver.grant(input_id).await;
    driver.step(Branch::Input).expect("terminate");
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
    driver.step(Branch::Input).expect("spawn the command run");
    input.push(2);
    driver.grant(input_id).await;
    driver.step(Branch::Input).expect("teardown + declare suba");
    assert_eq!(
        driver.entry_phase("fetch"),
        Some((Phase::Stopping, true)),
        "command run is stopping, unquiesced"
    );
    driver.step(Branch::Frame).expect("admission pass");
    assert!(
        driver.delivery().contains("admit:suba"),
        "a stopping command run does not defer subscription admission"
    );

    input.push(3);
    driver.grant(input_id).await;
    driver.step(Branch::Input).expect("swap suba for subb");
    driver.step(Branch::Frame).expect("stop pass defers");
    assert!(
        !driver.delivery().contains("admit:subb"),
        "a stopping subscription run defers unrelated admission"
    );

    for _ in 0..4 {
        if driver.delivery().contains("admit:subb") {
            break;
        }
        driver.await_exit_ready().await;
        driver.step(Branch::JoinExit).expect("reflect an exit");
        if driver.ready(Branch::Frame) {
            driver.step(Branch::Frame).expect("re-evaluation");
        }
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
    driver.step(Branch::Input).expect("child spawn via routing");
    driver
        .step(Branch::Frame)
        .expect("admit the scoped child sub");
    assert!(
        driver.delivery().contains("admit:child-sub"),
        "child sub up"
    );

    input.push(6);
    driver.grant(input_id).await;
    driver
        .step(Branch::Input)
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
        driver.await_exit_ready().await;
        driver.step(Branch::JoinExit).expect("reflect a child exit");
    }
    driver
        .step(Branch::Frame)
        .expect("post-teardown re-evaluation");
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
    driver.step(Branch::Frame).expect("consume the boot redraw");

    input.push(1);
    driver.grant(input_id).await;
    driver.step(Branch::Input).expect("spawn the command run");
    driver.step(Branch::Frame).expect("consume the update dirt");
    input.push(2);
    driver.grant(input_id).await;
    driver.step(Branch::Input).expect("stop the command run");
    driver.step(Branch::Frame).expect("consume the update dirt");
    assert_eq!(
        driver.step(Branch::Frame),
        Err(StepError::NotReady(Branch::Frame)),
        "nothing pending before the exit"
    );
    let reconciles = driver.delivery().count("reconcile");

    driver.await_exit_ready().await;
    driver
        .step(Branch::JoinExit)
        .expect("observe the stopped command exit");
    assert!(
        driver.delivery().contains("exit:fetch:Cancelled"),
        "the stopped command run exited"
    );
    assert_eq!(
        driver.step(Branch::Frame),
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
    driver.step(Branch::Input).expect("first keyed spawn");
    assert_eq!(driver.delivery().count("spawn:fetch:t2"), 1, "occupant up");

    input.push(2);
    driver.grant(input_id).await;
    driver.step(Branch::Input).expect("keep-in-flight spawn");
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
    driver.step(Branch::Input).expect("cancel-in-flight spawn");
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
    driver.step(Branch::Input).expect("both deliveries");
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
    driver.step(Branch::Input).expect("drain both");
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
        .step(Branch::Input)
        .expect("drain one; capacity frees");
    ack.await;
    driver.step(Branch::Input).expect("drain the second");
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
