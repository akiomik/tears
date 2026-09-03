//! The INV-LC7 producer-gauge census, shared via `#[path]` so every
//! integration target that reads the gauges reads one definition of the
//! field set and one meaning of "settled" — a producer gauge added here
//! reaches every census at once instead of leaving a copy waiting on the
//! old set.
//!
//! An including target must also declare the recorder at its crate root,
//! spelled exactly `#[path = "common/trace_recorder.rs"] mod
//! trace_recorder;` — this module resolves the type through that name.
//! Every item here is used by every including target, deliberately: an
//! item only one target needs (`no_producer_gauge_event_fired` lives in
//! `lifecycle.rs` for this reason) stays with its caller.

use crate::trace_recorder::TraceRecorder;

/// The settling producer gauges RFC 0011 INV-LC7 reads — three of RFC 0006
/// §4.4's four counts. `blocked`, the fourth, rides the same snapshot but
/// belongs to the bounded-send layer rather than to run lifecycle, so the
/// settle census does not hold it to a terminal reading. The names' code
/// source of truth is `GaugeSnapshot::dispatch` in `src/runtime/load.rs`,
/// the schema's single emitter.
pub const PRODUCER_GAUGES: [&str; 3] = ["subscriptions", "unkeyed_commands", "keyed_commands"];

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
) -> [(&'static str, Option<u64>, Vec<u64>); PRODUCER_GAUGES.len()] {
    PRODUCER_GAUGES.map(|field| {
        (
            field,
            recorder.current_u64(field),
            recorder.u64_values(field),
        )
    })
}

// Compiled per including target, so this row runs once per binary — the
// deliberate cost of each target pinning the census it links against.
#[cfg(test)]
mod tests {
    use super::*;

    // The strict/tolerant split is the census's whole contract: an active
    // gauge with no events is a silenced instrument, not a settled one,
    // while a gauge the row never raises may legitimately have none.
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
