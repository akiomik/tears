//! Shared anchor-phase deadline arithmetic for non-catch-up cadences.
//!
//! `Timer` (RFC 0009 §4.2) and the runtime's frame scheduler both deliver at
//! most one tick however many interval boundaries have elapsed, and resume on
//! the anchor's phase rather than resetting to "now + interval". The deadline
//! step they share lives here.

use std::time::Duration;

use tokio::time::Instant;

/// The first anchor-phase boundary strictly after `now`: the smallest
/// `anchor + k * interval` with `k >= 1` that lies after `now` (RFC 0009
/// §4.2 post-miss cadence).
pub fn next_anchor_phase_deadline(anchor: Instant, interval: Duration, now: Instant) -> Instant {
    let elapsed_intervals = now.duration_since(anchor).as_nanos() / interval.as_nanos();
    let offset_nanos = interval.as_nanos().saturating_mul(elapsed_intervals + 1);
    // Assemble the offset as seconds + sub-second nanos rather than through
    // `Duration::from_nanos`, whose u64 argument caps the offset at ~584
    // years of nanoseconds — a saturated deadline would sit permanently in
    // the past and turn every poll into an immediate tick, the forbidden
    // burst. `Duration`'s seconds field is u64, so this stays exact (and the
    // anchor phase preserved) far beyond any reachable horizon.
    let offset = Duration::new(
        u64::try_from(offset_nanos / 1_000_000_000).unwrap_or(u64::MAX),
        u32::try_from(offset_nanos % 1_000_000_000).expect("sub-second nanos fit u32"),
    );
    anchor + offset
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_anchor_phase_deadline_stays_exact_past_u64_nanoseconds() {
        // ~600 years past the anchor the offset no longer fits u64
        // nanoseconds: a `Duration::from_nanos`-based implementation
        // saturates the deadline permanently into the past, turning every
        // poll into an immediate tick — a forever catch-up burst.
        let anchor = Instant::now();
        let interval = Duration::from_millis(1);
        let now = anchor + Duration::from_secs(600 * 365 * 24 * 60 * 60);

        let next = next_anchor_phase_deadline(anchor, interval, now);

        assert!(next > now, "the deadline must stay strictly after now");
        assert!(
            next - now <= interval,
            "the deadline is the first boundary after now, at most one interval away"
        );
        assert_eq!(
            (next - anchor).as_nanos() % interval.as_nanos(),
            0,
            "the deadline stays on the anchor's phase"
        );
    }
}
