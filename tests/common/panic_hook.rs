// Focused local guard for integration tests that mutate the process-global
// panic hook, per docs/testing.md "Process-Global Panic Hook Tests". Kept
// separate from `crate::test_support::PANIC_HOOK_GUARD` (crate-internal,
// synchronous-only) because integration tests need to hold the guard across
// `.await` while a spawned command panics.

use std::panic::{self, PanicHookInfo};

use tokio::sync::Mutex;

/// Serializes integration tests that install a process-global panic hook.
///
/// The panic hook is process-global and tests in this binary can run
/// concurrently on different threads, so a test that silences the hook must
/// not overlap with another test that panics for real, or the latter's
/// message and location would be lost.
pub static PANIC_HOOK_GUARD: Mutex<()> = Mutex::const_new(());

type PanicHook = Box<dyn Fn(&PanicHookInfo<'_>) + Sync + Send + 'static>;

/// Installs a no-op panic hook for as long as the guard is held, restoring
/// the previous hook on drop.
///
/// The default hook synchronously captures and symbolicates a backtrace when
/// `RUST_BACKTRACE=1` (set repo-wide in CI), on the panicking thread, before
/// `catch_unwind` regains control. On Windows this symbolication can be slow
/// enough to blow through a bounded `timeout`, especially when the panic
/// happens on a `current_thread` runtime where the same thread also drives
/// the timers/subscriptions the test is waiting on. See docs/testing.md.
pub struct SilentPanicHook {
    previous: Option<PanicHook>,
}

impl SilentPanicHook {
    pub fn install() -> Self {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(|_info| {}));
        Self {
            previous: Some(previous),
        }
    }
}

impl Drop for SilentPanicHook {
    fn drop(&mut self) {
        if let Some(hook) = self.previous.take() {
            panic::set_hook(hook);
        }
    }
}
