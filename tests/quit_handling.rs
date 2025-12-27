#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used)]

use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use tears::{
    application::Application,
    command::{Action, Command},
    runtime::Runtime,
    subscription::Subscription,
};
use tokio::time::{Duration, Instant, sleep, timeout};

// Test application that sends quit from init command

#[tokio::test]
async fn test_quit_responsiveness_low_framerate() {
    // Test that quit is responsive even with low frame rate (16 FPS = 62.5ms per frame)
    // This test uses InitQuitApp which sends quit from init command
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    let runtime = Runtime::<InitQuitApp>::new(());

    let start = Instant::now();
    // Use low frame rate (16 FPS = 62.5ms per frame)
    let result = timeout(Duration::from_millis(200), runtime.run(&mut terminal, 16)).await;
    let elapsed = start.elapsed();

    assert!(result.is_ok(), "Runtime should quit within 200ms");
    assert!(
        result.unwrap().is_ok(),
        "Runtime should complete without error"
    );

    // With the fix, quit should happen much faster than frame duration (62.5ms)
    println!("Quit with low framerate took: {elapsed:?}");
    assert!(
        elapsed < Duration::from_millis(150),
        "Should quit quickly even with low framerate"
    );
}

struct InitQuitApp;

impl Application for InitQuitApp {
    type Message = ();
    type Flags = ();

    fn new(_flags: ()) -> (Self, Command<Self::Message>) {
        // Quit immediately after initialization
        (Self, Command::effect(Action::Quit))
    }

    fn update(&mut self, _msg: Self::Message) -> Command<Self::Message> {
        Command::none()
    }

    fn view(&self, _frame: &mut Frame<'_>) {}

    fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
        vec![]
    }
}

#[tokio::test]
async fn test_quit_from_init_command() {
    // Test that quit from init command is processed quickly
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    let runtime = Runtime::<InitQuitApp>::new(());

    let start = Instant::now();
    let result = timeout(Duration::from_secs(1), runtime.run(&mut terminal, 60)).await;
    let elapsed = start.elapsed();

    assert!(result.is_ok(), "Runtime should quit within 1 second");
    assert!(
        result.unwrap().is_ok(),
        "Runtime should complete without error"
    );

    // Should quit very quickly (within a few frames)
    println!("Init quit took: {elapsed:?}");
    assert!(
        elapsed < Duration::from_millis(200),
        "Should quit quickly from init command"
    );
}

// Test application that quits during frame wait
struct DelayedQuitApp;

impl Application for DelayedQuitApp {
    type Message = ();
    type Flags = ();

    fn new(_flags: ()) -> (Self, Command<Self::Message>) {
        // Schedule a quit command with delay
        let cmd = Command::future(async {
            sleep(Duration::from_millis(50)).await;
        });
        (Self, cmd)
    }

    fn update(&mut self, _msg: Self::Message) -> Command<Self::Message> {
        Command::effect(Action::Quit)
    }

    fn view(&self, _frame: &mut Frame<'_>) {}

    fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
        vec![]
    }
}

#[tokio::test]
async fn test_quit_during_frame_wait() {
    // Test that quit signal during frame wait is processed immediately
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    let runtime = Runtime::<DelayedQuitApp>::new(());

    let start = Instant::now();
    // Use very low frame rate (10 FPS = 100ms per frame)
    let result = timeout(Duration::from_millis(300), runtime.run(&mut terminal, 10)).await;
    let elapsed = start.elapsed();

    assert!(result.is_ok(), "Runtime should quit within 300ms");
    assert!(
        result.unwrap().is_ok(),
        "Runtime should complete without error"
    );

    // With tokio::select!, quit should happen around 50ms (command delay)
    // not 100ms+ (frame duration)
    println!("Delayed quit took: {elapsed:?}");
    assert!(
        elapsed < Duration::from_millis(200),
        "Should quit quickly even during frame wait"
    );
}

// Test application with multiple messages before quit
struct MultiMessageQuitApp {
    counter: u32,
}

#[derive(Debug, Clone)]
enum MultiMessage {
    Increment,
    Quit,
}

impl Application for MultiMessageQuitApp {
    type Message = MultiMessage;
    type Flags = ();

    fn new(_flags: ()) -> (Self, Command<Self::Message>) {
        // Send multiple messages, then quit
        let commands = vec![
            Command::future(async { MultiMessage::Increment }),
            Command::future(async { MultiMessage::Increment }),
            Command::future(async { MultiMessage::Increment }),
            Command::future(async { MultiMessage::Quit }),
        ];
        (Self { counter: 0 }, Command::batch(commands))
    }

    fn update(&mut self, msg: Self::Message) -> Command<Self::Message> {
        match msg {
            MultiMessage::Increment => {
                self.counter += 1;
                Command::none()
            }
            MultiMessage::Quit => Command::effect(Action::Quit),
        }
    }

    fn view(&self, _frame: &mut Frame<'_>) {}

    fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
        vec![]
    }
}

#[tokio::test]
async fn test_quit_after_multiple_messages() {
    // Test that quit is processed quickly even after multiple messages
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    let runtime = Runtime::<MultiMessageQuitApp>::new(());

    let start = Instant::now();
    let result = timeout(Duration::from_millis(500), runtime.run(&mut terminal, 60)).await;
    let elapsed = start.elapsed();

    assert!(result.is_ok(), "Runtime should quit within 500ms");
    assert!(
        result.unwrap().is_ok(),
        "Runtime should complete without error"
    );

    println!("Multi-message quit took: {elapsed:?}");
    assert!(
        elapsed < Duration::from_millis(300),
        "Should process messages and quit quickly"
    );
}
