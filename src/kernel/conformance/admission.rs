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
//! | INV-SE1 | RFC 0012 §8 | the spawner runs once per admitted run, at the admission, and not at all for a declaration the barrier deferred |
//! | INV-SE2 | RFC 0012 §8 | a continuing identity is neither stopped nor respawned by a re-evaluation, including one whose removed set is still quiescing |
//! | INV-SE3 | RFC 0012 §8 | a re-evaluation with no outstanding stop admits immediately |
//! | INV-SE4 | RFC 0012 §8 | the mandated `{A}` → `{B}` → `{C}` sequence: C is admitted and B's spawner is never invoked |
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
//! INV-RC12's own additions — (a) the non-participating run kinds and (b)
//! the non-dirt sources — live with the seams that produce them:
//! [`lifecycle`](super::lifecycle) and [`cleanup`](super::cleanup) carry
//! (a)'s command and cleanup kinds, and [`park`](super::park) and
//! [`cleanup`](super::cleanup) carry (b)'s three quiescences.

use crate::kernel::arbiter::WakeSource;

use super::support::{
    Feed, ProbeSource, Script, TEST_TURNS, accept, cap, config, driver, driver_with, parking_effect,
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
#[test]
fn a_superseded_generation_s_spawner_is_never_invoked() {
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
