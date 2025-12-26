use std::hash::{DefaultHasher, Hash, Hasher};

use crossterm::event::{Event, EventStream};
use futures::{StreamExt, stream::BoxStream};

use super::{SubscriptionId, SubscriptionInner};

/// Terminal event subscription using crossterm's EventStream.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TerminalSub;

impl TerminalSub {
    pub fn new() -> Self {
        Self
    }
}

impl SubscriptionInner for TerminalSub {
    type Output = Event;

    fn stream(&self) -> BoxStream<'static, Self::Output> {
        let stream = EventStream::new();

        // Create a stream that yields terminal events
        futures::stream::unfold(stream, |mut stream| async move {
            // NOTE: EventStream::next() returns Result<Event>, we need to handle errors
            match stream.next().await {
                Some(Ok(event)) => Some((event, stream)),
                Some(Err(_)) => None, // Stop the stream on error
                None => None,
            }
        })
        .boxed()
    }

    fn id(&self) -> SubscriptionId {
        // Since TerminalSub is a singleton (no parameters), use a constant ID
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        SubscriptionId::from_hash(hasher.finish())
    }
}

impl Hash for TerminalSub {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Hash a constant value since TerminalSub has no fields
        "terminal".hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_terminal_sub_new() {
        let sub = TerminalSub::new();
        assert_eq!(sub, TerminalSub);
    }

    #[test]
    fn test_terminal_sub_id_consistency() {
        let sub1 = TerminalSub::new();
        let sub2 = TerminalSub::new();

        // Same subscription should have the same ID
        assert_eq!(sub1.id(), sub2.id());
    }

    #[test]
    fn test_terminal_sub_hash_consistency() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let sub1 = TerminalSub::new();
        let sub2 = TerminalSub::new();

        let mut hasher1 = DefaultHasher::new();
        sub1.hash(&mut hasher1);
        let hash1 = hasher1.finish();

        let mut hasher2 = DefaultHasher::new();
        sub2.hash(&mut hasher2);
        let hash2 = hasher2.finish();

        // Same subscription should have the same hash
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_terminal_sub_different_from_empty_hash() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let terminal_sub = TerminalSub::new();

        let mut hasher1 = DefaultHasher::new();
        terminal_sub.hash(&mut hasher1);
        let terminal_hash = hasher1.finish();

        // Empty struct hash (should be different)
        #[derive(Hash)]
        struct Empty;
        let mut hasher2 = DefaultHasher::new();
        Empty.hash(&mut hasher2);
        let empty_hash = hasher2.finish();

        // Terminal sub should have a non-empty hash
        assert_ne!(
            terminal_hash, empty_hash,
            "TerminalSub hash should not be empty"
        );
    }
}
