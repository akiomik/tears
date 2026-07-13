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
use std::num::NonZeroU32;
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

    let frame_rate = FrameRate::new(NonZeroU32::new(60).expect("non-zero"))?;
    let runtime = Runtime::<App>::new((), frame_rate);
    let result = runtime.run(&mut terminal).await;

    ratatui::restore();

    result?;

    Ok(())
}
