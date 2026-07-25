# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- Short-interval `Timer` subscriptions no longer replay a catch-up burst of
  missed ticks after a delay: however many interval boundaries elapse while a
  tick goes untaken, exactly one tick becomes deliverable, and the cadence
  resumes on the timer's original phase (first anchor-phase boundary strictly
  after the late tick). The timer's anchor is fixed when its stream is built;
  the first tick still arrives one full interval later (RFC 0009 §4.2)

### Added

- `tears::testing::TestStore`, a deterministic, executor-free test harness
  that drives an `Application`'s `update` transitions and immediately ready
  effects synchronously (RFC 0008, stage 1)
  - `send` applies one message through `update`; `receive`, `receive_matching`,
    and `receive_quit` assert the next deliverable effect output — under a
    canonical per-leaf delivery order that is the store's own contract, not a
    runtime ordering guarantee — and apply it; `state`, `redraw_requested`,
    and `subscription_ids` observe the application state, the per-step redraw
    directive (RFC 0002), and the declared subscription set — deduplicated
    first-occurrence-stable — without starting any source
  - Assertions are exhaustive: deliverable output never received or an effect
    stream never driven to completion fails the test at `receive`, `finish`,
    or drop; output remaining after an observed quit is legally discarded,
    mirroring the runtime's shutdown contract
  - RFC 0003 cancellation semantics apply to the store's pending output:
    same-id supersede, keep-in-flight discard and release, explicit cancel,
    and quit suppression
  - `Application::Message`'s bounds stay exactly `Send + 'static`: the store
    carries its own bounds (`Debug` on the store, `PartialEq` only on
    `receive`) and requires `Clone` nowhere
  - Stage-1 scope: the store is executor-free — `TestStore::new` panics
    inside an entered Tokio runtime, so tests run on a plain `#[test]` —
    time-dependent command effects (`Command::timeout`, retry backoff) wait
    on the Clock DI RFC (RFC 0009), and subscription sources are never
    executed

## [0.10.1] - 2026-07-24

### Added

- `RuntimeConfig` and `Runtime::with_config` add opt-in runtime load control
  (RFC 0006, RFC 0007): a bounded delivery mode for the shared
  application-message channel (`app_channel_capacity`) and each keyed command's
  private channel (`keyed_channel_capacity`), plus a micro-batch count cap
  (`batch_max_messages`), carried alongside the frame rate
  - Bounded channels apply backpressure — a producer waits for capacity rather
    than dropping — so memory and shared-input latency stay bounded under
    overload; the dedicated quit channel is never bounded
  - Load observability is emitted through `tracing` under the
    `tears::runtime::load` target: a per-micro-batch event
    (`pulled`/`updated`/`shared_pending`), a bounded-mode capacity-wait event
    (`channel`/`wait_us`), and producer-count gauges (`subscriptions`,
    `unkeyed_commands`, `keyed_commands`, `blocked`); each gauge event also
    carries a per-runtime monotone `seq` — not a gauge but an ordering counter
    — that fixes the current gauge value as the greatest-`seq` event's, so
    consumers order gauge events by `seq`, not by arrival
  - Additive and non-breaking: `Runtime::new` is unchanged, and a
    load-control-unset `RuntimeConfig` reproduces the previous unbounded
    delivery path exactly
- `Subscription::scoped` and `Command::scoped` qualify a subscription's or
  command's lifecycle identity with one structural scope segment, so composed
  child instances that reuse the same local source/key or `CommandId` no
  longer alias each other's lifecycle (RFC 0005 Phase B)
  - `Command::scoped` qualifies the keyed spawn id (if `cancellable`/
    `cancellable_with` was already called) and every explicit cancel id
    already present at the call boundary; it does not retroactively cover
    ids attached by a later modifier call — see the ordering examples on
    `Command::cancellable`'s rustdoc
  - Scoping is additive: unscoped subscriptions and commands keep their
    existing 0.10.0 behavior unchanged

### Changed

- The per-micro-batch trace event moved off the `tears::runtime` target: the
  old `tears::runtime` event with the `messages` field is removed, replaced by
  the richer `tears::runtime::load` batch event (`pulled`/`updated`/
  `shared_pending`). A consumer filtering `tears::runtime` on `messages` must
  switch to the `tears::runtime::load` target

## [0.10.0] - 2026-07-17

### Changed

- **Breaking:** Subscription identity is now structural and collision-safe
  - `SubscriptionSource::id() -> SubscriptionId` is replaced by an associated
    `type Key` and `key() -> Self::Key`; return the owned logical key instead
    of a precomputed hash digest
  - `Subscription::new(source)` now combines that key with the concrete source
    type, and `SubscriptionId::of::<T>(u64)` is removed
  - `SubscriptionId` is `Clone` but no longer `Copy`; it remains `Send`,
    `Sync`, `UnwindSafe`, and `RefUnwindSafe`
  - Built-in subscription source types no longer implement `Hash`: `Timer`,
    `TerminalEvents`, Unix `Signal`, Windows `CtrlC` / `CtrlBreak`, and
    feature-gated `WebSocket` / `Query`; hash or store their structural key
    returned by `SubscriptionSource::key()` when lifecycle identity is needed
  - Duplicate desired subscriptions still keep the first declaration and now
    emit a warning under the `tears::subscription` tracing target

  Before:

  ```rust
  impl SubscriptionSource for WatchSource {
      type Output = WatchEvent;

      fn id(&self) -> SubscriptionId {
          let mut hasher = DefaultHasher::new();
          self.path.hash(&mut hasher);
          SubscriptionId::of::<Self>(hasher.finish())
      }

      // ...
  }
  ```

  After, return the original logical key and let `Subscription::new` construct
  the ID:

  ```rust
  impl SubscriptionSource for WatchSource {
      type Output = WatchEvent;
      type Key = PathBuf;

      fn key(&self) -> Self::Key {
          self.path.clone()
      }

      // ...
  }
  ```
- **Breaking:** Raised MSRV to Rust 1.88.0 (from 1.86.0)
  - Required to pull in `time >=0.3.47`, which resolves RUSTSEC-2026-0009
    (denial of service via stack exhaustion) in the `time` crate pulled in
    transitively via `ratatui-widgets`

### Removed

- **Breaking:** `Action` is no longer part of the public API
  - `tears::Action` and `tears::prelude::Action` are gone; `Command::effect`
    (which took an `Action`) is removed
  - Replace `Command::effect(Action::Quit)` with the new `Command::quit()`
  - Replace `Command::effect(Action::Message(msg))` with `Command::message(msg)`
    (already the preferred form)
  - `Action` remains as a private runtime-internal stream item, so no runtime
    behavior changes
- **Breaking:** Removed the fallible `try_new` constructors in favor of a
  single `new` per type
  - `Timer::try_new(u64) -> Option<Timer>` is gone; use
    `Timer::new(NonZeroU64::new(ms).expect(...))`
  - `RetryPolicy::try_new(usize) -> Option<RetryPolicy>` is gone; use
    `RetryPolicy::new(NonZeroUsize::new(n).expect(...))`
  - `FrameRate::try_new(u32) -> Result<FrameRate, FrameRateError>` is gone;
    use `FrameRate::new(NonZeroU32::new(fps).expect(...))`
  - `Runtime::try_new(flags, u32) -> Result<Runtime<_>, FrameRateError>` is
    gone; build a `FrameRate` first and pass it to `Runtime::new(flags,
    frame_rate)`
  - `FrameRateError::Zero` is removed: the non-zero invariant is now enforced
    by `NonZeroU32` at the call site instead of at runtime
  - `FrameRateError` is now `#[non_exhaustive]`
- **Breaking:** `SubscriptionManager` is no longer part of the public API
  - `tears::subscription::SubscriptionManager` is gone; it was already
    documented as runtime-internal and used only by `Runtime` and
    crate-internal tests
  - No behavior change for applications; the runtime continues to manage
    subscriptions the same way
- **Breaking:** `FrameRateError` is no longer re-exported from
  `tears::prelude`
  - `use tears::prelude::*;` no longer brings `FrameRateError` into scope;
    import it explicitly with `use tears::FrameRateError;` if needed
  - `tears::FrameRateError` (the crate-root path) is unaffected
- **Breaking:** `subscription::http::mutation` and `subscription::http::query`
  are no longer public modules (requires `http` feature)
  - `tears::subscription::http::mutation::Mutation` and
    `tears::subscription::http::query::Query` (and their sibling types) are
    gone; import them from `tears::subscription::http` instead, e.g.
    `use tears::subscription::http::Mutation;`
  - `config` / `key` / `result` already followed this single-path pattern;
    `mutation` / `query` were the only `http` submodules with a second,
    deeper public path to the same types
- **Breaking:** Closed the double public paths for `Application`, `Command`,
  `Runtime`, `Subscription`, `SubscriptionId`, `SubscriptionSource`, and
  `install_panic_hook`
  - `tears::application::Application`, `tears::command::Command`,
    `tears::runtime::Runtime`, `tears::subscription::Subscription`,
    `tears::subscription::SubscriptionId`, `tears::subscription::SubscriptionSource`,
    and `tears::panic::install_panic_hook` are gone
  - Each item's crate-root path is unaffected and is now the sole public
    path: `tears::Application`, `tears::Command`, `tears::Runtime`,
    `tears::Subscription`, `tears::SubscriptionId`, `tears::SubscriptionSource`,
    `tears::install_panic_hook`
  - `tears::command::{RetryError, RetryPolicy, ...}` and
    `tears::subscription::{http, mock, signal, terminal, time, websocket}`
    are unaffected; only the redundant second path to the skeleton types
    above is removed

### Added

- Opt-in command cancellation keyed by `command::CommandId`, including
  `Command::cancellable`, `Command::cancellable_with`, `Command::cancel`, and
  the `command::CancelPolicy` same-id behavior
- `Command::quit()` for requesting the application to quit, replacing the
  public use of `Command::effect(Action::Quit)`

## [0.9.3] - 2026-07-11

### Fixed

- Updated locked transitive dependencies to address RustSec advisories for
  `aws-lc-sys` and `crossbeam-epoch`, plus audit warnings for `anyhow` and
  `rand`.

### Added

- `Command::timeout()` for applying an overall deadline to each effect leaf,
  with at most one timeout message emitted per modifier call
- `Command::retry()` and `Command::retry_if()` for repeatable fallible
  operations, with non-zero attempt policies, optional fixed backoff, retry
  context, and structured terminal errors
- Subscription start tracing events now include a `restarted` field so initial
  starts and restarts can be distinguished.

## [0.9.2] - 2026-07-09

### Fixed

- Finished subscriptions are now restarted on every re-evaluation while still
  requested
  - The runtime cached a hash of the subscription IDs and skipped
    `SubscriptionManager::update` whenever the ID set was unchanged, which
    suppressed the manager's documented restart of subscriptions whose tasks had
    finished (e.g. a finite source completing, or a WebSocket disconnecting)
  - **Behavior change:** while a subscription ID keeps appearing in
    `subscriptions()`, a finished stream is restarted after the next message. To
    stop a source once it finishes, drop it from the returned set; keeping a
    finished WebSocket subscription in the set is now an implicit reconnect

### Added

- `Command::without_redraw()` for updates that handled a message without
  changing the visible view
  - The default behavior is unchanged: commands still request redraws unless
    explicitly opted out
  - Redraw suppression does not suppress subscription re-evaluation

### Changed

- Consolidated quit detection into the event loop's dedicated quit branch
  - `process_frame_tick` no longer polls the quit channel; the loop's
    `select!` already handles `Action::Quit` in a single always-on branch
  - Internal cleanup with no change in observable behavior

## [0.9.1] - 2026-07-03

### Added

- `install_panic_hook()` for restoring the terminal on panic
  - Wraps the current panic hook so the terminal leaves raw mode and the
    alternate screen *before* the previously installed hook (e.g. `color_eyre`)
    prints its report
  - Call it once during startup, after the reporter is installed and the
    terminal is initialized
  - See the new `panic_hook` example for a demonstration
- `tracing` instrumentation for the runtime hot paths
  - The runtime emits events for message batches, subscription updates, command
    spawns, renders, and shutdown under the `tears::runtime` and
    `tears::subscription` targets
  - WebSocket subscriptions now emit connection, disconnection, read/write
    error, and message-type/size events under
    `tears::subscription::websocket`
  - HTTP query subscriptions now emit invalidation, cell reuse, fetch
    lifecycle, and cache GC events under `tears::subscription::http`
  - Terminal, timer, and signal subscriptions now emit source-specific events
    under `tears::subscription::terminal`, `tears::subscription::time`, and
    `tears::subscription::signal`
  - Subscription instrumentation avoids logging payloads, query key values,
    pasted text, key presses, and error strings that may contain sensitive data
  - Events are inert unless a `tracing` subscriber is installed
  - Panics inside command and subscription tasks are now caught and logged at
    the `error` level instead of being silently lost

### Changed

- The runtime no longer wakes at the frame rate while idle
  - The event loop skips its frame tick when there is no pending redraw or
    subscription update, so an idle application consumes no CPU at the frame
    rate instead of waking (e.g. 60 times per second) only to do nothing
  - Rendering latency is unaffected: when a message re-enables the frame tick,
    the timer deadline has already elapsed, so the tick is ready on the next poll
    (`MissedTickBehavior::Skip` only controls where the following tick lands,
    avoiding a catch-up burst)
  - Each frame tick now emits a `tracing` event under the new
    `tears::runtime::frame` target

### Fixed

- Subscription tasks are now aborted when the `SubscriptionManager` is dropped
  - Previously, dropping the manager without calling `shutdown()` (for example
    while unwinding from a panic) detached the running tasks instead of
    cancelling them, leaking any parked subscription tasks
- Command tasks are now tracked and aborted on shutdown, and when the runtime is
  dropped (for example while unwinding from a panic)
  - Previously, `enqueue_command` discarded the task handle, so command tasks
    could not be cancelled and were detached

## [0.9.0] - 2026-07-03

### Changed

- **BREAKING**: Renamed `subscription::time::Message` to
  `subscription::time::TimerEvent`
  - This avoids colliding with the application-level `Message` type commonly
    used in TEA applications
  - `Timer` subscriptions now emit `TimerEvent::Tick`

- **BREAKING**: `Timer::new` now takes `NonZeroU64` for the interval in
  milliseconds
  - This prevents zero-duration timers from being constructed and later
    panicking when the stream is started
  - Added `Timer::try_new(u64) -> Option<Timer>` as a convenient constructor for
    millisecond values that may be zero

- **BREAKING**: `Runtime::new` now takes a validated `FrameRate` instead of a
  raw `u32`
  - This prevents `frame_rate == 0` from panicking due to division by zero
  - Extremely high frame rates that would round down to a zero-duration Tokio
    interval are rejected
  - Added `FrameRate::try_new(u32) -> Result<FrameRate, FrameRateError>` and
    `Runtime::try_new(flags, u32) -> Result<Runtime<_>, FrameRateError>` as
    convenient constructors for raw FPS values

- **BREAKING**: `QueryState` is removed in favor of the richer `QueryResult`
  (requires `http` feature)
  - `QueryResult` no longer exposes a `state` field; read the query state via
    accessors (`is_loading()`, `is_success()`, `is_error()`, `data()`,
    `error()`, `status()`, `fetch_status()`, `is_stale()`)
  - Added `QueryStatus` and `FetchStatus`, exposed via `status()` and
    `fetch_status()`

- **BREAKING**: `QueryClient::invalidate` no longer returns a `Command` and is
  applied synchronously (requires `http` feature)
  - Call it as a statement and return `Command::none()` instead of
    `return self.client.invalidate(key);`
  - The key argument is now `impl Into<QueryKey>`; string literals still work,
    but `&"key"` and non-string keys need updating

- **BREAKING**: A failed fetch now retains the previously successful data
  (requires `http` feature)
  - The cell becomes `data = Some(previous), status = Error`; read `data()` and
    `is_error()` independently. Previously the error state dropped the data

- **BREAKING**: HTTP query retention now uses inactive cells instead of legacy
  cache entries
  - `cache_time` is measured from when the last subscription becomes inactive,
    not from the original fetch timestamp
  - This keeps active query data available regardless of age while still
    collecting inactive query data after the configured retention window

- Replacing a `Query` fetcher while keeping the same key is not supported
  (requires `http` feature)
  - The runtime keys the running subscription by its identity (`QueryClient`,
    key, and value type), so a new `Query::new(key, new_fetcher, client)` with
    an unchanged key keeps the existing subscription and the old fetcher; the
    new fetcher never takes effect
  - To change the request, change the key (for example by including the varying
    parameter in it)

- Added a structured `QueryKey` (with `QueryKey` and `QueryKeyPart`) providing
  `From<&str>`, `From<String>`, and tuple conversions; `Query::new` and
  `QueryClient::invalidate` now accept `impl Into<QueryKey>` (requires `http`
  feature)

## [0.8.3] - 2026-07-02

### Fixed

- Fixed a `stale_time`/`cache_time` of `Duration::ZERO` not being treated as
  immediately stale/expired at the exact boundary (requires `http` feature)
  - Cache staleness and garbage collection used a strict `>` comparison against
    the elapsed time, so an entry was only stale/expired once elapsed time
    *exceeded* the configured duration; this contradicted the documented
    "immediately stale" semantics of the default `stale_time` of 0
  - Both comparisons now use `>=`, so a duration of `Duration::ZERO` marks the
    entry stale/expired immediately. In practice this only affects the exact
    boundary; wall-clock elapsed time is virtually never exactly equal to the
    configured duration, and these cache internals are not part of the public API

- Fixed `Query` subscriptions with the same key on different `QueryClient`
  instances being treated as identical, so switching the client did not restart
  the subscription (requires `http` feature)
  - `Query::id()` hashed only the string key, so the runtime's subscription diff
    deduplicated queries that shared a key even when they used a different
    `QueryClient`; the old client's cache and invalidation channel kept being
    used after the app switched clients
  - `QueryClient` now carries a process-unique `client_id` that is included in
    the query's hash. `#[derive(Clone)]` preserves the id, so cloned clients
    (which share the same cache and broadcast channel) still compare equal and
    are not needlessly restarted
  - Note: a fetcher that captures request parameters (user ID, page, base URL,
    ...) without reflecting them in the key still cannot be distinguished by the
    hash. The `Query` docs now state that every request parameter must be
    included in the key

- Fixed `QueryClient::invalidate` not marking cached entries stale, so
  invalidations were lost when no query subscription was active (requires
  `http` feature)
  - Previously `invalidate` only broadcast a notification; a `broadcast` channel
    only reaches current subscribers, so if no query was subscribed at that
    moment the cache stayed fresh and a later subscription within `stale_time`
    would serve the outdated data as fresh (mutation results appearing to "revert")
  - This was masked by the default `stale_time` of 0 (entries are treated as
    stale on next access anyway) and only surfaced with a configured
    `stale_time > 0`
  - `invalidate` now marks every typed cache entry sharing the invalidated key
    as stale via a new type-erased `AnyCacheEntry::mark_stale`, so a query that
    subscribes afterwards sees the entry as stale and refetches, in addition to
    the existing broadcast that refetches already-active queries

## [0.8.2] - 2026-07-02

### Fixed

- Fixed new subscriptions being started in hash iteration order instead of the
  order returned by `Application::subscriptions`
  - `SubscriptionManager::update()` previously diffed subscription IDs with
    `HashSet::difference`, so stream creation could happen in a nondeterministic
    order for newly added subscriptions
  - New subscriptions now start in input order while existing subscriptions
    continue running unchanged

- Fixed WebSocket TCP connections not being released during runtime shutdown
  when `SubscriptionManager::shutdown()` aborts the subscription task via
  `handle.abort()` (requires `ws` feature)
  - Previously, `stream()` spawned a background `run_subscription_loop` task
    that held the WebSocket TCP handles; when the outer consumer task was
    aborted the background task needed to be scheduled to react, but during
    Tokio runtime teardown it might never get that chance and the TCP
    connection was left open
  - The WebSocket connection loop now runs entirely inside the `stream::unfold`
    future (no separate `tokio::spawn`); aborting the outer task drops the
    TCP handles synchronously, so the OS-level connection is released
    regardless of whether the runtime has already started tearing down
  - **Note**: a WebSocket-level `Message::Close` frame is still not sent on
    task abort — Rust has no async `Drop`.  For a clean WebSocket close,
    send `WebSocketCommand::Close` before the runtime shuts down.  A future
    improvement could add a cooperative shutdown path via a `CancellationToken`
    combined with a grace period in `SubscriptionManager::shutdown()`

- Fixed `CacheEntry::check_staleness` having a misleading `&mut self` signature
  (requires `http` feature)
  - The method set `self.is_stale = true` on the receiver, but `QueryClient::get_cache`
    returns a clone of the entry, so the mutation was silently discarded and the
    actual cached entry's `is_stale` field remained `false` forever
  - The method now takes `&self`; staleness is evaluated as
    `self.is_stale || stale_time_elapsed`, preserving the existing return-value
    semantics while making the immutability explicit

- Fixed `Query` subscriptions with the same string key but different value types
  overwriting each other's cache entries (requires `http` feature)
  - The cache was keyed by the plain string key, so `Query<i32>` and `Query<String>`
    using the same key would each overwrite the other's entry; every `downcast_ref`
    to the "wrong" type would fail and cause an unnecessary refetch
  - The cache is now keyed by `(TypeId, string key)` so each value type has its
    own independent slot regardless of what other types share the same string key

- Fixed subscriptions not being restarted after their task completes naturally
  - If a subscription's underlying task finished on its own (e.g., a WebSocket connection
    dropped, or a finite stream reached its end), `SubscriptionManager::update()` still
    found its ID in the running map and skipped restarting it — the subscription was
    silently dead until the app explicitly toggled it off and on again
  - Finished tasks are now detected on every `update()` call and removed from the map so
    that the subscription is restarted if still requested, or cleaned up if not

- Fixed `Query` subscriptions missing invalidations broadcast while a fetch is in-flight
  (requires `http` feature)
  - Previously, `perform_fetch` subscribed to the invalidation channel *after*
    `fetcher().await` returned, so any invalidation broadcast during the fetch was
    silently dropped; the subscription would serve stale data indefinitely until the
    next explicit invalidation
  - Also fixed a narrower window in `State::Initial` where an invalidation could arrive
    between the cache read and the subscription setup in the fresh-data path
  - A single `broadcast::Receiver` is now subscribed at the very start of `State::Initial`
    and threaded through the entire state machine so no invalidation is ever missed

## [0.8.1] - 2026-06-25

### Changed

- **Performance**: Subscriptions are no longer re-evaluated on every frame
  - `Runtime` previously called `Application::subscriptions()` on every frame tick (e.g. 60×/s),
    allocating a `Vec` and rebuilding every `Subscription` (boxing spawners, cloning `Arc`s) even
    while idle
  - Since `subscriptions()` is a pure function of the application state, it is now re-evaluated
    only after a message is processed, eliminating this per-frame work for idle applications
  - Fully backward compatible for applications whose `subscriptions()` depends only on their state

### Added

- `QueryClient::gc()` to manually garbage collect expired cache entries (requires `http` feature)
  - Removes cached entries older than the configured `cache_time`
  - Useful for reclaiming memory for keys that are no longer being fetched

### Fixed

- Fixed WebSocket connections leaking when a subscription is cancelled (requires `ws` feature)
  - The connection task could stay parked on `read.next()` and `cmd_rx.recv()` after the
    subscription's message stream was dropped, keeping the underlying connection open
  - The connection task now observes the dropped message receiver via `msg_tx.closed()` and
    shuts the connection down cleanly, during both connection setup and the main loop
- Fixed the query cache growing without bound (requires `http` feature)
  - Cached entries were never removed, so `cache_time` had no effect and memory grew as new
    keys were fetched over time
  - Entries older than `cache_time` are now garbage collected automatically on each cache
    insertion, and can also be reclaimed manually via `QueryClient::gc()`
- Fixed `Query` subscriptions permanently terminating when invalidation notifications lagged
  (requires `http` feature)
  - A burst of invalidations exceeding the broadcast channel capacity caused the watcher to
    receive a `Lagged` error, which was treated as a closed channel and silently ended the
    subscription, so it never refetched again
  - A lagged receiver now refetches (since a dropped notification may have been for its key)
    and only a genuinely closed channel ends the subscription
- Improved frame rate timing accuracy in `Runtime`
  - The frame interval was computed with integer millisecond division (`1000 / frame_rate`),
    truncating the period; for example 60 FPS ran at 16ms (~62.5 FPS) and 144 FPS at 6ms
  - The period is now derived from an exact `Duration` division, so the requested frame rate
    is honored precisely

## [0.8.0] - 2026-01-22

### Added

- Added `Command::is_none()` and `Command::is_some()` methods for checking command state
  - Useful for testing whether `update()` returns the expected command type
  - Provides better encapsulation than accessing internal `stream` field directly

### Changed

- Added `#[must_use]` attribute to `Command` type to prevent accidental ignoring of side effects
- **BREAKING**: Upgraded `thiserror` from 1.0 to 2.0 (used in `http` feature)
  - This is a breaking change for downstream crates that depend on `tears` with the `http` feature and also depend on `thiserror` 1.x
  - The `QueryError` type is part of the public API and derives from `thiserror::Error`
  - If you use `thiserror` 1.x in your project alongside `tears`, you may need to upgrade to `thiserror` 2.0 to avoid dependency conflicts

## [0.7.0] - 2026-01-07

### Changed

- **BREAKING**: Simplified Runtime API
  - Changed from `Runtime::new(flags).run(&mut terminal, frame_rate)` to `Runtime::new(flags, frame_rate).run(&mut terminal)`
  - Frame rate is now specified during Runtime creation instead of when calling `run()`
  - Example:
    ```rust
    // Before
    let runtime = Runtime::<MyApp>::new(());
    runtime.run(&mut terminal, 60).await?;

    // After
    let runtime = Runtime::<MyApp>::new((), 60);
    runtime.run(&mut terminal).await?;
    ```

- **Performance**: Event-driven message processing for improved input responsiveness
  - Runtime now processes messages immediately as they arrive via `tokio::select!`
  - Message processing is no longer bound to frame rate interval
  - User input (keyboard, mouse) is now handled with sub-millisecond latency
  - Frame rate interval is still used for periodic subscription updates and batch rendering
  - Significantly improves perceived responsiveness, especially at lower frame rates (e.g., 16 FPS)
  - Combines with `should_render()` check to avoid unnecessary renders while maintaining instant input feedback

## [0.6.1] - 2026-01-06

### Added

- **`http` feature** for HTTP subscription and mutation support
  - New optional feature that can be enabled with `features = ["http"]`
  - Adds `subscription::http` module with Query and Mutation types
- HTTP subscription support for data fetching and mutations (requires `http` feature)
  - `Query` subscription for automatic data fetching with caching
    - Subscription-based design: monitors cache state and automatically refetches when needed
    - Stale-while-revalidate pattern: shows cached data while refetching in background
    - Automatic cache management with configurable stale time and cache time
    - `QueryClient` for cache management and invalidation
    - `QueryState` enum for Loading/Success/Error states with stale flag
  - `Mutation` for HTTP data modifications (POST, PUT, PATCH, DELETE)
    - Command-based API: returns `Command<Result<T, QueryError>>`
    - Works seamlessly with `Command::map` for flexible result handling
  - `QueryClient::invalidate()` for cache invalidation (returns `Command`)
    - Automatically triggers refetch in active Query subscriptions
    - TEA-compliant: all side effects expressed as Commands
  - Design philosophy documentation explaining subscription-based vs transaction-based patterns
  - Example: `examples/http_todo.rs` demonstrating Query, Mutation, and cache invalidation
- `Command::map` for transforming command message types
  - Similar to iced's `Task::map` (v0.14.0)
  - Enables flexible message type conversion
  - Preserves `Action::Quit` correctly
  - Used with `Mutation` for result handling without `to_message` parameter
- Added `reqwest` 0.12 with `json` feature (dev dependency for examples)
- Added `serde` 1.0 with `derive` feature (dev dependency for examples)
- Added `serde_json` 1.0 (dev dependency for examples)

### Changed

- **Performance**: Implemented conditional rendering with dirty flag
  - Runtime now skips rendering when application state hasn't changed
  - Added `needs_redraw` flag to `Runtime` for tracking render necessity
  - `Runtime::process_messages()` now returns `bool` indicating if messages were processed
  - Rendering only occurs when messages are processed or on initial draw
  - Significantly reduces CPU usage and terminal I/O operations
  - Expected ~98% reduction in rendering calls for idle applications
  - Near-zero CPU usage when no events are occurring
  - Fully backward compatible - no changes required to existing applications
- **Performance**: Optimized subscription updates with hash-based caching
  - Runtime now caches subscription IDs hash to skip unnecessary updates
  - Added `subscription_ids_hash` field to `Runtime` for change detection
  - `SubscriptionManager::update()` is only called when subscriptions actually change
  - Provides 37% CPU reduction in subscription processing (measured with flamegraph)
  - 50.7% overall performance improvement for applications with static subscriptions
  - Particularly effective for applications with fixed or infrequently-changing subscriptions
  - Maintains full support for dynamic subscriptions without performance penalty
  - Fully backward compatible - no changes required to existing applications

## [0.6.0] - 2026-01-05

### Added

- `Command::message()` - Send a message to the application immediately
  - This is a tears-specific feature for immediate message dispatch
  - Replaces `Command::single()` with a clearer name that better represents the operation
  - More explicit than `single` and reserves `send`/`dispatch`/`emit` for future extensions

### Changed

- **BREAKING**: `Command::single()` has been removed
  - Use `Command::message()` instead for sending messages immediately
  - This change aligns with iced v0.14.0 design principles while maintaining tears' self-messaging feature
- Simplified `Runtime` internals by removing `Instance` wrapper
  - `Runtime` now directly holds the application instead of wrapping it in `Instance<App>`
  - Eliminates unnecessary indirection (`.inner`) throughout the codebase
  - Improves code readability with no functional changes
- Optimized `Runtime::process_messages()` for better performance
  - Now collects all pending messages and batches their commands together
  - Reduces tokio task spawning overhead when processing multiple messages
  - Improves performance when there are many pending messages in a single frame
- Improved error handling in `examples/counter.rs`
  - `Message::TerminalError` now holds `io::Error` instead of `String`
  - Preserves full error information instead of converting to string
  - Removed unnecessary `Clone` derives from `Message` and `Counter`
- Upgraded `tokio` from 1.48.0 to 1.49.0
- Upgraded `tokio-stream` from 0.1.17 to 0.1.18
- Upgraded `tokio-util` from 0.7.17 to 0.7.18

### Migration Guide (v0.5.0 → v0.6.0)

#### Command API Changes

Replace all uses of `Command::single()` with `Command::message()`:

```rust
// Before (v0.5.0)
Command::single(Message::Refresh)

// After (v0.6.0)
Command::message(Message::Refresh)
```

This is a mechanical replacement with identical functionality.
The new name better clarifies the intent and reserves more generic verbs (`send`, `dispatch`, `emit`) for potential future features.

## [0.5.0] - 2026-01-04

### Changed

- Upgraded `ratatui` from 0.29 to 0.30
- Upgraded `crossterm` from 0.28 to 0.29
- Updated MSRV (Minimum Supported Rust Version) from 1.85.0 to 1.86.0
- Updated `Runtime::render` and `Runtime::run` return types to use generic backend error types
- **BREAKING**: WebSocket subscription now supports bidirectional communication
  - Added `WebSocketCommand` enum for sending messages (`SendText`, `SendBinary`, `Close`)
  - Added `WebSocketMessage` enum for subscription output (`Connected`, `Disconnected`, `Received`, `Error`)
  - `WebSocket` subscription now emits `WebSocketMessage` instead of raw `Message`
  - `WebSocketMessage::Connected` provides command sender when successfully connected
  - `WebSocketMessage::Disconnected` is emitted on normal connection closure
  - `Message::Close` frames are handled internally and result in `Disconnected` event
  - Single WebSocket connection handles both receiving and sending
  - Updated `examples/websocket.rs` to demonstrate bidirectional communication

## [0.4.1] - 2026-01-04

### Added

- Mock subscription source for deterministic testing
  - New `subscription::mock::MockSource` for controllable event emission in tests
  - Enables testing without real I/O or time dependencies
  - Shared (cloneable) design allows use in both application code and test code
  - Based on `tokio::sync::broadcast` for efficient multi-receiver support
  - Comprehensive documentation with testing examples in README.md
  - Added `sync` feature to `tokio-stream` dependency for broadcast stream support
- WebSocket subscription support for real-time bi-directional communication
  - New `subscription::websocket::WebSocket` subscription source (requires `ws` feature)
  - Supports both secure (wss://) and insecure (ws://) connections
  - TLS backend options:
    - `native-tls` - Platform's native TLS implementation
    - `rustls` - Pure Rust TLS with ring crypto provider and native root certificates
    - `rustls-tls-webpki-roots` - Pure Rust TLS with ring crypto provider and webpki root certificates
  - Automatic connection management and reconnection handling
  - Streams all WebSocket message types (Text, Binary, Ping, Pong, Close)
  - Example: `examples/websocket.rs` demonstrating WebSocket echo chat
  - Comprehensive documentation with usage examples and TLS configuration guide

## [0.4.0] - 2026-01-03

### Changed

- **BREAKING**: Improved `Timer` subscription performance and accuracy
  - Migrated from `tokio::time::sleep` to `tokio::time::interval` for better timing accuracy
  - Uses `MissedTickBehavior::Skip` to maintain consistent tick rate (drops missed ticks instead of catching up)
  - Provides drift correction for high frame rates (60+ FPS)
  - Added `tokio-stream` dependency for interval stream support
- Improved `Runtime` frame timing accuracy
  - Migrated from `tokio::time::sleep` to `tokio::time::interval` for consistent frame rate
  - Uses `MissedTickBehavior::Skip` to skip missed frames when rendering takes longer than frame duration
  - Provides more accurate and stable FPS delivery

## [0.3.0] - 2026-01-02

### Fixed

- Fixed signal subscriptions emitting spurious events on initialization

### Removed

- **BREAKING**: Removed `ignore_initial()` method from signal subscriptions (`Signal`, `CtrlC`, `CtrlBreak`) as it is no longer needed after fixing the spurious event bug

## [0.2.0] - 2026-01-02

### Added

- Signal subscription helpers
  - `ignore_initial()` method to filter spurious signals during TUI initialization

### Changed

- **BREAKING**: Signal subscriptions refactored for simplicity
  - `Signal`, `CtrlC`, and `CtrlBreak` include grace period configuration
  - Removed `Copy` implementation from signal types (not needed in practice)
- Initialized `color_eyre` in example applications for improved error reporting and debugging experience

## [0.1.1] - 2026-01-02

### Added

- Signal subscription support for handling OS signals
  - Unix: `signal::Signal` for SIGINT, SIGTERM, SIGHUP, SIGQUIT, and 20+ other signals
  - Windows: `signal::CtrlC` and `signal::CtrlBreak` for Ctrl+C and Ctrl+Break events
  - Uses `tokio::signal` types directly (no unnecessary wrappers)
  - Returns `Result<(), io::Error>` for proper error handling
  - Example: `examples/signals.rs` demonstrating signal handling with graceful shutdown

### Fixed

- Downgraded `crossterm` from 0.29 to 0.28 to match `ratatui`'s dependency requirements

## [0.1.0] - 2025-12-30

### Added

- Initial release of tears framework
- Core Elm Architecture implementation with `Application` trait
- Asynchronous command system with `Command` type
  - `Command::none()` - No-op command (const fn)
  - `Command::single(msg)` - Send a single message immediately
  - `Command::perform()` - Execute async operation with result transformation
  - `Command::future()` - Execute async operation that produces a message
  - `Command::effect()` - Execute an action immediately
  - `Command::batch()` - Execute multiple commands concurrently
  - `Command::stream()` - Create command from message stream
  - `Command::run()` - Transform and consume a stream
- Subscription system for event sources
  - Dynamic subscriptions (can change based on application state)
  - Built-in `TerminalEvents` subscription for keyboard/mouse/resize events
  - Built-in `Timer` subscription for periodic ticks
  - Support for custom subscriptions via `SubscriptionSource` trait
- `Runtime` for managing application lifecycle
  - Event loop with configurable frame rate
  - Automatic subscription management
  - Command execution and message dispatching
- Error handling
  - Subscriptions return `Result<T, E>` allowing user-controlled error handling
  - Terminal event errors are propagated to application
- Full async/await support with tokio
- Comprehensive API documentation with examples
- Counter example demonstrating timer and keyboard input

[unreleased]: https://github.com/akiomik/tears/compare/v0.10.1...HEAD
[0.10.1]: https://github.com/akiomik/tears/releases/tag/v0.10.1
[0.10.0]: https://github.com/akiomik/tears/releases/tag/v0.10.0
[0.9.3]: https://github.com/akiomik/tears/releases/tag/v0.9.3
[0.9.2]: https://github.com/akiomik/tears/releases/tag/v0.9.2
[0.9.1]: https://github.com/akiomik/tears/releases/tag/v0.9.1
[0.9.0]: https://github.com/akiomik/tears/releases/tag/v0.9.0
[0.8.3]: https://github.com/akiomik/tears/releases/tag/v0.8.3
[0.8.2]: https://github.com/akiomik/tears/releases/tag/v0.8.2
[0.8.1]: https://github.com/akiomik/tears/releases/tag/v0.8.1
[0.8.0]: https://github.com/akiomik/tears/releases/tag/v0.8.0
[0.7.0]: https://github.com/akiomik/tears/releases/tag/v0.7.0
[0.6.1]: https://github.com/akiomik/tears/releases/tag/v0.6.1
[0.6.0]: https://github.com/akiomik/tears/releases/tag/v0.6.0
[0.5.0]: https://github.com/akiomik/tears/releases/tag/v0.5.0
[0.4.1]: https://github.com/akiomik/tears/releases/tag/v0.4.1
[0.4.0]: https://github.com/akiomik/tears/releases/tag/v0.4.0
[0.3.0]: https://github.com/akiomik/tears/releases/tag/v0.3.0
[0.2.0]: https://github.com/akiomik/tears/releases/tag/v0.2.0
[0.1.1]: https://github.com/akiomik/tears/releases/tag/v0.1.1
[0.1.0]: https://github.com/akiomik/tears/releases/tag/v0.1.0
