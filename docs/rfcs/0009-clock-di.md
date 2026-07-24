# RFC 0009: Clock DI — deterministic time via the virtual clock

- Status: Draft
- Target: a crate-wide determinism contract for time-dependent behavior,
  with no new public API
- Scope: the no-clock-abstraction decision, the single-time-source rule,
  the controlled-context determinism contract consumed by TestStore
  stage 2 / debounce–throttle / subscription restart rate control, the
  HTTP time-source migration prerequisite, and the `test-util` feature
  decision
- Feature flag: none
- CHANGELOG: none (the prerequisite migration and the lint are internal
  and behavior-preserving)

## Summary

Four decisions:

1. **No clock abstraction** (§2). tears introduces no `Clock` trait, no
   injected clock value, and no clock parameter on any API. The crate's
   single time axis is the async executor's clock (`tokio::time`), which
   is already virtualizable: a single-threaded context can start it
   paused and advance it explicitly. "Clock DI" — the working name this
   RFC resolves — lands as a source discipline plus a citable
   determinism contract, not as an injection surface.
2. **Single time source** (§3.1, INV-C1). Every time read in library
   code goes through the virtualizable clock. `std::time::Instant::now`,
   `std::time::SystemTime::now`, and `std::thread::sleep` are banned
   mechanically via clippy's `disallowed-methods`. Two HTTP-module files
   violate the rule today; migrating them is this RFC's named
   prerequisite (§4).
3. **Controlled-context contract** (§3.2, INV-C2, INV-C3). A consumer
   that controls the clock may rely on: virtual time advances only under
   the controller's action (plus the executor's idle auto-advance, which
   a non-idling controller never triggers); time-gated behavior never
   fires before its deadline; and every time-dependent contract pinned
   elsewhere (RFC 0004 timeout/retry, `Timer` semantics) holds
   identically under virtual time, so a virtual-clock run is evidence
   about production behavior.
4. **Production neutrality** (§3.3, INV-C4). No production surface
   changes: the runtime never pauses or configures the clock, no public
   type or config field mentions time control, and unpaused behavior is
   byte-for-byte the platform monotonic clock.

The dependency direction fixed by RFC 0008 §7 is honored: TestStore
stage 2 consumes this contract; nothing here gates on TestStore.

## 1. Scope

### 1.1 In scope

- The decision that no clock abstraction exists (§2) and the rejected
  injection designs (§2.2).
- The single-time-source rule over all library code, with its
  enforcement (§3.1).
- The determinism contract a controlled time context provides to
  consumers (§3.2), and its negative space (§3.4).
- The HTTP time-source migration, as a named prerequisite (§4).
- The `tokio` `test-util` feature decision for in-crate controlled
  contexts (§5.1).
- A deterministic-time testing recipe in `docs/testing.md` for
  application authors (§5.5) — a documentation deliverable, not
  contract surface.

### 1.2 Out of scope

- **Calendar time.** The crate reads no wall-calendar time at all — no
  `SystemTime` read exists in the repository — and this RFC adds no
  calendar-time contract. An application that renders dates injects its
  own source through `Flags`, the general dependency-injection pattern,
  which stays a docs/examples concern.
- **Randomness.** Jitter for retry backoff or HTTP refetch is a
  separate determinism axis with its own reproducibility design,
  deferred by RFC 0004 §5.2; a future backoff-policy RFC owns it.
- **TestStore's time API.** The shape of `advance` on the store, and
  how time-gated leaves join the §4.2 canonical order, is the RFC 0008
  stage-2 amendment's job. This RFC provides the contract that
  amendment cites (§5.1).
- **Debounce/throttle and restart-rate semantics.** Their policies and
  invariants belong to their own RFCs; they consume this contract
  (§5.2, §5.3).
- **Real-time accuracy bounds.** No upper bound on timer lateness under
  the real clock is pinned here or anywhere (§3.4).

## 2. Decision: no clock abstraction

### 2.1 The decision and its rationale

tears ships no `Clock` trait and no clock handle. The crate's time axis
is the executor's clock, reached exclusively through `tokio::time`
(§3.1). Deterministic control — freezing time, advancing it explicitly,
firing timers without real waiting — is exercised by running the code
under test on a single-threaded executor with the clock started paused,
which the executor already supports.

Rationale:

- **The instrument already exists and is already the crate's accepted
  practice.** RFC 0004 §1.3 requirement 7 pins "Time-dependent tests
  use Tokio's paused clock" for every timeout/retry contract, and the
  runtime, command, and effect test suites already run dozens of
  paused-clock tests. This RFC promotes that instrument from a test
  technique into the crate-wide contract, rather than layering a second
  clock mechanism above it. RFC 0004 §1.4 deferred "clock dependency
  injection" as a separate concern; this RFC resolves that deferral.
- **An injected clock cannot reach the sites that need it without
  breaking the API.** `Command::timeout`, `Command::retry`'s backoff,
  and `Timer::new` construct time-gated behavior inside `update` and
  `subscriptions`, which receive no environment. Serving them with an
  injected clock requires either a clock parameter on every such
  constructor (breaking churn across the command and subscription
  surface) or an ambient task-local clock — a second, hand-rolled
  virtual clock with its own timer wheel and waker plumbing, shadowing
  the one the executor provides.
- **A second clock splits the time axis.** User effects run arbitrary
  async code inside `Command::future`/`perform`/`stream`, and that code
  is free to call `tokio::time` itself. Under an injected library-level
  clock, library timers would follow the injected clock while user
  timers followed the executor's — one `advance` moving one axis and
  not the other, silently breaking exactly the deterministic tests the
  injection exists to enable. Under the chosen design there is one
  axis: a paused context virtualizes library and user time together.
- **Zero cost where it matters.** No new public API, no new dependency,
  no trait object on any hot path, and production behavior untouched
  (§3.3). The entire user-visible delta is a documented testing recipe
  (§5.5).

### 2.2 Rejected alternatives

- **`Arc<dyn Clock>` injected via `Flags`/environment.** The
  reachability and axis-splitting failures above; additionally the
  trait needs async sleep/interval methods with waker integration to be
  virtualizable at all, duplicating the executor clock's machinery for
  no capability it does not already have. Rejected.
- **Clock resolved at drive time** (the runtime and TestStore pass a
  clock into effect leaves through the decomposition boundary). Touches
  the RFC 0008 INV-T3 parts type and every leaf's poll path, still
  requires the bespoke test clock implementation, and still splits the
  axis for user code inside futures. Rejected.
- **Per-source injection** (`Timer` gains a clock parameter, backoff
  gains another). Partial by construction — `Command::timeout` and
  retry sleeps sit behind no injectable seam — so the crate would carry
  two time regimes plus per-source API growth. Rejected.
- **A dependency registry** (TCA-style ambient dependency system).
  Heavyweight and macro-leaning against the crate's simplicity goals,
  and unnecessary: non-time dependencies already have the `Flags`
  pattern, and time needs no injection at all under the chosen design.
  Rejected.

## 3. The time contract

### 3.1 Single time source

Every **time read** in library code — any operation whose result or
completion depends on the current time: now-reads (`Instant::now`,
`elapsed`), sleeps, intervals, and deadlines — goes through the
executor's virtualizable clock, i.e. through `tokio::time` types and
functions. `Duration` values are plain data and are not time reads.

Inventory of production time reads at this RFC's writing:

| Site | Read | Purpose | Conforming |
| --- | --- | --- | --- |
| `src/command/effect.rs` (timeout leaf) | `tokio::time::sleep` | `Command::timeout` deadline (RFC 0004; anchored at the leaf's first poll) | yes |
| `src/command/retry.rs` (`run_retry`) | `tokio::time::sleep` | retry backoff delay (RFC 0004) | yes |
| `src/subscription/time.rs` (`Timer`) | `tokio::time::interval` | periodic ticks, missed ticks skipped | yes |
| `src/runtime.rs` (event loop) | `tokio::time::Instant` | micro-batch window deadline (RFC 0006 mechanism) | yes |
| `src/runtime/frame_scheduler.rs` | `tokio::time::interval` | frame cadence | yes |
| `src/runtime/channel.rs` (bounded send) | `tokio::time::Instant` | capacity-wait duration in load events (RFC 0006 observability) | yes |
| `src/subscription/http/cell.rs` | `std::time::Instant` | `stale_time` / `cache_time` decisions, `time_since_data` | **no — §4 migration** |
| `src/subscription/http/query.rs` | `std::time::Instant` | `elapsed_ms` tracing field | **no — §4 migration** |

Outside library code, exactly one deliberate exception exists:
`benches/runtime_load.rs` measures real wall-clock latency because
RFC 0006's statistical acceptance criteria are defined on real time; it
carries an explicit lint allow at its measurement sites. Unit and
integration tests contain no `std::time` reads today and fall under the
same rule.

Observability-only reads (tracing durations, load-event fields) follow
the same rule as contract-bearing reads. A uniform rule keeps the
universal claim mechanically checkable and makes observability output
deterministic under a controlled context; there is no exempt category
to reason about.

Enforcement is INV-C1 (§6).

### 3.2 Controlled contexts

A **controlled time context** is a single-threaded executor whose clock
starts paused. (The executor supports pausing only on a single-threaded
runtime; the production multi-thread configuration cannot be paused,
which is production neutrality working as intended, §3.3.) Within a
controlled context, the contract a consumer may cite:

- **Advancement.** Virtual time advances only through (a) the
  controller's explicit advance and (b) the executor's auto-advance to
  the earliest pending timer deadline when the executor is idle. A
  controller that never lets the executor idle on a timer — one that
  polls manually rather than awaiting, as TestStore's §4.1 poll budget
  already requires — therefore observes advancement only from its own
  advance calls. The (b) clause is stated because it is the executor's
  documented behavior, not a facility this contract grants: a consumer
  that awaits a gated future and observes it complete "by itself" is
  seeing auto-advance, and may not cite this RFC for explicit-only
  advancement (adversarial model: an idling test citing INV-C2 —
  excluded by the non-idling condition, which scopes the claim to what
  the controller itself contributes).
- **No early firing.** No time-gated behavior in the crate fires while
  virtual now is earlier than its deadline.
- **Readiness without waiting.** Once an advance moves virtual now to
  or past a deadline, the gated behavior is ready on its next poll with
  no real-time waiting.
- **Transparency.** Every time-dependent contract the crate pins
  elsewhere holds identically under the virtual clock: RFC 0004's
  timeout semantics (deadline anchored at the leaf's first poll, one
  timeout message per modifier) and retry-backoff semantics, and
  `Timer`'s semantics (no tick before one full interval; missed ticks
  skipped, so a single advance spanning multiple intervals yields one
  tick, not a replay burst). The contracts are inequality-shaped (never
  early; at-or-after), so the virtual clock's perfect granularity and
  the real clock's scheduler-dependent lateness both satisfy them; a
  paused-clock run is therefore evidence for the contract itself — the
  clock analogue of RFC 0008's INV-T3 boundary argument.

### 3.3 Production neutrality

The production runtime never pauses, constructs, or configures the
clock. No public type, method, or `RuntimeConfig` field mentions time
control. In an unpaused context every time read above is the platform
monotonic clock, exactly as before this RFC. There is nothing to
migrate, configure, or opt into for any existing application.

### 3.4 Negative space

Pinned deliberately as *not* guaranteed:

- **Equal-deadline ordering.** When two gated behaviors share a virtual
  deadline, their firing order is unspecified. A consumer needing a
  delivery order provides its own linearization — TestStore's §4.2
  canonical order already does — and may not cite the clock for it.
- **Real-time accuracy.** Under the real clock, a timer fires at or
  after its deadline with no pinned upper bound on lateness; wide-margin
  wall-clock tests remain non-normative smoke checks (§6, INV-C3).
- **Cross-context isolation is per-executor.** The paused clock belongs
  to one executor; two controlled contexts do not share time. No
  contract spans contexts.

## 4. Prerequisite: HTTP time-source migration

**Named prerequisite, owned by this RFC's implementation task.**
`src/subscription/http/cell.rs` (lifecycle `inactive_since`, data
`data_timestamp`, and the `stale_time`/`cache_time` comparisons over
them) and `src/subscription/http/query.rs` (the `elapsed_ms` tracing
read) read `std::time::Instant` and are the only library sites outside
the rule. They migrate to the virtualizable clock's `Instant`.

- The migration is behavior-preserving in unpaused contexts: the
  virtualizable `Instant` reads the same platform monotonic clock when
  time is not paused, and none of the migrated values appears in a
  public signature (`inactive_since` is `pub(super)`; the rest are
  module-internal), so the change is invisible outside the module.
- The payoff is that HTTP staleness and cache-eviction behavior —
  contract-bearing time comparisons — become testable under a
  controlled context like every other time-dependent contract.
- INV-C1's mechanical enforcement turns on with this migration; the
  lint cannot land first.

## 5. Consumers

### 5.1 TestStore stage 2 (RFC 0008 §7 amendment)

The stage-2 amendment gives the store a controlled time context and an
advance operation, making §4.3-class leaves (timeout, backoff, timer)
deliverable through ordinary `receive` flow. What it cites here:
INV-C2's explicit-advance determinism (the store's poll budget never
idles the executor, so the auto-advance clause never applies) and
INV-C3's transparency (store results are evidence about production time
contracts, completing the INV-T3 argument on the time axis).

Two design inputs recorded for that amendment:

- **Deadline anchoring.** RFC 0004 anchors a timeout's deadline at the
  leaf's *first poll*, and the store's scans decide when that first
  poll happens; the amendment's advance semantics must account for
  scan-order-dependent anchoring rather than assuming
  construction-time anchoring.
- **Feature availability.** The executor's pause-and-advance facilities
  sit behind the `tokio` `test-util` feature, today a dev-dependency
  only. **Decision:** when the first in-crate controlled-context
  consumer lands — TestStore stage 2 is the named owner — `test-util`
  joins the crate's unconditional `tokio` dependency features. tears
  adds no feature flag of its own, per the no-feature-flag precedent
  for test support (RFC 0008 §3.3): pausing is opt-in at runtime
  construction, so the unconditional feature changes no production
  behavior (INV-C4 continues to hold and its check covers this flip).

### 5.2 Debounce / throttle (future keyed-lifecycle work)

Coalescing timers added to the keyed registry read the executor clock
like every other gated behavior, so their determinism under test needs
no design of its own — their RFC owns coalescing semantics and cites
§3.2 for time control.

### 5.3 Subscription restart rate control (future work)

A minimum-restart-interval or backoff policy on the runtime's
subscription lifecycle reads the same clock; paused-clock event-loop
tests (already the runtime suite's practice) extend to it unchanged.

### 5.4 HTTP interval and backoff policies (future work)

Refetch intervals and failure backoff ride §4's migrated module and
this contract. Their jitter, if any, is randomness — out of scope here
(§1.2) and owned by that policy's RFC.

### 5.5 Applications

A downstream application enables the `tokio` `test-util` feature in its
own dev-dependencies and runs its tests on a paused single-threaded
runtime; feature unification makes every tears time read virtual in
those tests with no tears-side configuration. Deliverable: a
deterministic-time section in `docs/testing.md` documenting the recipe
(paused runtime, explicit advance, the §3.2 auto-advance caveat).
Documentation, not contract surface.

## 6. Invariants

Enforcement classes follow the pre-review checklist's definitions.

- **INV-C1**: every time read in library code goes through the
  virtualizable clock (`tokio::time`), and `std::time::Instant::now`,
  `std::time::SystemTime::now`, and `std::thread::sleep` appear nowhere
  in the repository's Rust targets except the named bench exception
  (§3.1) at its explicitly allowed measurement sites.
  Structural-mechanical: clippy `disallowed-methods` entries for the
  three named calls in `clippy.toml`, failing the workspace lint gate
  on any reintroduction, with the bench allow visible at its use sites.
  Structural-review for what a lint cannot name: a time source that
  bypasses `std` (a direct syscall, or a new dependency reading OS
  time) is checked at dependency and code review — the crate currently
  has no time-reading dependency, and adding one is a reviewable event.
  (Adversarial models: virtualized sleeps combined with `std` now-reads
  for comparisons — caught by the lint; a libc/syscall clock — caught
  only by the review half, which is why the class is structural rather
  than purely mechanical.)
- **INV-C2**: in a controlled context, §3.2's advancement rule holds —
  under a non-idling controller, a gated behavior stays pending across
  arbitrarily many polls until an explicit advance reaches its
  deadline, and is ready on the next poll after one that does.
  Behavioral: a paused-clock test polls a pending `Command::timeout`
  leaf repeatedly without advancing and asserts it remains pending
  (this is the check a compliant implementation passes and an
  implicitly-advancing clock fails), then advances exactly to the
  deadline and asserts readiness without real-time waiting.
- **INV-C3**: the time-dependent contracts pinned by RFC 0004 and by
  `Timer`'s documented semantics hold identically under the virtual
  clock. Behavioral: the existing paused-clock timeout/retry suites
  (`src/command/effect.rs`, `src/command/retry.rs`, `src/runtime.rs`)
  are the timeout/retry half; new paused-clock `Timer` tests pin the
  timer half — no tick ready before an advance of one full interval,
  the first tick ready after exactly one interval, and a single advance
  spanning several intervals yielding exactly one tick (the
  skipped-not-replayed contract). The existing wide-margin real-time
  `Timer` tests remain as non-normative smoke checks; the paused tests
  are the contract's proof.
- **INV-C4**: production neutrality — the crate exposes no time-control
  surface and the production runtime never pauses or configures the
  clock, before and after the §5.1 feature flip. Structural: review of
  `Runtime` construction and `RuntimeConfig` for the absence of any
  clock field, and the public-surface check (`tests/api_surface.rs`)
  showing no time-control item. The §4 migration's
  behavior-preservation half is structural too: the diff is a type
  swap with no logic change, and the existing HTTP suite stays green.

Surface–invariant coverage: this RFC adds no public API surface. Its
contract surface is the source rule (INV-C1), the controlled-context
guarantees (INV-C2, INV-C3), the neutrality guarantee and the §5.1
feature flip (INV-C4), and the §4 migration (INV-C1 turn-on plus
INV-C4's preservation check). The `docs/testing.md` recipe (§5.5) is
documentation and carries no invariant.

## 7. Open questions

None. The nearest candidates are recorded as negative space instead
(§3.4): equal-deadline firing order and real-time accuracy stay
unpinned until a consumer states a need, which none of the named
consumers (§5) does.

## 8. References

- RFC 0004 — command timeout and retry: §1.3 requirement 7 (paused
  clock as the deterministic instrument), §1.4/§5.2 (the clock-DI and
  jitter deferrals this RFC resolves and re-scopes), and the
  timeout/retry semantics INV-C3 carries onto the virtual clock.
- RFC 0006 — runtime load control: the real-time statistical criteria
  behind the bench exception (§3.1); the load-event fields under the
  uniform observability rule.
- RFC 0007 — RuntimeConfig: the config surface INV-C4 checks for the
  absence of clock fields.
- RFC 0008 — TestStore: §7 (the split this RFC completes), §4.1 (the
  poll budget that makes stage 2 a non-idling controller), §4.2 (the
  canonical order that supplies what §3.4 declines to), INV-T3 (the
  evidence-transfer argument INV-C3 mirrors).
- `src/command/effect.rs`, `src/command/retry.rs`,
  `src/subscription/time.rs`, `src/runtime.rs`,
  `src/runtime/frame_scheduler.rs`, `src/runtime/channel.rs` — the
  conforming inventory rows.
- `src/subscription/http/cell.rs`, `src/subscription/http/query.rs` —
  the §4 migration sites.
- `benches/runtime_load.rs` — the named real-time exception.
- `clippy.toml` — INV-C1's mechanical enforcement site.
- `docs/testing.md` — the paused-time testing conventions §5.5 extends.
- `docs/rfcs/pre-review-checklist.md` — enforcement-class definitions.
