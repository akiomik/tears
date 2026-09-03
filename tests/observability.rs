//! Integration test for the load-observability producer gauges (RFC 0006 §4.4,
//! INV-L13). These verify end-to-end that starting a subscription, an unkeyed
//! command, and a keyed command each raise the matching `tears::runtime::load`
//! gauge, and that every gauge falls back to zero once the run tears down. The
//! batch event, capacity-wait event, and the gauge/abort mechanics are covered
//! at their narrower deterministic layers in `src/runtime`.

mod common;
#[path = "common/gauges.rs"]
mod gauges;
#[path = "common/trace_recorder.rs"]
mod trace_recorder;

use std::future::pending;
use std::num::NonZeroU64;

use color_eyre::eyre::Result;
use gauges::{
    PRODUCER_GAUGES, SETTLE_STEPS, producer_gauge_report, producer_gauges_are_zero,
    producer_gauges_rose,
};
use ratatui::Frame;
use tears::command::CommandId;
use tears::prelude::*;
use tears::subscription::time::Timer;
use tokio::task::yield_now;
use tokio::time::{Duration, timeout};
use trace_recorder::TraceRecorder;

#[derive(Clone)]
enum Msg {
    Tick,
    Quit,
}

// The app keeps all three producer kinds active at once. `new` starts a
// top-level *keyed* parked command (raising `keyed_commands`; a batch would
// discard the key, so it must not be batched). Its subscription is a `Timer`
// (raising `subscriptions`). The timer's first tick spawns a short *unkeyed*
// command that emits `Quit` (raising `unkeyed_commands`, then lowering it on
// completion), which ends the run.
struct GaugeApp;

impl Application for GaugeApp {
    type Message = Msg;
    type Flags = ();

    fn new((): ()) -> (Self, Command<Self::Message>) {
        (
            Self,
            Command::future(pending::<Msg>())
                .cancellable(CommandId::new("keyed"))
                .into(),
        )
    }

    fn update(&mut self, msg: Self::Message) -> Command<Self::Message> {
        match msg {
            Msg::Tick => Command::future(async { Msg::Quit }).into(),
            Msg::Quit => Command::quit(),
        }
    }

    fn view(&self, _frame: &mut Frame<'_>) {}

    fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
        vec![
            Subscription::new(Timer::new(NonZeroU64::new(10).expect("non-zero")))
                .map(|_| Msg::Tick),
        ]
    }
}

// Producer gauges over a real run: each producer kind raises its field, and
// every field returns to zero once the run tears its producers down. Under a
// current-thread runtime the producers' gauge emissions land on the recorder's
// thread; paused time jumps straight to the timer tick.
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn producer_gauges_rise_and_fall_over_a_run() -> Result<()> {
    let recorder = TraceRecorder::new().with_target("tears::runtime::load");
    let _guard = recorder.set_default();

    let mut terminal = common::test_terminal()?;
    let runtime = Runtime::<GaugeApp>::new(());
    timeout(Duration::from_secs(5), runtime.run(&mut terminal))
        .await
        .expect("the timer tick should quit the run before the timeout")?;

    // The aborted subscription task drops its gauge guard on a later scheduler
    // pass. Poll for the gauges to settle to zero rather than assuming a fixed
    // number of passes — that count is a tokio cooperative-budget detail, so a
    // fixed wait would make this assertion flake on a scheduler change. The cap
    // only bounds a pathological hang; the loop exits the moment they settle.
    for _ in 0..SETTLE_STEPS {
        if producer_gauges_are_zero(&recorder, &PRODUCER_GAUGES) {
            break;
        }
        yield_now().await;
    }
    // The rise assertion comes first so a total-emission outage reports as
    // "never rose" rather than as a settle failure; the census assert then
    // carries the whole fall claim with its per-field diagnostic. Both halves
    // take the whole census, so a gauge added to it is held to both here.
    producer_gauges_rose(&recorder, &PRODUCER_GAUGES);

    assert!(
        producer_gauges_are_zero(&recorder, &PRODUCER_GAUGES),
        "every producer gauge must settle to zero: {:?}",
        producer_gauge_report(&recorder),
    );

    // INV-L13: every gauge event a real run emits carries the emitting
    // instance's `runtime_id` and its ordering `seq`, and one run is one
    // instance — the runtime builds a single `LoadObserver` at construction and
    // clones it to every producer, so all of these events share one id. The
    // `seq` values are checked for distinctness rather than for arrival-order
    // monotonicity: the schema orders gauge events by `seq`, not by arrival
    // (their strict increase per instance is pinned at the unit layer, where the
    // emission order is deterministic).
    let gauge_events: Vec<_> = recorder
        .field_name_sets()
        .into_iter()
        .filter(|fields| fields.iter().any(|name| name == "subscriptions"))
        .collect();
    assert!(!gauge_events.is_empty(), "gauge events should have fired");
    for fields in &gauge_events {
        for required in ["runtime_id", "seq"] {
            assert!(
                fields.iter().any(|name| name == required),
                "a gauge event is missing `{required}`: {fields:?}"
            );
        }
    }

    let ids = recorder.u64_values("runtime_id");
    assert!(
        ids.windows(2).all(|pair| pair[0] == pair[1]),
        "one run is one runtime instance, so its gauge events share one \
         runtime_id: {ids:?}"
    );

    let mut seqs = recorder.u64_values("seq");
    let emitted = seqs.len();
    seqs.sort_unstable();
    seqs.dedup();
    assert_eq!(
        seqs.len(),
        emitted,
        "no two gauge events of one runtime may share a seq: {seqs:?}"
    );

    Ok(())
}
