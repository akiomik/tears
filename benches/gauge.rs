//! Isolates the cost of a producer-gauge change when `tears::runtime::load`
//! has no subscriber, next to two references: the bare `tracing::enabled!`
//! check `LoadObserver::emit`'s fast path gates on, and a plain atomic
//! increment/decrement as a cost floor.
//!
//! This is what justified that fast path (`LoadObserver::emit`,
//! `src/runtime/load.rs`): before it existed, an unsubscribed
//! `track_subscription` + drop still paid the full capture/dispatch machinery
//! (~36ns locally) for no listener; skipping the capture, `seq` bump, and
//! drain-funnel bookkeeping while keeping the counter mutation itself
//! unconditional (needed so counts stay correct once a subscriber does
//! attach) cut that to ~17ns. The gate can't reach the bare-`enabled!` floor
//! (~0.3ns): the counter mutation is never optional, so the gauge mutex is
//! always locked once regardless of where `enabled!` is checked. Re-run this
//! after touching `LoadObserver::emit` to confirm the fast path is still
//! paying for itself.
//!
//! `benches/runtime_load.rs`'s full-loop harness would bury this cost under
//! update/render work, so it is not a substitute for this isolated measurement.
//!
//! Run with `cargo bench --bench gauge --features bench-internals`.

use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering};

use criterion::{Criterion, criterion_group, criterion_main};
use tears::LoadObserver;

fn bench_gauge_step_unsubscribed(c: &mut Criterion) {
    let observer = LoadObserver::default();
    c.bench_function("gauge_step_unsubscribed", |b| {
        b.iter(|| {
            let guard = observer.track_subscription();
            drop(guard);
        });
    });
}

fn bench_enabled_check_only(c: &mut Criterion) {
    c.bench_function("enabled_check_only", |b| {
        b.iter(|| {
            black_box(tracing::enabled!(
                target: "tears::runtime::load",
                tracing::Level::DEBUG
            ));
        });
    });
}

fn bench_atomic_baseline(c: &mut Criterion) {
    let counter = AtomicUsize::new(0);
    c.bench_function("atomic_increment_decrement", |b| {
        b.iter(|| {
            // `Relaxed`, not `SeqCst`: this is a cost floor, and nothing here
            // needs cross-thread ordering, so `SeqCst` would overstate the
            // floor on architectures where the two aren't equally cheap (e.g.
            // ARM, unlike x86).
            counter.fetch_add(1, Ordering::Relaxed);
            counter.fetch_sub(1, Ordering::Relaxed);
        });
    });
}

criterion_group!(
    benches,
    bench_gauge_step_unsubscribed,
    bench_enabled_check_only,
    bench_atomic_baseline
);
criterion_main!(benches);
