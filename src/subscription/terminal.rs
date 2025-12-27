use std::hash::{DefaultHasher, Hash, Hasher};

use crossterm::event::{Event, EventStream};
use futures::{stream::BoxStream, StreamExt};

use super::{SubscriptionId, SubscriptionSource};

/// Terminal event subscription using crossterm's EventStream.
#[derive(Debug, Clone, PartialEq, Eq, Default, Hash)]
pub struct TerminalSub;

impl TerminalSub {
    pub fn new() -> Self {
        Self
    }
}

impl SubscriptionSource for TerminalSub {
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
        SubscriptionId::of::<Self>(hasher.finish())
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
}
