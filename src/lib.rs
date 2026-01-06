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
//! enum Message { Increment }
//!
//! struct Counter { count: u32 }
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
//!         // Render UI
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
//! ### WebSocket Support
//!
//! ```toml
//! [dependencies]
//! tears = { version = "0.6", features = ["ws", "native-tls"] }
//! ```
//!
//! Enables `subscription::websocket::WebSocket`. Requires a TLS feature for `wss://`:
//! `native-tls`, `rustls`, or `rustls-tls-webpki-roots`.
//!
//! ### HTTP Support
//!
//! ```toml
//! [dependencies]
//! tears = { version = "0.6", features = ["http"] }
//! ```
//!
//! Enables `subscription::http` with Query and Mutation support.

pub mod application;
pub mod command;
pub mod prelude;
pub mod runtime;
pub mod subscription;

// Re-export commonly used types
pub use application::Application;
pub use command::{Action, Command};
pub use futures::stream::BoxStream;
pub use runtime::Runtime;
pub use subscription::{Subscription, SubscriptionId, SubscriptionSource};
