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
//! - [`Application`]: The main trait that defines your application
//! - [`Runtime`]: Manages the application lifecycle and event loop
//! - [`Command`]: Represents asynchronous side effects and runtime directives
//! - [`Subscription`]: Represents ongoing event sources
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

pub(crate) mod application;
pub mod command;
pub(crate) mod panic;
pub mod prelude;
pub(crate) mod runtime;
mod structural_key;
pub mod subscription;
// `TestStore`'s single public path is `tears::testing::TestStore`: no
// crate-root re-export and no prelude membership, because a minimal skeleton
// app never names the type (RFC 0008 §3.3; docs/api-guidelines.md "Prelude
// Membership").
pub mod testing;

// Re-export commonly used types
pub use application::Application;
pub use command::core::Command;
// Re-exported because implementing `SubscriptionSource::stream` requires
// writing this type out in the return position; see docs/api-guidelines.md
// "External Crate Re-exports".
pub use futures::stream::BoxStream;
pub use panic::install_panic_hook;
pub use runtime::Runtime;
// `RuntimeConfig` is `Runtime::with_config`'s companion type; re-exported at the
// crate root but deliberately *not* in the prelude, since a minimal skeleton app
// calling `Runtime::new` never names it (RFC 0007 §2.3, INV-C4).
pub use runtime::config::RuntimeConfig;
// `FrameRate` lives under `runtime` (it is a scheduling input); re-exported here
// as `tears::FrameRate` so it keeps a single canonical public path. See
// docs/api-guidelines.md for the module visibility / root promotion rules
// this follows.
pub use runtime::frame_rate::{FrameRate, FrameRateError};
pub use subscription::core::{Subscription, SubscriptionId, SubscriptionSource};
// Bench-only re-export exposing the producer-gauge observer to
// `benches/gauge.rs`, which compiles as a separate crate and only sees the
// public API. `runtime` is `pub(crate)`, so without this re-export the bench
// could not name `LoadObserver` at all. Gated behind `bench-internals`, which
// is not part of the public API and carries no semver guarantees; do not
// enable it for normal builds. See `BenchSubscriptionManager` for the same
// pattern applied to the subscription reconciliation hot path.
#[cfg(feature = "bench-internals")]
#[doc(hidden)]
pub use runtime::load::LoadObserver;

#[cfg(test)]
mod test_support;
