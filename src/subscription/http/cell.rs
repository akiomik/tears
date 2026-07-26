use std::any::Any;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::Instant;

use super::config::QueryConfig;
use super::query::QueryError;
use super::reconcile::{FetchDecisionInput, ReconcileReason, should_fetch};
use super::result::{FetchStatus, QueryResult, QueryStatus};

/// Type-erased operations needed by the heterogeneous query cell map.
pub(super) trait AnyCell: Any + Send + Sync + 'static {
    fn into_any_arc(self: Arc<Self>) -> Arc<dyn Any + Send + Sync>;
    fn invalidate(&self, config: &QueryConfig);
    fn gc_inactive_data_and_should_evict(&self, cache_time: Duration) -> bool;
}

#[allow(
    dead_code,
    reason = "query cell is constructed only through the http feature's Query/QueryClient runtime path and the module tests"
)]
pub(super) struct Cell<T> {
    state: Mutex<CellState<T>>,
    tx: watch::Sender<QueryResult<T>>,
    // Tokio watch does not let a sender mark a receiver's current value as
    // seen. Keep our own version so a subscription can skip snapshots it has
    // already returned directly from reconcile/fetch completion paths.
    version: AtomicU64,
    lifecycle: Mutex<CellLifecycle>,
}

#[derive(Debug)]
struct CellLifecycle {
    subscribers: usize,
    inactive_since: Option<Instant>,
}

#[allow(
    dead_code,
    reason = "subscription handle is constructed only by the http query runtime and exercised by the module tests"
)]
pub(super) struct CellSubscription<T>
where
    T: Clone + Send + Sync + 'static,
{
    cell: Arc<Cell<T>>,
    rx: watch::Receiver<QueryResult<T>>,
    // Last cell-level version this subscription emitted or intentionally
    // consumed. This is separate from tokio watch's internal receiver version.
    seen_version: u64,
}

#[derive(Debug)]
#[allow(
    dead_code,
    reason = "inner cell state is populated and read only through the http cell's reconcile/complete/snapshot paths"
)]
struct CellState<T> {
    data: Option<T>,
    error: Option<QueryError>,
    current_generation: u64,
    data_generation: Option<u64>,
    data_timestamp: Option<Instant>,
    status: QueryStatus,
    fetch_status: FetchStatus,
    in_flight_generation: Option<u64>,
    last_error_generation: Option<u64>,
}

impl<T> Cell<T>
where
    T: Clone + Send + Sync + 'static,
{
    #[allow(
        dead_code,
        reason = "constructor is reached only via new_subscribed on the http query path and the module tests"
    )]
    pub(super) fn new() -> Self {
        let result = QueryResult::pending(FetchStatus::Idle);
        let (tx, _) = watch::channel(result);
        Self {
            state: Mutex::new(CellState {
                data: None,
                error: None,
                current_generation: 0,
                data_generation: None,
                data_timestamp: None,
                status: QueryStatus::Pending,
                fetch_status: FetchStatus::Idle,
                in_flight_generation: None,
                last_error_generation: None,
            }),
            tx,
            version: AtomicU64::new(0),
            lifecycle: Mutex::new(CellLifecycle {
                subscribers: 0,
                inactive_since: Some(Instant::now()),
            }),
        }
    }

    #[allow(
        dead_code,
        reason = "used only by the http query client's cell-creation path and the module tests"
    )]
    pub(super) fn new_subscribed() -> (Arc<Self>, CellSubscription<T>) {
        let cell = Arc::new(Self::new());
        {
            let mut lifecycle = cell
                .lifecycle
                .lock()
                .expect("cell lifecycle mutex should not be poisoned");
            lifecycle.subscribers = 1;
            lifecycle.inactive_since = None;
        }
        let subscription = CellSubscription {
            cell: cell.clone(),
            rx: cell.tx.subscribe(),
            seen_version: cell.version(),
        };
        (cell, subscription)
    }

    #[allow(
        dead_code,
        reason = "extra-subscriber path used only by the http query runtime and the module tests"
    )]
    pub(super) fn subscribe(self: &Arc<Self>) -> CellSubscription<T> {
        {
            let mut lifecycle = self
                .lifecycle
                .lock()
                .expect("cell lifecycle mutex should not be poisoned");
            lifecycle.subscribers += 1;
            lifecycle.inactive_since = None;
        }

        CellSubscription {
            cell: self.clone(),
            rx: self.tx.subscribe(),
            seen_version: self.version(),
        }
    }

    #[allow(
        dead_code,
        reason = "read only by the module's cfg(test) GC tests, so non-test http builds see no caller"
    )]
    pub(super) fn snapshot(&self, config: &QueryConfig) -> QueryResult<T> {
        self.state
            .lock()
            .expect("cell state mutex should not be poisoned")
            .snapshot(config)
    }

    pub(super) fn reconcile(
        &self,
        reason: ReconcileReason,
        config: &QueryConfig,
    ) -> (QueryResult<T>, Option<u64>, Option<u64>) {
        let (snapshot, generation) = {
            let mut state = self
                .state
                .lock()
                .expect("cell state mutex should not be poisoned");
            let generation_stale = state.data_generation < Some(state.current_generation);
            let input = FetchDecisionInput {
                has_data: state.data.is_some(),
                generation_stale,
                time_since_data: state.data_timestamp.map(|timestamp| timestamp.elapsed()),
                stale_time: config.stale_time,
                has_in_flight: state.in_flight_generation.is_some(),
                last_error_generation_matches_current: state.last_error_generation
                    == Some(state.current_generation),
            };

            let generation = if should_fetch(reason, input) {
                let generation = state.current_generation;
                state.in_flight_generation = Some(generation);
                state.fetch_status = FetchStatus::Fetching;
                if state.data.is_none() {
                    state.status = QueryStatus::Pending;
                }
                Some(generation)
            } else {
                None
            };

            (state.snapshot(config), generation)
        };

        let sent_version = generation
            .is_some()
            .then(|| self.send_snapshot(snapshot.clone()));

        (snapshot, generation, sent_version)
    }

    pub(super) fn complete_success(
        &self,
        generation: u64,
        data: T,
        config: &QueryConfig,
    ) -> (QueryResult<T>, bool, u64) {
        let (snapshot, committed) = {
            let mut state = self
                .state
                .lock()
                .expect("cell state mutex should not be poisoned");
            if state.in_flight_generation == Some(generation) {
                state.in_flight_generation = None;
            }

            let committed = state.current_generation == generation;
            if committed {
                state.data = Some(data);
                state.error = None;
                state.data_generation = Some(generation);
                state.data_timestamp = Some(Instant::now());
                state.status = QueryStatus::Success;
                state.fetch_status = FetchStatus::Idle;
                state.last_error_generation = None;
            }

            (state.snapshot(config), committed)
        };
        let sent_version = self.send_snapshot(snapshot.clone());
        (snapshot, committed, sent_version)
    }

    pub(super) fn complete_error(
        &self,
        generation: u64,
        error: QueryError,
        config: &QueryConfig,
    ) -> (QueryResult<T>, bool, u64) {
        let (snapshot, committed) = {
            let mut state = self
                .state
                .lock()
                .expect("cell state mutex should not be poisoned");
            if state.in_flight_generation == Some(generation) {
                state.in_flight_generation = None;
            }

            let committed = state.current_generation == generation;
            if committed {
                state.error = Some(error);
                state.status = QueryStatus::Error;
                state.fetch_status = FetchStatus::Idle;
                state.last_error_generation = Some(generation);
            }

            (state.snapshot(config), committed)
        };
        let sent_version = self.send_snapshot(snapshot.clone());
        (snapshot, committed, sent_version)
    }

    pub(super) fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    fn send_snapshot(&self, snapshot: QueryResult<T>) -> u64 {
        let version = self.version.fetch_add(1, Ordering::AcqRel) + 1;
        let _ = self.tx.send(snapshot);
        version
    }
}

#[cfg(test)]
impl<T> Cell<T> {
    pub(super) fn subscriber_count(&self) -> usize {
        self.lifecycle
            .lock()
            .expect("cell lifecycle mutex should not be poisoned")
            .subscribers
    }

    pub(super) fn inactive_since(&self) -> Option<Instant> {
        self.lifecycle
            .lock()
            .expect("cell lifecycle mutex should not be poisoned")
            .inactive_since
    }
}

impl<T> CellSubscription<T>
where
    T: Clone + Send + Sync + 'static,
{
    #[allow(
        dead_code,
        reason = "shared-borrow accessor kept for symmetry with receiver_mut; the stream only needs the mutable accessor, so this one has no caller"
    )]
    pub(super) const fn receiver(&self) -> &watch::Receiver<QueryResult<T>> {
        &self.rx
    }

    #[allow(
        dead_code,
        reason = "used only by the http subscription stream's watch_cell change-wait loop"
    )]
    pub(super) const fn receiver_mut(&mut self) -> &mut watch::Receiver<QueryResult<T>> {
        &mut self.rx
    }

    pub(super) const fn seen_version(&self) -> u64 {
        self.seen_version
    }

    pub(super) const fn mark_seen_version(&mut self, version: u64) {
        self.seen_version = version;
    }
}

impl<T> Drop for CellSubscription<T>
where
    T: Clone + Send + Sync + 'static,
{
    fn drop(&mut self) {
        let mut lifecycle = self
            .cell
            .lifecycle
            .lock()
            .expect("cell lifecycle mutex should not be poisoned");

        debug_assert!(
            lifecycle.subscribers > 0,
            "cell subscription count should not underflow"
        );

        lifecycle.subscribers = lifecycle.subscribers.saturating_sub(1);
        if lifecycle.subscribers == 0 {
            lifecycle.inactive_since = Some(Instant::now());
        }
    }
}

impl<T> AnyCell for Cell<T>
where
    T: Clone + Send + Sync + 'static,
{
    fn into_any_arc(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn invalidate(&self, config: &QueryConfig) {
        let snapshot = {
            let mut state = self
                .state
                .lock()
                .expect("cell state mutex should not be poisoned");
            state.current_generation = state.current_generation.saturating_add(1);
            state.snapshot(config)
        };
        self.send_snapshot(snapshot);
    }

    fn gc_inactive_data_and_should_evict(&self, cache_time: Duration) -> bool {
        let lifecycle = self
            .lifecycle
            .lock()
            .expect("cell lifecycle mutex should not be poisoned");

        let should_clear = lifecycle.subscribers == 0
            && lifecycle
                .inactive_since
                .is_some_and(|inactive_since| inactive_since.elapsed() >= cache_time);

        if !should_clear {
            return false;
        }

        let cleared = {
            let mut state = self
                .state
                .lock()
                .expect("cell state mutex should not be poisoned");

            if state.data.is_none() && state.error.is_none() {
                false
            } else {
                state.data = None;
                state.error = None;
                state.data_generation = None;
                state.data_timestamp = None;
                state.status = QueryStatus::Pending;
                state.fetch_status = FetchStatus::Idle;
                state.in_flight_generation = None;
                state.last_error_generation = None;
                true
            }
        };
        drop(lifecycle);

        if cleared {
            self.send_snapshot(QueryResult::pending(FetchStatus::Idle));
        }

        true
    }
}

impl<T> CellState<T>
where
    T: Clone,
{
    #[allow(
        dead_code,
        reason = "invoked only through the http cell's reconcile/complete/snapshot paths"
    )]
    fn snapshot(&self, config: &QueryConfig) -> QueryResult<T> {
        let is_stale = self.data.is_some()
            && (self.data_generation < Some(self.current_generation)
                || self
                    .data_timestamp
                    .is_some_and(|timestamp| timestamp.elapsed() >= config.stale_time));

        match self.status {
            QueryStatus::Pending => QueryResult::pending(self.fetch_status),
            QueryStatus::Success => {
                let data = self
                    .data
                    .clone()
                    .expect("success cell state should contain data");
                QueryResult::success(data, is_stale, self.fetch_status)
            }
            QueryStatus::Error => QueryResult::failed(
                self.error
                    .clone()
                    .expect("error cell state should contain error"),
                self.data.clone(),
                is_stale,
                self.fetch_status,
            ),
        }
    }
}
