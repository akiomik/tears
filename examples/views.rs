//! Multiple view states example demonstrating state management and navigation.
//!
//! This example shows:
//! - State machine pattern for managing different views
//! - Navigation between views using messages
//! - Conditional subscriptions based on current view
//! - Different UI rendering for each view
//!
//! Views:
//! - Menu: Select which view to navigate to
//! - Counter: Auto-incrementing counter with timer
//! - Input: Text input with history
//! - List: Scrollable list of items
//!
//! Run with: cargo run --example views

use color_eyre::eyre::Result;
use crossterm::event::{Event, KeyCode, KeyEvent};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List as ListWidget, ListItem, Paragraph};
use tears::prelude::*;
use tears::subscription::{
    terminal::TerminalEvents,
    time::{Message as TimerMessage, Timer},
};

/// Application views (screens)
#[derive(Debug, Clone)]
enum View {
    /// Main menu
    Menu { selected: usize },
    /// Counter view with auto-increment
    Counter { count: u32 },
    /// Text input view
    Input { text: String, history: Vec<String> },
    /// List view with scrollable items
    List { items: Vec<String>, selected: usize },
}

/// Messages that the application can receive
#[derive(Debug, Clone)]
enum Message {
    // Navigation messages
    GoToMenu,
    GoToCounter,
    GoToInput,
    GoToList,
    Quit,

    // Menu messages
    MenuUp,
    MenuDown,
    MenuSelect,

    // Counter messages
    Tick,

    // Input messages
    InputChar(char),
    InputSubmit,
    InputBackspace,

    // List messages
    ListUp,
    ListDown,

    // Terminal events
    Terminal(Event),
    TerminalError(String),
}

/// Application state
struct App {
    view: View,
}

impl Application for App {
    type Message = Message;
    type Flags = ();

    /// Initialize the application with the menu view
    fn new(_flags: ()) -> (Self, Command<Self::Message>) {
        let app = Self {
            view: View::Menu { selected: 0 },
        };
        (app, Command::none())
    }

    /// Handle incoming messages and update state
    #[allow(clippy::too_many_lines)]
    fn update(&mut self, msg: Message) -> Command<Self::Message> {
        match msg {
            // Navigation
            Message::GoToMenu => {
                self.view = View::Menu { selected: 0 };
                Command::none()
            }
            Message::GoToCounter => {
                self.view = View::Counter { count: 0 };
                Command::none()
            }
            Message::GoToInput => {
                self.view = View::Input {
                    text: String::new(),
                    history: vec![],
                };
                Command::none()
            }
            Message::GoToList => {
                self.view = View::List {
                    items: vec![
                        "Item 1".to_string(),
                        "Item 2".to_string(),
                        "Item 3".to_string(),
                        "Item 4".to_string(),
                        "Item 5".to_string(),
                    ],
                    selected: 0,
                };
                Command::none()
            }
            Message::Quit => Command::effect(Action::Quit),

            // Menu view
            Message::MenuUp => {
                if let View::Menu { selected } = &mut self.view {
                    *selected = selected.saturating_sub(1);
                }
                Command::none()
            }
            Message::MenuDown => {
                if let View::Menu { selected } = &mut self.view {
                    *selected = (*selected + 1).min(3);
                }
                Command::none()
            }
            Message::MenuSelect => {
                if let View::Menu { selected } = self.view {
                    match selected {
                        0 => Command::single(Message::GoToCounter),
                        1 => Command::single(Message::GoToInput),
                        2 => Command::single(Message::GoToList),
                        3 => Command::single(Message::Quit),
                        _ => Command::none(),
                    }
                } else {
                    Command::none()
                }
            }

            // Counter view
            Message::Tick => {
                if let View::Counter { count } = &mut self.view {
                    *count += 1;
                }
                Command::none()
            }

            // Input view
            Message::InputChar(c) => {
                if let View::Input { text, .. } = &mut self.view {
                    text.push(c);
                }
                Command::none()
            }
            Message::InputBackspace => {
                if let View::Input { text, .. } = &mut self.view {
                    text.pop();
                }
                Command::none()
            }
            Message::InputSubmit => {
                if let View::Input { text, history } = &mut self.view {
                    if !text.is_empty() {
                        history.push(text.clone());
                        text.clear();
                    }
                }
                Command::none()
            }

            // List view
            Message::ListUp => {
                if let View::List { selected, .. } = &mut self.view {
                    *selected = selected.saturating_sub(1);
                }
                Command::none()
            }
            Message::ListDown => {
                if let View::List { items, selected } = &mut self.view {
                    *selected = (*selected + 1).min(items.len().saturating_sub(1));
                }
                Command::none()
            }

            // Terminal events
            Message::Terminal(Event::Key(key)) => handle_key_event(&self.view, key),
            Message::Terminal(_) => Command::none(),
            Message::TerminalError(e) => {
                eprintln!("Terminal error: {e}");
                Command::effect(Action::Quit)
            }
        }
    }

    /// Render the UI based on current view
    fn view(&self, frame: &mut Frame) {
        match &self.view {
            View::Menu { selected } => render_menu(frame, *selected),
            View::Counter { count } => render_counter(frame, *count),
            View::Input { text, history } => render_input(frame, text, history),
            View::List { items, selected } => render_list(frame, items, *selected),
        }
    }

    /// Subscribe to events based on current view
    fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
        let mut subs = vec![
            // Always listen to terminal events
            Subscription::new(TerminalEvents::new()).map(|result| match result {
                Ok(event) => Message::Terminal(event),
                Err(e) => Message::TerminalError(e.to_string()),
            }),
        ];

        // Add timer subscription only in Counter view
        if matches!(self.view, View::Counter { .. }) {
            subs.push(
                Subscription::new(Timer::new(1000)).map(|timer_msg| match timer_msg {
                    TimerMessage::Tick => Message::Tick,
                }),
            );
        }

        subs
    }
}

/// Handle keyboard events based on current view
#[allow(clippy::use_self)]
fn handle_key_event(view: &View, key: KeyEvent) -> Command<Message> {
    match view {
        View::Menu { .. } => match key.code {
            KeyCode::Up => Command::single(Message::MenuUp),
            KeyCode::Down => Command::single(Message::MenuDown),
            KeyCode::Enter => Command::single(Message::MenuSelect),
            KeyCode::Char('q') => Command::single(Message::Quit),
            _ => Command::none(),
        },
        View::Counter { .. } => match key.code {
            KeyCode::Char('b') | KeyCode::Esc => Command::single(Message::GoToMenu),
            KeyCode::Char('q') => Command::single(Message::Quit),
            _ => Command::none(),
        },
        View::Input { .. } => match key.code {
            KeyCode::Char(c) => Command::single(Message::InputChar(c)),
            KeyCode::Backspace => Command::single(Message::InputBackspace),
            KeyCode::Enter => Command::single(Message::InputSubmit),
            KeyCode::Esc => Command::single(Message::GoToMenu),
            _ => Command::none(),
        },
        View::List { .. } => match key.code {
            KeyCode::Up => Command::single(Message::ListUp),
            KeyCode::Down => Command::single(Message::ListDown),
            KeyCode::Char('b') | KeyCode::Esc => Command::single(Message::GoToMenu),
            KeyCode::Char('q') => Command::single(Message::Quit),
            _ => Command::none(),
        },
    }
}

/// Render the menu view
fn render_menu(frame: &mut Frame, selected: usize) {
    let area = frame.area();

    let menu_items = ["Counter", "Input", "List", "Quit"];
    let items: Vec<ListItem> = menu_items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let content = if i == selected {
                format!("> {item}")
            } else {
                format!("  {item}")
            };
            ListItem::new(content)
        })
        .collect();

    let list = ListWidget::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Menu (↑/↓: navigate, Enter: select, q: quit)"),
    );

    frame.render_widget(list, area);
}

/// Render the counter view
fn render_counter(frame: &mut Frame, count: u32) {
    let area = frame.area();

    let text = format!("Count: {count}\n\nPress 'b' or Esc to go back\nPress 'q' to quit");
    let paragraph =
        Paragraph::new(text).block(Block::default().borders(Borders::ALL).title("Counter"));

    frame.render_widget(paragraph, area);
}

/// Render the input view
fn render_input(frame: &mut Frame, text: &str, history: &[String]) {
    use std::fmt::Write;

    let area = frame.area();

    let mut content = String::from("Type to enter text, Enter to submit, Esc to go back\n\n");
    let _ = write!(content, "Input: {text}_\n\n");
    content.push_str("History:\n");
    for (i, item) in history.iter().enumerate() {
        let _ = writeln!(content, "  {}. {item}", i + 1);
    }

    let paragraph =
        Paragraph::new(content).block(Block::default().borders(Borders::ALL).title("Text Input"));

    frame.render_widget(paragraph, area);
}

/// Render the list view
fn render_list(frame: &mut Frame, items: &[String], selected: usize) {
    let area = frame.area();

    let list_items: Vec<ListItem> = items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let content = if i == selected {
                format!("> {item}")
            } else {
                format!("  {item}")
            };
            ListItem::new(content)
        })
        .collect();

    let list = ListWidget::new(list_items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("List (↑/↓: navigate, Esc: back, q: quit)"),
    );

    frame.render_widget(list, area);
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    let runtime = Runtime::<App>::new(());

    // Setup terminal
    let mut terminal = ratatui::init();

    // Run application at 60 FPS
    let result = runtime.run(&mut terminal, 60).await;

    // Restore terminal
    ratatui::restore();

    result?;

    Ok(())
}
