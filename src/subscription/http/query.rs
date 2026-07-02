//! HTTP query operations with caching and automatic refetching.
//!
//! This module provides the [`Query`] subscription and [`QueryClient`] for managing
//! HTTP GET requests with built-in caching, similar to SWR or TanStack Query.
//!
//! # Design Pattern: Subscription-based State Management
//!
//! Unlike traditional HTTP clients, queries are **subscriptions** that continuously
//! monitor and manage cached data. When you subscribe to a query:
//!
//! 1. If cached data exists, it's immediately emitted
//! 2. If data is stale or missing, a fetch is automatically triggered
//! 3. When the cache is invalidated, refetching happens automatically
//!
//! Failed fetches are emitted as [`QueryState::Error`] and are not retried
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
//!     user_state: QueryState<User>,
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
//!                 self.user_state = result.state;
//!                 Command::none()
//!             }
//!             Message::RefreshUser => {
//!                 self.query_client.invalidate("user-123")
//!             }
//!         }
//!     }
//! }
//! ```

use std::any::TypeId;
use std::fmt;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use dashmap::DashMap;
use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::{self, BoxStream};
use thiserror::Error;
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;

use crate::Command;
use crate::subscription::{SubscriptionId, SubscriptionSource};

use super::cache::{AnyCacheEntry, CacheEntry};
use super::config::QueryConfig;

static NEXT_QUERY_CLIENT_ID: AtomicU64 = AtomicU64::new(1);

/// Error type for query operations.
#[derive(Error, Debug, Clone)]
pub enum QueryError {
    #[error("Fetch failed: {0}")]
    FetchError(String),

    #[error("Network error: {0}")]
    NetworkError(String),
}

/// The state of a query result.
#[derive(Debug, Clone)]
pub enum QueryState<T> {
    /// Query is loading (fetching data).
    Loading,
    /// Query succeeded with data.
    Success {
        /// The data returned by the query.
        data: T,
        /// Whether the data is stale and should be refetched.
        is_stale: bool,
    },
    /// Query failed with an error.
    Error(String),
}

/// A query result containing the current state.
#[derive(Debug, Clone)]
pub struct QueryResult<T> {
    /// The current state of the query.
    pub state: QueryState<T>,
}

impl<T> QueryResult<T> {
    /// Returns the data if the query succeeded, otherwise `None`.
    pub const fn data(&self) -> Option<&T> {
        match &self.state {
            QueryState::Success { data, .. } => Some(data),
            _ => None,
        }
    }

    /// Returns `true` if the query is currently loading.
    pub const fn is_loading(&self) -> bool {
        matches!(self.state, QueryState::Loading)
    }

    /// Returns `true` if the query succeeded.
    pub const fn is_success(&self) -> bool {
        matches!(self.state, QueryState::Success { .. })
    }

    /// Returns `true` if the query failed.
    pub const fn is_error(&self) -> bool {
        matches!(self.state, QueryState::Error(_))
    }

    /// Returns `true` if the query data is stale.
    pub const fn is_stale(&self) -> bool {
        matches!(self.state, QueryState::Success { is_stale: true, .. })
    }
}

/// A client for managing query cache and invalidation.
///
/// The `QueryClient` is the central state manager for queries. It handles:
/// - Caching query results
/// - Broadcasting invalidation notifications
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
    // Keyed by (TypeId, user-provided key) so that Query<i32> and Query<String>
    // sharing the same string key occupy independent cache slots and do not
    // overwrite each other.
    cache: Arc<DashMap<(TypeId, String), Box<dyn AnyCacheEntry>>>,
    invalidation_tx: broadcast::Sender<String>,
    config: QueryConfig,
}

impl fmt::Debug for QueryClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The cached values are type-erased and may not be `Debug`, so only
        // expose the number of cached keys rather than their contents.
        f.debug_struct("QueryClient")
            .field("client_id", &self.client_id)
            .field("cached_keys", &self.cache.len())
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl QueryClient {
    /// Creates a new query client with default configuration.
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(QueryConfig::default())
    }

    /// Creates a new query client with the given configuration.
    #[must_use]
    pub fn with_config(config: QueryConfig) -> Self {
        let (invalidation_tx, _) = broadcast::channel(100);
        Self {
            client_id: NEXT_QUERY_CLIENT_ID.fetch_add(1, Ordering::Relaxed),
            cache: Arc::new(DashMap::new()),
            invalidation_tx,
            config,
        }
    }

    /// Invalidates the cache for the given key, triggering refetch in active queries.
    ///
    /// This returns a `Command` that performs the invalidation as a side effect.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// fn update(&mut self, msg: Message) -> Command<Message> {
    ///     match msg {
    ///         Message::UserUpdated => {
    ///             self.query_client.invalidate("user-123")
    ///         }
    ///     }
    /// }
    /// ```
    pub fn invalidate<Msg>(&self, key: &impl ToString) -> Command<Msg>
    where
        Msg: Send + 'static,
    {
        let cache = self.cache.clone();
        let tx = self.invalidation_tx.clone();
        let key_string = key.to_string();

        // Use a stream that completes without producing any messages
        Command {
            stream: Some(
                futures::stream::once(async move {
                    // Mark all typed cache entries for this user key as stale so
                    // invalidations are preserved even when no query is active.
                    for mut entry in cache.iter_mut() {
                        if entry.key().1.as_str() == key_string.as_str() {
                            entry.value_mut().mark_stale();
                        }
                    }

                    // Broadcast invalidation notification
                    let _ = tx.send(key_string);
                })
                .filter_map(|()| async { None })
                .boxed(),
            ),
        }
    }

    /// Subscribes to invalidation notifications for the given key.
    fn subscribe_invalidation(&self) -> broadcast::Receiver<String> {
        self.invalidation_tx.subscribe()
    }

    /// Gets a cached entry for the given key.
    fn get_cache<T: Clone + Send + Sync + 'static>(&self, key: &str) -> Option<CacheEntry<T>> {
        let typed_key = (TypeId::of::<T>(), key.to_string());
        self.cache
            .get(&typed_key)
            .and_then(|entry| entry.as_any().downcast_ref::<CacheEntry<T>>().cloned())
    }

    /// Sets a cached entry for the given key.
    ///
    /// Performs a garbage-collection sweep before inserting so that entries
    /// older than `cache_time` are removed and the cache does not grow without
    /// bound as new keys are fetched over time.
    fn set_cache<T: Clone + Send + Sync + 'static>(&self, key: String, entry: CacheEntry<T>) {
        self.gc_expired();
        self.cache.insert((TypeId::of::<T>(), key), Box::new(entry));
    }

    /// Removes cached entries that have outlived `cache_time`.
    ///
    /// This is called automatically on every cache insertion. Applications can
    /// also call [`QueryClient::gc`] to trigger a sweep explicitly (for example
    /// when no further fetches are expected for a while).
    fn gc_expired(&self) {
        let cache_time = self.config.cache_time;
        self.cache.retain(|_, entry| !entry.is_expired(cache_time));
    }

    /// Removes cached entries that have outlived the configured `cache_time`.
    ///
    /// Cache entries are also garbage collected automatically whenever a new
    /// entry is inserted. Use this method to reclaim memory for keys that are
    /// no longer being fetched (which would otherwise wait for the next
    /// insertion to be swept).
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
}

impl Default for QueryClient {
    fn default() -> Self {
        Self::new()
    }
}

/// A query subscription that monitors and fetches data with caching.
///
/// `Query` is a subscription that automatically manages data fetching and caching.
/// When subscribed:
///
/// 1. If cached data exists, it's immediately emitted as `Success`
/// 2. If data is missing or stale, a fetch is triggered and `Loading` is emitted
/// 3. When invalidated, the query automatically refetches
///
/// If a fetch fails, the query emits [`QueryState::Error`] and waits for the
/// next invalidation. It does not retry automatically; call
/// [`QueryClient::invalidate`] to request another fetch, or implement retry
/// behavior inside the fetcher.
///
/// # Query keys
///
/// The query key identifies both the cache entry and the running subscription.
/// Include every request parameter used by the fetcher in the key. If the
/// fetcher captures values such as a user ID, search term, page number, or base
/// URL but the key does not change, the runtime may keep the existing
/// subscription and continue using the old fetcher.
///
/// # Example
///
/// ```rust,ignore
/// use tears::subscription::{Subscription, http::{Query, QueryClient}};
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
/// A shared, reusable async fetcher producing a query's value.
type Fetcher<V> = Arc<dyn Fn() -> BoxFuture<'static, Result<V, QueryError>> + Send + Sync>;

pub struct Query<V> {
    key: String,
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
    /// * `key` - A unique identifier for this query (used for caching)
    /// * `fetcher` - An async function that fetches the data
    /// * `client` - The query client for cache management
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
    pub fn new<F>(key: &impl ToString, fetcher: F, client: Arc<QueryClient>) -> Self
    where
        F: Fn() -> BoxFuture<'static, Result<V, QueryError>> + Send + Sync + 'static,
    {
        Self {
            key: key.to_string(),
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

    fn stream(&self) -> BoxStream<'static, Self::Output> {
        let key = self.key.clone();
        let fetcher = self.fetcher.clone();
        let client = self.client.clone();

        stream::unfold(State::Initial, move |state| {
            let key = key.clone();
            let fetcher = fetcher.clone();
            let client = client.clone();

            async move {
                match state {
                    State::Initial => {
                        // Subscribe to invalidation notifications BEFORE checking the
                        // cache so that any invalidation that arrives between the cache
                        // read and when we first enter `State::Watching` is captured.
                        // The same receiver is threaded through `State::Fetching` so
                        // that invalidations sent while a fetch is in-flight are also
                        // buffered and visible as soon as the fetch completes.
                        let rx = client.subscribe_invalidation();

                        // Check cache first
                        if let Some(cached) = client.get_cache::<V>(&key) {
                            let is_stale = cached.check_staleness(client.config().stale_time);

                            let result = QueryResult {
                                state: QueryState::Success {
                                    data: cached.data,
                                    is_stale,
                                },
                            };

                            if is_stale {
                                // Stale data: emit it, then refetch.
                                // Pass rx so that invalidations during the refetch are captured.
                                Some((result, State::Fetching { rx }))
                            } else {
                                // Fresh data: emit it, then wait for invalidation.
                                Some((result, State::Watching { rx }))
                            }
                        } else {
                            // No cache: emit Loading, then fetch.
                            let result = QueryResult {
                                state: QueryState::Loading,
                            };
                            Some((result, State::Fetching { rx }))
                        }
                    }

                    // Fetch (cold start or after stale/invalidation). `rx` was
                    // subscribed before the fetch started so any invalidation that
                    // arrives during the fetch is already buffered in `rx`.
                    State::Fetching { rx } => {
                        Some(perform_fetch(&fetcher, &client, &key, rx).await)
                    }

                    State::Watching { rx } => watch_for_invalidation::<V>(rx, &key).await,
                }
            }
        })
        .boxed()
    }

    fn id(&self) -> SubscriptionId {
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        SubscriptionId::of::<Self>(hasher.finish())
    }
}

impl<V> Hash for Query<V> {
    fn hash<H>(&self, hasher: &mut H)
    where
        H: std::hash::Hasher,
    {
        self.client.client_id.hash(hasher);
        self.key.hash(hasher);
    }
}

/// Internal state machine for the Query subscription.
enum State {
    Initial,
    /// Fetching data; `rx` was subscribed *before* the fetch started so that
    /// invalidations arriving during the fetch are captured and not missed.
    Fetching {
        rx: broadcast::Receiver<String>,
    },
    Watching {
        rx: broadcast::Receiver<String>,
    },
}

/// Runs the fetcher, caches a successful result, and transitions to watching
/// for invalidations using the pre-subscribed receiver.
///
/// `rx` **must** have been subscribed before the fetch was initiated so that
/// invalidations broadcast during the fetch are buffered in `rx` and visible
/// immediately when the subscription enters `State::Watching`.
async fn perform_fetch<V>(
    fetcher: &Fetcher<V>,
    client: &QueryClient,
    key: &str,
    rx: broadcast::Receiver<String>,
) -> (QueryResult<V>, State)
where
    V: Clone + Send + Sync + 'static,
{
    let result = match fetcher().await {
        Ok(data) => {
            // Cache the result for future subscribers and refetches.
            client.set_cache(key.to_string(), CacheEntry::new(data.clone()));
            QueryResult {
                state: QueryState::Success {
                    data,
                    is_stale: false,
                },
            }
        }
        Err(e) => QueryResult {
            state: QueryState::Error(e.to_string()),
        },
    };

    // Re-use the pre-subscribed receiver rather than creating a new one so
    // that invalidations that arrived during the fetch are already buffered
    // and will be seen immediately upon entering State::Watching.
    (result, State::Watching { rx })
}

/// Waits for an invalidation notification and decides the next step.
///
/// Returns `Some((Loading, Fetching { rx }))` when this key should refetch
/// (either it was explicitly invalidated, or the receiver lagged and may have
/// missed its invalidation).  The receiver is threaded into `State::Fetching`
/// so that invalidations arriving during the subsequent fetch are not missed.
/// Returns `None` only when the channel is closed.
async fn watch_for_invalidation<V>(
    mut rx: broadcast::Receiver<String>,
    key: &str,
) -> Option<(QueryResult<V>, State)> {
    loop {
        match rx.recv().await {
            // Our key was invalidated: refetch.
            Ok(invalidated_key) if invalidated_key == key => {
                return Some((
                    QueryResult {
                        state: QueryState::Loading,
                    },
                    State::Fetching { rx },
                ));
            }
            // A different key was invalidated: keep waiting.
            Ok(_) => {}
            // The receiver fell behind and some invalidations were dropped.
            // One of them may have been for our key, so refetch to be safe
            // instead of risking stale data.
            Err(RecvError::Lagged(_)) => {
                return Some((
                    QueryResult {
                        state: QueryState::Loading,
                    },
                    State::Fetching { rx },
                ));
            }
            // Channel closed (QueryClient dropped): the subscription ends.
            Err(RecvError::Closed) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_query_result_data() {
        let result = QueryResult {
            state: QueryState::Success {
                data: 42,
                is_stale: false,
            },
        };
        assert_eq!(result.data(), Some(&42));

        let result: QueryResult<i32> = QueryResult {
            state: QueryState::Loading,
        };
        assert_eq!(result.data(), None);

        let result: QueryResult<i32> = QueryResult {
            state: QueryState::Error("error".to_string()),
        };
        assert_eq!(result.data(), None);
    }

    #[test]
    fn test_query_result_predicates() {
        let loading: QueryResult<i32> = QueryResult {
            state: QueryState::Loading,
        };
        assert!(loading.is_loading());
        assert!(!loading.is_success());
        assert!(!loading.is_error());
        assert!(!loading.is_stale());

        let success = QueryResult {
            state: QueryState::Success {
                data: 42,
                is_stale: false,
            },
        };
        assert!(!success.is_loading());
        assert!(success.is_success());
        assert!(!success.is_error());
        assert!(!success.is_stale());

        let stale = QueryResult {
            state: QueryState::Success {
                data: 42,
                is_stale: true,
            },
        };
        assert!(!stale.is_loading());
        assert!(stale.is_success());
        assert!(!stale.is_error());
        assert!(stale.is_stale());

        let error: QueryResult<i32> = QueryResult {
            state: QueryState::Error("error".to_string()),
        };
        assert!(!error.is_loading());
        assert!(!error.is_success());
        assert!(error.is_error());
        assert!(!error.is_stale());
    }

    #[test]
    fn test_query_client_new() {
        let client = QueryClient::new();
        assert_eq!(client.cache.len(), 0);
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
    fn test_query_client_cache_operations() {
        let client = QueryClient::new();

        // Initially no cache
        assert!(client.get_cache::<i32>("key1").is_none());

        // Set cache
        let entry = CacheEntry::new(42);
        client.set_cache("key1".to_string(), entry);

        // Get cache
        let cached = client.get_cache::<i32>("key1");
        assert!(cached.is_some());
        if let Some(entry) = cached {
            assert_eq!(entry.data, 42);
        }
    }

    #[test]
    fn test_query_error_display() {
        let err = QueryError::FetchError("test error".to_string());
        assert_eq!(err.to_string(), "Fetch failed: test error");

        let err = QueryError::NetworkError("network error".to_string());
        assert_eq!(err.to_string(), "Network error: network error");
    }

    #[tokio::test]
    async fn test_invalidate_command_execution() {
        use futures::StreamExt;

        let client = QueryClient::new();

        // Create invalidate command
        let cmd: Command<()> = client.invalidate(&"test-key");

        // Execute the command - should complete without producing messages
        let stream = cmd
            .stream
            .expect("invalidate should produce a command with a stream");

        // Collect all actions (should be empty since invalidate produces no messages)
        let actions: Vec<_> = stream.collect().await;
        assert!(
            actions.is_empty(),
            "invalidate should not produce any messages"
        );
    }

    #[tokio::test]
    async fn test_invalidate_broadcasts_notification() {
        use futures::StreamExt;

        let client = QueryClient::new();

        // Subscribe to invalidation notifications before invalidating
        let mut rx = client.subscribe_invalidation();

        // Create and execute invalidate command
        let cmd: Command<()> = client.invalidate(&"test-key");

        if let Some(stream) = cmd.stream {
            // Execute the command
            let _: Vec<_> = stream.collect().await;
        }

        // Check that notification was broadcast
        let result = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await;
        let key = result
            .expect("Should receive notification within timeout")
            .expect("Channel should not be closed");
        assert_eq!(key, "test-key");
    }

    #[tokio::test]
    async fn test_invalidate_nonexistent_key() {
        use futures::StreamExt;

        let client = QueryClient::new();

        // Invalidate a key that doesn't exist in cache
        let cmd: Command<()> = client.invalidate(&"nonexistent");

        if let Some(stream) = cmd.stream {
            let actions: Vec<_> = stream.collect().await;
            assert!(actions.is_empty());
        }

        // Should still broadcast notification even if cache entry doesn't exist
        let mut rx = client.subscribe_invalidation();
        let cmd: Command<()> = client.invalidate(&"nonexistent");

        if let Some(stream) = cmd.stream {
            let _: Vec<_> = stream.collect().await;
        }

        let result = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await;
        let key = result
            .expect("Should receive notification within timeout")
            .expect("Channel should not be closed");
        assert_eq!(key, "nonexistent");
    }

    #[tokio::test]
    async fn test_invalidate_marks_matching_cache_entries_stale() {
        use futures::StreamExt;

        let config = QueryConfig::new(Duration::from_secs(3600), Duration::from_secs(3600));
        let client = QueryClient::with_config(config);

        client.set_cache("data".to_string(), CacheEntry::new(42i32));
        client.set_cache("data".to_string(), CacheEntry::new("hello".to_string()));
        client.set_cache("other".to_string(), CacheEntry::new(7i32));

        let cmd: Command<()> = client.invalidate(&"data");
        if let Some(stream) = cmd.stream {
            let _: Vec<_> = stream.collect().await;
        }

        assert!(
            client
                .get_cache::<i32>("data")
                .expect("i32 data cache should exist")
                .is_stale,
            "invalidate should mark matching i32 cache entries stale"
        );
        assert!(
            client
                .get_cache::<String>("data")
                .expect("String data cache should exist")
                .is_stale,
            "invalidate should mark matching String cache entries stale"
        );
        assert!(
            !client
                .get_cache::<i32>("other")
                .expect("other cache should exist")
                .is_stale,
            "invalidate should not mark unrelated keys stale"
        );
    }

    #[tokio::test]
    async fn test_invalidate_without_active_subscription_refetches_on_next_subscribe() {
        use futures::StreamExt;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let config = QueryConfig::new(Duration::from_secs(3600), Duration::from_secs(3600));
        let client = Arc::new(QueryClient::with_config(config));
        client.set_cache("key".to_string(), CacheEntry::new(42));

        let cmd: Command<()> = client.invalidate(&"key");
        if let Some(stream) = cmd.stream {
            let _: Vec<_> = stream.collect().await;
        }

        let fetch_count = Arc::new(AtomicUsize::new(0));
        let fetch_count_clone = fetch_count.clone();
        let query = Query::new(
            &"key",
            move || {
                let count = fetch_count_clone.clone();
                Box::pin(async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok::<i32, QueryError>(99)
                })
            },
            client,
        );

        let mut stream = query.stream();

        let cached = stream.next().await;
        assert!(
            matches!(
                cached,
                Some(QueryResult {
                    state: QueryState::Success {
                        data: 42,
                        is_stale: true
                    }
                })
            ),
            "invalidated cache should be emitted as stale even with long stale_time"
        );

        let fetched = stream.next().await;
        assert!(
            matches!(
                fetched,
                Some(QueryResult {
                    state: QueryState::Success {
                        data: 99,
                        is_stale: false
                    }
                })
            ),
            "stale invalidated cache should trigger a refetch"
        );
        assert_eq!(fetch_count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_query_id_consistency() {
        let client = Arc::new(QueryClient::new());

        // Create two queries with the same key
        let query1 = Query::new(
            &"user-123",
            || Box::pin(async { Ok::<i32, QueryError>(42) }),
            client.clone(),
        );
        let query2 = Query::new(
            &"user-123",
            || Box::pin(async { Ok::<i32, QueryError>(42) }),
            client,
        );

        // Same key should produce the same ID
        assert_eq!(query1.id(), query2.id());
    }

    #[test]
    fn test_query_id_different_keys() {
        let client = Arc::new(QueryClient::new());

        // Create two queries with different keys
        let query1 = Query::new(
            &"user-123",
            || Box::pin(async { Ok::<i32, QueryError>(42) }),
            client.clone(),
        );
        let query2 = Query::new(
            &"user-456",
            || Box::pin(async { Ok::<i32, QueryError>(42) }),
            client,
        );

        // Different keys should produce different IDs
        assert_ne!(query1.id(), query2.id());
    }

    #[test]
    fn test_query_id_different_clients() {
        let client1 = Arc::new(QueryClient::new());
        let client2 = Arc::new(QueryClient::new());

        let query1 = Query::new(
            &"user-123",
            || Box::pin(async { Ok::<i32, QueryError>(42) }),
            client1,
        );
        let query2 = Query::new(
            &"user-123",
            || Box::pin(async { Ok::<i32, QueryError>(42) }),
            client2,
        );

        assert_ne!(
            query1.id(),
            query2.id(),
            "same-key queries on different QueryClient instances should be distinct subscriptions"
        );
    }

    #[test]
    fn test_query_id_cloned_client_consistency() {
        let client = Arc::new(QueryClient::new());
        let cloned_client = Arc::new((*client).clone());

        let query1 = Query::new(
            &"user-123",
            || Box::pin(async { Ok::<i32, QueryError>(42) }),
            client,
        );
        let query2 = Query::new(
            &"user-123",
            || Box::pin(async { Ok::<i32, QueryError>(42) }),
            cloned_client,
        );

        assert_eq!(
            query1.id(),
            query2.id(),
            "QueryClient::clone should preserve the client identity"
        );
    }

    #[test]
    fn test_query_id_same_key_different_type() {
        let client = Arc::new(QueryClient::new());

        // Create two queries with same key but different value types
        let query1 = Query::new(
            &"data",
            || Box::pin(async { Ok::<i32, QueryError>(42) }),
            client.clone(),
        );
        let query2 = Query::new(
            &"data",
            || Box::pin(async { Ok::<String, QueryError>("test".to_string()) }),
            client,
        );

        // Same key but different types should produce different IDs
        // because SubscriptionId::of::<Self> includes the type information
        assert_ne!(query1.id(), query2.id());
    }

    #[test]
    fn test_set_cache_evicts_expired_entries() {
        use std::thread::sleep;

        // Short cache_time so entries expire quickly.
        let config = QueryConfig::new(Duration::from_secs(0), Duration::from_millis(10));
        let client = QueryClient::with_config(config);

        client.set_cache("old".to_string(), CacheEntry::new(1));
        assert!(client.get_cache::<i32>("old").is_some());

        // Let the entry exceed cache_time.
        sleep(Duration::from_millis(20));

        // Inserting another entry must trigger a GC sweep that removes the
        // expired one, so the cache does not grow without bound.
        client.set_cache("new".to_string(), CacheEntry::new(2));

        assert!(
            client.get_cache::<i32>("old").is_none(),
            "expired entry should be evicted on insert"
        );
        assert!(
            client.get_cache::<i32>("new").is_some(),
            "fresh entry should remain"
        );
    }

    #[test]
    fn test_gc_removes_expired_entries() {
        use std::thread::sleep;

        let config = QueryConfig::new(Duration::from_secs(0), Duration::from_millis(10));
        let client = QueryClient::with_config(config);

        client.set_cache("a".to_string(), CacheEntry::new(1));
        assert_eq!(client.cache.len(), 1);

        sleep(Duration::from_millis(20));

        client.gc();

        assert!(client.get_cache::<i32>("a").is_none());
        assert_eq!(client.cache.len(), 0);
    }

    #[test]
    fn test_gc_keeps_fresh_entries() {
        // Long cache_time so nothing expires during the test.
        let config = QueryConfig::new(Duration::from_secs(0), Duration::from_secs(300));
        let client = QueryClient::with_config(config);

        client.set_cache("a".to_string(), CacheEntry::new(1));
        client.set_cache("b".to_string(), CacheEntry::new(2));

        client.gc();

        assert!(client.get_cache::<i32>("a").is_some());
        assert!(client.get_cache::<i32>("b").is_some());
        assert_eq!(client.cache.len(), 2);
    }

    #[test]
    fn test_gc_preserves_entries_of_different_types() {
        let config = QueryConfig::new(Duration::from_secs(0), Duration::from_secs(300));
        let client = QueryClient::with_config(config);

        client.set_cache("int".to_string(), CacheEntry::new(42));
        client.set_cache("str".to_string(), CacheEntry::new("hello".to_string()));

        client.gc();

        assert_eq!(client.get_cache::<i32>("int").map(|e| e.data), Some(42));
        assert_eq!(
            client.get_cache::<String>("str").map(|e| e.data),
            Some("hello".to_string())
        );
    }

    // ---------------------------------------------------------------------------
    // Regression tests for same-key / different-type cache collision.
    //
    // Before the fix the cache was keyed by the plain string key, so
    // `Query<i32>` and `Query<String>` sharing the same key would overwrite
    // each other's entries.  The second query to fetch would always see a cache
    // miss (downcast to the wrong type fails), wasting a network round-trip and
    // potentially causing alternating overwrites on every invalidation cycle.
    // ---------------------------------------------------------------------------

    /// Two queries with the same string key but different value types must
    /// occupy independent cache slots and never overwrite each other.
    #[test]
    fn test_same_key_different_types_use_independent_cache_slots() {
        let client = QueryClient::new();

        // Store an i32 and a String under the same string key.
        client.set_cache("data".to_string(), CacheEntry::new(42i32));
        client.set_cache("data".to_string(), CacheEntry::new("hello".to_string()));

        // Both entries must be independently readable without either one
        // having been overwritten by the other.
        assert_eq!(
            client.get_cache::<i32>("data").map(|e| e.data),
            Some(42),
            "i32 entry should not be overwritten by the String entry"
        );
        assert_eq!(
            client.get_cache::<String>("data").map(|e| e.data),
            Some("hello".to_string()),
            "String entry should not be overwritten by the i32 entry"
        );
    }

    /// A cache hit for one value type must not be seen as a miss when a
    /// different type uses the same string key.
    #[tokio::test]
    async fn test_same_key_different_types_fetch_independently() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let client = Arc::new(QueryClient::new());
        let i32_fetches = Arc::new(AtomicUsize::new(0));
        let str_fetches = Arc::new(AtomicUsize::new(0));

        let i32_fetches_clone = i32_fetches.clone();
        let query_i32 = Query::new(
            &"data",
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
            &"data",
            move || {
                let count = str_fetches_clone.clone();
                Box::pin(async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok::<String, QueryError>("one".to_string())
                })
            },
            client.clone(),
        );

        let mut stream_i32 = query_i32.stream();
        let mut stream_str = query_str.stream();

        // Both streams start in Loading (cache is empty).
        let _ = stream_i32.next().await; // Loading
        let _ = stream_i32.next().await; // Success (fetch completes, populates cache)

        let _ = stream_str.next().await; // Loading
        let _ = stream_str.next().await; // Success

        // Each type should have been fetched exactly once; the cache hit of
        // one type must not cause a spurious miss for the other.
        assert_eq!(
            i32_fetches.load(Ordering::SeqCst),
            1,
            "i32 query should have fetched exactly once"
        );
        assert_eq!(
            str_fetches.load(Ordering::SeqCst),
            1,
            "String query should have fetched exactly once; a type-collision cache miss would cause it to fetch again"
        );
    }

    // ---------------------------------------------------------------------------
    // Regression tests for the "missed invalidation during fetch" race.
    //
    // Before the fix, `perform_fetch` subscribed to the invalidation channel
    // *after* `fetcher().await` returned, creating a window where any
    // invalidation broadcast while the fetch was in-flight was silently
    // dropped.  The fix subscribes a single receiver in `State::Initial` and
    // threads it through the entire state machine so that no invalidation is
    // ever missed.
    // ---------------------------------------------------------------------------

    /// An invalidation broadcast while a fetch is in progress must trigger
    /// another refetch once the fetch completes.
    ///
    /// With the old code the invalidation was lost (the receiver was created
    /// *after* the fetch returned), so the subscription would stall in
    /// `State::Watching` until the next explicit invalidation.
    #[tokio::test]
    async fn test_invalidation_during_fetch_triggers_refetch() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::time::Duration;

        let client = Arc::new(QueryClient::new());
        let fetch_count = Arc::new(AtomicUsize::new(0));

        let fetch_count_clone = fetch_count.clone();
        let query = Query::new(
            &"key",
            move || {
                let count = fetch_count_clone.clone();
                Box::pin(async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    // Simulate a slow network request so the test has a window
                    // to inject an invalidation while the fetch is running.
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    Ok::<i32, QueryError>(1)
                })
            },
            client.clone(),
        );

        let mut stream = query.stream();

        // Cold start: Loading
        let first = stream.next().await;
        assert!(matches!(first, Some(ref r) if r.is_loading()));

        // Inject an invalidation while the first fetch is still sleeping.
        // The fetch takes 50 ms; we fire the invalidation at 25 ms.
        let client_for_invalidate = client.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(25)).await;
            let _ = client_for_invalidate
                .invalidation_tx
                .send("key".to_string());
        });

        // First fetch completes.
        let second = stream.next().await;
        assert!(
            matches!(second, Some(ref r) if r.is_success()),
            "first fetch should succeed"
        );

        // The invalidation that arrived during the first fetch must trigger a
        // second Loading → Success cycle.
        let third = stream.next().await;
        assert!(
            matches!(third, Some(ref r) if r.is_loading()),
            "invalidation during fetch must trigger a subsequent refetch"
        );

        let fourth = stream.next().await;
        assert!(
            matches!(fourth, Some(ref r) if r.is_success()),
            "second fetch should succeed"
        );

        assert_eq!(
            fetch_count.load(Ordering::SeqCst),
            2,
            "exactly two fetches should have been performed"
        );
    }

    /// An invalidation broadcast immediately after the Initial state emits
    /// fresh cached data must trigger a refetch, not be silently dropped.
    ///
    /// With the old code the receiver was subscribed *after* the cache check,
    /// leaving a small window where the invalidation could be missed; with the
    /// fix the receiver is subscribed before the cache check.
    #[tokio::test]
    async fn test_invalidation_after_initial_fresh_cache_emission_triggers_refetch() {
        use std::time::Duration;

        // Long stale_time so the cached entry is treated as fresh.
        let config = QueryConfig::new(Duration::from_secs(3600), Duration::from_secs(3600));
        let client = Arc::new(QueryClient::with_config(config));

        // Pre-populate the cache with a fresh entry.
        client.set_cache("key".to_string(), CacheEntry::new(42));

        let query = Query::new(
            &"key",
            || Box::pin(async { Ok::<i32, QueryError>(99) }),
            client.clone(),
        );

        let mut stream = query.stream();

        // Initial state emits the fresh cached value.
        let first = stream.next().await;
        assert!(
            matches!(first, Some(ref r) if r.is_success() && !r.is_stale()),
            "should emit fresh cached data"
        );

        // Send an invalidation now.  Because the receiver was subscribed
        // before the cache check, this is guaranteed to be captured even if
        // it fires before the stream is polled again.
        let _ = client.invalidation_tx.send("key".to_string());

        // The invalidation must surface as a Loading state followed by Success.
        let second = stream.next().await;
        assert!(
            matches!(second, Some(ref r) if r.is_loading()),
            "invalidation after initial fresh-cache emission must trigger a refetch"
        );

        let third = stream.next().await;
        assert!(
            matches!(third, Some(ref r) if r.is_success()),
            "refetch after invalidation should yield fresh data"
        );
        assert_eq!(
            third.expect("third result should be Some").data(),
            Some(&99),
            "should return the fetched value, not the stale cached value"
        );
    }

    #[tokio::test]
    async fn test_watching_survives_lagged_invalidations() {
        // A query that is watching for invalidations must not terminate when
        // the broadcast receiver lags (more invalidations buffered than the
        // channel capacity). Instead it should recover by refetching, since a
        // lag may have dropped this key's invalidation.
        let client = Arc::new(QueryClient::new());
        let query = Query::new(
            &"key",
            || Box::pin(async { Ok::<i32, QueryError>(1) }),
            client.clone(),
        );

        let mut stream = query.stream();

        // Initial Loading, then Success (the watcher's receiver is now active).
        let loading = stream.next().await;
        assert!(matches!(loading, Some(ref r) if r.is_loading()));
        let success = stream.next().await;
        assert!(matches!(success, Some(ref r) if r.is_success()));

        // Overflow the broadcast buffer (capacity 100) without polling the
        // stream so the watcher's receiver lags behind.
        for i in 0..150 {
            let _ = client.invalidation_tx.send(format!("other-{i}"));
        }

        // The subscription must survive the lag and refetch rather than ending.
        let next = stream.next().await;
        assert!(
            matches!(next, Some(ref r) if r.is_loading()),
            "lagged invalidations should trigger a refetch, not end the subscription"
        );
    }
}
