//! Test-only helpers shared across crate-internal unit tests.
//!
//! Keep helpers here when they depend on crate-private APIs or concrete
//! `tears` fixtures such as [`TestApp`]. Small dependency-free helpers may
//! intentionally be duplicated in integration-test `common` modules; that is
//! clearer than a workspace-only helper crate until reuse grows.

use std::sync::Mutex;

use ratatui::Frame;

use crate::application::Application;
use crate::command::{Action, Command};
use crate::subscription::Subscription;

pub use async_utils::{assert_pending_until, gate_fetches, wait_until};
pub use trace_recorder::TraceRecorder;

mod async_utils;
mod trace_recorder;

/// Serializes tests that install a process-global panic hook or deliberately
/// trigger panics.
///
/// The panic hook is process-global and shared across threads, so a test that
/// records hook activity and a test that panics must not run concurrently, or
/// the panicking test's hook invocation would pollute the recording one.
pub static PANIC_HOOK_GUARD: Mutex<()> = Mutex::new(());

/// A minimal counter [`Application`] shared by the runtime and runtime-core
/// tests. Increments on [`TestMessage::Increment`] and quits on
/// [`TestMessage::Quit`].
#[derive(Debug)]
pub struct TestApp {
    pub counter: i32,
    should_quit: bool,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum TestMessage {
    Increment,
    Quit,
}

impl Application for TestApp {
    type Message = TestMessage;
    type Flags = i32;

    fn new(initial: i32) -> (Self, Command<Self::Message>) {
        (
            Self {
                counter: initial,
                should_quit: false,
            },
            Command::none(),
        )
    }

    fn update(&mut self, msg: Self::Message) -> Command<Self::Message> {
        match msg {
            TestMessage::Increment => {
                self.counter += 1;
                Command::none()
            }
            TestMessage::Quit => {
                self.should_quit = true;
                Command::effect(Action::Quit)
            }
        }
    }

    fn view(&self, _frame: &mut Frame<'_>) {
        // No-op for testing
    }

    fn subscriptions(&self) -> Vec<Subscription<Self::Message>> {
        vec![]
    }
}
