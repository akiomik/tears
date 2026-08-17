//! Park and wake: the three `ParkProbe` series, plus the pass-unit
//! `idle wake` series (RFC 0014 §13.1).
//!
//! **The three probe series carry INV-RC16 and nothing else** (RFC 0008
//! §9.7). Each drives the production loop itself — the future
//! [`Kernel::run`] returns, polled by hand with the probe's own waker — and
//! each is scripted from a genuinely parked kernel with no other work
//! pending. A probe observation is never evidence for INV-RC13's
//! same-topology claim, for INV-RC14's scripted determinism, for RFC 0014
//! §3.5's stage order, or for production pass initiation.
//!
//! The witness has two stages and one arrival:
//!
//! 1. **That it parked.** A re-poll returns `Pending`, the wake count does
//!    not move, and the application's own instrumentation stays silent — no
//!    journal entry, no `view` call. Hardened, where the script allows it,
//!    with executor turns: any number of turns has to leave a parked loop's
//!    waker unsignalled, which is what separates a park from a loop that
//!    yields and re-arms itself every turn. The ledgers are not among these
//!    witnesses and could not be — a probe series polls the production
//!    future and no `TestDriver` is in play, so neither ledger exists in
//!    that execution.
//! 2. **Which source.** By construction: the script arranges the arrival of
//!    one source and no other, so the wake it observes can have come from
//!    nothing else, and the count moves **exactly once**.
//!
//! The `idle wake` series below is *not* a row of INV-RC16 (§12): it is
//! pass-unit driven and exercises the woken pass's exit-reflection stage —
//! RFC 0014 §5.2's dirt sources — rather than the park boundary.
//!
//! [`Kernel::run`]: crate::kernel::Kernel::run

use crate::command::{Command, CommandId};
use crate::kernel::arbiter::WakeSource;
use crate::reducer::Exit;
use crate::testing::driver::{Confirmed, ParkProbe};

use super::support::{
    Beacon, Call, Feed, Latch, ProbeSource, Script, TEST_TURNS, assert_parked,
    assert_parked_across_turns, drive_to_ready, driver, holding_effect, latched_effect,
    park_kernel, parked_quitting_effect, parking_effect, settle_until, terminal,
};

// --- `parked data-lane wake` ----------------------------------------------

// INV-RC16, the data-lane arm. The producer parks forever after its one
// send, so its task never exits and no producer-exit notification can stand
// in for the wake being measured: the acceptance of one message on the data
// lane is the only thing that happens between the park and the wake.
#[tokio::test(flavor = "current_thread")]
async fn parked_data_lane_wake() {
    let latch = Latch::default();
    let sent = Beacon::default();
    let (mut kernel, journal) = park_kernel(
        Script::new(latched_effect(latch.clone(), vec![7], sent.clone()))
            .replying([Command::quit()]),
    );
    let mut screen = terminal();
    let probe = ParkProbe::new();
    let mut run = Box::pin(async {
        kernel
            .run(&mut screen)
            .await
            .map(|_report| Exit::Quit)
            .map_err(|_error| ())
    });

    assert!(
        probe.poll(run.as_mut()).is_pending(),
        "the production loop suspends after the boot pass"
    );
    let parked_at =
        assert_parked_across_turns(&probe, run.as_mut(), &journal, "after the boot pass").await;
    assert_eq!(parked_at, 0, "nothing has woken the loop yet");
    let at_park = journal.calls();

    // The one event: the producer's message reaches the data lane. The gate
    // is production's immediate one, so the send commits as soon as the
    // latch releases the run.
    latch.open();
    settle_until(|| sent.marked(), "the message is accepted by the data lane").await;

    assert_eq!(
        journal.calls(),
        at_park,
        "the arrival ran no application code: the loop has not been polled since the park"
    );
    assert_eq!(
        probe.wakes(),
        parked_at + 1,
        "the data-lane arrival alone woke the parked loop, with exactly one signal"
    );

    assert_eq!(
        drive_to_ready(&probe, run.as_mut()).await,
        Ok(Exit::Quit),
        "the woken pass delivered the message, whose update quit"
    );
    assert_eq!(
        journal.reduced(),
        vec![7],
        "and the message it delivered is the one that arrived"
    );
}

// --- `parked control-quit wake` -------------------------------------------

// INV-RC16, the control-lane arm. The quitter emits one producer quit and
// then parks forever — the wake counterexample's own producer, so no task
// exit can stand in for the arrival — and no input accompanies it. The
// counterexample this excludes is a kernel arming wakers on the data lane
// and the join set only: it satisfies INV-RC9's "applied at the first
// control drain at or after its arrival" vacuously, because no pass ever
// begins, and its counter never leaves the parked value.
#[tokio::test(flavor = "current_thread")]
async fn parked_control_quit_wake() {
    let latch = Latch::default();
    let sent = Beacon::default();
    let (mut kernel, journal) = park_kernel(Script::new(parked_quitting_effect(
        latch.clone(),
        sent.clone(),
    )));
    let mut screen = terminal();
    let probe = ParkProbe::new();
    let mut run = Box::pin(async {
        kernel
            .run(&mut screen)
            .await
            .map(|_report| Exit::Quit)
            .map_err(|_error| ())
    });

    assert!(
        probe.poll(run.as_mut()).is_pending(),
        "the production loop suspends after the boot pass"
    );
    let parked_at =
        assert_parked_across_turns(&probe, run.as_mut(), &journal, "after the boot pass").await;
    assert_eq!(parked_at, 0, "nothing has woken the loop yet");
    let at_park = journal.calls();

    // The one event: a producer-originated quit reaches the control lane.
    latch.open();
    settle_until(|| sent.marked(), "the quit is accepted by the control lane").await;

    assert_eq!(
        journal.calls(),
        at_park,
        "no input accompanied the quit, and no pass has run since the park"
    );
    assert_eq!(
        probe.wakes(),
        parked_at + 1,
        "the control-lane arrival alone woke the parked loop, with exactly one signal"
    );

    assert_eq!(
        drive_to_ready(&probe, run.as_mut()).await,
        Ok(Exit::Quit),
        "the woken pass applied the arrived quit at its control drain"
    );
    assert!(
        journal.reduced().is_empty(),
        "the quit is backlog-independent: no input was delivered on its account"
    );
}

// --- `parked subscription-quiescence wake` --------------------------------

// INV-RC16, the producer-exit arm, at the notification that arms it for
// subscriptions. One message flips the declaration, so the woken pass stops
// `suba` and — being a stopping pass — admits nothing; the loop then parks
// with the stop outstanding. The stopped run's quiescence is the one event
// after that, and the woken pass's exit reflection marks dirt so the same
// pass re-evaluates and admits the successor, with no message in between.
#[tokio::test(flavor = "current_thread")]
async fn parked_subscription_quiescence_wake() {
    let latch = Latch::default();
    let sent = Beacon::default();
    let stopped = ProbeSource::silent("suba");
    let successor = ProbeSource::silent("subb");
    let (mut kernel, journal) = park_kernel(
        Script::new(latched_effect(latch.clone(), vec![1], sent.clone()))
            .feeding([Feed::new(stopped.clone()), Feed::new(successor.clone())])
            .wanting(["suba"])
            .redeclaring(1, ["subb"]),
    );
    let mut screen = terminal();
    let probe = ParkProbe::new();
    let mut run = Box::pin(async {
        kernel
            .run(&mut screen)
            .await
            .map(|_report| Exit::Quit)
            .map_err(|_error| ())
    });

    assert!(
        probe.poll(run.as_mut()).is_pending(),
        "the production loop suspends after the boot pass"
    );
    assert_parked_across_turns(&probe, run.as_mut(), &journal, "after the boot pass").await;
    assert_eq!(stopped.admissions(), 1, "boot admitted the declaration");
    assert_eq!(successor.admissions(), 0, "and only that one");

    latch.open();
    settle_until(|| sent.marked(), "the trigger's message is accepted").await;
    assert!(
        probe.poll(run.as_mut()).is_pending(),
        "the message-woken pass runs the re-evaluation"
    );
    assert_eq!(
        successor.admissions(),
        0,
        "the stopping pass admits nothing, successor included"
    );

    // This park carries the synchronous witness only: the executor turns
    // that would falsify a yielding impostor are the very turns that reclaim
    // the stopped run, so they cannot be spent before the awaited arrival.
    // The exactly-one-signal assertion below covers that gap instead — an
    // impostor re-arming itself every turn accumulates more than the one
    // signal the quiescence produces.
    let parked_at = assert_parked(&probe, run.as_mut(), &journal, "with the stop outstanding");
    let at_park = journal.calls();

    // The one event: the stop-requested subscription run quiesces. No input,
    // no control traffic, no pending frame work.
    settle_until(
        || stopped.quiescences() > 0,
        "the stopped subscription run quiesces",
    )
    .await;

    assert_eq!(
        journal.calls(),
        at_park,
        "the quiescence ran no application code by itself"
    );
    assert_eq!(
        probe.wakes(),
        parked_at + 1,
        "the quiescence notification alone woke the parked loop, with exactly one signal"
    );

    assert!(
        probe.poll(run.as_mut()).is_pending(),
        "the woken pass reflects the exit and re-evaluates"
    );
    assert_eq!(
        successor.admissions(),
        1,
        "the woken pass's frame stage admitted the successor"
    );
    assert!(
        !journal.calls()[at_park.len()..].contains(&Call::Reduce(1)),
        "no message ran between the quiescence and the re-evaluation: {:?}",
        journal.calls()
    );
    assert_parked(&probe, run.as_mut(), &journal, "after the woken pass");
}

// --- `idle wake` (pass-unit) ----------------------------------------------

// The positive row: the quiescence of a *stopped subscription* run is one of
// RFC 0014 §5.2's two dirt sources, so the pass its notification begins
// re-evaluates in its own frame stage — with no message in between, which is
// what makes this an idle wake rather than a message-driven one.
#[test]
fn a_stopped_subscription_s_quiescence_re_evaluates_in_the_pass_that_reflects_it() {
    let stopped = ProbeSource::silent("suba");
    let successor = ProbeSource::silent("subb");
    let (mut driver, journal) = driver(
        Script::new(parking_effect([1]))
            .feeding([Feed::new(stopped.clone()), Feed::new(successor.clone())])
            .wanting(["suba"])
            .redeclaring(1, ["subb"]),
    );
    let trigger = driver.boot().started[0].clone();

    let token = driver.grant(trigger).expect("no other grant");
    assert_eq!(driver.confirm(TEST_TURNS, token), Confirmed::Accepted);
    driver
        .step_pass(WakeSource::Data)
        .expect("the granted send is in the lane");
    assert_eq!(
        successor.admissions(),
        0,
        "the stop-issuing pass admits nothing in its own pass"
    );

    driver.settle(TEST_TURNS, || stopped.quiescences() > 0);
    let before = journal.calls();
    driver
        .step_pass(WakeSource::ProducerExit)
        .expect("the stopped run's exit is observable");

    assert_eq!(
        successor.admissions(),
        1,
        "the exit-reflection stage marked dirt and the same pass's frame stage admitted"
    );
    assert!(
        !journal.calls()[before.len()..]
            .iter()
            .any(|call| matches!(call, Call::Reduce(_))),
        "the re-evaluation was reached with no message in between: {:?}",
        journal.calls()
    );
}

// The natural-finish contrast: a subscription run that ends on its own marks
// no dirt, so the pass that reflects its exit re-evaluates nothing — and the
// still-declared source restarts only at the next message-driven
// re-evaluation (RFC 0014 §5.2).
#[test]
fn a_natural_finish_marks_no_dirt_and_restarts_at_the_next_re_evaluation() {
    let finishing = ProbeSource::finishing("fin");
    let (mut driver, journal) =
        driver(Script::new(parking_effect([1])).feeding([Feed::new(finishing.clone())]));
    let trigger = driver.boot().started[0].clone();
    assert_eq!(finishing.admissions(), 1, "boot admitted the declaration");

    driver.settle(TEST_TURNS, || finishing.quiescences() > 0);
    let evaluations = journal.evaluations();
    driver
        .step_pass(WakeSource::ProducerExit)
        .expect("the finished run's exit is observable");

    assert_eq!(
        journal.evaluations(),
        evaluations,
        "a natural finish triggers no re-evaluation"
    );
    assert_eq!(finishing.admissions(), 1, "so nothing restarted it");

    let token = driver.grant(trigger).expect("no other grant");
    assert_eq!(driver.confirm(TEST_TURNS, token), Confirmed::Accepted);
    driver
        .step_pass(WakeSource::Data)
        .expect("the granted send is in the lane");
    assert_eq!(
        finishing.admissions(),
        2,
        "the still-declared finished source restarts at the next re-evaluation"
    );
}

// The other non-source: a stopped *command* run's quiescence marks no dirt
// either — the dirt boundary quantifies over subscription runs alone
// (RFC 0014 §5.2, INV-RC12 (b)).
#[test]
fn a_stopped_command_run_s_quiescence_marks_no_dirt() {
    let keyed = CommandId::new("worker");
    let reclaimed = Beacon::default();
    let (mut driver, journal) = driver(
        Script::new(Command::batch([
            holding_effect(reclaimed.clone()).cancellable(keyed.clone()),
            parking_effect([1]),
        ]))
        .replying([Command::cancel(keyed)])
        .feeding([Feed::new(ProbeSource::silent("keep"))]),
    );
    let report = driver.boot();
    let trigger = report.started[1].clone();

    let token = driver.grant(trigger).expect("no other grant");
    assert_eq!(driver.confirm(TEST_TURNS, token), Confirmed::Accepted);
    driver
        .step_pass(WakeSource::Data)
        .expect("the granted send is in the lane");

    driver.settle(TEST_TURNS, || reclaimed.marked());
    let evaluations = journal.evaluations();
    driver
        .step_pass(WakeSource::ProducerExit)
        .expect("the cancelled run's exit is observable");

    assert_eq!(
        journal.evaluations(),
        evaluations,
        "a stopped command run's quiescence is no dirt source"
    );
}
