//! Pending render/subscription work tracking for the runtime event loop.
//!
//! The runtime renders conditionally (only when the view may have changed) and
//! re-evaluates subscriptions only after a message has been processed. This
//! module owns those two flags and the [`PendingWork::has_pending_work`] query
//! that the event loop uses to gate its frame branch, keeping that bookkeeping
//! out of the main loop.
//!
//! This type tracks *whether* there is work; it does not own the frame timer or
//! decide *when* to run — the runtime still owns the `Interval` and the
//! `take` → render/update ordering.

/// Tracks whether the runtime has pending render or subscription work.
///
/// Fields are `pub(super)` so the parent `runtime` module (and its tests) can
/// inspect them directly; the methods express the intended transitions.
pub(super) struct PendingWork {
    /// Whether the UI needs to be re-rendered (conditional rendering optimization).
    pub(super) needs_redraw: bool,
    /// Whether subscriptions may have changed and should be re-evaluated.
    ///
    /// `subscriptions()` is a pure function of the application state, which only
    /// changes when a message is processed. This flag is set after each message
    /// batch so that subscriptions are re-evaluated only when the state may have
    /// changed, rather than on every frame.
    pub(super) subscriptions_dirty: bool,
}

impl PendingWork {
    /// Creates the tracker in the initial state.
    ///
    /// The first frame must always render, so `needs_redraw` starts `true`. No
    /// subscription re-evaluation is pending: the initial subscriptions are
    /// started by the runtime before the loop begins, so `subscriptions_dirty`
    /// starts `false`.
    pub(super) const fn new() -> Self {
        Self {
            needs_redraw: true,
            subscriptions_dirty: false,
        }
    }

    /// Records whether a processed command requested a redraw.
    ///
    /// Uses `|=` so that a command opting out of a redraw cannot clear a redraw
    /// already requested by another command in the same batch.
    pub(super) const fn record_redraw(&mut self, requested: bool) {
        self.needs_redraw |= requested;
    }

    /// Marks that subscriptions may have changed and should be re-evaluated on
    /// the next frame. Called after each processed message batch, even when the
    /// batch suppressed a redraw.
    pub(super) const fn mark_subscriptions_dirty(&mut self) {
        self.subscriptions_dirty = true;
    }

    /// Whether the frame tick has pending work (a redraw or a subscription
    /// re-evaluation) to do.
    ///
    /// Gates the frame branch of the event loop's `select!`: when the application
    /// is idle (no message processed since the last frame), both flags are false
    /// and a frame tick would do nothing, so the loop can skip waking at the
    /// frame rate entirely — making it event-driven and saving CPU/battery.
    ///
    /// Must use `||` (not `&&`): the initial state has `needs_redraw == true` and
    /// `subscriptions_dirty == false`, and the first frame must still render.
    pub(super) const fn has_pending_work(&self) -> bool {
        self.needs_redraw || self.subscriptions_dirty
    }

    /// Returns whether a redraw is pending and clears the flag.
    pub(super) const fn take_redraw(&mut self) -> bool {
        let pending = self.needs_redraw;
        self.needs_redraw = false;
        pending
    }

    /// Returns whether subscriptions need re-evaluation and clears the flag.
    pub(super) const fn take_subscriptions_dirty(&mut self) -> bool {
        let pending = self.subscriptions_dirty;
        self.subscriptions_dirty = false;
        pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_starts_with_a_pending_first_frame() {
        let pending = PendingWork::new();
        assert!(pending.needs_redraw, "the first frame must render");
        assert!(
            !pending.subscriptions_dirty,
            "initial subscriptions are started outside the frame loop"
        );
        assert!(pending.has_pending_work());
    }

    #[test]
    fn has_pending_work_is_the_or_of_both_flags() {
        // Exhaustive truth table: the gate must fire when either flag is set,
        // and only go idle when both are clear.
        for (needs_redraw, subscriptions_dirty, expected) in [
            (false, false, false),
            (true, false, true),
            (false, true, true),
            (true, true, true),
        ] {
            let pending = PendingWork {
                needs_redraw,
                subscriptions_dirty,
            };
            assert_eq!(
                pending.has_pending_work(),
                expected,
                "needs_redraw={needs_redraw}, subscriptions_dirty={subscriptions_dirty}"
            );
        }
    }

    #[test]
    fn record_redraw_never_clears_an_existing_request() {
        let mut pending = PendingWork::new();
        pending.take_redraw(); // clear the initial pending frame

        // A command that does not request a redraw leaves the flag clear.
        pending.record_redraw(false);
        assert!(!pending.needs_redraw);

        // A command that requests a redraw sets it.
        pending.record_redraw(true);
        assert!(pending.needs_redraw);

        // A later opt-out cannot clear an already-requested redraw.
        pending.record_redraw(false);
        assert!(pending.needs_redraw);
    }

    #[test]
    fn mark_subscriptions_dirty_sets_the_flag() {
        let mut pending = PendingWork::new();
        assert!(!pending.subscriptions_dirty);
        pending.mark_subscriptions_dirty();
        assert!(pending.subscriptions_dirty);
    }

    #[test]
    fn take_redraw_returns_then_clears() {
        let mut pending = PendingWork::new();
        assert!(pending.take_redraw(), "initial state has a pending redraw");
        assert!(!pending.needs_redraw);
        assert!(!pending.take_redraw(), "already taken");
    }

    #[test]
    fn take_subscriptions_dirty_returns_then_clears() {
        let mut pending = PendingWork::new();
        pending.mark_subscriptions_dirty();
        assert!(pending.take_subscriptions_dirty());
        assert!(!pending.subscriptions_dirty);
        assert!(!pending.take_subscriptions_dirty(), "already taken");
    }

    #[test]
    fn goes_idle_once_pending_work_is_drained() {
        let mut pending = PendingWork::new();
        pending.mark_subscriptions_dirty();

        // Draining both kinds of pending work leaves the tracker idle, so the
        // event loop's frame branch is gated off.
        pending.take_redraw();
        pending.take_subscriptions_dirty();
        assert!(!pending.has_pending_work());

        // A new message batch re-arms the frame branch.
        pending.mark_subscriptions_dirty();
        assert!(pending.has_pending_work());
    }
}
