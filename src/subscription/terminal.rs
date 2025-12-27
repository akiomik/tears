use std::hash::{DefaultHasher, Hash, Hasher};

use crossterm::event::{Event, EventStream};
use futures::{StreamExt, stream::BoxStream};

use super::{SubscriptionId, SubscriptionSource};

/// A stream of terminal events using crossterm's EventStream.
#[derive(Debug, Clone, PartialEq, Eq, Default, Hash)]
pub struct TerminalEvents;

impl TerminalEvents {
    pub fn new() -> Self {
        Self
    }
}

impl SubscriptionSource for TerminalEvents {
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
        // Since TerminalEvents is a singleton (no parameters), use a constant ID
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        SubscriptionId::of::<Self>(hasher.finish())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_terminal_events_new() {
        let events = TerminalEvents::new();
        assert_eq!(events, TerminalEvents);
    }

    #[test]
    fn test_terminal_events_id_consistency() {
        let events1 = TerminalEvents::new();
        let events2 = TerminalEvents::new();

        // Same subscription should have the same ID
        assert_eq!(events1.id(), events2.id());
    }
}
