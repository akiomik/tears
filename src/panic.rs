//! Panic handling helpers for restoring the terminal on unwind.
//!
//! This module is crate-internal; see [`install_panic_hook`](crate::install_panic_hook)
//! (the sole public item re-exported from here) for the full documentation.

use std::future::Future;
use std::panic::{self, PanicHookInfo};

use tokio::task::futures::TaskLocalFuture;

/// The boxed panic hook type used by [`std::panic::set_hook`].
type PanicHook = Box<dyn Fn(&PanicHookInfo<'_>) + Sync + Send + 'static>;

tokio::task_local! {
    /// Marks the body of a runtime-owned producer task whose panics the runtime
    /// contains.
    ///
    /// Tokio installs the value on entry to every `poll` of the wrapping future
    /// and removes it on exit, including when that poll unwinds. The mark
    /// therefore brackets a single synchronous `poll`: it can never span an
    /// `.await` point, leak onto a worker thread that only parked the task, or
    /// fail to follow a task that migrates to another worker thread.
    static CONTAINED_PRODUCER: ();
}

/// Installs a panic hook that restores the terminal before delegating to the
/// previously installed hook.
///
/// A TUI application puts the terminal into raw mode and an alternate screen.
/// If the application panics, the stack unwinds straight out of
/// [`Runtime::run`](crate::Runtime::run) and the `ratatui::restore()` call
/// that would normally run afterwards is skipped, leaving the user's
/// terminal in a broken state (no echo, no line editing, stuck on the
/// alternate screen). This function wraps the current panic hook so the
/// terminal is restored *before* the original hook runs. Because it chains
/// into the existing hook rather than replacing it, panic reporters such as
/// `color_eyre` still print their report — now on a restored, readable
/// terminal.
///
/// Call it once, after initializing the terminal (and after installing any
/// panic reporter such as `color_eyre`):
///
/// ```rust,no_run
/// # use color_eyre::eyre::Result;
/// # fn main() -> Result<()> {
/// color_eyre::install()?;
/// let mut terminal = ratatui::init();
/// tears::install_panic_hook();
/// // ... run the application ...
/// # Ok(())
/// # }
/// ```
///
/// Panics the runtime contains are exempt from the restore. A panic inside a
/// runtime-owned task — an unkeyed command task, a keyed command task, a
/// subscription forwarder, or a [`Command::on_teardown`](crate::Command::on_teardown)
/// finalizer, which is spawned through the same contained path — is caught by
/// the runtime and does
/// not terminate the application (RFC 0011 §5, INV-LC8): the event loop keeps
/// running and keeps drawing. Restoring the terminal for such a panic would
/// drop a live UI out of raw mode and off the alternate screen, so the hook
/// only delegates to the previous hook in that case.
///
/// The delegated report of a contained panic is therefore written while the
/// terminal is still in raw mode and on the alternate screen: line breaks
/// stair-step, and the report's cells linger until the next redraw happens to
/// touch them. An application that wants contained-panic reports to stay
/// readable should point its panic reporter at a file or log target rather
/// than stderr.
///
/// Containment presupposes unwinding. Under `panic = "abort"` no panic is
/// contained — every panic ends the process — so the hook restores the
/// terminal unconditionally there, exactly as it does for driving-path panics.
///
/// Call it only once. Each call wraps the previous hook, so repeated calls
/// would restore the terminal multiple times (harmless, but pointless).
pub fn install_panic_hook() {
    let original = panic::take_hook();
    panic::set_hook(compose_hook(
        || {
            // Restore the terminal to its normal state (leave raw mode and the
            // alternate screen). Errors are ignored: we are already panicking and
            // there is nothing useful to do with a restore failure.
            ratatui::restore();
        },
        original,
    ));
}

/// Marks `body` as a runtime-owned producer task body whose panics the runtime
/// contains (RFC 0011 §5, INV-LC8).
///
/// The panic hook runs *before* the spawn site's `catch_unwind` regains
/// control, so without this mark it would restore the terminal for a panic
/// that leaves the application running. Wrapping the task body makes the
/// containment visible to the hook, which then skips the restore.
///
/// This is the single placement point for the mark: every runtime-owned
/// producer spawn site wraps its task body with it, so unifying those spawn
/// paths later moves one call each rather than a mechanism.
pub fn contained_producer<F: Future>(body: F) -> TaskLocalFuture<(), F> {
    CONTAINED_PRODUCER.scope((), body)
}

/// Reports whether the current thread is inside a [`contained_producer`] poll,
/// i.e. whether a panic raised here is one the runtime contains.
fn in_contained_producer() -> bool {
    CONTAINED_PRODUCER.try_with(|&()| ()).is_ok()
}

/// Composes a panic hook that runs `restore` and then delegates to `next`.
///
/// `restore` is skipped for a panic unwinding out of a [`contained_producer`]
/// poll, because the application it would restore the terminal away from is
/// still running (INV-LC8); the delegation to `next` is unconditional either
/// way. Under `panic = "abort"` the spawn sites' `catch_unwind` cannot contain
/// anything and the process is about to die, so the restore is unconditional
/// — the guard below is compile-time and untestable from this (necessarily
/// unwinding) test suite.
///
/// Extracted from [`install_panic_hook`] so the chaining order can be tested
/// without touching a real terminal.
pub fn compose_hook(restore: impl Fn() + Sync + Send + 'static, next: PanicHook) -> PanicHook {
    Box::new(move |info| {
        if cfg!(panic = "abort") || !in_contained_producer() {
            restore();
        }
        next(info);
    })
}

#[cfg(test)]
mod tests {
    use crate::test_support::{HookProbe, hook_guard, on_thread};

    use super::*;

    use std::future::pending;
    use std::panic::AssertUnwindSafe;
    use std::sync::{Arc, Mutex};
    use std::thread;

    use futures::future::join_all;
    use tokio::runtime::Builder;
    use tokio::sync::oneshot;
    use tokio::task::yield_now;

    /// Yields enough times to give the multi-thread scheduler opportunities to
    /// move the task to another worker between polls.
    async fn yield_repeatedly() {
        for _ in 0..8 {
            yield_now().await;
        }
    }

    /// Runs `trigger` with a composed hook installed that records the order of
    /// its two stages, counting only panics raised on threads named with
    /// `prefix`, and returns what it recorded.
    ///
    /// The filter is the recording test's own obligation: the caller's
    /// [`hook_guard`] serializes hook swaps, not the rest of the binary, so any
    /// `#[should_panic]` row passing on another thread would otherwise append a
    /// stage pair of its own (docs/testing.md "Process-Global Panic Hook
    /// Tests"). Callers hold the guard across the whole call.
    ///
    /// An unexpected panic out of `trigger` — a failed thread spawn, say — is
    /// caught long enough to reinstall the previous hook and then resumed, the
    /// same shape `with_silent_panic_hook` uses. A drop guard could not do it:
    /// `set_hook` panics when called from a panicking thread, so restoring
    /// mid-unwind is not available, and leaving the recording hook installed
    /// would strip every later panic in the binary of its reported location.
    fn record_hook_stages(prefix: &'static str, trigger: impl FnOnce()) -> Vec<&'static str> {
        let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let restore_order = order.clone();
        let next_order = order.clone();

        let next: PanicHook = Box::new(move |_info| {
            if on_thread(prefix) {
                next_order
                    .lock()
                    .expect("order mutex should not be poisoned")
                    .push("next");
            }
        });
        let hook = compose_hook(
            move || {
                if on_thread(prefix) {
                    restore_order
                        .lock()
                        .expect("order mutex should not be poisoned")
                        .push("restore");
                }
            },
            next,
        );

        let previous = panic::take_hook();
        panic::set_hook(hook);
        let triggered = panic::catch_unwind(AssertUnwindSafe(trigger));
        panic::set_hook(previous);
        if let Err(payload) = triggered {
            panic::resume_unwind(payload);
        }

        order
            .lock()
            .expect("order mutex should not be poisoned")
            .clone()
    }

    #[test]
    #[expect(clippy::panic, reason = "driving the panic hook requires a real panic")]
    fn test_compose_hook_restores_then_delegates_once() {
        // libtest names each test's thread after the test's full path, and the
        // panic below is raised inline, so this is the thread the hook records
        // from.
        const PROBE: &str = "panic::tests::test_compose_hook_restores_then_delegates_once";

        // Serialize against other tests that swap the global panic hook, so
        // one of them cannot install over the recording hook.
        let _hook_guard = hook_guard();

        // `restore` must run first so the terminal is usable before the
        // delegated reporter prints. `PanicHookInfo` cannot be constructed
        // directly, so drive the composed hook through the real panic runtime
        // and catch the unwind.
        let recorded = record_hook_stages(PROBE, || {
            let _ = panic::catch_unwind(|| panic!("trigger"));
        });

        assert_eq!(
            recorded,
            vec!["restore", "next"],
            "restore must run exactly once, before the delegated hook"
        );
    }

    #[test]
    #[expect(clippy::panic, reason = "driving the panic hook requires a real panic")]
    fn test_recording_hook_ignores_a_panic_from_another_thread() {
        const PROBE: &str = "panic::tests::test_recording_hook_ignores_a_panic_from_another_thread";
        const FOREIGN: &str = "tears-panic-foreign";

        // A panic on a differently named thread, standing in for the
        // `#[should_panic]` rows: they panic on their own libtest threads and
        // take no guard, so their panics reach whichever hook is installed.
        fn panic_elsewhere() {
            thread::Builder::new()
                .name(FOREIGN.to_owned())
                .spawn(|| {
                    let _ = panic::catch_unwind(|| panic!("foreign panic"));
                })
                .expect("the foreign thread should spawn")
                .join()
                .expect("the foreign thread should not unwind");
        }

        let _hook_guard = hook_guard();

        // A foreign panic on either side of this test's own, the interleaving
        // that appended a second and a third stage pair before the filter. The
        // inline panic is also the positive control: without it the assertion
        // below would pass on an empty record, and so would survive a `PROBE`
        // that had stopped matching the thread libtest names after this test.
        let recorded = record_hook_stages(PROBE, || {
            panic_elsewhere();
            let _ = panic::catch_unwind(|| panic!("trigger"));
            panic_elsewhere();
        });

        assert_eq!(
            recorded,
            vec!["restore", "next"],
            "only this test's own panic may be recorded"
        );
    }

    #[test]
    #[expect(clippy::panic, reason = "driving the panic hook requires a real panic")]
    fn test_an_unwinding_trigger_restores_the_previous_hook() {
        const PROBE: &str = "panic::tests::test_an_unwinding_trigger_restores_the_previous_hook";

        let _hook_guard = hook_guard();

        // Stands in for the previous hook — the real one reports a panic's
        // location. A recording hook left installed over it would strip that
        // from every later panic in the binary, so the helper has to reinstall
        // it even when the trigger unwinds, as a failed thread spawn would.
        let reports = Arc::new(Mutex::new(0_usize));
        let counted = reports.clone();
        let original = panic::take_hook();
        panic::set_hook(Box::new(move |_info| {
            if on_thread(PROBE) {
                *counted
                    .lock()
                    .expect("report counter should not be poisoned") += 1;
            }
        }));

        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
            record_hook_stages(PROBE, || panic!("the trigger unwinds"))
        }));
        // Reaches the hook that is installed now, whichever one that is.
        let _ = panic::catch_unwind(|| panic!("after the window"));

        let installed = panic::take_hook();
        panic::set_hook(original);
        drop(installed);

        assert!(outcome.is_err(), "the trigger's panic must be resumed");
        assert_eq!(
            *reports
                .lock()
                .expect("report counter should not be poisoned"),
            1,
            "the previous hook must be back once the window closes"
        );
    }

    #[test]
    #[expect(clippy::panic, reason = "driving the panic hook requires a real panic")]
    fn test_contained_producer_panic_skips_restore_while_application_panic_restores() {
        const PROBE: &str = "tears-panic-probe-contained";

        let _hook_guard = hook_guard();
        let runtime = Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name(PROBE)
            .build()
            .expect("probe runtime should build");
        let probe = HookProbe::install(PROBE);

        // A panic the runtime contains (INV-LC8): the application keeps
        // running, so its terminal must be left as it is.
        let contained = runtime.block_on(runtime.spawn(contained_producer(async {
            panic!("contained producer panicked");
        })));
        let after_contained = probe.counts();
        let survivor = runtime.block_on(runtime.spawn(async { 7_u32 }));

        // A panic on the application's own path stays fail-fast and restores.
        let application = runtime.block_on(runtime.spawn(async {
            panic!("application panicked");
        }));
        let after_application = probe.counts();

        probe.finish();

        assert!(contained.is_err(), "the contained producer should panic");
        assert_eq!(
            after_contained,
            (0, 1),
            "a contained producer panic must delegate without restoring the terminal"
        );
        assert_eq!(
            survivor.expect("the runtime should keep running after a contained panic"),
            7,
            "producers spawned after a contained panic must still run"
        );
        assert!(application.is_err(), "the application task should panic");
        assert_eq!(
            after_application,
            (1, 2),
            "an application panic must restore the terminal as before"
        );
    }

    #[test]
    #[expect(clippy::panic, reason = "driving the panic hook requires a real panic")]
    fn test_contained_mark_does_not_span_await_points() {
        const PROBE: &str = "tears-panic-probe-parked";

        let _hook_guard = hook_guard();
        // One worker thread, so the unrelated task below is guaranteed to run
        // on the very thread that parked the contained producer — the exact
        // condition a mark held across `.await` would misclassify.
        let runtime = Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name(PROBE)
            .build()
            .expect("probe runtime should build");
        let probe = HookProbe::install(PROBE);

        let (parked_tx, parked_rx) = oneshot::channel();
        let (resume_tx, resume_rx) = oneshot::channel();
        let producer = runtime.spawn(contained_producer(async move {
            let _ = parked_tx.send(());
            let _ = resume_rx.await;
            panic!("contained producer panicked after resuming");
        }));
        runtime.block_on(async {
            let _ = parked_rx.await;
        });

        let unrelated = runtime.block_on(runtime.spawn(async {
            panic!("unrelated task panicked");
        }));
        let after_unrelated = probe.counts();

        // The producer resumes on a later poll — possibly on another worker
        // thread — and its panic must still be classified as contained.
        let _ = resume_tx.send(());
        let resumed = runtime.block_on(producer);
        let after_resumed = probe.counts();

        probe.finish();

        assert!(unrelated.is_err(), "the unrelated task should panic");
        assert_eq!(
            after_unrelated,
            (1, 1),
            "a panic on a thread that only parked a contained producer must restore"
        );
        assert!(resumed.is_err(), "the resumed producer should panic");
        assert_eq!(
            after_resumed,
            (1, 2),
            "the contained mark must be re-established on the poll that resumes the producer"
        );
    }

    #[test]
    #[expect(clippy::panic, reason = "driving the panic hook requires a real panic")]
    fn test_contained_classification_survives_worker_migration() {
        const PROBE: &str = "tears-panic-probe-migrating";
        const TASKS: usize = 8;

        let _hook_guard = hook_guard();
        let runtime = Builder::new_multi_thread()
            .worker_threads(4)
            .thread_name(PROBE)
            .build()
            .expect("probe runtime should build");
        let probe = HookProbe::install(PROBE);

        // Contained and uncontained panics interleave across four workers,
        // each task yielding repeatedly before it panics. Whichever worker
        // ends up polling a task, its classification must follow the task.
        let outcomes = runtime.block_on(async {
            let mut handles = Vec::with_capacity(TASKS * 2);
            for _ in 0..TASKS {
                handles.push(tokio::spawn(contained_producer(async {
                    yield_repeatedly().await;
                    panic!("contained producer panicked");
                })));
                handles.push(tokio::spawn(async {
                    yield_repeatedly().await;
                    panic!("application panicked");
                }));
            }
            join_all(handles).await
        });
        let counts = probe.counts();

        probe.finish();

        assert!(
            outcomes.iter().all(Result::is_err),
            "every spawned task should panic"
        );
        assert_eq!(
            counts,
            (TASKS, TASKS * 2),
            "only the uncontained half may restore, however the workers interleave"
        );
    }

    #[test]
    #[expect(clippy::panic, reason = "driving the panic hook requires a real panic")]
    fn test_aborted_contained_producer_leaves_no_mark_on_the_worker() {
        const PROBE: &str = "tears-panic-probe-aborted";

        let _hook_guard = hook_guard();
        // One worker thread, so the panic below lands on the same thread the
        // aborted producer was polled and dropped on.
        let runtime = Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name(PROBE)
            .build()
            .expect("probe runtime should build");
        let probe = HookProbe::install(PROBE);

        let (parked_tx, parked_rx) = oneshot::channel();
        let producer = runtime.spawn(contained_producer(async move {
            let _ = parked_tx.send(());
            pending::<()>().await;
        }));
        runtime.block_on(async {
            let _ = parked_rx.await;
        });

        producer.abort();
        // Joining the aborted handle waits until the task future has been
        // dropped, so any mark the abort could have leaked would still be set.
        let aborted = runtime.block_on(producer);
        let unrelated = runtime.block_on(runtime.spawn(async {
            panic!("unrelated task panicked after the abort");
        }));
        let counts = probe.counts();

        probe.finish();

        assert!(
            aborted.is_err_and(|error| error.is_cancelled()),
            "the producer should report as cancelled"
        );
        assert!(unrelated.is_err(), "the unrelated task should panic");
        assert_eq!(
            counts,
            (1, 1),
            "an aborted contained producer must not leave its mark on the worker thread"
        );
    }
}
