//! `bounded-lane revocation` (RFC 0014 §13.1), and the two bounded
//! handshake rows it rests on.
//!
//! **Scope, stated before the rows.** This file runs INV-RC5's bounded-lane
//! half over the single data lane. It scripts the bounded lane through the
//! grant handshake **for enqueue order only** and claims no bounded-lane
//! determinism beyond that: the general claim needs its own verification
//! pass and the two protocol conditions RFC 0014 §13.3 names, and until that
//! lands the evidence here is evidence for revocation under a bounded lane
//! and nothing more.
//!
//! Both of those conditions are exercised here all the same, because a full
//! lane cannot be scripted without them. **Driver progress**: the driver
//! stays steppable while a grant's acceptance is outstanding, so the test
//! holds a token across the `step_pass` that drains the lane the send waits
//! on — which the token's detachment is for. **Ack correlation**: at most one
//! grant is outstanding driver-wide, so no acceptance can be some other
//! release's.
//!
//! **Each capacity claim here is a pair of rows, and it needs both.** A
//! positive row shows a release committing after a pass drained the lane; on
//! its own that is compatible with a release that would have committed
//! anyway, because `grant` turns nothing and an unreached ledger reads the
//! same as an unoffered one. The negative row is where the budget is spent
//! with the lane still full, and it is what makes "only after a dequeue" a
//! claim rather than a restatement of the script's order.

use crate::command::{Command, CommandId};
use crate::kernel::arbiter::WakeSource;
use crate::testing::driver::Confirmed;

use super::support::{Script, TEST_TURNS, accept, cap, config, driver_with, parking_effect};

// The series proper. A two-slot lane is exactly full with a revoked run's
// item still in it, and the revocation frees no capacity: what frees it is
// the delivery-side dequeue, which is also where the item is filtered — no
// `update` invocation for it — while the live origin's item queued behind it
// still delivers.
//
// The capacity claim is read from the handshake rather than asserted about
// the lane: a grant issued *after* the revocation still cannot commit, and
// commits only once the pass that dequeues the revoked envelope has run.
#[test]
fn a_bounded_lane_filters_a_revoked_run_s_committed_item_and_frees_its_slot_at_the_dequeue() {
    let worker = CommandId::new("worker");
    let (mut driver, journal) = driver_with(
        Script::new(Command::batch([
            parking_effect([1, 2]),
            parking_effect([10]).cancellable(worker.clone()),
            parking_effect([20]),
        ]))
        .replying([Command::cancel(worker)]),
        config()
            .app_channel_capacity(cap(2))
            .batch_max_messages(cap(1)),
    );
    let report = driver.boot();
    let (trigger, revoked, live) = (
        report.started[0].clone(),
        report.started[1].clone(),
        report.started[2].clone(),
    );

    // Two commits fill the lane exactly: the cancel trigger ahead of the
    // item it will revoke.
    accept(&mut driver, trigger.clone());
    accept(&mut driver, revoked);

    driver
        .step_pass(WakeSource::Data)
        .expect("the cancel trigger is in the lane");
    assert_eq!(journal.reduced(), vec![1], "the cancel applied");

    // One slot freed by that dequeue, and the live origin takes it — so the
    // lane is full again with the revoked item still occupying its slot.
    accept(&mut driver, live);
    assert_eq!(driver.accepted().len(), 3, "three sends have committed");

    // Issued *after* the revocation, onto a lane the revoked envelope still
    // fills. That it cannot commit until something dequeues is the row
    // below; what this one adds is which dequeue does it.
    let queued = driver.grant(trigger).expect("the previous grant resolved");

    driver
        .step_pass(WakeSource::Data)
        .expect("the revoked item is at the head of the lane");
    assert_eq!(
        journal.reduced(),
        vec![1],
        "no update invocation for the revoked origin's item"
    );
    assert_eq!(
        driver.try_confirm(&queued),
        None,
        "the dequeue freed the slot but did not itself commit the waiting send"
    );

    assert_eq!(
        driver.confirm(TEST_TURNS, queued),
        Confirmed::Accepted,
        "the dequeue that filtered the revoked envelope is what freed the capacity"
    );
    assert_eq!(driver.accepted().len(), 4, "so the waiting send committed");

    driver
        .step_pass(WakeSource::Data)
        .expect("the live origin's item is next");
    driver
        .step_pass(WakeSource::Data)
        .expect("and the trigger's second message behind it");
    assert_eq!(
        journal.reduced(),
        vec![1, 20, 2],
        "only live deliveries, on a bounded lane, in the scripted enqueue order"
    );
}

// The bounded handshake with capacity to spare: the sequential
// grant-then-acceptance handshake fixes the enqueue order exactly as it does
// on an unbounded lane, and each script replays to one observation
// sequence.
#[test]
fn a_bounded_lane_with_headroom_keeps_the_handshake_deterministic() {
    let forward = headroom_face(0, 1);
    assert_eq!(forward, headroom_face(0, 1), "one script, one sequence");
    let reversed = headroom_face(1, 0);
    assert_eq!(reversed, headroom_face(1, 0), "and so for the other");

    assert_eq!(
        forward,
        vec![100, 200],
        "script order holds under a bounded lane"
    );
    assert_eq!(
        reversed,
        vec![200, 100],
        "and reversing it reverses delivery"
    );
}

/// One run of the headroom face, on a lane wide enough that no send waits.
fn headroom_face(first: usize, second: usize) -> Vec<u8> {
    let (mut driver, journal) = driver_with(
        Script::new(Command::batch([
            parking_effect([100]),
            parking_effect([200]),
        ])),
        config().app_channel_capacity(cap(8)),
    );
    let report = driver.boot();
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

// The full-lane handshake: with the lane full, a grant acknowledges only
// after the real send commits, which happens only once a dequeue frees
// capacity. The acknowledgement therefore stays truthful — it never reports
// an acceptance the lane did not make — and reaching it needs the step the
// detached token exists to survive (RFC 0014 §13.3's driver-progress form).
#[test]
fn a_full_bounded_lane_acknowledges_only_after_a_dequeue_frees_capacity() {
    let (mut driver, journal) = driver_with(
        Script::new(Command::batch([parking_effect([1]), parking_effect([2])])),
        config()
            .app_channel_capacity(cap(1))
            .batch_max_messages(cap(1)),
    );
    let report = driver.boot();
    let (first, second) = (report.started[0].clone(), report.started[1].clone());

    accept(&mut driver, first);
    let blocked = driver.grant(second).expect("the previous grant resolved");

    driver
        .step_pass(WakeSource::Data)
        .expect("the committed message is in the lane");
    assert_eq!(journal.reduced(), vec![1], "the dequeue freed the slot");
    assert_eq!(
        driver.try_confirm(&blocked),
        None,
        "the dequeue freed the slot but did not itself commit the waiting send"
    );

    assert_eq!(
        driver.confirm(TEST_TURNS, blocked),
        Confirmed::Accepted,
        "and only then did the waiting send commit"
    );
    driver
        .step_pass(WakeSource::Data)
        .expect("the second message is in the lane now");
    assert_eq!(
        journal.reduced(),
        vec![1, 2],
        "delivery follows the scripted enqueue order across the capacity wait"
    );
}

// The other half of the two rows above, and the one that makes them say
// something: **without a dequeue the release never commits, however many
// turns it gets.** Both positive rows show a send committing after a pass
// drained the lane; on their own that is compatible with a send that would
// have committed anyway, because a grant turns nothing and a ledger a
// released send has not reached says the same thing as a ledger it has not
// been offered. What separates the two readings is a budget spent with the
// lane still full, which is what this row spends — and what `confirm` fails
// on rather than waiting out.
#[test]
#[should_panic(expected = "bounded `confirm` exhausted")]
fn a_release_onto_a_full_lane_does_not_commit_without_a_dequeue() {
    let (mut driver, _journal) = driver_with(
        Script::new(Command::batch([parking_effect([1]), parking_effect([2])])),
        config()
            .app_channel_capacity(cap(1))
            .batch_max_messages(cap(1)),
    );
    let report = driver.boot();
    let (first, second) = (report.started[0].clone(), report.started[1].clone());

    accept(&mut driver, first);
    let blocked = driver.grant(second).expect("the previous grant resolved");
    let _confirmed = driver.confirm(TEST_TURNS, blocked);
}

// The same negative for the revocation row: a revoked envelope holds its
// lane slot exactly as a live one does, so a release issued after the
// revocation waits on it. Revoking frees no capacity — the delivery-side
// dequeue does, which is the pass the positive row interposes and this one
// omits.
#[test]
#[should_panic(expected = "bounded `confirm` exhausted")]
fn a_revocation_frees_no_capacity_for_a_waiting_send() {
    let worker = CommandId::new("worker");
    let (mut driver, journal) = driver_with(
        Script::new(Command::batch([
            parking_effect([1, 2]),
            parking_effect([10]).cancellable(worker.clone()),
            parking_effect([20]),
        ]))
        .replying([Command::cancel(worker)]),
        config()
            .app_channel_capacity(cap(2))
            .batch_max_messages(cap(1)),
    );
    let report = driver.boot();
    let (trigger, revoked, live) = (
        report.started[0].clone(),
        report.started[1].clone(),
        report.started[2].clone(),
    );

    accept(&mut driver, trigger.clone());
    accept(&mut driver, revoked);
    driver
        .step_pass(WakeSource::Data)
        .expect("the cancel trigger is in the lane");
    assert_eq!(journal.reduced(), vec![1], "the cancel applied");

    accept(&mut driver, live);
    let queued = driver.grant(trigger).expect("the previous grant resolved");
    let _confirmed = driver.confirm(TEST_TURNS, queued);
}
