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

    /// Returns `true` if the entry has outlived `cache_time` and should be
    /// garbage collected.
    fn is_expired(&self, cache_time: Duration) -> bool;
}

impl<T: Send + Sync + 'static> AnyCacheEntry for CacheEntry<T> {
    fn as_any(&self) -> &dyn Any {
        self
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
    pub fn check_staleness(&mut self, stale_time: Duration) -> bool {
        if self.timestamp.elapsed() > stale_time {
            self.is_stale = true;
        }
        self.is_stale
    }

    /// Marks this entry as stale.
    #[allow(dead_code)]
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
        let mut entry = CacheEntry::new(42);
        let is_stale = entry.check_staleness(Duration::from_secs(1));
        assert!(!is_stale);
        assert!(!entry.is_stale);
    }

    #[test]
    fn test_check_staleness_stale() {
        let mut entry = CacheEntry::new(42);
        sleep(Duration::from_millis(10));
        let is_stale = entry.check_staleness(Duration::from_millis(5));
        assert!(is_stale);
        assert!(entry.is_stale);
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
