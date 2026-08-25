//! Integration tests for `Runtime::run`
//! These tests verify end-to-end scenarios.
//! Unit tests for individual methods are in src/runtime.rs

mod common;
#[path = "common/panic_hook.rs"]
mod panic_hook;
#[path = "common/trace_recorder.rs"]
mod trace_recorder;

use std::{
    future::pending,
    num::{NonZeroU64, NonZeroUsize},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use color_eyre::eyre::Result;
use ratatui::Frame;
use tears::command::RetryPolicy;
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
    #[expect(
        dead_code,
        reason = "this message variant completes the enum for the test app but is never constructed in this test"
    )]
    Increment,
}

impl Application for CounterApp {
    type Message = CounterMessage;
    type Flags = u32;

    fn new(max_count: u32) -> (Self, Command<Self::Message>) {
        let cmd = if max_count == 0 {
            // Quit immediately if max_count is 0
            Command::quit()
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
                    Command::quit()
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
            Command::quit()
        } else {
            Command::none()
        }
    }

    fn view(&self, _frame: &mut Frame<'_>) {}

    fn subscriptions(&self) -> Vec<Subscription<()>> {
        use tears::subscription::time::Timer;
        vec![Subscription::new(Timer::new(NonZeroU64::new(10).expect("non-zero"))).map(|_| ())]
    }
}

#[tokio::test]
async fn test_runtime_run_end_to_end_basic() -> Result<()> {
    // End-to-end: Basic application lifecycle
    let mut terminal = common::test_terminal()?;

    let runtime = Runtime::<CounterApp>::new(0); // Quit immediately

    let result = timeout(Duration::from_secs(1), runtime.run(&mut terminal)).await?;

    assert!(result.is_ok(), "Runtime should not error");

    Ok(())
}

#[tokio::test(flavor = "current_thread")]
#[expect(
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
                #[expect(
                    unreachable_code,
                    reason = "the preceding panic! diverges, so this trailing expression that types the async block is unreachable"
                )]
                Message::Quit
            });

            (Self, cmd.into())
        }

        fn update(&mut self, _msg: Message) -> Command<Message> {
            Command::quit()
        }

        fn view(&self, _frame: &mut Frame<'_>) {}

        fn subscriptions(&self) -> Vec<Subscription<Message>> {
            use tears::subscription::time::Timer;

            vec![
                Subscription::new(Timer::new(NonZeroU64::new(10).expect("non-zero")))
                    .map(|_| Message::Quit),
            ]
        }
    }

    // The default panic hook synchronously symbolicates a backtrace under
    // `RUST_BACKTRACE=1` (set repo-wide in CI). On Windows that can be slow
    // enough, on this `current_thread` runtime, to blow through the timeout
    // below before the Timer subscription gets a chance to send Quit. Silence
    // the hook for the panic this test triggers; see docs/testing.md.
    let recorder = TraceRecorder::new()
        .with_target("tears::kernel")
        .with_level(tracing::Level::ERROR);
    let _guard = recorder.set_default();

    let mut terminal = common::test_terminal()?;
    let runtime = Runtime::<PanicCommandApp>::new(());

    let run_result = panic_hook::with_silent_panic_hook(timeout(
        Duration::from_secs(5),
        runtime.run(&mut terminal),
    ))
    .await;
    run_result??;

    assert!(
        recorder.event_count() >= 1,
        "a panicking command task should log a tears::kernel error event"
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
                Command::future(async { "msg1".to_owned() }),
                Command::future(async { "msg2".to_owned() }),
                Command::future(async { "msg3".to_owned() }),
            ]);
            (Self { received: vec![] }, cmd)
        }

        fn update(&mut self, msg: String) -> Command<String> {
            self.received.push(msg);
            if self.received.len() >= 3 {
                Command::quit()
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

    let runtime = Runtime::<MessageApp>::new(());

    let result = timeout(Duration::from_secs(1), runtime.run(&mut terminal)).await?;

    assert!(result.is_ok());

    Ok(())
}

#[tokio::test]
async fn test_runtime_run_delivers_timeout_and_retry_messages_to_update() -> Result<()> {
    const TIMED_OUT: usize = 1;
    const RETRIED: usize = 2;

    struct LifecycleApp {
        observed: Arc<AtomicUsize>,
    }

    #[derive(Clone)]
    enum Message {
        TimedOut,
        Retried,
    }

    impl Application for LifecycleApp {
        type Message = Message;
        type Flags = Arc<AtomicUsize>;

        fn new(observed: Self::Flags) -> (Self, Command<Self::Message>) {
            let timeout_command =
                Command::future(pending::<Message>()).timeout(Duration::ZERO, || Message::TimedOut);

            let mut attempts = 0;
            let retry_command = Command::retry(
                RetryPolicy::new(NonZeroUsize::new(2).expect("non-zero")),
                move |_| {
                    attempts += 1;
                    let attempt = attempts;
                    async move {
                        if attempt == 2 {
                            Ok(())
                        } else {
                            Err("transient")
                        }
                    }
                },
                |result| {
                    result.expect("second retry attempt should succeed");
                    Message::Retried
                },
            );

            (
                Self { observed },
                Command::batch([timeout_command, retry_command]),
            )
        }

        fn update(&mut self, message: Self::Message) -> Command<Self::Message> {
            let bit = match message {
                Message::TimedOut => TIMED_OUT,
                Message::Retried => RETRIED,
            };
            let observed = self.observed.fetch_or(bit, Ordering::SeqCst) | bit;

            if observed == (TIMED_OUT | RETRIED) {
                Command::quit()
            } else {
                Command::none()
            }
        }

        fn view(&self, _frame: &mut Frame<'_>) {}

        fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
            vec![]
        }
    }

    let observed = Arc::new(AtomicUsize::new(0));
    let mut terminal = common::test_terminal()?;
    let runtime = Runtime::<LifecycleApp>::new(Arc::clone(&observed));

    timeout(Duration::from_secs(1), runtime.run(&mut terminal)).await??;

    assert_eq!(observed.load(Ordering::SeqCst), TIMED_OUT | RETRIED);

    Ok(())
}

#[tokio::test]
async fn test_runtime_run_end_to_end_with_subscriptions() -> Result<()> {
    // End-to-end: Subscription message processing
    let mut terminal = common::test_terminal()?;

    let runtime = Runtime::<SubApp>::new(());

    let result = timeout(Duration::from_secs(1), runtime.run(&mut terminal)).await?;

    assert!(result.is_ok());

    Ok(())
}
