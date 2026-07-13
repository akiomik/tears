// Intentionally duplicated with `tests/common/panic_hook.rs`. See
// docs/testing.md "Why Test Helpers Are Duplicated Instead of Shared" and
// "Process-Global Panic Hook Tests" for why, and how the two copies differ.

use std::future::Future;
use std::panic::{self, AssertUnwindSafe, PanicHookInfo};
use std::sync::{Mutex, PoisonError};
use std::thread;

use futures::FutureExt;

/// Serializes tests that install a process-global panic hook or deliberately
/// trigger panics.
///
/// The panic hook is process-global and shared across threads, so a test that
/// records hook activity and a test that panics must not run concurrently, or
/// the panicking test's hook invocation would pollute the recording one.
pub static PANIC_HOOK_GUARD: Mutex<()> = Mutex::new(());

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

/// Runs a future with a no-op process-global panic hook installed.
///
/// Panics from the future are caught long enough to restore the previous hook,
/// then resumed. Cancellation also restores the hook through the internal drop
/// guard. This helper is for `current_thread` tests because it holds
/// [`PANIC_HOOK_GUARD`] across `.await`.
#[allow(
    clippy::await_holding_lock,
    clippy::future_not_send,
    reason = "the helper intentionally serializes a process-global hook across a current-thread future"
)]
pub async fn with_silent_panic_hook<F, T>(future: F) -> T
where
    F: Future<Output = T>,
{
    let _hook_guard = PANIC_HOOK_GUARD
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[tokio::test(flavor = "current_thread")]
    #[allow(
        clippy::await_holding_lock,
        clippy::panic,
        reason = "the test verifies hook restoration across an intentional panic on one thread"
    )]
    async fn silent_scope_restores_the_previous_hook_before_resuming_a_panic() {
        let hook_guard = PANIC_HOOK_GUARD
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let original = panic::take_hook();
        let hook_calls = Arc::new(AtomicUsize::new(0));
        let recorded_calls = Arc::clone(&hook_calls);
        panic::set_hook(Box::new(move |_info| {
            recorded_calls.fetch_add(1, Ordering::SeqCst);
        }));

        let outcome = AssertUnwindSafe(run_with_silent_panic_hook(async {
            panic!("silenced panic");
        }))
        .catch_unwind()
        .await;
        let probe = panic::catch_unwind(|| panic!("restored hook probe"));

        let restored_hook = panic::take_hook();
        panic::set_hook(original);
        drop(restored_hook);
        drop(hook_guard);

        assert!(outcome.is_err());
        assert!(probe.is_err());
        assert_eq!(hook_calls.load(Ordering::SeqCst), 1);
    }
}
