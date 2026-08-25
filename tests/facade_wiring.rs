//! The production facade, wired end to end — and the pair that shows it adds
//! nothing.
//!
//! Two claims, and they need different instruments.
//!
//! The first is that `Runtime::<App>::new(flags).run(terminal)` actually
//! drives the kernel: that the init command is dispatched, the declared
//! subscription is admitted and its output delivered, `update` and `view` run
//! at the right points, and a returned quit terminates the loop with the
//! classification RFC 0011 INV-LC5 states. Every layer below the facade has
//! its own tests; none of them observes the facade's own construction path,
//! which is the seam a switch can get wrong while every unit below stays
//! green.
//!
//! The second is RFC 0014 INV-RC1: that an `Application` and a hand-written
//! `Program` are the same execution. The structural half of that invariant is
//! a review — there is no `Application`-typed branch below the adapter — and
//! this is its behavioural half. The two run the same script through the two
//! entry points and their journals must be equal, entry for entry. A fast
//! path for the facade, a phase the adapter skipped, an ordering that
//! differed by entry point: each would separate the two sequences.
//!
//! The journals record transitions, not wall-clock or task identity, so what
//! is compared is what the kernel is contracted to do rather than how a
//! particular run got scheduled.

mod common;

use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use color_eyre::eyre::Result;
use futures::stream;
use ratatui::Frame;
use ratatui::widgets::Paragraph;
use tears::prelude::*;
use tears::reducer::{Program, Reducer, ScopeValue};
use tears::{BoxStream, EffectCommand, Exit, ProgramRuntime, RuntimeConfig, SubscriptionSource};
use tokio::time::{Duration, timeout};

/// What both entry points write, in the order the kernel drives them.
#[derive(Clone, Default)]
struct Journal(Arc<Mutex<Vec<String>>>);

impl Journal {
    fn record(&self, entry: impl Into<String>) {
        self.0
            .lock()
            .expect("journal mutex should not be poisoned")
            .push(entry.into());
    }

    fn entries(&self) -> Vec<String> {
        self.0
            .lock()
            .expect("journal mutex should not be poisoned")
            .clone()
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum Message {
    /// Delivered by the init command, through the data lane.
    Started,
    /// Delivered by the subscription, through the data lane.
    Ticked(u8),
    /// Returned by `update`, applied synchronously at its dispatch.
    Stop,
}

/// A subscription that yields one value and then ends, so the run reaches a
/// natural finish rather than being torn down at termination.
#[derive(Clone)]
struct OneTick(&'static str);

impl SubscriptionSource for OneTick {
    type Output = Message;
    type Key = &'static str;

    fn key(&self) -> Self::Key {
        self.0
    }

    fn stream(&self) -> BoxStream<'static, Self::Output> {
        Box::pin(stream::once(async { Message::Ticked(7) }))
    }
}

/// The state both entry points reduce. Identical either way: the facade's
/// state *is* the application value, so this is the application.
struct State {
    journal: Journal,
    ticks: u8,
}

impl State {
    fn init(journal: &Journal) -> (Self, Command<Message>) {
        journal.record("init");
        (
            Self {
                journal: journal.clone(),
                ticks: 0,
            },
            Command::message(Message::Started).into(),
        )
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "mirrors `Reducer::reduce`, which takes the message by value; \
                  taking it by reference here would stop the two entry points \
                  from sharing one body, which is the whole point of the pair"
    )]
    fn reduce(&mut self, message: Message) -> Command<Message> {
        match message {
            Message::Started => {
                self.journal.record("update:started");
                Command::none()
            }
            Message::Ticked(value) => {
                self.ticks += value;
                self.journal.record(format!("update:ticked:{value}"));
                // Deliver, then quit — the order §3.3 pins for the
                // synchronous route.
                Command::message(Message::Stop).into()
            }
            Message::Stop => {
                self.journal.record("update:stop");
                Command::quit()
            }
        }
    }

    fn render(&self, frame: &mut Frame<'_>) {
        self.journal.record(format!("view:{}", self.ticks));
        frame.render_widget(Paragraph::new(format!("{}", self.ticks)), frame.area());
    }

    fn declared(&self) -> Vec<Subscription<Message>> {
        self.journal.record("subscriptions");
        vec![Subscription::from(OneTick("tick"))]
    }
}

// --- the facade's side ----------------------------------------------------

struct FacadeApp(State);

impl Application for FacadeApp {
    type Message = Message;
    type Flags = Journal;

    fn new(journal: Journal) -> (Self, Command<Message>) {
        let (state, command) = State::init(&journal);
        (Self(state), command)
    }

    fn update(&mut self, message: Message) -> Command<Message> {
        self.0.reduce(message)
    }

    fn view(&self, frame: &mut Frame<'_>) {
        self.0.render(frame);
    }

    fn subscriptions(&self) -> Vec<Subscription<Message>> {
        self.0.declared()
    }
}

// --- the hand-written program's side --------------------------------------

struct HandWritten;

impl Reducer for HandWritten {
    type State = State;
    type Message = Message;

    fn reduce(&self, state: &mut State, message: Message) -> Command<Message> {
        state.reduce(message)
    }

    fn subscriptions(&self, state: &State) -> Vec<Subscription<Message>> {
        state.declared()
    }
}

impl Program for HandWritten {
    type Flags = Journal;

    fn init(&self, journal: Journal) -> (State, Command<Message>) {
        State::init(&journal)
    }

    fn view(&self, state: &State, frame: &mut Frame<'_>) {
        state.render(frame);
    }
}

// --- the rows -------------------------------------------------------------

/// The facade reaches every stage: init command, subscription admission and
/// delivery, update, view, and a quit that terminates with `Ok(())`.
///
/// The journal is what makes this more than "it did not hang": a wiring that
/// dropped the init command, never re-evaluated subscriptions, or rendered
/// once and stopped would still return `Ok(())` here, and would not produce
/// this sequence.
#[tokio::test]
async fn the_facade_drives_every_stage_and_classifies_the_quit() -> Result<()> {
    let journal = Journal::default();
    let mut terminal = common::test_terminal()?;
    let runtime = Runtime::<FacadeApp>::new(journal.clone());

    let outcome = timeout(Duration::from_secs(5), runtime.run(&mut terminal))
        .await
        .expect("the quit should end the run well before the timeout");

    assert!(outcome.is_ok(), "an update-returned quit classifies as Ok");

    let entries = journal.entries();
    assert_eq!(entries.first().map(String::as_str), Some("init"));
    assert!(
        entries.iter().any(|entry| entry == "update:started"),
        "the init command's message was delivered: {entries:?}"
    );
    assert!(
        entries.iter().any(|entry| entry == "update:ticked:7"),
        "the declared subscription was admitted and its output delivered: {entries:?}"
    );
    assert_eq!(
        entries.last().map(String::as_str),
        Some("update:stop"),
        "the quit applied at its own dispatch, with nothing after it: {entries:?}"
    );
    assert!(
        entries.iter().any(|entry| entry.starts_with("view:")),
        "the view ran: {entries:?}"
    );
    Ok(())
}

/// The configured entry point runs the same script to the same end.
///
/// `with_config` had no test at all: every row above builds through
/// `Runtime::new`, so the bounded construction path — the one that reaches
/// the lane capacity control — was public API nothing exercised. A bounded
/// lane changes when a producer waits, not what the application observes, so
/// the assertion is that the journal is the one `new` produces.
#[tokio::test]
async fn the_configured_entry_point_runs_the_same_script() -> Result<()> {
    let default_journal = Journal::default();
    let mut terminal = common::test_terminal()?;
    timeout(
        Duration::from_secs(5),
        Runtime::<FacadeApp>::new(default_journal.clone()).run(&mut terminal),
    )
    .await
    .expect("the default run should end before the timeout")?;

    let bounded_journal = Journal::default();
    let config = RuntimeConfig::new().data_lane_capacity(NonZeroUsize::new(8).expect("non-zero"));
    let mut terminal = common::test_terminal()?;
    timeout(
        Duration::from_secs(5),
        Runtime::<FacadeApp>::with_config(bounded_journal.clone(), config).run(&mut terminal),
    )
    .await
    .expect("the bounded run should end before the timeout")?;

    assert_eq!(
        default_journal.entries(),
        bounded_journal.entries(),
        "a bounded lane changes when a producer waits, not what the application sees"
    );
    Ok(())
}

/// INV-RC1, behavioural half: the same script through both entry points
/// yields the same sequence of transitions.
#[tokio::test]
async fn the_facade_and_a_hand_written_program_trace_identically() -> Result<()> {
    let facade_journal = Journal::default();
    let mut terminal = common::test_terminal()?;
    timeout(
        Duration::from_secs(5),
        Runtime::<FacadeApp>::new(facade_journal.clone()).run(&mut terminal),
    )
    .await
    .expect("the facade run should end before the timeout")?;

    let program_journal = Journal::default();
    let mut terminal = common::test_terminal()?;
    let exit = timeout(
        Duration::from_secs(5),
        ProgramRuntime::new(HandWritten, program_journal.clone()).run(&mut terminal),
    )
    .await
    .expect("the program run should end before the timeout")?;

    assert_eq!(exit, Exit::Quit, "both routes reach the same exit");
    assert_eq!(
        facade_journal.entries(),
        program_journal.entries(),
        "the adapter contributes mapping calls only (RFC 0014 INV-RC1)"
    );
    Ok(())
}

/// The adapter is not a `Reducer` the user has to name: `ScopeValue`'s bound
/// is what a combinator segment must satisfy, and this pins that the public
/// bound is reachable without naming any private item — the compile is the
/// assertion.
#[test]
fn a_scope_segment_type_satisfies_the_public_bound() {
    fn assert_scope_value<S: ScopeValue>() {}
    assert_scope_value::<&'static str>();
    assert_scope_value::<u8>();
}

/// `Command::teardown` and `Command::on_teardown` are public, and both build
/// a command that spawns nothing — the carrier-free shape the cancel and
/// spawn phases consume.
#[test]
fn the_teardown_carriers_are_public_and_spawn_nothing() {
    let teardown: Command<Message> = Command::teardown("pane-1");
    let cleanup: Command<Message> = Command::on_teardown(async {});
    let effect: EffectCommand<Message> = Command::message(Message::Started);

    assert!(teardown.is_none(), "a teardown carries no stream to spawn");
    assert!(cleanup.is_none(), "nor does a cleanup registration");
    assert!(Command::from(effect).is_some(), "an effect carrier does");
}
