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

use std::any::Any;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use dashmap::DashMap;
use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::{self, BoxStream};
use thiserror::Error;
use tokio::sync::broadcast;

use crate::Command;
use crate::subscription::{SubscriptionId, SubscriptionSource};

use super::cache::CacheEntry;
use super::config::QueryConfig;

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
#[derive(Debug, Clone)]
pub struct QueryClient {
    cache: Arc<DashMap<String, Box<dyn Any + Send + Sync>>>,
    invalidation_tx: broadcast::Sender<String>,
    config: QueryConfig,
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
        let tx = self.invalidation_tx.clone();
        let key_string = key.to_string();

        // Use a stream that completes without producing any messages
        Command {
            stream: Some(
                futures::stream::once(async move {
                    // Note: We cannot directly mark cache entries as stale because
                    // the cache stores `Box<dyn Any>` and we don't know the concrete type.
                    // Instead, we just broadcast the invalidation notification,
                    // which will trigger Query subscriptions to refetch.

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
        self.cache
            .get(key)
            .and_then(|entry| entry.downcast_ref::<CacheEntry<T>>().cloned())
    }

    /// Sets a cached entry for the given key.
    fn set_cache<T: Clone + Send + Sync + 'static>(&self, key: String, entry: CacheEntry<T>) {
        self.cache.insert(key, Box::new(entry));
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
pub struct Query<V> {
    key: String,
    fetcher: Arc<dyn Fn() -> BoxFuture<'static, Result<V, QueryError>> + Send + Sync>,
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
                        // Check cache first
                        if let Some(mut cached) = client.get_cache::<V>(&key) {
                            let is_stale = cached.check_staleness(client.config().stale_time);

                            let result = QueryResult {
                                state: QueryState::Success {
                                    data: cached.data.clone(),
                                    is_stale,
                                },
                            };

                            if is_stale {
                                // Stale data: emit it, then refetch
                                Some((result, State::Refetching))
                            } else {
                                // Fresh data: emit it, then wait for invalidation
                                let rx = client.subscribe_invalidation();
                                Some((result, State::Watching { rx }))
                            }
                        } else {
                            // No cache: emit Loading, then fetch
                            let result = QueryResult {
                                state: QueryState::Loading,
                            };
                            Some((result, State::Fetching))
                        }
                    }

                    State::Fetching => {
                        // Perform the fetch
                        match fetcher().await {
                            Ok(data) => {
                                // Cache the result
                                let entry = CacheEntry::new(data.clone());
                                client.set_cache(key.clone(), entry);

                                let result = QueryResult {
                                    state: QueryState::Success {
                                        data,
                                        is_stale: false,
                                    },
                                };

                                // After successful fetch, watch for invalidation
                                let rx = client.subscribe_invalidation();
                                Some((result, State::Watching { rx }))
                            }
                            Err(e) => {
                                // Error: emit error, then watch for invalidation to retry
                                let result = QueryResult {
                                    state: QueryState::Error(e.to_string()),
                                };

                                let rx = client.subscribe_invalidation();
                                Some((result, State::Watching { rx }))
                            }
                        }
                    }

                    State::Refetching => {
                        // Similar to Fetching, but we already emitted stale data
                        match fetcher().await {
                            Ok(data) => {
                                let entry = CacheEntry::new(data.clone());
                                client.set_cache(key.clone(), entry);

                                let result = QueryResult {
                                    state: QueryState::Success {
                                        data,
                                        is_stale: false,
                                    },
                                };

                                let rx = client.subscribe_invalidation();
                                Some((result, State::Watching { rx }))
                            }
                            Err(e) => {
                                let result = QueryResult {
                                    state: QueryState::Error(e.to_string()),
                                };

                                let rx = client.subscribe_invalidation();
                                Some((result, State::Watching { rx }))
                            }
                        }
                    }

                    State::Watching { mut rx } => {
                        // Wait for invalidation notification
                        loop {
                            match rx.recv().await {
                                Ok(invalidated_key) if invalidated_key == key => {
                                    // Our key was invalidated, refetch
                                    let result = QueryResult {
                                        state: QueryState::Loading,
                                    };
                                    return Some((result, State::Fetching));
                                }
                                Ok(_) => {
                                    // Different key, keep waiting
                                }
                                Err(_) => {
                                    // Channel closed, subscription ends
                                    return None;
                                }
                            }
                        }
                    }
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
        self.key.hash(hasher);
    }
}

/// Internal state machine for the Query subscription.
enum State {
    Initial,
    Fetching,
    Refetching,
    Watching { rx: broadcast::Receiver<String> },
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
}
