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
tears = "0.9"
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
use std::num::NonZeroU32;

#[tokio::main]
async fn main() -> Result<()> {
    let frame_rate = FrameRate::new(NonZeroU32::new(60).expect("non-zero"))?;
    let runtime = Runtime::<App>::new((), frame_rate);

    // Setup terminal (see complete example below)
    // ...

    runtime.run(&mut terminal).await?;
    Ok(())
}
```

### Complete Example

Here's a simple counter application that increments every second:

```rust
use std::num::{NonZeroU32, NonZeroU64};

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

    // Run application at 60 FPS
    let frame_rate = FrameRate::new(NonZeroU32::new(60).expect("non-zero"))?;
    let runtime = Runtime::<Counter>::new((), frame_rate);
    let result = runtime.run(&mut terminal).await;

    // Restore terminal (normal exit path)
    ratatui::restore();

    result
}
```

### Command timeouts and retries

Commands can enforce an overall deadline without changing their message type:

```rust
use std::time::Duration;
use tears::prelude::*;

enum Message {
    Loaded(String),
    TimedOut,
}

let command = Command::perform(async { "data".to_string() }, Message::Loaded)
    .timeout(Duration::from_secs(5), || Message::TimedOut);
```

Retrying commands take a factory so every attempt receives a fresh future.
Retry support types are imported explicitly from `tears::command` rather than
from the crate root or prelude:

```rust
use std::num::NonZeroUsize;
use std::time::Duration;
use tears::Command;
use tears::command::{RetryError, RetryPolicy};

enum Message {
    Loaded(Result<String, RetryError<&'static str>>),
}

let policy = RetryPolicy::new(NonZeroUsize::new(3).expect("non-zero"))
    .with_fixed_backoff(Duration::from_millis(200));

let command = Command::retry(
    policy,
    |_| async { Ok::<_, &'static str>("data".to_string()) },
    Message::Loaded,
);
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

## Examples

Check out the [`examples/`](examples/) directory for more examples:

- [`counter.rs`](examples/counter.rs) - A simple counter with timer and keyboard input
- [`panic_hook.rs`](examples/panic_hook.rs) - Restoring the terminal on panic with `install_panic_hook`
- [`views.rs`](examples/views.rs) - Multiple view states with navigation and conditional subscriptions
- [`dashboard.rs`](examples/dashboard.rs) - Structured state management with nested state and child messages
- [`signals.rs`](examples/signals.rs) - OS signal handling with graceful shutdown (SIGINT, SIGTERM, etc.)
- [`websocket.rs`](examples/websocket.rs) - WebSocket echo chat demonstrating real-time communication (requires `ws` feature)
- [`http_todo.rs`](examples/http_todo.rs) - HTTP Todo list with Query subscription, Mutation, and cache management (requires `http` feature)

Run an example:

```bash
cargo run --example counter
cargo run --example panic_hook
cargo run --example views
cargo run --example dashboard
cargo run --example signals
cargo run --example websocket --features ws,rustls
cargo run --example http_todo --features http
```

## Optional Features

Tears supports optional features that can be enabled in your `Cargo.toml`:

### WebSocket Support

```toml
[dependencies]
tears = { version = "0.8", features = ["ws", "rustls"] }
```

- **`ws`**: Enables WebSocket subscription support
- **TLS backends** (choose one for `wss://` support):
  - `native-tls` - Platform's native TLS
  - `rustls` - Pure Rust TLS with native certificates
  - `rustls-tls-webpki-roots` - Pure Rust TLS with webpki certificates

### HTTP Support

```toml
[dependencies]
tears = { version = "0.8", features = ["http"] }
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

---

Built with ❤️ using [ratatui](https://ratatui.rs/)
