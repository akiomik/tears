//! The pass-initiation seam.
//!
//! A pass's *stages* are fixed and unarbitrated (RFC 0014 §3.5). What is
//! arbitrated is which of the armed wake sources begins a pass when several
//! are ready at once, and the production policy is **unbiased selection**
//! over them: no source is preferred, and none is starved by another's
//! continuous readiness. That is the whole claim — not which source is
//! picked on any occasion, not a fairness ratio, and not an initiation order
//! a consumer could rely on.
//!
//! Enforcement of that policy is structural, at the single selection site
//! below: the review to pass is that the choice is unbiased over the armed
//! set, with no per-source priority, quota, or ordering state kept beside
//! it. A behavioral test cannot establish the absence of bias from finitely
//! many draws, and §7.2's citation rule keeps a driver-scripted initiation
//! order out of evidence for it.
//!
//! This seam is also the first of the two driving differentials the stage-3
//! driver replaces with a script (the other is the send gate); replacing it
//! is what lets a test name the source that begins a pass, and readiness
//! stays un-fabricable because the driver checks it against the real lanes
//! and the real join set.

/// The sources that can wake a parked kernel, and the exhaustive set
/// INV-RC16 arms.
///
/// A parked kernel holds a registered waker on **every** one of these, and
/// a kernel that parks while one of them has arrived and stays unconsumed
/// is non-conforming.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WakeSource {
    /// An envelope is enqueued on the data lane.
    Data,
    /// A producer-originated quit has arrived on the control lane.
    Control,
    /// A producer exit or subscription quiescence is observable on the join
    /// set — the facts the pass's first stage reflects.
    Exit,
}

/// Why a pass begins.
///
/// This is [`WakeSource`] plus one member that is deliberately *not* a wake
/// source: `PendingFrame` names work a previous pass left behind — a marked
/// redraw or subscription dirt — which a pass consumes in its frame stage
/// and which cannot wake a parked kernel, because nothing arrives to wake
/// it. RFC 0014 §10 states this as "the frame step is not among them"; the
/// two types keep the distinction from having to be remembered.
///
/// The driver accepts all four so it can name the pass that starts from
/// bootstrap's pending redraw, while `park` arms exactly the three.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PassStart {
    /// Data-lane readiness.
    Data,
    /// Control-lane arrival.
    Control,
    /// Producer exit or subscription quiescence.
    Exit,
    /// Work a previous pass marked: a pending redraw or subscription dirt.
    PendingFrame,
}

impl From<WakeSource> for PassStart {
    fn from(source: WakeSource) -> Self {
        match source {
            WakeSource::Data => Self::Data,
            WakeSource::Control => Self::Control,
            WakeSource::Exit => Self::Exit,
        }
    }
}
