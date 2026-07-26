//! Panic handling helpers for restoring the terminal on unwind.
//!
//! This module is crate-internal; see [`install_panic_hook`](crate::install_panic_hook)
//! (the sole public item re-exported from here) for the full documentation.

use std::panic::{self, PanicHookInfo};

/// The boxed panic hook type used by [`std::panic::set_hook`].
type PanicHook = Box<dyn Fn(&PanicHookInfo<'_>) + Sync + Send + 'static>;

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

/// Composes a panic hook that runs `restore` and then delegates to `next`.
///
/// Extracted from [`install_panic_hook`] so the chaining order can be tested
/// without touching a real terminal.
fn compose_hook(restore: impl Fn() + Sync + Send + 'static, next: PanicHook) -> PanicHook {
    Box::new(move |info| {
        restore();
        next(info);
    })
}

#[cfg(test)]
mod tests {
    use crate::test_support::PANIC_HOOK_GUARD;

    use super::*;

    use std::sync::{Arc, Mutex, PoisonError};

    // A single test, because `set_hook`/`take_hook` mutate process-global state.
    // Running multiple hook-swapping tests in parallel would let them clobber
    // each other's hooks, so both properties are asserted here in one go.
    #[test]
    #[expect(clippy::panic, reason = "driving the panic hook requires a real panic")]
    fn test_compose_hook_restores_then_delegates_once() {
        // Serialize against other tests that touch the global panic hook or
        // panic, so a concurrent panic cannot invoke our recording hook.
        let _hook_guard = PANIC_HOOK_GUARD
            .lock()
            .unwrap_or_else(PoisonError::into_inner);

        // Records the order (and therefore the count) of the two stages.
        // `restore` must run first so the terminal is usable before the
        // delegated reporter prints.
        let order = Arc::new(Mutex::new(Vec::<&'static str>::new()));

        let restore_order = order.clone();
        let next_order = order.clone();
        let next: PanicHook = Box::new(move |_info| {
            next_order
                .lock()
                .expect("order mutex should not be poisoned")
                .push("next");
        });
        let hook = compose_hook(
            move || {
                restore_order
                    .lock()
                    .expect("order mutex should not be poisoned")
                    .push("restore");
            },
            next,
        );

        // `PanicHookInfo` cannot be constructed directly, so drive the composed
        // hook through the real panic runtime and catch the unwind.
        let previous = panic::take_hook();
        panic::set_hook(hook);
        let _ = panic::catch_unwind(|| panic!("trigger"));
        panic::set_hook(previous);

        let recorded = order
            .lock()
            .expect("order mutex should not be poisoned")
            .clone();
        assert_eq!(
            recorded,
            vec!["restore", "next"],
            "restore must run exactly once, before the delegated hook"
        );
    }
}
