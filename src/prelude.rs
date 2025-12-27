//! Prelude module for convenient imports.
//!
//! This module re-exports the most commonly used types and traits,
//! allowing you to import them all at once with:
//!
//! ```
//! use tears::prelude::*;
//! ```
//!
//! # What's included
//!
//! - [`Application`] - The main application trait
//! - [`Command`] - For performing side effects
//! - [`Action`] - Actions that commands can perform
//! - [`Subscription`] - For handling event sources
//! - [`Runtime`] - The application runtime
//!
//! # Example
//!
//! ```rust,no_run
//! use tears::prelude::*;
//! use ratatui::Frame;
//!
//! struct MyApp {
//!     count: u32,
//! }
//!
//! enum Message {
//!     Increment,
//! }
//!
//! impl Application for MyApp {
//!     type Message = Message;
//!     type Flags = ();
//!
//!     fn new(_flags: ()) -> (Self, Command<Message>) {
//!         (MyApp { count: 0 }, Command::none())
//!     }
//!
//!     fn update(&mut self, msg: Message) -> Command<Message> {
//!         match msg {
//!             Message::Increment => {
//!                 self.count += 1;
//!                 Command::none()
//!             }
//!         }
//!     }
//!
//!     fn view(&self, frame: &mut Frame<'_>) {
//!         // Render UI
//!     }
//!
//!     fn subscriptions(&self) -> Vec<Subscription<Message>> {
//!         vec![]
//!     }
//! }
//! ```

// Re-export the Application trait
pub use crate::application::Application;

// Re-export Command types
pub use crate::command::{Action, Command};

// Re-export Runtime
pub use crate::runtime::Runtime;

// Re-export Subscription types
pub use crate::subscription::Subscription;
