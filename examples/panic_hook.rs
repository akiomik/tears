//! Demonstrates terminal recovery on panic via [`tears::install_panic_hook`].
//!
//! A TUI puts the terminal into raw mode and an alternate screen. If the
//! application panics, the stack unwinds past the `ratatui::restore()` call that
//! would normally run after [`Runtime::run`], leaving the terminal broken.
//!
//! [`tears::install_panic_hook`] wraps the current panic hook so the terminal is
//! restored *before* the original hook runs, so the `color_eyre` report prints
//! on a clean, usable terminal.
//!
//! Try it:
//! - Press `p` to trigger a panic. The terminal is restored and a `color_eyre`
//!   report is printed normally (no leftover raw mode / alternate screen).
//! - Press `q` to quit normally.
//!
//! To see the broken behavior for comparison, comment out the
//! `tears::install_panic_hook()` call in `main` and press `p`: the report is
//! printed into the alternate screen and the terminal is left in raw mode.
//!
//! Run with: `cargo run --example panic_hook`

use std::io;
use std::num::NonZeroU32;

use color_eyre::eyre::Result;
use crossterm::event::{Event, KeyCode};
use ratatui::Frame;
use ratatui::text::Text;
use tears::prelude::*;
use tears::subscription::terminal::TerminalEvents;

/// Messages that the application can receive
#[derive(Debug)]
enum Message {
    /// Terminal input event (keyboard, mouse, resize)
    Terminal(Event),
    /// Terminal event stream error
    TerminalError(io::Error),
}

/// Application state
#[derive(Debug, Default)]
struct PanicDemo;

impl Application for PanicDemo {
    type Message = Message;
    type Flags = ();

    fn new(_flags: Self::Flags) -> (Self, Command<Self::Message>) {
        (Self, Command::none())
    }

    fn update(&mut self, msg: Self::Message) -> Command<Self::Message> {
        match msg {
            Message::Terminal(Event::Key(key)) => match key.code {
                // Intentionally panic to demonstrate terminal recovery.
                #[expect(clippy::panic, reason = "demonstrating panic recovery")]
                KeyCode::Char('p') => panic!("intentional panic triggered by 'p'"),
                // Quit normally.
                KeyCode::Char('q') => Command::quit(),
                _ => Command::none(),
            },
            Message::Terminal(_) => Command::none(),
            Message::TerminalError(e) => {
                eprintln!("Terminal error: {e}");
                Command::quit()
            }
        }
    }

    fn view(&self, frame: &mut Frame<'_>) {
        let text = Text::raw("Press 'p' to panic (terminal should recover), 'q' to quit");
        frame.render_widget(text, frame.area());
    }

    fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
        vec![
            Subscription::new(TerminalEvents::new()).map(|result| match result {
                Ok(event) => Message::Terminal(event),
                Err(e) => Message::TerminalError(e),
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

    // Run the application at 60 FPS
    let frame_rate = FrameRate::new(NonZeroU32::new(60).expect("non-zero"))?;
    let runtime = Runtime::<PanicDemo>::new((), frame_rate);
    let result = runtime.run(&mut terminal).await;

    // Restore terminal (normal exit path)
    ratatui::restore();

    result?;

    Ok(())
}
