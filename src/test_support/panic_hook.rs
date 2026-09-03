// Intentionally duplicated with `tests/common/panic_hook.rs`. See
// docs/testing.md "Why Test Helpers Are Duplicated Instead of Shared" and
// "Process-Global Panic Hook Tests" for why, and how the two copies differ.

use std::future::Future;
use std::panic::{self, AssertUnwindSafe, PanicHookInfo};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;

use futures::FutureExt;

use crate::panic::compose_hook;

/// Serializes tests that install a process-global panic hook or deliberately
/// trigger panics.
///
/// The panic hook is process-global and shared across threads, so a test that
/// records hook activity and a test that panics must not run concurrently, or
/// the panicking test's hook invocation would pollute the recording one.
static PANIC_HOOK_GUARD: Mutex<()> = Mutex::new(());

/// Locks [`PANIC_HOOK_GUARD`], recovering from poisoning.
///
/// Poisoning is the ordinary case here, not a corruption signal: the guard
/// serializes access to a process-global hook rather than to data, and the
/// tests it serializes panic on purpose, so any of them may unwind while
/// holding it. Recovering means one such panic fails its own test instead
/// of every later hook test, which would otherwise report a poison error in
/// place of their own assertions. This is the only place the recovery is
/// spelled out; hook tests call this rather than locking the static.
pub fn hook_guard() -> MutexGuard<'static, ()> {
    PANIC_HOOK_GUARD
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

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

/// Records how a [`compose_hook`](crate::panic::compose_hook)-composed hook
/// classified each panic, counting only panics raised on threads whose name
/// starts with the installing test's prefix.
///
/// Unlike [`with_silent_panic_hook`] (current-thread, async), this probe also
/// serves multi-thread runtimes and non-async tests; callers call
/// [`hook_guard`] and hold it for their whole critical section themselves.
/// That serializes the hook swaps but not the rest of the test binary: a
/// test that panics without taking the guard would still reach this hook. The
/// thread-name filter keeps such a panic out of the counts — libtest names
/// each test's thread after the test's full path, and multi-thread tests
/// stamp their runtime workers with their own prefix.
pub struct HookProbe {
    restores: Arc<AtomicUsize>,
    delegations: Arc<AtomicUsize>,
    previous: Option<PanicHook>,
}

impl HookProbe {
    pub fn install(thread_prefix: &'static str) -> Self {
        let restores = Arc::new(AtomicUsize::new(0));
        let delegations = Arc::new(AtomicUsize::new(0));

        let restore_counter = Arc::clone(&restores);
        let delegation_counter = Arc::clone(&delegations);
        let next: PanicHook = Box::new(move |_info| {
            if on_thread(thread_prefix) {
                delegation_counter.fetch_add(1, Ordering::SeqCst);
            }
        });
        let hook = compose_hook(
            move || {
                if on_thread(thread_prefix) {
                    restore_counter.fetch_add(1, Ordering::SeqCst);
                }
            },
            next,
        );

        let previous = panic::take_hook();
        panic::set_hook(hook);
        Self {
            restores,
            delegations,
            previous: Some(previous),
        }
    }

    /// The `(restores, delegations)` counts so far.
    pub fn counts(&self) -> (usize, usize) {
        (
            self.restores.load(Ordering::SeqCst),
            self.delegations.load(Ordering::SeqCst),
        )
    }

    /// Reinstalls the previous hook. Assertions run only after this, so a
    /// failing assertion still reports through the normal hook.
    pub fn finish(mut self) {
        self.restore_previous();
    }

    fn restore_previous(&mut self) {
        if let Some(hook) = self.previous.take() {
            panic::set_hook(hook);
        }
    }
}

impl Drop for HookProbe {
    fn drop(&mut self) {
        // `set_hook` panics when called from a panicking thread, so an
        // unwinding test leaves the probe hook installed rather than
        // aborting the process; the next test's `install` replaces it.
        if !thread::panicking() {
            self.restore_previous();
        }
    }
}

/// Reports whether the calling thread's name starts with `prefix`.
///
/// The filter a recording panic hook applies to keep panics raised by other
/// tests out of its records (docs/testing.md "Process-Global Panic Hook
/// Tests"): libtest names each test's thread after the test's full path, and
/// a test that drives a multi-thread runtime stamps its workers with a prefix
/// of its own.
pub fn on_thread(prefix: &str) -> bool {
    thread::current()
        .name()
        .is_some_and(|name| name.starts_with(prefix))
}

/// Runs a future with a no-op process-global panic hook installed.
///
/// Panics from the future are caught long enough to restore the previous hook,
/// then resumed. Cancellation also restores the hook through the internal drop
/// guard. This helper is for `current_thread` tests because it holds
/// [`PANIC_HOOK_GUARD`] across `.await`.
#[expect(
    clippy::await_holding_lock,
    clippy::future_not_send,
    reason = "the helper intentionally serializes a process-global hook across a current-thread future"
)]
pub async fn with_silent_panic_hook<F, T>(future: F) -> T
where
    F: Future<Output = T>,
{
    let _hook_guard = hook_guard();
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
    #[expect(
        clippy::await_holding_lock,
        clippy::panic,
        reason = "the test verifies hook restoration across an intentional panic on one thread"
    )]
    async fn silent_scope_restores_the_previous_hook_before_resuming_a_panic() {
        // Counts only panics raised on this test's own thread, the same
        // filter `HookProbe` applies and for the same reason: the guard
        // serializes hook swaps, not the rest of the binary, so a panic
        // from any concurrently running test would otherwise land in this
        // counter (docs/testing.md "Process-Global Panic Hook Tests").
        const PROBE: &str =
            "test_support::panic_hook::tests::silent_scope_restores_the_previous_hook";

        let guard = hook_guard();
        let original = panic::take_hook();
        let hook_calls = Arc::new(AtomicUsize::new(0));
        let recorded_calls = Arc::clone(&hook_calls);
        panic::set_hook(Box::new(move |_info| {
            if on_thread(PROBE) {
                recorded_calls.fetch_add(1, Ordering::SeqCst);
            }
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
        drop(guard);

        assert!(outcome.is_err());
        assert!(probe.is_err());
        assert_eq!(hook_calls.load(Ordering::SeqCst), 1);
    }
}
