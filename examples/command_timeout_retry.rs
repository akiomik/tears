//! Example demonstrating `Command::timeout` and `Command::retry`.
//!
//! This example shows four kinds of asynchronous outcomes:
//! - A fetch that completes well within its deadline
//! - A fetch that is slower than its deadline and times out
//! - A flaky operation that fails twice and recovers on retry
//! - A flaky operation that keeps failing until the retry policy is exhausted
//!
//! # Running the example
//!
//! ```bash
//! cargo run --example command_timeout_retry
//! ```
//!
//! Then try:
//! - Press 'f' for a fast fetch (completes before the deadline)
//! - Press 's' for a slow fetch (exceeds the deadline and times out)
//! - Press 'r' for a flaky fetch that recovers after two failed attempts
//! - Press 'x' for a flaky fetch that exhausts its retry attempts
//! - Press Ctrl+C to quit

use std::io;
use std::num::{NonZeroU32, NonZeroUsize};
use std::time::Duration;

use color_eyre::eyre::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
};
use tears::command::{RetryContext, RetryError, RetryPolicy};
use tears::prelude::*;
use tears::subscription::terminal::TerminalEvents;
use tokio::time::sleep;

#[derive(Debug)]
enum Message {
    Input(Event),
    InputError(io::Error),
    Loaded(String),
    TimedOut,
    RetryFinished(Result<String, RetryError<String>>),
}

struct App {
    should_quit: bool,
    log: Vec<String>,
}

impl Application for App {
    type Message = Message;
    type Flags = ();

    fn new(_flags: ()) -> (Self, Command<Message>) {
        (
            Self {
                should_quit: false,
                log: vec!["Ready".to_string()],
            },
            Command::none(),
        )
    }

    fn update(&mut self, msg: Message) -> Command<Message> {
        match msg {
            Message::Input(Event::Key(KeyEvent {
                code, modifiers, ..
            })) => match code {
                KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                    self.should_quit = true;
                    return Command::quit();
                }
                KeyCode::Char('f') => {
                    self.log
                        .push("Fast fetch: started (1s deadline)".to_string());
                    return Command::perform(
                        fetch(Duration::from_millis(200), "fast data"),
                        Message::Loaded,
                    )
                    .timeout(Duration::from_secs(1), || Message::TimedOut);
                }
                KeyCode::Char('s') => {
                    self.log
                        .push("Slow fetch: started (800ms deadline)".to_string());
                    return Command::perform(
                        fetch(Duration::from_secs(3), "slow data"),
                        Message::Loaded,
                    )
                    .timeout(Duration::from_millis(800), || Message::TimedOut);
                }
                KeyCode::Char('r') => {
                    self.log
                        .push("Recovering fetch: started (3 attempts)".to_string());
                    let policy = RetryPolicy::new(NonZeroUsize::new(3).expect("non-zero"))
                        .with_fixed_backoff(Duration::from_millis(300));
                    return Command::retry(policy, recovering_fetch, Message::RetryFinished);
                }
                KeyCode::Char('x') => {
                    self.log
                        .push("Exhausting fetch: started (2 attempts)".to_string());
                    let policy = RetryPolicy::new(NonZeroUsize::new(2).expect("non-zero"))
                        .with_fixed_backoff(Duration::from_millis(300));
                    return Command::retry(policy, always_failing_fetch, Message::RetryFinished);
                }
                _ => {}
            },
            Message::Input(_) => {}
            Message::InputError(e) => {
                self.log.push(format!("Terminal error: {e}"));
                self.should_quit = true;
            }
            Message::Loaded(data) => {
                self.log.push(format!("Loaded: {data}"));
            }
            Message::TimedOut => {
                self.log.push("Timed out before the deadline".to_string());
            }
            Message::RetryFinished(Ok(data)) => {
                self.log.push(format!("Retry succeeded: {data}"));
            }
            Message::RetryFinished(Err(e)) => {
                self.log.push(format!(
                    "Retry gave up after {} attempt(s): {}",
                    e.attempts(),
                    e.into_last_error()
                ));
            }
        }

        if self.should_quit {
            Command::quit()
        } else {
            Command::none()
        }
    }

    fn view(&self, frame: &mut Frame) {
        let chunks = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(3),
        ])
        .split(frame.area());

        let title = Paragraph::new("Command Timeout & Retry Example")
            .style(Style::default().fg(Color::Cyan))
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(title, chunks[0]);

        self.render_log(frame, chunks[1]);

        let instructions = Paragraph::new(
            "f: fast fetch | s: slow fetch (times out) | r: retry (recovers) | x: retry (exhausts) | Ctrl+C: quit",
        )
        .style(Style::default().fg(Color::Gray))
        .block(Block::default().borders(Borders::ALL));
        frame.render_widget(instructions, chunks[2]);
    }

    fn subscriptions(&self) -> Vec<Subscription<Message>> {
        vec![
            Subscription::new(TerminalEvents::new()).map(|result| match result {
                Ok(event) => Message::Input(event),
                Err(e) => Message::InputError(e),
            }),
        ]
    }
}

impl App {
    fn render_log(&self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .log
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
                .title("Log (most recent first)"),
        );

        frame.render_widget(list, area);
    }
}

async fn fetch(delay: Duration, data: &'static str) -> String {
    sleep(delay).await;
    data.to_string()
}

async fn recovering_fetch(ctx: RetryContext) -> Result<String, String> {
    sleep(Duration::from_millis(200)).await;
    if ctx.attempt().get() < 3 {
        Err(format!("attempt {} failed", ctx.attempt()))
    } else {
        Ok(format!("recovered on attempt {}", ctx.attempt()))
    }
}

async fn always_failing_fetch(ctx: RetryContext) -> Result<String, String> {
    sleep(Duration::from_millis(200)).await;
    Err(format!("attempt {} failed", ctx.attempt()))
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    let mut terminal = ratatui::init();

    let frame_rate = FrameRate::new(NonZeroU32::new(60).expect("non-zero"))?;
    let runtime = Runtime::<App>::new((), frame_rate);
    let result = runtime.run(&mut terminal).await;

    ratatui::restore();

    result?;

    Ok(())
}
