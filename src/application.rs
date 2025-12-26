use ratatui::Frame;

use crate::{command::Command, subscription::Subscription};

pub trait Application: Sized {
    type Message: Send + 'static;
    type Flags: Clone + Send;

    /// Initialize with flags (configuration)
    fn new(flags: Self::Flags) -> (Self, Command<Self::Message>);

    /// Update function
    fn update(&mut self, msg: Self::Message) -> Command<Self::Message>;

    /// View function
    fn view(&self, frame: &mut Frame<'_>);

    /// Subscriptions
    fn subscriptions(&self) -> Vec<Subscription<Self::Message>>;
}
