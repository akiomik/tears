# Testing Guidelines

This document describes repository-wide test structure and async test
synchronization rules.

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

Both tests are `#[ignore]`d so `cargo test` stays fast and stable-only by
default. Run them explicitly with:

```sh
cargo test --test api_surface -- --ignored
```

CI runs this as its own step with a nightly toolchain installed, separate
from the main stable test job.

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

The unit-test and integration-test recorders are intentionally duplicated for
now. Keep their structure aligned when changing shared behavior so fixes are
easy to compare across the two copies.

## Process-Global Panic Hook Tests

Use `crate::test_support::PANIC_HOOK_GUARD` for crate-internal tests that call
`std::panic::set_hook`, `std::panic::take_hook`, or deliberately trigger a panic
while a recording hook is installed. The panic hook is process-global and tests
run in parallel by default, so hook-swapping tests can otherwise clobber each
other or observe an unrelated panic.

Hold the guard for the full critical section: install or take the hook, trigger
and catch the panic if needed, restore the previous hook, and then inspect any
recorded hook state. Ordinary tests that do not mutate the panic hook do not
need the guard.

If an integration test ever needs to mutate the panic hook, add a focused local
guard under `tests/common/` rather than exposing the crate-internal test helper
as public API. Because integration tests are async, that local guard should use
`tokio::sync::Mutex` (not `std::sync::Mutex`) so it can be held across
`.await` — see `tests/common/panic_hook.rs`.

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
`timeout`, install a no-op panic hook for the duration (see
`tests/common/panic_hook.rs::SilentPanicHook`) rather than only widening the
timeout. Widening the timeout alone hides the same unbounded cost behind a
larger number instead of removing it.

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
