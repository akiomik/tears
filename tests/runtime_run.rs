// Integration tests for Runtime::run
// These tests verify end-to-end scenarios.
// Unit tests for individual methods are in src/runtime.rs

use ratatui::{Frame, Terminal, backend::TestBackend};
use tears::{
    application::Application,
    command::{Action, Command},
    runtime::Runtime,
    subscription::Subscription,
};
use tokio::time::{Duration, timeout};

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
            CounterApp {
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

    fn new(_: ()) -> (Self, Command<()>) {
        (SubApp { tick_count: 0 }, Command::none())
    }

    fn update(&mut self, _: ()) -> Command<()> {
        self.tick_count += 1;
        if self.tick_count >= 3 {
            Command::effect(Action::Quit)
        } else {
            Command::none()
        }
    }

    fn view(&self, _frame: &mut Frame<'_>) {}

    fn subscriptions(&self) -> Vec<Subscription<()>> {
        use tears::subscription::time::TimeSub;
        vec![Subscription::new(TimeSub::new(10)).map(|_| ())]
    }
}

#[tokio::test]
async fn test_runtime_run_end_to_end_basic() {
    // End-to-end: Basic application lifecycle
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    let runtime = Runtime::<CounterApp>::new(0); // Quit immediately

    let result = timeout(Duration::from_secs(1), runtime.run(&mut terminal, 60)).await;

    assert!(result.is_ok(), "Runtime should complete");
    assert!(result.unwrap().is_ok(), "Runtime should not error");
}

#[tokio::test]
async fn test_runtime_run_end_to_end_with_commands() {
    // End-to-end: Message processing from commands
    struct MessageApp {
        received: Vec<String>,
    }

    impl Application for MessageApp {
        type Message = String;
        type Flags = ();

        fn new(_: ()) -> (Self, Command<String>) {
            let cmd = Command::batch(vec![
                Command::future(async { "msg1".to_string() }),
                Command::future(async { "msg2".to_string() }),
                Command::future(async { "msg3".to_string() }),
            ]);
            (MessageApp { received: vec![] }, cmd)
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

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    let runtime = Runtime::<MessageApp>::new(());

    let result = timeout(Duration::from_secs(1), runtime.run(&mut terminal, 60)).await;

    assert!(result.is_ok());
    assert!(result.unwrap().is_ok());
}

#[tokio::test]
async fn test_runtime_run_end_to_end_with_subscriptions() {
    // End-to-end: Subscription message processing
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    let runtime = Runtime::<SubApp>::new(());

    let result = timeout(Duration::from_secs(1), runtime.run(&mut terminal, 60)).await;

    assert!(result.is_ok(), "Runtime should complete with subscriptions");
    assert!(result.unwrap().is_ok());
}
