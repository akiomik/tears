# tears

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

## Quick Start

Here's a simple counter application that increments every second:

```rust
use std::io;
use color_eyre::eyre::Result;
use crossterm::event::{Event, KeyCode};
use crossterm::{event, terminal};
use ratatui::prelude::CrosstermBackend;
use ratatui::text::Text;
use ratatui::{Frame, Terminal};
use tears::prelude::*;
use tears::subscription::{terminal::TerminalEvents, time::{Message as TimerMessage, Timer}};

#[derive(Debug, Clone)]
enum Message {
    Timer(TimerMessage),
    Terminal(Event),
}

#[derive(Debug, Clone, Default)]
struct Counter {
    count: u32,
}

impl Application for Counter {
    type Message = Message;
    type Flags = ();

    fn new(_flags: Self::Flags) -> (Self, Command<Self::Message>) {
        (Self::default(), Command::none())
    }

    fn update(&mut self, msg: Self::Message) -> Command<Self::Message> {
        match msg {
            Message::Timer(TimerMessage::Tick) => {
                self.count += 1;
                Command::none()
            }
            Message::Terminal(Event::Key(key)) => {
                if key.code == KeyCode::Char('q') {
                    return Command::effect(Action::Quit);
                }
                Command::none()
            }
            _ => Command::none(),
        }
    }

    fn view(&self, frame: &mut Frame<'_>) {
        let widget = Text::raw(format!("Count: {}", self.count));
        frame.render_widget(widget, frame.area());
    }

    fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
        vec![
            Subscription::new(Timer::new(1000)).map(Message::Timer),
            Subscription::new(TerminalEvents::new()).map(Message::Terminal),
        ]
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let runtime = Runtime::<Counter>::new(());

    // Setup terminal
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        terminal::EnterAlternateScreen,
        event::EnableMouseCapture
    )?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = runtime.run(&mut terminal, 16).await;

    // Restore terminal
    terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        terminal::LeaveAlternateScreen,
        event::DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    result
}
```

## Architecture

Tears follows **The Elm Architecture (TEA)** pattern:

```
┌──────────────────────────────────────────────┐
│                                              │
│  ┌─────────┐      ┌────────┐      ┌──────┐ │
│  │ Model   │─────▶│ View   │─────▶│  UI  │ │
│  └─────────┘      └────────┘      └──────┘ │
│       ▲                                      │
│       │                                      │
│  ┌────┴─────┐     ┌──────────────┐          │
│  │  Update  │◀────│   Messages   │          │
│  └──────────┘     └──────────────┘          │
│       ▲                  ▲                   │
│       │                  │                   │
│  ┌────┴─────┐      ┌─────┴──────┐           │
│  │ Commands │      │Subscriptions│          │
│  └──────────┘      └────────────┘           │
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

## Examples

Check out the [`examples/`](examples/) directory for more examples:

- [`counter.rs`](examples/counter.rs) - A simple counter with timer and keyboard input

Run an example:

```bash
cargo run --example counter
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
