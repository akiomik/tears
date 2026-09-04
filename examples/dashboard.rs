//! Structured state management, with the root owning the wiring.
//!
//! This example shows how to organize a larger application without putting all
//! state and update logic in one place:
//! - The root `App` owns several focused state structs.
//! - Root `Message` wraps child messages such as `TaskMessage`.
//! - Each child state updates only its own local data.
//! - The root coordinates cross-component effects such as activity logging.
//!
//! That is the right shape while every child is a single instance whose
//! command ids and subscriptions the root can keep distinct by choosing
//! distinct values. [`dashboard_composed.rs`](dashboard_composed.rs) is this
//! same application written with composed reducers, for when a child exists in
//! more than one instance at a time or comes and goes: there the task list is a
//! keyed collection with one child reducer per row and the details pane is an
//! optionally-present child, and each boundary qualifies its child's identities
//! and tears a removed child's runs down. `docs/composition.md` is the guide to
//! choosing between the two.
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
                return Command::quit();
            }
            Message::Quit => return Command::quit(),
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
                    title: "Review release checklist".to_owned(),
                    done: false,
                    notes: "Check docs, examples, and CI status.".to_owned(),
                },
                Task {
                    title: "Update onboarding guide".to_owned(),
                    done: true,
                    notes: "Keep the first example small.".to_owned(),
                },
                Task {
                    title: "Plan next subscription API".to_owned(),
                    done: false,
                    notes: "Collect friction from examples first.".to_owned(),
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
            notes: "Add notes in the details panel.".to_owned(),
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
                title: "No task selected".to_owned(),
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
            entries: vec!["Application started".to_owned()],
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
            message: "Ready".to_owned(),
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
        KeyCode::Char('q') => Command::message(Message::Quit).into(),
        KeyCode::Tab => Command::message(Message::FocusNext).into(),
        KeyCode::Up => match focus {
            Focus::Navigation => {
                Command::message(Message::Navigation(NavigationMessage::Up)).into()
            }
            Focus::Tasks => Command::message(Message::Tasks(TaskMessage::Up)).into(),
            _ => Command::none(),
        },
        KeyCode::Down => match focus {
            Focus::Navigation => {
                Command::message(Message::Navigation(NavigationMessage::Down)).into()
            }
            Focus::Tasks => Command::message(Message::Tasks(TaskMessage::Down)).into(),
            _ => Command::none(),
        },
        KeyCode::Char(' ') if focus == Focus::Tasks => {
            Command::message(Message::Tasks(TaskMessage::Toggle)).into()
        }
        KeyCode::Char('n') if focus == Focus::Tasks => {
            Command::message(Message::Tasks(TaskMessage::Add)).into()
        }
        KeyCode::Char('d') if focus == Focus::Tasks => {
            Command::message(Message::Tasks(TaskMessage::Delete)).into()
        }
        KeyCode::Char('c') if focus == Focus::Activity => {
            Command::message(Message::Activity(ActivityMessage::Clear)).into()
        }
        KeyCode::Char(c) if focus == Focus::Details => {
            Command::message(Message::Details(DetailMessage::Input(c))).into()
        }
        KeyCode::Backspace if focus == Focus::Details => {
            Command::message(Message::Details(DetailMessage::Backspace)).into()
        }
        KeyCode::Enter if focus == Focus::Details => {
            Command::message(Message::Details(DetailMessage::Save)).into()
        }
        KeyCode::Esc if focus == Focus::Details => {
            Command::message(Message::Details(DetailMessage::Reset)).into()
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
    let runtime = Runtime::<App>::new(());
    let result = runtime.run(&mut terminal).await;

    // Restore terminal
    ratatui::restore();

    result?;

    Ok(())
}

/// Deterministic tests for this example's structured `update` logic, driven by
/// `tears::testing::TestStore` (RFC 0008). This example's transitions are pure
/// — most produce `Command::none()`, and the key handler produces follow-up
/// messages via `Command::message` — so the store needs no virtual time here;
/// run them with `cargo test --example dashboard`.
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use tears::testing::TestStore;

    /// Builds the `Terminal` message a keystroke would produce, so a test can
    /// exercise the same `handle_key_event` path the subscription feeds.
    fn key(code: KeyCode) -> Message {
        Message::Terminal(Event::Key(KeyEvent::new(code, KeyModifiers::empty())))
    }

    /// Sending the child message directly walks focus through the panels.
    #[test]
    fn focus_next_cycles_through_panels() {
        let mut store = TestStore::<App>::new(());
        assert_eq!(store.state().focus, Focus::Navigation);

        store.send(Message::FocusNext);
        assert_eq!(store.state().focus, Focus::Tasks);

        store.send(Message::FocusNext);
        assert_eq!(store.state().focus, Focus::Details);
        store.finish();
    }

    /// A key event is dispatched through `handle_key_event`, whose returned
    /// `Command::message(FocusNext)` the store delivers as the next message.
    #[test]
    fn tab_key_emits_focus_next() {
        let mut store = TestStore::<App>::new(());

        store.send(key(KeyCode::Tab));
        store.receive_matching(|msg| matches!(msg, Message::FocusNext));

        assert_eq!(store.state().focus, Focus::Tasks);
        store.finish();
    }

    /// Toggling the selected task flips its `done` flag and records an activity
    /// entry.
    #[test]
    fn toggle_marks_selected_task_done() {
        let mut store = TestStore::<App>::new(());
        assert!(!store.state().tasks.tasks[0].done);
        let activity_before = store.state().activity.entries.len();

        store.send(Message::Tasks(TaskMessage::Toggle));

        assert!(store.state().tasks.tasks[0].done);
        assert_eq!(store.state().activity.entries.len(), activity_before + 1);
        store.finish();
    }

    /// Adding a task then deleting it returns to the original count while the
    /// id counter keeps advancing.
    #[test]
    fn add_then_delete_returns_to_original_count() {
        let mut store = TestStore::<App>::new(());
        assert_eq!(store.state().tasks.tasks.len(), 3);
        assert_eq!(store.state().tasks.next_id, 4);

        store.send(Message::Tasks(TaskMessage::Add));
        assert_eq!(store.state().tasks.tasks.len(), 4);

        store.send(Message::Tasks(TaskMessage::Delete));
        assert_eq!(store.state().tasks.tasks.len(), 3);
        assert_eq!(store.state().tasks.next_id, 5);
        store.finish();
    }

    /// Pressing 'q' asks the key handler for a `Quit` message, whose `update`
    /// returns `Command::quit()`; the store observes both the message and the
    /// quit request.
    #[test]
    fn quit_key_requests_shutdown() {
        let mut store = TestStore::<App>::new(());

        store.send(key(KeyCode::Char('q')));
        store.receive_matching(|msg| matches!(msg, Message::Quit));
        store.receive_quit();
        store.finish();
    }
}
