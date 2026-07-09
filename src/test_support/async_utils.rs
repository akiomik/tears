use std::collections::VecDeque;
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::oneshot;
use tokio::task::yield_now;
use tokio::time::timeout;

const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_secs(1);

/// Waits until a test-observed condition becomes true.
///
/// This hides the common `timeout + yield_now` polling loop used by tests that
/// observe state updated by spawned tasks.
pub async fn wait_until(mut condition: impl FnMut() -> bool, timeout_message: &str) {
    timeout(DEFAULT_WAIT_TIMEOUT, async {
        while !condition() {
            yield_now().await;
        }
    })
    .await
    .expect(timeout_message);
}

/// Asserts that `future` remains pending until `condition` becomes true.
///
/// Use this when a test needs to prove a gated future has started but must not
/// complete before the gate is released.
pub async fn assert_pending_until<Fut, Condition>(
    future: &mut Pin<&mut Fut>,
    mut condition: Condition,
    completed_message: &str,
    timeout_message: &str,
) where
    Fut: Future,
    Fut::Output: Debug,
    Condition: FnMut() -> bool,
{
    timeout(DEFAULT_WAIT_TIMEOUT, async {
        tokio::select! {
            result = future.as_mut() => panic!("{completed_message}: {result:?}"),
            () = async {
                while !condition() {
                    yield_now().await;
                }
            } => {}
        }
    })
    .await
    .expect(timeout_message);
}

/// Creates one-shot gates for a fixed number of expected fetches.
pub fn gate_fetches(count: usize) -> (FetchGateReleases, FetchGateQueue) {
    let mut releases = Vec::with_capacity(count);
    let mut receivers = VecDeque::with_capacity(count);

    for _ in 0..count {
        let (release, wait) = oneshot::channel();
        releases.push(Some(release));
        receivers.push_back(wait);
    }

    (
        FetchGateReleases { releases },
        FetchGateQueue {
            receivers: Arc::new(Mutex::new(receivers)),
        },
    )
}

/// Release handles returned by [`gate_fetches`].
pub struct FetchGateReleases {
    releases: Vec<Option<oneshot::Sender<()>>>,
}

impl FetchGateReleases {
    /// Releases the zero-based gate at `index`.
    pub fn release(&mut self, index: usize) {
        let release = self
            .releases
            .get_mut(index)
            .and_then(Option::take)
            .expect("fetch gate release should exist");
        assert!(
            release.send(()).is_ok(),
            "fetch gate receiver should still be waiting"
        );
    }
}

/// Receiver queue cloned into fetch closures by [`gate_fetches`].
#[derive(Clone)]
pub struct FetchGateQueue {
    receivers: Arc<Mutex<VecDeque<oneshot::Receiver<()>>>>,
}

impl FetchGateQueue {
    /// Returns the next expected fetch gate.
    pub fn next(&self) -> oneshot::Receiver<()> {
        self.receivers
            .lock()
            .expect("fetch gate mutex should not be poisoned")
            .pop_front()
            .expect("test should provide a gate for each expected fetch")
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future::pending,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };

    use tokio::task::yield_now;
    use tokio::time::timeout;

    // Exercise the re-exports so default-feature clippy does not flag them as unused.
    use crate::test_support::{assert_pending_until, gate_fetches, wait_until};

    #[tokio::test]
    async fn test_wait_until_observes_condition() {
        let ready = Arc::new(AtomicBool::new(false));
        let ready_clone = ready.clone();
        tokio::spawn(async move {
            yield_now().await;
            ready_clone.store(true, Ordering::SeqCst);
        });

        wait_until(
            || ready.load(Ordering::SeqCst),
            "condition should become true",
        )
        .await;
    }

    #[tokio::test]
    async fn test_assert_pending_until_observes_condition() {
        let ready = Arc::new(AtomicBool::new(false));
        let ready_clone = ready.clone();
        tokio::spawn(async move {
            yield_now().await;
            ready_clone.store(true, Ordering::SeqCst);
        });

        let pending = pending::<usize>();
        tokio::pin!(pending);
        assert_pending_until(
            &mut pending,
            || ready.load(Ordering::SeqCst),
            "future should remain pending",
            "condition should become true",
        )
        .await;
    }

    #[tokio::test]
    async fn test_gate_fetches_releases_expected_receivers() {
        let (mut releases, gates) = gate_fetches(2);
        let first = gates.next();
        let second = gates.next();

        releases.release(0);
        timeout(super::DEFAULT_WAIT_TIMEOUT, first)
            .await
            .expect("first gate should complete")
            .expect("first release should be delivered");

        releases.release(1);
        timeout(super::DEFAULT_WAIT_TIMEOUT, second)
            .await
            .expect("second gate should complete")
            .expect("second release should be delivered");
    }
}
