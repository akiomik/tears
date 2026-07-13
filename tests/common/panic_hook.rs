// Intentionally duplicated with `src/test_support/panic_hook.rs`. See
// docs/testing.md "Why Test Helpers Are Duplicated Instead of Shared" and
// "Process-Global Panic Hook Tests" for why, and how the two copies differ.

use std::future::Future;
use std::panic::{self, AssertUnwindSafe, PanicHookInfo};
use std::thread;

use futures::FutureExt;
use tokio::sync::Mutex;

/// Serializes integration tests that install a process-global panic hook.
///
/// The panic hook is process-global and tests in this binary can run
/// concurrently on different threads, so a test that silences the hook must
/// not overlap with another test that panics for real, or the latter's
/// message and location would be lost.
pub static PANIC_HOOK_GUARD: Mutex<()> = Mutex::const_new(());

type PanicHook = Box<dyn Fn(&PanicHookInfo<'_>) + Sync + Send + 'static>;

struct SilentPanicHook {
    previous: Option<PanicHook>,
}

impl SilentPanicHook {
    fn install() -> Self {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(|_info| {}));
        Self {
            previous: Some(previous),
        }
    }

    fn restore(mut self) {
        self.restore_inner();
    }

    fn restore_inner(&mut self) {
        if let Some(hook) = self.previous.take() {
            panic::set_hook(hook);
        }
    }
}

impl Drop for SilentPanicHook {
    fn drop(&mut self) {
        if !thread::panicking() {
            self.restore_inner();
        }
    }
}

/// Runs a future with the default panic reporter silenced.
///
/// The previous process-global hook is restored before a panic from `future`
/// is resumed. Cancellation restores it through the internal drop guard. The
/// default hook can synchronously symbolize a backtrace under
/// `RUST_BACKTRACE=1`, so timing-bound panic tests should use this scope rather
/// than installing hooks directly. See `docs/testing.md`.
pub async fn with_silent_panic_hook<F, T>(future: F) -> T
where
    F: Future<Output = T>,
{
    let _hook_guard = PANIC_HOOK_GUARD.lock().await;
    run_with_silent_panic_hook(future).await
}

async fn run_with_silent_panic_hook<F, T>(future: F) -> T
where
    F: Future<Output = T>,
{
    let hook = SilentPanicHook::install();
    let result = AssertUnwindSafe(future).catch_unwind().await;
    hook.restore();
    match result {
        Ok(output) => output,
        Err(payload) => panic::resume_unwind(payload),
    }
}
