//! Test-only helpers shared across crate-internal unit tests.
//!
//! Keep helpers here when they depend on crate-private APIs or concrete
//! `tears` fixtures such as [`TestApp`]. Small dependency-free helpers may
//! intentionally be duplicated in integration-test `common` modules; that is
//! clearer than a workspace-only helper crate until reuse grows.

pub use async_utils::{assert_pending_until, gate_fetches, wait_until};
pub use failing_backend::FailingBackend;
pub use panic_hook::{HookProbe, hook_guard, with_silent_panic_hook};
pub use trace_recorder::{TraceRecorder, set_default_subscriber};

mod async_utils;
mod failing_backend;
mod panic_hook;
mod trace_recorder;
