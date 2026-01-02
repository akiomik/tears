//! WebSocket echo chat example demonstrating real-time communication with tears.
//!
//! This example shows:
//! - WebSocket subscription for bi-directional communication
//! - Terminal event subscription for user input
//! - Message handling and display with color-coded output
//! - Dynamic message list rendering
//!
//! The example connects to echo.websocket.org, which echoes back any message sent to it.
//!
//! Run with: cargo run --example websocket --features ws,rustls

use std::io;

use color_eyre::eyre::Result;
use crossterm::event::{Event, KeyCode, KeyEventKind};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use tears::{
    prelude::*,
    subscription::{
        terminal::TerminalEvents,
        websocket::{WebSocket, WebSocketCommand, WebSocketMessage},
    },
};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

/// Messages that the application can receive
#[derive(Debug)]
pub enum Msg {
    /// Terminal input event (keyboard, mouse, resize)
    Terminal(Event),
    /// Terminal event stream error
    TerminalError(io::Error),
    /// WebSocket message from subscription
    WebSocket(WebSocketMessage),
    /// Quit the application
    Quit,
}

/// Application state for the WebSocket echo chat
pub struct EchoChat {
    /// Current input text being typed by the user
    input: String,
    /// History of sent and received messages
    messages: Vec<String>,
    /// WebSocket command sender for sending messages
    ws_sender: Option<mpsc::UnboundedSender<WebSocketCommand>>,
}

impl Default for EchoChat {
    fn default() -> Self {
        Self {
            input: String::new(),
            messages: vec!["Connecting to WebSocket...".to_string()],
            ws_sender: None,
        }
    }
}

impl EchoChat {
    /// Handle keyboard input and return appropriate commands
    fn handle_key(&mut self, code: KeyCode) -> Command<Msg> {
        match code {
            KeyCode::Char('q') => Command::single(Msg::Quit),
            KeyCode::Char(c) => {
                self.input.push(c);
                Command::none()
            }
            KeyCode::Backspace => {
                self.input.pop();
                Command::none()
            }
            KeyCode::Enter => self.submit_message(),
            _ => Command::none(),
        }
    }

    /// Submit the current input as a WebSocket message
    fn submit_message(&mut self) -> Command<Msg> {
        if self.input.is_empty() {
            return Command::none();
        }

        let text = self.input.clone();
        self.input.clear();

        // Send the message through WebSocket if connected
        if let Some(sender) = &self.ws_sender {
            if sender
                .send(WebSocketCommand::SendText(text.clone()))
                .is_ok()
            {
                self.messages.push(format!("Sent: {text}"));
            } else {
                self.messages.push("Failed to send message".to_string());
            }
        } else {
            self.messages.push("Not connected to WebSocket".to_string());
        }

        Command::none()
    }

    /// Determine the style for a message based on its prefix
    fn message_style(message: &str) -> Style {
        if message.starts_with("Sent:") {
            Style::default().fg(Color::Green)
        } else if message.starts_with("Received:") {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::Yellow)
        }
    }
}

impl Application for EchoChat {
    type Message = Msg;
    type Flags = ();

    /// Initialize the application
    fn new(_flags: ()) -> (Self, Command<Msg>) {
        (Self::default(), Command::none())
    }

    /// Handle incoming messages and update state
    fn update(&mut self, msg: Msg) -> Command<Msg> {
        match msg {
            // Handle key press events only (ignore key release)
            Msg::Terminal(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                self.handle_key(key.code)
            }
            // Ignore other terminal events (mouse, resize, etc.)
            Msg::Terminal(_) => Command::none(),
            // Log terminal errors as messages
            Msg::TerminalError(e) => {
                self.messages.push(format!("Terminal error: {e}"));
                Command::none()
            }
            // Handle WebSocket messages
            Msg::WebSocket(ws_msg) => {
                match ws_msg {
                    WebSocketMessage::Connected { sender } => {
                        self.ws_sender = Some(sender);
                        self.messages.push("Connected to WebSocket!".to_string());
                    }
                    WebSocketMessage::Disconnected => {
                        self.messages
                            .push("Disconnected from WebSocket".to_string());
                        self.ws_sender = None;
                    }
                    WebSocketMessage::Received(msg) => {
                        match msg {
                            Message::Text(text) => {
                                self.messages.push(format!("Received: {text}"));
                            }
                            Message::Binary(data) => {
                                self.messages
                                    .push(format!("Received binary: {} bytes", data.len()));
                            }
                            // Ping/Pong frames (usually handled automatically by tungstenite)
                            _ => {}
                        }
                    }
                    WebSocketMessage::Error { error } => {
                        self.messages.push(format!("Error: {error}"));
                    }
                }
                Command::none()
            }
            Msg::Quit => Command::effect(Action::Quit),
        }
    }

    /// Render the UI
    fn view(&self, frame: &mut Frame) {
        // Split the screen into two areas: messages (top) and input (bottom)
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(3)])
            .split(frame.area());

        // Render messages area
        self.render_messages(frame, chunks[0]);

        // Render input area
        self.render_input(frame, chunks[1]);
    }

    /// Subscribe to events (terminal input and WebSocket messages)
    fn subscriptions(&self) -> Vec<Subscription<Msg>> {
        vec![
            // Terminal events (keyboard, mouse, resize)
            Subscription::new(TerminalEvents::new()).map(|result| match result {
                Ok(event) => Msg::Terminal(event),
                Err(e) => Msg::TerminalError(e),
            }),
            // WebSocket connection (always active)
            Subscription::new(WebSocket::new("wss://echo.websocket.org")).map(Msg::WebSocket),
        ]
    }
}

impl EchoChat {
    /// Render the messages list widget
    fn render_messages(&self, frame: &mut Frame, area: Rect) {
        // Calculate how many messages fit in the available height
        let max_messages = area.height.saturating_sub(2) as usize;

        // Take the most recent messages that fit on screen
        let messages: Vec<ListItem> = self
            .messages
            .iter()
            .rev()
            .take(max_messages)
            .rev()
            .map(|m| {
                let style = Self::message_style(m);
                ListItem::new(Line::from(Span::styled(m.clone(), style)))
            })
            .collect();

        let messages_widget = List::new(messages).block(
            Block::default()
                .borders(Borders::ALL)
                .title("WebSocket Echo Chat (q: quit)"),
        );

        frame.render_widget(messages_widget, area);
    }

    /// Render the input field widget
    fn render_input(&self, frame: &mut Frame, area: Rect) {
        let input_widget = Paragraph::new(self.input.as_str())
            .style(Style::default().fg(Color::White))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Type message and press Enter"),
            );

        frame.render_widget(input_widget, area);
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    let runtime = Runtime::<EchoChat>::new(());

    // Setup terminal
    let mut terminal = ratatui::init();

    // Run the application at 60 FPS
    let result = runtime.run(&mut terminal, 60).await;

    // Restore terminal
    ratatui::restore();

    result?;

    Ok(())
}
