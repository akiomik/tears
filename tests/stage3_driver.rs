//! The stage-3 driving layer, reached the way an application's tests reach
//! it: through the public paths and nothing else.
//!
//! This file compiles against the published API only, so it is the
//! regression for RFC 0008 §9.1's placement clause rather than for any
//! driving behaviour — the behaviour is covered by the conformance series,
//! which live in-crate because they name kernel internals this file cannot.
//! What breaks here is a surface that stopped being public: demote
//! `testing::driver` back to `pub(crate)`, or drop a name from the
//! re-export list, and this file fails to compile.
//!
//! §9.1 fixes three things about placement, and each has a row:
//!
//! - every name it introduces resolves under `tears::testing`;
//! - none is re-exported at the crate root;
//! - none is in the prelude.
//!
//! The last two are absence claims, which a test cannot assert by running.
//! They are asserted by construction instead: this file imports the prelude
//! *and* the driving names, and every driving name is written with its
//! `testing::` path spelled out. A crate-root re-export would make
//! `tears::TestDriver` resolve, which `api_surface`'s single-path row
//! already fails on; prelude membership would make the glob import below
//! shadow-import these names, which that suite's prelude-subset row fails
//! on. Those two rows are the enforcement; this file is what proves the
//! positive half they are stated against.

use std::num::NonZeroUsize;

use ratatui::backend::TestBackend;
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal};
use tears::RuntimeConfig;
use tears::prelude::*;
use tears::reducer::{Program, Reducer};

// All fourteen names RFC 0008 §9.1 introduces, each under `tears::testing`
// and under no other path. Naming them in a `use` is the compile-time half
// of this file's claim: a name that stops being public stops resolving here.
use tears::testing::{
    AcceptanceLedger, Confirmed, GrantOutstanding, GrantToken, IntentLedger, Lane, NotReady,
    ParkProbe, RunKind, RunName, SendRecord, StepReport, TestDriver, WakeSource,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Message {
    Ping,
}

#[derive(Debug, Default)]
struct State {
    pings: usize,
}

struct Counter;

impl Reducer for Counter {
    type State = State;
    type Message = Message;

    fn reduce(&self, state: &mut State, message: Message) -> Command<Message> {
        match message {
            Message::Ping => {
                state.pings += 1;
                Command::none()
            }
        }
    }
}

impl Program for Counter {
    type Flags = ();

    fn init(&self, (): ()) -> (State, Command<Message>) {
        (State::default(), Command::message(Message::Ping).into())
    }

    fn view(&self, state: &State, frame: &mut Frame<'_>) {
        frame.render_widget(Paragraph::new(format!("{}", state.pings)), frame.area());
    }
}

fn driver() -> TestDriver<Counter, TestBackend> {
    let terminal = Terminal::new(TestBackend::new(8, 2)).expect("test backend");
    TestDriver::new(Counter, (), RuntimeConfig::new(), terminal)
}

/// Boot and one pass, driven entirely through the public surface.
///
/// The init command starts an anonymous run, so the boot report names one;
/// granting that run's send and confirming it puts an entry in the
/// acceptance ledger, and a data-lane pass then delivers it. That exercises
/// both driving seams — pass initiation and the send gate — which is what
/// makes this a smoke test of the layer rather than of its constructor.
#[test]
fn a_program_boots_and_takes_one_pass_through_the_public_paths() {
    let mut driver = driver();

    let boot: StepReport<_> = driver.boot();
    assert!(boot.terminated.is_none(), "bootstrap did not terminate");
    let run: &RunName = boot.started.first().expect("init started a run");
    assert_eq!(
        run.kind(),
        RunKind::Anonymous,
        "an init effect is anonymous"
    );

    // The two ledgers say different things, and the gap between them is
    // exactly what a grant closes: the init effect has reached its send and
    // is waiting at the gate, so it is an intent and not yet an acceptance.
    let intents: IntentLedger = driver.intents();
    assert_eq!(intents.iter().len(), 1, "the init effect reached its send");
    assert!(
        driver.accepted().is_empty(),
        "nothing is admitted before a grant releases it"
    );

    let token: GrantToken = driver.grant(run.clone()).expect("no grant outstanding");
    assert_eq!(driver.confirm(64, token), Confirmed::Accepted);

    let accepted: AcceptanceLedger = driver.accepted();
    let record: &SendRecord = accepted.iter().next().expect("one accepted send");
    assert_eq!(record.lane(), Lane::Data);
    assert_eq!(record.run().kind(), RunKind::Anonymous);

    driver
        .step_pass(WakeSource::Data)
        .expect("the lane is ready");
    driver.settle(64, || true);
}

/// The refusal types are public too, and both are reachable by driving
/// rather than only by name: a second grant while one is outstanding is
/// refused, and a wake source that has not arrived drives nothing.
#[test]
fn the_two_refusals_are_public_and_reachable() {
    let mut driver = driver();
    let boot = driver.boot();
    let run = boot.started.first().expect("init started a run").clone();

    let token = driver.grant(run.clone()).expect("first grant is issued");
    // `GrantToken` is deliberately neither `Clone` nor `PartialEq`, so the
    // refusal is matched rather than compared.
    let second: Result<GrantToken, GrantOutstanding> = driver.grant(run);
    assert!(
        matches!(second, Err(GrantOutstanding)),
        "one grant at a time"
    );
    assert_eq!(driver.confirm(64, token), Confirmed::Accepted);

    // The control lane carries no quit, so a control-woken pass is refused.
    let refused: Result<StepReport<_>, NotReady> = driver.step_pass(WakeSource::Control);
    assert!(
        matches!(refused, Err(NotReady)),
        "no quit on the control lane"
    );

    driver
        .step_pass(WakeSource::Data)
        .expect("the lane is ready");
    driver.settle(64, || true);
}

/// `ParkProbe` is public on the same terms and needs no driver to construct.
#[test]
fn a_park_probe_is_constructible_and_counts_from_zero() {
    let probe = ParkProbe::new();
    assert_eq!(probe.wakes(), 0);
}

/// The bounded-lane constructor is public as well — `RuntimeConfig`'s
/// capacity control reaches the driver through the same path production
/// uses.
#[test]
fn a_bounded_lane_driver_is_constructible_through_the_public_config() {
    let terminal = Terminal::new(TestBackend::new(8, 2)).expect("test backend");
    let config = RuntimeConfig::new().data_lane_capacity(NonZeroUsize::new(1).expect("non-zero"));
    let mut driver = TestDriver::new(Counter, (), config, terminal);

    let boot = driver.boot();
    assert!(boot.terminated.is_none(), "a one-slot lane still boots");
}
