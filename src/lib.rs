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
//! 6. **Commands**: Asynchronous operations and runtime directives
//!
//! ## Core Components
//!
//! - [`application::Application`]: The main trait that defines your application
//! - [`runtime::Runtime`]: Manages the application lifecycle and event loop
//! - [`command::Command`]: Represents asynchronous side effects and runtime directives
//! - [`subscription::Subscription`]: Represents ongoing event sources
//! - [`install_panic_hook`]: Restores the terminal if the application panics
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
//! ## Observability
//!
//! The runtime emits [`tracing`](https://docs.rs/tracing) events for its hot
//! paths — message batches, subscription updates, command spawns, renders, and
//! shutdown — under the `tears::runtime` and `tears::subscription` targets.
//! Events are inert unless a `tracing` subscriber is installed, so there is no
//! setup required to ignore them. To see them, install any subscriber (e.g.
//! `tracing_subscriber::fmt()`) before running the app.
//!
//! ## Optional Features
//!
//! ### WebSocket Support
//!
//! ```toml
//! [dependencies]
//! tears = { version = "0.9", features = ["ws", "native-tls"] }
//! ```
//!
//! Enables `subscription::websocket::WebSocket`. Requires a TLS feature for `wss://`:
//! `native-tls`, `rustls`, or `rustls-tls-webpki-roots`.
//!
//! ### HTTP Support
//!
//! ```toml
//! [dependencies]
//! tears = { version = "0.9", features = ["http"] }
//! ```
//!
//! Enables `subscription::http` with Query and Mutation support.

pub mod application;
pub mod command;
pub mod panic;
pub mod prelude;
pub mod runtime;
pub mod subscription;

// Re-export commonly used types
pub use application::Application;
pub use command::{Action, Command};
pub use futures::stream::BoxStream;
pub use panic::install_panic_hook;
pub use runtime::Runtime;
// `FrameRate` lives under `runtime` (it is a scheduling input); re-exported here
// as `tears::FrameRate` so it keeps a single canonical public path.
pub use runtime::frame_rate::{FrameRate, FrameRateError};
pub use subscription::{Subscription, SubscriptionId, SubscriptionSource};

#[cfg(test)]
mod test_support {
    //! Test-only helpers shared across modules.

    use std::sync::Mutex;

    use ratatui::Frame;

    use crate::application::Application;
    use crate::command::{Action, Command};
    use crate::subscription::Subscription;

    /// Serializes tests that install a process-global panic hook or deliberately
    /// trigger panics.
    ///
    /// The panic hook is process-global and shared across threads, so a test that
    /// records hook activity and a test that panics must not run concurrently, or
    /// the panicking test's hook invocation would pollute the recording one.
    pub static PANIC_HOOK_GUARD: Mutex<()> = Mutex::new(());

    /// A minimal counter [`Application`] shared by the runtime and runtime-core
    /// tests. Increments on [`TestMessage::Increment`] and quits on
    /// [`TestMessage::Quit`].
    #[derive(Debug)]
    pub struct TestApp {
        pub counter: i32,
        should_quit: bool,
    }

    #[derive(Debug, Clone)]
    #[allow(dead_code)]
    pub enum TestMessage {
        Increment,
        Quit,
    }

    impl Application for TestApp {
        type Message = TestMessage;
        type Flags = i32;

        fn new(initial: i32) -> (Self, Command<Self::Message>) {
            (
                Self {
                    counter: initial,
                    should_quit: false,
                },
                Command::none(),
            )
        }

        fn update(&mut self, msg: Self::Message) -> Command<Self::Message> {
            match msg {
                TestMessage::Increment => {
                    self.counter += 1;
                    Command::none()
                }
                TestMessage::Quit => {
                    self.should_quit = true;
                    Command::effect(Action::Quit)
                }
            }
        }

        fn view(&self, _frame: &mut Frame<'_>) {
            // No-op for testing
        }

        fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
            vec![]
        }
    }
}
