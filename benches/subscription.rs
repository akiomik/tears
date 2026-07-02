//! Benchmarks for subscription hot paths.
//!
//! These establish a regression baseline for the runtime's subscription work:
//!
//! - **id hashing**: the hash the runtime computes over subscription IDs each
//!   time it decides whether the subscription set changed
//!   ([`Runtime::update_subscriptions`](../src/runtime.rs)).
//! - **reconcile (steady)**: [`SubscriptionManager::update`] when the requested
//!   set is unchanged — the common per-message case, which must diff cheaply and
//!   keep the existing tasks running.
//! - **reconcile (churn)**: `update` when the whole set is replaced — measures
//!   the abort + spawn bookkeeping.
//!
//! Run with `cargo bench --bench subscription`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use futures::stream::{self, StreamExt};
use std::hint::black_box;
use tears::BoxStream;
use tears::subscription::{Subscription, SubscriptionId, SubscriptionManager, SubscriptionSource};

/// A minimal source with a caller-controlled ID whose stream never yields, so
/// its spawned task stays parked (alive) until the manager aborts it.
struct BenchSource {
    id: u64,
}

impl SubscriptionSource for BenchSource {
    type Output = ();

    fn stream(&self) -> BoxStream<'static, ()> {
        stream::pending().boxed()
    }

    fn id(&self) -> SubscriptionId {
        SubscriptionId::of::<Self>(self.id)
    }
}

fn subscriptions(ids: impl IntoIterator<Item = u64>) -> Vec<Subscription<()>> {
    ids.into_iter()
        .map(|id| Subscription::new(BenchSource { id }))
        .collect()
}

fn subscription_ids(count: u64) -> Vec<SubscriptionId> {
    (0..count).map(SubscriptionId::of::<BenchSource>).collect()
}

fn bench_id_hashing(c: &mut Criterion) {
    let mut group = c.benchmark_group("subscription_id_hashing");
    for count in [1u64, 8, 64, 256] {
        let ids = subscription_ids(count);
        group.bench_with_input(BenchmarkId::from_parameter(count), &ids, |b, ids| {
            b.iter(|| {
                let mut hasher = DefaultHasher::new();
                for id in ids {
                    id.hash(&mut hasher);
                }
                black_box(hasher.finish())
            });
        });
    }
    group.finish();
}

fn bench_reconcile_steady(c: &mut Criterion) {
    // `SubscriptionManager::update` spawns tasks, so it must run inside a Tokio
    // runtime context. The tasks park immediately on the pending stream.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .build()
        .expect("bench runtime should build");
    let _guard = rt.enter();

    let mut group = c.benchmark_group("subscription_reconcile_steady");
    for count in [1u64, 8, 64, 256] {
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, &count| {
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            let mut manager = SubscriptionManager::new(tx);
            // Prime the manager so subsequent updates hit the steady-state path
            // (same IDs, tasks already running -> no spawns, just the diff).
            manager.update(subscriptions(0..count));

            b.iter(|| {
                manager.update(subscriptions(0..count));
            });

            manager.shutdown();
        });
    }
    group.finish();
}

fn bench_reconcile_churn(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .build()
        .expect("bench runtime should build");
    let _guard = rt.enter();

    let mut group = c.benchmark_group("subscription_reconcile_churn");
    for count in [1u64, 8, 64, 256] {
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, &count| {
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            let mut manager = SubscriptionManager::new(tx);

            // Alternate between two disjoint ID sets so every update aborts the
            // previous set and spawns a whole new one.
            let mut toggle = false;
            b.iter(|| {
                let ids = if toggle { count..count * 2 } else { 0..count };
                toggle = !toggle;
                manager.update(subscriptions(ids));
            });

            manager.shutdown();
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_id_hashing,
    bench_reconcile_steady,
    bench_reconcile_churn
);
criterion_main!(benches);
