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
//! A diff of the two files shows more than the wiring. What is worth knowing
//! before reading one:
//!
//! - The **row children and the pane's occupant** have runs of their own — a
//!   timer and a keyed request each — where `dashboard.rs` has neither, its one
//!   subscription being the root's terminal source. That is not incidental:
//!   qualification and teardown are about runs, so a child with none gives a
//!   boundary nothing to do. `Navigation` and `Activity` are exactly that case
//!   and are composed anyway, for the organisation rather than the separation.
//! - **Three keys are not the same.** `r` reloads a row, Enter opens the pane
//!   from the task list, and Esc closes the pane, where `dashboard.rs` rereads
//!   the selected task's notes into a panel that is always there.
//! - **The activity log carries the same entries and more of its own.** The
//!   teardown lines a removal fires; the completion lines the children's runs
//!   write (`synced:`, `loaded details:`), which `dashboard.rs` has no
//!   counterpart for because nothing there completes with a value later — its
//!   commands hand a message straight back; `reloaded:`, for a key it
//!   does not have; and the terminal-error line, which this file records and
//!   `main` prints after restoring the terminal, where `dashboard.rs` prints it
//!   from inside the run. Every row-scoped line names the row's key, because
//!   here there is one and two rows added with `n` share a title.
//! - **The status line is fixed before the child runs**, for every operation
//!   a boundary claims. `dashboard.rs`'s root runs those updates itself and
//!   can report what they produced — "Selected section: Today"; here the root
//!   sets the line where it dispatches the message, so nothing the child
//!   computes can appear in it.
//! - **The details buffer does not follow the selection.** `dashboard.rs`
//!   rereads the panel from the selected task on every task message, so moving
//!   the selection throws an unsaved edit away; here the occupant is fixed to
//!   the task it was opened for, because a `Slot` holds an instance rather than
//!   a view of whatever is selected.
//! - **The tasks arrive as messages** rather than being built into the initial
//!   state. `init` could build them — `Keyed::from_iter` records no removal, so
//!   growing a collection there is fine — and this file routes them through
//!   `AddTask` so the seed and the `n` key take one path. What `init`'s command
//!   cannot do is start work *under a child's scope*, so the row's own setup is
//!   a message either way. That the rows start with the same notes and none
//!   marked done is `AddTask`'s doing rather than composition's: it carries a
//!   title and nothing else.
//!
//! Run with: `cargo run --example dashboard_composed`
//! Test with: `cargo test --example dashboard_composed`

use std::num::NonZeroU64;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use color_eyre::eyre::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
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

/// What a terminal-error line is written under.
///
/// Named once because two places have to agree on it: the reduce that records
/// the reason, and the `main` that reads it back after restoring the terminal.
/// A literal at each end would let one change silently and leave the other
/// matching nothing.
const TERMINAL_ERROR: &str = "terminal error: ";

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
    /// A key that needs a selected row arrived with none.
    ///
    /// A message rather than nothing, so the footer stops describing whatever
    /// came before it — the reason `select` sets its status before the guard
    /// that returns early on an empty collection.
    NoTaskSelected,
    /// Adds a task under a **freshly allocated** key.
    ///
    /// The key is chosen by the reduce and not by the caller on purpose. A
    /// caller reading `next_id` off the state reads a value the `AddTask`
    /// messages already in flight have not been applied to yet, so two of them
    /// would name the same key — and inserting over an occupied key is a
    /// replacement, which would tear the first row down instead of adding a
    /// second.
    AddTask(String),
    DeleteTask(TaskId),
    /// Discards a row's instance and starts a fresh one under the same key.
    ///
    /// The successor is the server's version of the task, so everything local
    /// to the instance that left is gone with it: the `done` flag, and any
    /// notes `SaveNotes` had written into it. That is what a replacement is
    /// here — the old instance is torn down and the new one starts fresh — and
    /// carrying state across would make it something else.
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
    ///
    /// **Handled idempotently**, because nothing guarantees it arrives once: a
    /// key press asks for it through `Command::message`, so two presses decided
    /// against the same state produce two of these, and every registration a
    /// scope holds is fired by the teardown that selects it. A second
    /// registration would put two lines in the activity log for one removal.
    Watch,
    Toggle,
    Sync,
    Synced(String),
    Tick,
}

#[derive(Debug)]
enum DetailsMessage {
    /// The occupant's one-time setup, for the reason [`TaskMessage::Watch`] is,
    /// and handled idempotently for the same reason.
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
    /// The row's own key.
    ///
    /// Redundant with the key the collection holds it under, and kept anyway:
    /// a child is handed its own state and nothing else, so this is how a hook
    /// it registers can name which row it was.
    id: TaskId,
    title: String,
    done: bool,
    notes: String,
    /// Whether this instance's setup has already run; see [`TaskMessage::Watch`].
    watched: bool,
    syncing: bool,
    ticks: u32,
}

impl TaskState {
    const fn new(id: TaskId, title: String, notes: String) -> Self {
        Self {
            id,
            title,
            done: false,
            notes,
            watched: false,
            syncing: false,
            ticks: 0,
        }
    }
}

struct DetailsState {
    task: TaskId,
    title: String,
    notes: String,
    /// Whether this instance's setup has already run; see [`DetailsMessage::Open`].
    opened: bool,
    loading: bool,
    ticks: u32,
}

impl DetailsState {
    const fn new(task: TaskId, title: String, notes: String) -> Self {
        Self {
            task,
            title,
            notes,
            opened: false,
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
                // Not releases. A terminal that reports them — Windows, or a
                // session with the kitty keyboard protocol on — sends two
                // events for one press, and the second would be acted on: `n`
                // would add two rows where one was asked for, and Enter would
                // open the pane and then replace it with a second pane on the
                // same row — a removal nobody asked for, in a file about which
                // removals happen.
                //
                // A *repeat* is kept, and is the other thing entirely: a held
                // key is repetition the reader is asking for. It is not the
                // same as the duplicate above, though — `open_details` moves
                // the focus, so a held Enter opens the pane once and then
                // saves into it, while the press and its release are both
                // decoded before either lands and so both see the focus that
                // opens. Testing for `Press` would drop the repeats with the
                // releases, and a held Down would move the selection once.
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    key_message(state, key).map_or_else(Command::none, |message| {
                        // A boundary claims a child-addressed message, so the
                        // root never sees it land and the arms for those below
                        // are unreachable. Its status is set here, at the
                        // dispatch, which is why the line says what was asked
                        // for rather than what happened: what happened is the
                        // child's, and a child reports to its own state.
                        if let Some(status) = child_status(&message) {
                            state.status = status;
                        }
                        Command::message(message).into()
                    })
                }
                _ => Command::none(),
            },
            Message::TerminalError(error) => {
                // Recorded rather than printed. The quit below ends the run
                // and `main` restores the terminal immediately after, so this
                // is the only report there is — and `main` prints it once the
                // screen it would have been drawn on is gone.
                state.activity.push(format!("{TERMINAL_ERROR}{error}"));
                Command::quit()
            }
            Message::Quit => Command::quit(),
            Message::NoTaskSelected => {
                state.status = "No task selected";
                Command::none()
            }
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
            Message::AddTask(title) => add_task(state, title),
            Message::ReloadTask(id) => reload_task(state, id),
            Message::DeleteTask(id) => delete_task(state, id),
            Message::OpenDetails(id) => open_details(state, id),
            Message::CloseDetails => {
                // Esc is guarded on the focus rather than on `editing_notes`,
                // so it is the way out of a `Details` focus with nothing behind
                // it — which is also why the status has to say which of the two
                // happened. Dismissing an empty slot removes no instance and
                // records nothing.
                state.status = if close_details(state) {
                    "Closed the details pane"
                } else {
                    "Left the details pane"
                };
                Command::none()
            }
            Message::SaveNotes => save_notes(state),
            // Claimed by a boundary above this reducer and never routed here;
            // `child_status` is where these get their status line.
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
                if state.watched {
                    // Inert, like the occupant's re-entry path: the first
                    // `Watch` this instance saw already asked for the sync, and
                    // a second request here would cancel that one in flight
                    // under the shared id. Idempotent in its effect and not
                    // only in its registration.
                    return Command::none();
                }
                state.watched = true;
                let activity = self.activity.clone();
                let title = format!("#{} {}", state.id.0, state.title);
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
                let verb = if state.done { "completed" } else { "reopened" };
                self.activity
                    .push(format!("{verb}: #{} {}", state.id.0, state.title));
                Command::message(TaskMessage::Sync).into()
            }
            TaskMessage::Sync => {
                state.syncing = true;
                // Named the way the teardown line is: two rows added with `n`
                // share a title, and an activity log that cannot tell them
                // apart is the one thing this example must not have.
                let title = format!("#{} {}", state.id.0, state.title);
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
                if state.opened {
                    return Command::none();
                }
                state.opened = true;
                let activity = self.activity.clone();
                // Named the way every other row-scoped line is; see
                // `TaskMessage::Sync`.
                let name = format!("#{} {}", state.task.0, state.title);
                let title = name.clone();
                Command::batch([
                    Command::on_teardown(async move {
                        activity.push(format!("closed details: {title}"));
                    }),
                    Command::perform(request(name), DetailsMessage::Loaded)
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
    .map(|label| Message::AddTask(label.to_owned()))
    .collect()
}

/// The root view. `Reducer` has no `view` and only `Program` does, so
/// composing child panes is ordinary function calls over the root state.
fn view(state: &App, frame: &mut Frame<'_>) {
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

    frame.render_widget(
        Paragraph::new(format!(
            "Composed dashboard · {} | Tab: focus | {}",
            state.navigation.selected_label(),
            // `q` is text while the notes editor has it, so the hint follows
            // the guard rather than contradicting it.
            if editing_notes(state) {
                "esc: close pane"
            } else {
                "q: quit"
            }
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
            "{marker} {checkbox} #{} {} · {}s{sync}",
            id.0, task.title, task.ticks
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
                "#{} {} · {}s{}\n\nNotes:\n{}_\n\nType to edit, Enter to save, Esc to close.",
                open.task.0,
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

/// Dismisses the details pane, reporting whether there was an occupant to
/// dismiss.
///
/// Dismissal is a removal, so the slot's boundary tears the occupant's runs
/// down — but only when there was one, which is why the caller is told: an
/// empty slot records nothing, and the status must not claim a close that did
/// not happen.
///
/// Restoring the focus is this function's other half, and it guards nothing:
/// [`editing_notes`] already asks the slot, so a `Details` focus over an empty
/// slot swallows no key. What it buys is that the focus does not sit on a pane
/// the user cannot see.
fn close_details(state: &mut App) -> bool {
    let dismissed = state.details.dismiss().is_some();
    if state.focus == Focus::Details {
        state.focus = Focus::Tasks;
    }
    dismissed
}

/// The same, but only when the pane is open on `task`.
fn close_details_opened_on(state: &mut App, task: TaskId) {
    if state.details.get().is_some_and(|open| open.task == task) {
        close_details(state);
    }
}

/// Adds a task under a freshly allocated key.
fn add_task(state: &mut App, title: String) -> Command<Message> {
    let id = TaskId(state.next_id);
    state.next_id += 1;
    // Insertion into an absent key records no removal: nothing was running
    // under it to tear down. The key is fresh, so this is that case and never
    // the replacing one.
    state.activity.push(format!("added: #{} {}", id.0, title));
    state.tasks.insert(
        id,
        TaskState::new(id, title, "Add notes in the details pane.".to_owned()),
    );
    state.selected = Some(id);
    state.status = "Added a task";
    Command::message(Message::Task(id, TaskMessage::Watch)).into()
}

/// Replaces a row's instance with a fresh one under the same key.
fn reload_task(state: &mut App, id: TaskId) -> Command<Message> {
    let Some(task) = state.tasks.get(&id) else {
        return Command::none();
    };
    // Inserting over an occupied key is a replacement, and a replacement is a
    // removal: the boundary tears the old instance's runs down before this
    // command's spawns start the successor's.
    let task_title = task.title.clone();
    state.tasks.insert(
        id,
        TaskState::new(
            id,
            task_title.clone(),
            "Reloaded from the server.".to_owned(),
        ),
    );
    // The pane was opened for the instance that just left, and it holds that
    // instance's title and notes; leaving it open would let a later
    // `SaveNotes` write them back over the reload.
    close_details_opened_on(state, id);
    state
        .activity
        .push(format!("reloaded: #{} {}", id.0, task_title));
    // Says what the successor threw away, because a cleared checkbox is
    // otherwise the only sign that it did.
    state.status = "Reloaded the task, discarding its local state";
    Command::message(Message::Task(id, TaskMessage::Watch)).into()
}

/// Removes a row.
fn delete_task(state: &mut App, id: TaskId) -> Command<Message> {
    // Read before the removal, because it is a position in the collection and
    // the removal is what changes it.
    let position = state.tasks.keys().position(|key| *key == id);
    // `remove` records the removal; the row boundary drains it in this same
    // reduce and merges the row's teardown into the command returned here.
    // Nothing below asks for that.
    let Some(task) = state.tasks.remove(&id) else {
        return Command::none();
    };
    state
        .activity
        .push(format!("deleted: #{} {}", id.0, task.title));
    // A details pane open on the row that just left goes with it, and the
    // slot's own boundary originates that teardown.
    close_details_opened_on(state, id);
    if state.selected == Some(id) {
        // The row that moved up into the position, or the new last row when
        // the deleted one was last — `dashboard.rs`'s behaviour, expressed
        // over keys instead of indices.
        let keys: Vec<TaskId> = state.tasks.keys().copied().collect();
        state.selected = position
            .map(|index| index.min(keys.len().saturating_sub(1)))
            .and_then(|index| keys.get(index).copied());
    }
    state.status = "Deleted the task";
    Command::none()
}

/// Presents the details pane for a row.
fn open_details(state: &mut App, id: TaskId) -> Command<Message> {
    let Some(task) = state.tasks.get(&id) else {
        return Command::none();
    };
    // Presenting over an occupied slot is a replacement too, for the same
    // reason `ReloadTask` is.
    state.details.present(DetailsState::new(
        id,
        task.title.clone(),
        task.notes.clone(),
    ));
    state.focus = Focus::Details;
    state.status = "Opened the details pane";
    Command::message(Message::Details(DetailsMessage::Open)).into()
}

/// Writes the pane's edited notes onto the task it was opened for.
fn save_notes(state: &mut App) -> Command<Message> {
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
        .push(format!("updated notes for #{} {}", task_id.0, task.title));
    state.status = "Saved the notes";
    // The row's own sync is the row's to run, so it is asked for through the
    // boundary rather than started here.
    Command::message(Message::Task(task_id, TaskMessage::Sync)).into()
}

/// Moves the task selection, stopping at the ends.
///
/// Clamped and not wrapped, because `dashboard.rs` clamps: the pair is meant
/// to differ in structure, and a selection that wrapped here would read as
/// something composition did.
fn select(state: &mut App, forward: bool) {
    // Before the guard below, so an empty list still moves the line: every
    // other operation reports, and `dashboard.rs` reports here too, so going
    // silent would leave the footer describing whatever came before it.
    state.status = "Selected another task";
    let keys: Vec<TaskId> = state.tasks.keys().copied().collect();
    if keys.is_empty() {
        return;
    }
    let position = state
        .selected
        .and_then(|selected| keys.iter().position(|key| *key == selected));
    let last = keys.len() - 1;
    let next = match position {
        Some(index) if forward => (index + 1).min(last),
        Some(index) => index.saturating_sub(1),
        None => 0,
    };
    state.selected = keys.get(next).copied();
}

/// Whether a key press is going into the notes editor.
///
/// Focus alone does not answer that. The pane lives in a `Slot`, and
/// [`Focus::next`] walks through `Details` whether or not the slot is occupied,
/// so most of the time that focus has no editor behind it. A guard written
/// against the focus would swallow keys — `q` included — into a boundary with
/// no occupant to claim them.
fn editing_notes(state: &App) -> bool {
    state.focus == Focus::Details && state.details.is_present()
}

/// The status line a child-addressed message deserves.
///
/// Read at the dispatch rather than at the landing, because a boundary claims
/// these and the root's `reduce` never sees them. That fixes what the line can
/// say before the child has run: nothing the child computes can appear in it,
/// so where `dashboard.rs` reports "Selected section: Today" — its root having
/// run the child update itself — these name the operation and stop there.
const fn child_status(message: &Message) -> Option<&'static str> {
    match message {
        Message::Navigation(_) => Some("Selected another section"),
        Message::Task(_, TaskMessage::Toggle) => Some("Toggled the task"),
        Message::Activity(_) => Some("Cleared the activity log"),
        Message::Details(_) => Some("Editing the notes"),
        _ => None,
    }
}

/// The message a key press asks for, if any.
///
/// Every binding here is for an unmodified key. Raw mode delivers no SIGINT, so
/// a reader pressing Ctrl+C to leave would otherwise have run whatever `c` is
/// bound to — clearing the activity log — while Ctrl+D and Ctrl+R would have
/// deleted and reloaded a row, and Ctrl+Enter would have replaced the pane's
/// occupant, which is a removal.
fn key_message(state: &App, key: KeyEvent) -> Option<Message> {
    // The way out of raw mode, from any focus: the notes editor holds `q` but
    // nothing holds this.
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Some(Message::Quit);
    }
    // Every binding below is for an unmodified key, so a chord this
    // application does not bind is answered here rather than falling through
    // to the bare key's. Above the match and not on the character arms,
    // because `Enter`, `Up`/`Down`, `Tab`, `Esc` and `Backspace` arrive with
    // modifiers too: Ctrl+Enter would open the pane — replacing an occupant,
    // which is a removal — and Alt+q would quit.
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return None;
    }
    match key.code {
        // Guarded like every other printable key below, so `q` stays typable
        // in the notes editor. Esc closes the pane, which is how you leave it.
        KeyCode::Char('q') if !editing_notes(state) => Some(Message::Quit),
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
        KeyCode::Char(' ') if state.focus == Focus::Tasks => {
            Some(state.selected.map_or(Message::NoTaskSelected, |id| {
                Message::Task(id, TaskMessage::Toggle)
            }))
        }
        KeyCode::Char('n') if state.focus == Focus::Tasks => {
            Some(Message::AddTask("New task".to_owned()))
        }
        KeyCode::Char('d') if state.focus == Focus::Tasks => Some(
            state
                .selected
                .map_or(Message::NoTaskSelected, Message::DeleteTask),
        ),
        KeyCode::Char('r') if state.focus == Focus::Tasks => Some(
            state
                .selected
                .map_or(Message::NoTaskSelected, Message::ReloadTask),
        ),
        KeyCode::Enter if state.focus == Focus::Tasks => Some(
            state
                .selected
                .map_or(Message::NoTaskSelected, Message::OpenDetails),
        ),
        KeyCode::Char('c') if state.focus == Focus::Activity => {
            Some(Message::Activity(ActivityMessage::Clear))
        }
        KeyCode::Enter if editing_notes(state) => Some(Message::SaveNotes),
        KeyCode::Esc if state.focus == Focus::Details => Some(Message::CloseDetails),
        KeyCode::Backspace if editing_notes(state) => {
            Some(Message::Details(DetailsMessage::Backspace))
        }
        KeyCode::Char(character) if editing_notes(state) => {
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
    let program = dashboard(activity.clone()).into_program(init, view);

    let mut terminal = ratatui::init();
    let result = ProgramRuntime::new(program, setup).run(&mut terminal).await;
    ratatui::restore();

    // What the run had to say and could not show. A terminal error quits, so
    // the pane holding this line is never drawn again; printing it here is
    // after the restore rather than into a screen that is about to go.
    for line in activity.entries() {
        if let Some(reason) = line.strip_prefix(TERMINAL_ERROR) {
            eprintln!("Terminal error: {reason}");
        }
    }

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

    use crossterm::event::{KeyEvent, KeyEventKind, KeyModifiers};
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

    /// The lines a teardown hook wrote, which is what most of these rows are
    /// about. The log also carries the entries a reduce writes — `added:`,
    /// `deleted:` — which `dashboard.rs` writes too.
    fn teardowns(activity: &ActivityLog) -> Vec<String> {
        activity
            .entries()
            .into_iter()
            .filter(|line| {
                line.starts_with("stopped watching: ") || line.starts_with("closed details: ")
            })
            .collect()
    }

    /// What an unmodified key press decodes to.
    fn bare(state: &App, code: KeyCode) -> Option<Message> {
        key_message(state, KeyEvent::new(code, KeyModifiers::empty()))
    }

    /// The message the terminal subscription produces for a key press.
    fn key_press(code: KeyCode) -> Message {
        key_of(code, KeyEventKind::Press)
    }

    /// The same for the other two kinds a terminal can report.
    fn key_of(code: KeyCode, kind: KeyEventKind) -> Message {
        Message::Terminal(Event::Key(KeyEvent::new_with_kind(
            code,
            KeyModifiers::empty(),
            kind,
        )))
    }

    fn recorded(activity: &ActivityLog, line: &str) -> bool {
        activity.entries().iter().any(|entry| entry == line)
    }

    /// Adds one task and drives it to the keyed sync its row runs, returning
    /// the identities that row's boundary produced.
    fn add_task_and_sync<P: Program, B: Backend>(
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
        // `AddTask` allocates the key, so the two rows below are `TaskId(1)`
        // and `TaskId(2)` in script order and the later messages can name them.
        let mut driver = scripted(
            &activity,
            vec![
                Message::AddTask("alpha".to_owned()),
                Message::AddTask("beta".to_owned()),
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

        let (alpha_timer, alpha_sync) = add_task_and_sync(&mut driver, &script);
        let (beta_timer, beta_sync) = add_task_and_sync(&mut driver, &script);

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
                Message::AddTask("alpha".to_owned()),
                Message::AddTask("beta".to_owned()),
                Message::OpenDetails(TaskId(1)),
                // Presenting over an occupied slot: a replacement, which is a
                // removal of the occupant.
                Message::OpenDetails(TaskId(2)),
                // An occupied slot dismissed.
                Message::CloseDetails,
                // Inserting over an occupied key: a replacement, one collection
                // over. The slot is empty by now, so this pass tears exactly
                // one instance down and the order below stays the removal
                // order rather than a race between two finalizers.
                Message::ReloadTask(TaskId(2)),
                // A row leaving the keyed collection.
                Message::DeleteTask(TaskId(1)),
            ],
        );

        let boot = driver.boot();
        assert!(boot.terminated.is_none(), "bootstrap did not terminate");
        let script = anonymous(&boot, "scripted input run");

        // Both rows added and set up, so both have a hook registered under
        // their own key.
        add_task_and_sync(&mut driver, &script);
        add_task_and_sync(&mut driver, &script);
        assert!(
            teardowns(&activity).is_empty(),
            "an insert into an absent key removes no instance, so nothing is torn down"
        );

        // The slot's first occupant, set up the same way.
        let report = deliver(&mut driver, &script);
        let open = anonymous(&report, "occupant setup message");
        deliver(&mut driver, &open);

        // Replacing the occupant removes it.
        let report = deliver(&mut driver, &script);
        driver.settle(TURNS, || recorded(&activity, "closed details: #1 alpha"));
        let open = anonymous(&report, "occupant setup message");
        deliver(&mut driver, &open);

        // Dismissing the slot removes its occupant.
        deliver(&mut driver, &script);
        driver.settle(TURNS, || recorded(&activity, "closed details: #2 beta"));

        // Replacing a row removes it, and the successor starts fresh under the
        // same key.
        let report = deliver(&mut driver, &script);
        driver.settle(TURNS, || recorded(&activity, "stopped watching: #2 beta"));
        let watch = anonymous(&report, "row setup message");
        deliver(&mut driver, &watch);

        // Removing a row removes it, and leaves the successor row alone.
        deliver(&mut driver, &script);
        driver.settle(TURNS, || recorded(&activity, "stopped watching: #1 alpha"));

        // Without the script's own `added:` lines, which are setup rather than
        // subject. They are also the two the log would shed first — it sits
        // exactly on `ACTIVITY_LIMIT` — so dropping them here keeps this
        // comparison about the teardowns rather than about the buffer's size.
        let written: Vec<String> = activity
            .entries()
            .into_iter()
            .filter(|line| !line.starts_with("added: "))
            .collect();
        assert_eq!(
            written,
            vec![
                "closed details: #1 alpha".to_owned(),
                "closed details: #2 beta".to_owned(),
                "reloaded: #2 beta".to_owned(),
                "stopped watching: #2 beta".to_owned(),
                // The root's own line, written by the reduce that removed the
                // row, before the teardown it originated ran.
                "deleted: #1 alpha".to_owned(),
                "stopped watching: #1 alpha".to_owned(),
            ],
            "one teardown per removal, in removal order, and none for the successor row that is \
             still present"
        );
    }

    /// A child's setup message is not guaranteed to arrive once, and a second
    /// one must not arm a second hook.
    ///
    /// Two `Enter` presses decided against the same state both ask to open the
    /// pane. The first occupant is replaced before it ever handles its `Open`,
    /// so it registered nothing and its removal reports nothing; both `Open`
    /// messages then reach the survivor. Every registration a scope holds is
    /// fired by the teardown that selects it, so a child that armed on each
    /// would put two lines in the log for one removal.
    #[test]
    fn a_repeated_setup_message_arms_one_hook() {
        let activity = ActivityLog::default();
        let mut driver = scripted(
            &activity,
            vec![
                Message::AddTask("alpha".to_owned()),
                Message::OpenDetails(TaskId(1)),
                Message::OpenDetails(TaskId(1)),
                Message::CloseDetails,
            ],
        );

        let boot = driver.boot();
        assert!(boot.terminated.is_none(), "bootstrap did not terminate");
        let script = anonymous(&boot, "scripted input run");
        add_task_and_sync(&mut driver, &script);

        // Both opens land before either `Open` is delivered, which is what a
        // second key press ahead of the first message looks like.
        let first = deliver(&mut driver, &script);
        let second = deliver(&mut driver, &script);
        assert!(
            teardowns(&activity).is_empty(),
            "the replaced occupant never handled its `Open`, so it had registered nothing"
        );

        let first = anonymous(&first, "occupant setup message");
        let second = anonymous(&second, "occupant setup message");
        deliver(&mut driver, &first);
        deliver(&mut driver, &second);

        deliver(&mut driver, &script);
        driver.settle(TURNS, || recorded(&activity, "closed details: #1 alpha"));

        assert_eq!(
            teardowns(&activity),
            vec!["closed details: #1 alpha".to_owned()],
            "one removal, one line: the second `Open` armed nothing"
        );
    }

    /// Replacing a row closes a pane opened on the instance that left.
    ///
    /// The pane holds the replaced instance's title and notes, so leaving it
    /// open would let a later `SaveNotes` write them back over the reload.
    ///
    /// Both teardowns are originated by the same reduce, so this asserts which
    /// hooks fired and not the order they finished in.
    #[test]
    fn replacing_a_row_closes_a_pane_opened_on_it() {
        let activity = ActivityLog::default();
        let mut driver = scripted(
            &activity,
            vec![
                Message::AddTask("alpha".to_owned()),
                Message::OpenDetails(TaskId(1)),
                Message::ReloadTask(TaskId(1)),
            ],
        );

        let boot = driver.boot();
        assert!(boot.terminated.is_none(), "bootstrap did not terminate");
        let script = anonymous(&boot, "scripted input run");
        add_task_and_sync(&mut driver, &script);

        let report = deliver(&mut driver, &script);
        let open = anonymous(&report, "occupant setup message");
        deliver(&mut driver, &open);

        deliver(&mut driver, &script);
        driver.settle(TURNS, || {
            recorded(&activity, "stopped watching: #1 alpha")
                && recorded(&activity, "closed details: #1 alpha")
        });

        assert_eq!(
            teardowns(&activity).len(),
            2,
            "the row and the pane opened on it, and nothing else"
        );
    }

    /// `q` quits unless the notes editor is actually there to receive it.
    ///
    /// The pane is a `Slot` and the focus cycle passes through `Details`
    /// either way, so a guard written against the focus alone would swallow
    /// `q` into a boundary with no occupant — leaving a focus the header still
    /// advertises `q: quit` from, where it does nothing.
    #[test]
    fn q_is_typed_into_the_notes_editor_only_while_one_is_open() {
        let (mut state, _bootstrap) = init(Setup {
            activity: ActivityLog::default(),
            input: Vec::new(),
            keyboard: false,
        });

        state.focus = Focus::Details;
        assert!(
            matches!(bare(&state, KeyCode::Char('q')), Some(Message::Quit)),
            "the focus is on an empty pane, so there is no editor to type into"
        );

        state.details.present(DetailsState::new(
            TaskId(1),
            "alpha".to_owned(),
            String::new(),
        ));
        assert!(
            matches!(
                bare(&state, KeyCode::Char('q')),
                Some(Message::Details(DetailsMessage::Input('q')))
            ),
            "with an occupant the key is text"
        );

        state.focus = Focus::Tasks;
        assert!(
            matches!(bare(&state, KeyCode::Char('q')), Some(Message::Quit)),
            "and an open pane the focus is not on does not hold the key"
        );
    }

    /// Esc leaves a `Details` focus whether or not a pane is behind it, and
    /// says which of the two it did.
    ///
    /// It is guarded on the focus and not on `editing_notes`, because a focus
    /// with an empty slot behind it still needs a way out. Dismissing an empty
    /// slot removes no instance, so reporting a close there would describe a
    /// teardown that never happened, in the file whose subject is which
    /// removals produce which teardowns.
    #[test]
    fn escaping_an_empty_pane_leaves_it_without_claiming_a_close() {
        let (mut state, _bootstrap) = init(Setup {
            activity: ActivityLog::default(),
            input: Vec::new(),
            keyboard: false,
        });

        state.focus = Focus::Details;
        let _nothing_to_close = Root.reduce(&mut state, Message::CloseDetails);
        assert_eq!(
            state.focus,
            Focus::Tasks,
            "the focus is restored either way, which is what makes Esc the way out"
        );
        assert_eq!(
            state.status, "Left the details pane",
            "there was no occupant, so nothing was closed"
        );

        state.details.present(DetailsState::new(
            TaskId(1),
            "alpha".to_owned(),
            String::new(),
        ));
        state.focus = Focus::Details;
        let _closed = Root.reduce(&mut state, Message::CloseDetails);
        assert!(
            !state.details.is_present(),
            "an occupant is dismissed, which is the removal the boundary tears down"
        );
        assert_eq!(
            state.status, "Closed the details pane",
            "and only then does the status say so"
        );
    }

    /// The selection behaves as `dashboard.rs`'s does: it stops at the ends,
    /// and a delete leaves it on the row that moved up into the position.
    ///
    /// Neither is composition's doing, which is the point — the pair is meant
    /// to differ in structure, so a selection that wrapped or jumped to the
    /// first row would read as something a boundary did.
    #[test]
    fn the_selection_clamps_and_survives_a_delete_in_place() {
        let (mut state, _bootstrap) = init(Setup {
            activity: ActivityLog::default(),
            input: Vec::new(),
            keyboard: false,
        });
        for _ in 0..3 {
            let _watch = Root.reduce(&mut state, Message::AddTask("task".to_owned()));
        }
        assert_eq!(
            state.selected,
            Some(TaskId(3)),
            "adding selects the row it added"
        );

        let _at_the_end = Root.reduce(&mut state, Message::SelectNext);
        assert_eq!(
            state.selected,
            Some(TaskId(3)),
            "the last row is an end, not a wrap"
        );
        for _ in 0..3 {
            let _upwards = Root.reduce(&mut state, Message::SelectPrev);
        }
        assert_eq!(
            state.selected,
            Some(TaskId(1)),
            "and so is the first, after one step past it"
        );

        state.selected = Some(TaskId(2));
        let _middle = Root.reduce(&mut state, Message::DeleteTask(TaskId(2)));
        assert_eq!(
            state.selected,
            Some(TaskId(3)),
            "the row that moved up into the deleted one's position"
        );

        state.selected = Some(TaskId(3));
        let _last = Root.reduce(&mut state, Message::DeleteTask(TaskId(3)));
        assert_eq!(
            state.selected,
            Some(TaskId(1)),
            "and the new last row when the deleted one was last"
        );

        let _emptied = Root.reduce(&mut state, Message::DeleteTask(TaskId(1)));
        let _nowhere_to_go = Root.reduce(&mut state, Message::SelectNext);
        assert_eq!(
            state.status, "Selected another task",
            "an empty list has nowhere to move to, and still reports rather \
             than leaving the last operation's line standing"
        );
    }

    /// A key press that a boundary will claim still moves the status line.
    ///
    /// The root's arms for those messages are unreachable — a boundary takes
    /// them first — so a status set when they land would never be set at all.
    /// It is set at the dispatch instead, which is also why it names the
    /// request: the outcome belongs to a child the root has not run yet.
    #[test]
    fn a_child_addressed_key_still_moves_the_status_line() {
        let (mut state, _bootstrap) = init(Setup {
            activity: ActivityLog::default(),
            input: Vec::new(),
            keyboard: false,
        });
        let _added = Root.reduce(&mut state, Message::AddTask("alpha".to_owned()));

        state.focus = Focus::Navigation;
        let _navigating = Root.reduce(&mut state, key_press(KeyCode::Down));
        assert_eq!(
            state.status, "Selected another section",
            "the navigation child claims the message, so the root says so here"
        );

        state.focus = Focus::Activity;
        let _clearing = Root.reduce(&mut state, key_press(KeyCode::Char('c')));
        assert_eq!(
            state.status, "Cleared the activity log",
            "and the same for the log child"
        );

        state.focus = Focus::Tasks;
        let _toggling = Root.reduce(&mut state, key_press(KeyCode::Char(' ')));
        assert_eq!(
            state.status, "Toggled the task",
            "and for a row, which is claimed by key"
        );
    }

    /// A held key is an instruction; the release after it is not.
    ///
    /// The protocol that reports releases is the one that reports repeats, so
    /// a filter written as `== Press` would drop both and a held Down would
    /// move the selection once.
    #[test]
    fn a_repeat_is_an_instruction_and_a_release_is_not() {
        let (mut state, _bootstrap) = init(Setup {
            activity: ActivityLog::default(),
            input: Vec::new(),
            keyboard: false,
        });
        for _ in 0..2 {
            let _added = Root.reduce(&mut state, Message::AddTask("task".to_owned()));
        }
        state.focus = Focus::Tasks;
        state.selected = Some(TaskId(1));

        assert!(
            !Root
                .reduce(&mut state, key_of(KeyCode::Down, KeyEventKind::Repeat))
                .is_none(),
            "a repeat asks for the move a press would have asked for"
        );
        assert!(
            Root.reduce(&mut state, key_of(KeyCode::Down, KeyEventKind::Release))
                .is_none(),
            "and the release after it asks for nothing"
        );
    }

    /// A key that needs a selected row says so when there is none.
    ///
    /// Silence would leave the footer describing the operation before it — the
    /// reason `select` sets its status ahead of its own empty-collection
    /// guard.
    #[test]
    fn a_key_needing_a_selection_reports_when_there_is_none() {
        let (mut state, _bootstrap) = init(Setup {
            activity: ActivityLog::default(),
            input: Vec::new(),
            keyboard: false,
        });
        state.focus = Focus::Tasks;
        assert_eq!(state.selected, None, "nothing has been added");

        for code in [
            KeyCode::Char(' '),
            KeyCode::Char('d'),
            KeyCode::Char('r'),
            KeyCode::Enter,
        ] {
            assert!(
                matches!(bare(&state, code), Some(Message::NoTaskSelected)),
                "{code:?} needs a row and there is none, so it reports rather than \
                 leaving the last operation's line standing"
            );
        }

        let _reported = Root.reduce(&mut state, Message::NoTaskSelected);
        assert_eq!(state.status, "No task selected");
    }

    /// Reloading a row is a replacement, so the successor starts fresh and
    /// everything local to the instance that left goes with it.
    ///
    /// A cleared checkbox is otherwise the only sign, so the status says it.
    #[test]
    fn reloading_a_row_discards_what_was_local_to_it() {
        let (mut state, _bootstrap) = init(Setup {
            activity: ActivityLog::default(),
            input: Vec::new(),
            keyboard: false,
        });
        let _added = Root.reduce(&mut state, Message::AddTask("alpha".to_owned()));
        let row = state
            .tasks
            .get_mut(&TaskId(1))
            .expect("the row that was just added");
        row.done = true;
        row.notes = "edited and saved".to_owned();

        let _reloaded = Root.reduce(&mut state, Message::ReloadTask(TaskId(1)));

        let row = state.tasks.get(&TaskId(1)).expect("the successor");
        assert!(!row.done, "the done flag was the old instance's");
        assert_ne!(
            row.notes, "edited and saved",
            "and so were the notes saved into it"
        );
        assert_eq!(
            state.status, "Reloaded the task, discarding its local state",
            "which the reader is told, since the cleared checkbox is the only other sign"
        );
    }

    /// A `CONTROL` chord runs no bare-key binding, and Ctrl+C leaves.
    ///
    /// Raw mode delivers no SIGINT, so Ctrl+C arrives as an ordinary key
    /// event. Without the guard it would have found whatever `c` is bound to —
    /// and Ctrl+D and Ctrl+R would have found the destructive two.
    #[test]
    fn a_control_chord_runs_no_bare_binding_and_ctrl_c_leaves() {
        let (mut state, _bootstrap) = init(Setup {
            activity: ActivityLog::default(),
            input: Vec::new(),
            keyboard: false,
        });
        let _added = Root.reduce(&mut state, Message::AddTask("alpha".to_owned()));

        let chord =
            |state: &App, code, modifiers| key_message(state, KeyEvent::new(code, modifiers));

        // Not only the characters: `Enter`, the arrows, `Tab`, `Esc` and
        // `Backspace` arrive with modifiers too, and Ctrl+Enter would open the
        // pane — replacing an occupant, which is a removal.
        state.focus = Focus::Tasks;
        for code in [
            KeyCode::Char('d'),
            KeyCode::Char('r'),
            KeyCode::Char(' '),
            KeyCode::Char('n'),
            KeyCode::Enter,
            KeyCode::Tab,
            KeyCode::Up,
            KeyCode::Down,
        ] {
            for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
                assert!(
                    chord(&state, code, modifiers).is_none(),
                    "{code:?} with {modifiers:?} is a chord this application does not bind"
                );
            }
        }

        state.focus = Focus::Activity;
        assert!(
            matches!(
                chord(&state, KeyCode::Char('c'), KeyModifiers::CONTROL),
                Some(Message::Quit)
            ),
            "Ctrl+C leaves rather than clearing the log the bare key clears"
        );
        assert!(
            chord(&state, KeyCode::Char('c'), KeyModifiers::ALT).is_none(),
            "while Alt+c is bound to nothing and clears nothing"
        );

        state.details.present(DetailsState::new(
            TaskId(1),
            "alpha".to_owned(),
            String::new(),
        ));
        state.focus = Focus::Details;
        assert!(
            matches!(
                chord(&state, KeyCode::Char('c'), KeyModifiers::CONTROL),
                Some(Message::Quit)
            ),
            "and from the notes editor too, which holds the bare `q` but not this"
        );
        for (code, modifiers) in [
            (KeyCode::Char('x'), KeyModifiers::CONTROL),
            (KeyCode::Char('q'), KeyModifiers::ALT),
            (KeyCode::Backspace, KeyModifiers::CONTROL),
            (KeyCode::Esc, KeyModifiers::ALT),
        ] {
            assert!(
                chord(&state, code, modifiers).is_none(),
                "{code:?} with {modifiers:?} neither edits nor leaves"
            );
        }
    }

    /// A terminal error is recorded, because the quit that follows it leaves
    /// nowhere to draw the report.
    ///
    /// `main` prints the recorded line after restoring the terminal. What this
    /// row holds is the half the program owns: that the reason is written down
    /// at all, rather than lost with the screen.
    #[test]
    fn a_terminal_error_leaves_a_reason_behind() {
        let activity = ActivityLog::default();
        let (mut state, _bootstrap) = init(Setup {
            activity: activity.clone(),
            input: Vec::new(),
            keyboard: false,
        });

        let quit = Root.reduce(&mut state, Message::TerminalError("broken pipe".to_owned()));

        assert!(
            recorded(&activity, "terminal error: broken pipe"),
            "the reason is written down where `main` can still read it"
        );
        assert!(!quit.is_none(), "and the run ends");
    }
}
