//! `both quit semantics` (RFC 0014 §13.1), carrying INV-RC9's rows.
//!
//! Two physical routes, and the series drives both through the production
//! seams: an `update`-returned quit, applied synchronously at the dispatch
//! that returned it, and a producer-originated quit, released through the
//! send gate exactly as a producer's messages are (RFC 0008 §9.6) and
//! applied at the first control drain at or after its arrival.
//!
//! **Two of INV-RC9's rows are not reachable from this surface, for one
//! structural reason.** The *mid-batch* row ("a quit arriving later in a
//! pass is preceded only by the in-progress batch's remainder") and the
//! *cancel-beats-quit* row ("a buffered quit whose origin is revoked is
//! discarded") both need a control-lane arrival that lands **strictly
//! inside** a pass. A pass is a synchronous region here: RFC 0014 §3.5's
//! four stages run to completion without the driving task yielding, and the
//! executor the determinism claim is scoped to (RFC 0008 §9.8) has one
//! thread, so no producer can commit while a pass is running. The window is
//! empty rather than unexercised. It is reachable in production on a
//! multi-worker executor, which is outside INV-RC14's verified range, and no
//! sleep, stage probe, or kernel change would give it to a pass-unit script.
//!
//! What *is* driven for those two: the pass-boundary neighbour of the
//! mid-batch bound (below), which witnesses the bound's observable content —
//! a batch bounded by the cap, then the next pass's control drain applying
//! the quit with no further input — minus the mid-pass arrival itself. The
//! cancel-beats-quit row has no such neighbour and is left to the
//! implementation-acceptance tier.

use crate::command::Command;
use crate::kernel::arbiter::WakeSource;
use crate::reducer::Exit;
use crate::testing::driver::NotReady;

use super::support::{
    Script, accept, cap, config, driver, driver_with, parking_effect, quitting_effect,
    silent_effect,
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

// INV-RC9's mid-batch bound, at the pass boundary — the constructible
// neighbour of the row this module's header says is unreachable. The batch
// that ran is bounded by the cap, the quit is committed after it, and the
// next pass's control drain applies it before any further batch begins: no
// input at all follows the arrival. What this does *not* witness is the
// arrival landing inside the batch; the bound's remainder clause is
// therefore exercised at its degenerate value here, and the row stays open.
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
