//! Scan-cost micro-benchmarks for the kernel's per-dispatch bookkeeping walks.
//!
//! `ScopeRegistry` is a `BTreeMap<RunToken, RunEntry>` and four of its
//! operations are linear walks over it — `keyed_occupant` (every explicit
//! cancel and every keyed spawn), `sub_running` (every declaration at every
//! re-evaluation), `any_stopping_sub` (every re-evaluation), and
//! `select_prefix` (every teardown) — as is `CleanupLedger::take_under`
//! (every teardown). Which structure answers them is mechanism (RFC 0013
//! §3.7), and `ScopeRegistry::select_prefix`'s own doc defers the walk's cost
//! in the number of live runs to the load acceptance RFC 0014 §13.5
//! re-derives. This is that measurement's micro half: the scans in isolation,
//! at 8 / 64 / 512 live runs, so a later index is a change with a number
//! beside it rather than a guess.
//!
//! Both a *hit* and a *miss* are measured for the two lookups. The miss is the
//! exhaustive walk and the hit names the last entry of its kind in token
//! order, so the pair differs only in the terminating `find` — the same walk
//! either way, which is the point.
//!
//! `ScopeRegistry` and `CleanupLedger` are crate-private, so this benchmark
//! goes through `RegistryScan` / `CleanupLedgerScan`, thin bench-only handles
//! gated behind the `bench-internals` feature.
//!
//! Run with `cargo bench --bench kernel_scan --features bench-internals`.

use std::hint::black_box;

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use tears::{CleanupLedgerScan, RegistryScan};
use tokio::runtime::Builder;

/// Live-run counts every scan is measured at.
const SIZES: [usize; 3] = [8, 64, 512];

fn bench_registry_scans(criterion: &mut Criterion) {
    // `RunEntry` holds an `AbortHandle`, which only a real spawn produces, so
    // populating a registry needs a runtime context. The tasks are never
    // polled — the handles are all the entries want.
    let executor = Builder::new_current_thread()
        .build()
        .expect("current-thread executor");
    let _context = executor.enter();

    let mut group = criterion.benchmark_group("registry_scan");
    for size in SIZES {
        let probe = RegistryScan::with_runs(size);
        assert_eq!(
            probe.len(),
            size,
            "the probe holds the runs it was asked for"
        );

        group.bench_with_input(
            BenchmarkId::new("keyed_occupant_miss", size),
            &size,
            |b, _| {
                b.iter(|| black_box(probe.keyed_occupant_miss()));
            },
        );
        group.bench_with_input(
            BenchmarkId::new("keyed_occupant_hit", size),
            &size,
            |b, _| {
                b.iter(|| black_box(probe.keyed_occupant_hit()));
            },
        );
        group.bench_with_input(BenchmarkId::new("sub_running_miss", size), &size, |b, _| {
            b.iter(|| black_box(probe.sub_running_miss()));
        });
        group.bench_with_input(BenchmarkId::new("sub_running_hit", size), &size, |b, _| {
            b.iter(|| black_box(probe.sub_running_hit()));
        });
        group.bench_with_input(BenchmarkId::new("any_stopping_sub", size), &size, |b, _| {
            b.iter(|| black_box(probe.any_stopping_sub()));
        });
        group.bench_with_input(
            BenchmarkId::new("select_prefix_all", size),
            &size,
            |b, _| {
                b.iter(|| black_box(probe.select_prefix_all()));
            },
        );
        group.bench_with_input(
            BenchmarkId::new("select_prefix_none", size),
            &size,
            |b, _| {
                b.iter(|| black_box(probe.select_prefix_none()));
            },
        );
    }
    group.finish();
}

fn bench_cleanup_ledger(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("cleanup_ledger");
    for size in SIZES {
        // Selects nothing, so the ledger survives the call and the walk
        // repeats without a refill: the partition alone.
        let mut probe = CleanupLedgerScan::with_registrations(size);
        assert_eq!(probe.len(), size, "the probe arms what it was asked for");
        group.bench_with_input(BenchmarkId::new("take_under_none", size), &size, |b, _| {
            b.iter(|| black_box(probe.take_under_none()));
        });

        // Selects everything, so each iteration consumes the ledger and needs
        // a fresh one; the arming cost stays in the batched setup.
        group.bench_with_input(BenchmarkId::new("take_under_all", size), &size, |b, _| {
            b.iter_batched_ref(
                || CleanupLedgerScan::with_registrations(size),
                |probe| black_box(probe.take_under_all()),
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, bench_registry_scans, bench_cleanup_ledger);
criterion_main!(benches);
