use std::time::Duration;

/// The reason a query stream is reconciling its cell state.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ReconcileReason {
    InitialObserve,
    WatchChanged,
}

/// Snapshot of the state needed to decide whether reconcile should fetch.
#[expect(
    clippy::struct_excessive_bools,
    reason = "struct models several independent boolean fetch-decision inputs, not a single state better expressed as an enum"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FetchDecisionInput {
    pub(super) has_data: bool,
    pub(super) generation_stale: bool,
    pub(super) time_since_data: Option<Duration>,
    pub(super) stale_time: Duration,
    pub(super) has_in_flight: bool,
    pub(super) last_error_generation_matches_current: bool,
}

/// Decides whether a reconcile pass should start a fetch.
///
/// Data absence and generation staleness are fetch reasons for every reconcile
/// reason. Time staleness is only a fetch reason for observe boundaries, not
/// for [`ReconcileReason::WatchChanged`].
#[must_use]
pub(super) fn should_fetch(reason: ReconcileReason, input: FetchDecisionInput) -> bool {
    let time_stale = input
        .time_since_data
        .is_some_and(|elapsed| elapsed >= input.stale_time);
    let time_stale_can_fetch = matches!(reason, ReconcileReason::InitialObserve);
    let needs_data_or_generation_fetch = !input.has_data || input.generation_stale;
    let needs_time_stale_fetch = input.has_data && time_stale && time_stale_can_fetch;

    (needs_data_or_generation_fetch || needs_time_stale_fetch)
        && !input.has_in_flight
        && !input.last_error_generation_matches_current
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    #[test]
    fn watch_changed_does_not_refetch_for_time_stale_same_generation() {
        let should_fetch = should_fetch(
            ReconcileReason::WatchChanged,
            FetchDecisionInput {
                has_data: true,
                generation_stale: false,
                time_since_data: Some(Duration::ZERO),
                stale_time: Duration::ZERO,
                has_in_flight: false,
                last_error_generation_matches_current: false,
            },
        );

        assert!(
            !should_fetch,
            "WatchChanged must not start a same-generation refetch for time-stale data"
        );
    }

    #[test]
    fn watch_changed_fetches_for_missing_data_or_generation_stale_data() {
        let missing_data = should_fetch(
            ReconcileReason::WatchChanged,
            FetchDecisionInput {
                has_data: false,
                generation_stale: false,
                time_since_data: None,
                stale_time: Duration::ZERO,
                has_in_flight: false,
                last_error_generation_matches_current: false,
            },
        );
        assert!(missing_data, "missing data should fetch for every reason");

        let generation_stale = should_fetch(
            ReconcileReason::WatchChanged,
            FetchDecisionInput {
                has_data: true,
                generation_stale: true,
                time_since_data: Some(Duration::ZERO),
                stale_time: Duration::ZERO,
                has_in_flight: false,
                last_error_generation_matches_current: false,
            },
        );
        assert!(
            generation_stale,
            "generation-stale data should fetch for every reason"
        );
    }
}
