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
//! | termination discards unfired hooks | [`termination_discards_an_unfired_registration`], [`a_final_update_that_tears_down_and_quits_fires_no_hook_from_termination`] |
//! | termination cancels running ones | [`termination_cancels_a_running_cleanup_run`] |
//!
//! RFC 0013 §7.2's own two cleanup rows are
//! [`a_teardown_starts_the_prefix_s_registration_at_its_application_point`]
//! and
//! [`a_teardown_and_reregister_command_consumes_the_old_hook_and_arms_the_new_one`];
//! its final-update row is the one named above, which returns the teardown
//! and the quit from **one** update.
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
//! # The mutation run against the spawn-phase rows
//!
//! §11's *cancel-phase cleanup registration* adversary is a plan that
//! satisfies every other row here, so the rows that exclude it were checked
//! by mutation. Moving the registration entries into the cancel phase —
//! chained with the explicit cancels, ahead of the teardowns — leaves 180
//! rows passing and fails exactly three: this module's
//! [`a_teardown_and_reregister_command_consumes_the_old_hook_and_arms_the_new_one`],
//! and the two plan-level rows beside the lowering,
//! `kernel::lowering::tests::a_teardown_and_reregister_command_arms_the_new_hook_after_the_teardown`
//! and `kernel::lowering::tests::the_quit_still_completes_a_command_that_also_arms_a_hook`.
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
    holding_effect, marking_effect, parking_effect,
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

// INV-RC12 (a)'s cleanup **neighbour**, which is the reachable statement: a
// cleanup run *in flight* defers no subscription admission.
//
// The clause itself says something else and has no row anywhere, by
// construction rather than by omission. It is about a **stop-requested**
// cleanup run, and outside termination nothing stop-requests one — a
// teardown excludes the kind, a cancel and a supersession address keyed
// slots, a re-evaluation addresses subscription runs — while at termination
// no admission site is left for one to defer. Its carrier is structural: the
// barrier's predicate reads subscription runs only, so it can never see a
// cleanup run whatever the run's phase. The row below is the neighbour, not
// a weaker form of the clause.
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
// nothing waits for anything.
//
// **What is constructible here, and what is not.** The clause claims
// concurrency, not an order, and the constructible content is two facts.
// First, the start does not *follow* quiescence: a pass is a synchronous
// region, so when `step_pass` returns — the teardown having applied inside
// it — the torn-down run has not been dismantled yet, and the finalizer was
// already spawned. Second, neither blocked on the other: one bounded settle
// reaches both. A *strict* interleaving — the finalizer's own progress
// before the quiescence — is not constructible and is not claimed: the
// abort is issued before the cleanup spawn, so this executor dismantles the
// aborted task first, and a row asserting the reverse would be asserting
// against the contract rather than for it.
#[test]
fn a_finalizer_starts_before_the_quiescence_it_accompanies_and_waits_for_none_of_it() {
    let (ran, reclaimed) = (Beacon::default(), Beacon::default());
    let (mut driver, _journal) = driver_with(
        Script::new(Command::batch([
            parking_effect([1]),
            holding_effect(reclaimed.clone()).scoped("pane"),
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

    assert!(
        !reclaimed.marked(),
        "the pass returned with the torn-down run not yet quiesced, so the finalizer this pass \
         started did not begin after that quiescence"
    );
    driver.settle(TEST_TURNS, || ran.marked() && reclaimed.marked());
}

// RFC 0013 §7.2's final-update row: a teardown and a quit returned by the
// **same** update. The teardown applies in the cancel phase and the quit at
// the dispatch's completion, so this is the one command in which both
// halves of §6 meet.
//
// What the row pins is that termination fires no hook. The registration the
// teardown covered was consumed and started by the teardown — that is the
// teardown's doing, not termination's — and the registration under a prefix
// the teardown does *not* cover is discarded unfired. Meanwhile RFC 0011
// §4.4's two postconditions hold unchanged: the step reports the
// termination, and the settle that follows it has reclaimed every
// runtime-owned task, the cleanup run this same command started included.
#[test]
fn a_final_update_that_tears_down_and_quits_fires_no_hook_from_termination() {
    let (cleanup_reclaimed, elsewhere, occupant) =
        (Beacon::default(), Beacon::default(), Beacon::default());
    let (mut driver, _journal) = driver_with(
        Script::new(Command::batch([
            parking_effect([1]),
            holding_effect(occupant.clone()).scoped("pane"),
            // Its own start is not the subject here — termination may
            // cancel it before its first poll — so only the guard it holds
            // is read.
            parked_finalizer(Beacon::default(), cleanup_reclaimed.clone()).scoped("pane"),
            finalizer(elsewhere.clone()).scoped("other-pane"),
        ]))
        .replying([Command::batch([Command::teardown("pane"), Command::quit()])]),
        config().batch_max_messages(cap(1)),
    );
    let trigger = driver.boot().started[0].clone();

    accept(&mut driver, trigger);
    let stepped = driver
        .step_pass(WakeSource::Data)
        .expect("the trigger is in the lane");

    assert!(
        stepped.terminated.is_some(),
        "the quit applied at the completion of the update that also tore down"
    );
    assert!(
        !elsewhere.marked(),
        "termination is not a teardown: the registration outside the torn-down prefix was \
         discarded rather than fired"
    );
    assert!(
        occupant.marked(),
        "the quiescent postcondition reclaimed the torn-down run"
    );
    assert!(
        cleanup_reclaimed.marked(),
        "and the cleanup run the teardown had just started, with no grace window"
    );
}
