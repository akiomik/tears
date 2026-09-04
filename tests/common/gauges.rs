//! The INV-LC7 producer-gauge census, shared via `#[path]` so every
//! integration target that reads the gauges reads one definition of the
//! field set, one meaning of "rose", one meaning of "settled", and one
//! bound on how long settling may take — a producer gauge added here
//! reaches every census at once, on both halves, instead of leaving a copy
//! waiting on the old set.
//!
//! An including target must also declare the recorder at its crate root,
//! spelled exactly `#[path = "common/trace_recorder.rs"] mod
//! trace_recorder;` — this module resolves the type through that name.
//! Every item here is used by every including target, deliberately: an
//! item only one target needs (`no_producer_gauge_event_fired` lives in
//! `lifecycle.rs` for this reason) stays with its caller.

use crate::trace_recorder::{Readings, TraceRecorder};

/// The settling producer gauges RFC 0011 INV-LC7 reads — three of RFC 0006
/// §4.4's four counts. `blocked`, the fourth, rides the same snapshot but
/// belongs to the bounded-send layer rather than to run lifecycle, so the
/// settle census does not hold it to a terminal reading. The names' code
/// source of truth is `GAUGE_EVENT_FIELDS` in `src/runtime/load.rs`, which
/// a unit row holds to the schema its emitter actually writes. Nothing
/// binds these three literals to that array, though: a coordinated rename
/// leaves them stale, and what refuses it is the strict branch below —
/// a renamed gauge reads `None`, which is not a settled gauge.
pub const PRODUCER_GAUGES: [&str; 3] = ["subscriptions", "unkeyed_commands", "keyed_commands"];

/// How many settle steps a census may take before it reports the quiescent
/// postcondition as unmet.
///
/// This counts executor drains, not intervals, so host load cannot consume it.
/// One step is one `yield_now`, and under the `current_thread` scheduler that
/// is a complete drain of the run queue: the caller's waker goes on the
/// deferred list, so `block_on` keeps popping tasks until the queue is empty
/// before it wakes the deferred wakers and polls the caller again. The
/// teardown needs exactly one such drain — aborting a task leaves it queued
/// and cancelled, and running it drops the future — so the bound carries three
/// orders of magnitude of margin over the mechanism it waits on.
///
/// A step must not advance the paused clock, which rules out a
/// `sleep`/`timeout` bound however appealing "settled or never will" sounds as
/// a formulation. Advancing virtual time hands a producer that the teardown
/// failed to cancel a second way to stop — its own timer firing into the
/// channel whose receiver the teardown dropped — and the rows this bounds
/// exist to catch exactly that failure. `yield_now` reschedules through the
/// deferred list, which keeps the scheduler off the parking path and therefore
/// keeps the clock still.
pub const SETTLE_STEPS: usize = 1_000;

/// Asserts every gauge named in `expected_active` rose while the row's
/// producers ran.
///
/// The census's positive half, and what makes its settle half witness
/// anything: every gauge event carries all the counts at once, so a gauge that
/// never rose still reads `Some(0)` on some other producer's event and settles
/// on iteration zero. Naming the risen gauges is what separates "wound down"
/// from "never started". Taking the set as an argument is what keeps this half
/// following [`PRODUCER_GAUGES`] — a gauge added there is rise-checked
/// wherever the caller passes the whole census, instead of leaving a
/// hand-unrolled copy asserting the old set.
///
/// "Rose" is any reading above zero rather than a reading of exactly one. The
/// two agree for a counter that moves by one per producer and emits on every
/// move, so they can only differ where an emission was lost — and there the
/// exact-value form fails for the wrong reason, reporting a gauge that plainly
/// ran as one that never started. How many producers ran is a different claim,
/// left to the rows that make it.
///
/// Guarded on census membership like [`producer_gauges_are_zero`], and for
/// a reason particular to this direction: `u64_values` reads a field name
/// off the whole `tears::runtime::load` target, and every gauge event also
/// carries `seq` and `runtime_id` — both above zero by construction. A name
/// that misses the census does not read empty and fail; it can read one of
/// those and *pass*, asserting that the ordering counter rose. The guard is
/// what keeps this half unable to satisfy itself on a non-gauge.
///
/// An empty `expected_active` is refused, which is the opposite of what
/// [`producer_gauges_are_zero`] does with one — read the two together, since
/// a caller usually hands the same slice to both. Empty is a meaningful
/// reading there (no producers to settle) and no reading at all here, and
/// letting it through would take both halves of the census vacuous at once:
/// a rise check that asserts nothing beside a settle loop that returns on
/// its first iteration.
///
/// Written as a plain loop rather than an iterator chain for the same
/// `#[track_caller]` reason as [`producer_gauges_are_zero`].
///
/// # Panics
///
/// If `expected_active` is empty, if any name in it is outside
/// [`PRODUCER_GAUGES`], or if any named gauge has no reading above zero.
#[track_caller]
pub fn producer_gauges_rose(recorder: &TraceRecorder, expected_active: &[&str]) {
    assert!(
        !expected_active.is_empty(),
        "an empty rise check asserts nothing, and its caller hands the same slice to \
         `producer_gauges_are_zero`, where empty is the tolerant reading — so both halves \
         of the census would go vacuous at once"
    );
    assert!(
        expected_active
            .iter()
            .all(|field| PRODUCER_GAUGES.contains(field)),
        "a rise assertion on a name outside the census can pass on an unrelated load field: \
         {expected_active:?}"
    );
    for field in expected_active {
        let values = recorder.u64_values(field);
        assert!(
            values.contains_nonzero(),
            "the {field} gauge must have risen while this row's producers ran, \
             or its fall to zero witnesses nothing: {values:?}"
        );
    }
}

/// Whether every producer gauge has reached its terminal reading.
///
/// A gauge whose producer the row actually starts (`expected_active`) must
/// currently read zero: `None` is not a settled gauge but a silenced
/// instrument — no gauge event fired at all, or none carried the `seq` the
/// current-value read requires — and accepting it there would let a
/// mutation that stops or de-schemas the emission satisfy INV-LC7 on
/// iteration zero. Gauges outside the set take the weaker reading: `None`
/// stays acceptable, and — since every gauge event carries all the counts
/// at once — a fired event still holds them to `Some(0)`; the set names
/// which gauges must have *risen*, not which are exempt from settling.
///
/// Written as a plain loop rather than an iterator chain so the
/// `#[track_caller]` chain holds: a closure would break it, and
/// `current_u64`'s cross-instance panic would then name this file instead
/// of the caller. The chain propagates one frame — a row calling directly
/// is named; a shared helper in between becomes the named frame itself.
///
/// An empty `expected_active` makes every branch tolerant: that is a
/// no-producers reading, not an INV-LC7 check — rows that start no
/// producer assert on the raw event log instead.
///
/// # Panics
///
/// Inherits `current_u64`'s single-runtime guard: a recorder that observed
/// gauge events from more than one runtime instance panics here rather
/// than settling on a cross-instance reading.
#[track_caller]
pub fn producer_gauges_are_zero(recorder: &TraceRecorder, expected_active: &[&str]) -> bool {
    assert!(
        expected_active
            .iter()
            .all(|field| PRODUCER_GAUGES.contains(field)),
        "an expected-active name outside the census silently weakens every gauge to the \
         tolerant branch: {expected_active:?}"
    );
    for field in PRODUCER_GAUGES {
        let current = recorder.current_u64(field);
        let settled = if expected_active.contains(&field) {
            current == Some(0)
        } else {
            matches!(current, None | Some(0))
        };
        if !settled {
            return false;
        }
    }
    true
}

/// Each gauge beside its current value and its arrival-order log — the
/// census's one diagnostic shape, interpolated by both settle asserts so a
/// change to it reaches every failure message at once. No `#[track_caller]`:
/// the `.map` closure would break the chain anyway, and every count rides
/// one snapshot, so a clash here would already have fired on the census's
/// first read.
#[must_use]
pub fn producer_gauge_report(
    recorder: &TraceRecorder,
) -> [(&'static str, Option<u64>, Readings); PRODUCER_GAUGES.len()] {
    PRODUCER_GAUGES.map(|field| {
        (
            field,
            recorder.current_u64(field),
            recorder.u64_values(field),
        )
    })
}

// Compiled per including target, so these rows run once per binary — the
// deliberate cost of each target pinning the census it links against.
#[cfg(test)]
mod tests {
    use super::*;

    /// The target `GaugeSnapshot::dispatch` emits on, so a row here can
    /// stand up an event of the shape the census reads.
    const LOAD_TARGET: &str = "tears::runtime::load";

    // The two guards on the rise half. Its sibling's strict/tolerant split
    // has `an_active_gauge_with_no_events_is_not_settled` below; these two
    // had nothing, and a rise check that quietly asserts nothing leaves no
    // trace to notice. Both are `#[should_panic]` rather than checks on an
    // extracted predicate, because the guards *are* asserts — a predicate
    // tested beside them would be pinned without its use.
    //
    // Safe to panic here, unlike the `#[should_panic]` rows #306 is about:
    // those share the lib binary with `panic`'s recording-hook row, which
    // counts hook invocations. The integration targets this module compiles
    // into have no hook-counting row — `common/panic_hook.rs` silences
    // reporting and asserts nothing about it.
    #[test]
    #[should_panic(expected = "outside the census")]
    fn a_non_census_name_cannot_satisfy_the_rise_half() {
        // The recorder is installed and a real gauge-shaped event emitted,
        // so `seq` genuinely reads above zero. That is what makes this row
        // witness the hazard rather than the guard's message: without the
        // guard the rise assert *passes* here, on the ordering counter
        // having risen. `u64_values` matches a bare field name across the
        // whole target, so nothing but the guard separates a census gauge
        // from the two markers every event carries.
        let recorder = TraceRecorder::new().with_target(LOAD_TARGET);
        let _guard = recorder.set_default();
        tracing::debug!(
            target: LOAD_TARGET,
            runtime_id = 1u64,
            seq = 1u64,
            subscriptions = 0u64,
            unkeyed_commands = 0u64,
            keyed_commands = 0u64,
            blocked = 0u64,
            "producer gauges",
        );
        let seqs = recorder.u64_values("seq");
        assert_eq!(
            seqs.only(),
            Some(1),
            "the premise: the marker reads above zero, so an unguarded rise \
             check on it would pass: {seqs:?}"
        );

        producer_gauges_rose(&recorder, &["seq"]);
    }

    #[test]
    #[should_panic(expected = "asserts nothing")]
    fn an_empty_rise_check_is_refused() {
        producer_gauges_rose(&TraceRecorder::new(), &[]);
    }

    // The strict/tolerant split is the settle half's whole contract: an
    // active gauge with no events is a silenced instrument, not a settled
    // one, while a gauge the row never raises may legitimately have none.
    #[test]
    fn an_active_gauge_with_no_events_is_not_settled() {
        let recorder = TraceRecorder::new();
        assert!(
            !producer_gauges_are_zero(&recorder, &["subscriptions"]),
            "an expected-active gauge must have fired to count as settled"
        );
        assert!(
            producer_gauges_are_zero(&recorder, &[]),
            "a gauge the row never raises settles on no events at all"
        );
    }
}
