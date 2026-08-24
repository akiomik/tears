//! Example demonstrating keyed command cancellation.
//!
//! Every keystroke starts a new "search" command tied to the same
//! [`CommandId`], so the runtime can decide what to do with the previous
//! in-flight search:
//! - [`CancelPolicy::CancelInFlight`] (the default) aborts it and starts the
//!   new one, so only the most recently typed query ever produces a result
//! - [`CancelPolicy::KeepInFlight`] lets the current search finish and drops
//!   the new command instead, so the results can lag behind what's typed
//!
//! # Running the example
//!
//! ```bash
//! cargo run --example command_cancellation
//! ```
//!
//! Then try:
//! - Type letters to search (results arrive after a simulated 400ms delay)
//! - Type quickly and compare how results settle under each policy
//! - Press Tab to switch between `CancelInFlight` and `KeepInFlight`
//! - Press Esc to clear the query and explicitly cancel any in-flight search
//! - Press Backspace to edit the query
//! - Press Ctrl+C to quit

use std::io;
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
use tears::command::{CancelPolicy, CommandId};
use tears::prelude::*;
use tears::subscription::terminal::TerminalEvents;
use tokio::time::sleep;

const WORDS: &[&str] = &[
    "rust",
    "ratatui",
    "tokio",
    "elm",
    "tears",
    "cancel",
    "command",
    "runtime",
    "subscription",
    "message",
];

#[derive(Debug)]
enum Message {
    Input(Event),
    InputError(io::Error),
    SearchFinished {
        query: String,
        results: Vec<&'static str>,
    },
}

struct App {
    should_quit: bool,
    query: String,
    policy: CancelPolicy,
    log: Vec<String>,
}

impl Application for App {
    type Message = Message;
    type Flags = ();

    fn new(_flags: ()) -> (Self, Command<Message>) {
        (
            Self {
                should_quit: false,
                query: String::new(),
                policy: CancelPolicy::CancelInFlight,
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
                KeyCode::Tab => {
                    self.policy = match self.policy {
                        CancelPolicy::CancelInFlight => CancelPolicy::KeepInFlight,
                        _ => CancelPolicy::CancelInFlight,
                    };
                    self.log
                        .push(format!("Policy switched to {:?}", self.policy));
                }
                KeyCode::Esc => {
                    self.query.clear();
                    self.log
                        .push("Query cleared, cancelling in-flight search".to_owned());
                    return Command::cancel(search_id());
                }
                KeyCode::Backspace => {
                    self.query.pop();
                    if self.query.is_empty() {
                        self.log
                            .push("Query empty, cancelling in-flight search".to_owned());
                        return Command::cancel(search_id());
                    }
                    return self.search_command();
                }
                KeyCode::Char(c) if !modifiers.contains(KeyModifiers::CONTROL) => {
                    self.query.push(c);
                    return self.search_command();
                }
                _ => {}
            },
            Message::Input(_) => {}
            Message::InputError(e) => {
                self.log.push(format!("Terminal error: {e}"));
                self.should_quit = true;
            }
            Message::SearchFinished { query, results } => {
                self.log.push(format!("Results for '{query}': {results:?}"));
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
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(3),
        ])
        .split(frame.area());

        let title = Paragraph::new("Command Cancellation Example")
            .style(Style::default().fg(Color::Cyan))
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(title, chunks[0]);

        let status = Paragraph::new(format!(
            "Query: {}  |  Policy: {:?}",
            self.query, self.policy
        ))
        .style(Style::default().fg(Color::White))
        .block(Block::default().borders(Borders::ALL));
        frame.render_widget(status, chunks[1]);

        self.render_log(frame, chunks[2]);

        let instructions = Paragraph::new(
            "Type to search | Tab: toggle policy | Esc: clear & cancel | Backspace: edit | Ctrl+C: quit",
        )
        .style(Style::default().fg(Color::Gray))
        .block(Block::default().borders(Borders::ALL));
        frame.render_widget(instructions, chunks[3]);
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
    fn search_command(&mut self) -> Command<Message> {
        let query = self.query.clone();
        self.log
            .push(format!("Searching '{query}' (policy: {:?})", self.policy));

        let message_query = query.clone();
        Command::perform(search(query), move |results| Message::SearchFinished {
            query: message_query,
            results,
        })
        .cancellable_with(search_id(), self.policy)
        .into()
    }

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

fn search_id() -> CommandId {
    CommandId::new("search")
}

async fn search(query: String) -> Vec<&'static str> {
    sleep(Duration::from_millis(400)).await;
    let needle = query.to_lowercase();
    WORDS
        .iter()
        .copied()
        .filter(|word| word.contains(&needle))
        .collect()
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

/// Deterministic tests for this example's keyed-cancellation logic, driven by
/// `tears::testing::TestStore` (RFC 0008). The store honors the same
/// `CancelPolicy` admission the runtime does, so a superseded or explicitly
/// cancelled search produces no output. The 400ms search delay is crossed with
/// `advance`, never the wall clock; run them with
/// `cargo test --example command_cancellation`.
#[cfg(test)]
mod tests {
    use super::*;
    use tears::testing::TestStore;

    const SEARCH_DELAY: Duration = Duration::from_millis(400);

    /// Builds the `Input` message a character keystroke would produce.
    fn press(c: char) -> Message {
        Message::Input(Event::Key(KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::empty(),
        )))
    }

    /// Builds the `Input` message a non-character key would produce.
    fn key(code: KeyCode) -> Message {
        Message::Input(Event::Key(KeyEvent::new(code, KeyModifiers::empty())))
    }

    /// Under the default `CancelInFlight` policy, a second keystroke aborts the
    /// first search, so only the most recently typed query ever delivers a
    /// result and the store finishes with no orphaned effect.
    #[test]
    fn cancel_in_flight_delivers_only_the_latest_query() {
        let mut store = TestStore::<App>::new(());

        store.send(press('r')); // starts a search for "r"
        store.send(press('u')); // supersedes it with a search for "ru"

        store.advance(SEARCH_DELAY);
        store.receive_matching(
            |msg| matches!(msg, Message::SearchFinished { query, .. } if query == "ru"),
        );
        store.finish();
    }

    /// Under `KeepInFlight`, the running search is preserved and the newer
    /// command is dropped, so the delivered result lags behind the typed query.
    #[test]
    fn keep_in_flight_preserves_the_running_search() {
        let mut store = TestStore::<App>::new(());

        store.send(key(KeyCode::Tab)); // switch to KeepInFlight
        assert_eq!(store.state().policy, CancelPolicy::KeepInFlight);

        store.send(press('r')); // starts a search for "r"
        store.send(press('u')); // dropped; the "r" search keeps running

        store.advance(SEARCH_DELAY);
        store.receive_matching(
            |msg| matches!(msg, Message::SearchFinished { query, .. } if query == "r"),
        );
        assert_eq!(store.state().query, "ru");
        store.finish();
    }

    /// Pressing Esc issues an explicit `Command::cancel`, which removes the
    /// in-flight search leaf without polling it, so advancing past the search
    /// delay delivers nothing.
    #[test]
    fn esc_cancels_the_in_flight_search() {
        let mut store = TestStore::<App>::new(());

        store.send(press('r'));
        store.send(key(KeyCode::Esc));
        assert!(store.state().query.is_empty());

        store.advance(SEARCH_DELAY);
        store.finish();
    }
}
