// Integration tests for Runtime::run
// These tests verify end-to-end scenarios.
// Unit tests for individual methods are in src/runtime.rs

mod common;
#[path = "common/trace_recorder.rs"]
mod trace_recorder;

use color_eyre::eyre::Result;
use ratatui::Frame;
use tears::prelude::*;
use tokio::time::{Duration, timeout};
use trace_recorder::TraceRecorder;

// Helper: Simple counter app
#[derive(Debug)]
struct CounterApp {
    count: u32,
    max_count: u32,
}

#[derive(Debug, Clone)]
enum CounterMessage {
    #[allow(dead_code)]
    Increment,
}

impl Application for CounterApp {
    type Message = CounterMessage;
    type Flags = u32;

    fn new(max_count: u32) -> (Self, Command<Self::Message>) {
        let cmd = if max_count == 0 {
            // Quit immediately if max_count is 0
            Command::effect(Action::Quit)
        } else {
            Command::none()
        };

        (
            Self {
                count: 0,
                max_count,
            },
            cmd,
        )
    }

    fn update(&mut self, msg: Self::Message) -> Command<Self::Message> {
        match msg {
            CounterMessage::Increment => {
                self.count += 1;
                if self.count >= self.max_count {
                    Command::effect(Action::Quit)
                } else {
                    Command::none()
                }
            }
        }
    }

    fn view(&self, _frame: &mut Frame<'_>) {}

    fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
        vec![]
    }
}

// Helper: App with subscriptions
struct SubApp {
    tick_count: u32,
}

impl Application for SubApp {
    type Message = ();
    type Flags = ();

    fn new((): ()) -> (Self, Command<()>) {
        (Self { tick_count: 0 }, Command::none())
    }

    fn update(&mut self, (): ()) -> Command<()> {
        self.tick_count += 1;
        if self.tick_count >= 3 {
            Command::effect(Action::Quit)
        } else {
            Command::none()
        }
    }

    fn view(&self, _frame: &mut Frame<'_>) {}

    fn subscriptions(&self) -> Vec<Subscription<()>> {
        use tears::subscription::time::Timer;
        vec![
            Subscription::new(Timer::try_new(10).expect("timer interval must be non-zero"))
                .map(|_| ()),
        ]
    }
}

#[tokio::test]
async fn test_runtime_run_end_to_end_basic() -> Result<()> {
    // End-to-end: Basic application lifecycle
    let mut terminal = common::test_terminal()?;

    let runtime = Runtime::<CounterApp>::try_new(0, 60).expect("frame rate must be valid"); // Quit immediately

    let result = timeout(Duration::from_secs(1), runtime.run(&mut terminal)).await?;

    assert!(result.is_ok(), "Runtime should not error");

    Ok(())
}

#[tokio::test(flavor = "current_thread")]
#[allow(
    clippy::panic,
    reason = "the test intentionally panics inside a command"
)]
async fn test_runtime_run_logs_command_task_panic() -> Result<()> {
    struct PanicCommandApp;

    #[derive(Clone)]
    enum Message {
        Quit,
    }

    impl Application for PanicCommandApp {
        type Message = Message;
        type Flags = ();

        fn new((): ()) -> (Self, Command<Message>) {
            let cmd = Command::future(async {
                panic!("boom");
                #[allow(unreachable_code)]
                Message::Quit
            });

            (Self, cmd)
        }

        fn update(&mut self, _msg: Message) -> Command<Message> {
            Command::effect(Action::Quit)
        }

        fn view(&self, _frame: &mut Frame<'_>) {}

        fn subscriptions(&self) -> Vec<Subscription<Message>> {
            use tears::subscription::time::Timer;

            vec![
                Subscription::new(Timer::try_new(10).expect("timer interval must be non-zero"))
                    .map(|_| Message::Quit),
            ]
        }
    }

    let recorder = TraceRecorder::new()
        .with_target("tears::runtime")
        .with_level(tracing::Level::ERROR);
    let _guard = recorder.set_default();

    let mut terminal = common::test_terminal()?;
    let runtime = Runtime::<PanicCommandApp>::try_new((), 60).expect("frame rate must be valid");

    timeout(Duration::from_secs(1), runtime.run(&mut terminal)).await??;

    assert!(
        recorder.event_count() >= 1,
        "a panicking command task should log a tears::runtime error event"
    );

    Ok(())
}

#[tokio::test]
async fn test_runtime_run_end_to_end_with_commands() -> Result<()> {
    // End-to-end: Message processing from commands
    struct MessageApp {
        received: Vec<String>,
    }

    impl Application for MessageApp {
        type Message = String;
        type Flags = ();

        fn new((): ()) -> (Self, Command<String>) {
            let cmd = Command::batch(vec![
                Command::future(async { "msg1".to_string() }),
                Command::future(async { "msg2".to_string() }),
                Command::future(async { "msg3".to_string() }),
            ]);
            (Self { received: vec![] }, cmd)
        }

        fn update(&mut self, msg: String) -> Command<String> {
            self.received.push(msg);
            if self.received.len() >= 3 {
                Command::effect(Action::Quit)
            } else {
                Command::none()
            }
        }

        fn view(&self, _frame: &mut Frame<'_>) {}

        fn subscriptions(&self) -> Vec<Subscription<String>> {
            vec![]
        }
    }

    let mut terminal = common::test_terminal()?;

    let runtime = Runtime::<MessageApp>::try_new((), 60).expect("frame rate must be valid");

    let result = timeout(Duration::from_secs(1), runtime.run(&mut terminal)).await?;

    assert!(result.is_ok());

    Ok(())
}

#[tokio::test]
async fn test_runtime_run_end_to_end_with_subscriptions() -> Result<()> {
    // End-to-end: Subscription message processing
    let mut terminal = common::test_terminal()?;

    let runtime = Runtime::<SubApp>::try_new((), 60).expect("frame rate must be valid");

    let result = timeout(Duration::from_secs(1), runtime.run(&mut terminal)).await?;

    assert!(result.is_ok());

    Ok(())
}
