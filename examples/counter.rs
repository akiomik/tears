//! A simple counter example demonstrating the Elm Architecture with tears.
//!
//! This example shows:
//! - Timer subscription (increments counter every second)
//! - Terminal event subscription (keyboard input)
//! - Error handling for terminal events
//! - Basic rendering with ratatui
//!
//! Run with: cargo run --example counter

use color_eyre::eyre::Result;
use crossterm::event::{Event, KeyCode};
use ratatui::Frame;
use ratatui::text::Text;
use tears::prelude::*;
use tears::subscription::{
    terminal::TerminalEvents,
    time::{Message as TimerMessage, Timer},
};

/// Messages that the application can receive
#[derive(Debug, Clone)]
enum Message {
    /// Timer tick message (sent every second)
    Timer(TimerMessage),
    /// Terminal input event (keyboard, mouse, resize)
    Terminal(crossterm::event::Event),
    /// Terminal event stream error
    TerminalError(String),
}

/// Application state
#[derive(Debug, Clone, Default)]
struct Counter {
    count: u32,
}

impl Application for Counter {
    type Message = Message;
    type Flags = ();

    /// Initialize the application
    fn new(_flags: Self::Flags) -> (Self, Command<Self::Message>) {
        (Self::default(), Command::none())
    }

    /// Handle incoming messages and update state
    fn update(&mut self, msg: Self::Message) -> Command<Self::Message> {
        match msg {
            Message::Timer(TimerMessage::Tick) => {
                // Increment counter on each timer tick
                self.count += 1;
                Command::none()
            }
            Message::Terminal(event) => {
                // Handle keyboard events
                if let Event::Key(key) = event {
                    if key.code == KeyCode::Char('q') {
                        // Quit on 'q' key press
                        return Command::effect(Action::Quit);
                    }
                }
                Command::none()
            }
            Message::TerminalError(e) => {
                // Handle terminal event stream errors
                // In this example, we log the error and quit
                eprintln!("Terminal error: {e}");
                Command::effect(Action::Quit)
            }
        }
    }

    /// Render the UI
    fn view(&self, frame: &mut Frame<'_>) {
        let text = Text::raw(format!("Count: {} (Press 'q' to quit)", self.count));
        frame.render_widget(text, frame.area());
    }

    /// Subscribe to events (timer and keyboard input)
    fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
        vec![
            // Timer that ticks every 1000ms (1 second)
            Subscription::new(Timer::new(1000)).map(Message::Timer),
            // Terminal events (keyboard, mouse, resize)
            // Note: Returns Result to handle potential I/O errors
            Subscription::new(TerminalEvents::new()).map(|result| match result {
                Ok(event) => Message::Terminal(event),
                Err(e) => Message::TerminalError(e.to_string()),
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

    // Run the application at 60 FPS
    let result = runtime.run(&mut terminal, 60).await;

    // Restore terminal
    ratatui::restore();

    result
}
