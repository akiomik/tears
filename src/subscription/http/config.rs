use std::time::Duration;

/// Configuration for query behavior.
///
/// This controls how queries retain data and when they consider it stale.
///
/// # Testing staleness deterministically
///
/// The `stale_time` and `cache_time` comparisons read the executor's
/// virtualizable clock (RFC 0009 §4.1), so they can be tested on a paused
/// single-threaded runtime (`#[tokio::test(start_paused = true)]`, with
/// tokio's `test-util` feature in your dev-dependencies) and driven with
/// `tokio::time::advance` — no wall-clock waiting. One caveat: a paused
/// runtime auto-advances the clock whenever the executor is idle, so a
/// test awaiting real network I/O can jump past a staleness deadline
/// before the response arrives. Drive time and I/O explicitly — feed
/// responses through test-controlled sources, then advance — rather than
/// awaiting a live socket. See the [`testing`](crate::testing) module doc
/// for the full recipe.
#[derive(Debug, Clone)]
pub struct QueryConfig {
    /// How long data is considered fresh before becoming stale.
    ///
    /// When data is fresh, queries will use retained data without refetching.
    /// Once stale, queries will refetch in the background while still showing retained data.
    pub stale_time: Duration,

    /// How long inactive query data is retained before being garbage collected.
    ///
    /// Active cells keep their data regardless of age. Inactive cell data is
    /// removed once this much time has elapsed since the last subscriber
    /// dropped. Garbage collection runs automatically after each fetch and can
    /// also be triggered manually via
    /// [`QueryClient::gc`](crate::subscription::http::QueryClient::gc).
    pub cache_time: Duration,
}

impl Default for QueryConfig {
    fn default() -> Self {
        Self {
            stale_time: Duration::from_secs(0),      // immediately stale
            cache_time: Duration::from_secs(5 * 60), // 5 minutes
        }
    }
}

impl QueryConfig {
    /// Creates a new query configuration with the given stale and cache times.
    #[must_use]
    pub const fn new(stale_time: Duration, cache_time: Duration) -> Self {
        Self {
            stale_time,
            cache_time,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = QueryConfig::default();
        assert_eq!(config.stale_time, Duration::from_secs(0));
        assert_eq!(config.cache_time, Duration::from_secs(5 * 60));
    }

    #[test]
    fn test_new_config() {
        let config = QueryConfig::new(Duration::from_secs(30), Duration::from_secs(300));
        assert_eq!(config.stale_time, Duration::from_secs(30));
        assert_eq!(config.cache_time, Duration::from_secs(300));
    }
}
