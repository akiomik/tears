//! Terminal event subscription.
//!
//! This module provides the [`TerminalEvents`] subscription source for handling
//! terminal input events using crossterm.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::io;

use crossterm::event::{Event, EventStream};
use futures::{StreamExt, stream::BoxStream};

use super::{SubscriptionId, SubscriptionSource};

/// A subscription source for terminal events.
///
/// This provides a stream of terminal events such as keyboard input, mouse events,
/// and window resize events using crossterm's `EventStream`.
///
/// # Example
///
/// ```rust,no_run
/// use tears::subscription::{Subscription, terminal::TerminalEvents};
/// use crossterm::event::{Event, KeyCode};
///
/// enum Message {
///     Input(Event),
///     InputError(std::io::Error),
/// }
///
/// // Create a subscription for terminal events
/// let sub = Subscription::new(TerminalEvents::new())
///     .map(|result| match result {
///         Ok(event) => Message::Input(event),
///         Err(e) => Message::InputError(e),
///     });
/// ```
///
/// # Error Handling
///
/// This subscription yields `Result<Event, io::Error>` values. Applications are
/// responsible for handling errors appropriately. Common error scenarios include:
///
/// - **Terminal disconnection**: Usually unrecoverable, may want to exit gracefully
/// - **I/O errors**: May be transient or permanent depending on the cause
///
/// You can choose to:
/// - Log errors and continue (errors stop producing further events)
/// - Display an error message to the user
/// - Attempt to recreate the subscription
/// - Exit the application
///
/// ```rust,no_run
/// # use tears::prelude::*;
/// # use crossterm::event::Event;
/// # struct App;
/// enum Message {
///     Input(Event),
///     InputError(std::io::Error),
/// }
/// # impl Application for App {
/// #     type Message = Message;
/// #     type Flags = ();
/// #     fn new(_: ()) -> (Self, Command<Message>) { (App, Command::none()) }
/// #     fn view(&self, _: &mut ratatui::Frame) {}
/// #     fn subscriptions(&self) -> Vec<Subscription<Message>> { vec![] }
///
/// fn update(&mut self, msg: Message) -> Command<Message> {
///     match msg {
///         Message::Input(event) => {
///             // Handle the event
///             Command::none()
///         }
///         Message::InputError(e) => {
///             // Handle the error (log, show UI, exit, etc.)
///             eprintln!("Terminal error: {}", e);
///             Command::effect(Action::Quit)
///         }
///     }
/// }
/// # }
/// ```
///
/// # Note
///
/// This is a singleton subscription - all instances are considered identical
/// and only one terminal event stream will be active at a time.
#[derive(Debug, Clone, PartialEq, Eq, Default, Hash)]
pub struct TerminalEvents;

impl TerminalEvents {
    /// Create a new terminal events subscription.
    ///
    /// # Example
    ///
    /// ```
    /// use tears::subscription::terminal::TerminalEvents;
    ///
    /// let events = TerminalEvents::new();
    /// ```
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl SubscriptionSource for TerminalEvents {
    type Output = Result<Event, io::Error>;

    fn stream(&self) -> BoxStream<'static, Self::Output> {
        let stream = EventStream::new();

        // Create a stream that yields terminal events as Results
        futures::stream::unfold(stream, |mut stream| async move {
            // NOTE: EventStream::next() returns Result<Event, io::Error>
            // We pass the Result directly to the application, allowing users to
            // decide how to handle errors (log, retry, show error UI, etc.)
            stream.next().await.map(|result| (result, stream))
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
