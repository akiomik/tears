# RFC 0009: Clock DI — deterministic time via the virtual clock

- Status: Accepted
- Target: a crate-wide determinism contract for time-dependent behavior,
  with no new public API
- Scope: the no-clock-abstraction decision, the single-time-source rule,
  the controlled-context determinism contract consumed by TestStore
  stage 2 / debounce–throttle / subscription restart rate control, the
  HTTP time-source migration prerequisite (§4.1), the `Timer`
  non-catch-up contract-alignment fix (§4.2), and the `test-util`
  feature decision
- Feature flag: none
- CHANGELOG: `Fixed` — short-interval `Timer` subscriptions no longer
  replay a catch-up burst of missed ticks after a delay (§4.2). The HTTP
  time-source migration (§4.1) and the lint are internal and
  behavior-preserving and carry no entry.

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
   code goes through the virtualizable clock. The `std` entry points
   that read the current time or wait on its passage are banned
   mechanically via clippy's `disallowed-methods`, whose entries are
   derived as an inventory in §3.1. Two HTTP-module files violate the
   rule today; migrating them is this RFC's named prerequisite (§4.1).
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
   type or config field mentions time control, and unpaused time reads
   are observably identical to the platform monotonic clock.

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
- The HTTP time-source migration, as a named prerequisite (§4.1).
- The `Timer` non-catch-up contract-alignment fix (§4.2), a
  behavior-changing deliverable carrying a `CHANGELOG: Fixed` entry.
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
  how time-gated leaves join RFC 0008's §4.2 canonical order, is the
  RFC 0008 stage-2 amendment's job. This RFC provides the contract that
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
  and `Timer` (constructed via `Timer::new` in `subscriptions`, its time
  anchored when its stream is built, §4.2) declare time-gated behavior
  inside `update` and `subscriptions`, which receive no environment.
  Serving them with an
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
| `src/subscription/time.rs` (`Timer`) | `tokio::time::interval` | periodic ticks; first-tick and non-catch-up semantics (INV-C3) | yes |
| `src/runtime.rs` (event loop) | `tokio::time::Instant` | micro-batch window deadline (RFC 0006 mechanism) | yes |
| `src/runtime/frame_scheduler.rs` | `tokio::time::interval` | frame cadence | yes |
| `src/runtime/channel.rs` (bounded send) | `tokio::time::Instant` | capacity-wait duration in load events (RFC 0006 observability) | yes |
| `src/subscription/http/cell.rs` | `std::time::Instant` | `stale_time` / `cache_time` decisions, `time_since_data` | **no — §4.1 migration** |
| `src/subscription/http/query.rs` | `std::time::Instant` | `elapsed_ms` tracing field | **no — §4.1 migration** |

The rule's mechanical floor is an inventory of the `std` entry points
whose call reads the current time, blocks on its passage, or configures
a timed block, derived by walking the stable surface of `std`'s time
(`std::time`), thread and synchronization (`std::thread`, `std::sync`),
and portable network I/O (`std::net`) families for those three
classes — not by listing the calls the crate happens to use today:

| Banned entry point | Class |
| --- | --- |
| `std::time::Instant::now` | now-read |
| `std::time::Instant::elapsed` | now-read; reachable on a value converted out of the virtual clock (`tokio::time::Instant::into_std`), so banning `now` alone does not close the type |
| `std::time::SystemTime::now` | now-read |
| `std::time::SystemTime::elapsed` | now-read; callable on the `UNIX_EPOCH` constant without any prior `now` |
| `std::thread::sleep` | real-time wait |
| `std::thread::park_timeout` | real-time wait |
| `std::sync::Condvar::wait_timeout` | real-time wait |
| `std::sync::Condvar::wait_timeout_while` | real-time wait |
| `std::sync::mpsc::Receiver::recv_timeout` | real-time wait |
| `std::sync::mpsc::Receiver::recv_deadline` | real-time wait |
| `std::net::TcpStream::connect_timeout` | real-time wait |
| `std::net::TcpStream::set_read_timeout`, `set_write_timeout` | timed-wait configuration; banning the setters closes the only route to a timed socket wait while the blocking `read`/`write` sites stay unbanned |
| `std::net::UdpSocket::set_read_timeout`, `set_write_timeout` | timed-wait configuration |
| `std::os::unix::net::UnixStream::set_read_timeout`, `set_write_timeout` | timed-wait configuration; `allow-invalid = true` (unresolvable on non-unix targets), enforced by the Linux CI lint |
| `std::os::unix::net::UnixDatagram::set_read_timeout`, `set_write_timeout` | timed-wait configuration; `allow-invalid = true`, enforced by the Linux CI lint |

The deprecated `_ms` spellings (`std::thread::sleep_ms`,
`std::thread::park_timeout_ms`, `std::sync::Condvar::wait_timeout_ms`)
carry their own lint entries. Pure arithmetic between existing time
values (`duration_since`, `checked_add`, comparisons) reads nothing and
stays allowed; untimed blocking (`std::thread::park`, `Condvar::wait`)
waits on events, not on time, and stays allowed. The platform-gated
rows (`std::os::unix::net::UnixStream`, `UnixDatagram` timeout setters)
do not resolve on every compilation target, but `disallowed-methods`
accepts a per-entry `allow-invalid = true` that suppresses the "does not
refer to a reachable function" diagnostic clippy otherwise raises (and
itself suggests) for an unresolvable path, so they are mechanical lint
entries like every other row, and the CI lint gate — which runs on
Linux, where `std::os::unix` resolves — actually enforces them. The
review-half fallback for a platform-gated row would catch a target CI
never lints; that set is empty today (`std` exposes no Windows-only
socket timeout setter — `std::os::windows` adds no such type — so every
current row resolves and is enforced on the Linux gate), and the clause
stands as headroom for a future platform-gated member. The review half
likewise covers what the lint does not name at all — a direct syscall,
or a dependency other than the executor used as a time source (next
paragraph).
Unstable members of the same classes — `std::thread::sleep_until`, the
`std::sync::mpsc`-style timed receives of `std::sync::mpmc`, and
`std::net::TcpStream::set_linger` (`tcp_linger`), which time-bounds the
blocking of `close` and so falls in the timed-wait-configuration
class — are uncallable on the crate's stable toolchain and join the
table on stabilization; `recv_deadline`, equally unstable, is listed
ahead of that defensively because it completes a family already
present, and the §4.1 implementation task confirms the lint accepts its
path.

Dependencies are scoped the same way: the executor clock itself
(`tokio::time`) is the crate's one sanctioned time source, and no
*other* dependency serves library code as a time source — adding one
is a reviewable event. Time spent inside dependencies on the crate's
behalf — the bench harness measuring elapsed time (`criterion`), the
terminal backend's internal event polling — is measurement or
external-input timing, not a time read issued by the crate's code, and
sits outside the axis this rule governs (like network latency, it was
never virtual).

Outside library code, exactly one deliberate exception exists:
`benches/runtime_load.rs` measures real wall-clock latency because
RFC 0006's statistical acceptance criteria are defined on real time.
The §4.1 migration, which lands the lint configuration, adds an explicit
lint allow at the bench's measurement sites in the same change. Unit
and integration tests contain no `std::time` reads today and fall under
the same rule.

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
- **Readiness without waiting.** Once virtual now is at or past a
  deadline, the gated behavior becomes ready with no real-time
  (wall-clock) waiting — the executor need only be driven to make
  progress (process the elapsed timers), not to sleep. This RFC does
  not pin *which* poll observes readiness, nor whether an `advance` call
  itself carries a timer-driver barrier: Tokio's `advance` documents
  that it does not wait for the sleeps it moves past to complete, so the
  exact readiness point is an executor-progress question, not a
  wall-clock one. TestStore stage 2's advance semantics fix it (§5.1);
  the guarantee this RFC pins is only that readiness costs no wall-clock
  time.
- **Transparency.** Every time-dependent contract the crate pins
  elsewhere holds identically under the virtual clock: RFC 0004's
  timeout semantics (deadline anchored at the leaf's first poll, one
  timeout message per modifier) and retry-backoff semantics, and
  `Timer`'s semantics, stated over a running `next_deadline` that starts
  one full interval after the timer's stream-construction anchor: while
  virtual now `<` `next_deadline` no tick is deliverable; once now `>=`
  `next_deadline` exactly one tick becomes deliverable, never a burst,
  however many deadlines have elapsed, and after it is taken nothing more
  is deliverable until now reaches the new `next_deadline`; and delivering
  a tick advances `next_deadline` to the first anchor-phase boundary
  strictly after the current now, so the phase is preserved, not reset
  (§4.2). This is deadline-relative, not gap-relative: after a tick
  delivered late, the very next boundary can be less than one interval
  away and still fires. This fixes *how many* ticks are deliverable and
  the cadence, not *which* poll observes them — that follows the
  readiness guarantee above, and `Timer` claims no stronger same-poll
  guarantee than any other gated behavior. These are observable
  properties of `Timer`, not a claim about `.skip(1)` or any other
  mechanism. This non-catch-up property is a
  contract the `Timer` implementation provides directly; Tokio's
  `MissedTickBehavior::Skip` does not supply it, because its skip engages
  only once a tick is late by more than a fixed margin, so for sub-margin
  intervals it replays one tick per elapsed interval (INV-C3). The
  contracts are inequality-shaped (never
  early; at-or-after), so the virtual clock's perfect granularity and
  the real clock's scheduler-dependent lateness both satisfy them; a
  paused-clock run is therefore evidence for the contract itself — the
  clock analogue of RFC 0008's INV-T3 boundary argument.

### 3.3 Production neutrality

The production runtime never pauses, constructs, or configures the
clock. No public type, method, or `RuntimeConfig` field mentions time
control. In an unpaused context every time read above returns what the
platform monotonic clock returns — observably unchanged from before
this RFC, and still observably unchanged after the §5.1 feature flip,
which adds a clock-context check on the read path but no observable
difference. There is nothing to migrate, configure, or opt into for
any existing application.

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

## 4. Implementation deliverables

Both deliverables below are owned by this RFC's implementation task.

### 4.1 HTTP time-source migration (prerequisite)

**Named prerequisite.**
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

### 4.2 Timer contract alignment

`src/subscription/time.rs`'s `Timer` does not currently satisfy the
non-catch-up contract (§3.2, INV-C3): it leans on Tokio's
`MissedTickBehavior::Skip`, whose skip engages only once a tick is late
by more than a fixed margin (5 ms in tokio 1.50.0's `interval.rs`), so a
short-interval timer replays one tick per missed interval after a delay.
This deliverable changes `Timer` to provide the non-catch-up property
directly and pins the semantics in rustdoc. Unlike §4.1, it is **not**
behavior-preserving.

- **Observable change.** After a delay spanning several intervals,
  `Timer` yields exactly one tick — no catch-up burst — instead of one
  tick per missed interval. This is a user-visible behavior change for
  short-interval timers under load, so it lands with a `CHANGELOG: Fixed`
  entry, not as a mere rustdoc adjustment.
- **Anchor.** The time anchor is fixed at **stream construction** — the
  moment `Timer::stream()` is called (`interval()` in
  `src/subscription/time.rs`), not `Timer::new`, which only stores the
  interval value. Time that passes between `Timer::new` and `stream()`
  does not count against the timer; the first `next_deadline` is one
  full interval after the `stream()` call.
- **Pinned observable properties**, stated over a running `next_deadline`
  (independent of the implementation mechanism, so a from-scratch
  reimplementation that drops `.skip(1)` or `interval` altogether still
  conforms):
  - `next_deadline` starts one full interval after the stream-construction
    anchor. The construction-instant tick is never delivered.
  - While virtual now `<` `next_deadline`, no tick is deliverable. Once
    now `>=` `next_deadline`, **exactly one** tick becomes deliverable —
    however many interval boundaries lie between the old `next_deadline`
    and now, never a burst — and after it is taken, no further tick is
    deliverable until now reaches the new `next_deadline`. This is a
    count-and-cadence claim about *how many* ticks are deliverable, not
    about *which* poll observes them: the readiness-poll timing follows
    §3.2's general guarantee (no wall-clock wait once the executor
    progresses; the observing poll is not fixed), and `Timer` claims no
    stronger same-poll guarantee than any other gated behavior.
  - **Post-miss cadence.** Delivering a tick advances `next_deadline` to
    the first anchor-phase boundary strictly after the current now (the
    phase is preserved — Skip-style alignment — not reset to
    "now + interval", Delay-style). This is deadline-relative: after a
    late tick, the next boundary can be less than one interval away and
    still fires. It preserves the drift-corrected cadence the `Timer`
    rustdoc describes.
- **rustdoc.** `Timer` gains a first-tick, non-catch-up, and post-miss
  cadence sentence citing this RFC as the semantics of record, and names
  the stream-construction anchor.
- **Tests.** INV-C3's paused-clock tests at 1 ms and 2 ms intervals are
  the proof; a Skip-margin-dependent implementation fails them.

## 5. Consumers

### 5.1 TestStore stage 2 (RFC 0008 §7 amendment)

The stage-2 amendment gives the store a controlled time context and an
advance operation, making RFC 0008 §4.3-class *command* time leaves
(`timeout`, retry backoff) deliverable through ordinary `receive` flow.
`Timer` is deliberately excluded: it is a subscription source, not a
command leaf, and TestStore never executes subscription sources
(RFC 0008 §1.2), so a `Timer` leaf never enters the store's pending
command set at all. Delivering it would require a subscription-execution
design — source spawn, ID reconciliation, restart, and source
cancellation, everything the runtime's `SubscriptionManager` provides —
which is out of scope for both RFCs; if a future need arises it is a
separate, explicitly-designed amendment, not this stage-2 one.
`Timer`'s own determinism under the virtual clock is still pinned here
(INV-C3), exercised directly against its stream rather than through
TestStore. RFC 0008 §7's
non-normative sketch pictured "a store-held clock handle"; this RFC's
design supersedes that shape — there is no clock value to hold. The
store holds a controlled time context (§3.2) and `advance` acts on that
context's clock; the amendment specifies its API against this RFC, not
against the sketch. What it cites here:
INV-C2's explicit-advance determinism (the store's poll budget never
idles the executor, so the auto-advance clause never applies) and
INV-C3's transparency (store results are evidence about production time
contracts, completing the INV-T3 argument on the time axis).

Three design inputs recorded for that amendment:

- **Deadline anchoring.** RFC 0004 anchors a timeout's deadline at the
  leaf's *first poll*, and the store's scans decide when that first
  poll happens; the amendment's advance semantics must account for
  scan-order-dependent anchoring rather than assuming
  construction-time anchoring. (`Timer`'s stream-construction anchor
  (§4.2) is not a stage-2 concern: stage 2 delivers command time leaves
  only, never `Timer`.)
- **Advance semantics and executor context.** Tokio's `advance` does
  not wait for the sleeps it moves past to complete (§3.2), so the
  amendment must fix whether the store's advance carries a timer-driver
  barrier or whether "ready after advance" means "ready once the
  executor is driven to progress, with no wall-clock wait". It must also
  fix the executor context the store owns or borrows — whether the store
  holds its own current-thread paused runtime or runs inside the
  caller's `#[tokio::test]` paused runtime — and how a user effect that
  itself spawns (`tokio::spawn`) or nests a runtime is handled. Recorded
  here as stage-2 design inputs, not resolved by this RFC.
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

Refetch intervals and failure backoff ride §4.1's migrated module and
this contract. Their jitter, if any, is randomness — out of scope here
(§1.2) and owned by that policy's RFC.

### 5.5 Applications

A downstream application enables the `tokio` `test-util` feature in its
own dev-dependencies and runs its tests on a paused single-threaded
runtime; feature unification makes every tears time read virtual in
those tests with no tears-side configuration. Deliverable: a
deterministic-time section in `docs/testing.md` documenting the recipe
(paused runtime, explicit advance, the §3.2 auto-advance caveat). The
caveat is named concretely: awaiting real network I/O under a paused
runtime lets the executor judge itself idle and auto-advance the clock
to the next timer before the I/O completes, so a test exercising §4.1's
HTTP staleness must drive time and I/O explicitly rather than awaiting a
live socket — the auto-advance-versus-real-I/O gotcha, stated because
§4.1's payoff is precisely HTTP staleness testability. Documentation, not
contract surface.

## 6. Invariants

Enforcement classes follow the pre-review checklist's definitions.

- **INV-C1**: every time read in library code goes through the
  virtualizable clock (`tokio::time`), and none of §3.1's banned entry
  points appears in the repository's Rust targets except the named
  bench exception (§3.1) at its explicitly allowed measurement sites.
  Structural-mechanical: clippy `disallowed-methods` in `clippy.toml`
  carries exactly §3.1's banned-entry-point inventory (landing with the
  §4.1 migration, which removes the last violations), failing the
  workspace lint gate on any reintroduction. The platform-gated `std::os`
  socket timeout setters are carried as `allow-invalid = true` entries
  (§3.1) and so are enforced mechanically wherever CI resolves them —
  the Linux gate covers `std::os::unix`, so every current row is
  enforced and the platform-gated review-half fallback covers an empty
  set today (§3.1). Structural-review for time reads the lint cannot
  mechanically reach — a future target CI never lints, and everything
  outside `std` (a direct syscall, or a dependency other than the
  executor used by library code as a time source, §3.1's dependency
  scoping) — checked at code and dependency review.
  (Adversarial models: virtual sleeps combined
  with `std` now-reads for comparisons — the table's now-read rows;
  wall-clock time reached without `SystemTime::now` via
  `UNIX_EPOCH.elapsed()` — the `SystemTime::elapsed` row exists for it;
  a `std` `Instant` smuggled out of the virtual clock via `into_std`
  and then read — the `Instant::elapsed` row; a libc/syscall clock or a
  time-source dependency beyond the executor — the review half, which
  is why the class is structural rather than purely mechanical.)
- **INV-C2**: in a controlled context, §3.2's advancement rule holds —
  under a non-idling controller, a gated behavior stays pending across
  arbitrarily many polls until an explicit advance reaches its
  deadline, and thereafter becomes ready with no wall-clock waiting once
  the executor is driven to progress. This RFC does not fix which poll
  observes readiness (§3.2's readiness guarantee); the claim is only
  "pending until advanced, ready without wall-clock waiting after".
  Behavioral: a paused-clock test polls a pending `Command::timeout`
  leaf repeatedly without advancing and asserts it remains pending
  (this is the check a compliant implementation passes and an
  implicitly-advancing clock fails), then advances to the deadline and
  asserts the behavior becomes ready, with no wall-clock waiting, once
  the executor is driven to progress.
- **INV-C3**: the time-dependent contracts pinned by RFC 0004 and by
  `Timer`'s semantics hold identically under the virtual clock.
  `Timer`'s current rustdoc documents only the missed-tick Skip
  behavior, and the current implementation does not in fact provide the
  non-catch-up contract at short intervals: it composes
  `MissedTickBehavior::Skip` with `.skip(1)`, but Tokio's Skip engages
  only once a tick is late by more than a fixed margin (5 ms in
  tokio 1.50.0's `interval.rs`), so a paused-clock advance spanning
  several sub-margin intervals replays one tick per interval — a
  catch-up burst this contract forbids. `.skip(1)` supplies only the
  first-tick timing (no tick at the construction instant); it does not
  suppress catch-up. The §4.2 implementation task pins the contract by
  making `Timer` provide the non-catch-up property directly (rather than
  leaning on Tokio's lateness-margin Skip) and adds a first-tick,
  non-catch-up, and post-miss cadence sentence to `Timer`'s rustdoc
  citing this RFC as the semantics of record, so INV-C3 references a
  written contract the code actually satisfies rather than an
  implementation accident it does not. The contract is stated as
  observable properties (§4.2), not as a claim about `.skip(1)` or any
  other mechanism, so the implementation is free to drop them.
  Behavioral: the existing paused-clock timeout/retry suites
  (`src/command/effect.rs`, `src/command/retry.rs`, `src/runtime.rs`)
  are the timeout/retry half; new paused-clock `Timer` tests pin the
  timer half at short intervals (1 ms and 2 ms, chosen to fall inside
  Tokio's lateness margin so a Skip-dependent implementation fails them),
  covering the §4.2 observable properties:
  (a) no tick ready while now is before the first `next_deadline` (one
  interval after the stream-construction anchor);
  (b) the first tick becoming ready once now reaches that deadline;
  (c) a single advance past several interval boundaries making exactly
  one tick ready, not a burst, followed by Pending until the new
  deadline, whether it is the first poll or a later one (the test drives
  the executor to observe readiness — it asserts tick count and the
  following Pending, not the specific poll index, consistent with §3.2);
  (d) **post-miss cadence** — after a late tick delivered at, say,
  now = 3.5 ms on a 1 ms timer, the next tick fires at 4 ms (the first
  anchor-phase boundary strictly after now, only 0.5 ms later), *not* at
  4.5 ms; a Delay-style "now + interval" reset fails this, and a
  gap-length rule that suppressed the sub-interval boundary would too;
  and
  (e) **anchor** — advancing virtual time before `stream()` is called
  does not consume the first interval: the first deadline is
  `stream_time + interval` (later in absolute time for a later `stream()`
  call), because the anchor is the `stream()` call, not `Timer::new`,
  which stores only the interval. The
  existing wide-margin real-time `Timer` tests remain as non-normative
  smoke checks; the paused tests are the contract's proof.
- **INV-C4**: production neutrality — the crate exposes no time-control
  surface and the production runtime never pauses or configures the
  clock, before and after the §5.1 feature flip. Structural: review of
  `Runtime` construction and `RuntimeConfig` for the absence of any
  clock field, and the public-surface check (`tests/api_surface.rs`)
  showing no time-control item. The flip's "observably identical" claim
  rests on tokio's never-paused fast path (the read path adds one
  atomic load), but the runtime reads virtual now per iteration in hot
  loops (e.g. the micro-batch window in `src/runtime.rs`), so the §5.1
  implementation task carries a load-path regression check rather than
  a bespoke wall-clock comparison: RFC 0006's acceptance criteria must
  continue to pass on its reference machine after the flip, under
  RFC 0006's own measurement conditions and gating. That reuses an
  apparatus with a defined pass/fail and environment scope instead of
  reading "observably free" off a two-run criterion difference. The §4
  migration's
  behavior-preservation half is structural too: the diff is a type
  swap with no logic change, and the existing HTTP suite stays green.

Surface–invariant coverage: this RFC adds no public API surface. Its
contract surface is the source rule (INV-C1), the controlled-context
guarantees (INV-C2, INV-C3), the neutrality guarantee and the §5.1
feature flip (INV-C4), the §4.1 migration (INV-C1 turn-on plus
INV-C4's preservation check), and the §4.2 `Timer` contract-alignment
fix (INV-C3's timer half, plus the `CHANGELOG: Fixed` entry for its
observable behavior change). The `docs/testing.md` recipe (§5.5) is
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
  the §4.1 migration sites.
- `benches/runtime_load.rs` — the named real-time exception.
- `clippy.toml` — INV-C1's mechanical enforcement site.
- `docs/testing.md` — the paused-time testing conventions §5.5 extends.
- `docs/rfcs/pre-review-checklist.md` — enforcement-class definitions.
