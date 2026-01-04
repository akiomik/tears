//! # Tears - TUI Elm Architecture Runtime System
//!
//! Tears is a TUI (Text User Interface) framework based on the Elm Architecture (TEA),
//! built on top of [ratatui](https://ratatui.rs/). It provides a clean and type-safe
//! way to build terminal applications using a functional, message-driven architecture.
//!
//! ## Architecture
//!
//! The framework follows the Elm Architecture pattern:
//!
//! 1. **Model**: Your application state
//! 2. **Message**: Events that can change the state
//! 3. **Update**: Function that processes messages and updates the model
//! 4. **View**: Function that renders the UI based on the current model
//! 5. **Subscriptions**: External event sources (keyboard, timers, etc.)
//! 6. **Commands**: Asynchronous operations that produce messages
//!
//! ## Core Components
//!
//! - [`application::Application`]: The main trait that defines your application
//! - [`runtime::Runtime`]: Manages the application lifecycle and event loop
//! - [`command::Command`]: Represents asynchronous side effects
//! - [`subscription::Subscription`]: Represents ongoing event sources
//!
//! ## Example
//!
//! ```rust,no_run
//! use ratatui::Frame;
//! use tears::prelude::*;
//!
//! #[derive(Debug)]
//! enum Message {
//!     Increment,
//! }
//!
//! struct Counter {
//!     count: u32,
//! }
//!
//! impl Application for Counter {
//!     type Message = Message;
//!     type Flags = ();
//!
//!     fn new(_flags: ()) -> (Self, Command<Message>) {
//!         (Counter { count: 0 }, Command::none())
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
//!         // Render your UI here
//!     }
//!
//!     fn subscriptions(&self) -> Vec<Subscription<Message>> {
//!         vec![]
//!     }
//! }
//! ```
//!
//! ## Optional Features
//!
//! Tears provides optional features that can be enabled in your `Cargo.toml`:
//!
//! ### WebSocket Support
//!
//! - **`ws`**: Enables WebSocket subscription support via `subscription::websocket::WebSocket`
//!
//! Requires one of the following TLS features for secure connections (`wss://`):
//! - **`native-tls`**: Uses the platform's native TLS implementation
//! - **`rustls`**: Uses rustls with ring crypto provider and native root certificates
//! - **`rustls-tls-webpki-roots`**: Uses rustls with ring crypto provider and webpki root certificates
//!
//! Example:
//!
//! ```toml
//! [dependencies]
//! tears = { version = "0.5", features = ["ws", "native-tls"] }
//! ```
//!
//! ## Design Inspiration
//!
//! This framework is inspired by [iced](https://github.com/iced-rs/iced) 0.12,
//! adapted for TUI applications.

pub mod application;
pub mod command;
pub mod prelude;
pub mod runtime;
pub mod subscription;

// Re-export the most commonly used types at the crate root
// This allows users to import them directly: `use tears::Application;`
pub use application::Application;
pub use command::{Action, Command};
pub use runtime::Runtime;
pub use subscription::{Subscription, SubscriptionId, SubscriptionSource};

// Re-export commonly used external types
// This allows users to implement custom subscriptions without directly depending on futures
pub use futures::stream::BoxStream;
