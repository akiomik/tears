//! Example demonstrating `Command::timeout` and `Command::retry`.
//!
//! This example shows several asynchronous outcomes:
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
use std::num::NonZeroUsize;
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
                log: vec!["Ready".to_owned()],
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
                        .push("Fast fetch: started (1s deadline)".to_owned());
                    return Command::perform(
                        fetch(Duration::from_millis(200), "fast data"),
                        Message::Loaded,
                    )
                    .timeout(Duration::from_secs(1), || Message::TimedOut)
                    .into();
                }
                KeyCode::Char('s') => {
                    self.log
                        .push("Slow fetch: started (800ms deadline)".to_owned());
                    return Command::perform(
                        fetch(Duration::from_secs(3), "slow data"),
                        Message::Loaded,
                    )
                    .timeout(Duration::from_millis(800), || Message::TimedOut)
                    .into();
                }
                KeyCode::Char('r') => {
                    self.log
                        .push("Recovering fetch: started (3 attempts)".to_owned());
                    let policy = RetryPolicy::new(NonZeroUsize::new(3).expect("non-zero"))
                        .with_fixed_backoff(Duration::from_millis(300));
                    return Command::retry(policy, recovering_fetch, Message::RetryFinished).into();
                }
                KeyCode::Char('x') => {
                    self.log
                        .push("Exhausting fetch: started (2 attempts)".to_owned());
                    let policy = RetryPolicy::new(NonZeroUsize::new(2).expect("non-zero"))
                        .with_fixed_backoff(Duration::from_millis(300));
                    return Command::retry(policy, always_failing_fetch, Message::RetryFinished)
                        .into();
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
                self.log.push("Timed out before the deadline".to_owned());
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
    data.to_owned()
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

    let runtime = Runtime::<App>::new(());
    let result = runtime.run(&mut terminal).await;

    ratatui::restore();

    result?;

    Ok(())
}

/// Deterministic tests for this example's timeout and retry logic, driven by
/// `tears::testing::TestStore` (RFC 0008). Every time-dependent command effect
/// is moved forward with `advance`, so nothing waits on the wall clock; run
/// them with `cargo test --example command_timeout_retry`.
#[cfg(test)]
mod tests {
    use super::*;
    use tears::testing::TestStore;

    /// Builds the `Input` message a keystroke would produce, so a test can
    /// script the same commands the terminal subscription feeds at runtime.
    fn press(c: char) -> Message {
        Message::Input(Event::Key(KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::empty(),
        )))
    }

    /// A fetch that finishes before its deadline delivers its data, and the
    /// timeout leaf that guarded it is driven to completion with nothing left
    /// over.
    #[test]
    fn fast_fetch_completes_before_deadline() {
        let mut store = TestStore::<App>::new(());

        // 'f' starts `perform(fetch(200ms)).timeout(1s)`. The fetch is pending
        // until virtual time reaches its 200ms sleep.
        store.send(press('f'));
        store.advance(Duration::from_millis(200));

        store.receive_matching(|msg| matches!(msg, Message::Loaded(data) if data == "fast data"));
        assert_eq!(store.state().log.last().unwrap(), "Loaded: fast data");
        store.finish();
    }

    /// A fetch slower than its deadline delivers the timeout message and never
    /// the (still-sleeping) data.
    #[test]
    fn slow_fetch_times_out() {
        let mut store = TestStore::<App>::new(());

        // 's' starts `perform(fetch(3s)).timeout(800ms)`. Advancing to the
        // deadline fires the timeout before the inner fetch can finish.
        store.send(press('s'));
        store.advance(Duration::from_millis(800));

        store.receive_matching(|msg| matches!(msg, Message::TimedOut));
        assert_eq!(
            store.state().log.last().unwrap(),
            "Timed out before the deadline"
        );
        store.finish();
    }

    /// A flaky operation that fails twice then succeeds recovers on its third
    /// attempt. Each 200ms attempt and each 300ms fixed backoff is a separate
    /// virtual-time wait the retry leaf registers only after the previous one
    /// is observed, so the test crosses them one `advance` at a time.
    #[test]
    fn retry_recovers_after_two_failures() {
        let mut store = TestStore::<App>::new(());

        store.send(press('r'));
        store.advance(Duration::from_millis(200)); // attempt 1 fails
        store.advance(Duration::from_millis(300)); // backoff before attempt 2
        store.advance(Duration::from_millis(200)); // attempt 2 fails
        store.advance(Duration::from_millis(300)); // backoff before attempt 3
        store.advance(Duration::from_millis(200)); // attempt 3 succeeds

        store.receive_matching(|msg| {
            matches!(msg, Message::RetryFinished(Ok(data)) if data.contains("recovered on attempt 3"))
        });
        store.finish();
    }

    /// A retry policy that runs out of attempts delivers the final error, and
    /// the reported attempt count matches the policy's limit.
    #[test]
    fn retry_gives_up_after_exhausting_attempts() {
        let mut store = TestStore::<App>::new(());

        store.send(press('x'));
        store.advance(Duration::from_millis(200)); // attempt 1 fails
        store.advance(Duration::from_millis(300)); // backoff before attempt 2
        store.advance(Duration::from_millis(200)); // attempt 2 fails, none left

        store.receive_matching(|msg| matches!(msg, Message::RetryFinished(Err(_))));
        assert!(
            store
                .state()
                .log
                .last()
                .unwrap()
                .starts_with("Retry gave up after 2 attempt(s)")
        );
        store.finish();
    }
}
