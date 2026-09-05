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

use std::sync::{Arc, Mutex};

use color_eyre::eyre::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use tears::prelude::*;
use tears::subscription::terminal::TerminalEvents;

const ACTIVITY_LIMIT: usize = 8;

/// Where the run leaves a reason `main` can print once the terminal is back.
///
/// A handle rather than a field of the state, and the same one
/// `dashboard_composed.rs` has: a terminal error quits, `main` restores the
/// terminal immediately after, and anything written to the screen — or to
/// stderr while the alternate screen is up — goes with it. The report has to
/// outlive the run to be a report at all.
#[derive(Clone, Debug, Default)]
struct ExitReason(Arc<Mutex<Option<String>>>);

impl ExitReason {
    fn set(&self, reason: String) {
        *self.0.lock().expect("the exit reason lock is not poisoned") = Some(reason);
    }

    fn take(&self) -> Option<String> {
        self.0
            .lock()
            .expect("the exit reason lock is not poisoned")
            .take()
    }
}

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
    reason: ExitReason,
    focus: Focus,
    navigation: NavigationState,
    tasks: TaskListState,
    details: DetailState,
    activity: ActivityLogState,
    status: StatusState,
}

impl Application for App {
    type Message = Message;
    type Flags = ExitReason;

    fn new(reason: ExitReason) -> (Self, Command<Message>) {
        let tasks = TaskListState::new();
        let details = DetailState::from_task(tasks.selected_task());

        (
            Self {
                reason,
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
            // Not releases. A terminal that reports them — Windows, or a
            // session with the kitty keyboard protocol on — sends two events
            // for one press, and the second would be acted on. A *repeat* is
            // kept, and is the other thing entirely: a held key is repetition
            // the reader is asking for. Testing for `Press` would drop those
            // with the releases, and a held Down would move the selection
            // once.
            Message::Terminal(Event::Key(key)) if key.kind != KeyEventKind::Release => {
                return handle_key_event(self.focus, key);
            }
            Message::Terminal(_) => {}
            Message::TerminalError(e) => {
                // Recorded rather than printed: the quit below ends the run and
                // `main` restores the terminal immediately after, so this is
                // reported once there is a screen left to report it on.
                self.reason.set(e);
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

        Self::render_header(frame, header, self.focus);
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

        match outcome.action {
            DetailAction::Save => {
                if let Some(task) = self.tasks.selected_task_mut() {
                    task.notes.clone_from(&self.details.notes);
                    self.activity
                        .push(format!("updated notes for {}", task.title));
                    self.status.set_info("Saved task notes");
                } else {
                    // Silence here would leave the footer reporting the edit
                    // that was not saved.
                    self.status.set_info("No task selected");
                }
            }
            // Esc throws the edits away and reads the selected task again,
            // which is what the panel's hint offers and what `Reset` names.
            DetailAction::Reset => {
                let reloaded = self.tasks.selected_task().is_some();
                self.details.sync_from_task(self.tasks.selected_task());
                // With no task there is nothing to reread, and the panel says
                // so; the footer must not claim otherwise, for the reason the
                // `Save` arm above has an `else`.
                if reloaded {
                    self.status.set_info(outcome.status);
                } else {
                    self.status.set_info("No task selected");
                }
            }
            DetailAction::Keep => self.status.set_info(outcome.status),
        }
    }

    fn update_activity(&mut self, msg: ActivityMessage) {
        self.activity.update(msg);
        self.status.set_info("Cleared activity log");
    }

    fn render_header(frame: &mut Frame, area: Rect, focus: Focus) {
        // `q` is text while the details editor has it, so the hint follows the
        // guard in `handle_key_event` rather than contradicting it.
        let text = if focus == Focus::Details {
            "Dashboard state example | Tab: leave the editor"
        } else {
            "Dashboard state example | Tab: focus | q: quit"
        };
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
            return TaskOutcome::status("No task selected");
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

/// What the details panel wants the root to do with its buffer.
///
/// The panel holds a copy of the selected task's notes and cannot reach the
/// task itself, so both of the interesting outcomes are requests to the root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailAction {
    /// Leave the buffer and the task as they are.
    Keep,
    /// Write the buffer onto the selected task.
    Save,
    /// Throw the buffer away and read the selected task again.
    Reset,
}

#[derive(Debug, Clone)]
struct DetailOutcome {
    status: String,
    action: DetailAction,
}

impl DetailOutcome {
    fn status(status: impl Into<String>) -> Self {
        Self {
            status: status.into(),
            action: DetailAction::Keep,
        }
    }

    fn save(status: impl Into<String>) -> Self {
        Self {
            status: status.into(),
            action: DetailAction::Save,
        }
    }

    fn reset(status: impl Into<String>) -> Self {
        Self {
            status: status.into(),
            action: DetailAction::Reset,
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
            DetailMessage::Reset => DetailOutcome::reset("Reloaded the selected task"),
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

/// Every binding here is for an unmodified key. Raw mode delivers no SIGINT, so
/// a reader pressing Ctrl+C to leave would otherwise have run whatever `c` is
/// bound to — clearing the activity log — and Ctrl+D would have deleted a task.
fn handle_key_event(focus: Focus, key: KeyEvent) -> Command<Message> {
    // The way out of raw mode, from any focus: the details editor holds `q`
    // but nothing holds this.
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Command::message(Message::Quit).into();
    }
    // Every binding below is for an unmodified key, so a chord this
    // application does not bind is answered here rather than falling through
    // to the bare key's. Above the match and not on the character arms,
    // because `Enter`, `Up`/`Down`, `Tab` and `Esc` arrive with modifiers too.
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return Command::none();
    }
    match key.code {
        // Guarded like the other printable keys below, so `q` stays typable in
        // the details editor. This panel is always present, and Esc rereads the
        // selected task's notes rather than leaving, so Tab is how you get out
        // of this focus.
        KeyCode::Char('q') if focus != Focus::Details => Command::message(Message::Quit).into(),
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
    let reason = ExitReason::default();
    let runtime = Runtime::<App>::new(reason.clone());
    let result = runtime.run(&mut terminal).await;

    // Restore terminal
    ratatui::restore();

    // What the run had to say and could not show, now that there is a screen
    // to say it on.
    if let Some(reason) = reason.take() {
        eprintln!("Terminal error: {reason}");
    }

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
    use tears::testing::TestStore;

    /// Builds the `Terminal` message a keystroke would produce, so a test can
    /// exercise the same `handle_key_event` path the subscription feeds.
    fn key(code: KeyCode) -> Message {
        Message::Terminal(Event::Key(KeyEvent::new(code, KeyModifiers::empty())))
    }

    /// The same held with `CONTROL`.
    fn chord(code: KeyCode) -> Message {
        chord_with(code, KeyModifiers::CONTROL)
    }

    /// The same under any modifiers.
    fn chord_with(code: KeyCode, modifiers: KeyModifiers) -> Message {
        Message::Terminal(Event::Key(KeyEvent::new(code, modifiers)))
    }

    /// The same for the other two kinds a terminal can report.
    fn key_of(code: KeyCode, kind: KeyEventKind) -> Message {
        Message::Terminal(Event::Key(KeyEvent::new_with_kind(
            code,
            KeyModifiers::empty(),
            kind,
        )))
    }

    /// Sending the child message directly walks focus through the panels.
    #[test]
    fn focus_next_cycles_through_panels() {
        let mut store = TestStore::<App>::new(ExitReason::default());
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
        let mut store = TestStore::<App>::new(ExitReason::default());

        store.send(key(KeyCode::Tab));
        store.receive_matching(|msg| matches!(msg, Message::FocusNext));

        assert_eq!(store.state().focus, Focus::Tasks);
        store.finish();
    }

    /// Toggling the selected task flips its `done` flag and records an activity
    /// entry.
    #[test]
    fn toggle_marks_selected_task_done() {
        let mut store = TestStore::<App>::new(ExitReason::default());
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
        let mut store = TestStore::<App>::new(ExitReason::default());
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
        let mut store = TestStore::<App>::new(ExitReason::default());

        store.send(key(KeyCode::Char('q')));
        store.receive_matching(|msg| matches!(msg, Message::Quit));
        store.receive_quit();
        store.finish();
    }

    /// The details panel is a text field, so 'q' is a character there rather
    /// than a request to quit. Tab is how you leave that focus.
    #[test]
    fn quit_key_is_text_while_the_details_panel_has_focus() {
        let mut store = TestStore::<App>::new(ExitReason::default());
        store.send(Message::FocusNext);
        store.send(Message::FocusNext);
        assert_eq!(store.state().focus, Focus::Details);
        let notes = store.state().details.notes.clone();

        store.send(key(KeyCode::Char('q')));
        store.receive_matching(|msg| matches!(msg, Message::Details(DetailMessage::Input('q'))));

        assert_eq!(store.state().details.notes, format!("{notes}q"));
        store.finish();
    }

    /// Esc throws the buffer away and reads the selected task again, which is
    /// what the panel's hint offers and what `Reset` names.
    #[test]
    fn escape_rereads_the_selected_task_s_notes() {
        let mut store = TestStore::<App>::new(ExitReason::default());
        store.send(Message::FocusNext);
        store.send(Message::FocusNext);
        assert_eq!(store.state().focus, Focus::Details);
        let original = store.state().details.notes.clone();

        store.send(key(KeyCode::Char('x')));
        store.receive_matching(|msg| matches!(msg, Message::Details(DetailMessage::Input('x'))));
        assert_eq!(store.state().details.notes, format!("{original}x"));

        store.send(key(KeyCode::Esc));
        store.receive_matching(|msg| matches!(msg, Message::Details(DetailMessage::Reset)));

        assert_eq!(
            store.state().details.notes,
            original,
            "the edit is gone and the task's own notes are back"
        );
        store.finish();
    }

    /// A `CONTROL` chord runs no bare-key binding, and Ctrl+C leaves.
    ///
    /// Raw mode delivers no SIGINT, so Ctrl+C arrives as an ordinary key
    /// event; without the guard it would have cleared the activity log, and
    /// Ctrl+D would have deleted the selected task.
    #[test]
    fn a_control_chord_runs_no_bare_binding_and_ctrl_c_leaves() {
        let mut store = TestStore::<App>::new(ExitReason::default());
        store.send(Message::FocusNext);
        assert_eq!(store.state().focus, Focus::Tasks);
        let before = store.state().tasks.tasks.len();

        // Not only the characters: `Enter`, the arrows, `Tab` and `Esc` arrive
        // with modifiers too.
        let selected = store.state().tasks.selected;
        for code in [
            KeyCode::Char('d'),
            KeyCode::Char('n'),
            KeyCode::Char(' '),
            KeyCode::Tab,
            KeyCode::Down,
        ] {
            for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
                store.send(chord_with(code, modifiers));
            }
        }
        assert_eq!(
            store.state().tasks.tasks.len(),
            before,
            "no chord added or deleted a task"
        );
        assert_eq!(
            store.state().tasks.selected,
            selected,
            "and none moved the selection"
        );
        assert_eq!(
            store.state().focus,
            Focus::Tasks,
            "and none cycled the focus"
        );

        store.send(chord(KeyCode::Char('c')));
        store.receive_matching(|msg| matches!(msg, Message::Quit));
        store.receive_quit();
        store.finish();
    }

    /// Saving with no task selected says so rather than leaving the footer
    /// reporting the edit it did not write.
    #[test]
    fn saving_without_a_task_reports_rather_than_going_silent() {
        let mut store = TestStore::<App>::new(ExitReason::default());
        store.send(Message::FocusNext);
        for _ in 0..3 {
            store.send(Message::Tasks(TaskMessage::Delete));
        }
        assert!(store.state().tasks.tasks.is_empty());

        store.send(Message::Details(DetailMessage::Input('a')));
        assert_eq!(store.state().status.message, "Editing notes");

        store.send(Message::Details(DetailMessage::Save));

        assert_eq!(
            store.state().status.message,
            "No task selected",
            "the save wrote nothing and says so"
        );
        store.finish();
    }

    /// Escaping with no task selected says so rather than reporting a reload
    /// that had nothing to read.
    #[test]
    fn escaping_without_a_task_reports_rather_than_claiming_a_reload() {
        let mut store = TestStore::<App>::new(ExitReason::default());
        store.send(Message::FocusNext);
        for _ in 0..3 {
            store.send(Message::Tasks(TaskMessage::Delete));
        }
        assert!(store.state().tasks.tasks.is_empty());

        store.send(Message::Details(DetailMessage::Reset));

        assert_eq!(
            store.state().status.message,
            "No task selected",
            "there was nothing to reread"
        );
        store.finish();
    }

    /// A terminal that reports releases as well as presses sends two events
    /// per keystroke. The release is not an instruction; a repeat is.
    #[test]
    fn a_key_release_asks_for_nothing_and_a_repeat_asks() {
        let mut store = TestStore::<App>::new(ExitReason::default());
        assert_eq!(store.state().focus, Focus::Navigation);

        store.send(key_of(KeyCode::Tab, KeyEventKind::Release));
        assert_eq!(
            store.state().focus,
            Focus::Navigation,
            "the release is not a second Tab"
        );

        // The protocol that reports releases is the one that reports a held
        // key, so a filter written as `== Press` would drop these too.
        store.send(key_of(KeyCode::Tab, KeyEventKind::Repeat));
        store.receive_matching(|msg| matches!(msg, Message::FocusNext));
        assert_eq!(
            store.state().focus,
            Focus::Tasks,
            "a held Tab keeps cycling"
        );
        store.finish();
    }
}
