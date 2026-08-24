//! Integration tests for quit handling.
//!
//! Two rows that used to live here were selected by frame pacing — a quit at
//! a deliberately slow frame rate, and a quit arriving while the loop was
//! parked on the frame timer — and pacing is gone (RFC 0014 §6.3, §9 row 4).
//! Neither had a property left to assert: render cadence is pass-bounded now,
//! so there is no frame period for a quit to be gated by and no frame timer to
//! park on. The successor statements are the kernel's own: quit latency is
//! bounded by the batch cap (§3.3) and the parked-kernel wake sources are
//! INV-RC16's, both carried by the conformance series.

mod common;

use color_eyre::eyre::Result;
use ratatui::Frame;
use tears::prelude::*;

use tokio::time::{Duration, Instant, timeout};

// Test application that sends quit from init command

struct InitQuitApp;

impl Application for InitQuitApp {
    type Message = ();
    type Flags = ();

    fn new(_flags: ()) -> (Self, Command<Self::Message>) {
        // Quit immediately after initialization
        (Self, Command::quit())
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
async fn test_quit_from_init_command() -> Result<()> {
    // Test that quit from init command is processed quickly
    let mut terminal = common::test_terminal()?;

    let runtime = Runtime::<InitQuitApp>::new(());

    let start = Instant::now();
    let result = timeout(Duration::from_secs(1), runtime.run(&mut terminal)).await?;
    let elapsed = start.elapsed();

    assert!(result.is_ok(), "Runtime should complete without error");

    // Should quit very quickly (within a few frames)
    println!("Init quit took: {elapsed:?}");
    assert!(
        elapsed < Duration::from_millis(200),
        "Should quit quickly from init command"
    );

    Ok(())
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
            MultiMessage::Quit => Command::quit(),
        }
    }

    fn view(&self, _frame: &mut Frame<'_>) {}

    fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
        vec![]
    }
}

#[tokio::test]
async fn test_quit_after_multiple_messages() -> Result<()> {
    // Test that quit is processed quickly even after multiple messages
    let mut terminal = common::test_terminal()?;

    let runtime = Runtime::<MultiMessageQuitApp>::new(());

    let start = Instant::now();
    let result = timeout(Duration::from_millis(500), runtime.run(&mut terminal)).await?;
    let elapsed = start.elapsed();

    assert!(result.is_ok(), "Runtime should complete without error");

    println!("Multi-message quit took: {elapsed:?}");
    assert!(
        elapsed < Duration::from_millis(300),
        "Should process messages and quit quickly"
    );

    Ok(())
}
