//! Lifecycle: `both panic classes`, `shutdown-scoped send failure`,
//! `termination under owned work`, and `stop/restart safe window`
//! (RFC 0014 §13.1), plus the remaining rows of RFC 0011's own invariant
//! suite that INV-RC11 re-runs against the kernel.
//!
//! All but one row is pass-unit driven. The exception is named by §13.1
//! itself: `shutdown-scoped send failure` has two arms, "full topology" and
//! "component level", and the component arm drives the production producer
//! body over a real lane whose receiver is gone. It is a component-level
//! test by the series' own definition rather than a stage probe of the
//! driver, and it carries no same-topology claim.
//!
//! # INV-RC11's coverage map
//!
//! RFC 0011 states its rows over the old runtime's shapes — a frame pass
//! arbitrated against an input batch, a subscription manager, a keyed task
//! set. The kernel has one pass with four fixed stages and one registry, so
//! each row below is the owner's *claim* over the kernel's seam rather than
//! a transcription of its mechanism. Where a claim is structural in the
//! owner it stays structural here.
//!
//! | Owner row | Where |
//! | --- | --- |
//! | INV-LC1 (rendering and re-evaluation only in the frame stage, at most one of each) | this file, `a_multi_message_batch_renders_once_and_re_evaluates_once_after_it` |
//! | INV-LC2 (render before re-evaluation, both on the pass's current state) | this file, plus `kernel`'s own order and `without_redraw` rows |
//! | INV-LC3 (construction is inert) | this file, `constructing_a_driver_starts_nothing_and_dropping_it_winds_nothing_down` |
//! | INV-LC4 (bootstrap intake; first render eligible unconditionally) | `kernel`'s intake row, plus this file's `without_redraw` init row |
//! | INV-LC5 (controlled causes, with the return classification) | `termination under owned work`, above |
//! | INV-LC6 (abrupt causes, one row per cause and call site) | `both panic classes` and `termination under owned work`, above, plus this file's `view`, `subscriptions` (both call sites), lazy-constructor, and never-run rows |
//! | INV-LC7 (two-stage postcondition, bounded settle, gauges zero) | `kernel`'s settle row, at the gauge surface |
//! | INV-LC8 (containment, one row per producer kind) | `both panic classes`, above, plus this file's keyed and subscription-forwarder rows |
//! | INV-LC9 (one driver at a time) | structural: every driving call takes `&mut self`, `boot` runs once, and the production entry consumes its kernel — a reentrant path is unrepresentable rather than untested |
//!
//! The bootstrap row RFC 0014 §6.2 amends — an init quit reconciling
//! nothing and starting no source — lives with `kernel`'s own bootstrap
//! tests, beside the intake order it amends.

use std::sync::Arc;

use futures::StreamExt;
use futures::stream;

use crate::command::{Action, Command, CommandId};
use crate::kernel::accounting::PendingCounter;
use crate::kernel::arbiter::WakeSource;
use crate::kernel::lane::{GateMode, IngressHandle, SendGate, control_lane};
use crate::kernel::producer::{EffectCtx, command_body};
use crate::reducer::Exit;
use crate::runtime::channel::channel_observed;
use crate::runtime::load::{Channel, LoadObserver};
use crate::test_support::TraceRecorder;
use crate::testing::driver::{AcceptanceRecorder, IntentRecorder};

use super::support::{
    Beacon, Call, Feed, ProbeSource, Script, TEST_TURNS, accept, cap, config, driver, driver_with,
    failing_driver, holding_effect, marking_effect, panicking_effect, parking_effect,
    quitting_effect, silently,
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

// The containment class over the second producer kind (RFC 0011 INV-LC8's
// per-kind inventory): a *keyed* run's panic is contained exactly as an
// anonymous one is. The key names a slot in the run bookkeeping and takes
// no part in the panic's classification, which is the point of the row —
// the kernel's single registry is what makes the three kinds share one
// containment path rather than three.
#[test]
fn a_keyed_producer_panic_is_contained_and_delivery_continues() {
    let panicked = Beacon::default();
    silently(|| {
        let (mut driver, journal) = driver(Script::new(Command::batch([
            panicking_effect([10], panicked.clone()).cancellable(CommandId::new("worker")),
            parking_effect([20]),
        ])));
        let report = driver.boot();
        let (panicky, healthy) = (report.started[0].clone(), report.started[1].clone());

        accept(&mut driver, panicky);
        driver.settle(TEST_TURNS, || panicked.marked());

        let stepped = driver
            .step_pass(WakeSource::ProducerExit)
            .expect("the panicking keyed run's exit is observable");
        assert!(
            stepped.terminated.is_none(),
            "containment: a keyed producer's panic is not a termination cause either"
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

// The third producer kind: a subscription's *stream* panicking while the
// forwarder task polls it. Distinct from the lazy-constructor row below,
// which unwinds on the driving task and is fail-fast — the difference is
// which task holds the panic, and it is the whole of what separates the two
// classes.
#[test]
fn a_subscription_forwarder_panic_is_contained_and_delivery_continues() {
    let exploding = ProbeSource::exploding("boom");
    silently(|| {
        let (mut driver, journal) = driver(
            Script::new(parking_effect([20]))
                .feeding([Feed::new(exploding.clone())])
                .redeclaring(20, []),
        );
        let healthy = driver.boot().started[0].clone();
        assert_eq!(exploding.admissions(), 1, "boot admitted the source");

        driver.settle(TEST_TURNS, || exploding.quiescences() > 0);
        let stepped = driver
            .step_pass(WakeSource::ProducerExit)
            .expect("the panicking forwarder's exit is observable");
        assert!(
            stepped.terminated.is_none(),
            "containment: a forwarder panic is not a termination cause"
        );

        accept(&mut driver, healthy);
        driver
            .step_pass(WakeSource::Data)
            .expect("the healthy producer's send is in the lane");
        assert_eq!(
            journal.reduced(),
            vec![20],
            "the loop kept running and the surviving producer's output arrived"
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

// --- RFC 0011's remaining rows, on the kernel (INV-RC11) ------------------

// INV-LC1: rendering and re-evaluation happen in the frame stage and
// nowhere else, and one pass performs at most one of each however many
// messages its batch delivered. The whole claim reads off one call
// sequence: three `Reduce`s with nothing between them, then exactly one
// `View` and one `Subscriptions`.
#[test]
fn a_multi_message_batch_renders_once_and_re_evaluates_once_after_it() {
    let (mut driver, journal) = driver_with(
        Script::new(parking_effect([1, 2, 3])),
        config().batch_max_messages(cap(3)),
    );
    let sender = driver.boot().started[0].clone();
    for _ in 0..3 {
        accept(&mut driver, sender.clone());
    }
    let before = journal.calls().len();

    driver
        .step_pass(WakeSource::Data)
        .expect("three sends are in the lane");

    assert_eq!(
        journal.calls()[before..],
        [
            Call::Reduce(1),
            Call::Reduce(2),
            Call::Reduce(3),
            Call::View,
            Call::Subscriptions,
        ],
        "no render and no re-evaluation inside the batch, and one of each after it"
    );
}

// INV-LC2's state clause: the pass's one render observes the state the
// whole batch left, so the intermediate states the batch passed through are
// never rendered. The negative half is what makes this a claim — a
// per-message render would show `Some(1)` and `Some(2)` here.
#[test]
fn the_pass_s_one_render_observes_the_state_its_whole_batch_left() {
    let (mut driver, journal) = driver_with(
        Script::new(parking_effect([1, 2, 3])),
        config().batch_max_messages(cap(3)),
    );
    let sender = driver.boot().started[0].clone();
    for _ in 0..3 {
        accept(&mut driver, sender.clone());
    }

    driver
        .step_pass(WakeSource::Data)
        .expect("three sends are in the lane");

    assert_eq!(
        journal.rendered(),
        vec![None, Some(3)],
        "the bootstrap render observed a state with no message in it, and the batch's render \
         observed the state its last message left — never the two in between"
    );
}

// INV-LC3, with INV-LC6's never-run row: construction polls no effect,
// starts no source, calls nothing on the program, and fires no
// producer-gauge event — and dropping the result winds nothing down,
// because there is nothing to wind down.
//
// The primary check for INV-LC3 is structural in its owner, and stays so:
// `Kernel::new` builds lanes and empty bookkeeping and has no spawn or
// dispatch site, `boot` being the only place `init` is reached. This row is
// the behavioral regression beside it.
#[test]
fn constructing_a_driver_starts_nothing_and_dropping_it_winds_nothing_down() {
    let recorder = TraceRecorder::new().with_target("tears::runtime::load");
    let _subscriber = recorder.set_default();
    let polled = Beacon::default();
    let source = ProbeSource::silent("feed");
    let (driver, journal) =
        driver(Script::new(marking_effect(polled.clone())).feeding([Feed::new(source.clone())]));

    assert!(
        journal.calls().is_empty(),
        "construction called nothing on the program, `init` included"
    );
    assert!(!polled.marked(), "no init effect was polled");
    assert_eq!(source.admissions(), 0, "no subscription source was started");

    drop(driver);

    assert!(!polled.marked(), "and the drop started nothing either");
    assert_eq!(source.admissions(), 0);
    assert_eq!(
        recorder.event_count(),
        0,
        "no producer-gauge event fired across construction and drop"
    );
}

// INV-LC4's eligibility half: the first render is marked unconditionally
// and independently of the init command's redraw directive, so a
// `without_redraw` init still renders — with no message processed — before
// `boot` returns.
#[test]
fn the_first_render_is_marked_independently_of_the_init_command_s_directive() {
    let (mut driver, journal) = driver(Script::new(Command::none().without_redraw()));

    driver.boot();

    assert_eq!(
        journal.calls(),
        vec![Call::Init, Call::Subscriptions, Call::View],
        "the intake order, then the continuation pass's render"
    );
    assert_eq!(
        journal.rendered(),
        vec![None],
        "and it rendered with no message processed"
    );
}

// INV-LC6 at the render call site: a panic in `view` unwinds through the
// pass rather than being converted into a continuation, and the unwind
// reclaims the owned work on its way out.
#[test]
fn an_application_panic_in_view_is_fail_fast() {
    let reclaimed = Beacon::default();
    let outcome = silently(|| {
        let (mut driver, _journal) =
            driver(Script::new(holding_effect(reclaimed.clone())).panicking_in_view());
        drop(driver.boot());
    });

    assert!(
        outcome.is_err(),
        "the render panic escaped the bootstrap's continuation pass"
    );
    assert!(
        reclaimed.marked(),
        "and the unwind's drop reclaimed the owned work the kernel held"
    );
}

// INV-LC6 at the bootstrap declaration site: the initial reconcile runs on
// the driving task, so a panic there escapes `boot` itself.
#[test]
fn an_application_panic_in_subscriptions_at_the_bootstrap_call_site_is_fail_fast() {
    let reclaimed = Beacon::default();
    let outcome = silently(|| {
        let (mut driver, _journal) = driver(
            Script::new(holding_effect(reclaimed.clone()))
                .panicking_in_subscriptions_at_bootstrap(),
        );
        drop(driver.boot());
    });

    assert!(outcome.is_err(), "the panic escaped the initial reconcile");
    assert!(reclaimed.marked(), "and the owned work was reclaimed");
}

// INV-LC6 at the steady declaration site: the same call, reached from a
// pass's frame stage after a message, is on the same task and unwinds the
// same way. Two rows rather than one because the two call sites are two
// places the kernel could have got the containment boundary wrong.
#[test]
fn an_application_panic_in_subscriptions_at_the_steady_call_site_is_fail_fast() {
    let reclaimed = Beacon::default();
    let outcome = silently(|| {
        let (mut driver, _journal) = driver(
            Script::new(Command::batch([
                holding_effect(reclaimed.clone()),
                parking_effect([7]),
            ]))
            .panicking_in_subscriptions_after(7),
        );
        let sender = driver.boot().started[1].clone();
        accept(&mut driver, sender);
        drop(driver.step_pass(WakeSource::Data));
    });

    assert!(
        outcome.is_err(),
        "the re-evaluation's panic escaped the pass that delivered the message"
    );
    assert!(reclaimed.marked(), "and the owned work was reclaimed");
}

// INV-LC6's lazy-constructor row, and the counterpart to the contained
// forwarder panic above. The spawner is invoked at the admission, on the
// driving task (RFC 0012 INV-SE1), which is exactly why its panic is
// fail-fast while the stream's is contained: the kernel calls one and a
// runtime-owned task polls the other.
#[test]
fn a_panic_in_a_lazy_source_constructor_is_fail_fast_at_the_admission() {
    let reclaimed = Beacon::default();
    let source = ProbeSource::unbuildable("feed");
    let outcome = silently(|| {
        let (mut driver, _journal) = driver(
            Script::new(holding_effect(reclaimed.clone())).feeding([Feed::new(source.clone())]),
        );
        drop(driver.boot());
    });

    assert!(
        outcome.is_err(),
        "the constructor panicked on the driving task and escaped the reconcile"
    );
    assert_eq!(
        source.admissions(),
        1,
        "the kernel did invoke the spawner: this is an admission that unwound, not one skipped"
    );
    assert!(reclaimed.marked(), "and the owned work was reclaimed");
}
