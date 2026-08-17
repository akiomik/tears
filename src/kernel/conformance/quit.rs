//! `both quit semantics` (RFC 0014 §13.1), carrying INV-RC9's rows.
//!
//! Two physical routes, and the series drives both through the production
//! seams: an `update`-returned quit, applied synchronously at the dispatch
//! that returned it, and a producer-originated quit, released through the
//! send gate exactly as a producer's messages are (RFC 0008 §9.6) and
//! applied at the first control drain at or after its arrival.
//!
//! **Two of INV-RC9's rows need a window a single-threaded executor does not
//! have**, and they get it from the harness rather than from a weaker claim.
//! The *mid-batch* case ("a quit arriving later in a pass is preceded only by
//! the in-progress batch's remainder") and the *cancel-beats-quit* case ("a
//! buffered quit whose origin is revoked is discarded") both require a
//! control-lane arrival that lands **strictly inside** a pass. A pass is a
//! synchronous region — RFC 0014 §3.5's four stages run without the driving
//! task yielding — so the producer has to be running on a thread the pass is
//! not occupying.
//!
//! Those two rows therefore drive a multi-worker executor, and **their
//! determinism is their own**: the commit is pinned to a stage boundary by
//! [`MidBatchHandshake`], an application-side rendezvous that neither side
//! can pass, never by the scheduler. They cite no part of INV-RC14, whose
//! scripted-determinism claim RFC 0008 §9.8 scopes to the current-thread
//! range. What they do keep is pass-unit driving — one step is still one
//! whole pass in the fixed stage order — so they stay inside the evidence
//! surface RFC 0008 §9.9 names, under the same citation rule as every other
//! series here.

use crate::command::Command;
use crate::kernel::arbiter::WakeSource;
use crate::reducer::Exit;
use crate::testing::driver::{Confirmed, NotReady};

use super::support::{
    Call, MidBatchHandshake, Script, THREADED_TURNS, accept, accept_within, cap, config, driver,
    driver_with, gated_quitting_effect, parking_effect, quitting_effect, silent_effect,
    threaded_driver_with,
};

// --- the synchronous route ------------------------------------------------

// INV-RC9's first clause: an `update`-returned quit terminates at its
// dispatch's completion with **no intervening input processed**. The batch
// that carried it is already dequeued, and the messages behind the quit in
// that same batch are never reduced.
#[test]
fn an_update_returned_quit_terminates_with_no_further_input_processed() {
    let (mut driver, journal) = driver_with(
        Script::new(parking_effect([1, 7, 2])).replying([
            Command::none(),
            Command::quit(),
            Command::none(),
        ]),
        config().batch_max_messages(cap(3)),
    );
    let run = driver.boot().started[0].clone();

    for _ in 0..3 {
        accept(&mut driver, run.clone());
    }

    let report = driver
        .step_pass(WakeSource::Data)
        .expect("the three granted sends are in the lane");

    assert_eq!(
        report.terminated.map(|result| result.map_err(drop)),
        Some(Ok(Exit::Quit)),
        "the quit is the production result of the pass that dispatched it"
    );
    assert_eq!(
        journal.reduced(),
        vec![1, 7],
        "the batch stopped at the update that returned the quit: the message behind it in the \
         same batch was never reduced"
    );
}

// The bootstrap corner of the same route (RFC 0014 §6.2, INV-RC11): an init
// command carrying a quit terminates during the init dispatch, before the
// initial reconcile and before any render — so `boot` returns with the
// termination set and its continuation pass never ran.
#[test]
fn an_init_quit_terminates_before_the_reconcile_and_before_any_render() {
    let (mut driver, journal) = driver(Script::new(Command::batch([
        silent_effect(),
        Command::quit(),
    ])));

    let report = driver.boot();

    assert_eq!(
        report.terminated.map(|result| result.map_err(drop)),
        Some(Ok(Exit::Quit)),
        "the init quit is the production result"
    );
    assert_eq!(
        journal.evaluations(),
        0,
        "no initial reconcile follows an init quit"
    );
    assert_eq!(journal.views(), 0, "and no render");
    assert_eq!(
        report.started.len(),
        1,
        "the sibling spawned before the quit is still a run this step started, and termination \
         tears it down"
    );
}

// --- the producer route ---------------------------------------------------

// INV-RC9's pass-start row, under a flooded data lane: a producer quit that
// has arrived when a pass begins is applied by that pass's control drain
// with **zero** further inputs processed, however much input is ready behind
// it. This is also the backlog-independence claim — the control drain
// precedes the input batch in every pass, so the quit never waits behind the
// data lane's queue.
#[test]
fn a_producer_quit_arrived_at_pass_start_is_applied_with_zero_inputs_processed() {
    let (mut driver, journal) = driver_with(
        Script::new(Command::batch([
            parking_effect([1, 2, 3, 4]),
            quitting_effect(),
        ])),
        config().batch_max_messages(cap(2)),
    );
    let report = driver.boot();
    let (flood, quitter) = (report.started[0].clone(), report.started[1].clone());

    for _ in 0..4 {
        accept(&mut driver, flood.clone());
    }
    accept(&mut driver, quitter);

    let stepped = driver
        .step_pass(WakeSource::Control)
        .expect("the quit is on the control lane");

    assert_eq!(
        stepped.terminated.map(|result| result.map_err(drop)),
        Some(Ok(Exit::Quit)),
        "the arrived quit terminated the pass it began"
    );
    assert!(
        journal.reduced().is_empty(),
        "zero inputs were processed, with a full batch ready behind the quit"
    );
}

// The same bound named by the other wake source: a pass begun by the data
// lane still drains control first, so a quit that arrived before it wins
// over the input that arrived with it. This is RFC 0014 §3.5's plainly
// stated consequence — an input whose `update` would have cancelled the
// quit's origin does not run first.
#[test]
fn a_pass_begun_by_the_data_lane_still_drains_the_arrived_quit_first() {
    let (mut driver, journal) = driver(Script::new(Command::batch([
        parking_effect([1]),
        quitting_effect(),
    ])));
    let report = driver.boot();
    let (sender, quitter) = (report.started[0].clone(), report.started[1].clone());

    accept(&mut driver, sender);
    accept(&mut driver, quitter);

    let stepped = driver
        .step_pass(WakeSource::Data)
        .expect("the message is on the data lane");

    assert!(
        stepped.terminated.is_some(),
        "the pass the data lane began applied the quit at its control drain"
    );
    assert!(
        journal.reduced().is_empty(),
        "the ready input did not precede the quit"
    );
}

// The between-passes case, which the mid-batch row below does not cover: a
// quit committed after a capped batch has ended is applied by the next
// pass's control drain before any further batch begins, so the number of
// inputs that follow its arrival is zero. Its remainder is the degenerate
// one — the batch had already ended when the quit arrived — which is exactly
// what makes it a different row from the mid-batch case rather than a
// weaker reading of it.
#[test]
fn a_quit_committed_after_a_capped_batch_applies_before_the_next_batch_begins() {
    let (mut driver, journal) = driver_with(
        Script::new(Command::batch([
            parking_effect([1, 2, 3, 4]),
            quitting_effect(),
        ])),
        config().batch_max_messages(cap(2)),
    );
    let report = driver.boot();
    let (flood, quitter) = (report.started[0].clone(), report.started[1].clone());

    for _ in 0..4 {
        accept(&mut driver, flood.clone());
    }
    driver
        .step_pass(WakeSource::Data)
        .expect("the flood is in the lane");
    assert_eq!(
        journal.reduced(),
        vec![1, 2],
        "the in-progress batch ended after the cap"
    );

    accept(&mut driver, quitter);
    let stepped = driver
        .step_pass(WakeSource::Control)
        .expect("the quit is on the control lane");

    assert!(
        stepped.terminated.is_some(),
        "the quit was applied at the first control drain at or after its arrival"
    );
    assert_eq!(
        journal.reduced(),
        vec![1, 2],
        "and the next input batch never began"
    );
}

// The revocation clause of the same route, in its constructible direction: a
// quit whose origin is revoked *before the quit is ever released* puts
// nothing on the control lane at all, so the kernel keeps running. The
// producer is torn down at the gate, where the release it was waiting for
// can no longer be taken.
#[test]
fn a_quit_whose_origin_is_revoked_at_the_gate_never_reaches_the_control_lane() {
    let (mut driver, journal) = driver_with(
        Script::new(Command::batch([
            parking_effect([1]),
            quitting_effect().scoped("pane"),
        ]))
        .replying([Command::teardown("pane")]),
        config().batch_max_messages(cap(1)),
    );
    let report = driver.boot();
    let trigger = report.started[0].clone();

    accept(&mut driver, trigger);
    assert_eq!(
        driver.intents().len(),
        2,
        "both producers reached their send; the quitter is held at the gate"
    );

    let stepped = driver
        .step_pass(WakeSource::Data)
        .expect("the trigger's message is in the lane");
    assert!(
        stepped.terminated.is_none(),
        "the teardown revoked the quitter before it was ever released"
    );
    assert_eq!(journal.reduced(), vec![1], "and the pass ran normally");

    assert_eq!(
        driver.step_pass(WakeSource::Control).err(),
        Some(NotReady),
        "nothing ever reached the control lane, so no quit can be applied"
    );
}

// --- the mid-batch window ---------------------------------------------------

// INV-RC9's mid-batch row, in full: the quit's send commits **during** the
// in-progress input batch, and only that batch's remainder — bounded by the
// cap — precedes its application, which happens at the next pass's control
// drain with no further batch beginning.
//
// The arrival is a literal reading of the journal rather than an inference.
// `update` for the first message opens the gate and blocks until the gated
// producer reports its commit, then records `Committed`; the batch's next
// message follows. So `Reduce(1), Committed, Reduce(2)` is the interleaving,
// and the handshake — not the scheduler — is what produced it.
#[test]
fn a_quit_committed_mid_batch_is_preceded_only_by_that_batch_s_remainder() {
    let (handshake, gate) = MidBatchHandshake::new();
    let (mut driver, journal) = threaded_driver_with(
        Script::new(Command::batch([
            parking_effect([1, 2, 3, 4]),
            gated_quitting_effect(Vec::new(), gate),
        ]))
        .handshaking_on(1, handshake),
        config().batch_max_messages(cap(2)),
    );
    let report = driver.boot();
    let (flood, quitter) = (report.started[0].clone(), report.started[1].clone());

    for _ in 0..4 {
        accept_within(&mut driver, flood.clone(), THREADED_TURNS);
    }

    // Banked before the pass and outstanding across it: this is the release
    // the quitter takes when the reducer opens its gate, mid-batch. Holding
    // a token across a step is what the token's detachment is for.
    let banked = driver.grant(quitter).expect("the previous grant resolved");
    let stepped = driver
        .step_pass(WakeSource::Data)
        .expect("the flood is in the lane");

    assert!(
        stepped.terminated.is_none(),
        "a quit arriving mid-pass is not applied by the pass it arrived in"
    );
    assert_eq!(
        driver.confirm(THREADED_TURNS, banked),
        Confirmed::Accepted,
        "the quit reached the control lane while the batch was running"
    );
    assert_eq!(
        journal.calls(),
        vec![
            Call::Init,
            Call::Subscriptions,
            Call::View,
            Call::Reduce(1),
            Call::Committed,
            Call::Reduce(2),
            Call::View,
            Call::Subscriptions,
        ],
        "the arrival landed inside the batch, and only the remainder followed it"
    );

    let stepped = driver
        .step_pass(WakeSource::Control)
        .expect("the quit is on the control lane");
    assert!(
        stepped.terminated.is_some(),
        "applied at the first control drain at or after its arrival"
    );
    assert_eq!(
        journal.reduced(),
        vec![1, 2],
        "and the next input batch never began"
    );
}

// INV-RC9's cancel-beats-quit row: a producer quit that is already buffered
// on the control lane when its origin is revoked is **discarded**, not
// applied, and the kernel keeps running. The revocation is the teardown the
// same `update` returns — after the handshake, so the commit is strictly
// first — which is RFC 0003 INV-9's intent preserved through origin
// revocation rather than through a private channel's drop.
#[test]
fn a_quit_buffered_before_its_origin_s_revocation_is_discarded() {
    let (handshake, gate) = MidBatchHandshake::new();
    let (mut driver, journal) = threaded_driver_with(
        Script::new(Command::batch([
            parking_effect([5, 6, 7]),
            gated_quitting_effect(Vec::new(), gate).scoped("quit"),
        ]))
        .handshaking_on(5, handshake)
        .replying([Command::teardown("quit")]),
        config().batch_max_messages(cap(2)),
    );
    let report = driver.boot();
    let (feed, quitter) = (report.started[0].clone(), report.started[1].clone());

    for _ in 0..3 {
        accept_within(&mut driver, feed.clone(), THREADED_TURNS);
    }

    let banked = driver.grant(quitter).expect("the previous grant resolved");
    let stepped = driver
        .step_pass(WakeSource::Data)
        .expect("the feed is in the lane");

    assert!(
        stepped.terminated.is_none(),
        "the quit was not applied here"
    );
    assert_eq!(
        driver.confirm(THREADED_TURNS, banked),
        Confirmed::Accepted,
        "the quit was buffered before its origin was revoked"
    );
    assert_eq!(
        journal.calls()[3..6],
        [Call::Reduce(5), Call::Committed, Call::Reduce(6)],
        "the commit landed between the teardown's own message and the batch's next"
    );

    let stepped = driver
        .step_pass(WakeSource::Control)
        .expect("the buffered quit is on the control lane");
    assert!(
        stepped.terminated.is_none(),
        "a revoked origin's quit never terminates the application"
    );
    assert_eq!(
        journal.reduced(),
        vec![5, 6, 7],
        "and the kernel kept delivering past it"
    );
}
