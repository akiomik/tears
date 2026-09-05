# Testing Guidelines

This document describes repository-wide test structure and async test
synchronization rules, and one obligation that no test can carry.

## Prose Describing Code Is Read Again When That Code Changes

Some of what documentation says about code is checked: a link that no longer
resolves fails the doc build, and an example that no longer compiles fails a
doctest. A sentence is neither. Its truth is not expressed in a form anything
runs, so a change that makes it false leaves nothing behind.

So an edit is not finished until the file's own documentation has been read
again: the module header, the doc on the item, the comment over the line.

## Test Placement

Place crate-internal, white-box tests next to the code they exercise under
`src/**/*.rs`. These tests may use private modules, inspect internal state, and
share helpers from `src/test_support.rs`.

Place black-box integration tests under `tests/`. These tests should exercise
the public API the way downstream users would. Shared integration-test helpers
belong in `tests/common/`; keep them focused so each integration test only
imports what it needs.

When a behavior can be tested either way, prefer the narrowest layer that proves
the contract:

- Use unit tests for pure logic, internal state transitions, and edge cases that
  need private access.
- Use integration tests for end-to-end runtime behavior, public API contracts,
  and interactions between runtime components.

## Why Test Helpers Are Duplicated Instead of Shared

`src/test_support.rs` (crate-internal, white-box) and `tests/common/`
(integration, black-box; see "Test Placement" above) intentionally keep
separate copies of small helpers like `TraceRecorder` and
`with_silent_panic_hook` rather than sharing one implementation. Two
alternatives have been considered and rejected:

- **A workspace-only helper crate.** This made publish/tarball behavior less
  intuitive and added a crate to navigate, for little payoff at the current
  helper size.
- **A `test-support` Cargo feature** exposing `src/test_support` publicly,
  mirroring `bench-internals`/`loom-core`. Rejected because:
  - Cargo has no notion of a private feature: anyone depending on `tears`
    could enable it and rely on internals that are "not covered by semver"
    only by documentation convention. `bench-internals` and `loom-core`
    accept that risk for one bench-only wrapper and one `cfg` swap
    respectively; `test_support` is a wider, still-growing surface
    (recorders, panic-hook guards, async wait helpers, fixtures), so the
    same risk compounds with every future helper.
  - It would blur integration tests from black-box (exercising the public
    API the way downstream users would) into partially white-box, since
    they would import literal crate-internal fixtures through the feature
    door.
  - It adds an invocation mode contributors must remember for every
    integration-test target that needs it: `required-features` wiring (as
    `[[bench]]` already needs for `bench-internals`) and passing
    `--features test-support` locally, instead of today's zero-flag
    `cargo test`.

Duplication is also not always a pure copy: `src/test_support/panic_hook.rs`
uses `std::sync::Mutex` because it only serializes `current_thread` tests,
while `tests/common/panic_hook.rs` uses `tokio::sync::Mutex` because
integration tests hold the guard across `.await` on a multi-threaded runtime.
A shared implementation would need to satisfy both constraints, not just
merge two identical files.

Keep duplicating small helpers for now. Revisit only if a helper's
implementation, not just its call sites, needs to change often enough that
keeping copies in sync becomes the bottleneck.

## Public API Surface Tests

`tests/api_surface.rs` is a different category from the rest of `tests/`:
it checks the *shape* of the crate's public API (which paths are reachable)
per `docs/api-guidelines.md`, not runtime behavior. A downstream user never
observes this directly — it only matters to whoever is deciding where a new
item should live.

This means it doesn't fit either bucket in "Test Placement" above (it needs
no private access, but it isn't exercising behavior either), and it has
different mechanics from every other test in this repository:

- It parses rustdoc JSON via the `public-api` crate, which is only available
  on the nightly toolchain — unlike everything else in this repo, which
  builds under the pinned stable toolchain in `rust-toolchain.toml`.
- Each test shells out to `cargo +nightly rustdoc`, so it needs a nightly
  toolchain installed (`rustup toolchain install nightly`) and takes several
  seconds per run.
- It builds the surface with the features a dependant can enable, read at run
  time from `just --evaluate user_features`, so `just` also has to be on PATH
  (`cargo install just`). It does not use `--all-features`: that enables
  `loom-core`, which removes code rather than adding it. As the code stands
  the two sets produce the same surface, so nothing is lost either way; but an
  item compiled out is one these rules never examine, so the check would pass
  by looking at less.

Both tests are `#[ignore]`d so `cargo test` stays fast and stable-only by
default. Run them explicitly with:

```sh
cargo test --test api_surface -- --ignored
```

CI runs this in its own job, `Public API Surface`, with a nightly toolchain
installed, separate from the test matrix.

## Do Not Use Sleep For Synchronization

Tests should not use `tokio::time::sleep(Duration::from_millis(...))` to wait
for another task, subscription, stream, or command to reach a state. Sleeping
makes the test depend on scheduler timing and CI load, so the test can become
both flaky and unnecessarily slow.

Prefer explicit synchronization:

- `oneshot` for a single gate or milestone.
- `watch` for state that changes over time.
- `Notify` for repeated readiness events where no value needs to be carried.
- `crate::test_support::wait_until` for bounded condition waits in
  crate-internal tests.
- `crate::test_support::assert_pending_until` for futures that must stay
  pending until a gated condition is observed.
- `crate::test_support::gate_fetches` for query tests that need deterministic
  in-flight fetch windows.
- `timeout` only as a failure bound around an explicit condition, not as the
  condition itself.

Real time is still valid when time is the behavior under test. Timer accuracy
tests, delayed command behavior, and paused-time event-loop tests may use
`sleep` or `timeout` when the assertion is about elapsed time, virtual time, or
timer ordering.

## Tracing Assertions

Use `TraceRecorder` when a test asserts `tracing` output. Crate-internal unit
tests should import `crate::test_support::TraceRecorder`; integration tests
that need tracing assertions should import the local helper from
`tests/common/trace_recorder.rs` with an explicit `#[path]` module:

```rust
#[path = "common/trace_recorder.rs"]
mod trace_recorder;

use trace_recorder::TraceRecorder;
```

`tests/common/mod.rs` does not re-export this helper. Importing it explicitly
keeps unused tracing code out of integration-test targets that do not assert
tracing output.

Read a gauge's *current value* through `current_u64`: the gauge contract
orders events by `seq`, not by arrival. The accessor assumes one runtime
instance per recorder and panics otherwise; a deliberately multi-instance
test reads `current_u64_by_runtime`, which applies the same greatest-`seq`
rule inside each `runtime_id`'s own events and which `current_u64` is the
one-instance convenience over. The rule covers the gauge snapshot's fields —
the ones whose events carry a `seq`. Other unsigned-integer fields on the
same target (`pulled`, `updated`, `shared_pending`, `wait_us`) are per-event
readings with no current value to take; `current_u64` returns `None` for
them, so keep those on `u64_values`.

`u64_values` returns `Readings`, not a `Vec<u64>`, and that is what keeps the
arrival-order read out of a current-value assertion: the type publishes no
positional read — no first, no last, no index, no slice, no iterator — so
`u64_values(...).last()` does not compile. What stays open are the reads that
ask what the log contains (`len`, `is_empty`, `contains`, `contains_nonzero`,
`only`, `all_unique`, `all_equal`), and a row that needs one of those asks for
it by name rather than reaching into the sequence: `tests/observability.rs`
takes `all_equal` for one instance's shared `runtime_id` and `all_unique` for
its distinct `seq` values, and `src/runtime/channel.rs` takes `only` for the
lone `wait_us` reading it has just established.

A row whose claim really is about arrival order names `arrival_order()` — or
`into_arrival_order()` where it sorts the readings, indexes them after a move,
or keeps them past the borrow. Two shapes qualify: an exact emission sequence
(`src/runtime/load.rs` pinning `[3, 2, 1, 0]` as every value a subscriber saw)
and a positional read into the log (the same file's `ids[0]` against `ids[2]`,
or `src/kernel/conformance/quit.rs`'s suffix after a recorded offset).

One grep for `arrival_order` enumerates the rows that take that dependency
through `Readings`. It is not the whole list, and a divergence between dispatch
order and `seq` order has to re-examine the rest as well: a row reading the same
log through `u64_event_values` depends on arrival order while naming nothing.
`src/runtime/load.rs`'s row that `seq` strictly increases inside one
`runtime_id` builds its partitions by iterating the rows as they arrived, and
`src/kernel/producer.rs`'s gauge trace compares exact arrival-order sequences.

Two reads that look positional but are not: "this gauge never fired at all" is
`is_empty()` on the log — `tests/lifecycle.rs`'s complement census, whose own
comment says why it reads the log rather than the current value — and "the
gauge rose while the producers ran" is `contains_nonzero()`, which the INV-LC7
census takes. Neither needs the opt-out.

The refusal is a rule about the type's surface, enforced by the compiler at
every call site that goes through `u64_values`. Two paths stay outside it, and
review is what holds them. An edit that added `last()` to `Readings` would pass
the whole run. And `u64_event_values` (below) hands back plain `Vec<[u64; N]>`
rows, where `.last()` still compiles — with a single name that accessor is
`u64_values` narrowed to each event's first occurrence, so a current-value read
written that way is the arrival-order read again, on a gauge field. Take the
current value from `current_u64`, and keep `u64_event_values` for what it is
for: reading several names off one event together.

Read several fields of the *same* event through `u64_event_values([...])`
rather than by zipping as many `u64_values` logs: the per-field logs are
flattened across events, so zipping them pairs values by position and
truncates to the shortest input without reporting the mismatch — a field
that stops riding every event then shifts the tuples or shortens the vector
instead of failing. `u64_event_values` keeps the per-event grouping and
refuses an event that records only some of the names.

The INV-LC7 producer-gauge census in `tests/common/gauges.rs` is included
with the same `#[path]` mechanism as the recorder above; see its module
docs for the include prerequisite.

Prefer filtering the recorder to the event being asserted, for example with
`.with_target("tears::runtime")` and `.with_level(tracing::Level::DEBUG)`.
Avoid ad hoc subscribers that filter by returning `false` from `enabled()`:
`tracing` caches callsite interest process-wide, so one test can accidentally
make a callsite invisible to another parallel test. `TraceRecorder` keeps
interest open and filters in `event()` instead.

`TraceRecorder` also keeps a process-local no-op interest keeper alive once a
tracing assertion is installed. This prevents callsites first reached by other
parallel test threads from caching `Interest::never` while the asserting test's
thread-local recorder is active.

The unit-test and integration-test recorders are intentionally duplicated (see
"Why Test Helpers Are Duplicated Instead of Shared" above). Keep their
structure aligned when changing shared behavior so fixes are easy to compare
across the two copies.

## Process-Global Panic Hook Tests

Use `crate::test_support::hook_guard()` for crate-internal tests that directly
call `std::panic::set_hook` or `std::panic::take_hook`. The panic hook is
process-global and tests run in parallel by default, so hook-swapping tests can
otherwise clobber each other or observe an unrelated panic. The helper locks the
process-global `PANIC_HOOK_GUARD` and recovers from poisoning in one place —
poisoning is ordinary here, since the tests it serializes panic on purpose, and
recovering keeps one such panic from failing every later hook test with a poison
error instead of its own assertion.

Hold the guard for the full critical section: install or take the hook, trigger
and catch the panic if needed, restore the previous hook, and then inspect any
recorded hook state. Ordinary tests that do not mutate the panic hook do not
need the guard.

Crate-internal timing-bound tests that intentionally panic should run the
relevant future through `crate::test_support::with_silent_panic_hook`. The helper
owns the guard, catches an unexpected panic long enough to restore the previous
hook, and only then resumes unwinding. Its guard uses `std::sync::Mutex`, so the
helper is restricted to `current_thread` tests.

Tests that assert on how the composed hook *classified* a panic (terminal
restore skipped or taken) use `crate::test_support::HookProbe` instead: it
installs a counting hook built from the real `compose_hook`, serves
multi-thread runtimes and non-async tests, and filters its counts by worker
thread name so a concurrent unrelated panic cannot move them. Callers hold the
guard themselves — including across `block_on` in non-async tests. It is
lib-only: it needs the crate-private `compose_hook`, so the integration copy
under `tests/common/panic_hook.rs` deliberately has no equivalent.

**A recording hook filters by thread name. That is the primary defence, and it
is the recording test's own obligation.** The guard serializes hook swaps, not
the rest of the binary: any test in the process may panic while a recording hook
is installed, and there is no way to enumerate — let alone guard — every test
that panics on purpose, since `#[should_panic]` tests panic by construction. So
a test that records hook activity records only panics raised on its own threads,
through `crate::test_support::on_thread(prefix)` — the check `HookProbe` applies
to its counts, and the one the hand-rolled recording hooks in `src/panic.rs` and
`src/test_support/panic_hook.rs` apply for the same reason. A test whose panic
is raised inline passes its own full path, since libtest names the thread after
it — or, where that does not fit on a line, a leading part of the path that no
other test's name shares; a test that drives its own runtime passes the prefix
it stamped on the workers. A recording hook without that filter is the defect:
it asserts over the whole process while claiming to measure one test.

**A loom model takes the guard because it is a hook swapper**, which is the
first rule of this section rather than a category of its own. Loom drives each
model thread as a `generator` coroutine, and generator manages the
process-global hook around its own unwinds: tearing a coroutine down, it calls
`take_hook`, installs a no-op so the internal unwind prints nothing, and
reinstalls the previous hook afterwards (`generator`'s `gen_impl.rs`). A test
running concurrently with that sequence is running concurrently with a hook
swap it did not make — exactly what the guard serializes. That the swap is
performed by a dependency rather than by the test's own code changes nothing
about the hazard. Robustness against future loom or generator versions is a
secondary reason, and the cost is four models serializing against the
hook-holding tests, which is nothing.

The guard is not, on the locked versions, holding back a stream of hook calls.
Measured on loom 0.7.2 and generator 0.8.9, built both with and without
`--cfg loom`: a succeeding model explored 27 interleavings and reached an
installed hook **zero** times, with the counting hook unfiltered (it would have
counted a panic on any thread) and its liveness proven in the same run by a
deliberate panic that it did count. Only a failing assertion inside a model
reaches the hook, once, when the build is already red.

The rule has an observed origin worth recording as it happened. During the kernel
spike the hook-restoration test began failing intermittently under the parallel
full-suite run — three hook calls counted where one was expected — and
instrumenting the recording hook with thread names attributed the extra calls to
panics raised on other tests' threads: deliberate panics in tests that were not
holding the guard, and the loom models running in the same binary. That
diagnosis named generator's completion path as the primary cause, and it is a
real path — `done()` raises `panic_any` in generator's `yield_.rs`, and loom's
scheduler ends a coroutine body with it. Serializing both against the guard
closed the flake, with twelve consecutive green runs of the full lib suite
where the same conditions had failed frequently before. The re-measurement
above does not reproduce that path reaching an installed hook, and the two
observations are left as they are rather than reconciled by
argument: whether a generator unwind reaches the hook you installed depends on
how generator is managing the hook at that moment, which is the same take/set/
restore sequence the paragraph above makes the guard's reason. The fix at the
time treated the hazard from the swapper side; the filter treats it from the
recorder side, which is the side that scales.

Integration tests cannot use crate-private test support, so they use the focused
local `with_silent_panic_hook` under `tests/common/panic_hook.rs`. Its guard uses
`tokio::sync::Mutex` so it can be held across `.await` without blocking a runtime
worker thread. See "Why Test Helpers Are Duplicated Instead of Shared" above for
why this is a separate implementation rather than a shared one.

Do not expose or directly hold the internal silent-hook RAII guard. Calling
`std::panic::set_hook` from a panicking thread itself panics, so attempting to
restore a hook from `Drop` during unwinding can abort the entire test process.
The scoped helpers prevent that failure mode structurally. Keep ordinary
timeout failures as `Result` values where practical, leave the silent scope,
and perform assertions only after the previous hook has been restored.

### Panic Hook Cost In Timing-Bound Tests

A test that intentionally panics inside a spawned command (to assert
`catch_unwind` logging, for example) still pays for the *default* panic hook:
before `catch_unwind` regains control, the hook runs synchronously on the
panicking thread and, under `RUST_BACKTRACE=1` (set repo-wide in CI),
captures and symbolicates a full backtrace. On Windows that symbolication is
markedly slower than on Linux/macOS and its duration is load-dependent, which
makes it a source of flakiness rather than a fixed cost.

This matters most when the panic happens on a `current_thread` runtime: the
same thread that is blocked doing symbolication is also the thread driving
any timer or subscription the test is relying on to reach its next state, so
the hook cost eats directly into the test's `timeout` budget. A test that
otherwise finishes in single-digit milliseconds can intermittently blow
through a one-second timeout on a loaded Windows runner.

If a test intentionally triggers a command-task panic under a bounded
`timeout`, use the appropriate `with_silent_panic_hook` scope. Widening the
timeout alone hides the same unbounded cost behind a larger number instead of
removing it.

## Sleep Audit Notes

Keep this document focused on durable policy rather than a complete inventory of
individual tests. Exact cleanup history belongs in PRs and git log, where it can
age with the code that changed.

When auditing sleeps, classify each use into one of these buckets:

- **Remove immediately** when the awaited work is already synchronized, such as
  synchronous channel sends that can be drained directly.
- **Replace with explicit observation** when a spawned task is expected to send
  a message, emit a quit signal, drop a guard, or update a counter.
- **Keep intentionally** when elapsed time, virtual time, timer ordering, or
  command delay is the behavior under test.
- **Defer to async helper work** when several tests need the same wait pattern;
  move those cases to `crate::test_support` helpers instead of open-coding
  loops.
