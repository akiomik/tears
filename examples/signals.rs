//! Example demonstrating signal handling with tears.
//!
//! This example shows how to handle OS signals (SIGINT, SIGTERM on Unix, or Ctrl+C on Windows)
//! gracefully in a TUI application. The application will display which signals it receives
//! and allow clean shutdown.
//!
//! # Running the example
//!
//! ```bash
//! cargo run --example signals
//! ```
//!
//! Then try:
//! - Press Ctrl+C to send SIGINT
//! - Send SIGTERM from another terminal: `kill -TERM <pid>`
//! - Press 'q' to quit normally

use std::io;
#[cfg(unix)]
use std::process;

use color_eyre::eyre::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
};

use tears::{prelude::*, subscription::terminal::TerminalEvents};

#[derive(Debug)]
enum Message {
    // Terminal events
    TerminalEvent(Event),
    TerminalError(io::Error),
    // Signal events
    #[cfg(unix)]
    SignalInterrupt,
    #[cfg(unix)]
    SignalTerminate,
    #[cfg(unix)]
    SignalHangup,
    #[cfg(unix)]
    SignalError(io::Error),
    #[cfg(windows)]
    CtrlC,
    #[cfg(windows)]
    CtrlBreak,
    #[cfg(windows)]
    SignalError(io::Error),
}

struct App {
    should_quit: bool,
    signal_log: Vec<String>,
}

impl Application for App {
    type Message = Message;
    type Flags = ();

    fn new(_flags: ()) -> (Self, Command<Message>) {
        (
            Self {
                should_quit: false,
                signal_log: vec!["Application started".to_string()],
            },
            Command::none(),
        )
    }

    fn update(&mut self, msg: Message) -> Command<Message> {
        match msg {
            Message::TerminalEvent(Event::Key(KeyEvent {
                code, modifiers, ..
            })) => {
                match code {
                    KeyCode::Char('q') | KeyCode::Esc => {
                        self.signal_log
                            .push("User requested quit (q/Esc)".to_string());
                        self.should_quit = true;
                    }
                    KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                        // Ctrl+C is captured by terminal events in raw mode
                        // We need to treat it as quit here since signal won't receive it
                        self.signal_log
                            .push("Received Ctrl+C (via terminal event)".to_string());
                        self.should_quit = true;
                    }
                    KeyCode::Char('c') => {
                        self.signal_log.clear();
                    }
                    _ => {}
                }
            }
            Message::TerminalEvent(_) => {}
            Message::TerminalError(e) => {
                self.signal_log.push(format!("Terminal error: {e}"));
                self.should_quit = true;
            }
            #[cfg(unix)]
            Message::SignalInterrupt => {
                self.signal_log.push("Received SIGINT".to_string());
                self.should_quit = true;
            }
            #[cfg(unix)]
            Message::SignalTerminate => {
                self.signal_log.push("Received SIGTERM".to_string());
                self.should_quit = true;
            }
            #[cfg(unix)]
            Message::SignalHangup => {
                self.signal_log.push("Received SIGHUP".to_string());
                // Don't quit on SIGHUP, just log it
            }
            #[cfg(unix)]
            Message::SignalError(e) => {
                self.signal_log.push(format!("Signal error: {e}"));
                // Don't quit on signal initialization errors, just log them
            }
            #[cfg(windows)]
            Message::CtrlC => {
                self.signal_log.push("Received Ctrl+C".to_string());
                self.should_quit = true;
            }
            #[cfg(windows)]
            Message::CtrlBreak => {
                self.signal_log.push("Received Ctrl+Break".to_string());
                self.should_quit = true;
            }
            #[cfg(windows)]
            Message::SignalError(e) => {
                self.signal_log.push(format!("Signal error: {e}"));
                // Don't quit on signal initialization errors, just log them
            }
        }

        if self.should_quit {
            Command::effect(Action::Quit)
        } else {
            Command::none()
        }
    }

    fn view(&self, frame: &mut Frame) {
        let area = frame.area();
        let chunks = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(3),
        ])
        .split(area);

        // Title
        Self::render_title(frame, chunks[0]);

        // Signal log
        self.render_signal_log(frame, chunks[1]);

        // Instructions
        Self::render_instructions(frame, chunks[2]);
    }

    fn subscriptions(&self) -> Vec<Subscription<Message>> {
        let mut subs = vec![
            // Terminal events
            Subscription::new(TerminalEvents::new()).map(|result| match result {
                Ok(event) => Message::TerminalEvent(event),
                Err(e) => Message::TerminalError(e),
            }),
        ];

        // Platform-specific signal subscriptions
        #[cfg(unix)]
        {
            use tears::subscription::signal::Signal;
            use tokio::signal::unix::SignalKind;

            subs.push(
                Subscription::new(Signal::new(SignalKind::interrupt())).map(
                    |result| match result {
                        Ok(()) => Message::SignalInterrupt,
                        Err(e) => Message::SignalError(e),
                    },
                ),
            );

            subs.push(
                Subscription::new(Signal::new(SignalKind::terminate())).map(
                    |result| match result {
                        Ok(()) => Message::SignalTerminate,
                        Err(e) => Message::SignalError(e),
                    },
                ),
            );

            subs.push(Subscription::new(Signal::new(SignalKind::hangup())).map(
                |result| match result {
                    Ok(()) => Message::SignalHangup,
                    Err(e) => Message::SignalError(e),
                },
            ));
        }

        #[cfg(windows)]
        {
            use tears::subscription::signal::{CtrlBreak, CtrlC};

            subs.push(Subscription::new(CtrlC::new()).map(|result| match result {
                Ok(()) => Message::CtrlC,
                Err(e) => Message::SignalError(e),
            }));

            subs.push(
                Subscription::new(CtrlBreak::new()).map(|result| match result {
                    Ok(()) => Message::CtrlBreak,
                    Err(e) => Message::SignalError(e),
                }),
            );
        }

        subs
    }
}

impl App {
    fn render_title(frame: &mut Frame, area: Rect) {
        let title_text = format!("Signal Handling Example (PID: {})", std::process::id());
        let title = Paragraph::new(title_text)
            .style(Style::default().fg(Color::Cyan))
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(title, area);
    }

    fn render_signal_log(&self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .signal_log
            .iter()
            .rev()
            .take(20)
            .map(|msg| {
                ListItem::new(Line::from(vec![
                    Span::raw("• "),
                    Span::styled(msg, Style::default().fg(Color::Yellow)),
                ]))
            })
            .collect();

        let list = List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Signal Log (most recent first)"),
        );

        frame.render_widget(list, area);
    }

    fn render_instructions(frame: &mut Frame, area: Rect) {
        #[cfg(unix)]
        let text = format!(
            "Press 'q' or Esc to quit | 'c' to clear log | Ctrl+C or: kill -TERM {} | kill -HUP {}",
            process::id(),
            process::id()
        );

        #[cfg(windows)]
        let text =
            "Press 'q' or Esc to quit | 'c' to clear log | Try: Ctrl+C, Ctrl+Break".to_string();

        let instructions = Paragraph::new(text)
            .style(Style::default().fg(Color::Gray))
            .block(Block::default().borders(Borders::ALL));

        frame.render_widget(instructions, area);
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    #[cfg(unix)]
    {
        println!("Starting signal handling example (Unix)");
        println!("PID: {}", process::id());
        println!("You can test signals from another terminal:");
        println!("  kill -INT {}  (same as Ctrl+C)", process::id());
        println!("  kill -TERM {}", process::id());
        println!("  kill -HUP {}", process::id());
        println!();
    }

    #[cfg(windows)]
    {
        println!("Starting signal handling example (Windows)");
        println!("Try pressing Ctrl+C or Ctrl+Break");
        println!();
    }

    let runtime = Runtime::<App>::new(());

    // Setup terminal
    let mut terminal = ratatui::init();

    // Run the application at 60 FPS
    let result = runtime.run(&mut terminal, 60).await;

    // Restore terminal
    ratatui::restore();

    println!("Application shut down gracefully.");
    result
}
