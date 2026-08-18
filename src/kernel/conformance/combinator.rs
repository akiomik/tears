//! The composition combinators, driven through the kernel.
//!
//! The unit rows beside the combinators themselves
//! ([`crate::reducer::combinator`]) read a boundary's *command*: which
//! carriers it qualified, which teardowns it merged. These rows read what
//! the kernel then **does** with them — runs reclaimed, identities kept
//! apart, successors started fresh — through the production dispatch path,
//! pass-unit driven.
//!
//! The program under test is a closed combinator stack
//! ([`ReducerExt::into_program`]), so every row here is also the evidence
//! that a stack closed this way is a [`Program`] the kernel and the driver
//! drive like any other — with no `Application` anywhere in the topology
//! (INV-RC1's composed half).
//!
//! # The rows and what they carry
//!
//! | row | invariant |
//! | --- | --- |
//! | [`sibling_boundaries_keep_equal_local_ids_apart`] | INV-RC2 at the lowering seam, both halves |
//! | [`a_row_s_subscriptions_are_qualified_and_retracted_with_it`] | INV-RC2's declaration half, INV-RC6 through a boundary |
//! | [`an_anonymous_child_effect_is_reached_by_its_boundary_s_teardown`] | INV-RC7 through a combinator |
//! | [`closing_a_row_tears_down_the_runs_under_it`] | INV-RC3's drain, as the kernel applies it |
//! | [`dismissing_the_slot_tears_down_its_occupant_s_runs`] | INV-RC3's dismissal shape, likewise |
//! | [`a_same_update_recreate_tears_the_old_instance_down_and_starts_the_successor_fresh`] | INV-RC3's no-diff adversary and INV-RC4's batch remove-and-reinsert |
//! | [`a_message_for_a_closed_row_starts_nothing`] | §2.5's routing boundary, INV-ST8 |
//!
//! [`ReducerExt::into_program`]: crate::reducer::combinator::ReducerExt::into_program

use std::collections::{HashMap, VecDeque};

use futures::StreamExt;
use futures::stream;
use ratatui::Frame;
use ratatui::backend::TestBackend;

use crate::command::{Command, CommandId};
use crate::kernel::arbiter::WakeSource;
use crate::reducer::Reducer;
use crate::reducer::collection::{Keyed, Slot};
use crate::reducer::combinator::{ForEach, IntoProgram, Presented, ReducerExt};
use crate::subscription::Subscription;
use crate::testing::driver::{RunKind, RunName, TestDriver};

use super::support::{
    Beacon, ProbeSource, TEST_TURNS, accept, cap, config, holding_effect, terminal,
};

/// What a pane is asked to do.
#[derive(Clone, Copy, Debug)]
enum PaneMsg {
    /// Start a run under the pane's own local id — the same id in every
    /// pane, so only the boundary keeps two of them apart.
    Work,
    /// Start an unkeyed run: an anonymous effect spawned through a
    /// composition boundary, which nothing but its scope can address
    /// (INV-RC7).
    Anon,
}

/// One pane instance: the runs it starts hold `reclaimed`, and it declares
/// `declares` when it has one.
struct PaneState {
    reclaimed: Beacon,
    declares: Option<ProbeSource>,
}

impl PaneState {
    const fn new(reclaimed: Beacon) -> Self {
        Self {
            reclaimed,
            declares: None,
        }
    }

    fn declaring(reclaimed: Beacon, source: ProbeSource) -> Self {
        Self {
            reclaimed,
            declares: Some(source),
        }
    }
}

/// The child reducer every boundary in the stack composes.
struct Pane;

impl Reducer for Pane {
    type State = PaneState;
    type Message = PaneMsg;

    fn reduce(&self, state: &mut PaneState, message: PaneMsg) -> Command<PaneMsg> {
        // Parks forever holding a drop-marking guard, so the run's
        // reclamation is what the row reads.
        let effect = holding_effect(state.reclaimed.clone()).map(|_| PaneMsg::Work);
        match message {
            PaneMsg::Work => effect.cancellable(CommandId::new("work")),
            PaneMsg::Anon => effect,
        }
    }

    fn subscriptions(&self, state: &PaneState) -> Vec<Subscription<PaneMsg>> {
        state
            .declares
            .iter()
            .map(|source| Subscription::new(source.clone()).map(|_| PaneMsg::Work))
            .collect()
    }
}

/// What one scripted step tells the root to do.
#[derive(Clone, Copy, Debug)]
enum Act {
    /// Remove the pane under `key`.
    Close(u8),
    /// Remove and re-insert `key` in one update, returning a keyed run
    /// placed under that same key — so one command carries the journal's
    /// teardown *and* the successor's spawn (RFC 0013 R4).
    Recreate(u8),
    /// Empty the slot.
    Dismiss,
}

/// The root state: the two collections the boundaries project, plus the
/// script.
struct RootState {
    panes: Keyed<u8, PaneState>,
    modal: Slot<PaneState>,
    acts: HashMap<u8, Act>,
    /// The instance each [`Act::Recreate`] installs, in order. The test
    /// builds them so it can watch each instance's runs separately.
    successors: VecDeque<PaneState>,
}

/// The root reducer: it owns the collections and nothing else.
struct Root;

impl Reducer for Root {
    type State = RootState;
    type Message = Msg;

    fn reduce(&self, state: &mut RootState, message: Msg) -> Command<Msg> {
        let Msg::Act(step) = message else {
            // Every other message is claimed by a boundary before it gets
            // here; reaching this arm would mean the routing failed.
            return Command::none();
        };
        match state.acts.get(&step).copied() {
            Some(Act::Close(key)) => {
                state.panes.remove(&key);
                Command::none()
            }
            Some(Act::Recreate(key)) => {
                let successor = state
                    .successors
                    .pop_front()
                    .expect("the script supplies one successor per recreate");
                let reclaimed = successor.reclaimed.clone();
                state.panes.remove(&key);
                state.panes.insert(key, successor);
                // Placed under the same segment the boundary uses, so the
                // journal's teardown and this spawn address one prefix.
                holding_effect(reclaimed)
                    .map(|_| Msg::Act(0))
                    .cancellable(CommandId::new("work"))
                    .scoped(key)
            }
            Some(Act::Dismiss) => {
                state.modal.dismiss();
                Command::none()
            }
            None => Command::none(),
        }
    }
}

/// The root message type.
#[derive(Clone, Debug)]
enum Msg {
    /// Root-handled: run the scripted act for this step.
    Act(u8),
    /// Routed to the pane under this key.
    Row(u8, PaneMsg),
    /// Routed to the slot's occupant.
    Modal(PaneMsg),
}

fn row_extract(message: Msg) -> Result<(u8, PaneMsg), Msg> {
    match message {
        Msg::Row(key, pane) => Ok((key, pane)),
        other => Err(other),
    }
}

fn modal_extract(message: Msg) -> Result<PaneMsg, Msg> {
    match message {
        Msg::Modal(pane) => Ok(pane),
        other => Err(other),
    }
}

/// What the program is told at `init`.
struct Setup {
    /// Panes open before any message arrives, in insertion order.
    panes: Vec<(u8, PaneState)>,
    /// The slot's initial occupant.
    modal: Option<PaneState>,
    /// The scripted acts, by step.
    acts: HashMap<u8, Act>,
    /// One instance per [`Act::Recreate`], in order.
    successors: VecDeque<PaneState>,
    /// The messages the init effect emits, one per grant.
    trigger: Vec<Msg>,
}

impl Setup {
    fn new(trigger: Vec<Msg>) -> Self {
        Self {
            panes: Vec::new(),
            modal: None,
            acts: HashMap::new(),
            successors: VecDeque::new(),
            trigger,
        }
    }

    fn opening(mut self, panes: Vec<(u8, PaneState)>) -> Self {
        self.panes = panes;
        self
    }

    fn presenting(mut self, occupant: PaneState) -> Self {
        self.modal = Some(occupant);
        self
    }

    fn acting(mut self, step: u8, act: Act) -> Self {
        self.acts.insert(step, act);
        self
    }

    fn succeeding(mut self, successor: PaneState) -> Self {
        self.successors.push_back(successor);
        self
    }
}

/// The init effect: emits each scripted trigger and then parks forever, so
/// the triggers arrive one grant at a time and no producer exit accompanies
/// them.
fn init(setup: Setup) -> (RootState, Command<Msg>) {
    let Setup {
        panes,
        modal,
        acts,
        successors,
        trigger,
    } = setup;
    let mut state = RootState {
        panes: Keyed::new(),
        modal: Slot::empty(),
        acts,
        successors,
    };
    // First insertions and a first presentation, so neither records a
    // removal: bootstrap opens instances rather than replacing any.
    for (key, pane) in panes {
        state.panes.insert(key, pane);
    }
    if let Some(occupant) = modal {
        state.modal.present(occupant);
    }
    (
        state,
        Command::stream(stream::iter(trigger).chain(stream::pending())),
    )
}

fn view(_state: &RootState, _frame: &mut Frame<'_>) {}

/// The closed stack: a `for_each` over the panes, a `presented` slot, and
/// the root `init`/`view` that close it.
type Composed = IntoProgram<Presented<ForEach<Root, Pane, u8>, Pane, &'static str>, Setup>;

fn program() -> Composed {
    Root.for_each(
        Pane,
        |state: &RootState| &state.panes,
        |state: &mut RootState| &mut state.panes,
        row_extract,
        Msg::Row,
    )
    .presented(
        Pane,
        "modal",
        |state: &RootState| &state.modal,
        |state: &mut RootState| &mut state.modal,
        modal_extract,
        Msg::Modal,
    )
    .into_program(init, view)
}

/// A driver over the closed stack, one message per pass.
fn driver(setup: Setup) -> TestDriver<Composed, TestBackend> {
    TestDriver::new(
        program(),
        setup,
        config().batch_max_messages(cap(1)),
        terminal(),
    )
}

/// Releases the next scripted trigger and runs the pass it begins.
fn deliver(driver: &mut TestDriver<Composed, TestBackend>, trigger: &RunName) -> Vec<RunKind> {
    accept(driver, trigger.clone());
    driver
        .step_pass(WakeSource::Data)
        .expect("the scripted trigger is in the lane")
        .started
        .iter()
        .map(RunName::kind)
        .collect()
}

// INV-RC2 at the lowering seam, both halves at once. Two panes reduce the
// *same* child with the *same* local id, and the boundary is the only thing
// keeping the two runs apart: were the ids to alias, the second pane's
// `CancelInFlight` spawn would replace the first pane's run and reclaim it.
// The teardown half then shows the qualification reaching the placement
// scope too — closing pane 1 selects its run and leaves pane 2's.
#[test]
fn sibling_boundaries_keep_equal_local_ids_apart() {
    let (first, second) = (Beacon::default(), Beacon::default());
    let mut driver = driver(
        Setup::new(vec![
            Msg::Row(1, PaneMsg::Work),
            Msg::Row(2, PaneMsg::Work),
            Msg::Act(1),
        ])
        .opening(vec![
            (1, PaneState::new(first.clone())),
            (2, PaneState::new(second.clone())),
        ])
        .acting(1, Act::Close(1)),
    );
    let trigger = driver.boot().started[0].clone();

    let started = deliver(&mut driver, &trigger);
    assert!(matches!(started.as_slice(), [RunKind::Keyed(_)]));
    let started = deliver(&mut driver, &trigger);
    assert!(matches!(started.as_slice(), [RunKind::Keyed(_)]));

    driver.settle(TEST_TURNS, || true);
    assert!(
        !first.marked(),
        "the second pane's same-local-id spawn did not replace the first pane's run"
    );

    deliver(&mut driver, &trigger);
    driver.settle(TEST_TURNS, || first.marked());

    assert!(
        !second.marked(),
        "closing one pane selected only the runs placed under its own key"
    );
}

// INV-RC2's declaration half through the kernel, with INV-RC6 as its second
// clause. Two panes declare the same source under the same local key, and
// both are admitted — which they could not be if the boundary had not
// qualified the declared identities, since a live run for an id suppresses
// a second admission of it. Closing one pane then stops that pane's
// subscription run and leaves the other's.
#[test]
fn a_row_s_subscriptions_are_qualified_and_retracted_with_it() {
    let (first, second) = (ProbeSource::silent("feed"), ProbeSource::silent("feed"));
    let mut driver = driver(
        Setup::new(vec![Msg::Act(1)])
            .opening(vec![
                (1, PaneState::declaring(Beacon::default(), first.clone())),
                (2, PaneState::declaring(Beacon::default(), second.clone())),
            ])
            .acting(1, Act::Close(1)),
    );
    let trigger = driver.boot().started[0].clone();

    assert_eq!(
        (first.admissions(), second.admissions()),
        (1, 1),
        "both panes' declarations were admitted, so their identities differ"
    );

    deliver(&mut driver, &trigger);
    driver.settle(TEST_TURNS, || first.quiescences() > 0);

    assert_eq!(
        second.quiescences(),
        0,
        "the sibling pane's subscription run is untouched"
    );
}

// INV-RC7 through a composition boundary: an effect the child returned
// *without* a key has no logical identity at all, and the only thing that
// can address it is the scope its boundary placed it under. Closing the row
// reaches it.
#[test]
fn an_anonymous_child_effect_is_reached_by_its_boundary_s_teardown() {
    let reclaimed = Beacon::default();
    let mut driver = driver(
        Setup::new(vec![Msg::Row(1, PaneMsg::Anon), Msg::Act(1)])
            .opening(vec![(1, PaneState::new(reclaimed.clone()))])
            .acting(1, Act::Close(1)),
    );
    let trigger = driver.boot().started[0].clone();

    let started = deliver(&mut driver, &trigger);
    assert!(
        matches!(started.as_slice(), [RunKind::Anonymous]),
        "the child's unkeyed effect started an anonymous run: {started:?}"
    );

    deliver(&mut driver, &trigger);
    driver.settle(TEST_TURNS, || reclaimed.marked());
}

// INV-RC3's drain as the kernel applies it: the removal the parent's own
// `update` recorded becomes a teardown in that same update's command, and
// the runs under the removed row are reclaimed by it. Nothing in the
// reducer wrote `.teardown(...)` — the boundary did.
#[test]
fn closing_a_row_tears_down_the_runs_under_it() {
    let reclaimed = Beacon::default();
    let mut driver = driver(
        Setup::new(vec![Msg::Row(1, PaneMsg::Work), Msg::Act(1)])
            .opening(vec![(1, PaneState::new(reclaimed.clone()))])
            .acting(1, Act::Close(1)),
    );
    let trigger = driver.boot().started[0].clone();

    deliver(&mut driver, &trigger);
    driver.settle(TEST_TURNS, || true);
    assert!(!reclaimed.marked(), "the row's run is live");

    deliver(&mut driver, &trigger);
    driver.settle(TEST_TURNS, || reclaimed.marked());
}

// The dismissal shape of the same drain, through the slot boundary: the
// teardown the journal yields is of the boundary's own segment, and it
// reaches the occupant's runs.
#[test]
fn dismissing_the_slot_tears_down_its_occupant_s_runs() {
    let reclaimed = Beacon::default();
    let mut driver = driver(
        Setup::new(vec![Msg::Modal(PaneMsg::Work), Msg::Act(1)])
            .presenting(PaneState::new(reclaimed.clone()))
            .acting(1, Act::Dismiss),
    );
    let trigger = driver.boot().started[0].clone();

    deliver(&mut driver, &trigger);
    driver.settle(TEST_TURNS, || true);
    assert!(!reclaimed.marked());

    deliver(&mut driver, &trigger);
    driver.settle(TEST_TURNS, || reclaimed.marked());
}

// RFC 0014 §11's *diff-based removal detection* and *fold-era batch*
// adversaries in one row. The update removes key 1 and re-inserts it, so
// the collection is byte-for-byte what it was and a diff would report
// nothing; and the command it returns carries both the journal's teardown
// **and** the successor's keyed spawn, under one prefix and one identity.
// The cancel phase precedes every spawn of the same command, so the old
// instance's run is reclaimed and the successor starts fresh rather than
// being replaced or suppressed by what it succeeded.
#[test]
fn a_same_update_recreate_tears_the_old_instance_down_and_starts_the_successor_fresh() {
    let (old, new) = (Beacon::default(), Beacon::default());
    let mut driver = driver(
        Setup::new(vec![Msg::Row(1, PaneMsg::Work), Msg::Act(1)])
            .opening(vec![(1, PaneState::new(old.clone()))])
            .acting(1, Act::Recreate(1))
            .succeeding(PaneState::new(new.clone())),
    );
    let trigger = driver.boot().started[0].clone();

    deliver(&mut driver, &trigger);
    let started = deliver(&mut driver, &trigger);

    assert!(
        matches!(started.as_slice(), [RunKind::Keyed(_)]),
        "the same command's spawn phase started the successor: {started:?}"
    );
    driver.settle(TEST_TURNS, || old.marked());
    assert!(
        !new.marked(),
        "and the successor is the run that survives the application point"
    );
}

// RFC 0014 §2.5's routing boundary, which is also INV-ST8's "does not
// re-route key-addressed input": a message for a key the collection no
// longer holds reaches no reducer, so nothing is started and no successor
// inherits it.
#[test]
fn a_message_for_a_closed_row_starts_nothing() {
    let mut driver = driver(
        Setup::new(vec![Msg::Act(1), Msg::Row(1, PaneMsg::Work)])
            .opening(vec![(1, PaneState::new(Beacon::default()))])
            .acting(1, Act::Close(1)),
    );
    let trigger = driver.boot().started[0].clone();

    deliver(&mut driver, &trigger);
    let started = deliver(&mut driver, &trigger);

    assert!(
        started.is_empty(),
        "the closed row claimed nothing and started nothing: {started:?}"
    );
}
