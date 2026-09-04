# tears

[![Crates.io](https://img.shields.io/crates/v/tears.svg)](https://crates.io/crates/tears)
[![Documentation](https://docs.rs/tears/badge.svg)](https://docs.rs/tears)
[![CI](https://github.com/akiomik/tears/workflows/CI/badge.svg)](https://github.com/akiomik/tears/actions/workflows/ci.yml)
[![License](https://img.shields.io/crates/l/tears.svg)](LICENSE)
[![Rust Version](https://img.shields.io/badge/rust-1.88.0%2B-blue.svg)](https://www.rust-lang.org)
[![codecov](https://codecov.io/gh/akiomik/tears/graph/badge.svg?token=QF9SO8I0AM)](https://codecov.io/gh/akiomik/tears)

A simple and elegant framework for building TUI applications using **The Elm Architecture (TEA)**.

Built on top of [ratatui](https://ratatui.rs/), Tears provides a clean, type-safe, and functional approach to terminal user interface development.

## Features

- 🎯 **Simple & Predictable**: Based on The Elm Architecture - easy to reason about and test
- 🔄 **Async-First**: Built-in support for async operations via Commands
- 📡 **Subscriptions**: Handle terminal events, timers, and custom event sources
- 🧪 **Testable**: Pure functions for update logic make testing straightforward
- 🚀 **Powered by Ratatui**: Leverage the full power of the ratatui ecosystem
- 🦀 **Type-Safe**: Leverages Rust's type system for safer TUI applications

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
tears = "0.11"
ratatui = "0.30"
crossterm = "0.29"
tokio = { version = "1", features = ["full"] }
```

See the [Optional Features](#optional-features) section for information about enabling `ws` (WebSocket) and `http` (HTTP Query/Mutation) features.

## Getting Started

### Minimal Example

Every tears application implements the `Application` trait with four required methods:

```rust
use tears::prelude::*;
use ratatui::Frame;

struct App;

enum Message {}

impl Application for App {
    type Message = Message;  // Your message type
    type Flags = ();         // Initialization data (use () if none)

    // Initialize your app
    fn new(_flags: ()) -> (Self, Command<Message>) {
        (App, Command::none())
    }

    // Handle messages and update state
    fn update(&mut self, _msg: Message) -> Command<Message> {
        Command::none()
    }

    // Render your UI
    fn view(&self, frame: &mut Frame) {
        // Use ratatui widgets here
    }

    // Subscribe to events (keyboard, timers, etc.)
    fn subscriptions(&self) -> Vec<Subscription<Message>> {
        vec![]
    }
}
```

To run your application, create an `Runtime` and call `run()`:

```rust
#[tokio::main]
async fn main() -> Result<()> {
    let runtime = Runtime::<App>::new(());

    // Setup terminal (see complete example below)
    // ...

    runtime.run(&mut terminal).await?;
    Ok(())
}
```

### Complete Example

Here's a simple counter application that increments every second:

```rust
use std::num::NonZeroU64;

use color_eyre::eyre::Result;
use crossterm::event::{Event, KeyCode};
use ratatui::{Frame, text::Text};
use tears::prelude::*;
use tears::subscription::{terminal::TerminalEvents, time::{Timer, TimerEvent}};

#[derive(Debug, Clone)]
enum Message {
    Tick,
    Input(Event),
    InputError(String),
}

struct Counter {
    count: u32,
}

impl Application for Counter {
    type Message = Message;
    type Flags = ();

    fn new(_flags: ()) -> (Self, Command<Message>) {
        (Counter { count: 0 }, Command::none())
    }

    fn update(&mut self, msg: Message) -> Command<Message> {
        match msg {
            Message::Tick => {
                self.count += 1;
                Command::none()
            }
            Message::Input(Event::Key(key)) if key.code == KeyCode::Char('q') => {
                Command::quit()
            }
            Message::InputError(e) => {
                eprintln!("Input error: {e}");
                Command::quit()
            }
            _ => Command::none(),
        }
    }

    fn view(&self, frame: &mut Frame) {
        let text = Text::raw(format!("Count: {} (Press 'q' to quit)", self.count));
        frame.render_widget(text, frame.area());
    }

    fn subscriptions(&self) -> Vec<Subscription<Message>> {
        vec![
            Subscription::new(Timer::new(NonZeroU64::new(1000).expect("non-zero"))).map(|timer_msg| {
                match timer_msg {
                    TimerEvent::Tick => Message::Tick,
                }
            }),
            Subscription::new(TerminalEvents::new()).map(|result| match result {
                Ok(event) => Message::Input(event),
                Err(e) => Message::InputError(e.to_string()),
            }),
        ]
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    // Setup terminal
    let mut terminal = ratatui::init();

    // Restore the terminal on panic before the color_eyre report runs.
    // Installed after `color_eyre::install()` so it wraps that hook.
    tears::install_panic_hook();

    // Run the application
    let runtime = Runtime::<Counter>::new(());
    let result = runtime.run(&mut terminal).await;

    // Restore terminal (normal exit path)
    ratatui::restore();

    result
}
```

## Architecture

Tears follows **The Elm Architecture (TEA)** pattern:

```
┌──────────────────────────────────────────────┐
│                                              │
│  ┌─────────┐      ┌────────┐      ┌──────┐   │
│  │  Model  │─────▶│  View  │─────▶│  UI  │   │
│  └─────────┘      └────────┘      └──────┘   │
│       ▲                                      │
│       │                                      │
│  ┌────┴─────┐     ┌──────────────┐           │
│  │  Update  │◀────│   Messages   │           │
│  └──────────┘     └──────────────┘           │
│       ▲                   ▲                  │
│       │                   │                  │
│  ┌────┴─────┐      ┌──────┴──────┐           │
│  │ Commands │      │Subscriptions│           │
│  └──────────┘      └─────────────┘           │
│                                              │
└──────────────────────────────────────────────┘
```

### Core Concepts

- **Model**: Your application state
- **Message**: Events that trigger state changes
- **Update**: Pure function that processes messages and returns new state + commands
- **View**: Pure function that renders UI based on current state
- **Subscriptions**: External event sources (keyboard, timers, network, etc.)
- **Commands**: Asynchronous side effects that produce messages

### Built-in Subscriptions

- **Terminal Events** (`terminal::TerminalEvents`): Keyboard, mouse, and resize events
- **Timer** (`time::Timer`): Periodic tick events
- **Signal** (`signal::Signal`): OS signal handling (Unix/Windows)
- **WebSocket** (`websocket::WebSocket`, requires `ws`): Real-time bidirectional communication
- **Query** (`http::Query`, requires `http`): HTTP data fetching with caching
- **Mutation** (`http::Mutation`, requires `http`): HTTP data modifications
- **MockSource** (`mock::MockSource`): Controllable mock for testing

Create custom subscriptions by implementing the `SubscriptionSource` trait.

### Composing Reducers

A `Reducer` owns one state transition and the subscriptions that state declares.
`scope`, `for_each` and `presented` compose one under a parent — a sibling
feature, one child per row of a `Keyed` collection, or an optionally-present
child in a `Slot` — and `into_program` closes the stack into a runnable
`Program` for `ProgramRuntime`. Each boundary qualifies the identities its child
produces with its own segment and tears a removed child's runs down, so rows
declaring the same subscription or keying the same command do not collide and no
removal leaks a run.

An `Application` is run through an adapter over the same kernel, so this is a
way to write a program rather than a second runtime. See
[docs/composition.md](docs/composition.md) for when the rewrite is worth it, and
[`examples/dashboard_composed.rs`](examples/dashboard_composed.rs) for
`dashboard.rs` written the other way.

## Examples

Check out the [`examples/`](examples/) directory. They fall into two groups,
and each group is named after what a reader arrives knowing.

### Application structure

Named after the application, because someone choosing a structure knows the
shape of their problem and not yet the name of the API. Read them in order:
each one is the previous one at the next scale.

- [`counter.rs`](examples/counter.rs) - A simple counter with timer and keyboard input
- [`views.rs`](examples/views.rs) - Multiple view states with navigation and conditional subscriptions
- [`dashboard.rs`](examples/dashboard.rs) - Structured state management, with the root owning the wiring
- [`dashboard_composed.rs`](examples/dashboard_composed.rs) - The same application with composed reducers: a keyed collection of child reducers, an optionally-present child, automatic scope application and teardown of removed children

### Framework features

Named after the API item they demonstrate, because that is what a reader
arrives looking for. `http_todo.rs` carries both, as a feature that needs a
real application to be worth showing.

- [`panic_hook.rs`](examples/panic_hook.rs) - Restoring the terminal on panic with `install_panic_hook`
- [`signals.rs`](examples/signals.rs) - OS signal handling with graceful shutdown (SIGINT, SIGTERM, etc.)
- [`command_timeout_retry.rs`](examples/command_timeout_retry.rs) - Enforcing a `Command` deadline with `timeout` and recovering from failures with `retry`
- [`command_cancellation.rs`](examples/command_cancellation.rs) - Cancelling superseded in-flight commands with `cancellable`/`cancellable_with` and `CancelPolicy`
- [`websocket.rs`](examples/websocket.rs) - WebSocket echo chat demonstrating real-time communication (requires `ws` feature)
- [`http_todo.rs`](examples/http_todo.rs) - HTTP Todo list with Query subscription, Mutation, and cache management (requires `http` feature)

`RetryError`/`RetryPolicy` and `CommandId`/`CancelPolicy` are imported explicitly
from `tears::command` rather than from the crate root or prelude.

Run an example:

```bash
cargo run --example counter
cargo run --example views
cargo run --example dashboard
cargo run --example dashboard_composed
cargo run --example panic_hook
cargo run --example signals
cargo run --example command_timeout_retry
cargo run --example command_cancellation
cargo run --example websocket --features ws,rustls
cargo run --example http_todo --features http
```

## Testing Your Application

`tears::testing::TestStore` drives an `Application`'s `update` transitions and
command effects synchronously and deterministically, with no wall-clock waiting.
A test constructs the store from the application's flags, scripts messages with
`send`, moves virtual time with `advance`, asserts effect output with
`receive`/`receive_matching`/`receive_quit`, and closes the run with `finish`
(which fails the test if any deliverable output or unfinished effect is left
unaccounted for). Assertions are exhaustive by design.

```rust,ignore
use tears::testing::TestStore;

let mut store = TestStore::<App>::new(flags);
store.send(some_message);
store.advance(Duration::from_millis(200)); // move a Command::timeout deadline
store.receive_matching(|msg| matches!(msg, Message::Loaded(_)));
store.finish();
```

`TestStore` is constructed on a plain `#[test]` (never `#[tokio::test]`; it owns
its own paused time context) and does not execute subscription sources — it
observes only the *declared* set via `subscription_ids`. See the
[`tears::testing`](https://docs.rs/tears/latest/tears/testing/) module docs for
the full contract, including deterministic time without `TestStore`. Worked,
runnable tests ship with these examples:

```bash
cargo test --example command_timeout_retry
cargo test --example command_cancellation
cargo test --example dashboard
```

A composed `Program` is driven with `tears::testing::TestDriver` rather than
`TestStore`, which takes an `Application`; `examples/dashboard_composed.rs`
carries worked `TestDriver` tests:

```bash
cargo test --example dashboard_composed
```

Repository-wide test conventions live in [docs/testing.md](docs/testing.md).

## Optional Features

Tears supports optional features that can be enabled in your `Cargo.toml`:

### WebSocket Support

```toml
[dependencies]
tears = { version = "0.11", features = ["ws", "rustls"] }
```

- **`ws`**: Enables WebSocket subscription support
- **TLS backends** (choose one for `wss://` support):
  - `native-tls` - Platform's native TLS
  - `rustls` - Pure Rust TLS with native certificates
  - `rustls-tls-webpki-roots` - Pure Rust TLS with webpki certificates

### HTTP Support

```toml
[dependencies]
tears = { version = "0.11", features = ["http"] }
```

- **`http`**: Enables HTTP Query and Mutation support
  - `Query` subscription for automatic data fetching with caching
  - `Mutation` for data modifications (POST, PUT, PATCH, DELETE)
  - `QueryClient` for cache management and invalidation
  - Design rationale and invariants: [RFC 0001: `http` Module Redesign](docs/rfcs/0001-http-module-redesign.md)

## Inspiration & Design Philosophy

Tears is inspired by battle-tested architectures:

- **[Elm](https://elm-lang.org/)**: The original Elm Architecture
- **[iced](https://github.com/iced-rs/iced)**: Rust GUI framework (v0.12 design)
- **[Bubble Tea](https://github.com/charmbracelet/bubbletea)**: Go TUI framework with TEA

The framework is designed with these principles:

- **Simplicity First**: Minimal and easy-to-understand API
- **Thin Framework**: Minimal abstraction over ratatui - you have full control
- **Type Safety**: Leverage Rust's type system for correctness

## Minimum Supported Rust Version (MSRV)

Tears requires Rust 1.88.0 or later (uses edition 2024).

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for details.

## Contributing

Contributions are welcome! Please feel free to submit issues or pull requests.

Design contracts and invariants live in [docs/rfcs](docs/rfcs/README.md).
If you are writing or amending an RFC, run the
[pre-review checklist](docs/rfcs/pre-review-checklist.md) before
requesting review. Testing conventions are documented in
[docs/testing.md](docs/testing.md), and the release procedure in
[docs/releasing.md](docs/releasing.md).

An edit is not finished until the changed file's own documentation has been
read again — the module doc, every doc comment under it, the comments in the
body — and any disagreement with the code fixed on one side or the other,
explicitly.

---

Built with ❤️ using [ratatui](https://ratatui.rs/)
