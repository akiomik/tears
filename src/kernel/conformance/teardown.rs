//! Scope teardown, the kernel rows of RFC 0013 §7.1.
//!
//! Every row here runs through the production dispatch path, pass-unit
//! driven: a teardown reaches the kernel only as the lowered part of a
//! command the application returned, exactly as in production — no driver
//! method applies one (RFC 0008 §9.2).
//!
//! The invariants these carry: INV-ST1's prefix selection over every run
//! kind, with the kernel carriers INV-RC7 (anonymous reachability) and
//! INV-RC6 (retraction of subscription output); INV-ST3's cancel-phase
//! application; INV-ST5's totality and idempotence; INV-ST6's selection
//! isolation; and the observable half of INV-ST7's reuse rule. INV-ST7's
//! absence half is structural by its own statement — no finite set of
//! fresh-start scripts proves it — and is not attempted here.

use crate::command::{Command, CommandId};
use crate::kernel::arbiter::WakeSource;
use crate::testing::driver::{RunKind, RunName};

use super::support::{
    Beacon, Feed, ProbeSource, Script, TEST_TURNS, accept, cap, config, driver_with,
    holding_effect, parking_effect,
};

// INV-ST1, over all three run kinds at once, with INV-ST6's isolation as its
// negative half. The prefix selects the keyed command run, the *anonymous*
// command run — INV-RC7's reachability, the kind the old contract could not
// address at all — and the subscription run declared under the same
// boundary. Nothing outside is touched, including a keyed run holding the
// **same local id** under the root: local keys never participate in
// selection.
#[test]
fn a_prefix_teardown_selects_every_run_kind_under_it_and_nothing_outside() {
    let local = CommandId::new("worker");
    let (child_keyed, child_anon) = (Beacon::default(), Beacon::default());
    let (root_keyed, root_anon) = (Beacon::default(), Beacon::default());
    let child_source = ProbeSource::silent("child");
    let root_source = ProbeSource::silent("root");
    let (mut driver, journal) = driver_with(
        Script::new(Command::batch([
            Command::batch([
                holding_effect(child_keyed.clone()).cancellable(local.clone()),
                holding_effect(child_anon.clone()),
            ])
            .scoped("pane"),
            holding_effect(root_keyed.clone()).cancellable(local).into(),
            holding_effect(root_anon.clone()).into(),
            parking_effect([1]).into(),
        ]))
        .replying([Command::teardown("pane")])
        .feeding([
            Feed::new(child_source.clone()).under("pane"),
            Feed::new(root_source.clone()),
        ]),
        config().batch_max_messages(cap(1)),
    );
    let trigger = driver.boot().started[4].clone();
    assert_eq!(child_source.admissions(), 1, "the child feed is running");
    assert_eq!(root_source.admissions(), 1, "and so is the root feed");

    accept(&mut driver, trigger);
    driver
        .step_pass(WakeSource::Data)
        .expect("the teardown trigger is in the lane");
    assert_eq!(journal.reduced(), vec![1], "the teardown was dispatched");

    driver.settle(TEST_TURNS, || {
        child_keyed.marked() && child_anon.marked() && child_source.quiescences() > 0
    });

    assert!(!root_keyed.marked(), "the root keyed run is untouched");
    assert!(!root_anon.marked(), "and so is the root anonymous run");
    assert_eq!(
        root_source.quiescences(),
        0,
        "and the subscription declared outside the prefix keeps running"
    );
}

// INV-ST4 through its kernel carrier INV-RC6: after the teardown's
// application point, zero deliveries from a selected subscription run — the
// output it had already committed included. The check is the invocation
// itself, recorded by the reducer, not a count of what the lane admitted.
#[test]
fn a_teardown_retracts_a_selected_subscription_s_committed_output() {
    let source = ProbeSource::sending("child", [50]);
    let (mut driver, journal) = driver_with(
        Script::new(parking_effect([1]))
            .replying([Command::teardown("pane")])
            .feeding([Feed::new(source).under("pane")]),
        config().batch_max_messages(cap(1)),
    );
    let report = driver.boot();
    let (trigger, forwarder) = (report.started[0].clone(), report.started[1].clone());

    // The handshake puts the teardown trigger ahead of the subscription's
    // own output, so the output is buffered when the teardown applies.
    accept(&mut driver, trigger);
    accept(&mut driver, forwarder);

    driver
        .step_pass(WakeSource::Data)
        .expect("the teardown trigger is in the lane");
    assert_eq!(journal.reduced(), vec![1], "the teardown was dispatched");

    driver
        .step_pass(WakeSource::Data)
        .expect("the retracted output is still in the lane");
    assert_eq!(
        journal.reduced(),
        vec![1],
        "no update invocation for the torn-down subscription's buffered output"
    );
}

// INV-ST3 and the observable half of INV-ST7 in one command: a teardown and
// a same-prefix spawn returned together apply in that order — the teardown
// in the cancel phase, before every spawn of the same command — so the
// successor starts fresh under a prefix whose previous occupant is gone.
#[test]
fn a_teardown_and_a_same_prefix_spawn_start_the_successor_fresh() {
    let local = CommandId::new("worker");
    let (predecessor, successor) = (Beacon::default(), Beacon::default());
    let (mut driver, _journal) = driver_with(
        Script::new(Command::batch([
            holding_effect(predecessor.clone())
                .cancellable(local.clone())
                .scoped("pane"),
            parking_effect([1]),
        ]))
        .replying([Command::batch([
            Command::teardown("pane"),
            holding_effect(successor.clone()).cancellable(local).into(),
        ])
        .scoped("pane")]),
        config().batch_max_messages(cap(1)),
    );
    let trigger = driver.boot().started[1].clone();

    accept(&mut driver, trigger);
    let stepped = driver
        .step_pass(WakeSource::Data)
        .expect("the trigger is in the lane");

    let started: Vec<RunKind> = stepped.started.iter().map(RunName::kind).collect();
    assert!(
        matches!(started.as_slice(), [RunKind::Keyed(_)]),
        "the same command's spawn phase started the successor: {started:?}"
    );
    driver.settle(TEST_TURNS, || predecessor.marked());
    assert!(
        !successor.marked(),
        "and the successor is the run that survives the application point"
    );
}

// INV-ST5: every constructible prefix is accepted, zero matches is a no-op,
// and reapplication with no intervening spawn is observationally a single
// application. A teardown of a prefix nothing is under changes nothing an
// application can see, and the second application of a matching prefix
// neither errors nor re-fires anything.
#[test]
fn a_teardown_is_total_and_idempotent() {
    let occupant = Beacon::default();
    let (mut driver, journal) = driver_with(
        Script::new(Command::batch([
            holding_effect(occupant.clone()).scoped("pane"),
            parking_effect([1, 2, 3]),
        ]))
        .replying([
            Command::teardown("nothing-is-here"),
            Command::teardown("pane"),
            Command::teardown("pane"),
        ]),
        config().batch_max_messages(cap(1)),
    );
    let trigger = driver.boot().started[1].clone();

    accept(&mut driver, trigger.clone());
    driver
        .step_pass(WakeSource::Data)
        .expect("the first trigger is in the lane");
    assert!(
        !occupant.marked(),
        "a prefix with zero matches selected nothing"
    );

    accept(&mut driver, trigger.clone());
    driver
        .step_pass(WakeSource::Data)
        .expect("the second trigger is in the lane");
    driver.settle(TEST_TURNS, || occupant.marked());
    assert_eq!(occupant.marks(), 1, "the occupant was reclaimed once");

    accept(&mut driver, trigger);
    driver
        .step_pass(WakeSource::Data)
        .expect("the third trigger is in the lane");
    driver.settle(TEST_TURNS, || true);
    assert_eq!(
        occupant.marks(),
        1,
        "reapplying the same prefix is observationally a single application"
    );
    assert_eq!(
        journal.reduced(),
        vec![1, 2, 3],
        "and every application left the kernel delivering"
    );
}

// The divergent composition the `Command` surface deliberately admits:
// `work.scoped(s).cancellable(id)` places a run under `s` while giving it a
// root-global key, which `Command::scoped`'s own docs bless as a scoped
// effect participating in an application-wide slot. Its two identity paths
// disagree — the carrier's scope is `s`, the key's is the root — and the
// kernel reaches the run from **both** directions, neither derived from the
// other: the prefix selects it because that is where the run is placed
// (RFC 0014 §4.1), and the explicit cancel reaches it because that is its
// cancel identity (RFC 0005 §4.3).
//
// One shape, two runs, because the two reaches are separate claims: the
// first run is taken by the teardown and the second by the cancel.
#[test]
fn a_scoped_run_under_a_root_global_key_is_reachable_by_prefix_and_by_id() {
    let global = CommandId::new("slot");
    let (by_prefix, by_id) = (Beacon::default(), Beacon::default());
    let (mut driver, journal) = driver_with(
        Script::new(Command::batch([
            holding_effect(by_prefix.clone())
                .scoped("pane")
                .cancellable(global.clone()),
            parking_effect([1, 2]),
        ]))
        .replying([
            // The teardown's prefix names where the run sits, not its key.
            Command::batch([
                Command::teardown("pane"),
                holding_effect(by_id.clone())
                    .scoped("pane")
                    .cancellable(global.clone())
                    .into(),
            ]),
            // The cancel names the key, which the boundary never qualified.
            Command::cancel(global),
        ]),
        config().batch_max_messages(cap(1)),
    );
    let trigger = driver.boot().started[1].clone();

    accept(&mut driver, trigger.clone());
    driver
        .step_pass(WakeSource::Data)
        .expect("the teardown trigger is in the lane");
    driver.settle(TEST_TURNS, || by_prefix.marked());
    assert!(
        !by_id.marked(),
        "the successor this same command spawned starts fresh under the torn-down prefix"
    );

    accept(&mut driver, trigger);
    driver
        .step_pass(WakeSource::Data)
        .expect("the cancel trigger is in the lane");
    driver.settle(TEST_TURNS, || by_id.marked());

    assert_eq!(
        journal.reduced(),
        vec![1, 2],
        "both reaches applied through the ordinary dispatch path"
    );
}
