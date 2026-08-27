//! RFC 0012's admission suite, re-run on the kernel — INV-RC12's first
//! clause ("RFC 0012's admission suite passes").
//!
//! The suite's owner states its rows at the subscription manager's layer,
//! which the kernel does not have: there is one run registry for every
//! producer kind and one admission site, the re-evaluation inside a pass's
//! frame stage (RFC 0014 §5.1, §5.3). So each row below is the owner's
//! claim over the kernel's seam, driven pass-unit like the rest of the
//! suite and read from the sources' own instrumentation.
//!
//! | Row | Owner | What it holds here |
//! | --- | --- | --- |
//! | INV-SE1 | RFC 0012 §11 | the spawner runs once per admitted run, at the admission, and not at all for a declaration the barrier deferred |
//! | INV-SE2 | RFC 0012 §11 | a continuing identity is neither stopped nor respawned by a re-evaluation, including one whose removed set is still quiescing |
//! | INV-SE3 | RFC 0012 §11 | a re-evaluation with no outstanding stop admits immediately |
//! | INV-SE4 | RFC 0012 §11 | the mandated `{A}` → `{B}` → `{C}` sequence: C is admitted and B's spawner is never invoked |
//!
//! **INV-SE4 has two rows, and only one of them is the mandated sequence.**
//! The owner's sequence turns on a middle state — `{C}` installed while A's
//! stop is still outstanding — and that state is not constructible on a
//! current-thread executor, where a stop resolves within one turn. The
//! collapsed face runs there and witnesses the supersession without the
//! window; the mandated face runs on the multi-worker driver and constructs
//! the window by holding A's *dismantling* at a script-controlled gate, so
//! the un-quiesced interval is the application's own and not a race. That
//! second row therefore sits with the other multi-worker rows in its
//! evidence standing: pass-unit driven like the rest, but citing no part of
//! INV-RC14's determinism claim, whose verified range is the current-thread
//! executor (RFC 0008 §9.8).
//!
//! Three of the suite's rows are carried elsewhere and are not repeated
//! here: INV-SE3's deferral half and INV-SE5's wake requirement are the
//! `stop/restart safe window` and `parked subscription-quiescence wake`
//! series ([`lifecycle`](super::lifecycle), [`park`](super::park)), and
//! INV-SE3's finished-restart case is `park`'s natural-finish row. INV-SE5's
//! structural half is the admission site itself: `Kernel::reconcile` is the
//! only place a subscription run is started, its stop phase is read whole
//! before any stop is issued, and it takes no second admission attempt after
//! issuing one — which is also INV-RC12 (c)'s structural carrier.
//!
//! One row here has no owner in that suite: that a repeated declaration
//! admits one run. It was a property of the superseded manager's map keying
//! and is now a property of `reconcile`'s read order, so it changed owners
//! rather than disappearing, and it is asserted where the admission site is.
//!
//! INV-RC12's own additions — (a) the non-participating run kinds and (b)
//! the non-dirt sources — live with the seams that produce them:
//! [`lifecycle`](super::lifecycle) and [`cleanup`](super::cleanup) carry
//! (a)'s command and cleanup kinds, and [`park`](super::park) and
//! [`cleanup`](super::cleanup) carry (b)'s three quiescences.

use crate::kernel::arbiter::WakeSource;

use super::support::{
    Feed, ProbeSource, QuiescenceGate, Script, TEST_TURNS, THREADED_TURNS, accept, accept_within,
    cap, config, driver, driver_with, gated_threaded_driver_with, parking_effect, step_when_ready,
};

// INV-SE1: one spawner invocation per admitted run, made at the admission
// and nowhere else. The declaration is returned at every re-evaluation — the
// program declares it from state, so `subscriptions` builds a fresh
// `Subscription` each time — and the count moves only where a run actually
// started, which is what separates admission from declaration.
#[test]
fn a_declaration_s_spawner_runs_once_per_admitted_run_and_not_per_declaration() {
    let source = ProbeSource::silent("feed");
    let (mut driver, journal) =
        driver(Script::new(parking_effect([1, 2])).feeding([Feed::new(source.clone())]));
    let trigger = driver.boot().started[0].clone();
    assert_eq!(source.admissions(), 1, "boot admitted the declaration once");

    for _ in 0..2 {
        accept(&mut driver, trigger.clone());
        driver
            .step_pass(WakeSource::Data)
            .expect("the send is in the lane");
    }

    assert_eq!(
        journal.evaluations(),
        3,
        "the bootstrap reconcile and both messages' re-evaluations"
    );
    assert_eq!(
        source.admissions(),
        1,
        "and the still-declared identity was admitted at none of them"
    );
    assert_eq!(source.quiescences(), 0, "nor was its run ever stopped");
}

// The declared set is a set. A `subscriptions` that returns one identity
// twice admits one run, and it is the first: `reconcile` inserts the entry
// before it reads the next declaration, so the duplicate finds the identity
// already running and reaches nothing.
//
// Collision independence rides along structurally rather than as a second
// row. The registry compares `SubscriptionId`s by value on a linear walk and
// never hashes one, so two distinct identities cannot be conflated by their
// hashes agreeing. The superseded manager keyed a map by identity, which is
// what made that a behavioural risk with a row of its own there; here the
// property is a consequence of the lookup and has nothing to fail.
#[test]
fn a_repeated_declaration_admits_one_run_and_it_is_the_first() {
    let source = ProbeSource::silent("feed");
    let (mut driver, journal) = driver(
        Script::new(parking_effect([1]))
            .feeding([Feed::new(source.clone()), Feed::new(source.clone())]),
    );
    driver.boot();

    assert_eq!(
        journal.evaluations(),
        1,
        "the bootstrap reconcile read the declarations once"
    );
    assert_eq!(
        source.admissions(),
        1,
        "and admitted the repeated identity once, not once per declaration"
    );
    assert_eq!(source.quiescences(), 0, "nothing was stopped to make room");
}

// INV-SE2: a continuing identity is exempt. The re-evaluation that removes
// `gone` leaves `kept` alone — not stopped, not awaited, not respawned —
// and it stays that way while the removed run is still quiescing, which is
// the window the barrier holds open.
#[test]
fn a_continuing_identity_is_untouched_by_a_re_evaluation_that_removes_another() {
    let kept = ProbeSource::silent("kept");
    let gone = ProbeSource::silent("gone");
    let (mut driver, _journal) = driver_with(
        Script::new(parking_effect([1]))
            .feeding([Feed::new(kept.clone()), Feed::new(gone.clone())])
            .redeclaring(1, ["kept"]),
        config().batch_max_messages(cap(1)),
    );
    let trigger = driver.boot().started[0].clone();
    assert_eq!(kept.admissions(), 1, "boot admitted both");
    assert_eq!(gone.admissions(), 1);

    accept(&mut driver, trigger);
    driver
        .step_pass(WakeSource::Data)
        .expect("the removal trigger is in the lane");

    assert_eq!(
        kept.quiescences(),
        0,
        "the kept identity's run was not stopped by the re-evaluation that removed the other"
    );
    driver.settle(TEST_TURNS, || gone.quiescences() > 0);
    assert_eq!(
        kept.admissions(),
        1,
        "and it was not respawned while the removed run quiesced"
    );
    assert_eq!(kept.quiescences(), 0, "nor stopped during that window");
}

// INV-SE3's immediate half: a re-evaluation with no outstanding stop admits
// in its own pass. This is the control for the deferral rows — without it
// "nothing was admitted" would be indistinguishable from "nothing is ever
// admitted in a stopping pass".
#[test]
fn a_pure_addition_is_admitted_in_the_pass_that_declares_it() {
    let added = ProbeSource::silent("added");
    let (mut driver, _journal) = driver_with(
        Script::new(parking_effect([1]))
            .feeding([Feed::new(added.clone())])
            .wanting([])
            .redeclaring(1, ["added"]),
        config().batch_max_messages(cap(1)),
    );
    let trigger = driver.boot().started[0].clone();
    assert_eq!(added.admissions(), 0, "nothing was declared at boot");

    accept(&mut driver, trigger);
    let stepped = driver
        .step_pass(WakeSource::Data)
        .expect("the trigger is in the lane");

    assert_eq!(
        added.admissions(),
        1,
        "the re-evaluation issued no stop, so it admitted in its own pass"
    );
    assert_eq!(
        stepped.started.len(),
        1,
        "and the run it started is that pass's own"
    );
}

// INV-SE4, the owner's mandated sequence, which is a required test rather
// than an example: `{A}` → `{B}` with A stop-requested → `{C}` before an
// admission site is reached → A quiesces → the next re-evaluation admits C.
// **B's spawner is never invoked at any point.**
//
// The kernel reaches the sequence's third state by the stage order rather
// than by a scheduler race. The pass that delivers the second trigger
// reflects A's exit in stage 1 — marking dirt — and only then runs the
// batch that installs `{C}`, so the re-evaluation in that same pass's frame
// stage reads the newest desired set. A superseded generation therefore has
// no pending spawner to discard: there is nothing between the deferral and
// the next re-evaluation for one to sit in.
//
// **What this face does not construct**, and the row below does: the
// sequence's middle state, where `{C}` is installed while A's stop is still
// outstanding. Here A quiesces before `{C}` is installed, because on the
// current-thread executor a stop resolves within one turn and the driver's
// own waiting is what advances it — so this is the collapsed form of the
// sequence, and it is worth having as its own row because it is the shape
// production reaches whenever a stopped source dismantles promptly.
#[test]
fn a_supersession_that_lands_after_the_stop_quiesced_never_invokes_the_superseded_spawner() {
    let first = ProbeSource::silent("a");
    let superseded = ProbeSource::silent("b");
    let newest = ProbeSource::silent("c");
    let (mut driver, _journal) = driver_with(
        Script::new(parking_effect([1, 2]))
            .feeding([
                Feed::new(first.clone()),
                Feed::new(superseded.clone()),
                Feed::new(newest.clone()),
            ])
            .wanting(["a"])
            .redeclaring(1, ["b"])
            .redeclaring(2, ["c"]),
        config().batch_max_messages(cap(1)),
    );
    let trigger = driver.boot().started[0].clone();
    assert_eq!(first.admissions(), 1, "boot admitted A");

    accept(&mut driver, trigger.clone());
    accept(&mut driver, trigger);

    // `{A}` → `{B}`: the re-evaluation stops A and, having issued a stop,
    // admits nothing in its own pass.
    driver
        .step_pass(WakeSource::Data)
        .expect("the first trigger is in the lane");
    assert_eq!(
        superseded.admissions(),
        0,
        "the stopping pass admitted nothing, B included"
    );

    // A quiesces, with `{B}` still the declared set.
    driver.settle(TEST_TURNS, || first.quiescences() > 0);
    assert_eq!(
        superseded.admissions(),
        0,
        "and nothing admitted behind the stop while it settled"
    );

    // `{B}` → `{C}`, then the re-evaluation that finally admits.
    driver
        .step_pass(WakeSource::Data)
        .expect("the second trigger is in the lane");

    assert_eq!(
        newest.admissions(),
        1,
        "the re-evaluation ran against the newest desired set and admitted C"
    );
    assert_eq!(
        superseded.admissions(),
        0,
        "and B's spawner was never invoked at any point in the sequence"
    );
    assert_eq!(first.admissions(), 1, "A was never restarted either");
}

// INV-SE4's sequence with its middle state actually constructed: `{C}` is
// installed **while A's stop is still outstanding**, which is the state the
// owner's mandated sequence turns on and the row above cannot reach.
//
// The window is opened by holding A's *dismantling*, not by racing it. A's
// source carries a guard whose drop blocks at a gate the script controls,
// and the abort drops that guard on a worker thread, so the driving thread
// stays steppable throughout while A sits stop-requested and un-quiesced for
// as long as the script wants. The determinism is that gate's, not the
// scheduler's: `entered` says the dismantling began and `quiesced` says it
// finished, and neither side can pass the other. Like the other multi-worker
// rows, this one cites no part of INV-RC14's determinism claim, whose
// verified range is the current-thread executor (RFC 0008 §9.8).
//
// The middle pass is where the barrier does its work: a re-evaluation runs,
// reads `{C}`, and admits **nothing** — not C, because A has not quiesced,
// and not B, because B is no longer declared by the time an admission site
// is reached. That B's spawner is never invoked is the invariant's own
// claim, and here it holds across a window in which B *was* the declared set
// when the stop was issued.
#[test]
fn the_mandated_supersession_window_never_invokes_the_superseded_spawner() {
    let gate = QuiescenceGate::default();
    let first = ProbeSource::gated("a", gate.clone());
    let superseded = ProbeSource::silent("b");
    let newest = ProbeSource::silent("c");
    // Driven through the gated constructor, not `threaded_driver_with`: the
    // driver it returns owns the gate's release and drops it before shutting
    // the executor down, so a failure inside the window below unwinds into a
    // test failure rather than a join on the held worker (`GatedDriver`).
    let (mut driver, journal) = gated_threaded_driver_with(
        Script::new(parking_effect([1, 2]))
            .feeding([
                Feed::new(first.clone()),
                Feed::new(superseded.clone()),
                Feed::new(newest.clone()),
            ])
            .wanting(["a"])
            .redeclaring(1, ["b"])
            .redeclaring(2, ["c"]),
        config().batch_max_messages(cap(1)),
        &gate,
    );
    let trigger = driver.boot().started[0].clone();
    assert_eq!(first.admissions(), 1, "boot admitted A");

    accept_within(&mut driver, trigger.clone(), THREADED_TURNS);
    accept_within(&mut driver, trigger, THREADED_TURNS);

    // `{A}` → `{B}`: the re-evaluation stops A and defers its own
    // admissions.
    driver
        .step_pass(WakeSource::Data)
        .expect("the first trigger is in the lane");
    assert_eq!(
        superseded.admissions(),
        0,
        "the stopping pass admitted nothing, B included"
    );

    // The window opens: A's dismantling has begun on a worker and is held
    // there, so the stop is outstanding and the quiescence has not happened.
    driver.settle(THREADED_TURNS, || gate.entered());
    assert_eq!(
        first.quiescences(),
        0,
        "A is stop-requested and has not quiesced, which is the state the sequence needs"
    );

    // `{B}` → `{C}` inside that window. The barrier defers this
    // re-evaluation's admissions too, so the generation that was declared
    // when the stop was issued passes without ever being admitted.
    let evaluations = journal.evaluations();
    driver
        .step_pass(WakeSource::Data)
        .expect("the second trigger is in the lane");
    assert!(
        journal.evaluations() > evaluations,
        "a re-evaluation did run in that pass, so the two counts below are a deferral rather \
         than an absent admission site"
    );
    assert_eq!(
        superseded.admissions(),
        0,
        "B was the declared set across the stop and was still never admitted"
    );
    assert_eq!(
        newest.admissions(),
        0,
        "and C waits behind the outstanding stop rather than admitting beside it"
    );
    assert_eq!(
        first.quiescences(),
        0,
        "the hold was still holding across that pass, so the two readings above are the \
         barrier's answer and not a released run's"
    );

    // The window closes: A quiesces, and the pass its notification begins
    // re-evaluates against the then-current state.
    gate.open();
    driver.settle(THREADED_TURNS, || first.quiescences() > 0);
    step_when_ready(&mut driver, WakeSource::ProducerExit, THREADED_TURNS);

    assert_eq!(
        newest.admissions(),
        1,
        "the re-evaluation ran against the newest desired set and admitted C"
    );
    assert_eq!(
        superseded.admissions(),
        0,
        "and B's spawner was never invoked at any point in the sequence"
    );
    assert_eq!(first.admissions(), 1, "A was never restarted either");
}
