//! Delivery: `cancel vs buffered output` and `simultaneous readiness`
//! (RFC 0014 §13.1), with INV-RC10's flood rows beside them.
//!
//! Every claim here is read where INV-RC5 is checked — on the pass that
//! dequeues, from the application's own record of what `update` was invoked
//! with (RFC 0008 §9.11). Neither ledger witnesses revocation: `accepted`
//! records what passed the *gate*, and a revoked run's committed send
//! belongs in it, which is exactly why "the item was accepted" and "the item
//! was delivered" are asserted from different places here.
//!
//! **Not reachable from this surface**: INV-RC5's *buffered-quit* half — a
//! producer quit already on the control lane whose origin is then revoked.
//! Revocation is applied by `update`, which runs in stage 3 of a pass, and
//! the control drain that would discard the quit is stage 2 of the *same*
//! pass; on the current-thread executor the driver owns, a pass is a
//! synchronous region, so a control arrival can never land strictly inside
//! one. The window has measure zero here rather than being unexercised, and
//! no sleep, stage probe, or kernel change would give it to a pass-unit
//! script. The message half below is the constructible half of the same
//! invariant.

use crate::command::{Command, CommandId};
use crate::kernel::arbiter::WakeSource;
use crate::testing::driver::{NotReady, SendRecord};

use super::support::{
    Beacon, Call, Script, TEST_TURNS, accept, cap, config, driver, driver_with, finishing_effect,
    marking_effect, parking_effect,
};

// --- `cancel vs buffered output` ------------------------------------------

// INV-RC5's core row: from the revocation's application point no output of
// the revoked run is delivered to `update` — the item buffered *before* it
// included — while a live origin's item queued behind it still is. The
// counterexample this excludes is the §11 filter-at-update model: an
// implementation that filters where `update` is called rather than at the
// dequeue passes an "applied messages" check and still runs `update`, so the
// assertion is over the invocation itself, which the reducer records for
// itself.
#[test]
fn a_revoked_run_s_buffered_output_never_reaches_update() {
    let worker = CommandId::new("worker");
    let (mut driver, journal) = driver_with(
        Script::new(Command::batch([
            parking_effect([1, 2]),
            parking_effect([10]).cancellable(worker.clone()),
        ]))
        .replying([Command::cancel(worker)]),
        config().batch_max_messages(cap(1)),
    );
    let report = driver.boot();
    let (trigger, revoked) = (report.started[0].clone(), report.started[1].clone());

    // The handshake fixes the enqueue order: the cancel trigger is ahead of
    // the run it cancels, and that run's own output is ahead of the live
    // origin's next message.
    accept(&mut driver, trigger.clone());
    accept(&mut driver, revoked.clone());
    accept(&mut driver, trigger);
    assert_eq!(
        driver.accepted().len(),
        3,
        "all three sends passed the gate: admission is not delivery"
    );

    driver
        .step_pass(WakeSource::Data)
        .expect("the granted sends are in the lane");
    assert_eq!(journal.reduced(), vec![1], "the cancel trigger was applied");

    driver
        .step_pass(WakeSource::Data)
        .expect("the revoked run's buffered item is next");
    assert_eq!(
        journal.reduced(),
        vec![1],
        "the buffered item of the revoked run ran no update at all"
    );

    driver
        .step_pass(WakeSource::Data)
        .expect("the live origin's item is queued behind it");
    assert_eq!(
        journal.reduced(),
        vec![1, 2],
        "and the live origin's item queued behind the revoked one still delivers"
    );
    assert_eq!(
        driver.accepted().len(),
        3,
        "the ledger still records the revoked send: it says what passed the gate, not what \
         `update` saw"
    );
    assert!(
        driver
            .accepted()
            .iter()
            .map(SendRecord::run)
            .any(|run| *run == revoked),
        "including the record naming the run whose output was retracted"
    );
}

// The natural-finish control of the same invariant: a run that finished on
// its own is not revoked, so its buffered output is still delivered — the
// entry outlives the task as a tombstone precisely to carry it.
#[test]
fn a_naturally_finished_run_s_buffered_output_is_still_delivered() {
    let done = Beacon::default();
    let (mut driver, journal) = driver(Script::new(finishing_effect(vec![9], done.clone())));
    let run = driver.boot().started[0].clone();

    accept(&mut driver, run);
    driver.settle(TEST_TURNS, || done.marked());

    driver
        .step_pass(WakeSource::ProducerExit)
        .expect("the finished run's exit is observable");
    assert_eq!(
        journal.reduced(),
        vec![9],
        "the exit reflected in the same pass's first stage retracted nothing"
    );
}

// The late-task-exit adversary: the revoked run's exit is reflected while
// its committed envelope is still in the lane, so the entry survives its own
// task as a tombstone and the envelope is filtered when the batch reaches it
// — an implementation that retired the entry at the exit would panic at that
// dequeue instead.
#[test]
fn a_revoked_run_s_exit_lands_before_its_buffered_item_is_filtered() {
    let worker = CommandId::new("worker");
    let reclaimed = Beacon::default();
    let (mut driver, journal) = driver_with(
        Script::new(Command::batch([
            parking_effect([1]),
            finishing_effect(vec![10], reclaimed.clone()).cancellable(worker.clone()),
        ]))
        .replying([Command::cancel(worker)]),
        config().batch_max_messages(cap(1)),
    );
    let report = driver.boot();
    let (trigger, revoked) = (report.started[0].clone(), report.started[1].clone());

    accept(&mut driver, trigger);
    accept(&mut driver, revoked);
    driver.settle(TEST_TURNS, || reclaimed.marked());

    // One pass reflects the finished run's exit at stage 1 and revokes it at
    // stage 3: the entry is a tombstone by the time the cancel reaches it,
    // which is the late-exit ordering this row is about.
    driver
        .step_pass(WakeSource::Data)
        .expect("the cancel trigger is in the lane");
    assert_eq!(journal.reduced(), vec![1], "the cancel applied");
    assert_eq!(
        driver.step_pass(WakeSource::ProducerExit).err(),
        Some(NotReady),
        "the run's exit was already reflected by that pass's first stage"
    );

    driver
        .step_pass(WakeSource::Data)
        .expect("the tombstone's buffered item is still in the lane");
    assert_eq!(
        journal.reduced(),
        vec![1],
        "an exit reflected before the revocation left the buffered item filterable, not          deliverable"
    );
}

// --- `simultaneous readiness`, the producer face --------------------------

// With both producers parked at their send intent, the sequential grant
// handshake alone fixes the enqueue order, and each script replays to an
// identical observation sequence (INV-RC14). Raw grant order is not
// expressible: a second grant is refused driver-wide until the first
// resolves, so the *only* thing that ordered these two releases is the
// handshake.
#[test]
fn the_grant_handshake_alone_orders_two_ready_producers() {
    let forward = producer_face(0, 1);
    assert_eq!(forward, producer_face(0, 1), "one script, one sequence");
    let reversed = producer_face(1, 0);
    assert_eq!(reversed, producer_face(1, 0), "and so for the other");

    assert_eq!(
        forward,
        vec![100, 200],
        "delivery follows the scripted order"
    );
    assert_eq!(
        reversed,
        vec![200, 100],
        "and the reversed script reverses it"
    );
}

/// One run of the producer face: both producers wait at the gate, and the
/// script releases `first` then `second`.
fn producer_face(first: usize, second: usize) -> Vec<u8> {
    let (mut driver, journal) = driver(Script::new(Command::batch([
        parking_effect([100]),
        parking_effect([200]),
    ])));
    let report = driver.boot();
    assert_eq!(
        driver.intents().len(),
        2,
        "both producers reached their send and are held at the gate"
    );
    assert!(
        driver.accepted().is_empty(),
        "and no producer output reaches a lane before a grant releases it"
    );

    let (first, second) = (
        report.started[first].clone(),
        report.started[second].clone(),
    );
    accept(&mut driver, first);
    accept(&mut driver, second);
    driver
        .step_pass(WakeSource::Data)
        .expect("both releases are in the lane");
    journal.reduced()
}

// --- `simultaneous readiness`, the pass-initiation face -------------------

// With two wake sources simultaneously arrived on one kernel, the script
// picks which begins the next pass; each script replays identically, and —
// the pass being RFC 0014 §3.5's fixed pipeline rather than a set of
// arbitrated branches — the choice does not fork the observation sequence.
//
// The citation rule (RFC 0008 §9.9) bounds what this shows: the order a
// driver establishes is never evidence of a production order, and which
// source production picks among several arrived at once stays unobserved
// here. What is shown is that neither choice is refused — both sources had
// arrived — and that neither changes what the pass does.
#[test]
fn either_arrived_source_begins_the_same_pass() {
    let by_data = initiation_face(WakeSource::Data);
    assert_eq!(
        by_data,
        initiation_face(WakeSource::Data),
        "one script, one sequence"
    );
    let by_exit = initiation_face(WakeSource::ProducerExit);
    assert_eq!(
        by_exit,
        initiation_face(WakeSource::ProducerExit),
        "and so for the other"
    );

    assert_eq!(
        by_data, by_exit,
        "the fixed pass pipeline makes the initiation choice observably inconsequential"
    );
}

/// One run of the initiation face: a finished run's exit and a committed
/// message are both arrived, and `woken_by` names which begins the pass.
fn initiation_face(woken_by: WakeSource) -> Vec<Call> {
    let done = Beacon::default();
    let (mut driver, journal) = driver(Script::new(Command::batch([
        marking_effect(done.clone()),
        parking_effect([7]),
    ])));
    let sender = driver.boot().started[1].clone();

    driver.settle(TEST_TURNS, || done.marked());
    accept(&mut driver, sender);

    driver
        .step_pass(woken_by)
        .expect("both sources had arrived, so either is admitted");
    journal.calls()
}

// --- INV-RC10: flood properties -------------------------------------------

// Under a continuously ready producer: every other producer's enqueued
// output is delivered after exactly the FIFO prefix ahead of it — no
// starvation — the backlog takes a backlog-proportional number of passes
// each consuming up to the batch cap, and a redraw a batch marks is rendered
// before the next input batch begins. The interposed probe is the second
// producer's message, and the render observation is the application's own
// `view` count: the driver reports nothing about frames.
#[test]
fn a_flood_delivers_the_fifo_prefix_and_renders_between_its_batches() {
    let (mut driver, journal) = driver_with(
        Script::new(Command::batch([
            parking_effect([1, 2, 3, 4]),
            parking_effect([9]),
        ])),
        config().batch_max_messages(cap(2)),
    );
    let report = driver.boot();
    let (flood, probe) = (report.started[0].clone(), report.started[1].clone());
    assert_eq!(journal.views(), 1, "the bootstrap render, and no other yet");

    for _ in 0..4 {
        accept(&mut driver, flood.clone());
    }
    accept(&mut driver, probe);

    for pass in 1..=3 {
        driver
            .step_pass(WakeSource::Data)
            .expect("the backlog is still ready");
        assert_eq!(
            journal.views(),
            pass + 1,
            "each pass ends in exactly one render, so the flood cannot suppress one"
        );
    }
    assert_eq!(
        driver.step_pass(WakeSource::Data).err(),
        Some(NotReady),
        "a backlog of five under a cap of two takes three passes and no more"
    );

    assert_eq!(
        journal.reduced(),
        vec![1, 2, 3, 4, 9],
        "the interposed probe's message was delivered after exactly the FIFO prefix ahead of it"
    );
    assert_eq!(
        journal.calls(),
        vec![
            Call::Init,
            Call::Subscriptions,
            Call::View,
            Call::Reduce(1),
            Call::Reduce(2),
            Call::View,
            Call::Subscriptions,
            Call::Reduce(3),
            Call::Reduce(4),
            Call::View,
            Call::Subscriptions,
            Call::Reduce(9),
            Call::View,
            Call::Subscriptions,
        ],
        "every batch's redraw is rendered before the next batch begins"
    );
}
