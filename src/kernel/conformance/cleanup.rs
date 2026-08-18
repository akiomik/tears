//! Cleanup hooks, RFC 0014 INV-RC8's clauses.
//!
//! Every row runs through the production dispatch path, pass-unit driven: a
//! registration reaches the kernel only as the lowered part of a command the
//! application returned, and so does the teardown that starts it — no driver
//! method arms one and none fires one (RFC 0008 §9.2).
//!
//! # Which clause each row carries
//!
//! | clause (RFC 0014 §4.4) | row |
//! | --- | --- |
//! | started at the application point | [`a_teardown_starts_the_prefix_s_registration_at_its_application_point`] |
//! | at most once, consumed not re-run | [`a_repeated_teardown_starts_no_consumed_hook_again`] |
//! | no runtime-visible output | [`an_ordinary_cleanup_run_is_invisible_to_the_application`] (regression neighbour — see below) |
//! | termination discards unfired hooks | [`termination_discards_an_unfired_registration`] |
//! | termination cancels running ones | [`termination_cancels_a_running_cleanup_run`] |
//!
//! # The no-output clause is structural-primary
//!
//! It is reviewed at the cleanup task's **construction site**
//! ([`CleanupHarness`](crate::kernel::producer::CleanupHarness)), where the
//! run is handed no lane sender, no ingress, and no directive capability.
//! That is the evidence, and it has to be, because no cleanup run that
//! *attempts* an output exists for a test to observe failing: a passing test
//! would witness the absence of the attempt rather than of the output
//! (RFC 0014 INV-RC8). The behavioral neighbour below is one regression row
//! over an ordinary finalizer — nothing delivered to `update`, no
//! termination, no redraw and no subscription dirt attributable to it — and
//! it no more proves the absence of the capability than a finite scenario
//! proves a pool's absence.
//!
//! # Where a claim is read
//!
//! A finalizer's own progress is application-side, in a [`Beacon`] it marks
//! and a [`DropMark`] its body holds: a cleanup run presents no send-intent
//! and makes no ledger record, so it is named by no [`RunName`] and reported
//! in no step's `started` list (RFC 0008 §9.4), and the bounded
//! [`settle`](crate::testing::driver::TestDriver::settle) is what gives it
//! turns to run in. Non-delivery is read from the reducer's own journal, as
//! everywhere else in this suite.
//!
//! [`RunName`]: crate::testing::driver::RunName

use std::future::pending;

use crate::command::Command;
use crate::kernel::arbiter::WakeSource;

use super::support::{
    Beacon, DropMark, Feed, ProbeSource, Script, TEST_TURNS, accept, cap, config, driver_with,
    marking_effect, parking_effect,
};

/// A finalizer that marks `ran` and ends.
fn finalizer(ran: Beacon) -> Command<u8> {
    Command::on_teardown(async move {
        ran.mark();
    })
}

/// A finalizer that marks `started` on its first poll and then parks
/// forever, holding a guard that marks `reclaimed` when the run is
/// dismantled.
///
/// The guard is captured by the future rather than created inside it, so a
/// registration cancelled before its first poll still marks — which is what
/// makes the termination row read the reclamation rather than the run's
/// having gotten far enough to notice.
fn parked_finalizer(started: Beacon, reclaimed: Beacon) -> Command<u8> {
    let guard = DropMark::new(reclaimed);
    Command::on_teardown(async move {
        let _reclaimed = guard;
        started.mark();
        pending::<()>().await;
    })
}

// The application point: a registration is inert until a teardown whose
// prefix covers its scope applies, and it starts *there* — in the cancel
// phase of the command that carried the teardown, concurrently with the
// quiescence of the runs that same point stopped.
//
// The negative half is what makes the row about the application point rather
// than about the finalizer eventually running: the turns spent reaching the
// init effect's own completion are turns the finalizer would have run in had
// arming started it.
#[test]
fn a_teardown_starts_the_prefix_s_registration_at_its_application_point() {
    let (ran, booted) = (Beacon::default(), Beacon::default());
    let (mut driver, journal) = driver_with(
        Script::new(Command::batch([
            parking_effect([1]),
            marking_effect(booted.clone()),
            finalizer(ran.clone()).scoped("pane"),
        ]))
        .replying([Command::teardown("pane")]),
        config().batch_max_messages(cap(1)),
    );
    let trigger = driver.boot().started[0].clone();

    driver.settle(TEST_TURNS, || booted.marked());
    assert!(
        !ran.marked(),
        "arming a registration starts nothing: the turns that ran the init effect to its end \
         would have run the finalizer too"
    );

    accept(&mut driver, trigger);
    let stepped = driver
        .step_pass(WakeSource::Data)
        .expect("the teardown trigger is in the lane");

    assert_eq!(journal.reduced(), vec![1], "the teardown was dispatched");
    assert!(
        stepped.started.is_empty(),
        "a cleanup run presents no send-intent and makes no ledger record, so no name is minted \
         for it (RFC 0008 §9.4)"
    );
    driver.settle(TEST_TURNS, || ran.marked());
    assert_eq!(ran.marks(), 1);
}

// INV-RC8's at-most-once clause, and RFC 0013 INV-ST5's "re-fires nothing":
// consumption removes the registration, so the second application of the
// same prefix has nothing left to start. The kernel keeps no fired-flag for
// a later path to read wrongly — the ledger entry is simply gone.
#[test]
fn a_repeated_teardown_starts_no_consumed_hook_again() {
    let ran = Beacon::default();
    let (mut driver, journal) = driver_with(
        Script::new(Command::batch([
            parking_effect([1, 2]),
            finalizer(ran.clone()).scoped("pane"),
        ]))
        .replying([Command::teardown("pane"), Command::teardown("pane")]),
        config().batch_max_messages(cap(1)),
    );
    let trigger = driver.boot().started[0].clone();

    accept(&mut driver, trigger.clone());
    driver
        .step_pass(WakeSource::Data)
        .expect("the first trigger is in the lane");
    driver.settle(TEST_TURNS, || ran.marked());
    assert_eq!(ran.marks(), 1);

    accept(&mut driver, trigger);
    driver
        .step_pass(WakeSource::Data)
        .expect("the second trigger is in the lane");
    driver.settle(TEST_TURNS, || true);

    assert_eq!(
        ran.marks(),
        1,
        "the consumed hook is not re-fired by a second application of its prefix"
    );
    assert_eq!(journal.reduced(), vec![1, 2]);
}

// Selection isolation for registrations (RFC 0013 INV-ST6 applied to the
// cleanup half of INV-ST1's selection set): a teardown of a sibling prefix
// consumes nothing, and the registration is still there for its own prefix.
#[test]
fn a_sibling_prefix_consumes_no_registration() {
    let ran = Beacon::default();
    let (mut driver, _journal) = driver_with(
        Script::new(Command::batch([
            parking_effect([1, 2]),
            finalizer(ran.clone()).scoped("pane"),
        ]))
        .replying([Command::teardown("other-pane"), Command::teardown("pane")]),
        config().batch_max_messages(cap(1)),
    );
    let trigger = driver.boot().started[0].clone();

    accept(&mut driver, trigger.clone());
    driver
        .step_pass(WakeSource::Data)
        .expect("the sibling teardown is in the lane");
    driver.settle(TEST_TURNS, || true);
    assert!(!ran.marked(), "a sibling prefix selected no registration");

    accept(&mut driver, trigger);
    driver
        .step_pass(WakeSource::Data)
        .expect("the covering teardown is in the lane");
    driver.settle(TEST_TURNS, || ran.marked());
}

// The no-output clause's behavioral neighbour. An ordinary finalizer runs to
// completion and the application observes nothing of it: no `update`
// invocation, no termination, and — through the pass that reflects its
// exit — no render and no re-evaluation, so it is not a source of redraw or
// of subscription dirt either.
//
// This is a regression row, not the clause's evidence. The evidence is the
// construction-site review named in this module's docs.
#[test]
fn an_ordinary_cleanup_run_is_invisible_to_the_application() {
    let ran = Beacon::default();
    let (mut driver, journal) = driver_with(
        Script::new(Command::batch([
            parking_effect([1]),
            finalizer(ran.clone()).scoped("pane"),
        ]))
        .replying([Command::teardown("pane")]),
        config().batch_max_messages(cap(1)),
    );
    let trigger = driver.boot().started[0].clone();

    accept(&mut driver, trigger);
    driver
        .step_pass(WakeSource::Data)
        .expect("the teardown trigger is in the lane");
    driver.settle(TEST_TURNS, || ran.marked());
    let (views, evaluations) = (journal.views(), journal.evaluations());

    let stepped = driver
        .step_pass(WakeSource::ProducerExit)
        .expect("the finished cleanup run's exit is observable");

    assert!(
        stepped.terminated.is_none(),
        "a cleanup run cannot originate a quit: it holds no control-lane sender"
    );
    assert_eq!(
        journal.reduced(),
        vec![1],
        "and no message of its reached update: it holds no data-lane sender"
    );
    assert_eq!(
        journal.views(),
        views,
        "its quiescence marks no redraw, so the pass that reflected it rendered nothing"
    );
    assert_eq!(
        journal.evaluations(),
        evaluations,
        "and marks no subscription dirt, so that pass re-evaluated nothing (RFC 0014 §5.2)"
    );
}

// INV-RC8's last clause, first half: termination is not a teardown. An
// armed registration is discarded rather than fired, and the discard happens
// at the immediate postcondition — before the settle that follows it, so
// there is no window in which the hook could start.
#[test]
fn termination_discards_an_unfired_registration() {
    let ran = Beacon::default();
    let (mut driver, _journal) = driver_with(
        Script::new(Command::batch([
            parking_effect([1]),
            finalizer(ran.clone()).scoped("pane"),
        ]))
        .replying([Command::quit()]),
        config().batch_max_messages(cap(1)),
    );
    let trigger = driver.boot().started[0].clone();

    accept(&mut driver, trigger);
    let stepped = driver
        .step_pass(WakeSource::Data)
        .expect("the quit trigger is in the lane");

    assert!(
        stepped.terminated.is_some(),
        "the update-returned quit terminated at its dispatch"
    );
    assert!(
        !ran.marked(),
        "termination fires no hooks: the settle it performed drained every runtime-owned task, \
         and no cleanup run was among them"
    );
}

// INV-RC8's last clause, second half: a cleanup run already in flight is
// cancelled like every other runtime-owned task, with no grace window. The
// reclamation is read from the guard the finalizer's body holds, which the
// abort drops.
#[test]
fn termination_cancels_a_running_cleanup_run() {
    let (started, reclaimed) = (Beacon::default(), Beacon::default());
    let (mut driver, _journal) = driver_with(
        Script::new(Command::batch([
            parking_effect([1, 2]),
            parked_finalizer(started.clone(), reclaimed.clone()).scoped("pane"),
        ]))
        .replying([Command::teardown("pane"), Command::quit()]),
        config().batch_max_messages(cap(1)),
    );
    let trigger = driver.boot().started[0].clone();

    accept(&mut driver, trigger.clone());
    driver
        .step_pass(WakeSource::Data)
        .expect("the teardown trigger is in the lane");
    driver.settle(TEST_TURNS, || started.marked());
    assert!(
        !reclaimed.marked(),
        "the finalizer is running and holding its guard"
    );

    accept(&mut driver, trigger);
    let stepped = driver
        .step_pass(WakeSource::Data)
        .expect("the quit trigger is in the lane");

    assert!(stepped.terminated.is_some());
    assert!(
        reclaimed.marked(),
        "termination cancelled the running cleanup run along with every other owned task"
    );
}

// INV-RC12 (a), the cleanup kind: a cleanup run in flight defers no
// subscription admission. The barrier's predicate reads subscription runs
// only, which is where the invariant's other half — a *stop-requested*
// cleanup run — lives: outside termination nothing stop-requests a cleanup
// run (a teardown excludes the kind, a cancel and a supersession address
// keyed slots, a re-evaluation addresses subscription runs), and at
// termination there is no admission site left for anything to defer. The
// reachable half is the one below.
#[test]
fn an_in_flight_cleanup_run_defers_no_subscription_admission() {
    let (started, reclaimed) = (Beacon::default(), Beacon::default());
    let source = ProbeSource::silent("feed");
    let (mut driver, _journal) = driver_with(
        Script::new(Command::batch([
            parking_effect([1, 2]),
            parked_finalizer(started.clone(), reclaimed.clone()).scoped("pane"),
        ]))
        .replying([Command::teardown("pane")])
        .feeding([Feed::new(source.clone())])
        .wanting([])
        .redeclaring(2, ["feed"]),
        config().batch_max_messages(cap(1)),
    );
    let trigger = driver.boot().started[0].clone();
    assert_eq!(source.admissions(), 0, "the feed is not declared yet");

    accept(&mut driver, trigger.clone());
    driver
        .step_pass(WakeSource::Data)
        .expect("the teardown trigger is in the lane");
    driver.settle(TEST_TURNS, || started.marked());

    accept(&mut driver, trigger);
    driver
        .step_pass(WakeSource::Data)
        .expect("the re-declaring trigger is in the lane");

    assert_eq!(
        source.admissions(),
        1,
        "the re-evaluation admitted while a cleanup run was in flight"
    );
    assert!(
        !reclaimed.marked(),
        "and that run was still in flight when it did"
    );
}

// RFC 0014 §3.4's spawn-phase rule, and §11's *cancel-phase cleanup
// registration* adversary: one command tears a prefix down and registers a
// new hook under it. Because registration applies in the spawn phase, the
// teardown consumes the **old** occupant's hook and leaves the new one
// armed — a plan that registered in the cancel phase would consume the
// registration it had just armed, and the new occupant would be left with
// none.
#[test]
fn a_teardown_and_reregister_command_consumes_the_old_hook_and_arms_the_new_one() {
    let (first, second) = (Beacon::default(), Beacon::default());
    let (mut driver, _journal) = driver_with(
        Script::new(Command::batch([
            parking_effect([1, 2]),
            finalizer(first.clone()).scoped("pane"),
        ]))
        .replying([
            // The shape a combinator produces: the boundary's teardown of
            // its own segment, merged with the child's command already
            // qualified by that segment.
            Command::batch([
                Command::teardown("pane"),
                finalizer(second.clone()).scoped("pane"),
            ]),
            Command::teardown("pane"),
        ]),
        config().batch_max_messages(cap(1)),
    );
    let trigger = driver.boot().started[0].clone();

    accept(&mut driver, trigger.clone());
    driver
        .step_pass(WakeSource::Data)
        .expect("the teardown-and-reregister trigger is in the lane");
    driver.settle(TEST_TURNS, || first.marked());
    assert!(
        !second.marked(),
        "the registration this same command armed was not consumed by its own teardown"
    );

    accept(&mut driver, trigger);
    driver
        .step_pass(WakeSource::Data)
        .expect("the second teardown is in the lane");
    driver.settle(TEST_TURNS, || second.marked());

    assert_eq!(first.marks(), 1, "and the old hook fired exactly once");
}

// The cleanup window's placement (RFC 0013 §1.2, §5): a finalizer starts at
// the head of the stop-requested→quiesced interval and runs *concurrently*
// with the quiescence of the runs the same application point stopped —
// nothing waits for anything. Both the torn-down run's reclamation and the
// finalizer's completion are reached by the same bounded settle.
#[test]
fn a_finalizer_runs_concurrently_with_the_quiescence_it_accompanies() {
    let (ran, reclaimed) = (Beacon::default(), Beacon::default());
    let (mut driver, _journal) = driver_with(
        Script::new(Command::batch([
            parking_effect([1]),
            super::support::holding_effect(reclaimed.clone()).scoped("pane"),
            finalizer(ran.clone()).scoped("pane"),
        ]))
        .replying([Command::teardown("pane")]),
        config().batch_max_messages(cap(1)),
    );
    let trigger = driver.boot().started[0].clone();

    accept(&mut driver, trigger);
    driver
        .step_pass(WakeSource::Data)
        .expect("the teardown trigger is in the lane");

    driver.settle(TEST_TURNS, || ran.marked() && reclaimed.marked());
}
