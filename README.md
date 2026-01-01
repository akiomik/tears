# tears

[![Crates.io](https://img.shields.io/crates/v/tears.svg)](https://crates.io/crates/tears)
[![Documentation](https://docs.rs/tears/badge.svg)](https://docs.rs/tears)
[![CI](https://github.com/akiomik/tears/workflows/CI/badge.svg)](https://github.com/akiomik/tears/actions/workflows/ci.yml)
[![License](https://img.shields.io/crates/l/tears.svg)](LICENSE)
[![Rust Version](https://img.shields.io/badge/rust-1.85.0%2B-blue.svg)](https://www.rust-lang.org)
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
tears = "0.1"
ratatui = "0.29"
crossterm = "0.28"
tokio = { version = "1", features = ["full"] }
```

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

To run your application, create a `Runtime` and call `run()`:

```rust
#[tokio::main]
async fn main() -> Result<()> {
    let runtime = Runtime::<App>::new(());

    // Setup terminal (see complete example below)
    // ...

    runtime.run(&mut terminal, 60).await?;
    Ok(())
}
```

### Complete Example

Here's a simple counter application that increments every second:

```rust
use color_eyre::eyre::Result;
use crossterm::event::{Event, KeyCode};
use ratatui::{Frame, text::Text};
use tears::prelude::*;
use tears::subscription::{terminal::TerminalEvents, time::{Message as TimerMessage, Timer}};

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
                Command::effect(Action::Quit)
            }
            Message::InputError(e) => {
                eprintln!("Input error: {e}");
                Command::effect(Action::Quit)
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
            Subscription::new(Timer::new(1000)).map(|timer_msg| {
                match timer_msg {
                    TimerMessage::Tick => Message::Tick,
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

    let runtime = Runtime::<Counter>::new(());

    // Setup terminal
    let mut terminal = ratatui::init();

    // Run application at 60 FPS
    let result = runtime.run(&mut terminal, 60).await;

    // Restore terminal
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

Tears provides several built-in subscription sources:

- **Terminal Events** (`subscription::terminal::TerminalEvents`): Keyboard, mouse, and window resize events
- **Timer** (`subscription::time::Timer`): Periodic tick events at configurable intervals
- **Signal** (Unix: `subscription::signal::Signal`, Windows: `subscription::signal::CtrlC`, `subscription::signal::CtrlBreak`): OS signal handling for graceful shutdown and interrupt handling

You can also create custom subscriptions by implementing the `SubscriptionSource` trait.

## Examples

Check out the [`examples/`](examples/) directory for more examples:

- [`counter.rs`](examples/counter.rs) - A simple counter with timer and keyboard input
- [`views.rs`](examples/views.rs) - Multiple view states with navigation and conditional subscriptions
- [`signals.rs`](examples/signals.rs) - OS signal handling with graceful shutdown (SIGINT, SIGTERM, etc.)

Run an example:

```bash
cargo run --example counter
cargo run --example views
cargo run --example signals
```

## Design Philosophy

Tears is designed with these principles in mind:

1. **Simplicity First**: Keep the API minimal and easy to understand
2. **Thin Framework**: Minimal abstraction over ratatui - you have full control
3. **Proven Patterns**: Based on battle-tested architectures (Elm, iced)
4. **Type Safety**: Leverage Rust's type system for correctness

## Inspiration

This framework is heavily inspired by:

- **[Elm](https://elm-lang.org/)**: The original Elm Architecture
- **[iced](https://github.com/iced-rs/iced)**: Rust GUI framework (v0.12 design)
- **[Bubble Tea](https://github.com/charmbracelet/bubbletea)**: Go TUI framework with TEA

## Minimum Supported Rust Version (MSRV)

Tears requires Rust 1.85.0 or later (uses edition 2024).

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for details.

## Contributing

Contributions are welcome! Please feel free to submit issues or pull requests.

---

Built with ❤️ using [ratatui](https://ratatui.rs/)
