use std::any::Any;
use std::time::{Duration, Instant};

/// A type-erased view over a [`CacheEntry`] used by the query cache.
///
/// The cache stores entries for arbitrary value types behind `Box<dyn ...>`.
/// This trait exposes the operations the cache needs without knowing the
/// concrete value type: downcasting for retrieval and expiry checks for
/// garbage collection.
pub trait AnyCacheEntry: Send + Sync {
    /// Returns the entry as `&dyn Any` so callers can downcast to the
    /// concrete `CacheEntry<T>`.
    fn as_any(&self) -> &dyn Any;

    /// Marks the entry as stale without knowing its concrete value type.
    fn mark_stale(&mut self);

    /// Returns `true` if the entry has outlived `cache_time` and should be
    /// garbage collected.
    fn is_expired(&self, cache_time: Duration) -> bool;
}

impl<T: Send + Sync + 'static> AnyCacheEntry for CacheEntry<T> {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn mark_stale(&mut self) {
        // Call the inherent method explicitly. `self.mark_stale()` would also
        // resolve to it today (inherent methods take priority over trait
        // methods), but being explicit avoids silently turning into infinite
        // recursion if the inherent method is ever renamed or removed.
        Self::mark_stale(self);
    }

    fn is_expired(&self, cache_time: Duration) -> bool {
        self.should_gc(cache_time)
    }
}

/// A cached entry with timestamp and staleness information.
#[derive(Debug, Clone)]
pub struct CacheEntry<T> {
    pub data: T,
    pub timestamp: Instant,
    pub is_stale: bool,
}

impl<T> CacheEntry<T> {
    /// Creates a new cache entry with the given data.
    pub fn new(data: T) -> Self {
        Self {
            data,
            timestamp: Instant::now(),
            is_stale: false,
        }
    }

    /// Checks if this entry is stale based on the given stale time.
    ///
    /// Returns `true` if the entry was explicitly marked stale via
    /// [`mark_stale`](Self::mark_stale) or if `stale_time` has elapsed
    /// since the entry was created.  Does **not** mutate the entry.
    pub fn check_staleness(&self, stale_time: Duration) -> bool {
        self.is_stale || self.timestamp.elapsed() > stale_time
    }

    /// Marks this entry as stale.
    pub const fn mark_stale(&mut self) {
        self.is_stale = true;
    }

    /// Updates the entry with new data, resetting timestamp and staleness.
    #[allow(dead_code)]
    pub fn update(&mut self, data: T) {
        self.data = data;
        self.timestamp = Instant::now();
        self.is_stale = false;
    }

    /// Checks if this entry should be garbage collected based on cache time.
    pub fn should_gc(&self, cache_time: Duration) -> bool {
        self.timestamp.elapsed() > cache_time
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn test_new_entry() {
        let entry = CacheEntry::new(42);
        assert_eq!(entry.data, 42);
        assert!(!entry.is_stale);
    }

    #[test]
    fn test_check_staleness_fresh() {
        let entry = CacheEntry::new(42);
        let is_stale = entry.check_staleness(Duration::from_secs(1));
        assert!(!is_stale);
        assert!(!entry.is_stale);
    }

    #[test]
    fn test_check_staleness_stale() {
        let entry = CacheEntry::new(42);
        sleep(Duration::from_millis(10));
        let is_stale = entry.check_staleness(Duration::from_millis(5));
        assert!(is_stale);
        // check_staleness must NOT mutate the entry — previously this
        // was called on a clone from the cache so the mutation was silently
        // discarded anyway; now the signature enforces immutability.
        assert!(
            !entry.is_stale,
            "check_staleness must not mutate is_stale on the entry"
        );
    }

    #[test]
    fn test_check_staleness_respects_mark_stale() {
        // An entry that was explicitly marked stale via mark_stale should
        // be reported as stale by check_staleness even within the stale_time.
        let mut entry = CacheEntry::new(42);
        entry.mark_stale();
        assert!(
            entry.check_staleness(Duration::from_secs(3600)),
            "entry marked stale should be reported stale regardless of elapsed time"
        );
    }

    #[test]
    fn test_mark_stale() {
        let mut entry = CacheEntry::new(42);
        assert!(!entry.is_stale);
        entry.mark_stale();
        assert!(entry.is_stale);
    }

    #[test]
    fn test_update() {
        let mut entry = CacheEntry::new(42);
        entry.mark_stale();
        assert!(entry.is_stale);

        entry.update(100);
        assert_eq!(entry.data, 100);
        assert!(!entry.is_stale);
    }
}
