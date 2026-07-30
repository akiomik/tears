//! HTTP query operations with retained data and automatic refetching.
//!
//! This module provides the [`Query`] subscription and [`QueryClient`] for managing
//! HTTP GET requests with retained data, similar to SWR or TanStack Query.
//!
//! # Design Pattern: Subscription-based State Management
//!
//! Unlike traditional HTTP clients, queries are **subscriptions** that continuously
//! monitor and manage retained data. When you subscribe to a query:
//!
//! 1. If retained data exists, it's immediately emitted
//! 2. If data is stale or missing, a fetch is automatically triggered
//! 3. When the query is invalidated, refetching happens automatically
//!
//! Failed fetches are emitted with [`QueryStatus::Error`](super::QueryStatus::Error) and are not retried
//! automatically. To retry after an error, invalidate the query key with
//! [`QueryClient::invalidate`], or include your own retry/backoff behavior in
//! the fetcher.
//!
//! This design keeps your UI in sync with the data state without manual management.
//!
//! # Example
//!
//! ```rust,ignore
//! use tears::prelude::*;
//! use tears::subscription::http::{Query, QueryClient, QueryResult};
//! use std::sync::Arc;
//!
//! struct App {
//!     query_client: Arc<QueryClient>,
//!     user_result: Option<QueryResult<User>>,
//! }
//!
//! impl Application for App {
//!     fn subscriptions(&self) -> Vec<Subscription<Message>> {
//!         vec![
//!             Subscription::new(Query::new(
//!                 "user-123",
//!                 || Box::pin(fetch_user()),
//!                 self.query_client.clone(),
//!             ))
//!             .map(Message::UserQuery)
//!         ]
//!     }
//!
//!     fn update(&mut self, msg: Message) -> Command<Message> {
//!         match msg {
//!             Message::UserQuery(result) => {
//!                 self.user_result = Some(result);
//!                 Command::none()
//!             }
//!             Message::RefreshUser => {
//!                 self.query_client.invalidate("user-123");
//!                 Command::none()
//!             }
//!         }
//!     }
//! }
//! ```

use std::any::{TypeId, type_name};
use std::fmt;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use tokio::time::Instant;

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::{self, BoxStream};
use thiserror::Error;

use crate::subscription::SubscriptionSource;

use super::cell::{AnyCell, Cell, CellSubscription};
use super::config::QueryConfig;
use super::key::QueryKey;
use super::reconcile::ReconcileReason;
#[cfg(test)]
use super::result::FetchStatus;
use super::result::QueryResult;

static NEXT_QUERY_CLIENT_ID: AtomicU64 = AtomicU64::new(1);

/// Error type for query operations.
#[derive(Error, Debug, Clone)]
pub enum QueryError {
    /// The fetcher returned an application-level failure.
    #[error("Fetch failed: {0}")]
    FetchError(String),

    /// The fetcher failed due to a network-level issue.
    #[error("Network error: {0}")]
    NetworkError(String),
}

/// A client for managing query retention and invalidation.
///
/// The `QueryClient` is the central state manager for queries. It handles:
/// - Retaining query results while cells are active or recently inactive
/// - Configuration management
///
/// # Example
///
/// ```rust
/// use tears::subscription::http::{QueryClient, QueryConfig};
/// use std::sync::Arc;
/// use std::time::Duration;
///
/// let config = QueryConfig::new(
///     Duration::from_secs(30),  // stale_time
///     Duration::from_secs(300), // cache_time
/// );
///
/// let client = Arc::new(QueryClient::with_config(config));
/// ```
#[derive(Clone)]
pub struct QueryClient {
    client_id: u64,
    cells: Arc<DashMap<(TypeId, QueryKey), Arc<dyn AnyCell>>>,
    config: QueryConfig,
}

impl fmt::Debug for QueryClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QueryClient")
            .field("client_id", &self.client_id)
            .field("cells", &self.cells.len())
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl QueryClient {
    /// Creates a new query client with default configuration.
    ///
    /// # Panics
    ///
    /// Panics if the process-wide client-id space is exhausted (`u64::MAX`
    /// clients already created): `client_id` is a component of subscription
    /// identity, so ids are never reused within a process (RFC 0001).
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(QueryConfig::default())
    }

    /// Creates a new query client with the given configuration.
    ///
    /// # Panics
    ///
    /// Panics if the process-wide client-id space is exhausted (`u64::MAX`
    /// clients already created): `client_id` is a component of subscription
    /// identity, so ids are never reused within a process (RFC 0001).
    #[must_use]
    pub fn with_config(config: QueryConfig) -> Self {
        Self {
            // `client_id` is an identity component — subscription identity is
            // `(client_id, TypeId, QueryKey)` (RFC 0001) — so reusing an id
            // would collide distinct clients' identities. The allocator fails
            // before it can reuse a value: `checked_add` leaves the counter
            // saturated at `u64::MAX`, making this and every later allocation
            // panic instead of wrapping into reuse.
            client_id: NEXT_QUERY_CLIENT_ID
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
                .expect(
                    "QueryClient id space exhausted; client ids are never \
                     reused within a process (RFC 0001 subscription identity)",
                ),
            cells: Arc::new(DashMap::new()),
            config,
        }
    }

    /// Invalidates the retained data for the given key, triggering refetch in active queries.
    ///
    /// Invalidation is applied synchronously: matching cells have their
    /// generation bumped before this method returns.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// fn update(&mut self, msg: Message) -> Command<Message> {
    ///     match msg {
    ///         Message::UserUpdated => {
    ///             self.query_client.invalidate("user-123");
    ///             Command::none()
    ///         }
    ///     }
    /// }
    /// ```
    pub fn invalidate<K>(&self, key: K)
    where
        K: Into<QueryKey>,
    {
        let query_key = key.into();
        let key_hash = trace_key_hash(&query_key);
        let mut matched_cells = 0usize;

        for entry in self.cells.iter() {
            if entry.key().1 == query_key {
                matched_cells += 1;
                entry.value().invalidate(&self.config);
            }
        }

        tracing::debug!(
            target: "tears::subscription::http",
            client_id = self.client_id,
            key_hash,
            matched_cells,
            "query invalidated"
        );
    }

    /// Removes retained data that has outlived `cache_time`.
    ///
    /// Applications can call [`QueryClient::gc`] to trigger a sweep explicitly
    /// (for example when no further fetches are expected for a while).
    fn gc_expired(&self) {
        let cache_time = self.config.cache_time;
        let cells_before = self.cells.len();
        self.cells
            .retain(|_, cell| !cell.gc_inactive_data_and_should_evict(cache_time));
        let evicted_cells = cells_before.saturating_sub(self.cells.len());
        if evicted_cells > 0 {
            tracing::trace!(
                target: "tears::subscription::http",
                client_id = self.client_id,
                evicted_cells,
                "query cache gc evicted cells"
            );
        }
    }

    /// Removes retained query data that has outlived the configured `cache_time`.
    ///
    /// Active cells keep their data regardless of `cache_time`; inactive cells
    /// clear their data and are removed after `cache_time` has elapsed since
    /// the last subscriber dropped.
    ///
    /// # Example
    ///
    /// ```rust
    /// use tears::subscription::http::QueryClient;
    ///
    /// let client = QueryClient::new();
    /// // ... time passes, queries become inactive ...
    /// client.gc();
    /// ```
    pub fn gc(&self) {
        self.gc_expired();
    }

    /// Gets the query configuration.
    const fn config(&self) -> &QueryConfig {
        &self.config
    }

    pub(super) fn get_or_subscribe_cell<T>(
        &self,
        key: impl Into<QueryKey>,
    ) -> (Arc<Cell<T>>, CellSubscription<T>)
    where
        T: Clone + Send + Sync + 'static,
    {
        let query_key = key.into();
        let key_hash = trace_key_hash(&query_key);
        let cell_key = (TypeId::of::<T>(), query_key);

        match self.cells.entry(cell_key) {
            Entry::Occupied(entry) => {
                tracing::trace!(
                    target: "tears::subscription::http",
                    client_id = self.client_id,
                    key_hash,
                    value_type = type_name::<T>(),
                    reused = true,
                    "query cell subscribed"
                );
                let cell = downcast_cell(entry.get().clone());
                let subscription = cell.subscribe();
                (cell, subscription)
            }
            Entry::Vacant(entry) => {
                tracing::trace!(
                    target: "tears::subscription::http",
                    client_id = self.client_id,
                    key_hash,
                    value_type = type_name::<T>(),
                    reused = false,
                    "query cell subscribed"
                );
                let (cell, subscription) = Cell::<T>::new_subscribed();
                let new_cell: Arc<dyn AnyCell> = cell.clone();
                entry.insert(new_cell);
                (cell, subscription)
            }
        }
    }
}

fn downcast_cell<T>(cell: Arc<dyn AnyCell>) -> Arc<Cell<T>>
where
    T: Clone + Send + Sync + 'static,
{
    cell.into_any_arc()
        .downcast::<Cell<T>>()
        .expect("TypeId keyed cell slot should contain the requested type")
}

impl Default for QueryClient {
    fn default() -> Self {
        Self::new()
    }
}

/// A shared, reusable async fetcher producing a query's value.
type Fetcher<V> = Arc<dyn Fn() -> BoxFuture<'static, Result<V, QueryError>> + Send + Sync>;

/// A query subscription that monitors and fetches retained data.
///
/// `Query` is a subscription that automatically manages data fetching and retention.
/// When subscribed:
///
/// 1. If retained data exists, it's immediately emitted as `Success`
/// 2. If data is missing or stale, a fetch is triggered and `Loading` is emitted
/// 3. When invalidated, the query automatically refetches
///
/// If a fetch fails, the query emits an error [`QueryResult`] and waits for the
/// next invalidation. It does not retry automatically; call
/// [`QueryClient::invalidate`] to request another fetch, or implement retry
/// behavior inside the fetcher.
///
/// # Query keys
///
/// The query key identifies the retained cell and the running subscription.
/// Include every request parameter used by the fetcher in the key. If the
/// fetcher captures values such as a user ID, search term, page number, or base
/// URL but the key does not change, the runtime keeps the existing subscription
/// and continues using the old fetcher.
///
/// # Changing the request
///
/// Replacing the fetcher while keeping the same key is **not supported**. The
/// runtime keys the running subscription by its identity (`QueryClient`, key,
/// and value type), so constructing a new `Query::new(key, new_fetcher, client)`
/// with an unchanged key keeps the existing stream and the old fetcher; the new
/// fetcher never takes effect. **To change the request, change the key** (for
/// example by including the varying parameter in it).
///
/// # Example
///
/// ```rust,ignore
/// use tears::Subscription;
/// use tears::subscription::http::{Query, QueryClient};
/// use std::sync::Arc;
///
/// let client = Arc::new(QueryClient::new());
///
/// let query = Subscription::new(Query::new(
///     "user-123",
///     || Box::pin(async { fetch_user().await }),
///     client.clone(),
/// ))
/// .map(Message::UserQuery);
/// ```
pub struct Query<V> {
    key: QueryKey,
    fetcher: Fetcher<V>,
    client: Arc<QueryClient>,
}

impl<V> Query<V>
where
    V: Clone + Send + Sync + 'static,
{
    /// Creates a new query with the given key, fetcher, and client.
    ///
    /// # Arguments
    ///
    /// * `key` - A unique identifier for this query (used for retained data)
    /// * `fetcher` - An async function that fetches the data
    /// * `client` - The query client for retained data management
    ///
    /// The key must include every value that changes the request made by the
    /// fetcher. For example, prefer keys like `format!("user-{user_id}")` or
    /// `format!("todos-page-{page}")` over a constant key when the fetcher
    /// captures `user_id` or `page`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let query = Query::new(
    ///     "user-123",
    ///     || Box::pin(async {
    ///         fetch_user_from_api().await
    ///     }),
    ///     query_client,
    /// );
    /// ```
    pub fn new<K, F>(key: K, fetcher: F, client: Arc<QueryClient>) -> Self
    where
        K: Into<QueryKey>,
        F: Fn() -> BoxFuture<'static, Result<V, QueryError>> + Send + Sync + 'static,
    {
        Self {
            key: key.into(),
            fetcher: Arc::new(fetcher),
            client,
        }
    }
}

impl<V> SubscriptionSource for Query<V>
where
    V: Clone + Send + Sync + 'static,
{
    type Output = QueryResult<V>;
    type Key = (u64, QueryKey);

    fn stream(&self) -> BoxStream<'static, Self::Output> {
        let key = self.key.clone();
        let fetcher = self.fetcher.clone();
        let client = self.client.clone();
        let trace = QueryTrace {
            client_id: self.client.client_id,
            key_hash: trace_key_hash(&self.key),
            value_type: type_name::<V>(),
        };

        stream::unfold(State::Initial, move |state| {
            let key = key.clone();
            let fetcher = fetcher.clone();
            let client = client.clone();
            let trace = trace;

            async move {
                match state {
                    State::Initial => {
                        let (cell, mut subscription) =
                            client.get_or_subscribe_cell::<V>(key.clone());
                        let (result, generation, sent_version) =
                            cell.reconcile(ReconcileReason::InitialObserve, client.config());
                        if let Some(version) = sent_version {
                            subscription.mark_seen_version(version);
                        }
                        let next = if let Some(generation) = generation {
                            tracing::trace!(
                                target: "tears::subscription::http",
                                client_id = trace.client_id,
                                key_hash = trace.key_hash,
                                value_type = trace.value_type,
                                generation,
                                reason = "initial_observe",
                                "query fetch scheduled"
                            );
                            State::Fetching {
                                subscription,
                                cell,
                                generation,
                            }
                        } else {
                            State::Watching { subscription, cell }
                        };
                        Some((result, next))
                    }

                    State::Fetching {
                        subscription,
                        cell,
                        generation,
                    } => {
                        let result = perform_fetch(
                            &fetcher,
                            cell,
                            client.config(),
                            generation,
                            subscription,
                            trace,
                        )
                        .await;
                        client.gc_expired();
                        Some(result)
                    }

                    State::Watching { subscription, cell } => {
                        watch_cell(subscription, cell, client.config(), trace).await
                    }
                }
            }
        })
        .boxed()
    }

    fn key(&self) -> Self::Key {
        (self.client.client_id, self.key.clone())
    }
}

#[derive(Clone, Copy)]
struct QueryTrace {
    client_id: u64,
    key_hash: u64,
    value_type: &'static str,
}

/// Internal state machine for the Query subscription.
enum State<V>
where
    V: Clone + Send + Sync + 'static,
{
    Initial,
    Fetching {
        subscription: CellSubscription<V>,
        cell: Arc<Cell<V>>,
        generation: u64,
    },
    Watching {
        subscription: CellSubscription<V>,
        cell: Arc<Cell<V>>,
    },
}

async fn perform_fetch<V>(
    fetcher: &Fetcher<V>,
    cell: Arc<Cell<V>>,
    config: &QueryConfig,
    generation: u64,
    mut subscription: CellSubscription<V>,
    trace: QueryTrace,
) -> (QueryResult<V>, State<V>)
where
    V: Clone + Send + Sync + 'static,
{
    tracing::debug!(
        target: "tears::subscription::http",
        client_id = trace.client_id,
        key_hash = trace.key_hash,
        value_type = trace.value_type,
        generation,
        "query fetch started"
    );
    let started_at = Instant::now();
    let (result, generation) = match fetcher().await {
        Ok(data) => {
            let (result, committed, sent_version) = cell.complete_success(generation, data, config);
            tracing::debug!(
                target: "tears::subscription::http",
                client_id = trace.client_id,
                key_hash = trace.key_hash,
                value_type = trace.value_type,
                generation,
                committed,
                elapsed_ms = started_at.elapsed().as_millis(),
                "query fetch succeeded"
            );
            if committed {
                subscription.mark_seen_version(sent_version);
                (result, None)
            } else {
                let (result, generation, sent_version) =
                    cell.reconcile(ReconcileReason::WatchChanged, config);
                if let Some(version) = sent_version {
                    subscription.mark_seen_version(version);
                }
                if let Some(next_generation) = generation {
                    tracing::trace!(
                        target: "tears::subscription::http",
                        client_id = trace.client_id,
                        key_hash = trace.key_hash,
                        value_type = trace.value_type,
                        generation = next_generation,
                        reason = "stale_success_completion",
                        "query fetch scheduled"
                    );
                }
                (result, generation)
            }
        }
        Err(error) => {
            let error_kind = trace_query_error_kind(&error);
            let (result, committed, sent_version) = cell.complete_error(generation, error, config);
            tracing::debug!(
                target: "tears::subscription::http",
                client_id = trace.client_id,
                key_hash = trace.key_hash,
                value_type = trace.value_type,
                generation,
                committed,
                error_kind,
                elapsed_ms = started_at.elapsed().as_millis(),
                "query fetch failed"
            );
            if committed {
                subscription.mark_seen_version(sent_version);
                (result, None)
            } else {
                let (result, generation, sent_version) =
                    cell.reconcile(ReconcileReason::WatchChanged, config);
                if let Some(version) = sent_version {
                    subscription.mark_seen_version(version);
                }
                if let Some(next_generation) = generation {
                    tracing::trace!(
                        target: "tears::subscription::http",
                        client_id = trace.client_id,
                        key_hash = trace.key_hash,
                        value_type = trace.value_type,
                        generation = next_generation,
                        reason = "stale_error_completion",
                        "query fetch scheduled"
                    );
                }
                (result, generation)
            }
        }
    };
    let next = if let Some(generation) = generation {
        State::Fetching {
            subscription,
            cell,
            generation,
        }
    } else {
        State::Watching { subscription, cell }
    };

    (result, next)
}

async fn watch_cell<V>(
    mut subscription: CellSubscription<V>,
    cell: Arc<Cell<V>>,
    config: &QueryConfig,
    trace: QueryTrace,
) -> Option<(QueryResult<V>, State<V>)>
where
    V: Clone + Send + Sync + 'static,
{
    loop {
        if subscription.receiver_mut().changed().await.is_err() {
            return None;
        }

        let changed_version = cell.version();
        if changed_version <= subscription.seen_version() {
            continue;
        }

        let (result, generation, sent_version) =
            cell.reconcile(ReconcileReason::WatchChanged, config);
        subscription.mark_seen_version(sent_version.unwrap_or(changed_version));
        let next = if let Some(generation) = generation {
            tracing::trace!(
                target: "tears::subscription::http",
                client_id = trace.client_id,
                key_hash = trace.key_hash,
                value_type = trace.value_type,
                generation,
                reason = "watch_changed",
                "query fetch scheduled"
            );
            State::Fetching {
                subscription,
                cell,
                generation,
            }
        } else {
            State::Watching { subscription, cell }
        };
        return Some((result, next));
    }
}

fn trace_key_hash(key: &QueryKey) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

const fn trace_query_error_kind(error: &QueryError) -> &'static str {
    match error {
        QueryError::FetchError(_) => "fetch",
        QueryError::NetworkError(_) => "network",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::AtomicUsize;

    use tokio::time::{Duration, timeout};

    use crate::test_support::{assert_pending_until, gate_fetches};

    #[test]
    fn test_query_result_data() {
        let result = QueryResult::success(42, false, FetchStatus::Idle);
        assert_eq!(result.data(), Some(&42));

        let result: QueryResult<i32> = QueryResult::pending(FetchStatus::Fetching);
        assert_eq!(result.data(), None);

        let result: QueryResult<i32> = QueryResult::failed(
            QueryError::FetchError("error".to_owned()),
            None,
            false,
            FetchStatus::Idle,
        );
        assert_eq!(result.data(), None);
    }

    #[test]
    fn test_query_result_predicates() {
        let loading: QueryResult<i32> = QueryResult::pending(FetchStatus::Fetching);
        assert!(loading.is_loading());
        assert!(!loading.is_success());
        assert!(!loading.is_error());
        assert!(!loading.is_stale());

        let success = QueryResult::success(42, false, FetchStatus::Idle);
        assert!(!success.is_loading());
        assert!(success.is_success());
        assert!(!success.is_error());
        assert!(!success.is_stale());

        let stale = QueryResult::success(42, true, FetchStatus::Idle);
        assert!(!stale.is_loading());
        assert!(stale.is_success());
        assert!(!stale.is_error());
        assert!(stale.is_stale());

        let error: QueryResult<i32> = QueryResult::failed(
            QueryError::FetchError("error".to_owned()),
            None,
            false,
            FetchStatus::Idle,
        );
        assert!(!error.is_loading());
        assert!(!error.is_success());
        assert!(error.is_error());
        assert!(!error.is_stale());
    }

    #[test]
    fn test_query_client_new() {
        let client = QueryClient::new();
        assert_eq!(client.config.stale_time, Duration::from_secs(0));
    }

    #[test]
    fn test_query_client_with_config() {
        let config = QueryConfig::new(Duration::from_secs(30), Duration::from_secs(300));
        let client = QueryClient::with_config(config);
        assert_eq!(client.config.stale_time, Duration::from_secs(30));
        assert_eq!(client.config.cache_time, Duration::from_secs(300));
    }

    #[test]
    fn test_query_client_cells_are_type_separated() {
        let client = QueryClient::new();

        let (int_cell, _int_subscription) = client.get_or_subscribe_cell::<i32>("data");
        let (string_cell, _string_subscription) = client.get_or_subscribe_cell::<String>("data");
        let (int_cell_again, _second_int_subscription) =
            client.get_or_subscribe_cell::<i32>("data");

        assert!(Arc::ptr_eq(&int_cell, &int_cell_again));
        assert_eq!(int_cell.subscriber_count(), 2);
        assert_eq!(string_cell.subscriber_count(), 1);
        assert_eq!(client.cells.len(), 2);
    }

    #[test]
    fn test_subscribed_cell_is_not_gc_evicted_with_zero_cache_time() {
        let config = QueryConfig::new(Duration::ZERO, Duration::ZERO);
        let client = QueryClient::with_config(config);

        let (cell, _subscription) = client.get_or_subscribe_cell::<i32>("data");
        client.gc();

        let (same_key_cell, _second_subscription) = client.get_or_subscribe_cell::<i32>("data");
        assert!(
            Arc::ptr_eq(&cell, &same_key_cell),
            "active cell must not be evicted between insertion and subscription"
        );
    }

    #[test]
    fn test_gc_does_not_clear_active_cell_data_with_zero_cache_time() {
        let config = QueryConfig::new(Duration::ZERO, Duration::ZERO);
        let client = QueryClient::with_config(config);

        let (cell, _subscription) = client.get_or_subscribe_cell::<i32>("data");
        let (result, committed, _) = cell.complete_success(0, 42, client.config());
        assert!(committed);
        assert_eq!(result.data(), Some(&42));

        client.gc();

        let after_gc = cell.snapshot(client.config());
        assert_eq!(
            after_gc.data(),
            Some(&42),
            "active cell data must not be collected by cache_time"
        );
    }

    #[test]
    fn test_gc_clears_inactive_cell_data_after_cache_time() {
        let config = QueryConfig::new(Duration::ZERO, Duration::ZERO);
        let client = QueryClient::with_config(config);

        let (cell, subscription) = client.get_or_subscribe_cell::<i32>("data");
        let (result, committed, _) = cell.complete_success(0, 42, client.config());
        assert!(committed);
        assert_eq!(result.data(), Some(&42));

        drop(subscription);
        client.gc();

        let after_gc = cell.snapshot(client.config());
        assert!(
            after_gc.data().is_none() && after_gc.is_loading(),
            "inactive cell data should be collected after cache_time"
        );
        assert_eq!(
            client.cells.len(),
            0,
            "inactive cell shell should be evicted after its data is collected"
        );
    }

    #[tokio::test]
    async fn test_fetch_triggers_auto_sweep_of_inactive_cell() {
        // Verify that gc_expired() is called after every fetch, without an
        // explicit client.gc() call.  If the auto-sweep hook is removed this
        // test will fail because cell A will still be in cells after B's fetch.
        let config = QueryConfig::new(Duration::ZERO, Duration::ZERO);
        let client = Arc::new(QueryClient::with_config(config));

        // Populate and then deactivate cell A.
        {
            let (cell_a, subscription_a) = client.get_or_subscribe_cell::<i32>("key-a");
            let (_, committed, _) = cell_a.complete_success(0, 1, client.config());
            assert!(committed);
            drop(subscription_a); // A becomes inactive; inactive_since is set
        }
        assert_eq!(
            client.cells.len(),
            1,
            "cell A should exist before any auto-sweep"
        );

        // Stream cell B through a full fetch cycle.
        let query_b = Query::new(
            "key-b",
            || Box::pin(async { Ok::<i32, QueryError>(2) }),
            client.clone(),
        );

        let mut stream = query_b.stream();

        let loading = stream.next().await; // Initial: B inserted, fetch queued
        assert!(matches!(loading, Some(ref r) if r.is_loading()));
        assert_eq!(
            client.cells.len(),
            2,
            "A and B should both be in cells before fetch completes"
        );

        let success = stream.next().await; // Fetching: perform_fetch + auto-sweep
        assert!(matches!(success, Some(ref r) if r.is_success()));

        assert_eq!(
            client.cells.len(),
            1,
            "inactive cell A must be evicted by auto-sweep after key-b fetch, without explicit gc()"
        );
    }

    #[test]
    fn test_cell_subscription_guard_updates_lifecycle() {
        let cell = Arc::new(Cell::<i32>::new());

        assert_eq!(cell.subscriber_count(), 0);
        assert!(cell.inactive_since().is_some());

        {
            let _subscription = cell.subscribe();
            assert_eq!(cell.subscriber_count(), 1);
            assert!(cell.inactive_since().is_none());
        }

        assert_eq!(cell.subscriber_count(), 0);
        assert!(cell.inactive_since().is_some());
    }

    #[test]
    fn test_query_error_display() {
        let err = QueryError::FetchError("test error".to_owned());
        assert_eq!(err.to_string(), "Fetch failed: test error");

        let err = QueryError::NetworkError("network error".to_owned());
        assert_eq!(err.to_string(), "Network error: network error");
    }

    #[tokio::test]
    async fn test_invalidate_executes_synchronously() {
        let client = QueryClient::new();

        client.invalidate("test-key");
    }

    #[tokio::test]
    async fn test_cell_retains_fresh_data_across_subscriptions_without_refetch() {
        let config = QueryConfig::new(Duration::from_secs(3600), Duration::from_secs(3600));
        let client = Arc::new(QueryClient::with_config(config));
        let fetch_count = Arc::new(AtomicUsize::new(0));

        let fetch_count_clone = fetch_count.clone();
        let query = Query::new(
            "key",
            move || {
                let count = fetch_count_clone.clone();
                Box::pin(async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok::<i32, QueryError>(42)
                })
            },
            client.clone(),
        );

        {
            let mut first_stream = query.stream();
            let loading = first_stream.next().await;
            assert!(matches!(loading, Some(ref result) if result.is_loading()));
            let success = first_stream.next().await;
            assert!(
                matches!(success, Some(ref result) if result.is_success() && result.data() == Some(&42))
            );
        }

        assert_eq!(fetch_count.load(Ordering::SeqCst), 1);

        let mut second_stream = query.stream();
        let retained = second_stream.next().await;
        assert!(
            matches!(retained, Some(ref result) if result.is_success() && !result.is_stale() && result.data() == Some(&42)),
            "second subscription should observe retained cell data without refetching"
        );

        let duplicate = timeout(Duration::from_millis(25), second_stream.next()).await;
        assert!(
            duplicate.is_err(),
            "fresh retained cell data should not be re-emitted or refetched"
        );
        assert_eq!(
            fetch_count.load(Ordering::SeqCst),
            1,
            "retained cell data should satisfy the second subscription"
        );
    }

    #[tokio::test]
    async fn test_cell_retained_data_can_be_invalidated_before_next_subscribe() {
        let config = QueryConfig::new(Duration::from_secs(3600), Duration::from_secs(3600));
        let client = Arc::new(QueryClient::with_config(config));
        let fetch_count = Arc::new(AtomicUsize::new(0));

        let fetch_count_clone = fetch_count.clone();
        let query = Query::new(
            "key",
            move || {
                let count = fetch_count_clone.clone();
                Box::pin(async move {
                    let fetch_number = count.fetch_add(1, Ordering::SeqCst) + 1;
                    Ok::<i32, QueryError>(
                        i32::try_from(fetch_number).expect("test fetch count should fit in i32"),
                    )
                })
            },
            client.clone(),
        );

        {
            let mut first_stream = query.stream();
            let loading = first_stream.next().await;
            assert!(matches!(loading, Some(ref result) if result.is_loading()));
            let success = first_stream.next().await;
            assert!(
                matches!(success, Some(ref result) if result.is_success() && result.data() == Some(&1))
            );
        }

        assert_eq!(fetch_count.load(Ordering::SeqCst), 1);
        client.invalidate("key");

        let mut second_stream = query.stream();
        let stale = second_stream.next().await;
        assert!(
            matches!(stale, Some(ref result) if result.is_success() && result.is_stale() && result.is_fetching() && result.data() == Some(&1)),
            "inactive retained cell data should surface as stale and start a refetch after invalidate"
        );

        let fresh = second_stream.next().await;
        assert!(
            matches!(fresh, Some(ref result) if result.is_success() && !result.is_stale() && result.data() == Some(&2)),
            "invalidate-while-inactive should refetch on the next subscription"
        );
        assert_eq!(fetch_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_cell_invalidate_with_data_emits_stale_then_refetches() {
        let config = QueryConfig::new(Duration::from_secs(3600), Duration::from_secs(3600));
        let client = Arc::new(QueryClient::with_config(config));
        let fetch_count = Arc::new(AtomicUsize::new(0));

        let fetch_count_clone = fetch_count.clone();
        let query = Query::new(
            "key",
            move || {
                let count = fetch_count_clone.clone();
                Box::pin(async move {
                    let fetch_number = count.fetch_add(1, Ordering::SeqCst) + 1;
                    Ok::<i32, QueryError>(
                        i32::try_from(fetch_number).expect("test fetch count should fit in i32"),
                    )
                })
            },
            client.clone(),
        );

        let mut stream = query.stream();
        let loading = stream.next().await;
        assert!(matches!(loading, Some(ref result) if result.is_loading()));
        let success = stream.next().await;
        assert!(
            matches!(success, Some(ref result) if result.is_success() && result.data() == Some(&1))
        );
        client.invalidate("key");

        let stale = stream.next().await;
        assert!(
            matches!(stale, Some(ref result) if result.is_success() && result.is_stale() && result.is_fetching() && result.data() == Some(&1)),
            "data-bearing cell invalidation should keep stale data visible while refetching"
        );

        let fresh = stream.next().await;
        assert!(
            matches!(fresh, Some(ref result) if result.is_success() && !result.is_stale() && result.data() == Some(&2))
        );
        assert_eq!(fetch_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_cell_invalidate_without_data_emits_pending_fetching_not_stale() {
        let config = QueryConfig::new(Duration::from_secs(3600), Duration::from_secs(3600));
        let client = Arc::new(QueryClient::with_config(config));
        let fetch_count = Arc::new(AtomicUsize::new(0));
        let (mut releases, gates) = gate_fetches(2);

        let fetch_count_clone = fetch_count.clone();
        let gates_clone = gates.clone();
        let query = Query::new(
            "key",
            move || {
                let count = fetch_count_clone.clone();
                let gates = gates_clone.clone();
                Box::pin(async move {
                    let fetch_number = count.fetch_add(1, Ordering::SeqCst) + 1;
                    gates.next().await.expect("fetch gate should be released");
                    Ok::<i32, QueryError>(
                        i32::try_from(fetch_number).expect("test fetch count should fit in i32"),
                    )
                })
            },
            client.clone(),
        );

        let mut stream = query.stream();
        let loading = stream.next().await;
        assert!(matches!(loading, Some(ref result) if result.is_loading() && !result.is_stale()));

        let invalidated_poll = stream.next();
        tokio::pin!(invalidated_poll);
        assert_pending_until(
            &mut invalidated_poll,
            || fetch_count.load(Ordering::SeqCst) >= 1,
            "first fetch completed before its gate was released",
            "first fetch should start",
        )
        .await;

        client.invalidate("key");
        releases.release(0);

        let pending = timeout(Duration::from_millis(100), invalidated_poll)
            .await
            .expect("invalidate without data should keep the cell pending");
        assert!(
            matches!(pending, Some(ref result) if result.is_loading() && result.is_fetching() && !result.is_stale()),
            "data-less cell invalidation should emit Pending/Fetching, not stale"
        );

        let success_poll = stream.next();
        tokio::pin!(success_poll);
        assert_pending_until(
            &mut success_poll,
            || fetch_count.load(Ordering::SeqCst) >= 2,
            "second fetch completed before its gate was released",
            "second fetch should start",
        )
        .await;

        releases.release(1);
        let success = timeout(Duration::from_millis(100), success_poll)
            .await
            .expect("second fetch should complete");
        assert!(
            matches!(success, Some(ref result) if result.is_success() && !result.is_stale() && result.data() == Some(&2))
        );
        assert_eq!(fetch_count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn test_query_id_consistency() {
        let client = Arc::new(QueryClient::new());

        // Create two queries with the same key
        let query1 = Query::new(
            "user-123",
            || Box::pin(async { Ok::<i32, QueryError>(42) }),
            client.clone(),
        );
        let query2 = Query::new(
            "user-123",
            || Box::pin(async { Ok::<i32, QueryError>(42) }),
            client,
        );

        // Same key should produce the same ID
        assert_eq!(query1.key(), query2.key());
    }

    #[test]
    fn test_query_id_different_keys() {
        let client = Arc::new(QueryClient::new());

        // Create two queries with different keys
        let query1 = Query::new(
            "user-123",
            || Box::pin(async { Ok::<i32, QueryError>(42) }),
            client.clone(),
        );
        let query2 = Query::new(
            "user-456",
            || Box::pin(async { Ok::<i32, QueryError>(42) }),
            client,
        );

        // Different keys should produce different IDs
        assert_ne!(query1.key(), query2.key());
    }

    #[test]
    fn test_query_id_different_clients() {
        let client1 = Arc::new(QueryClient::new());
        let client2 = Arc::new(QueryClient::new());

        let query1 = Query::new(
            "user-123",
            || Box::pin(async { Ok::<i32, QueryError>(42) }),
            client1,
        );
        let query2 = Query::new(
            "user-123",
            || Box::pin(async { Ok::<i32, QueryError>(42) }),
            client2,
        );

        assert_ne!(
            query1.key(),
            query2.key(),
            "same-key queries on different QueryClient instances should be distinct subscriptions"
        );
    }

    #[test]
    fn test_query_id_cloned_client_consistency() {
        let client = Arc::new(QueryClient::new());
        let cloned_client = Arc::new((*client).clone());

        let query1 = Query::new(
            "user-123",
            || Box::pin(async { Ok::<i32, QueryError>(42) }),
            client,
        );
        let query2 = Query::new(
            "user-123",
            || Box::pin(async { Ok::<i32, QueryError>(42) }),
            cloned_client,
        );

        assert_eq!(
            query1.key(),
            query2.key(),
            "QueryClient::clone should preserve the client identity"
        );
    }

    #[test]
    fn test_query_id_same_key_different_type() {
        let client = Arc::new(QueryClient::new());

        // Create two queries with same key but different value types
        let query1 = Query::new(
            "data",
            || Box::pin(async { Ok::<i32, QueryError>(42) }),
            client.clone(),
        );
        let query2 = Query::new(
            "data",
            || Box::pin(async { Ok::<String, QueryError>("test".to_owned()) }),
            client,
        );

        // Same key but different types should produce different IDs
        // The source type includes the response type, so full IDs differ even
        // though these logical keys are equal.
        assert_ne!(
            crate::Subscription::new(query1).id,
            crate::Subscription::new(query2).id
        );
    }

    /// Same-key queries with different output types must use independent cells.
    #[tokio::test]
    async fn test_same_key_different_types_fetch_independently() {
        let client = Arc::new(QueryClient::new());
        let i32_fetches = Arc::new(AtomicUsize::new(0));
        let str_fetches = Arc::new(AtomicUsize::new(0));

        let i32_fetches_clone = i32_fetches.clone();
        let query_i32 = Query::new(
            "data",
            move || {
                let count = i32_fetches_clone.clone();
                Box::pin(async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok::<i32, QueryError>(1)
                })
            },
            client.clone(),
        );

        let str_fetches_clone = str_fetches.clone();
        let query_str = Query::new(
            "data",
            move || {
                let count = str_fetches_clone.clone();
                Box::pin(async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok::<String, QueryError>("one".to_owned())
                })
            },
            client.clone(),
        );

        let mut stream_i32 = query_i32.stream();
        let mut stream_str = query_str.stream();

        // Both streams start in Loading (neither typed cell has data).
        let _ = stream_i32.next().await; // Loading
        let _ = stream_i32.next().await; // Success (fetch completes, populates the i32 cell)

        let _ = stream_str.next().await; // Loading
        let _ = stream_str.next().await; // Success

        // Each type should have been fetched exactly once; the retained data
        // for one type must not satisfy the other type.
        assert_eq!(
            i32_fetches.load(Ordering::SeqCst),
            1,
            "i32 query should have fetched exactly once"
        );
        assert_eq!(
            str_fetches.load(Ordering::SeqCst),
            1,
            "String query should have fetched exactly once"
        );
    }

    #[tokio::test]
    async fn test_same_identity_streams_share_single_in_flight_fetch() {
        let client = Arc::new(QueryClient::new());
        let fetch_count = Arc::new(AtomicUsize::new(0));
        let (mut releases, gates) = gate_fetches(1);

        let fetch_count_clone = fetch_count.clone();
        let gates_clone = gates.clone();
        let query = Query::new(
            "key",
            move || {
                let count = fetch_count_clone.clone();
                let gates = gates_clone.clone();
                Box::pin(async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    gates.next().await.expect("fetch gate should be released");
                    Ok::<i32, QueryError>(7)
                })
            },
            client,
        );

        let mut first_stream = query.stream();
        let mut second_stream = query.stream();

        let first_loading = first_stream.next().await;
        assert!(matches!(first_loading, Some(ref result) if result.is_loading()));

        let first_success_poll = first_stream.next();
        tokio::pin!(first_success_poll);
        assert_pending_until(
            &mut first_success_poll,
            || fetch_count.load(Ordering::SeqCst) >= 1,
            "first fetch completed before its gate was released",
            "first fetch should start",
        )
        .await;

        let second_loading = second_stream.next().await;
        assert!(matches!(second_loading, Some(ref result) if result.is_loading()));
        assert_eq!(
            fetch_count.load(Ordering::SeqCst),
            1,
            "same identity streams must share the cell in-flight fetch"
        );

        releases.release(0);

        let first_success = timeout(Duration::from_millis(100), first_success_poll)
            .await
            .expect("winner stream should receive success");
        assert!(
            matches!(first_success, Some(ref result) if result.is_success() && result.data() == Some(&7))
        );

        let second_success = timeout(Duration::from_millis(100), second_stream.next())
            .await
            .expect("loser stream should receive the shared success");
        assert!(
            matches!(second_success, Some(ref result) if result.is_success() && result.data() == Some(&7))
        );

        assert_eq!(
            fetch_count.load(Ordering::SeqCst),
            1,
            "loser stream must not invoke the fetcher after the shared success"
        );
    }

    #[tokio::test]
    async fn test_invalidations_during_one_fetch_window_coalesce_to_one_refetch() {
        let client = Arc::new(QueryClient::new());
        let fetch_count = Arc::new(AtomicUsize::new(0));
        let (mut releases, gates) = gate_fetches(2);

        let fetch_count_clone = fetch_count.clone();
        let gates_clone = gates.clone();
        let query = Query::new(
            "key",
            move || {
                let count = fetch_count_clone.clone();
                let gates = gates_clone.clone();
                Box::pin(async move {
                    let fetch_number = count.fetch_add(1, Ordering::SeqCst) + 1;
                    gates.next().await.expect("fetch gate should be released");
                    Ok::<i32, QueryError>(
                        i32::try_from(fetch_number).expect("test fetch count should fit in i32"),
                    )
                })
            },
            client.clone(),
        );

        let mut stream = query.stream();

        let first = stream.next().await;
        assert!(matches!(first, Some(ref result) if result.is_loading()));

        let refetching_poll = stream.next();
        tokio::pin!(refetching_poll);
        assert_pending_until(
            &mut refetching_poll,
            || fetch_count.load(Ordering::SeqCst) >= 1,
            "first fetch completed before its gate was released",
            "first fetch should start",
        )
        .await;
        assert_eq!(fetch_count.load(Ordering::SeqCst), 1);

        client.invalidate("key");
        client.invalidate("key");
        client.invalidate("key");

        releases.release(0);

        let refetching = timeout(Duration::from_millis(100), refetching_poll)
            .await
            .expect("coalesced invalidations should start one refetch");
        assert!(
            matches!(refetching, Some(ref result) if result.is_loading() && result.is_fetching()),
            "stale in-flight completion should be discarded and current generation should refetch"
        );

        assert_eq!(
            fetch_count.load(Ordering::SeqCst),
            1,
            "the second fetch starts when the stream advances into Fetching"
        );

        let second_poll = stream.next();
        tokio::pin!(second_poll);
        assert_pending_until(
            &mut second_poll,
            || fetch_count.load(Ordering::SeqCst) >= 2,
            "second fetch completed before its gate was released",
            "second fetch should start",
        )
        .await;
        assert_eq!(
            fetch_count.load(Ordering::SeqCst),
            2,
            "multiple invalidations in one in-flight window should coalesce to one additional fetch"
        );

        releases.release(1);
        let success = timeout(Duration::from_millis(100), second_poll)
            .await
            .expect("second fetch should complete")
            .expect("stream should produce success");
        assert!(
            success.is_success() && success.data() == Some(&2),
            "refetch should commit the current generation"
        );

        let duplicate = timeout(Duration::from_millis(25), stream.next()).await;
        assert!(
            duplicate.is_err(),
            "coalesced invalidations should not leave another fetch pending"
        );
        assert_eq!(fetch_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_fetch_error_does_not_retry_until_invalidated() {
        let client = Arc::new(QueryClient::new());
        let fetch_count = Arc::new(AtomicUsize::new(0));

        let fetch_count_clone = fetch_count.clone();
        let query = Query::new(
            "key",
            move || {
                let count = fetch_count_clone.clone();
                Box::pin(async move {
                    let fetch_number = count.fetch_add(1, Ordering::SeqCst) + 1;
                    if fetch_number == 1 {
                        Err::<i32, QueryError>(QueryError::FetchError("boom".to_owned()))
                    } else {
                        Ok::<i32, QueryError>(42)
                    }
                })
            },
            client.clone(),
        );

        let mut stream = query.stream();

        let loading = stream.next().await;
        assert!(matches!(loading, Some(ref result) if result.is_loading()));

        let error = stream.next().await;
        assert!(matches!(error, Some(ref result) if result.is_error()));
        assert_eq!(fetch_count.load(Ordering::SeqCst), 1);

        let retry = timeout(Duration::from_millis(25), stream.next()).await;
        assert!(
            retry.is_err(),
            "same-generation error must not trigger a tight retry loop"
        );
        assert_eq!(fetch_count.load(Ordering::SeqCst), 1);

        client.invalidate("key");

        let refetching = timeout(Duration::from_millis(100), stream.next())
            .await
            .expect("invalidate should restart fetch after error");
        assert!(
            matches!(refetching, Some(ref result) if result.is_loading() && result.is_fetching())
        );

        let success = timeout(Duration::from_millis(100), stream.next())
            .await
            .expect("retry fetch should complete");
        assert!(
            matches!(success, Some(ref result) if result.is_success() && result.data() == Some(&42))
        );
        assert_eq!(fetch_count.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_zero_stale_time_success_does_not_watch_refetch_loop() {
        let config = QueryConfig::new(Duration::ZERO, Duration::from_secs(300));
        let client = Arc::new(QueryClient::with_config(config));
        let fetch_count = Arc::new(AtomicUsize::new(0));

        let fetch_count_clone = fetch_count.clone();
        let query = Query::new(
            "key",
            move || {
                let count = fetch_count_clone.clone();
                Box::pin(async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok::<i32, QueryError>(1)
                })
            },
            client,
        );

        let mut stream = query.stream();

        let loading = stream.next().await;
        assert!(matches!(loading, Some(ref result) if result.is_loading()));

        let success = stream.next().await;
        assert!(matches!(success, Some(ref result) if result.is_success()));

        let next = timeout(Duration::from_millis(25), stream.next()).await;
        assert!(
            next.is_err(),
            "WatchChanged must not turn time-stale data into an immediate refetch loop"
        );
        assert_eq!(fetch_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_loser_observes_shared_fetch_error_without_retrying() {
        let client = Arc::new(QueryClient::new());
        let fetch_count = Arc::new(AtomicUsize::new(0));
        let (mut releases, gates) = gate_fetches(1);

        let fetch_count_clone = fetch_count.clone();
        let gates_clone = gates.clone();
        let query = Query::new(
            "key",
            move || {
                let count = fetch_count_clone.clone();
                let gates = gates_clone.clone();
                Box::pin(async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    gates.next().await.expect("fetch gate should be released");
                    Err::<i32, QueryError>(QueryError::FetchError("boom".to_owned()))
                })
            },
            client,
        );

        let mut first_stream = query.stream();
        let mut second_stream = query.stream();

        let first_loading = first_stream.next().await;
        assert!(matches!(first_loading, Some(ref result) if result.is_loading()));

        let first_error_poll = first_stream.next();
        tokio::pin!(first_error_poll);
        assert_pending_until(
            &mut first_error_poll,
            || fetch_count.load(Ordering::SeqCst) >= 1,
            "first fetch completed before its gate was released",
            "first fetch should start",
        )
        .await;

        let second_loading = second_stream.next().await;
        assert!(matches!(second_loading, Some(ref result) if result.is_loading()));
        assert_eq!(fetch_count.load(Ordering::SeqCst), 1);

        releases.release(0);

        let first_error = timeout(Duration::from_millis(100), first_error_poll)
            .await
            .expect("winner stream should receive error");
        assert!(matches!(first_error, Some(ref result) if result.is_error()));

        let second_error = timeout(Duration::from_millis(100), second_stream.next())
            .await
            .expect("loser stream should receive shared error");
        assert!(matches!(second_error, Some(ref result) if result.is_error()));

        let first_retry = timeout(Duration::from_millis(25), first_stream.next()).await;
        let second_retry = timeout(Duration::from_millis(25), second_stream.next()).await;
        assert!(
            first_retry.is_err() && second_retry.is_err(),
            "shared same-generation error must not cause either stream to retry"
        );
        assert_eq!(fetch_count.load(Ordering::SeqCst), 1);
    }

    // ---------------------------------------------------------------------------
    // Regression tests for the "missed invalidation during fetch" race.
    //
    // Before the fix, `perform_fetch` subscribed to the invalidation channel
    // *after* `fetcher().await` returned, creating a window where any
    // invalidation while the fetch was in-flight was lost. The cell generation
    // now records the invalidation and causes a follow-up fetch.
    // ---------------------------------------------------------------------------

    /// An invalidation while a fetch is in progress must trigger
    /// another refetch once the fetch completes.
    ///
    /// With the old code the invalidation was lost (the receiver was created
    /// *after* the fetch returned), so the subscription would stall in
    /// `State::Watching` until the next explicit invalidation.
    #[tokio::test]
    async fn test_invalidation_during_fetch_triggers_refetch() {
        let client = Arc::new(QueryClient::new());
        let fetch_count = Arc::new(AtomicUsize::new(0));
        let (mut releases, gates) = gate_fetches(2);

        let fetch_count_clone = fetch_count.clone();
        let gates_clone = gates.clone();
        let query = Query::new(
            "key",
            move || {
                let count = fetch_count_clone.clone();
                let gates = gates_clone.clone();
                Box::pin(async move {
                    let fetch_number = count.fetch_add(1, Ordering::SeqCst) + 1;
                    gates.next().await.expect("fetch gate should be released");
                    Ok::<i32, QueryError>(
                        i32::try_from(fetch_number).expect("test fetch count should fit in i32"),
                    )
                })
            },
            client.clone(),
        );

        let mut stream = query.stream();

        // Cold start: Loading
        let first = stream.next().await;
        assert!(matches!(first, Some(ref r) if r.is_loading()));

        // Gate the fetcher instead of sleeping so CI timing cannot miss the in-flight window.
        let refetching_poll = stream.next();
        tokio::pin!(refetching_poll);
        assert_pending_until(
            &mut refetching_poll,
            || fetch_count.load(Ordering::SeqCst) >= 1,
            "first fetch completed before its gate was released",
            "first fetch should start",
        )
        .await;

        // Inject an invalidation while the first fetch is still gated.
        client.invalidate("key");

        releases.release(0);

        // The first fetch completes after it has already been invalidated, so
        // the stale completion is discarded and the stream immediately starts
        // fetching the next generation.
        let second = timeout(Duration::from_millis(100), refetching_poll)
            .await
            .expect("invalidated in-flight fetch should start a subsequent refetch");
        assert!(
            matches!(second, Some(ref r) if r.is_loading() && r.is_fetching()),
            "invalidated in-flight fetch should start a subsequent refetch"
        );

        // The invalidation that arrived during the first fetch must trigger a
        // second fetch that produces the visible success.
        let success_poll = stream.next();
        tokio::pin!(success_poll);
        assert_pending_until(
            &mut success_poll,
            || fetch_count.load(Ordering::SeqCst) >= 2,
            "second fetch completed before its gate was released",
            "second fetch should start",
        )
        .await;

        releases.release(1);
        let third = timeout(Duration::from_millis(100), success_poll)
            .await
            .expect("second fetch should complete");
        assert!(
            matches!(third, Some(ref r) if r.is_success() && r.data() == Some(&2)),
            "second fetch should succeed"
        );

        assert_eq!(
            fetch_count.load(Ordering::SeqCst),
            2,
            "exactly two fetches should have been performed"
        );
    }

    #[tokio::test]
    async fn test_cold_fetch_success_is_not_emitted_twice() {
        let client = Arc::new(QueryClient::new());
        let query = Query::new(
            "key",
            || Box::pin(async { Ok::<i32, QueryError>(1) }),
            client,
        );

        let mut stream = query.stream();

        let loading = stream.next().await;
        assert!(matches!(loading, Some(ref result) if result.is_loading()));

        let success = stream.next().await;
        assert!(matches!(success, Some(ref result) if result.is_success()));

        let duplicate = timeout(Duration::from_millis(25), stream.next()).await;
        assert!(
            duplicate.is_err(),
            "watch receiver must not re-emit the success snapshot already returned by perform_fetch"
        );
    }

    #[tokio::test]
    async fn test_watching_survives_many_invalidations() {
        // A query that is watching its cell must not terminate when many
        // invalidations happen before the stream is polled again. The watch
        // receiver may coalesce snapshots, but it must still reconcile the
        // latest generation and refetch.
        let client = Arc::new(QueryClient::new());
        let query = Query::new(
            "key",
            || Box::pin(async { Ok::<i32, QueryError>(1) }),
            client.clone(),
        );

        let mut stream = query.stream();

        // Initial Loading, then Success (the watcher's receiver is now active).
        let loading = stream.next().await;
        assert!(matches!(loading, Some(ref r) if r.is_loading()));
        let success = stream.next().await;
        assert!(matches!(success, Some(ref r) if r.is_success()));

        // Trigger many matching invalidations without polling the stream.
        for _ in 0..150 {
            client.invalidate("key");
        }

        // The subscription must survive the coalesced watch updates and
        // refetch rather than ending.
        let next = stream.next().await;
        assert!(
            matches!(next, Some(ref r) if r.is_success() && r.is_stale() && r.is_fetching()),
            "coalesced invalidations should trigger a refetch, not end the subscription"
        );
    }
}
