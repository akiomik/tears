//! The observability seam: the batch event's kernel reading and INV-RC15's
//! behavioural neighbour (RFC 0014 §9 row 9, RFC 0006 §4.4's INV-L13
//! schema).
//!
//! **What is read here is a production surface, not a driver one.** These
//! rows install a `tracing` recorder on `tears::runtime::load` and read the
//! events the kernel and its lane emit — the same way RFC 0011 INV-LC7's
//! gauge half is read, and deliberately not through a driver accessor: the
//! ledgers are the driver's evidence surface and are no part of the schema
//! (RFC 0012 INV-SE8's boundary). Everything else stays as it is elsewhere
//! in the suite: pass-unit driving, bounded waits, application-side
//! instrumentation for anything the schema does not carry.
//!
//! **The field names are the contract.** Row 9 re-reads meanings and leaves
//! every name where it is, so these rows assert on `pulled`, `updated`,
//! `shared_pending`, and `channel` by name, and one of them asserts the
//! batch event's field set entire — a renamed field is the silent break
//! that row exists to prevent.

use crate::command::{Command, CommandId};
use crate::kernel::arbiter::WakeSource;
use crate::test_support::TraceRecorder;
use crate::testing::driver::{Confirmed, RunKind};

use super::support::{
    Beacon, Feed, ProbeSource, Script, TEST_TURNS, accept, cap, config, driver, driver_with,
    marking_effect, parking_effect,
};

/// The one target the whole schema lives on (RFC 0006 §4.4).
const TARGET: &str = "tears::runtime::load";

// --- the batch event ------------------------------------------------------

// The three fields on one batch, each with a different value, so no reading
// can be right by coincidence: a cap of two takes two of the three committed
// sends, both reach `update`, and the third is what the batch left in the
// data lane — `shared_pending`'s successor quantity (RFC 0014 §9 row 9).
#[test]
fn a_batch_reports_its_dequeues_its_updates_and_the_residue_it_left() {
    let recorder = TraceRecorder::new().with_target(TARGET);
    let _subscriber = recorder.set_default();
    let (mut driver, journal) = driver_with(
        Script::new(parking_effect([1, 2, 3])),
        config().batch_max_messages(cap(2)),
    );
    let sender = driver.boot().started[0].clone();
    for _ in 0..3 {
        accept(&mut driver, sender.clone());
    }

    driver
        .step_pass(WakeSource::Data)
        .expect("three sends are in the lane");

    assert_eq!(
        journal.reduced(),
        vec![1, 2],
        "the cap took two of the three"
    );
    assert_eq!(
        recorder.u64_values("pulled"),
        vec![2],
        "one batch event, for the one batch that pulled anything"
    );
    assert_eq!(
        recorder.u64_values("updated"),
        vec![2],
        "both dequeues reached `update`"
    );
    assert_eq!(
        recorder.u64_values("shared_pending"),
        vec![1],
        "and the third input is what it left in the data lane"
    );
}

// The two counts come apart exactly where RFC 0006 INV-L12 says they do: a
// dequeue is the counted unit whether or not it reached `update`, so a
// revoked origin's envelope — filtered at the delivery decision — is
// `pulled` without being `updated`. This is the kernel's successor to the
// old loop's `Closed` input, the differing-value case that schema's DoD
// required.
#[test]
fn a_revoked_origin_s_envelope_is_pulled_without_being_updated() {
    let recorder = TraceRecorder::new().with_target(TARGET);
    let _subscriber = recorder.set_default();
    let worker = CommandId::new("worker");
    let (mut driver, journal) = driver_with(
        Script::new(Command::batch([
            parking_effect([1]),
            parking_effect([10]).cancellable(worker.clone()),
        ]))
        .replying([Command::cancel(worker)]),
        config().batch_max_messages(cap(2)),
    );
    let report = driver.boot();
    let (trigger, revoked) = (report.started[0].clone(), report.started[1].clone());

    accept(&mut driver, trigger);
    accept(&mut driver, revoked);
    driver
        .step_pass(WakeSource::Data)
        .expect("both sends are in the lane");

    assert_eq!(
        journal.reduced(),
        vec![1],
        "the cancel applied, and the revoked origin's item never reached `update`"
    );
    assert_eq!(
        recorder.u64_values("pulled"),
        vec![2],
        "the discarded envelope is still a dequeue"
    );
    assert_eq!(recorder.u64_values("updated"), vec![1], "but not an update");
    assert_eq!(recorder.u64_values("shared_pending"), vec![0]);
}

// The firing condition, unchanged by row 9 and the one thing the kernel's
// shape could have widened. The old loop's batch *began* with an input, so
// "a batch that pulled nothing" had no existence to report; the kernel runs
// stage 3 on every pass, including passes an exit began, and those report
// nothing.
#[test]
fn a_pass_that_pulls_nothing_fires_no_batch_event() {
    let recorder = TraceRecorder::new().with_target(TARGET);
    let _subscriber = recorder.set_default();
    let finished = Beacon::default();
    let (mut driver, _journal) = driver(Script::new(marking_effect(finished.clone())));

    driver.boot();
    assert!(
        recorder.u64_values("pulled").is_empty(),
        "the bootstrap's continuation pass pulled nothing, so it reported nothing"
    );

    driver.settle(TEST_TURNS, || finished.marked());
    driver
        .step_pass(WakeSource::ProducerExit)
        .expect("the finished run's exit is observable");

    assert!(
        recorder.u64_values("pulled").is_empty(),
        "and neither did the pass its exit began"
    );
}

// The other half of the firing condition: a quit-terminated batch reports
// nothing, exactly as the old loop's early exit did. The dequeue happened
// and `update` ran — this is not a claim that nothing was processed, it is
// the schema's own rule that a batch cut short by a quit has no batch event.
#[test]
fn a_quit_terminated_batch_fires_no_batch_event() {
    let recorder = TraceRecorder::new().with_target(TARGET);
    let _subscriber = recorder.set_default();
    let (mut driver, journal) =
        driver(Script::new(parking_effect([5])).replying([Command::quit()]));
    let sender = driver.boot().started[0].clone();

    accept(&mut driver, sender);
    let stepped = driver
        .step_pass(WakeSource::Data)
        .expect("the send is in the lane");

    assert!(
        stepped.terminated.is_some(),
        "the update-returned quit applied"
    );
    assert_eq!(journal.reduced(), vec![5], "the input was processed");
    assert!(
        recorder.u64_values("pulled").is_empty(),
        "yet the batch it terminated reported nothing"
    );
}

// The schema itself, asserted as a set rather than field by field. Row 9's
// whole decision is that these names do not move — a renamed telemetry field
// breaks dashboards and log parsers off the compiler's path entirely — so
// this row fails on an addition, a removal, or a rename alike, which no
// per-field value assertion does.
#[test]
fn the_batch_event_carries_the_field_names_it_always_has() {
    let recorder = TraceRecorder::new().with_target(TARGET);
    let _subscriber = recorder.set_default();
    let (mut driver, _journal) = driver(Script::new(parking_effect([1])));
    let sender = driver.boot().started[0].clone();

    accept(&mut driver, sender);
    driver
        .step_pass(WakeSource::Data)
        .expect("the send is in the lane");

    let batch_events: Vec<Vec<String>> = recorder
        .field_name_sets()
        .into_iter()
        .filter(|names| names.iter().any(|name| name == "pulled"))
        .collect();
    assert_eq!(
        batch_events,
        vec![vec![
            "message".to_owned(),
            "pulled".to_owned(),
            "shared_pending".to_owned(),
            "updated".to_owned(),
        ]],
        "the batch event's fields are RFC 0006 §4.4's, re-read and not renamed"
    );
}

// --- the producer gauges --------------------------------------------------

// Row 9's third clause, the kind-count mapping: `unkeyed_commands` counts
// anonymous runs, `keyed_commands` keyed runs, and `subscriptions`
// subscription runs. Read together on one kernel holding one of each, so a
// mapping that crossed two of the three fields fails here rather than
// passing three single-kind rows.
#[test]
fn the_gauge_kind_counts_map_onto_the_kernel_s_run_kinds() {
    let recorder = TraceRecorder::new().with_target(TARGET);
    let _subscriber = recorder.set_default();
    let (mut driver, _journal) = driver(
        Script::new(Command::batch([
            parking_effect([1]),
            parking_effect([2]).cancellable(CommandId::new("worker")),
        ]))
        .feeding([Feed::new(ProbeSource::silent("feed"))]),
    );

    driver.boot();

    assert_eq!(
        recorder.u64_values("unkeyed_commands").last(),
        Some(&1),
        "the anonymous run"
    );
    assert_eq!(
        recorder.u64_values("keyed_commands").last(),
        Some(&1),
        "the keyed run"
    );
    assert_eq!(
        recorder.u64_values("subscriptions").last(),
        Some(&1),
        "and the subscription run"
    );
}

// The complement, which is contract rather than an omission: a cleanup run
// is runtime-owned but is none of the three producer kinds, so starting one
// moves no gauge field (RFC 0006 §5.2's successor note, RFC 0014 §9 row 9).
// What accounts for it instead is the settle drain, which is total over the
// join set.
#[test]
fn a_cleanup_run_counts_in_no_gauge_field() {
    let recorder = TraceRecorder::new().with_target(TARGET);
    let _subscriber = recorder.set_default();
    let started = Beacon::default();
    let (mut driver, _journal) = driver_with(
        Script::new(Command::batch([
            parking_effect([1]),
            Command::on_teardown({
                let started = started.clone();
                async move {
                    started.mark();
                }
            })
            .scoped("pane"),
        ]))
        .replying([Command::teardown("pane")]),
        config().batch_max_messages(cap(1)),
    );
    let trigger = driver.boot().started[0].clone();
    let kinds = |field: &str| recorder.u64_values(field).last().copied();
    let (anonymous, keyed, subscriptions) = (
        kinds("unkeyed_commands"),
        kinds("keyed_commands"),
        kinds("subscriptions"),
    );

    accept(&mut driver, trigger);
    driver
        .step_pass(WakeSource::Data)
        .expect("the teardown trigger is in the lane");
    driver.settle(TEST_TURNS, || started.marked());

    assert_eq!(
        (
            kinds("unkeyed_commands"),
            kinds("keyed_commands"),
            kinds("subscriptions")
        ),
        (anonymous, keyed, subscriptions),
        "the teardown started a cleanup run and no gauge field moved for it"
    );
}

// --- INV-RC15's behavioural neighbour -------------------------------------

// One data lane reaches every producer kind, so a blocked send has exactly
// one channel to name. The three rows below are the same script with the
// blocked run's kind varied, and together they are the behavioural half of
// INV-RC15: the structural half — one sender construction site, no second
// message channel constructible — is where the topology itself is held.

// A subscription run's blocked send.
#[test]
fn a_blocked_subscription_send_reports_the_data_lane() {
    let source = ProbeSource::sending("feed", [9]);
    let channels = blocked_send_channel(
        Script::new(parking_effect([1])).feeding([Feed::new(source)]),
        0,
        1,
        |kind| matches!(kind, RunKind::Subscription(_)),
    );
    assert_eq!(channels, vec!["data".to_owned()]);
}

// An anonymous command run's blocked send.
#[test]
fn a_blocked_anonymous_command_send_reports_the_data_lane() {
    let channels = blocked_send_channel(
        Script::new(Command::batch([parking_effect([1]), parking_effect([2])])),
        0,
        1,
        |kind| matches!(kind, RunKind::Anonymous),
    );
    assert_eq!(channels, vec!["data".to_owned()]);
}

// A keyed command run's blocked send. The key sizes nothing and isolates
// nothing: it names a slot in the run bookkeeping, and the lane its output
// waits on is the same one (RFC 0014 §9 row 2's property loss, read from the
// observability side).
#[test]
fn a_blocked_keyed_command_send_reports_the_data_lane() {
    let channels = blocked_send_channel(
        Script::new(Command::batch([
            parking_effect([1]),
            parking_effect([2]).cancellable(CommandId::new("worker")),
        ])),
        0,
        1,
        |kind| matches!(kind, RunKind::Keyed(_)),
    );
    assert_eq!(channels, vec!["data".to_owned()]);
}

/// Drives one blocked send to acceptance and returns the `channel` values
/// the capacity-wait events carried.
///
/// The script is the same in every face: a one-slot lane, a filler run whose
/// committed send fills it, and the run under test released onto the full
/// lane. The step that follows drains the lane — its pre-pass turn is where
/// the released send finds the lane full and waits, which the `blocked`
/// gauge assertion pins so the row cannot pass on a send that never
/// blocked — and the confirm is where it is finally accepted, which is where
/// the capacity-wait event fires (RFC 0006 §4.4).
fn blocked_send_channel(
    script: Script,
    filler: usize,
    blocked: usize,
    kind: impl Fn(&RunKind) -> bool,
) -> Vec<String> {
    let recorder = TraceRecorder::new().with_target(TARGET);
    let _subscriber = recorder.set_default();
    let (mut driver, _journal) = driver_with(
        script,
        config()
            .app_channel_capacity(cap(1))
            .batch_max_messages(cap(1)),
    );
    let report = driver.boot();
    let (filler, blocked) = (
        report.started[filler].clone(),
        report.started[blocked].clone(),
    );
    assert!(
        kind(&blocked.kind()),
        "the run under test is of the kind this face names, got {:?}",
        blocked.kind()
    );

    accept(&mut driver, filler);
    let waiting = driver
        .grant(blocked)
        .expect("no other grant is outstanding");
    driver
        .step_pass(WakeSource::Data)
        .expect("the filler's send is in the lane");
    assert!(
        recorder.u64_values("blocked").contains(&1),
        "the released send found the lane full and waited, which is what this row measures"
    );

    assert_eq!(
        driver.confirm(TEST_TURNS, waiting),
        Confirmed::Accepted,
        "the dequeue freed the slot and the waiting send was accepted"
    );
    recorder.str_values("channel")
}
