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
//! | [`a_replacement_s_successor_declares_again_at_the_predecessor_s_exit`] | §5.1's barrier and §5.2's dirt, over a boundary's replacement |
//! | [`a_replaced_slot_occupant_s_successor_declares_again_at_the_predecessor_s_exit`] | the same, for the slot's replacing shape |
//! | [`a_replacement_that_stops_no_subscription_declares_in_the_replacing_pass`] | the same barrier's condition, from its other side |
//! | [`a_key_addressed_message_after_a_reinsert_reaches_the_new_instance`] | INV-ST8's positive half, §2.5's third routing clause |
//! | [`a_message_for_a_closed_row_starts_nothing`] | §2.5's routing boundary, INV-ST8's negative half |
//!
//! # The rows a mutation was run against
//!
//! Four rows here have a subject an unrelated mechanism could satisfy, so
//! each was checked by mutation rather than by reading. Every mutation
//! below was run; what is recorded is which rows in **this file** failed,
//! not which were expected to. Rows elsewhere may fail too — dropping the
//! barrier reaches one in [`admission`](super::admission), for instance —
//! and the column does not enumerate them.
//!
//! | mutation | rows in this file it fails |
//! | --- | --- |
//! | the drain returns only keys absent from the collection at drain time | the no-diff adversary, on its unkeyed run; the keyed replacement, where it asserts the replacing pass admits nothing |
//! | `any_stopping_sub` dropped from the reconcile barrier | both replacement rows, where they assert the replacing pass admits nothing |
//! | `Keyed::insert` records the removal without installing the value | the keyed replacement, on the source only its successor declares; the condition row, where it asserts the admission |
//! | `Slot::present` records the dismissal but keeps the occupant | the slot replacement, likewise |
//! | `is_stopping_sub` widened to any stopping run | the barrier's condition row, where it asserts the admission |
//!
//! The no-diff adversary's own comment records why its assertion is on the
//! unkeyed run and not the keyed one. The two "records the removal but does
//! not install" mutations are what the shared-plus-own source pair exists
//! for on the replacement rows: one identity declared by both instances
//! cannot tell a successor declaring from a predecessor left in place
//! re-declaring at its own exit. The condition row needs no such pair — its
//! successor is the only declarer there — which is why the same mutation
//! reaches it. What that row does need is a predecessor holding a command
//! run: holding nothing leaves the barrier nothing to read either, and the
//! row would pass without its subject being what made it.
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

/// One pane instance.
///
/// The two run beacons are separate on purpose. A keyed run can be reclaimed
/// by two different things — a prefix teardown, or a same-identity
/// supersession — while an **unkeyed** run has no identity to supersede, so
/// only a teardown reaches it. A row that wants to witness the journal's
/// teardown and nothing else asserts on `anon`.
struct PaneState {
    /// Marked when the run `PaneMsg::Work` starts is reclaimed.
    keyed: Beacon,
    /// Marked when the run `PaneMsg::Anon` starts is reclaimed.
    anon: Beacon,
    /// Marked every time *this instance's* state is reduced — how a row
    /// tells the predecessor's state from the successor's.
    seen: Beacon,
    /// The sources this instance declares, if any.
    declares: Vec<ProbeSource>,
}

impl PaneState {
    /// Both runs marking one beacon, for rows that do not need them apart.
    fn new(reclaimed: Beacon) -> Self {
        Self::with_runs(reclaimed.clone(), reclaimed)
    }

    /// The keyed run and the unkeyed run marking their own beacons.
    fn with_runs(keyed: Beacon, anon: Beacon) -> Self {
        Self {
            keyed,
            anon,
            seen: Beacon::default(),
            declares: Vec::new(),
        }
    }

    /// An instance whose *reductions* are what the row watches.
    fn watching(seen: Beacon) -> Self {
        Self {
            seen,
            ..Self::new(Beacon::default())
        }
    }

    fn declaring(reclaimed: Beacon, source: ProbeSource) -> Self {
        Self::declaring_all(reclaimed, vec![source])
    }

    /// An instance declaring one or more sources. The two-source case is
    /// what it exists for: a row that needs one identity shared with
    /// another instance and one only this instance declares.
    fn declaring_all(reclaimed: Beacon, sources: Vec<ProbeSource>) -> Self {
        Self {
            declares: sources,
            ..Self::new(reclaimed)
        }
    }
}

/// The child reducer every boundary in the stack composes.
struct Pane;

impl Reducer for Pane {
    type State = PaneState;
    type Message = PaneMsg;

    fn reduce(&self, state: &mut PaneState, message: PaneMsg) -> Command<PaneMsg> {
        state.seen.mark();
        // Parks forever holding a drop-marking guard, so the run's
        // reclamation is what the row reads.
        match message {
            PaneMsg::Work => holding_effect(state.keyed.clone())
                .map(|_| PaneMsg::Work)
                .cancellable(CommandId::new("work"))
                .into(),
            PaneMsg::Anon => holding_effect(state.anon.clone())
                .map(|_| PaneMsg::Work)
                .into(),
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
    /// Replace `key`'s occupant through [`Keyed::insert`] alone, returning
    /// no command — so nothing but the replacement's own teardown drives
    /// what follows.
    Replace(u8),
    /// The slot's shape of [`Act::Replace`]: [`Slot::present`] over an
    /// occupant, returning no command.
    Represent,
    /// Empty the slot.
    Dismiss,
}

/// The root state: the two collections the boundaries project, plus the
/// script.
struct RootState {
    panes: Keyed<u8, PaneState>,
    modal: Slot<PaneState>,
    acts: HashMap<u8, Act>,
    /// The instance each act that installs one takes, in order — an act
    /// that only removes takes none. The test builds them so it can watch
    /// each instance's runs separately.
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
                let reclaimed = successor.keyed.clone();
                state.panes.remove(&key);
                state.panes.insert(key, successor);
                // Placed under the same segment the boundary uses, so the
                // journal's teardown and this spawn address one prefix.
                holding_effect(reclaimed)
                    .map(|_| Msg::Act(0))
                    .cancellable(CommandId::new("work"))
                    .scoped(key)
                    .into()
            }
            Some(Act::Replace(key)) => {
                let successor = state
                    .successors
                    .pop_front()
                    .expect("the script supplies one successor per replacement");
                state.panes.insert(key, successor);
                Command::none()
            }
            Some(Act::Represent) => {
                let successor = state
                    .successors
                    .pop_front()
                    .expect("the script supplies one successor per replacement");
                state.modal.present(successor);
                Command::none()
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
    /// One instance per act that installs one, in order.
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
    // Built rather than mutated: `from_iter` records no removal at all, and
    // a first presentation into an empty slot records none either, so
    // bootstrap leaves the journals clean. Reaching for `insert` here would
    // owe a teardown to the first message that arrives — the rule the
    // collection module states for mutating outside a `reduce`.
    let mut state = RootState {
        panes: panes.into_iter().collect(),
        modal: Slot::empty(),
        acts,
        successors,
    };
    if let Some(occupant) = modal {
        state.modal.present(occupant);
    }
    (
        state,
        Command::stream(stream::iter(trigger).chain(stream::pending())).into(),
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
// instance's runs are reclaimed and the successor starts fresh rather than
// being replaced or suppressed by what it succeeded.
//
// **The load-bearing assertion is the *anonymous* run's reclamation.** The
// old instance holds one keyed run and one unkeyed one. The successor's
// spawn carries the same qualified identity as the old keyed run, so a
// kernel with no journal at all would still reclaim *that* one by
// `CancelInFlight` supersession — asserting on it would let a diff-based
// journal pass. The unkeyed run has no identity to supersede: a prefix
// teardown is the only thing that reaches it, and the journal is the only
// thing that emits one here. Verified by mutation: with the drain filtered
// to keys absent at drain time (a state-diff-equivalent journal), this row
// fails on `old_anon`. The module doc's table records which other row that
// mutation reaches.
#[test]
fn a_same_update_recreate_tears_the_old_instance_down_and_starts_the_successor_fresh() {
    let (old_keyed, old_anon, new) = (Beacon::default(), Beacon::default(), Beacon::default());
    let mut driver = driver(
        Setup::new(vec![
            Msg::Row(1, PaneMsg::Work),
            Msg::Row(1, PaneMsg::Anon),
            Msg::Act(1),
        ])
        .opening(vec![(
            1,
            PaneState::with_runs(old_keyed.clone(), old_anon.clone()),
        )])
        .acting(1, Act::Recreate(1))
        .succeeding(PaneState::new(new.clone())),
    );
    let trigger = driver.boot().started[0].clone();

    let started = deliver(&mut driver, &trigger);
    assert!(matches!(started.as_slice(), [RunKind::Keyed(_)]));
    let started = deliver(&mut driver, &trigger);
    assert!(
        matches!(started.as_slice(), [RunKind::Anonymous]),
        "the old instance also holds a run no identity can supersede: {started:?}"
    );

    let started = deliver(&mut driver, &trigger);

    assert!(
        matches!(started.as_slice(), [RunKind::Keyed(_)]),
        "the same command's spawn phase started the successor: {started:?}"
    );
    driver.settle(TEST_TURNS, || old_anon.marked());
    assert!(
        old_keyed.marked(),
        "the old keyed run went with it, by teardown and supersession alike"
    );
    assert!(
        !new.marked(),
        "and the successor is the run that survives the application point"
    );
}

// When a replacement's successor starts running what it declares, read
// through the kernel rather than inferred from the teardown contract.
//
// The replacing pass admits nothing: the teardown stop-requests the
// outgoing row's subscription run, and the uniform barrier defers every
// admission runtime-wide while a subscription run is still stopping
// (RFC 0014 §5.1). What ends the gap is the predecessor's own exit — its
// quiescence marks subscriptions dirty (§5.2) and `ProducerExit` is a wake
// source — so no unrelated event is needed, and the row drives that pass by
// that source to say so.
//
// The successor declares two sources and the predecessor one of them. The
// shared identity is the case the gap is hardest to see in — the same
// `SubscriptionId` leaves and returns, with one pass in between running
// none of it — and it is on its own indistinguishable from a predecessor
// left in place re-declaring at its own exit. The source only the successor
// declares is what tells those apart.
#[test]
fn a_replacement_s_successor_declares_again_at_the_predecessor_s_exit() {
    let shared = ProbeSource::silent("both");
    let successor_only = ProbeSource::silent("successor");
    let mut driver = driver(
        Setup::new(vec![Msg::Act(1)])
            .opening(vec![(
                1,
                PaneState::declaring(Beacon::default(), shared.clone()),
            )])
            .acting(1, Act::Replace(1))
            .succeeding(PaneState::declaring_all(
                Beacon::default(),
                vec![shared.clone(), successor_only.clone()],
            )),
    );
    let trigger = driver.boot().started[0].clone();
    assert_eq!(
        (shared.admissions(), successor_only.admissions()),
        (1, 0),
        "boot admitted the row's one declaration"
    );

    deliver(&mut driver, &trigger);
    assert_eq!(
        (shared.admissions(), successor_only.admissions()),
        (1, 0),
        "the replacing pass admits nothing behind its own teardown"
    );

    // `settle` turns the executor without running a pass, and admission
    // happens only inside one, so there is nothing to assert between here
    // and the step below that a mutation could reach.
    driver.settle(TEST_TURNS, || shared.quiescences() > 0);

    assert!(
        driver.step_pass(WakeSource::Data).is_err(),
        "no unrelated arrival is waiting to drive the pass"
    );

    let stepped = driver
        .step_pass(WakeSource::ProducerExit)
        .expect("the predecessor's exit is a wake source of its own");
    assert_eq!(
        successor_only.admissions(),
        1,
        "the successor is what declares, not a predecessor left in place"
    );
    assert_eq!(
        shared.admissions(),
        2,
        "and the identity both instances declare left and returned"
    );
    assert_eq!(
        stepped.started.len(),
        2,
        "both are fresh runs, started by that pass"
    );
}

// The slot's replacing shape, which is what `ReducerExt::presented` states
// shares `for_each`'s timing. Only that shape differs from the row above —
// `Slot::present` over an occupant rather than `Keyed::insert` over a key —
// so this row reads the two facts the shape has to reach and leaves the
// wake-source negative probe to the row that established it.
#[test]
fn a_replaced_slot_occupant_s_successor_declares_again_at_the_predecessor_s_exit() {
    let shared = ProbeSource::silent("both");
    let successor_only = ProbeSource::silent("successor");
    let mut driver = driver(
        Setup::new(vec![Msg::Act(1)])
            .presenting(PaneState::declaring(Beacon::default(), shared.clone()))
            .acting(1, Act::Represent)
            .succeeding(PaneState::declaring_all(
                Beacon::default(),
                vec![shared.clone(), successor_only.clone()],
            )),
    );
    let trigger = driver.boot().started[0].clone();
    assert_eq!(
        (shared.admissions(), successor_only.admissions()),
        (1, 0),
        "boot admitted the occupant's one declaration"
    );

    deliver(&mut driver, &trigger);
    assert_eq!(
        (shared.admissions(), successor_only.admissions()),
        (1, 0),
        "the replacing pass admits nothing behind its own teardown"
    );

    driver.settle(TEST_TURNS, || shared.quiescences() > 0);
    let stepped = driver
        .step_pass(WakeSource::ProducerExit)
        .expect("the predecessor's exit is a wake source of its own");
    assert_eq!(
        (shared.admissions(), successor_only.admissions()),
        (2, 1),
        "the successor is what declares, at the predecessor's exit"
    );
    assert_eq!(
        stepped.started.len(),
        2,
        "both are fresh runs, started by that pass"
    );
}

// The condition on the row above, from its other side. The defer is
// `issued || any_stopping_sub`, so a replacement whose predecessor holds a
// run the barrier does not read leaves it nothing to hold: the successor's
// declarations are admitted in the pass that replaced it. What
// `ReducerExt::for_each` states is conditioned on a *subscription* run being
// stopped, and this is that condition read off the kernel rather than off
// the sentence.
//
// So the predecessor is given a keyed command run to hold — `PaneMsg::Work`
// before the replacement — and the teardown stops it. A predecessor holding
// nothing at all would leave the barrier nothing to read either, and the row
// would pass without the sub-only scope being what made it. That scope is
// INV-RC12 (a)'s and is pinned in `lifecycle`; what this row adds is a
// boundary's replacement reaching it.
#[test]
fn a_replacement_that_stops_no_subscription_declares_in_the_replacing_pass() {
    let successor_only = ProbeSource::silent("successor");
    let mut driver = driver(
        Setup::new(vec![Msg::Row(1, PaneMsg::Work), Msg::Act(1)])
            .opening(vec![(1, PaneState::new(Beacon::default()))])
            .acting(1, Act::Replace(1))
            .succeeding(PaneState::declaring(
                Beacon::default(),
                successor_only.clone(),
            )),
    );
    let trigger = driver.boot().started[0].clone();
    assert_eq!(
        successor_only.admissions(),
        0,
        "the outgoing row declared nothing to begin with"
    );

    let started = deliver(&mut driver, &trigger);
    assert!(
        matches!(started.as_slice(), [RunKind::Keyed(_)]),
        "the predecessor holds a command run for the teardown to stop: {started:?}"
    );

    deliver(&mut driver, &trigger);
    assert_eq!(
        successor_only.admissions(),
        1,
        "with no subscription run stopping, the replacing pass admits the successor's"
    );
}

// RFC 0013 INV-ST8's positive half, and RFC 0014 §2.5's third routing
// clause: key-addressed external input arriving after a same-key
// remove-and-reinsert reaches the **new** instance. Input carries no run
// origin — only producer output does — so the key is the whole of the
// routing, and the key now names the successor. The row reads it from the
// two instances' own reduction counts, so "reached the new instance" is the
// successor's *state* having been reduced rather than an inference from a
// command.
#[test]
fn a_key_addressed_message_after_a_reinsert_reaches_the_new_instance() {
    let (predecessor, successor) = (Beacon::default(), Beacon::default());
    let mut driver = driver(
        Setup::new(vec![
            Msg::Row(1, PaneMsg::Work),
            Msg::Act(1),
            Msg::Row(1, PaneMsg::Work),
        ])
        .opening(vec![(1, PaneState::watching(predecessor.clone()))])
        .acting(1, Act::Recreate(1))
        .succeeding(PaneState::watching(successor.clone())),
    );
    let trigger = driver.boot().started[0].clone();

    deliver(&mut driver, &trigger);
    assert_eq!(
        (predecessor.marks(), successor.marks()),
        (1, 0),
        "the first message reached the instance that was open then"
    );

    deliver(&mut driver, &trigger);
    let started = deliver(&mut driver, &trigger);

    assert_eq!(
        successor.marks(),
        1,
        "the message after the reinsert was reduced against the successor's own state"
    );
    assert_eq!(
        predecessor.marks(),
        1,
        "and the predecessor's state saw nothing further"
    );
    assert!(
        matches!(started.as_slice(), [RunKind::Keyed(_)]),
        "the successor answered it with its own run: {started:?}"
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
