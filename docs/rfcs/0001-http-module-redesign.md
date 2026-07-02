# RFC 0001: `http` Module Redesign

- Status: Implemented
- Target: 0.9.0 (breaking changes allowed)
- Feature flag: `http`

## Summary

The `http` module (subscription-based queries and mutations, similar to SWR or
TanStack Query) accumulated a disproportionate number of bugs relative to its
size. The bugs were not isolated mistakes; they shared a small set of structural
causes in the internal model. This RFC replaces that internal model with one
that is *correct by construction*: a single source of truth per query (a state
"cell") driven by an observe-and-reconcile loop, with invariants pinned down by
contract tests (and `loom` for the concurrency core).

The public read API (`QueryResult` accessors, `Query::new`) is largely
preserved; the breaking changes are a consequence of the new model, not a goal
in themselves.

## 1. Background and Motivation

Counting `http`-related fixes in the changelog since the module was introduced
in 0.6.1, there were at least eight, compared with roughly two non-`http` bugs
in the same period — far more than the module's size would predict.

| Release | Bug | Root category |
|---|---|---|
| 0.8.1 | Cache grows without bound (`cache_time` ineffective) | Lifecycle |
| 0.8.1 | A lagging receiver terminates the subscription permanently | Notification model |
| 0.8.2 | `check_staleness(&mut)` mutates a clone and is lost | Type erasure + clone |
| 0.8.2 | Same key, different types overwrite each other in the cache | Type erasure + keying |
| 0.8.2 | Invalidation during an in-flight fetch is dropped | Notification model / state machine |
| 0.8.3 | `invalidate` does not mark the cache stale, so it is lost when no subscriber is active | Dual source of truth |
| 0.8.3 | Same key, different client treated as identical | Identity |
| 0.8.3 | Zero-duration boundary | Spec / implementation drift |

These stem from structural flaws in the internal architecture. The main lever
for stability is therefore the internal model; API changes follow only where
necessary.

## 2. Root Causes

1. **Dual source of truth.** A `DashMap` (cache) and a `broadcast::Sender<String>`
   (notification) existed side by side and had to be kept in sync by hand. The
   "dropped invalidation during fetch," "lost when no subscriber," "terminate on
   lag," and "`invalidate` does not mark stale" bugs all trace back to this. As
   long as notification and state can disagree, bugs keep appearing.
2. **Hand-rolled async state machine.** Each subscription's `stream::unfold`
   threaded a broadcast receiver by hand across `Initial`/`Fetching`/`Watching`,
   with "subscribe before read," "refetch on lag," etc. handled as manual cases.
   One missed case equals one bug.
3. **Type erasure returning clones.** Downcasting `Box<dyn Any>` and returning a
   clone caused both keying collisions and "mutation on a clone is lost."
4. **Weak identity.** Subscription identity used a string key and a hand-written
   `Hash` that dropped the client and request parameters.

## 3. Goals and Non-Goals

### Goals

- A *correct-by-construction* internal model: single source of truth plus
  observe-and-reconcile.
- Invariants stated explicitly and pinned by contract tests (and `loom` for
  races).
- A foundation on which SWR-like features can be added later as **non-breaking
  `Added`** changes.

### Non-Goals (not implemented in 0.9.0)

- **Timer-driven background revalidation** (periodic refetch with no triggering
  event, i.e. a `refetchInterval` equivalent). Time-based `stale_time` itself is
  **retained** in the 0.9.0 core (see note below).
- Retry / backoff policy.
- Prefetch / advanced cache control.
- **Optimistic update** (out of scope, but the design leaves room to add it
  later; see §9).

**Note: `stale_time` is retained in the 0.9.0 core.** `QueryConfig::stale_time`
is already public in 0.8 with observable behavior (emit-then-refetch when the
data is time-stale on subscribe). Removing time-based staleness in 0.9.0 would
be a breaking *feature regression*, so it is retained:

- `is_stale` is defined as the OR of generation staleness and time staleness
  (§5.2).
- On subscribe/reconcile, if data exists and is stale, the query emits the stale
  value and then refetches (revalidate-on-observe). This matches 0.8 behavior.
- Only "timer-driven refetch with no event" is a non-goal; the value, type, and
  revalidate-on-observe behavior of `stale_time` are unchanged, so this is not a
  breaking `QueryConfig` change.

## 4. Non-negotiables (preconditions for the additive path)

The small core must be "the final architecture with features removed," not "a
rewrite in progress." Violating this would force a re-architecture when SWR is
added later.

**(A) The state shape is rich from the start.** A flat
`enum QueryState { Loading | Success | Error }` cannot express "data present +
fetching," so adding SWR later would be a breaking re-model. The state is a
struct with private fields and accessor-based reads (§5.1).

**(B) State is centralized in a cell; the subscription stream drives fetching
via observe-and-reconcile.** Each subscription observes the cell and reconciles;
the stream that wins single-flight fetches with its own fetcher and writes the
result back. The cell is the single source of truth for state; the stream
remains the driver of fetching (§5.3). This makes the "notification vs. cache
disagreement" bug class structurally impossible — the concept of "lag"
disappears entirely.

## 5. Design

### 5.1 `QueryResult` (rich state)

```rust
pub struct QueryResult<T> {
    data: Option<T>,
    error: Option<QueryError>,
    status: QueryStatus,        // Pending | Success | Error
    fetch_status: FetchStatus,  // Idle | Fetching
    is_stale: bool,
}

#[non_exhaustive]
pub enum QueryStatus { Pending, Success, Error }

#[non_exhaustive]
pub enum FetchStatus { Idle, Fetching }
```

- Fields are **private**; reads go through accessors (`data()`, `status()`,
  `error()`, `is_loading()`, `is_success()`, `is_error()`, `is_fetching()`,
  `is_stale()`). Public fields would contradict the ability to add an optimistic
  overlay or extra state non-breakingly later (§9).
- `QueryStatus::Error` is a *state kind*; the error value itself is exposed via
  `QueryResult::error() -> Option<&QueryError>`. Because the design allows
  `data = Some(previous), status = Error`, encoding the error as an
  `Error(QueryError)` variant would leak "keeps data even in the error state"
  into the enum and clash with the accessor-centric API.
- Public enums (`QueryStatus`, `FetchStatus`) are `#[non_exhaustive]` so variants
  can be added non-breakingly.
- `fetch_status` only moves between `Idle`/`Fetching` in the core, but the type
  exists from the start so future features do not change user code.
- **`is_stale` is a snapshot value taken at emit time.** Its definition depends
  on wall-clock (`data_timestamp.elapsed() >= stale_time`), but the watch
  snapshot only updates on events such as observe/reconcile (timer-driven updates
  are a non-goal). So after fresh data is emitted, if `stale_time` elapses with
  no event, the subscriber keeps an `is_stale = false` snapshot until the next
  reconcile. This is intended and documented: elapsed `stale_time` is not
  immediately reflected as `is_stale = true` (this prevents the recurring
  "time vs. flag drift" bug class).

### 5.2 Staleness (generation + time)

The cell holds `current_generation` (bumped by `invalidate`), the
`data_generation` the current data was fetched at, and the fetch time
`data_timestamp`.

```text
is_stale := data.is_some()
         && ( data_generation < current_generation
           || data_timestamp.elapsed() >= stale_time )
```

Data is stale if an invalidation arrived after it was fetched, or `stale_time`
has elapsed (time-based staleness retained per §3). The boundary is `>=` (the
0.8.3 fix).

This makes the states orthogonal:

- fresh + idle: `data = Some, is_stale = false, fetch_status = Idle`
- stale-while-revalidate: `data = Some, is_stale = true, fetch_status = Fetching`

Data-less states are `is_stale = false` (staleness presupposes data). When there
is no data (e.g. the first subscribe after `invalidate`), the query is
Pending/Fetching and fetches; `generation` is used only for the commit race
check.

### 5.3 State cell and observe-and-reconcile (single source of truth)

Per (TypeId, key) there is a **state cell** owned by the `QueryClient`, holding:
`data`, `current_generation`, `data_generation`, `data_timestamp`, `status`,
`fetch_status`, `in_flight_generation: Option<u64>`, and
`last_error_generation: Option<u64>`. Each cell owns a typed
`watch::channel::<QueryResult<T>>` used purely as a change notification.

- The subscription stream observes the cell via `watch::Receiver::changed()` and
  reconciles. Only the stream that wins single-flight fetches with its own
  fetcher and writes the result back.
- The watch payload is never read directly by the stream; on every wakeup the
  stream re-reads authoritative state under the cell mutex. The watch is a
  notification only, so a stale payload cannot cause incorrect behavior.

### 5.4 Single-flight (cell contract)

Single-flight does **not** depend on the runtime deduplicating to "one stream
per identity." It is a **cell contract**, so it holds even when `Query::stream()`
is constructed multiple times directly, across multiple `SubscriptionManager`s,
or via a future alternate runtime.

Whether a reconcile pass should fetch is decided by a predicate (§5.7). The
check-and-set of `in_flight_generation` happens under the cell state mutex, so
two concurrent reconciles cannot both acquire the in-flight slot:

- The winner (which sets `in_flight_generation = Some(current_generation)`)
  fetches with its own fetcher.
- Losers do not fetch; they await `watch.changed()` for the result.
- A stream whose `last_error_generation == current_generation` (already failed
  at this generation) neither fetches nor waits; it observes the current
  (stale + Error) snapshot and waits for `invalidate`/generation progress (§5.6).

Any two streams for the same identity share the same cell (via the cell map), so
single-flight holds across them without runtime deduplication.

**Fetcher arbitration** = "the fetcher of the first stream to acquire the
in-flight slot." For the same identity the request is equivalent by the §5.8
contract, so any stream's fetcher yields the same result.

**On completion the in-flight slot is always released** (success, error, or
discard) under the mutex (liveness; INV-8). Completion branches into one of:

- Acquired generation `G == current_generation` (compare-and-commit succeeds):
  apply the result to `data`/`status`, set `in_flight_generation = None`. On
  success set `last_error_generation = None`; on error set
  `last_error_generation = Some(G)` and keep the previous data/`is_stale` (§5.6).
- `invalidate` bumped the generation during the fetch, so `G < current_generation`:
  **discard the result, clear `in_flight_generation`, send on the watch**, and
  re-fire reconcile at the latest generation.

**Coalescing.** Multiple bumps within one in-flight window coalesce into a
single refetch at the next latest generation; if a further bump occurs during
that refetch, the commit fails again, so the loop repeats until
`acquired generation == current_generation` commits (the structural fix for the
0.8.2 "invalidation during fetch"; consistent with INV-3).

### 5.5 Invalidation (synchronous)

`invalidate()` completes cell state update (bump `current_generation`) **and**
the watch send as a single synchronous operation; the two are not separated. So
active subscribers reconcile regardless of command execution order, and the time
`T` in INV-1 is unambiguously the method-call time.

- `invalidate()` returns nothing (no `Command`). Callers write it as a statement
  inside `update()`: `self.client.invalidate(&key);`. The cell is `Arc`-shared
  mutable state, so a `&self` method updates it directly.
- Trade-off: this departs from "side effects go through `Command`" in TEA.
  However, the cell is already `Arc`-shared mutable state, and this is the same
  kind of direct side effect as calling a method on `Arc<Mutex<_>>`. Correctness
  (INV-1/INV-4 not depending on command execution) is prioritized.

### 5.6 Error handling and retry suppression

The main benefit of the rich `QueryResult` (orthogonal `data` and `status`) is
that a background refetch can fail while keeping the previous data.

- **Data retention.** When a fetch fails, the previously successful `data` is
  retained. The cell can represent `data = Some(previous), status = Error`;
  `data()` returns the previous value while `is_error()` is true. An error with
  no prior data is `data = None, status = Error`. The error value is observed via
  `QueryResult::error()`.
- **In-flight release.** An error is a kind of completion, so
  `in_flight_generation` is always released (INV-8).
- **Staleness.** On error the retained data keeps its `is_stale` (an error does
  not change freshness). So the cell after an error can be
  `data = Some(stale), status = Error, is_stale = true, in_flight = None`.
- **Retry suppression.** If reconcile only fetched on "no data or stale," an
  error's watch update would immediately refetch at the same generation and spin
  on failure. So the cell keeps `last_error_generation`, and the fetch predicate
  requires `last_error_generation != current_generation`. Once a generation has
  failed, it does not refetch until `invalidate` (a generation bump) or
  generation progress. On success `last_error_generation` is cleared. This makes
  the "error → watch update → immediate refetch" loop structurally impossible;
  automatic retry/backoff stays a non-goal and can later be added on top of
  `last_error_generation` non-breakingly (INV-9).

### 5.7 Reconcile decision

Reconcile is driven by a small pure predicate over a snapshot of cell state and
the reconcile reason:

```rust
pub(super) enum ReconcileReason { InitialObserve, WatchChanged }

pub(super) struct FetchDecisionInput {
    has_data: bool,
    generation_stale: bool,
    time_since_data: Option<Duration>,
    stale_time: Duration,
    has_in_flight: bool,
    last_error_generation_matches_current: bool,
}

pub(super) fn should_fetch(reason: ReconcileReason, input: FetchDecisionInput) -> bool
```

- Data absence and generation staleness are fetch reasons for **every** reason.
- **Time staleness only triggers a fetch on `InitialObserve`**, not on
  `WatchChanged`. This is the loop guard (INV-10): with `stale_time = ZERO`, a
  fetch-completion watch update received by another stream would otherwise
  satisfy `elapsed >= ZERO` and refetch the same generation forever. Restricting
  time-stale fetches to observe boundaries prevents this.
- Only two reasons are needed: an explicit `invalidate` on an active subscriber
  is handled by generation staleness under `WatchChanged` (no distinct reason
  required), and re-observing an inactive cell always goes through
  `State::Initial` = `InitialObserve` (where time-stale fetches are allowed).
  `ReconcileReason` is `#[non_exhaustive]` internally, so a future feature (e.g.
  timer-driven revalidation) can add a reason.
- `ReconcileReason`, `FetchDecisionInput`, and `should_fetch` are `pub(super)`
  (internal to the `http` module); they are not part of the public API.

The predicate combines as:

```text
needs_fetch := (no_data || generation_stale || (time_stale && reason == InitialObserve))
            && !has_in_flight
            && last_error_generation != current_generation
```

### 5.8 Identity

- A structured `QueryKey(Arc<[QueryKeyPart]>)` with
  `QueryKeyPart::{Str, I64, U64, Bool}` (`#[non_exhaustive]`). `From<&str>`,
  `From<String>`, and small tuple/array conversions are provided. Structured
  keys compare structurally and do not collide.
- **Subscription identity = (`client_id`, `TypeId::of::<V>()`, `QueryKey`).**
  Identity must always include `TypeId` so that `Query<i32>("data")` and
  `Query<String>("data")` are distinct (prevents the 0.8.2 type collision).
- **Cell map key = (`TypeId`, `QueryKey`).** Client separation is achieved by the
  `QueryClient` *owning* its cell map (per-client), so `client_id` is not part of
  the cell map key (that would be redundant double-bookkeeping). Subscription
  identity still includes `client_id` because the runtime shares one subscription
  space and must distinguish the same (TypeId, QueryKey) across clients.

### 5.9 Type erasure

Type erasure is unavoidable since one client handles multiple value types. To
avoid the "return a clone, lose the mutation" bug, the cell updates by atomic
replace and never leaks a mutable reference.

Because both the value and the cell (which holds `data: Option<T>` and a typed
`watch` channel) are typed, the whole cell is erased into a heterogeneous map:

```rust
trait AnyCell: Any + Send + Sync + 'static {
    fn into_any_arc(self: Arc<Self>) -> Arc<dyn Any + Send + Sync>;
    fn invalidate(&self, config: &QueryConfig);
    fn subscriber_count(&self) -> usize;
    fn inactive_since(&self) -> Option<Instant>;
    fn gc_inactive_data_and_should_evict(&self, cache_time: Duration) -> bool;
}
```

- The cell map is `DashMap<(TypeId, QueryKey), Arc<dyn AnyCell>>`.
- Typed access goes through `get_or_subscribe_cell::<T>()`, which downcasts via
  `Arc::downcast` (needed because it returns an owned `Arc`). Because `TypeId` is
  part of the key, the downcast always matches.
- Operations that do not need `T` (GC, subscriber count, inactive time) are
  `AnyCell` methods called directly on `Arc<dyn AnyCell>` with no downcast.
- The typed `watch` stays inside `Cell<T>`; the map holds `dyn AnyCell`. Updates
  are limited to atomic replace + `watch.send`.

### 5.10 GC and cell lifecycle

The cell is the single retention store; there is no separate cache. GC targets
the cell's *data* (and related metadata), not the cell identity while it is
active.

- **Active cells keep their data regardless of `cache_time`.** `cache_time` is
  the inactive-retention axis (how long to keep data after the last subscriber
  drops); `stale_time` is the freshness axis. They are independent, and
  `cache_time < stale_time` is allowed. Dropping fresh data from an active cell
  by `cache_time` alone would be a "fresh but gone" bug, so data GC is limited to
  inactive cells; freshness of active cells is handled by `stale_time`
  revalidate-on-observe.
- **Inactive cells (zero subscribers) past `cache_time` are cleared and evicted.**
  The `cache_time` timer is measured from when the cell became inactive
  (last subscriber dropped), not from the fetch timestamp.
- **Subscriber counting** uses an explicit ref count: a `CellSubscription` guard
  increments on creation and decrements on `Drop`, setting `inactive_since` when
  the count reaches zero. This matches "active subscription streams" rather than
  internal receiver clones.
- **Cell creation and the first subscribe are atomic.** `get_or_subscribe_cell`
  performs "insert + first subscribe" under the map shard lock (`entry`), so a
  concurrent GC sweep cannot evict a freshly created cell before its first
  subscriber registers (which would otherwise split one identity across two cells
  and break single-flight).
- **GC runs automatically after each fetch** (`gc_expired` is called on fetch
  completion) and can also be triggered manually via `QueryClient::gc()`. Because
  a fetch always drives a sweep, INV-7 ("inactive data is not retained forever")
  holds without requiring the application to call `gc()`.

### Emission de-duplication

Because the watch receiver is not touched while a fetch is in progress, the
snapshots the stream sends itself during fetching would otherwise be re-delivered
as a spurious duplicate on the first `Watching` wakeup. Each cell carries a
`version: AtomicU64` bumped on every send, and each subscription tracks the
`seen_version` it has already emitted; a wakeup whose version is not newer than
`seen_version` is skipped. The watch is used purely as a notification, so this
only suppresses redundant re-emission and never drops a genuine change.

## 6. Invariants (contract tests)

Invariants are stated first and pinned as executable tests so later additions
cannot break the contract.

- **INV-1.** A query subscribed before the `invalidate()` call time `T` (§5.5,
  synchronous bump) does not lose the invalidation issued at `T`.
- **INV-2.** Single-flight is guaranteed by the cell's `in_flight_generation`
  contract (§5.4): at most one in-flight fetch per identity. It does not depend on
  runtime deduplication and holds even with multiple direct `Query::stream()`s.
  Multiple `invalidate`s during a fetch coalesce into one refetch. Identity is
  `(client_id, TypeId, QueryKey)`.
- **INV-3.** A fetch result is applied only when its target generation equals
  `current_generation` (results from an outdated in-flight fetch are discarded).
- **INV-4a.** With **data present**, subscribing after `invalidate` (or after
  `stale_time` elapses) emits `is_stale = true` data and then refetches
  (stale-while-revalidate).
- **INV-4b.** With **no data**, subscribing after `invalidate` is observed as
  `is_stale = false` Pending/Fetching and fetches respecting the generation (not
  stale data).
- **INV-5.** Different `client_id` / `TypeId` / `QueryKey` — differing in any one
  — hold independent subscriptions and cache slots (includes preventing the 0.8.2
  type collision).
- **INV-6.** A cell with an active subscriber is not GC'd, and its `data` is not
  dropped by `cache_time` (cell, watch channel, and data are retained; data GC is
  limited to inactive cells).
- **INV-7.** An inactive cell's data is reclaimed once `cache_time` elapses from
  when it became inactive (not retained forever).
- **INV-8 (liveness).** A fetch releases `in_flight_generation` whether it ends in
  success, error, or discard, so no cell holds an in-flight slot without
  completing; after a bump the next fetch slot can always be acquired.
- **INV-9 (post-error retry suppression).** After a fetch fails at generation `G`,
  it does not refetch while `current_generation` is still `G` (even if data is
  stale). A refetch happens only when the generation advances (e.g. `invalidate`)
  or a success clears `last_error_generation`. The tight "error → watch update →
  immediate refetch" loop does not occur.
- **INV-10 (time-stale loop suppression).** Even with `stale_time = ZERO` (or an
  elapsed `stale_time`), a `WatchChanged` wakeup caused by the cell's own
  fetch-completion send does not refetch the same generation for "time stale."
  Time-stale fetches are limited to the observe boundary (`InitialObserve`);
  generation staleness and data absence are fetch reasons for any reason.

## 7. Testing strategy

- **`loom`** exhaustively checks the minimal `Mutex`/`Atomic` cell core
  (generation bump, compare-and-commit, single-flight selection, in-flight
  release on completion = INV-8, post-error retry suppression = INV-9). In
  particular it makes explicit the "fetch `G` completes racing a bump to `G+1`,
  no slot leaks" liveness and the "error completion racing `invalidate`" cases.
  The loom core is isolated from `tokio`/`watch`/async and runs under the
  `loom-core` feature.
- **`tokio` integration tests** cover the wiring and timing on the real `Cell<T>`:
  INV-2 (two direct streams share one fetch), coalescing to a single refetch,
  INV-9 end-to-end, INV-10 (`stale_time = ZERO` does not loop on `WatchChanged`),
  and the cell-native retention/invalidation behaviors (INV-4a/4b, cross-
  subscription retention, invalidate-while-inactive).
- **Reconcile predicate tests** for `should_fetch` live next to the code and run
  with the standard `--features http` suite.
- Division of labor: "correctness of the core synchronization primitives" = loom;
  "correctness of the wiring and timing" = tokio tests.

## 8. Migration (0.8.x → 0.9.0)

`http` is behind a feature flag with a limited user base, is pre-1.0, and has
precedent for hard breaks (0.5/0.6/0.7). A long deprecation is over-investment; a
clean break with a good migration guide is clearer and cheaper.

Mitigations:

- `From<&str> for QueryKey` keeps migrating from string keys light.
- The read accessors (`data()` / `is_loading()` / `is_stale()`) are preserved, so
  read-side code stays roughly the same.
- The `Query::new(key, fetcher, client)` shape is preserved.

Semantic breaks to call out in the migration guide:

- **`invalidate` no longer returns a `Command`** and completes "generation bump +
  watch send" synchronously on call (§5.5). Rewrite
  `return self.client.invalidate(&key);` as
  `self.client.invalidate(&key); Command::none()`.
- **`QueryState` is removed** in favor of the rich `QueryResult` (§5.1). Match on
  the accessors (`is_loading()` / `is_success()` / `is_error()` / `data()` /
  `error()`) instead of the old enum.
- **On fetch error, the previous data is retained** (`data = Some, status = Error`)
  rather than lost. UIs that assumed an error clears data should read `data()` and
  `is_error()` independently (§5.6).
- **Retention semantics change:** `cache_time` is measured from when the last
  subscriber becomes inactive, not from the fetch timestamp (§5.10).
- **Replacing a fetcher for the same identity is not supported.** The runtime keys
  the running subscription by identity (`QueryClient`, key, value type), so a new
  `Query::new(key, new_fetcher, client)` with an unchanged key keeps the existing
  subscription and the old fetcher. To change the request, change the key.

## 9. Optimistic update

Optimistic update is out of scope (and is not supported in 0.8.x either —
`Mutation::mutate` is refetch-based and never touches the store), so excluding it
in 0.9.0 is not a regression. The cell state leaves room to add it
non-breakingly later: a "provisional overlay" (provisional value + rollback info)
can be layered on top of the committed value; in 0.9.0 that overlay is always
empty.

## 10. Future work (additive, non-breaking)

Each of these is expected to be an `Added` change, not a `Changed`/`Fixed`. That
they are additive is the confirmation that the core (A)(B) hold; if any turns out
to require a breaking change, the core is missing something.

- **Timer-driven background revalidation** (`refetchInterval` equivalent): add a
  timer branch to reconcile. Distinct from the `stale_time` revalidate-on-observe
  retained in the core (observe-triggered vs. timer-triggered).
- **Retry / backoff policy**: add "up to N times at the same generation / refetch
  after backoff" to the reconcile predicate, built on `last_error_generation`
  (§5.6).
- **Prefetch**, a `refetch(key)` convenience, etc.
