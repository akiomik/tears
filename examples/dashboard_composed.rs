//! [`dashboard.rs`](dashboard.rs) written with composed reducers.
//!
//! The same application: a navigation list, a task list, a details pane for
//! the selected task, an activity log and a status line, cycled through with
//! Tab. What differs is who owns the wiring.
//!
//! In `dashboard.rs` the root owns it. `App::update` matches every child
//! variant and forwards it, `App::subscriptions` is the root's alone, and any
//! work a task started would be the root's to stop when that task is deleted.
//! That is the right shape while every child is a single instance whose
//! identities the root can keep distinct by hand.
//!
//! Here the task list is a [`Keyed`] collection with one child reducer per
//! row, and the details pane is a [`Slot`] whose occupant comes and goes. Both
//! break that assumption: every row wants the same timer and the same command
//! id, and only the row it belongs to tells two of them apart. So:
//!
//! - [`ReducerExt::scope`] composes the two fixed siblings (`Navigation` and
//!   `Activity`) under segments of their own.
//! - [`ReducerExt::for_each`] composes one `Task` per row, each under its key.
//! - [`ReducerExt::presented`] composes the `Details` occupant.
//! - [`ReducerExt::into_program`] closes the stack with the two root-level
//!   functions composition has no place for: `init` and `view`.
//!
//! Every timer below is declared on the **same interval** and every command
//! keyed with the **same** `CommandId::new(SYNC)`. Written by hand those are
//! collisions: one timer shared by every row, one sync slot for the whole
//! application. Composed they are not, and no `.scoped(...)` call appears
//! anywhere in this file.
//!
//! Removal is the other half. A row leaves through [`Keyed::remove`] or is
//! replaced by [`Keyed::insert`]; the occupant leaves through [`Slot::dismiss`]
//! or a replacing [`Slot::present`]. The boundary turns each of those into a
//! teardown of that instance's scope, so its timer stops and its in-flight sync
//! is cancelled without this file asking for either. The `on_teardown` hooks
//! each child registers write the line you see in the activity pane when it
//! happens — a finalizer produces no message by design, so the log it writes to
//! is a handle rather than a message.
//!
//! Cross-child work stays the root's. Saving the details pane's notes back
//! onto the task it was opened for touches two children, so it is a root
//! message; see `Message::SaveNotes`.
//!
//! Run with: `cargo run --example dashboard_composed`
//! Test with: `cargo test --example dashboard_composed`

use std::num::NonZeroU64;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use color_eyre::eyre::Result;
use crossterm::event::{Event, KeyCode};
use futures::stream;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use tears::ProgramRuntime;
use tears::command::CommandId;
use tears::prelude::*;
use tears::reducer::{Keyed, Reducer, ReducerExt, Slot};
use tears::subscription::terminal::TerminalEvents;
use tears::subscription::time::{Timer, TimerEvent};
use tokio::time::sleep;

/// The interval every timer in this example is declared on.
///
/// Deliberately one constant: `Timer`'s subscription key *is* its interval, so
/// every declaration below carries the same raw identity. What keeps them from
/// collapsing into one subscription is the segment each boundary applies.
const TICK_MS: u64 = 1_000;

/// The command id every stand-in request in this example runs under.
///
/// Deliberately one constant too. Keyed on it without a boundary, two rows'
/// syncs would cancel each other; under `for_each` they are two slots.
const SYNC: &str = "sync";

/// How long the stand-in requests take.
const REQUEST_MS: u64 = 900;

/// How many activity lines the pane keeps.
const ACTIVITY_LIMIT: usize = 8;

/// The activity log, shared by the reducers that write to it and the teardown
/// finalizers that report through it.
///
/// It is a handle rather than a plain `Vec` for one reason:
/// [`Command::on_teardown`] takes a future with `Output = ()`, so a finalizer
/// cannot send a message and needs somewhere to write that outlives the reduce
/// that registered it. Everything else about it is ordinary state — it is the
/// `Activity` child's whole state, and the root view renders it.
#[derive(Clone, Default)]
struct ActivityLog(Arc<Mutex<Vec<String>>>);

impl ActivityLog {
    fn push(&self, entry: String) {
        let mut entries = self.entries_mut();
        entries.push(entry);
        if entries.len() > ACTIVITY_LIMIT {
            entries.remove(0);
        }
    }

    fn clear(&self) {
        self.entries_mut().clear();
    }

    fn entries(&self) -> Vec<String> {
        self.entries_mut().clone()
    }

    fn entries_mut(&self) -> MutexGuard<'_, Vec<String>> {
        self.0
            .lock()
            .expect("the activity log lock is not poisoned")
    }
}

/// The fixed segments this composition uses.
///
/// A `for_each` boundary segments by the row's own key, so only the three
/// fixed-segment boundaries need a value here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Segment {
    Navigation,
    Activity,
    Details,
}

/// A task's key, and therefore its segment at the row boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct TaskId(u32);

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
}

/// What the program is started with.
struct Setup {
    /// The activity log the reducers, the finalizers and the view share.
    activity: ActivityLog,
    /// Messages dispatched at startup.
    ///
    /// `init`'s command is the *root's* command: it crosses no boundary, so
    /// nothing it starts is scoped to a child. Work that belongs to a child
    /// therefore starts as a message routed through the boundary — which is
    /// what these are — rather than as a command returned from here.
    input: Vec<Message>,
    /// Whether the root reads the terminal. The runnable binary does; a test
    /// driving this program scripts `input` instead and leaves the terminal
    /// alone.
    keyboard: bool,
}

#[derive(Debug)]
enum Message {
    Terminal(Event),
    TerminalError(String),
    Quit,
    FocusNext,
    SelectPrev,
    SelectNext,
    AddTask(TaskId, String),
    DeleteTask(TaskId),
    /// Discards a row's instance and starts a fresh one under the same key.
    ReloadTask(TaskId),
    OpenDetails(TaskId),
    CloseDetails,
    /// Writes the occupant's edited notes onto the task it was opened for.
    ///
    /// Two children, so the root does it: a child is handed its own projected
    /// state and can reach neither the collection it sits beside nor the slot
    /// it sits in.
    SaveNotes,
    Navigation(NavigationMessage),
    Task(TaskId, TaskMessage),
    Details(DetailsMessage),
    Activity(ActivityMessage),
}

#[derive(Debug)]
enum NavigationMessage {
    Up,
    Down,
}

#[derive(Debug)]
enum TaskMessage {
    /// The row's one-time setup, routed through the row boundary so the hook it
    /// registers anchors at the row's scope.
    Watch,
    Toggle,
    Sync,
    Synced(String),
    Tick,
}

#[derive(Debug)]
enum DetailsMessage {
    /// The occupant's one-time setup, for the reason [`TaskMessage::Watch`] is.
    Open,
    Loaded(String),
    Input(char),
    Backspace,
    Tick,
}

#[derive(Debug)]
enum ActivityMessage {
    Clear,
}

/// The root state. Each child's state is a field of it.
struct App {
    focus: Focus,
    navigation: NavigationState,
    tasks: Keyed<TaskId, TaskState>,
    details: Slot<DetailsState>,
    activity: ActivityLog,
    status: &'static str,
    selected: Option<TaskId>,
    next_id: u32,
    keyboard: bool,
}

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

    fn selected_label(&self) -> &'static str {
        self.items[self.selected]
    }
}

struct TaskState {
    title: String,
    done: bool,
    notes: String,
    syncing: bool,
    ticks: u32,
}

impl TaskState {
    const fn new(title: String, notes: String) -> Self {
        Self {
            title,
            done: false,
            notes,
            syncing: false,
            ticks: 0,
        }
    }
}

struct DetailsState {
    task: TaskId,
    title: String,
    notes: String,
    loading: bool,
    ticks: u32,
}

impl DetailsState {
    const fn new(task: TaskId, title: String, notes: String) -> Self {
        Self {
            task,
            title,
            notes,
            loading: true,
            ticks: 0,
        }
    }
}

/// A stand-in for a request to a server.
async fn request(what: String) -> String {
    sleep(Duration::from_millis(REQUEST_MS)).await;
    what
}

/// The heartbeat every child with runs of its own declares, on the one shared
/// interval.
fn tick<Msg: Send + 'static>(to_message: fn(TimerEvent) -> Msg) -> Subscription<Msg> {
    Subscription::new(Timer::new(
        NonZeroU64::new(TICK_MS).expect("the tick interval is non-zero"),
    ))
    .map(to_message)
}

/// The root reducer: what none of the boundaries claimed lands here.
struct Root;

impl Reducer for Root {
    type State = App;
    type Message = Message;

    fn reduce(&self, state: &mut App, message: Message) -> Command<Message> {
        match message {
            Message::Terminal(event) => match event {
                Event::Key(key) => key_message(state, key.code)
                    .map_or_else(Command::none, |message| Command::message(message).into()),
                _ => Command::none(),
            },
            Message::TerminalError(error) => {
                state.activity.push(format!("terminal error: {error}"));
                Command::quit()
            }
            Message::Quit => Command::quit(),
            Message::FocusNext => {
                state.focus = state.focus.next();
                state.status = "Changed focus";
                Command::none()
            }
            Message::SelectPrev | Message::SelectNext => {
                let forward = matches!(message, Message::SelectNext);
                select(state, forward);
                Command::none()
            }
            Message::AddTask(id, title) => {
                // Insertion into an absent key records no removal: nothing was
                // running under it to tear down.
                state.tasks.insert(
                    id,
                    TaskState::new(title, "Add notes in the details pane.".to_owned()),
                );
                state.next_id = state.next_id.max(id.0 + 1);
                state.selected = Some(id);
                state.status = "Added a task";
                Command::message(Message::Task(id, TaskMessage::Watch)).into()
            }
            Message::ReloadTask(id) => {
                let Some(task) = state.tasks.get(&id) else {
                    return Command::none();
                };
                // Inserting over an occupied key is a replacement, and a
                // replacement is a removal: the boundary tears the old
                // instance's runs down before this command's spawns start the
                // successor's.
                let title = task.title.clone();
                state.tasks.insert(
                    id,
                    TaskState::new(title, "Reloaded from the server.".to_owned()),
                );
                state.status = "Reloaded the task";
                Command::message(Message::Task(id, TaskMessage::Watch)).into()
            }
            Message::DeleteTask(id) => {
                // `remove` records the removal; the row boundary drains it in
                // this same reduce and merges the row's teardown into the
                // command returned here. Nothing below asks for that.
                let Some(task) = state.tasks.remove(&id) else {
                    return Command::none();
                };
                state.activity.push(format!("deleted: {}", task.title));
                // A details pane open on the row that just left goes with it,
                // and the slot's own boundary originates that teardown.
                if state.details.get().is_some_and(|open| open.task == id) {
                    state.details.dismiss();
                }
                if state.selected == Some(id) {
                    state.selected = state.tasks.keys().next().copied();
                }
                state.status = "Deleted the task";
                Command::none()
            }
            Message::OpenDetails(id) => {
                let Some(task) = state.tasks.get(&id) else {
                    return Command::none();
                };
                // Presenting over an occupied slot is a replacement too, for
                // the same reason `ReloadTask` is.
                state.details.present(DetailsState::new(
                    id,
                    task.title.clone(),
                    task.notes.clone(),
                ));
                state.focus = Focus::Details;
                state.status = "Opened the details pane";
                Command::message(Message::Details(DetailsMessage::Open)).into()
            }
            Message::CloseDetails => {
                state.details.dismiss();
                state.status = "Closed the details pane";
                Command::none()
            }
            Message::SaveNotes => {
                let Some(open) = state.details.get() else {
                    return Command::none();
                };
                let (task_id, notes) = (open.task, open.notes.clone());
                let Some(task) = state.tasks.get_mut(&task_id) else {
                    return Command::none();
                };
                task.notes = notes;
                state
                    .activity
                    .push(format!("updated notes for '{}'", task.title));
                state.status = "Saved the notes";
                // The row's own sync is the row's to run, so it is asked for
                // through the boundary rather than started here.
                Command::message(Message::Task(task_id, TaskMessage::Sync)).into()
            }
            // Claimed by a boundary above this reducer and never routed here.
            Message::Navigation(_)
            | Message::Task(..)
            | Message::Details(_)
            | Message::Activity(_) => Command::none(),
        }
    }

    fn subscriptions(&self, state: &App) -> Vec<Subscription<Message>> {
        if !state.keyboard {
            return Vec::new();
        }
        vec![
            Subscription::new(TerminalEvents::new()).map(|result| match result {
                Ok(event) => Message::Terminal(event),
                Err(error) => Message::TerminalError(error.to_string()),
            }),
        ]
    }
}

/// A fixed sibling with no runs of its own: `scope` buys code organisation
/// here, not identity separation.
struct Navigation;

impl Reducer for Navigation {
    type State = NavigationState;
    type Message = NavigationMessage;

    fn reduce(
        &self,
        state: &mut NavigationState,
        message: NavigationMessage,
    ) -> Command<NavigationMessage> {
        match message {
            NavigationMessage::Up => state.selected = state.selected.saturating_sub(1),
            NavigationMessage::Down => {
                state.selected = (state.selected + 1).min(state.items.len().saturating_sub(1));
            }
        }
        Command::none()
    }
}

/// The other fixed sibling.
struct Activity;

impl Reducer for Activity {
    type State = ActivityLog;
    type Message = ActivityMessage;

    fn reduce(
        &self,
        state: &mut ActivityLog,
        message: ActivityMessage,
    ) -> Command<ActivityMessage> {
        match message {
            ActivityMessage::Clear => state.clear(),
        }
        Command::none()
    }
}

/// One row of the keyed collection.
struct Task {
    activity: ActivityLog,
}

impl Reducer for Task {
    type State = TaskState;
    type Message = TaskMessage;

    fn reduce(&self, state: &mut TaskState, message: TaskMessage) -> Command<TaskMessage> {
        match message {
            TaskMessage::Watch => {
                let activity = self.activity.clone();
                let title = state.title.clone();
                Command::batch([
                    // Registered here rather than at the root, so it anchors at
                    // this row's scope and the row's own teardown selects it. A
                    // registration made outside a boundary anchors at the root,
                    // which no teardown reaches.
                    Command::on_teardown(async move {
                        activity.push(format!("stopped watching: {title}"));
                    }),
                    Command::message(TaskMessage::Sync).into(),
                ])
            }
            TaskMessage::Toggle => {
                state.done = !state.done;
                Command::message(TaskMessage::Sync).into()
            }
            TaskMessage::Sync => {
                state.syncing = true;
                let title = state.title.clone();
                Command::perform(request(title), TaskMessage::Synced)
                    .cancellable(CommandId::new(SYNC))
                    .into()
            }
            TaskMessage::Synced(title) => {
                state.syncing = false;
                self.activity.push(format!("synced: {title}"));
                Command::none()
            }
            TaskMessage::Tick => {
                state.ticks += 1;
                Command::none()
            }
        }
    }

    fn subscriptions(&self, _state: &TaskState) -> Vec<Subscription<TaskMessage>> {
        vec![tick(|TimerEvent::Tick| TaskMessage::Tick)]
    }
}

/// The optionally-present child.
struct Details {
    activity: ActivityLog,
}

impl Reducer for Details {
    type State = DetailsState;
    type Message = DetailsMessage;

    fn reduce(&self, state: &mut DetailsState, message: DetailsMessage) -> Command<DetailsMessage> {
        match message {
            DetailsMessage::Open => {
                let activity = self.activity.clone();
                let title = state.title.clone();
                Command::batch([
                    Command::on_teardown(async move {
                        activity.push(format!("closed details: {title}"));
                    }),
                    Command::perform(request(state.title.clone()), DetailsMessage::Loaded)
                        // The same id every row's sync uses. Two boundaries,
                        // two slots.
                        .cancellable(CommandId::new(SYNC))
                        .into(),
                ])
            }
            DetailsMessage::Loaded(title) => {
                state.loading = false;
                self.activity.push(format!("loaded details: {title}"));
                Command::none()
            }
            DetailsMessage::Input(character) => {
                state.notes.push(character);
                Command::none()
            }
            DetailsMessage::Backspace => {
                state.notes.pop();
                Command::none()
            }
            DetailsMessage::Tick => {
                state.ticks += 1;
                Command::none()
            }
        }
    }

    fn subscriptions(&self, _state: &DetailsState) -> Vec<Subscription<DetailsMessage>> {
        vec![tick(|TimerEvent::Tick| DetailsMessage::Tick)]
    }
}

/// The composition: one root and four boundaries over it.
///
/// Each call adds one boundary and the result is still a reducer over the
/// root's state and message, which is why they chain. The outermost boundary
/// gets first refusal on a message; what no boundary claims reaches [`Root`].
fn dashboard(activity: ActivityLog) -> impl Reducer<State = App, Message = Message> {
    Root.scope(
        Navigation,
        Segment::Navigation,
        |state| &state.navigation,
        |state| &mut state.navigation,
        |message| match message {
            Message::Navigation(inner) => Ok(inner),
            other => Err(other),
        },
        Message::Navigation,
    )
    .scope(
        Activity,
        Segment::Activity,
        |state| &state.activity,
        |state| &mut state.activity,
        |message| match message {
            Message::Activity(inner) => Ok(inner),
            other => Err(other),
        },
        Message::Activity,
    )
    .for_each(
        Task {
            activity: activity.clone(),
        },
        |state| &state.tasks,
        |state| &mut state.tasks,
        |message| match message {
            Message::Task(id, inner) => Ok((id, inner)),
            other => Err(other),
        },
        Message::Task,
    )
    .presented(
        Details { activity },
        Segment::Details,
        |state| &state.details,
        |state| &mut state.details,
        |message| match message {
            Message::Details(inner) => Ok(inner),
            other => Err(other),
        },
        Message::Details,
    )
}

/// The root's initial state and the command dispatched at bootstrap.
fn init(setup: Setup) -> (App, Command<Message>) {
    let Setup {
        activity,
        input,
        keyboard,
    } = setup;
    let state = App {
        focus: Focus::Navigation,
        navigation: NavigationState::new(),
        tasks: Keyed::new(),
        details: Slot::empty(),
        activity,
        status: "Ready",
        selected: None,
        next_id: 1,
        keyboard,
    };
    (state, Command::stream(stream::iter(input)).into())
}

/// The tasks the runnable binary starts with, as the startup messages that
/// route each one through the row boundary.
fn seed() -> Vec<Message> {
    [
        "Review release checklist",
        "Update onboarding guide",
        "Plan next subscription API",
    ]
    .into_iter()
    .enumerate()
    .map(|(index, title)| {
        let id = u32::try_from(index).expect("three seeds fit") + 1;
        Message::AddTask(TaskId(id), title.to_owned())
    })
    .collect()
}

/// The root view. `Reducer` has no `view` and only `Program` does, so
/// composing child panes is ordinary function calls over the root state.
fn view(state: &App, frame: &mut Frame<'_>) {
    let [header, body, activity, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(8),
        Constraint::Length(7),
        Constraint::Length(3),
    ])
    .areas(frame.area());

    let [nav_area, tasks_area, details_area] = Layout::horizontal([
        Constraint::Length(24),
        Constraint::Percentage(40),
        Constraint::Percentage(60),
    ])
    .areas(body);

    frame.render_widget(
        Paragraph::new(format!(
            "Composed dashboard · {} | Tab: focus | q: quit",
            state.navigation.selected_label()
        ))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Reducer Composition"),
        ),
        header,
    );
    render_navigation(state, frame, nav_area);
    render_tasks(state, frame, tasks_area);
    render_details(state, frame, details_area);
    render_activity(state, frame, activity);
    frame.render_widget(
        Paragraph::new(format!("Status: {}", state.status))
            .block(Block::default().borders(Borders::ALL).title("Status")),
        footer,
    );
}

fn pane_title(label: &str, focused: bool) -> String {
    if focused {
        format!("{label} [focused]")
    } else {
        label.to_owned()
    }
}

fn render_navigation(state: &App, frame: &mut Frame<'_>, area: Rect) {
    let items = state
        .navigation
        .items
        .iter()
        .enumerate()
        .map(|(index, label)| {
            let marker = if index == state.navigation.selected {
                ">"
            } else {
                " "
            };
            ListItem::new(format!("{marker} {label}"))
        });
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .title(pane_title("Navigation", state.focus == Focus::Navigation)),
        ),
        area,
    );
}

fn render_tasks(state: &App, frame: &mut Frame<'_>, area: Rect) {
    let items = state.tasks.iter().map(|(id, task)| {
        let marker = if state.selected == Some(*id) {
            ">"
        } else {
            " "
        };
        let checkbox = if task.done { "[x]" } else { "[ ]" };
        let sync = if task.syncing { " (syncing)" } else { "" };
        ListItem::new(format!(
            "{marker} {checkbox} {} · {}s{sync}",
            task.title, task.ticks
        ))
    });
    frame.render_widget(
        List::new(items).block(Block::default().borders(Borders::ALL).title(format!(
            "{} (up/down, space, n, d, r, enter)",
            pane_title("Tasks", state.focus == Focus::Tasks)
        ))),
        area,
    );
}

fn render_details(state: &App, frame: &mut Frame<'_>, area: Rect) {
    let text = state.details.get().map_or_else(
        || "No details open. Select a task and press Enter.".to_owned(),
        |open| {
            format!(
                "{} · {}s{}\n\nNotes:\n{}_\n\nType to edit, Enter to save, Esc to close.",
                open.title,
                open.ticks,
                if open.loading { " (loading)" } else { "" },
                open.notes
            )
        },
    );
    frame.render_widget(
        Paragraph::new(text).wrap(Wrap { trim: false }).block(
            Block::default()
                .borders(Borders::ALL)
                .title(pane_title("Details", state.focus == Focus::Details)),
        ),
        area,
    );
}

fn render_activity(state: &App, frame: &mut Frame<'_>, area: Rect) {
    let items: Vec<ListItem<'_>> = state
        .activity
        .entries()
        .iter()
        .rev()
        .map(|entry| ListItem::new(format!("- {entry}")))
        .collect();
    frame.render_widget(
        List::new(items).block(Block::default().borders(Borders::ALL).title(format!(
            "{} (c: clear)",
            pane_title("Activity", state.focus == Focus::Activity)
        ))),
        area,
    );
}

/// Moves the task selection, wrapping.
fn select(state: &mut App, forward: bool) {
    let keys: Vec<TaskId> = state.tasks.keys().copied().collect();
    if keys.is_empty() {
        return;
    }
    let position = state
        .selected
        .and_then(|selected| keys.iter().position(|key| *key == selected));
    let next = match position {
        Some(index) if forward => (index + 1) % keys.len(),
        Some(index) => (index + keys.len() - 1) % keys.len(),
        None => 0,
    };
    state.selected = keys.get(next).copied();
    state.status = "Selected another task";
}

/// The message a key press asks for, if any.
fn key_message(state: &App, code: KeyCode) -> Option<Message> {
    match code {
        KeyCode::Char('q') => Some(Message::Quit),
        KeyCode::Tab => Some(Message::FocusNext),
        KeyCode::Up => match state.focus {
            Focus::Navigation => Some(Message::Navigation(NavigationMessage::Up)),
            Focus::Tasks => Some(Message::SelectPrev),
            _ => None,
        },
        KeyCode::Down => match state.focus {
            Focus::Navigation => Some(Message::Navigation(NavigationMessage::Down)),
            Focus::Tasks => Some(Message::SelectNext),
            _ => None,
        },
        KeyCode::Char(' ') if state.focus == Focus::Tasks => state
            .selected
            .map(|id| Message::Task(id, TaskMessage::Toggle)),
        KeyCode::Char('n') if state.focus == Focus::Tasks => Some(Message::AddTask(
            TaskId(state.next_id),
            format!("New task {}", state.next_id),
        )),
        KeyCode::Char('d') if state.focus == Focus::Tasks => {
            state.selected.map(Message::DeleteTask)
        }
        KeyCode::Char('r') if state.focus == Focus::Tasks => {
            state.selected.map(Message::ReloadTask)
        }
        KeyCode::Enter if state.focus == Focus::Tasks => state.selected.map(Message::OpenDetails),
        KeyCode::Char('c') if state.focus == Focus::Activity => {
            Some(Message::Activity(ActivityMessage::Clear))
        }
        KeyCode::Enter if state.focus == Focus::Details => Some(Message::SaveNotes),
        KeyCode::Esc if state.focus == Focus::Details => Some(Message::CloseDetails),
        KeyCode::Backspace if state.focus == Focus::Details => {
            Some(Message::Details(DetailsMessage::Backspace))
        }
        KeyCode::Char(character) if state.focus == Focus::Details => {
            Some(Message::Details(DetailsMessage::Input(character)))
        }
        _ => None,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    let activity = ActivityLog::default();
    activity.push("Application started".to_owned());
    let setup = Setup {
        activity: activity.clone(),
        input: seed(),
        keyboard: true,
    };
    let program = dashboard(activity).into_program(init, view);

    let mut terminal = ratatui::init();
    let result = ProgramRuntime::new(program, setup).run(&mut terminal).await;
    ratatui::restore();

    let _exit = result?;
    Ok(())
}

/// Deterministic tests for this example's composition, driven by
/// `tears::testing::TestDriver` (RFC 0008 §9). `TestStore` takes an
/// `Application`, which is what `dashboard.rs`'s tests use; a composed
/// `Program` is driven here instead. Run them with
/// `cargo test --example dashboard_composed`.
#[cfg(test)]
mod tests {
    use super::*;

    use ratatui::Terminal;
    use ratatui::backend::{Backend, TestBackend};
    use tears::reducer::Program;
    use tears::testing::{Confirmed, RunKind, RunName, StepReport, TestDriver, WakeSource};
    use tears::{RuntimeConfig, SubscriptionId};

    /// The turn budget every driving call below states.
    const TURNS: usize = 64;

    /// The program `main` runs, driven message by message.
    ///
    /// The same `dashboard` stack, closed with the same `init` and the same
    /// `view`. Only the `Setup` differs: a scripted `input` instead of the
    /// binary's seed, and `keyboard` off so the run reads no terminal.
    fn scripted(
        activity: &ActivityLog,
        input: Vec<Message>,
    ) -> TestDriver<impl Program<Flags = Setup>, TestBackend> {
        let setup = Setup {
            activity: activity.clone(),
            input,
            keyboard: false,
        };
        let terminal = Terminal::new(TestBackend::new(80, 24)).expect("the test backend is built");
        TestDriver::new(
            dashboard(activity.clone()).into_program(init, view),
            setup,
            RuntimeConfig::new(),
            terminal,
        )
    }

    /// Releases one of `run`'s sends and runs the pass that delivers it.
    fn deliver<P: Program, B: Backend>(
        driver: &mut TestDriver<P, B>,
        run: &RunName,
    ) -> StepReport<B::Error> {
        let token = driver.grant(run.clone()).expect("no grant is outstanding");
        assert_eq!(
            driver.confirm(TURNS, token),
            Confirmed::Accepted,
            "the released send reached the data lane"
        );
        let report = driver
            .step_pass(WakeSource::Data)
            .expect("the data lane is ready");
        assert!(
            report.terminated.is_none(),
            "the pass did not terminate the program"
        );
        report
    }

    /// The one anonymous run a step started — the message a reduce asked for.
    fn anonymous<E>(report: &StepReport<E>, what: &str) -> RunName {
        one(
            report
                .started
                .iter()
                .filter(|run| run.kind() == RunKind::Anonymous)
                .cloned()
                .collect(),
            what,
        )
    }

    /// The subscription identities a step started.
    fn subscriptions<E>(report: &StepReport<E>) -> Vec<SubscriptionId> {
        report
            .started
            .iter()
            .filter_map(|run| match run.kind() {
                RunKind::Subscription(id) => Some(id),
                RunKind::Keyed(_) | RunKind::Anonymous => None,
            })
            .collect()
    }

    /// The command identities the keyed runs a step started carry.
    fn keys<E>(report: &StepReport<E>) -> Vec<CommandId> {
        report
            .started
            .iter()
            .filter_map(|run| match run.kind() {
                RunKind::Keyed(id) => Some(id),
                RunKind::Subscription(_) | RunKind::Anonymous => None,
            })
            .collect()
    }

    fn one<T>(mut values: Vec<T>, what: &str) -> T {
        assert_eq!(values.len(), 1, "expected exactly one {what}");
        values.pop().expect("the length was just asserted")
    }

    fn recorded(activity: &ActivityLog, line: &str) -> bool {
        activity.entries().iter().any(|entry| entry == line)
    }

    /// Adds one task and drives it to the keyed sync its row runs, returning
    /// the identities that row's boundary produced.
    fn add_task<P: Program, B: Backend>(
        driver: &mut TestDriver<P, B>,
        script: &RunName,
    ) -> (SubscriptionId, CommandId) {
        let report = deliver(driver, script);
        let timer = one(subscriptions(&report), "row timer");
        let watch = anonymous(&report, "row setup message");
        let report = deliver(driver, &watch);
        let sync = anonymous(&report, "row sync message");
        (timer, one(keys(&deliver(driver, &sync)), "row sync"))
    }

    /// Every timer in this composition is declared on one interval and every
    /// request keyed under one command id, and no two of them collide.
    ///
    /// That is the boundary's doing and nothing else's: `dashboard` writes no
    /// `.scoped(...)`, and neither does any reducer in this file.
    #[test]
    fn identical_child_identities_stay_apart_under_their_boundaries() {
        let activity = ActivityLog::default();
        let mut driver = scripted(
            &activity,
            vec![
                Message::AddTask(TaskId(1), "alpha".to_owned()),
                Message::AddTask(TaskId(2), "beta".to_owned()),
                Message::OpenDetails(TaskId(1)),
            ],
        );

        // Nothing declares a subscription over state that is not there yet, so
        // bootstrap starts the scripted input and nothing else.
        let boot = driver.boot();
        assert!(boot.terminated.is_none(), "bootstrap did not terminate");
        assert!(
            subscriptions(&boot).is_empty(),
            "no row and no occupant, so no timer"
        );
        let script = anonymous(&boot, "scripted input run");

        let (alpha_timer, alpha_sync) = add_task(&mut driver, &script);
        let (beta_timer, beta_sync) = add_task(&mut driver, &script);

        // The occupant declares the same timer again and keys its load with
        // the same id, one boundary over.
        let report = deliver(&mut driver, &script);
        let details_timer = one(subscriptions(&report), "occupant timer");
        let open = anonymous(&report, "occupant setup message");
        let details_load = one(keys(&deliver(&mut driver, &open)), "occupant load");

        assert_ne!(
            alpha_timer, beta_timer,
            "each row's timer is qualified by that row's key, so the two rows declaring the same \
             interval are two subscriptions"
        );
        assert_ne!(
            alpha_timer, details_timer,
            "and the slot's segment separates the occupant's timer from a row's"
        );
        assert_ne!(
            alpha_sync, beta_sync,
            "each row's sync is keyed under that row's segment, so one row's sync cannot cancel \
             another's"
        );
        assert_ne!(
            alpha_sync, details_load,
            "nor can the occupant's load, one boundary over"
        );
        assert_ne!(
            alpha_sync,
            CommandId::new(SYNC),
            "and no run occupies the unqualified id the reducers wrote"
        );
    }

    /// The four removal shapes a boundary tears down, and the one thing that is
    /// not a removal.
    ///
    /// Nothing in this file calls `Command::teardown`: each `stopped watching`
    /// and `closed details` line below is a hook a child registered, fired by
    /// the teardown its boundary originated when the instance left.
    #[test]
    fn every_removal_tears_its_instance_down() {
        let activity = ActivityLog::default();
        let mut driver = scripted(
            &activity,
            vec![
                Message::AddTask(TaskId(1), "alpha".to_owned()),
                Message::AddTask(TaskId(2), "beta".to_owned()),
                Message::OpenDetails(TaskId(1)),
                // Presenting over an occupied slot: a replacement, which is a
                // removal of the occupant.
                Message::OpenDetails(TaskId(2)),
                // Inserting over an occupied key: the same, one collection
                // over.
                Message::ReloadTask(TaskId(2)),
                // A row leaving the keyed collection.
                Message::DeleteTask(TaskId(1)),
                // An occupied slot dismissed.
                Message::CloseDetails,
            ],
        );

        let boot = driver.boot();
        assert!(boot.terminated.is_none(), "bootstrap did not terminate");
        let script = anonymous(&boot, "scripted input run");

        // Both rows added and set up, so both have a hook registered under
        // their own key.
        add_task(&mut driver, &script);
        add_task(&mut driver, &script);
        assert!(
            activity.entries().is_empty(),
            "an insert into an absent key removes no instance, so nothing is torn down"
        );

        // The slot's first occupant, set up the same way.
        let report = deliver(&mut driver, &script);
        let open = anonymous(&report, "occupant setup message");
        deliver(&mut driver, &open);

        // Replacing the occupant removes it.
        let report = deliver(&mut driver, &script);
        driver.settle(TURNS, || recorded(&activity, "closed details: alpha"));
        let open = anonymous(&report, "occupant setup message");
        deliver(&mut driver, &open);

        // Replacing a row removes it, and the successor starts fresh under the
        // same key.
        let report = deliver(&mut driver, &script);
        driver.settle(TURNS, || recorded(&activity, "stopped watching: beta"));
        let watch = anonymous(&report, "row setup message");
        deliver(&mut driver, &watch);

        // Removing a row removes it, and leaves the other row alone.
        deliver(&mut driver, &script);
        driver.settle(TURNS, || recorded(&activity, "stopped watching: alpha"));

        // Dismissing the slot removes its occupant.
        deliver(&mut driver, &script);
        driver.settle(TURNS, || recorded(&activity, "closed details: beta"));

        assert_eq!(
            activity.entries(),
            vec![
                "closed details: alpha".to_owned(),
                "stopped watching: beta".to_owned(),
                // The root's own line, written by the reduce that removed the
                // row, before the teardown it originated ran.
                "deleted: alpha".to_owned(),
                "stopped watching: alpha".to_owned(),
                "closed details: beta".to_owned(),
            ],
            "one teardown per removal, in removal order, and none for the successor row that is \
             still present"
        );
    }
}
