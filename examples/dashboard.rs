//! Structured state management example.
//!
//! This example shows how to organize a larger application without putting all
//! state and update logic in one place:
//! - The root `App` owns several focused state structs.
//! - Root `Message` wraps child messages such as `TaskMessage`.
//! - Each child state updates only its own local data.
//! - The root coordinates cross-component effects such as activity logging.
//!
//! Run with: cargo run --example dashboard

use color_eyre::eyre::Result;
use crossterm::event::{Event, KeyCode, KeyEvent};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use tears::prelude::*;
use tears::subscription::terminal::TerminalEvents;

const ACTIVITY_LIMIT: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Navigation,
    Tasks,
    Details,
    Activity,
}

impl Focus {
    const fn next(self) -> Self {
        match self {
            Self::Navigation => Self::Tasks,
            Self::Tasks => Self::Details,
            Self::Details => Self::Activity,
            Self::Activity => Self::Navigation,
        }
    }

    fn title(self, label: &'static str) -> &'static str {
        match self {
            Self::Navigation if matches!(label, "Navigation") => "Navigation [focused]",
            Self::Tasks if matches!(label, "Tasks") => "Tasks [focused]",
            Self::Details if matches!(label, "Details") => "Details [focused]",
            Self::Activity if matches!(label, "Activity") => "Activity [focused]",
            _ => label,
        }
    }
}

#[derive(Debug, Clone)]
enum Message {
    FocusNext,
    Navigation(NavigationMessage),
    Tasks(TaskMessage),
    Details(DetailMessage),
    Activity(ActivityMessage),
    Terminal(Event),
    TerminalError(String),
    Quit,
}

#[derive(Debug, Clone, Copy)]
enum NavigationMessage {
    Up,
    Down,
}

#[derive(Debug, Clone, Copy)]
enum TaskMessage {
    Up,
    Down,
    Toggle,
    Add,
    Delete,
}

#[derive(Debug, Clone, Copy)]
enum DetailMessage {
    Input(char),
    Backspace,
    Save,
    Reset,
}

#[derive(Debug, Clone, Copy)]
enum ActivityMessage {
    Clear,
}

#[derive(Debug, Clone)]
struct App {
    focus: Focus,
    navigation: NavigationState,
    tasks: TaskListState,
    details: DetailState,
    activity: ActivityLogState,
    status: StatusState,
}

impl Application for App {
    type Message = Message;
    type Flags = ();

    fn new(_flags: ()) -> (Self, Command<Message>) {
        let tasks = TaskListState::new();
        let details = DetailState::from_task(tasks.selected_task());

        (
            Self {
                focus: Focus::Navigation,
                navigation: NavigationState::new(),
                tasks,
                details,
                activity: ActivityLogState::new(),
                status: StatusState::default(),
            },
            Command::none(),
        )
    }

    fn update(&mut self, msg: Message) -> Command<Message> {
        match msg {
            Message::FocusNext => {
                self.focus = self.focus.next();
                self.status.set_info("Changed focus");
            }
            Message::Navigation(msg) => self.update_navigation(msg),
            Message::Tasks(msg) => self.update_tasks(msg),
            Message::Details(msg) => self.update_details(msg),
            Message::Activity(msg) => self.update_activity(msg),
            Message::Terminal(Event::Key(key)) => return handle_key_event(self.focus, key),
            Message::Terminal(_) => {}
            Message::TerminalError(e) => {
                eprintln!("Terminal error: {e}");
                return Command::effect(Action::Quit);
            }
            Message::Quit => return Command::effect(Action::Quit),
        }

        Command::none()
    }

    fn view(&self, frame: &mut Frame) {
        let [header, body, activity, footer] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(7),
            Constraint::Length(3),
        ])
        .areas(frame.area());

        let [nav_area, tasks_area, details_area] = Layout::horizontal([
            Constraint::Length(24),
            Constraint::Percentage(36),
            Constraint::Percentage(64),
        ])
        .areas(body);

        Self::render_header(frame, header);
        self.navigation
            .render(frame, nav_area, self.focus == Focus::Navigation);
        self.tasks
            .render(frame, tasks_area, self.focus == Focus::Tasks);
        self.details
            .render(frame, details_area, self.focus == Focus::Details);
        self.activity
            .render(frame, activity, self.focus == Focus::Activity);
        self.status.render(frame, footer);
    }

    fn subscriptions(&self) -> Vec<Subscription<Message>> {
        vec![
            Subscription::new(TerminalEvents::new()).map(|result| match result {
                Ok(event) => Message::Terminal(event),
                Err(e) => Message::TerminalError(e.to_string()),
            }),
        ]
    }
}

impl App {
    fn update_navigation(&mut self, msg: NavigationMessage) {
        self.navigation.update(msg);
        self.status.set_info(format!(
            "Selected section: {}",
            self.navigation.selected_label()
        ));
    }

    fn update_tasks(&mut self, msg: TaskMessage) {
        let outcome = self.tasks.update(msg);
        self.details.sync_from_task(self.tasks.selected_task());

        if let Some(entry) = outcome.activity {
            self.activity.push(entry);
        }
        self.status.set_info(outcome.status);
    }

    fn update_details(&mut self, msg: DetailMessage) {
        let outcome = self.details.update(msg);

        if outcome.save_requested {
            if let Some(task) = self.tasks.selected_task_mut() {
                task.notes.clone_from(&self.details.notes);
                self.activity
                    .push(format!("Updated notes for '{}'", task.title));
                self.status.set_info("Saved task notes");
            }
        } else {
            self.status.set_info(outcome.status);
        }
    }

    fn update_activity(&mut self, msg: ActivityMessage) {
        self.activity.update(msg);
        self.status.set_info("Cleared activity log");
    }

    fn render_header(frame: &mut Frame, area: Rect) {
        let text = "Dashboard state example | Tab: focus | q: quit";
        let block = Block::default()
            .borders(Borders::ALL)
            .title("Structured State");
        frame.render_widget(Paragraph::new(text).block(block), area);
    }
}

#[derive(Debug, Clone)]
struct NavigationState {
    items: Vec<&'static str>,
    selected: usize,
}

impl NavigationState {
    fn new() -> Self {
        Self {
            items: vec!["Inbox", "Today", "Upcoming", "Archive"],
            selected: 0,
        }
    }

    fn update(&mut self, msg: NavigationMessage) {
        match msg {
            NavigationMessage::Up => self.selected = self.selected.saturating_sub(1),
            NavigationMessage::Down => {
                self.selected = (self.selected + 1).min(self.items.len().saturating_sub(1));
            }
        }
    }

    fn selected_label(&self) -> &'static str {
        self.items[self.selected]
    }

    fn render(&self, frame: &mut Frame, area: Rect, focused: bool) {
        let items = self.items.iter().enumerate().map(|(index, label)| {
            let marker = if index == self.selected { ">" } else { " " };
            ListItem::new(format!("{marker} {label}"))
        });

        let title = if focused {
            Focus::Navigation.title("Navigation")
        } else {
            "Navigation"
        };
        let list = List::new(items).block(Block::default().borders(Borders::ALL).title(title));
        frame.render_widget(list, area);
    }
}

#[derive(Debug, Clone)]
struct Task {
    title: String,
    done: bool,
    notes: String,
}

#[derive(Debug, Clone)]
struct TaskListState {
    tasks: Vec<Task>,
    selected: usize,
    next_id: usize,
}

#[derive(Debug, Clone)]
struct TaskOutcome {
    status: String,
    activity: Option<String>,
}

impl TaskOutcome {
    fn status(status: impl Into<String>) -> Self {
        Self {
            status: status.into(),
            activity: None,
        }
    }

    fn activity(status: impl Into<String>, activity: impl Into<String>) -> Self {
        Self {
            status: status.into(),
            activity: Some(activity.into()),
        }
    }
}

impl TaskListState {
    fn new() -> Self {
        Self {
            tasks: vec![
                Task {
                    title: "Review release checklist".to_string(),
                    done: false,
                    notes: "Check docs, examples, and CI status.".to_string(),
                },
                Task {
                    title: "Update onboarding guide".to_string(),
                    done: true,
                    notes: "Keep the first example small.".to_string(),
                },
                Task {
                    title: "Plan next subscription API".to_string(),
                    done: false,
                    notes: "Collect friction from examples first.".to_string(),
                },
            ],
            selected: 0,
            next_id: 4,
        }
    }

    fn update(&mut self, msg: TaskMessage) -> TaskOutcome {
        match msg {
            TaskMessage::Up => {
                self.selected = self.selected.saturating_sub(1);
                TaskOutcome::status("Selected previous task")
            }
            TaskMessage::Down => {
                self.selected = (self.selected + 1).min(self.tasks.len().saturating_sub(1));
                TaskOutcome::status("Selected next task")
            }
            TaskMessage::Toggle => self.toggle_selected(),
            TaskMessage::Add => self.add_task(),
            TaskMessage::Delete => self.delete_selected(),
        }
    }

    fn selected_task(&self) -> Option<&Task> {
        self.tasks.get(self.selected)
    }

    fn selected_task_mut(&mut self) -> Option<&mut Task> {
        self.tasks.get_mut(self.selected)
    }

    fn toggle_selected(&mut self) -> TaskOutcome {
        let Some(task) = self.selected_task_mut() else {
            return TaskOutcome::status("No task selected");
        };

        task.done = !task.done;
        let state = if task.done { "completed" } else { "reopened" };
        TaskOutcome::activity(
            format!("Marked task as {state}"),
            format!("{state}: {}", task.title),
        )
    }

    fn add_task(&mut self) -> TaskOutcome {
        let title = format!("New task {}", self.next_id);
        self.next_id += 1;
        self.tasks.push(Task {
            title: title.clone(),
            done: false,
            notes: "Add notes in the details panel.".to_string(),
        });
        self.selected = self.tasks.len() - 1;
        TaskOutcome::activity("Added a task", format!("added: {title}"))
    }

    fn delete_selected(&mut self) -> TaskOutcome {
        if self.tasks.is_empty() {
            return TaskOutcome::status("No task to delete");
        }

        let removed = self.tasks.remove(self.selected);
        self.selected = self.selected.min(self.tasks.len().saturating_sub(1));
        TaskOutcome::activity(
            "Deleted selected task",
            format!("deleted: {}", removed.title),
        )
    }

    fn render(&self, frame: &mut Frame, area: Rect, focused: bool) {
        let items = self.tasks.iter().enumerate().map(|(index, task)| {
            let marker = if index == self.selected { ">" } else { " " };
            let checkbox = if task.done { "[x]" } else { "[ ]" };
            ListItem::new(format!("{marker} {checkbox} {}", task.title))
        });

        let title = if focused {
            Focus::Tasks.title("Tasks")
        } else {
            "Tasks"
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!("{title} (up/down, space, n, d)"));
        frame.render_widget(List::new(items).block(block), area);
    }
}

#[derive(Debug, Clone)]
struct DetailState {
    title: String,
    notes: String,
}

#[derive(Debug, Clone)]
struct DetailOutcome {
    status: String,
    save_requested: bool,
}

impl DetailOutcome {
    fn status(status: impl Into<String>) -> Self {
        Self {
            status: status.into(),
            save_requested: false,
        }
    }

    fn save(status: impl Into<String>) -> Self {
        Self {
            status: status.into(),
            save_requested: true,
        }
    }
}

impl DetailState {
    fn from_task(task: Option<&Task>) -> Self {
        let Some(task) = task else {
            return Self {
                title: "No task selected".to_string(),
                notes: String::new(),
            };
        };

        Self {
            title: task.title.clone(),
            notes: task.notes.clone(),
        }
    }

    fn sync_from_task(&mut self, task: Option<&Task>) {
        *self = Self::from_task(task);
    }

    fn update(&mut self, msg: DetailMessage) -> DetailOutcome {
        match msg {
            DetailMessage::Input(c) => {
                self.notes.push(c);
                DetailOutcome::status("Editing notes")
            }
            DetailMessage::Backspace => {
                self.notes.pop();
                DetailOutcome::status("Editing notes")
            }
            DetailMessage::Save => DetailOutcome::save("Saved task notes"),
            DetailMessage::Reset => DetailOutcome::status("No changes saved"),
        }
    }

    fn render(&self, frame: &mut Frame, area: Rect, focused: bool) {
        let title = if focused {
            Focus::Details.title("Details")
        } else {
            "Details"
        };
        let text = format!(
            "{}\n\nNotes:\n{}_\n\nType to edit, Enter to save, Esc to reload selected task.",
            self.title, self.notes
        );
        let paragraph = Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(title));
        frame.render_widget(paragraph, area);
    }
}

#[derive(Debug, Clone)]
struct ActivityLogState {
    entries: Vec<String>,
}

impl ActivityLogState {
    fn new() -> Self {
        Self {
            entries: vec!["Application started".to_string()],
        }
    }

    fn update(&mut self, msg: ActivityMessage) {
        match msg {
            ActivityMessage::Clear => self.entries.clear(),
        }
    }

    fn push(&mut self, entry: String) {
        self.entries.push(entry);
        if self.entries.len() > ACTIVITY_LIMIT {
            self.entries.remove(0);
        }
    }

    fn render(&self, frame: &mut Frame, area: Rect, focused: bool) {
        let title = if focused {
            Focus::Activity.title("Activity")
        } else {
            "Activity"
        };
        let items = self
            .entries
            .iter()
            .rev()
            .map(|entry| ListItem::new(format!("- {entry}")))
            .collect::<Vec<_>>();
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!("{title} (c: clear)"));
        frame.render_widget(List::new(items).block(block), area);
    }
}

#[derive(Debug, Clone)]
struct StatusState {
    message: String,
}

impl Default for StatusState {
    fn default() -> Self {
        Self {
            message: "Ready".to_string(),
        }
    }
}

impl StatusState {
    fn set_info(&mut self, message: impl Into<String>) {
        self.message = message.into();
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        let text = format!("Status: {}", self.message);
        let block = Block::default().borders(Borders::ALL).title("Status");
        frame.render_widget(Paragraph::new(text).block(block), area);
    }
}

fn handle_key_event(focus: Focus, key: KeyEvent) -> Command<Message> {
    match key.code {
        KeyCode::Char('q') => Command::message(Message::Quit),
        KeyCode::Tab => Command::message(Message::FocusNext),
        KeyCode::Up => match focus {
            Focus::Navigation => Command::message(Message::Navigation(NavigationMessage::Up)),
            Focus::Tasks => Command::message(Message::Tasks(TaskMessage::Up)),
            _ => Command::none(),
        },
        KeyCode::Down => match focus {
            Focus::Navigation => Command::message(Message::Navigation(NavigationMessage::Down)),
            Focus::Tasks => Command::message(Message::Tasks(TaskMessage::Down)),
            _ => Command::none(),
        },
        KeyCode::Char(' ') if focus == Focus::Tasks => {
            Command::message(Message::Tasks(TaskMessage::Toggle))
        }
        KeyCode::Char('n') if focus == Focus::Tasks => {
            Command::message(Message::Tasks(TaskMessage::Add))
        }
        KeyCode::Char('d') if focus == Focus::Tasks => {
            Command::message(Message::Tasks(TaskMessage::Delete))
        }
        KeyCode::Char('c') if focus == Focus::Activity => {
            Command::message(Message::Activity(ActivityMessage::Clear))
        }
        KeyCode::Char(c) if focus == Focus::Details => {
            Command::message(Message::Details(DetailMessage::Input(c)))
        }
        KeyCode::Backspace if focus == Focus::Details => {
            Command::message(Message::Details(DetailMessage::Backspace))
        }
        KeyCode::Enter if focus == Focus::Details => {
            Command::message(Message::Details(DetailMessage::Save))
        }
        KeyCode::Esc if focus == Focus::Details => {
            Command::message(Message::Details(DetailMessage::Reset))
        }
        _ => Command::none(),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    // Setup terminal
    let mut terminal = ratatui::init();

    // Run application at 60 FPS
    let runtime = Runtime::<App>::try_new((), 60)?;
    let result = runtime.run(&mut terminal).await;

    // Restore terminal
    ratatui::restore();

    result?;

    Ok(())
}
