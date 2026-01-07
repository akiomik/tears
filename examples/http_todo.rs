//! HTTP Todo List example demonstrating Query and Mutation with caching.
//!
//! This example shows:
//! - Query subscription for automatic data fetching and caching
//! - Mutation for creating new todos
//! - Cache invalidation after mutations
//! - Loading and error states
//!
//! This uses `JSONPlaceholder` API (<https://jsonplaceholder.typicode.com/>) as a mock backend.
//!
//! Run with: `cargo run --example http_todo --features http`

use std::sync::Arc;

use crossterm::event::{Event, KeyCode, KeyEventKind};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use serde::{Deserialize, Serialize};
use std::io;
use tears::prelude::*;
use tears::subscription::http::{
    Mutation, Query, QueryClient, QueryError, QueryResult, QueryState,
};
use tears::subscription::terminal::TerminalEvents;

/// A todo item from the API
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Todo {
    id: u32,
    #[serde(rename = "userId")]
    user_id: u32,
    title: String,
    completed: bool,
}

/// Messages that the application can receive
#[derive(Debug)]
enum Message {
    /// Terminal input event
    Terminal(Event),
    /// Terminal error
    TerminalError(std::io::Error),
    /// Query result for todos
    TodosQuery(QueryResult<Vec<Todo>>),
    /// Create todo result
    TodoCreated(Result<Todo, QueryError>),
    /// User input changed
    InputChanged(char),
    /// Backspace pressed
    InputBackspace,
    /// Submit new todo
    SubmitTodo,
    /// Quit application
    Quit,
}

/// Application state
struct App {
    /// Query client for cache management
    query_client: Arc<QueryClient>,
    /// Current todos state
    todos_state: QueryState<Vec<Todo>>,
    /// Input field for new todo
    input: String,
    /// Status message
    status: String,
}

impl Default for App {
    fn default() -> Self {
        Self {
            query_client: Arc::new(QueryClient::new()),
            todos_state: QueryState::Loading,
            input: String::new(),
            status: String::new(),
        }
    }
}

impl Application for App {
    type Message = Message;
    type Flags = ();

    fn new(_flags: ()) -> (Self, Command<Message>) {
        (Self::default(), Command::none())
    }

    fn update(&mut self, msg: Message) -> Command<Message> {
        match msg {
            Message::Terminal(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                match key.code {
                    KeyCode::Char('q') => Command::message(Message::Quit),
                    KeyCode::Char(c) => Command::message(Message::InputChanged(c)),
                    KeyCode::Backspace => Command::message(Message::InputBackspace),
                    KeyCode::Enter => Command::message(Message::SubmitTodo),
                    _ => Command::none(),
                }
            }
            Message::Terminal(_) => Command::none(),
            Message::TerminalError(e) => {
                self.status = format!("Terminal error: {e}");
                Command::none()
            }
            Message::TodosQuery(result) => {
                self.todos_state = result.state;
                Command::none()
            }
            Message::InputChanged(c) => {
                self.input.push(c);
                Command::none()
            }
            Message::InputBackspace => {
                self.input.pop();
                Command::none()
            }
            Message::SubmitTodo => {
                if self.input.is_empty() {
                    self.status = "Please enter a todo title".to_string();
                    return Command::none();
                }

                let title = self.input.clone();
                self.input.clear();
                self.status = "Creating todo...".to_string();

                Mutation::mutate(
                    Todo {
                        id: 0, // API will assign ID
                        user_id: 1,
                        title,
                        completed: false,
                    },
                    |todo| {
                        Box::pin(async move {
                            let client = reqwest::Client::new();
                            let response = client
                                .post("https://jsonplaceholder.typicode.com/todos")
                                .json(&todo)
                                .send()
                                .await
                                .map_err(|e| QueryError::NetworkError(e.to_string()))?;

                            response
                                .json()
                                .await
                                .map_err(|e| QueryError::FetchError(e.to_string()))
                        })
                    },
                )
                .map(Message::TodoCreated)
            }
            Message::TodoCreated(Ok(todo)) => {
                self.status = format!("Created: {}", todo.title);
                // Invalidate cache to refetch todos
                self.query_client.invalidate(&"todos")
            }
            Message::TodoCreated(Err(e)) => {
                self.status = format!("Failed to create todo: {e}");
                Command::none()
            }
            Message::Quit => Command::effect(Action::Quit),
        }
    }

    fn view(&self, frame: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Title
                Constraint::Min(1),    // Todo list
                Constraint::Length(3), // Input
                Constraint::Length(3), // Status
            ])
            .split(frame.area());

        // Title
        let title = Paragraph::new("HTTP Todo List (q: quit, Enter: add todo)")
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(title, chunks[0]);

        // Todo list
        self.render_todos(frame, chunks[1]);

        // Input
        let input = Paragraph::new(self.input.as_str())
            .block(Block::default().borders(Borders::ALL).title("New Todo"));
        frame.render_widget(input, chunks[2]);

        // Status
        let status = Paragraph::new(self.status.as_str())
            .block(Block::default().borders(Borders::ALL).title("Status"));
        frame.render_widget(status, chunks[3]);
    }

    fn subscriptions(&self) -> Vec<Subscription<Message>> {
        vec![
            // Terminal events
            Subscription::new(TerminalEvents::new()).map(|result| match result {
                Ok(event) => Message::Terminal(event),
                Err(e) => Message::TerminalError(e),
            }),
            // Todos query - automatically fetches and caches
            Subscription::new(Query::new(
                &"todos",
                || {
                    Box::pin(async {
                        let client = reqwest::Client::new();
                        let response = client
                            .get("https://jsonplaceholder.typicode.com/todos")
                            .query(&[("_limit", "10")]) // Limit to 10 for demo
                            .send()
                            .await
                            .map_err(|e| QueryError::NetworkError(e.to_string()))?;

                        response
                            .json()
                            .await
                            .map_err(|e| QueryError::FetchError(e.to_string()))
                    })
                },
                self.query_client.clone(),
            ))
            .map(Message::TodosQuery),
        ]
    }
}

impl App {
    fn render_todos(&self, frame: &mut Frame, area: Rect) {
        match &self.todos_state {
            QueryState::Loading => {
                let loading = Paragraph::new("Loading todos...")
                    .block(Block::default().borders(Borders::ALL).title("Todos"));
                frame.render_widget(loading, area);
            }
            QueryState::Success { data, is_stale } => {
                let title = if *is_stale {
                    "Todos (stale, refetching...)"
                } else {
                    "Todos"
                };

                let items: Vec<ListItem> = data
                    .iter()
                    .map(|todo| {
                        let status = if todo.completed { "✓" } else { " " };
                        let style = if todo.completed {
                            Style::default().fg(Color::Green)
                        } else {
                            Style::default()
                        };
                        ListItem::new(format!("[{}] {}", status, todo.title)).style(style)
                    })
                    .collect();

                let list =
                    List::new(items).block(Block::default().borders(Borders::ALL).title(title));
                frame.render_widget(list, area);
            }
            QueryState::Error(e) => {
                let error = Paragraph::new(format!("Error loading todos: {e}"))
                    .style(Style::default().fg(Color::Red))
                    .block(Block::default().borders(Borders::ALL).title("Todos"));
                frame.render_widget(error, area);
            }
        }
    }
}

#[tokio::main]
async fn main() -> io::Result<()> {
    // Setup terminal
    let mut terminal = ratatui::init();

    // Run the application at 60 FPS
    let runtime = Runtime::<App>::new((), 60);
    let result = runtime.run(&mut terminal).await;

    // Restore terminal
    ratatui::restore();

    result
}
