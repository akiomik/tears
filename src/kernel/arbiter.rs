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
//! Enforcement of that policy is structural, at the **single selection
//! site**: the `select!` inside [`Kernel::park`], and nowhere else. The
//! review to pass is that the choice is unbiased over the armed set, with no
//! per-source priority, quota, or ordering state kept beside it — and the
//! kernel keeps no such state: nothing here or on the kernel records which
//! source last won, how often one has, or in what order the three are
//! offered. A behavioral test cannot establish the absence of bias from
//! finitely many draws, and §7.2's citation rule keeps a driver-scripted
//! initiation order out of evidence for it.
//!
//! This seam is also the first of the two driving differentials the stage-3
//! driver replaces with a script (the other is the send gate). Replacing it
//! is what lets a test name the source that begins a pass; readiness stays
//! un-fabricable because the driver checks the named source against the real
//! lanes and the real join set ([`Kernel::wake_source_ready`]), which is the
//! whole kernel-side surface that seam needs — the script chooses *among
//! arrived sources* and supplies none.
//!
//! **There is no fourth member and no second vocabulary.** A pending frame is
//! not a reason a pass begins: RFC 0014 §3.5's stage 4 consumes a pass's own
//! redraw and dirt inside that same pass, so in steady state no frame work
//! survives a pass to start the next, and bootstrap's pending render is
//! consumed by the continuation pass [`Kernel::boot`] runs itself (RFC 0008
//! §9.5). Frame work is therefore part of the park *condition*
//! ([`Kernel::pass_work_ready`] — a kernel with work does not park) without
//! being a wake source, which is why it is expressed there as a predicate
//! rather than here as a member.
//!
//! [`Kernel::park`]: super::Kernel::park
//! [`Kernel::boot`]: super::Kernel::boot
//! [`Kernel::pass_work_ready`]: super::Kernel::pass_work_ready
//! [`Kernel::wake_source_ready`]: super::Kernel::wake_source_ready

/// The sources that can wake a parked kernel, and the exhaustive set
/// INV-RC16 arms.
///
/// A parked kernel holds a registered waker on **every** one of these, and
/// a kernel that parks while one of them has arrived and stays unconsumed
/// is non-conforming.
///
/// This is also the stage-3 driver's whole pass-initiation vocabulary
/// (RFC 0008 §9.5): three members, no fourth.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WakeSource {
    /// An envelope is enqueued on the data lane.
    Data,
    /// A producer-originated quit has arrived on the control lane.
    Control,
    /// A producer exit or subscription quiescence is observable on the join
    /// set — the facts the pass's first stage reflects.
    ProducerExit,
}

impl WakeSource {
    /// The armed set, in a fixed listing order that is *not* a selection
    /// order: this is what a readiness sweep iterates, while the choice
    /// among the ready ones is made by the unbiased site in
    /// [`Kernel::park`](super::Kernel::park).
    pub const ALL: [Self; 3] = [Self::Data, Self::Control, Self::ProducerExit];
}
