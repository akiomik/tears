//! Multiple view states example demonstrating navigation between screens.
//!
//! This example shows:
//! - State machine pattern for switching between views
//! - Navigation between views using messages
//! - Conditional subscriptions based on current view
//! - Different UI rendering for each view
//!
//! For a larger state-management example with nested state structs and child
//! messages, see `examples/dashboard.rs`.
//!
//! Views:
//! - Menu: Select which view to navigate to
//! - Counter: Auto-incrementing counter with timer
//! - Input: Text input with history
//! - List: Scrollable list of items
//!
//! Run with: cargo run --example views

use std::num::{NonZeroU32, NonZeroU64};

use color_eyre::eyre::Result;
use crossterm::event::{Event, KeyCode, KeyEvent};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List as ListWidget, ListItem, Paragraph};
use tears::prelude::*;
use tears::subscription::{
    terminal::TerminalEvents,
    time::{Timer, TimerEvent},
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

    /// Route incoming messages to the view-specific update logic.
    fn update(&mut self, msg: Message) -> Command<Self::Message> {
        match msg {
            Message::GoToMenu
            | Message::GoToCounter
            | Message::GoToInput
            | Message::GoToList
            | Message::Quit => self.update_navigation(&msg),
            Message::MenuUp | Message::MenuDown | Message::MenuSelect => self.update_menu(&msg),
            Message::Tick => self.update_counter(),
            Message::InputChar(c) => self.update_input_char(c),
            Message::InputSubmit => self.update_input_submit(),
            Message::InputBackspace => self.update_input_backspace(),
            Message::ListUp | Message::ListDown => self.update_list(&msg),
            Message::Terminal(Event::Key(key)) => handle_key_event(&self.view, key),
            Message::Terminal(_) => Command::none(),
            Message::TerminalError(e) => handle_terminal_error(&e),
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
                Subscription::new(Timer::new(NonZeroU64::new(1000).expect("non-zero"))).map(
                    |timer_msg| match timer_msg {
                        TimerEvent::Tick => Message::Tick,
                    },
                ),
            );
        }

        subs
    }
}

impl App {
    fn update_navigation(&mut self, msg: &Message) -> Command<Message> {
        match msg {
            Message::GoToMenu => self.view = View::Menu { selected: 0 },
            Message::GoToCounter => self.view = View::Counter { count: 0 },
            Message::GoToInput => {
                self.view = View::Input {
                    text: String::new(),
                    history: vec![],
                };
            }
            Message::GoToList => {
                self.view = View::List {
                    items: vec![
                        "Item 1".to_owned(),
                        "Item 2".to_owned(),
                        "Item 3".to_owned(),
                        "Item 4".to_owned(),
                        "Item 5".to_owned(),
                    ],
                    selected: 0,
                };
            }
            Message::Quit => return Command::quit(),
            _ => {}
        }

        Command::none()
    }

    fn update_menu(&mut self, msg: &Message) -> Command<Message> {
        let View::Menu { selected } = &mut self.view else {
            return Command::none();
        };

        match msg {
            Message::MenuUp => *selected = selected.saturating_sub(1),
            Message::MenuDown => *selected = (*selected + 1).min(3),
            Message::MenuSelect => {
                return match *selected {
                    0 => Command::message(Message::GoToCounter),
                    1 => Command::message(Message::GoToInput),
                    2 => Command::message(Message::GoToList),
                    3 => Command::message(Message::Quit),
                    _ => Command::none(),
                };
            }
            _ => {}
        }

        Command::none()
    }

    const fn update_counter(&mut self) -> Command<Message> {
        if let View::Counter { count } = &mut self.view {
            *count += 1;
        }

        Command::none()
    }

    fn update_input_char(&mut self, c: char) -> Command<Message> {
        if let View::Input { text, .. } = &mut self.view {
            text.push(c);
        }

        Command::none()
    }

    fn update_input_backspace(&mut self) -> Command<Message> {
        if let View::Input { text, .. } = &mut self.view {
            text.pop();
        }

        Command::none()
    }

    fn update_input_submit(&mut self) -> Command<Message> {
        if let View::Input { text, history } = &mut self.view
            && !text.is_empty()
        {
            history.push(text.clone());
            text.clear();
        }

        Command::none()
    }

    fn update_list(&mut self, msg: &Message) -> Command<Message> {
        let View::List { items, selected } = &mut self.view else {
            return Command::none();
        };

        match msg {
            Message::ListUp => *selected = selected.saturating_sub(1),
            Message::ListDown => *selected = (*selected + 1).min(items.len().saturating_sub(1)),
            _ => {}
        }

        Command::none()
    }
}

/// Handle keyboard events based on current view
#[allow(clippy::use_self)]
fn handle_key_event(view: &View, key: KeyEvent) -> Command<Message> {
    match view {
        View::Menu { .. } => match key.code {
            KeyCode::Up => Command::message(Message::MenuUp),
            KeyCode::Down => Command::message(Message::MenuDown),
            KeyCode::Enter => Command::message(Message::MenuSelect),
            KeyCode::Char('q') => Command::message(Message::Quit),
            _ => Command::none(),
        },
        View::Counter { .. } => match key.code {
            KeyCode::Char('b') | KeyCode::Esc => Command::message(Message::GoToMenu),
            KeyCode::Char('q') => Command::message(Message::Quit),
            _ => Command::none(),
        },
        View::Input { .. } => match key.code {
            KeyCode::Char(c) => Command::message(Message::InputChar(c)),
            KeyCode::Backspace => Command::message(Message::InputBackspace),
            KeyCode::Enter => Command::message(Message::InputSubmit),
            KeyCode::Esc => Command::message(Message::GoToMenu),
            _ => Command::none(),
        },
        View::List { .. } => match key.code {
            KeyCode::Up => Command::message(Message::ListUp),
            KeyCode::Down => Command::message(Message::ListDown),
            KeyCode::Char('b') | KeyCode::Esc => Command::message(Message::GoToMenu),
            KeyCode::Char('q') => Command::message(Message::Quit),
            _ => Command::none(),
        },
    }
}

fn handle_terminal_error(error: &str) -> Command<Message> {
    eprintln!("Terminal error: {error}");
    Command::quit()
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

    // Setup terminal
    let mut terminal = ratatui::init();

    // Run application at 60 FPS
    let frame_rate = FrameRate::new(NonZeroU32::new(60).expect("non-zero"))?;
    let runtime = Runtime::<App>::new((), frame_rate);
    let result = runtime.run(&mut terminal).await;

    // Restore terminal
    ratatui::restore();

    result?;

    Ok(())
}
