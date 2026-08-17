//! Lifecycle: `both panic classes`, `shutdown-scoped send failure`,
//! `termination under owned work`, and `stop/restart safe window`
//! (RFC 0014 §13.1).
//!
//! All but one row is pass-unit driven. The exception is named by §13.1
//! itself: `shutdown-scoped send failure` has two arms, "full topology" and
//! "component level", and the component arm drives the production producer
//! body over a real lane whose receiver is gone. It is a component-level
//! test by the series' own definition rather than a stage probe of the
//! driver, and it carries no same-topology claim.

use std::sync::Arc;

use futures::StreamExt;
use futures::stream;

use crate::command::{Action, Command};
use crate::kernel::accounting::PendingCounter;
use crate::kernel::arbiter::WakeSource;
use crate::kernel::lane::{GateMode, IngressHandle, SendGate, control_lane};
use crate::kernel::producer::{EffectCtx, command_body};
use crate::reducer::Exit;
use crate::runtime::channel::channel_observed;
use crate::runtime::load::{Channel, LoadObserver};
use crate::testing::driver::{AcceptanceRecorder, IntentRecorder};

use super::support::{
    Beacon, Feed, ProbeSource, Script, TEST_TURNS, accept, cap, config, driver, driver_with,
    failing_driver, holding_effect, panicking_effect, parking_effect, quitting_effect, silently,
};

// --- `both panic classes` -------------------------------------------------

// The contained class (RFC 0011 INV-LC8): a producer panic surfaces as a
// join error, its already-committed output still delivers through the live
// tombstone, the kernel keeps running, and other producers keep delivering.
#[test]
fn a_producer_panic_is_contained_and_delivery_continues() {
    let panicked = Beacon::default();
    silently(|| {
        let (mut driver, journal) = driver(Script::new(Command::batch([
            panicking_effect([10], panicked.clone()),
            parking_effect([20]),
        ])));
        let report = driver.boot();
        let (panicky, healthy) = (report.started[0].clone(), report.started[1].clone());

        accept(&mut driver, panicky);
        driver.settle(TEST_TURNS, || panicked.marked());

        let stepped = driver
            .step_pass(WakeSource::ProducerExit)
            .expect("the panicking run's exit is observable");
        assert!(
            stepped.terminated.is_none(),
            "containment: a producer panic is not a termination cause"
        );
        assert_eq!(
            journal.reduced(),
            vec![10],
            "the output it committed before panicking still delivered"
        );

        accept(&mut driver, healthy);
        driver
            .step_pass(WakeSource::Data)
            .expect("the healthy producer's send is in the lane");
        assert_eq!(
            journal.reduced(),
            vec![10, 20],
            "and the loop kept delivering for every other producer"
        );
    })
    .expect("the test body itself does not unwind");
}

// The fail-fast class: a panic in application code on the *driving* task is
// not converted into a continuation — it escapes the pass — and dropping the
// driver reclaims every producer it owned.
#[test]
fn an_application_panic_on_the_driving_task_is_fail_fast() {
    let reclaimed = Beacon::default();
    let outcome = silently(|| {
        let (mut driver, _journal) = driver(
            Script::new(Command::batch([
                holding_effect(reclaimed.clone()),
                parking_effect([13]),
            ]))
            .panicking_on(13),
        );
        let sender = driver.boot().started[1].clone();
        accept(&mut driver, sender);
        drop(driver.step_pass(WakeSource::Data));
    });

    assert!(
        outcome.is_err(),
        "the application panic escaped the driving pass"
    );
    assert!(
        reclaimed.marked(),
        "and the unwind's drop reclaimed the owned work the kernel held"
    );
}

// --- `shutdown-scoped send failure`, full topology ------------------------

// A producer blocked in a bounded send is reclaimed by termination, and both
// stages of RFC 0011 §4.4's postcondition are visible from the application
// side: the immediate stage drops the receivers so the blocked send fails
// and ends its run, and the quiescent stage drains the join set before the
// step reports the termination — so the owned work is already reclaimed when
// the report comes back.
//
// The grant outstanding at the blocked send is deliberately never confirmed:
// termination puts the driver in its terminated state, where `confirm` is
// misuse, so a shutdown-time reclamation is not something that resolution
// reports (RFC 0008 §9.6). What the send never did is read where it is
// readable — the gate admitted nothing for it.
#[test]
fn a_blocked_sender_is_reclaimed_by_termination_without_ever_committing() {
    let reclaimed = Beacon::default();
    let (mut driver, journal) = driver_with(
        Script::new(Command::batch([
            parking_effect([1, 2]),
            holding_effect(reclaimed.clone()),
            quitting_effect(),
        ])),
        config().app_channel_capacity(cap(1)),
    );
    let report = driver.boot();
    let (flood, quitter) = (report.started[0].clone(), report.started[2].clone());

    accept(&mut driver, flood.clone());
    accept(&mut driver, quitter);
    assert_eq!(
        driver.accepted().len(),
        2,
        "the first message filled the one-slot lane, and the quit took the control lane"
    );

    // Released, but the lane is full: the producer parks inside the real
    // send holding its reservation.
    let blocked = driver.grant(flood).expect("the previous grant resolved");
    let stepped = driver
        .step_pass(WakeSource::Control)
        .expect("the quit is on the control lane");

    assert_eq!(
        stepped.terminated.map(|result| result.map_err(drop)),
        Some(Ok(Exit::Quit)),
        "the control drain terminated under a blocked sender"
    );
    assert!(
        reclaimed.marked(),
        "the quiescent postcondition drained the join set before the step reported"
    );
    assert_eq!(
        driver.accepted().len(),
        2,
        "the blocked send never committed: no false acceptance"
    );
    assert!(
        journal.reduced().is_empty(),
        "and the backlog was discarded rather than applied"
    );
    drop(blocked);
}

// --- `shutdown-scoped send failure`, component level ----------------------

// The production producer body over a real lane whose receiver is gone: the
// send returns `Err`, the run ends itself — the send-stop policy, RFC 0014
// §6.1 — and the reservation it took is released, so closure is never
// counted as an acceptance. Component-level by §13.1's own split; it carries
// no same-topology claim and no pass order.
#[tokio::test]
async fn a_producer_body_observes_lane_closure_and_stops_itself() {
    let observer = LoadObserver::new();
    let (data, data_rx) = channel_observed(Some(cap(4)), Channel::Data, observer);
    let (control, control_rx) = control_lane::<u8>();
    let counter = Arc::new(PendingCounter::default());
    let intents = IntentRecorder::new(GateMode::Scripted);
    let acceptances = AcceptanceRecorder::new(GateMode::Scripted);
    let handle = IngressHandle::new(
        1,
        Arc::clone(&counter),
        Arc::new(SendGate::new(GateMode::Immediate)),
        data,
        control,
        intents.clone(),
        acceptances.clone(),
    );
    drop(data_rx);
    drop(control_rx);

    let body = command_body(stream::iter([Action::Message(7_u8), Action::Message(8)]).boxed());
    body(EffectCtx { handle }).await;

    assert_eq!(
        counter.value(),
        0,
        "the failed send released the reservation it took"
    );
    assert!(
        acceptances.snapshot().is_empty(),
        "closure is never counted as an acceptance"
    );
    assert_eq!(
        intents.snapshot().len(),
        1,
        "the run stopped itself at the first failed send rather than trying the next"
    );
}

// --- `termination under owned work` ---------------------------------------

// The `update`-returned cause: owned work is reclaimed and the queued
// backlog is discarded rather than applied.
#[test]
fn an_update_quit_reclaims_owned_work_and_discards_the_backlog() {
    let reclaimed = Beacon::default();
    let (mut driver, journal) = driver_with(
        Script::new(Command::batch([
            holding_effect(reclaimed.clone()),
            parking_effect([5, 7, 6]),
        ]))
        .replying([Command::none(), Command::quit(), Command::none()]),
        config().batch_max_messages(cap(3)),
    );
    let sender = driver.boot().started[1].clone();
    for _ in 0..3 {
        accept(&mut driver, sender.clone());
    }

    let stepped = driver
        .step_pass(WakeSource::Data)
        .expect("the three sends are in the lane");

    assert!(stepped.terminated.is_some(), "the quit applied");
    assert_eq!(journal.reduced(), vec![5, 7], "the backlog was discarded");
    assert!(reclaimed.marked(), "and the owned work was reclaimed");
}

// The producer-originated cause, through the control lane.
#[test]
fn a_producer_quit_reclaims_owned_work() {
    let reclaimed = Beacon::default();
    let (mut driver, journal) = driver(Script::new(Command::batch([
        holding_effect(reclaimed.clone()),
        parking_effect([5]),
        quitting_effect(),
    ])));
    let report = driver.boot();
    let (sender, quitter) = (report.started[1].clone(), report.started[2].clone());

    accept(&mut driver, sender);
    accept(&mut driver, quitter);
    let stepped = driver
        .step_pass(WakeSource::Control)
        .expect("the quit is on the control lane");

    assert!(stepped.terminated.is_some(), "the quit applied");
    assert!(journal.reduced().is_empty(), "the backlog went untouched");
    assert!(reclaimed.marked(), "and the owned work was reclaimed");
}

// The render-failure cause: it routes through the same termination, and only
// the reported result distinguishes it (RFC 0011 INV-LC5's `Err`).
#[test]
fn a_render_failure_reclaims_owned_work_through_the_same_termination() {
    let reclaimed = Beacon::default();
    let (mut driver, journal) = failing_driver(
        Script::new(Command::batch([
            holding_effect(reclaimed.clone()),
            parking_effect([5]),
        ])),
        1,
    );
    let sender = driver.boot().started[1].clone();

    accept(&mut driver, sender);
    let stepped = driver
        .step_pass(WakeSource::Data)
        .expect("the send is in the lane");

    assert!(
        stepped.terminated.is_some_and(|result| result.is_err()),
        "the failing render is the production result's error side"
    );
    assert_eq!(
        journal.reduced(),
        vec![5],
        "the batch ran before the frame stage that failed"
    );
    assert!(reclaimed.marked(), "and the owned work was reclaimed");
}

// The host-side cause: dropping the driver drops the kernel, whose own drop
// aborts everything it owns, so no runtime-owned task outlives it.
#[test]
fn dropping_the_driver_reclaims_owned_work() {
    let reclaimed = Beacon::default();
    let (mut driver, _journal) = driver(Script::new(holding_effect(reclaimed.clone())));
    driver.boot();
    assert!(!reclaimed.marked(), "the run is alive while the driver is");

    drop(driver);

    assert!(
        reclaimed.marked(),
        "a host-side drop reclaims owned work through the kernel's drop postcondition"
    );
}

// --- `stop/restart safe window` -------------------------------------------

// Removing and re-adding one subscription identity: the successor is
// admitted strictly after the predecessor has quiesced, and it starts fresh
// — a new run under the same logical identity, not a resumption of the old
// one.
//
// The window this walks is the one INV-ST7's observable half names. What it
// does *not* walk is the uniform barrier's runtime-wide deferral of a
// *later* pass's admissions: a stop-requested run's abort resolves within
// one executor turn here, so the only pass in which a stopped subscription
// run is still unquiesced is the pass that issued the stop — which is
// covered by the stopping-pass defer rule instead (RFC 0014 §5.3, witnessed
// in `park`). INV-RC12 (c) records the same limit from the invariant's side
// and takes the structural class for it.
#[test]
fn a_same_identity_successor_waits_out_its_predecessor_s_quiescence() {
    let source = ProbeSource::silent("suba");
    let (mut driver, _journal) = driver_with(
        Script::new(parking_effect([1, 2]))
            .feeding([Feed::new(source.clone())])
            .redeclaring(1, [])
            .redeclaring(2, ["suba"]),
        config().batch_max_messages(cap(1)),
    );
    let trigger = driver.boot().started[0].clone();
    assert_eq!(source.admissions(), 1, "boot admitted the declaration");

    accept(&mut driver, trigger.clone());
    accept(&mut driver, trigger);

    driver
        .step_pass(WakeSource::Data)
        .expect("the removal trigger is in the lane");
    assert_eq!(
        source.admissions(),
        1,
        "the stop-issuing pass admits nothing"
    );

    driver.settle(TEST_TURNS, || source.quiescences() > 0);
    assert_eq!(
        source.admissions(),
        1,
        "and nothing admits behind the stop while it is settling"
    );

    let stepped = driver
        .step_pass(WakeSource::Data)
        .expect("the re-declaration trigger is in the lane");
    assert_eq!(
        source.admissions(),
        2,
        "the successor is admitted strictly after the predecessor quiesced"
    );
    assert_eq!(
        stepped.started.len(),
        1,
        "and it is a fresh run, started by this pass"
    );
}

// The barrier's scope, INV-RC12 (a): a stop-requested *command* run defers no
// subscription admission — only subscription runs join the uniform barrier.
#[test]
fn a_stopping_command_run_defers_no_subscription_admission() {
    let reclaimed = Beacon::default();
    let source = ProbeSource::silent("late");
    let (mut driver, _journal) = driver_with(
        Script::new(Command::batch([
            holding_effect(reclaimed.clone()).scoped("pane"),
            parking_effect([1]),
        ]))
        .feeding([Feed::new(source.clone())])
        .wanting([])
        .redeclaring(1, ["late"])
        .replying([Command::teardown("pane")]),
        config().batch_max_messages(cap(1)),
    );
    let trigger = driver.boot().started[1].clone();
    assert_eq!(source.admissions(), 0, "nothing is declared yet");

    accept(&mut driver, trigger);
    driver
        .step_pass(WakeSource::Data)
        .expect("the trigger is in the lane");

    assert_eq!(
        source.admissions(),
        1,
        "the same pass tore a command run down and still admitted the new declaration"
    );
    assert!(
        !reclaimed.marked(),
        "the command run is stop-requested but has not quiesced, which is the point"
    );
}
