# RFC 0006: Runtime Load Control

- Status: Accepted (moves to Implemented when the sections 4–6
  implementation lands)
- Target: release-gate decision for 0.10.0 (section 3); implementation after
  0.10.0 (additive)
- Scope: bounded memory, backpressure, and latency behavior of the runtime
  under load; the delivery contract of every runtime-owned channel
- Feature flag: none
- CHANGELOG: `Added` entry lands at the load-control implementation release
  (opt-in `RuntimeConfig`); 0.10.0 itself needs no CHANGELOG entry for this
  RFC (see section 3)

> **Decision scope.** Section 3 (the release-gate verdict) is the part of this
> RFC that 0.10.0 depends on, and it is final unless the contract design in
> sections 4–6 turns out to be unimplementable without breaking the public
> API. Sections 4–6 fix the load-control contract. The items this RFC
> delegated to the separate `RuntimeConfig` RFC — capacity values,
> recommended defaults, the section 5.1 bounded-run parameters, and the CI
> smoke-profile question — are settled there; that RFC is Accepted, so no
> prerequisite of the implementation PR remains open (sections 3.2, 6). The
> three contracts flagged for settling before the post-0.10.0
> implementation — the exact scope of the memory bound (INV-L1), the quit
> delivery path, and cross-channel fairness — are settled (open
> questions 3, 7, and 6; sections 4.5, 4.6, and 4.7), as are the INV-L4
> acceptance formulation, the `batch_max_messages` semantics, and the
> observability schema (sections 4.1, 4.4, 5). None of them gated the 0.10.0
> release.

## Summary

Every runtime-owned channel is unbounded and every sender completes
immediately. Under sustained overload — producers faster than the update loop
— pending messages, memory, and input-to-screen latency all grow without
bound, and outputs of keyed (cancellable) commands are starved for as long as
the shared queue stays non-empty. Full-loop measurements
(`benches/runtime_load.rs`) confirm each of these failure modes and also
confirm that the runtime behaves well within capacity: bounded queues, sub-ms
latencies, and a stable frame rate, with bursts absorbed and drained.

This RFC defines the load-control contract in two steps:

1. **0.10.0 release gate (decided here).** Load control can be delivered as an
   opt-in, additive `RuntimeConfig`; the default delivery semantics and the
   existing constructors do not change. This RFC therefore adds **no
   breaking change** to 0.10.0, and 0.10.0 does not wait for this RFC's
   implementation.
2. **Post-0.10.0 implementation (direction fixed).** A bounded
   delivery mode with per-source-class capacity and backpressure contracts,
   configured through `RuntimeConfig`, plus load observability via `tracing`.

## 1. Context and constraints

### 1.1 Channel and scheduling inventory

The runtime owns these channels and scheduling mechanisms today:

| Input | Channel | Capacity | Sender behavior |
| --- | --- | --- | --- |
| Shared app messages | one `mpsc::unbounded_channel` | unbounded | `send` never waits; errors only after shutdown |
| Subscription output | forwarded into the shared channel by one task per subscription | unbounded | forwarding task polls the source stream as fast as `send` allows, i.e. unpaced |
| Unkeyed command output (`Action::Message`) | sent into the shared channel by the command task | unbounded | same |
| Keyed command output | one private `mpsc::unbounded_channel` per `CommandId` | unbounded | same |
| Quit (`Action::Quit`) | dedicated `mpsc::unbounded_channel` | unbounded | same |

`Action::Quit` never enters the "Unkeyed command output" row above: for an
unkeyed command, the task sends `Action::Quit` straight to the dedicated
quit channel, never to the shared channel, regardless of source (including
a user-initiated quit returned from `update`). A keyed command's
`Action::Quit`, by contrast, is sent into that command's own private
channel like any other keyed output (section 4.2).

Scheduling facts that interact with load:

- The event loop is an unbiased `tokio::select!` over app inputs, the frame
  tick, and quit. The frame branch is gated on pending work and paced by the
  frame interval (`MissedTickBehavior::Skip`); the quit branch is always
  armed.
- Message micro-batching drains additional ready inputs for up to 100µs after
  the first one. The window is time-capped but not count-capped.
- `AppInputs` polls the shared receiver before keyed receivers at every pull
  point. This is RFC 0003's INV-14: a ready shared cancel message must be able
  to suppress a ready keyed output before delivery. RFC 0003 already notes
  that a continuous stream of ready shared inputs can delay keyed delivery;
  section 2 quantifies how far that goes under overload.
- Subscription forwarding applies no pacing of its own: a source stream that
  is always ready is polled in a tight send loop on a worker task.

### 1.2 Requirements carried into the contract

- **R1**: Memory used by each pending *app-facing* runtime-owned channel
  buffer — the shared app channel and each keyed command channel — must be
  boundable by configuration. The dedicated quit channel is deliberately
  outside R1: it is R4's unbounded exception — quit signals must never
  participate in backpressure — so it is never bounded (section 4.1), does
  not appear in the buffer total below, and R1 makes no occupancy claim
  for it. The bound is per channel, not aggregate:
  `RuntimeConfig` alone bounds the shared channel and each keyed channel
  individually, and the buffer total is `app_channel_capacity + m ×
  keyed_channel_capacity`, where `m` — the number of active `CommandId`s —
  is a contract input the application must bound, not something
  `RuntimeConfig` controls (it is one component of the producer-count
  premise, section 4.5). "Active" here means an entry exists in the keyed
  map, including a finished-but-undrained run: a keyed channel outlives
  its command's completion until its buffered output is drained and the
  id released for retry (RFC 0005,
  `delivering_the_closed_senders_last_item_releases_the_id_for_retry`), so
  such a run still counts toward `m` and still occupies its
  `keyed_channel_capacity` share. Nor is this a bound on all pending-work memory:
  each producer blocked awaiting channel capacity additionally holds one
  in-flight message outside the channel, and the number of concurrent
  producers (active subscriptions, running commands) is likewise controlled
  by the application. A full memory bound also requires bounding producer
  count, which this RFC's channel-capacity controls do not deliver on their
  own; open question 3 resolved this by keeping R1 scoped as stated —
  per-channel bounds from configuration, with the producer-count premise
  (including `m`) application-owned and observable (INV-L1, section 4.5).
- **R2**: A configured bound must not silently drop messages by default;
  backpressure (slowing the producer) is the default overload response.
- **R3**: Input-to-screen latency under load must be observable and, with a
  bounded configuration, bounded by queue capacity rather than by overload
  duration — given a bounded number of concurrent producers, the
  application-owned premise INV-L3 depends on (open question 3, resolved in
  section 4.5).
- **R4**: A quit signal already in the dedicated quit channel must be
  delivered with latency independent of app backlog (the dedicated channel
  and its always-armed select branch must remain — see INV-L4 for the
  current implementation's fairness caveat on "always-armed"). Every
  unkeyed `Action::Quit` — including a user-initiated quit that `update`
  returns as `Command::quit()` — is sent directly to this dedicated
  channel, never through the shared channel, so its *delivery* is already
  covered by R4 today, unaffected by `app_channel_capacity` in both the
  unbounded default and the future bounded mode. What R4 does not cover is
  the time for an unkeyed command's task to *reach* a quit that follows
  earlier `Action::Message` items in the same stream: the task processes
  its stream strictly in order, so it cannot dispatch the quit until each
  earlier item's send has completed, and in bounded mode those earlier
  sends can wait on shared channel capacity. A keyed `Action::Quit` is
  different again — it travels through its command's private channel like
  any other keyed output — and follows that channel's delivery semantics
  (section 4.2).
- **R5**: RFC 0003's cancellation and ordering invariants
  (cancel-before-delivery, INV-14 shared-first pull, INV-10 one-item
  drain) must be preserved or explicitly amended. Keyed-quit ordering —
  a keyed run's `Action::Quit` is delivered only after the same run's
  earlier outputs, which RFC 0003's model assumes but never states as an
  invariant — is pinned by this RFC as INV-L10 (section 5) and preserved
  under the same rule. An unkeyed quit has no such delivery order: it
  travels the dedicated channel while the same command's earlier messages
  travel the shared channel, so it can be observed first (section 4.2) —
  that asymmetry is existing behavior and stays outside every ordering
  claim in this RFC.
- **R6**: The default (unconfigured) behavior of existing applications must
  not change in 0.10.0.

## 2. Measurements

Full-loop load characteristics were measured with `benches/runtime_load.rs`,
which drives the real `Runtime` through the public API: a subscription floods
the shared channel at a configured rate; `update` and `view` simulate CPU cost
(2–25µs and 500µs respectively); a keyed command emits probe messages every
25ms in the `keyed_*` scenarios. Reference run: Apple M1 Max (10 cores),
rustc 1.97.0, `cargo bench --bench runtime_load`, 60 FPS target; measured
2026-07-17 — the reference date this section's measured results, and the
section 5.1 baseline that reuses them, are defined on.

| Scenario | Load | Max queue depth | Update latency p50 / p99 | Render latency p50 / p99 | Keyed latency p50 / max | FPS |
| --- | --- | --- | --- | --- | --- | --- |
| `steady_20k` | 20k/s for 5s, 2µs update | 140 | 0.04ms / 0.77ms | 0.60ms / 2.3ms | — | 60 |
| `steady_200k` | 200k/s for 5s, 2µs update | 400 | 0.26ms / 1.2ms | 0.92ms / 1.7ms | — | 60 |
| `burst_200k` | 200k at t=0, 2µs update | 193,221 | 212ms / 421ms | 210ms / 422ms | — | 61 |
| `overload` | 100k/s for 5s vs ~38k/s capacity, 25µs update | 307,869 | 4.0s / 7.9s | 4.0s / 7.9s | — | 60 |
| `keyed_steady` | as `steady_20k` + keyed probe | 680 | 0.05ms / 2.2ms | 0.63ms / 8.9ms | 0.10ms / 5.2ms | 60 |
| `keyed_overload` | as `overload` + keyed probe | 308,733 | 4.0s / 7.9s | 4.0s / 7.9s | **9.2s / 13.0s** | 60 |

Quit responsiveness under backlog is measured by the `quit_*` scenarios
(added for open question 8). Each scenario runs many short trials
— the event loop's `select!` is unbiased, so quit latency is a distribution
and only tail statistics across trials are meaningful. A trial floods the
shared channel (burst or paced), then `update` returns `Command::quit()`
while the backlog is still deep, mirroring a real quit key press: the
unkeyed variant reaches the dedicated quit channel through its command task
(section 1.1); the keyed variant travels its command's private channel
instead (section 4.2). Two per-trial values are recorded: **quit→delivered**
(quit request to the event loop's quit branch observing it — the harness
installs a process-global `tracing` subscriber and timestamps the runtime's
own "quit signal received" / "keyed quit signal received" debug events on
the `tears::runtime` target, so this is the delivery instant itself, not a
bound, and is usable as an acceptance measurement) and **quit→exit** (to
`run()` returning, which adds teardown, including backlog deallocation that
scales with depth and must not be misread as delivery).

| Scenario | Load at quit | Trials | Depth at quit | Quit→delivered p50 / p99 / max | Quit→exit p50 / max |
| --- | --- | --- | --- | --- | --- |
| `quit_idle` | none | 200 | 0 | 0.59ms / 0.71ms / 1.9ms | 0.60ms / 1.9ms |
| `quit_backlog_50k` | draining burst | 200 | ~50k | 0.11ms / 0.49ms / 0.64ms | 0.58ms / 1.1ms |
| `quit_backlog_300k` | draining burst | 200 | ~300k | 0.11ms / 0.61ms / 0.61ms | 2.9ms / 3.4ms |
| `quit_overload` | producer at 100k/s | 200 | ~8k | 0.11ms / 0.62ms / 0.84ms | 0.19ms / 0.92ms |
| `quit_keyed_backlog_50k` | draining burst | 20 | ~50k | 1300ms / 1302ms / 1302ms | 1300ms / 1302ms |

Findings:

- **F1 — In-capacity behavior is healthy.** At up to 200k msg/s within
  consumer capacity, queue depth stays bounded (≤400), latency stays around a
  frame period, and the frame rate holds. No contract change is needed for
  the in-capacity regime.
- **F2 — Bursts are absorbed, then drained.** A 200k-message burst peaks at
  ~4.4MiB of backlog and drains in 0.43s. The unbounded queue is what makes
  this absorption free of coordination; a bounded contract must state what
  replaces it (capacity sized for expected bursts, or backpressure into the
  source).
- **F3 — Sustained overload grows without bound.** With the producer ~2.6×
  consumer capacity, queue depth grows linearly (~62k messages per overload
  second, ~7MiB in 5s) and input-to-screen latency reaches 8s, bounded only
  by how long the overload lasts. Rendering continues at 60 FPS, so the UI
  looks alive while showing state that is seconds old. This is the failure
  mode R1–R3 target.
- **F4 — Keyed outputs starve while the shared queue is non-empty.** Under
  shared overload, keyed probe delivery is deferred until the shared backlog
  fully drains (p50 9.2s, max ≈ the run's full wall time), versus p50 0.1ms
  in the steady control. This is the INV-14 shared-first bias taken to its
  limit, and RFC 0003 already states that bounded fairness is not
  guaranteed. A bounded shared queue does *not* bound this delay: every
  message the loop pulls lets a waiting producer refill the queue, so the
  shared channel can stay continuously ready for as long as overload lasts,
  even with a capacity of 1. Capacity controls backlog and memory (F3);
  bounded keyed-delivery latency would need a scheduling policy of its own
  (section 4.3) — open question 6 has since resolved against adding one
  (section 4.7).
- **F5 — The frame branch survives overload.** The unbiased select plus the
  100µs batch cap kept the frame branch at 60 FPS through every overload
  scenario; no structural event-loop change is required for frame
  scheduling. Quit responsiveness under backlog is measured separately by
  the `quit_*` trial scenarios above (F6, F7), added for open question 8.
- **F6 — Unkeyed quit delivery is backlog-independent under the unbiased
  select.** Across 600 loaded trials at depths from ~8k to ~300k,
  quit→delivered stays at p50 0.11ms, p99 ≤ 0.62ms, max ≤ 0.84ms, with no
  separation between the 50k and 300k depths and none between a draining
  burst and an actively refilling producer. The idle baseline is *higher* at
  p50 (0.59ms) because the quit request also marks a redraw and one 500µs
  `view` usually runs before the quit branch wins; under load that render is
  amortized into work already happening. The cost of a lost unbiased
  tie-break is therefore a few micro-batches, not a function of queue depth.
  Quit→exit *does* scale with depth (p50 0.58ms at 50k vs 2.9ms at 300k) —
  that is shutdown-time backlog deallocation, not delivery. This is the
  measurement basis INV-L4 was waiting for: the statistical formulation
  (INV-L4 option (b)) is already satisfiable by the current implementation,
  while a deterministic quit priority (option (a)) would buy a hard per-run
  bound, not depth-independence, which the data shows the unbiased select
  provides on its own.
- **F7 — A keyed quit waits for the full shared drain.** With ~50k queued,
  the keyed variant's quit→delivered is p50 1.300s — the full drain of the
  remaining backlog at ~26µs per message — with spread across 20 trials
  under 0.2%. This is F4's shared-first starvation applied to quit: delivery
  scales linearly with backlog depth. It quantifies the status quo that
  open question 7 resolved to keep: the wait is cancel-before-delivery
  holding under load — any pending shared input could cancel the quit's
  command, and under the unbounded default every one of them is already
  *ready* in the channel, so shared-first pull processes them all before
  delivering it — not incidental starvation (section 4.6). The full-drain
  wait is specific to the unbounded default; in bounded mode keyed-quit
  delivery is capacity-dependent (section 5.1).

## 3. Release-gate decision for 0.10.0

Two questions decide whether this RFC forces breaking changes into 0.10.0.

### 3.1 Does the default send contract change?

**No.** The measured failure modes (F3, F4) appear only under sustained
overload; the in-capacity and burst regimes (F1, F2) — which are what typical
TUI applications inhabit — behave well with the current unbounded,
never-waiting senders. Changing the default to bounded delivery would impose
new deadlock and tuning considerations on every existing application to fix a
regime most of them never enter. The default therefore stays: unbounded
channels, senders never wait, no message loss before shutdown.

Bounded delivery ships as an explicit opt-in. If field experience (e.g.
long-running dashboards with hot subscriptions) later argues for a bounded
default, that is a deliberate pre-1.0 breaking change for a later minor
release, made with migration notes — not a 0.10.0 gate.

### 3.2 Does the opt-in fit the existing public API additively?

**Yes.** The opt-in surface is a new `RuntimeConfig` consumed by a new
constructor `Runtime::with_config(flags, config)`, with `Runtime::new`
unchanged and equivalent to the default configuration. The exact public
shape — constructor signature, field set, naming, and
construction/validation style — is fixed by the separate `RuntimeConfig`
RFC (Accepted; section 6), so the load-control implementation PR can
start. The verdict here needs only the additivity argument, which holds
for any shape that keeps `Runtime::new` unchanged. Bounded behavior
activates only through that surface:

- Capacity limits replace the unbounded channels inside the runtime; no
  public channel type is exposed today, so this is internal. The one
  place a channel type appears in a `pub` signature —
  `BenchSubscriptionManager::new` takes the shared
  `mpsc::UnboundedSender` — is `bench-internals`-gated, `#[doc(hidden)]`,
  and explicitly outside semver; the implementation updates that wrapper
  (and `benches/subscription.rs`) alongside `SubscriptionManager`.
- Backpressure applies inside the runtime-owned forwarding and command tasks:
  an awaiting `send` stops the task from polling its source stream or command
  stream until the consumer catches up. `SubscriptionSource::stream` and
  `Command` contracts already permit arbitrary poll pacing, so no trait or
  type changes.
- A micro-batch count cap is internal scheduling policy, observable only as
  performance.

Nothing in the current public API documents unbounded buffering as a
guarantee, so offering a bounded mode contradicts no published contract.

### 3.3 Verdict

**This RFC requires no additional breaking change in 0.10.0.** The 0.10.0
release proceeds with the breaking changes already on `main`; the
load-control implementation (sections 4–6) lands after 0.10.0 behind
`RuntimeConfig`.

## 4. Contract design (post-0.10.0, direction fixed)

### 4.1 Configuration surface

`RuntimeConfig` — public shape fixed by the separate `RuntimeConfig` RFC
(Accepted; section 3.2) — carries the three controls below. That RFC
adopts these names unchanged and groups the frame rate into the same
config, so `Runtime::with_config(flags, config)` takes the frame rate
inside `config`; the semantics stated here are the contract.

- `app_channel_capacity: Option<NonZeroUsize>` — `None` (default) keeps the
  unbounded shared channel; `Some(n)` bounds it.
- `keyed_channel_capacity: Option<NonZeroUsize>` — same for each keyed
  command's private channel.
- `batch_max_messages: Option<NonZeroUsize>` — count cap for one micro-batch
  window, complementing the 100µs time cap. The counted unit is a *pulled
  input*: every input the batch takes from `AppInputs` counts toward the
  cap — the input that opened the batch counts as the first, and inputs
  that do not invoke `update` (`ReceiverEvent::Closed`) count like any
  other, because an input's kind is known only after pulling it. `Some(n)`
  therefore lets the batch loop drain at most `n - 1` inputs after the
  first; the cap is not the `update`-invocation count the batch loop
  already tracks — the two differ by exactly the pulled inputs that do
  not invoke `update`. A batch ends at whichever cap is reached first
  (count or the 100µs window), or earlier when no input is ready or a
  quit is pulled. `Some(1)` degenerates to one input per batch; `None`
  (default) keeps the time-capped-only loop (INV-L6). Pinned as INV-L12
  with its unit-level check (section 5).

The dedicated quit channel is never bounded (R4); signals in it are rare and
must not participate in backpressure.

### 4.2 Backpressure semantics per source class

One channel policy is not applied mechanically to all inputs. The bounded
mode's send contract, by source class:

- **Subscriptions (Timer, TerminalEvents, signals, WebSocket, Query,
  user sources)**: the forwarding task awaits channel capacity. Backpressure
  propagates by not polling the source stream. Sources whose upstream cannot
  pause (e.g. a WebSocket peer) buffer or shed in source-specific policy —
  that is a separate debounce/throttle task and source-level concerns, not
  the runtime's. Terminal input is a subscription like any other; awaiting
  simply defers reading further input events, which is the desired behavior
  (typing ahead of an overloaded app queues in the terminal, not in
  unbounded memory).
- **Commands (keyed and unkeyed)**: the command task awaits capacity before
  its next send. Command streams are already async pull; no API change.
- **Quit**: the dedicated quit channel is never blocked and never dropped,
  and every unkeyed `Action::Quit` — including a user-initiated quit that
  `update` returns as `Command::quit()` — is sent directly into it (section
  1.1), so its *delivery* is already independent of `app_channel_capacity`
  today, before any bounded-mode change (R4). What bounded mode changes is
  only how long an unkeyed command's task takes to *reach* a quit that
  follows earlier `Action::Message` items in the same stream: the task
  consumes the stream strictly in order and cannot dispatch the quit until
  each earlier item's send completes, so a quit behind a backlog of earlier
  sends in the same stream waits on that backlog, not on the quit channel
  itself. A keyed `Action::Quit` is delivered through its command's private
  channel instead, so in bounded mode it can wait on
  `keyed_channel_capacity` like any other keyed output, and is additionally
  subject to INV-14 shared-first pull once delivered (keyed quit is already
  subject to that ordering today). Open question 7 asked whether keyed /
  in-stream quit should instead be re-routed to the dedicated channel —
  matching what unkeyed `Action::Quit` already does — and resolved that it
  stays in the private channel: that channel's delivery order is what keeps
  a keyed quit cancellable, and a reroute in either shape would break
  buffered-quit suppression (RFC 0003 INV-9) and the keyed-quit ordering
  and shared-first precedence pinned as INV-L10/INV-L11
  (section 4.6). User-initiated quit
  itself was never part of that question: as an unkeyed command with no earlier
  actions ahead of it, it already gets R4's independence from
  `app_channel_capacity`; only the input leg (the key press traveling
  through the shared channel to `update`) is shared-channel traffic and
  benefits from INV-L3 in bounded mode.

This source-class split relies on a structural precondition: no send
described above ever originates from the event loop task itself (INV-L7);
otherwise an awaiting send under backpressure would block on progress that
only the event loop can make.

Delivery in bounded mode remains lossless up to shutdown: the runtime never
drops a message to relieve pressure (R2). Lossy strategies (coalescing,
sampling) are source-level policies layered on top (a separate
debounce/throttle task), where the semantics of "which messages may be
merged" are known.

### 4.3 Interaction with existing invariants

- **In-order delivery per source**: unchanged; awaiting a send preserves
  order. The ordering that holds today is scoped per source class, and
  bounded mode keeps each scope as it is: an unkeyed command's or a
  subscription's *messages* are delivered in send order (one task
  sending into the one shared FIFO channel), a keyed run's outputs
  travel its one private FIFO channel, and the keyed-quit form — the
  quit never overtakes the same run's earlier output — is pinned as
  INV-L10 (section 5). An unkeyed `Action::Quit` is outside every one of
  these claims: it travels the dedicated channel while the same
  command's earlier messages travel the shared channel, so the event
  loop can observe it first (sections 1.1, 4.2). RFC 0003's model
  assumes stream-order consumption but states no FIFO invariant.
- **INV-14 shared-first pull**: unchanged, and bounded mode adds no fairness
  guarantee on top of it. A full shared channel is refilled by a waiting
  producer each time the loop pulls one message, so shared readiness — and
  with it F4's keyed starvation — can persist for as long as overload
  lasts, independent of the configured capacity. Bounded capacity controls
  backlog and memory only. Open question 6 asked whether bounded
  keyed-delivery latency should be a goal and resolved that it is not: no
  fairness policy is added — not in the initial bounded mode and not later
  under this contract — because any policy that guarantees a
  keyed-delivery bound must deliver keyed output while ready shared
  inputs, the class that can still cancel it, remain queued (section 4.7).
- **Cancellation**: cancel-before-delivery and buffered-output suppression
  (RFC 0003) hold exactly as stated, and a keyed producer blocked on a full
  private channel is aborted exactly like a running one — INV-14's literal
  guarantee (a *ready* shared cancel message suppresses a *ready* keyed
  output before delivery) is unchanged. Bounded mode does narrow the
  guarantee's practical reach, though. With unbounded channels `send`
  completes immediately, so a cancelling input that has occurred is, for all
  practical purposes, already sitting in the shared queue and visible to the
  shared-first pull. With bounded channels, the forwarding task carrying
  that input can itself be waiting for admission — outside the queue — for
  as long as the shared channel stays full, and a keyed output racing during
  that admission window can still be delivered before the cancel arrives.
  This is not an INV-14 violation (the input was never "ready" in the
  channel), but it does mean RFC 0003's practical goal of prompt
  cancellation is weaker in bounded mode than under the current unbounded
  default.
- **Redraw suppression** (RFC 0002) and subscription re-evaluation gating:
  unchanged; they operate downstream of input delivery.
- **Shutdown**: bounded channels close exactly like unbounded ones; senders
  blocked in `send` observe the closed channel and terminate their tasks.

### 4.4 Observability (open question 4 resolved)

Load observability is delivered through `tracing` only in the initial
implementation (open question 4, resolved): tears already
depends on `tracing` unconditionally, a subscriber can derive rates and
high-water marks from raw events, and dedicated profiling-hook counters
remain future additive work that this schema does not preclude. The
minimal schema below is normative — the implementation must emit these
events with these targets, levels, fields, and firing conditions, in both
delivery modes except where marked bounded-only; renaming or dropping any
of them is a contract change. The schema and its verification are pinned
as INV-L13 (section 5):

- **Batch event** — target `tears::runtime::load`, level `trace`, fired
  once per completed micro-batch (a quit-terminated batch fires nothing —
  the loop exits instead, matching the current early return). Fields:
  `pulled` (inputs taken from `AppInputs` in this batch, the opening
  input included — INV-L12's counted unit), `updated` (how many of them
  invoked `update` — the count the current `tears::runtime` batch trace
  reports), and `shared_pending` (shared-channel occupancy when the batch
  ends, read from the receiver). Queue-depth high-water marks are derived
  by the subscriber from `shared_pending`, not tracked by the runtime.
  This event subsumes the existing "processed message batch" trace event,
  which moves to this target and gains the new fields.
- **Capacity-wait event** — target `tears::runtime::load`, level `debug`,
  bounded mode only: fired once per send that had to await capacity, at
  the moment the send completes. Fields: `channel` (`"shared"` or
  `"keyed"`) and `wait_us` (time from the first unready attempt to
  acceptance). Sends accepted immediately fire nothing — in-capacity
  operation stays silent at `debug`. Per-send firing is a deliberate
  choice, not a placeholder: sustained overload can emit tens of
  thousands of these events per second, and that volume is expected to be
  managed by `tracing` filtering (level or target), not by the runtime
  aggregating internally. Because the schema is pinned as INV-L13,
  collapsing this into a periodic aggregate later is a breaking change to
  this event's firing condition; adding a separate aggregate event
  alongside it is additive and does not require revisiting this choice.
- **Producer gauges** — target `tears::runtime::load`, level `debug`,
  fired whenever any counted value changes. Fields: `subscriptions`
  (active forwarding tasks), `unkeyed_commands` (running unkeyed command
  tasks), `keyed_commands` (active keyed entries), and `blocked`
  (producers currently awaiting capacity; always 0 in unbounded mode).
  These gauges are how the application-owned producer-count premise stays
  observable rather than enforced (section 4.5), including the
  blocked-producer anti-pattern named there.

Definition of done for the observability slice: a runtime batch-layer
test together with an integration test, each installing a `tracing`
subscriber (the technique the `quit_*` harness already uses, section 2).
The runtime batch-layer test drives `process_input_batch` directly to fix
the batch event's `Closed` differing-value case deterministically (below);
the integration test drives a scripted load for the remaining values —
the equal-count batch case, `shared_pending`, the capacity-wait event, and
the producer gauges. Both verify values and firing conditions, not mere
event presence — an implementation that emits each event once with wrong
values must fail:

- **Batch event**: `pulled` and `updated` equal the scripted input
  counts, including a batch where the two differ. The differing case is
  observed at the runtime batch layer rather than over the integration
  load: a `tracing` subscriber over a `Some(1)` batch whose opening input
  is a `ReceiverEvent::Closed` must observe `pulled = 1` and
  `updated = 0`. This is the opening-`Closed` placement INV-L12 uses and
  for the same reason — a queued `Closed` is not deterministically
  constructible (section 4.7 shared-first pull; `StreamMap` order), so
  requiring a real `Closed` mid-load would reintroduce that
  non-determinism in the observability slice. `shared_pending` is
  verified against a scripted leftover, not merely present: with
  `batch_max_messages = Some(n)` and `n + k` shared messages queued under
  the paused clock, the capped batch must report `shared_pending = k`.
- **Capacity-wait event**: an immediately accepted send fires no event;
  a send that had to await capacity fires exactly one event, at
  acceptance, with `channel` naming the channel that blocked it.
  `wait_us` is verified against a controlled wait: a send held blocked
  while the test advances the paused clock by a scripted duration
  before freeing capacity must report `wait_us` of at least that
  duration — which requires the wait to be measured with
  `tokio::time::Instant`, the pausable clock the batch deadline already
  uses — so an implementation that hardcodes `wait_us = 0` fails.
- **Producer gauges**: starting a subscription, an unkeyed command, and
  a keyed command each raise the matching field, and each completion
  lowers it again; `blocked` rises when a producer begins awaiting
  capacity, falls when the send is accepted, and also falls when a
  blocked producer is aborted by cancellation (section 4.3) — the
  decrement must not depend on the send ever completing.

The bounded run of the section 5.1 matrix records capacity-wait and
blocked-producer numbers from these same events. Per-keyed-channel occupancy gauges are
deliberately outside the minimal schema (emission cost scales with `m`,
the active-`CommandId` count); if field experience needs them, adding
events under this target is additive.

### 4.5 Producer-count premise (open question 3 resolved)

INV-L1 and INV-L3 bound channel buffers and drain-side latency on a
premise: the number of concurrent producers — subscription forwarding
tasks, unkeyed command tasks, keyed command tasks — is bounded. Open
question 3 asked whether the runtime should enforce that premise with an
admission limit, upgrading R1/R3 from channel-buffer claims to
total-pending-work guarantees. The resolution: **no admission limit — the
premise stays with the application, and the runtime makes it observable
instead of enforced.**

An admission limit is structurally unsound in this runtime:

- Every producer is created synchronously on the event loop task. A
  command returned from `update` is dispatched (`enqueue_command`) before
  the loop pulls its next input, and subscription reconciliation spawns
  forwarding tasks inside the frame branch. An admission limit that
  *waits* for a free producer slot would therefore have the event loop
  await capacity that only its own draining can release — the self-deadlock
  INV-L7 exists to prevent, moved from send sites to spawn sites.
- The non-waiting alternatives change contracts this RFC promised to
  preserve. Rejecting a subscription breaks the reconciliation contract
  (a declared subscription runs, including restart of finished ones —
  RFC 0005 and `SubscriptionManager::update`). Rejecting or dropping a
  command silently discards an effect, a strictly worse loss than the
  message loss R2 forbids. Parking un-admitted producers in a queue only
  moves the unbounded buffer up one level, from messages to un-started
  streams and closures, and defers effects for an unbounded time besides.

Leaving the premise to the application is acceptable because producer
count is a property of application structure, not of external load. A hot
source raises the message *rate* through a fixed producer set — that is
exactly what bounded channel capacity controls (F3). Producer *count*
grows only through the application's own choices: the size of
`subscriptions()` and how fast it spawns commands relative to how fast
they complete. This is the same trust class as `update`'s CPU cost, which
the runtime measures but does not bound. One load-coupled amplification
deserves documentation as an anti-pattern: an application that spawns a
command per processed message can, under bounded-mode overload, accumulate
*blocked* command tasks — bounded channels convert message backlog into
blocked-producer backlog for that pattern, and no channel capacity bounds
it. The producer gauges in section 4.4 make that pattern visible; the
recommended-defaults documentation (open question 1) should carry the
anti-pattern note.

Within the same question, keyed channels are bounded **per command**, not
by a shared permit pool. A pool would couple backpressure across
independent `CommandId`s — one hot command's backlog would block unrelated
commands' sends, a producer-side analogue of F4's starvation — and permit
accounting would have to reclaim permits from producers aborted by
cancellation (RFC 0003), coupling two mechanisms this RFC otherwise keeps
orthogonal. A pool also cannot rescue a total bound: the number of active
`CommandId`s is application-owned, so with or without a pool the premise
stays with the application. Per-command capacity matches the
one-private-FIFO-channel-per-run delivery structure and its keyed-quit
ordering contract (INV-L10), at a deliberate cost R1
states explicitly: the keyed buffer
total is `m × keyed_channel_capacity` with `m` — the number of active
`CommandId`s — an application-owned contract input, not a `RuntimeConfig`
knob.

The decision is captured in two invariants: INV-L8 (load control paces
sends, never producer admission) and INV-L9 (keyed backpressure is
isolated per command — the testable form of the no-shared-pool choice).

### 4.6 Keyed quit routing (open question 7 resolved)

A keyed `Action::Quit` is delivered through its command's private channel,
like every other keyed output (sections 1.1, 4.2). Open question 7 asked
whether it should instead be re-routed to the dedicated quit channel at
the producer side — matching what unkeyed `Action::Quit` already does —
in either of two shapes: re-routing once the quit reaches the front of
its own stream, or bypassing stream order for a quit still behind unsent
earlier items. The resolution: **no reroute, in either shape — keyed quit
stays in the private channel.**

The decisive fact is that a keyed quit's delivery order *is* its
cancellation semantics. Keying a quit is the API's way of saying "quit,
unless this command is cancelled or superseded first" (RFC 0003 INV-9: a
cancelled or superseded keyed `Action::Quit` does not quit the
application). An application that wants an unconditional, prompt quit
already has one — unkeyed `Command::quit()` — with measured
backlog-independent delivery (R4, F6). Both delivery behaviors therefore
already exist in the API, chosen per command; a reroute would not add the
fast path, it would remove the cancellable one.

The front-of-stream reroute breaks three delivery properties: the
post-dispatch suppression and shared-first precedence that together make
a keyed quit cancellable, and the keyed-quit ordering that keeps it
behind its own run's earlier output. RFC 0003's stated invariants carry
only the first — INV-9 defines the suppression of an *already*-cancelled
or superseded quit, while INV-14 orders ready inputs at a single
`AppInputs` pull point without saying anything quit-specific or
FIFO-shaped — so this resolution pins the other two as invariants of this
RFC, INV-L10 and INV-L11 (section 5):

- **Post-dispatch suppression (RFC 0003 INV-9).** The dedicated quit
  channel carries no command identity (its payload is `()`), and its
  select branch is always armed. Once a keyed quit is sent into it, no
  later explicit cancel or `CancelInFlight` supersede can suppress it.
  Today suppression works precisely because the quit sits in the keyed
  entry's private channel: cancellation removes the entry and drops the
  buffered `CommandOutput::Quit` with it.
- **Keyed-quit ordering (INV-L10).** At the front of its stream, a
  quit's earlier `Action::Message` items have been *sent* but not
  necessarily *delivered*: under shared backlog they wait behind the
  shared drain (F4) while a dedicated-channel quit would be delivered at
  backlog-independent speed (F6). The quit would overtake its own run's
  earlier output — a keyed stream emitting `[Message(saved), Quit]`
  would exit the application before `saved` reaches `update`. (An
  *unkeyed* `[Message(saved), Quit]` can already be observed in that
  order today — its two actions travel different channels, section 4.2 —
  which is exactly why the keyed ordering is a property of the private
  channel, not of commands in general.)
- **Shared-first precedence (INV-L11).** A rerouted quit arrives on the
  dedicated branch, which the event loop can select while ready shared
  inputs are still queued — and any of those inputs could be the one
  whose `update` cancels the quit's command. Private-channel routing
  instead orders the quit behind every ready shared input at each pull
  point, so those potential cancels run first. Cancelling inputs of
  other classes — ready keyed outputs, inputs not yet admitted — are
  ordered by neither routing (see the identity-carrying variant below).

Adversarial variants considered and excluded:

- **Identity-carrying quit channel** — the quit channel carries
  `(CommandId, RunToken)` and the quit branch checks the keyed entry's
  liveness at delivery. The check restores post-dispatch suppression but
  fails precisely the case that motivates the reroute. Every cancellation
  of a keyed command is dispatched through `enqueue_command` — from the
  init command (before any keyed command exists) or from a command
  returned by `update` — and every `update` invocation consumes an input
  delivered through the shared channel or a keyed channel. Under backlog,
  the input whose `update` would dispatch the cancel can be sitting
  *ready* in the shared channel, unprocessed, and a liveness check at
  quit delivery evaluates state that has not seen it. Ready shared
  inputs are exactly the class of cancelling input the runtime orders
  ahead of a keyed quit — INV-L11 — and the class F7's backlog consists
  of; the reroute delivers the quit ahead of them by construction. For
  the other classes neither routing orders the race: a cancelling input
  arriving through another keyed channel has no guaranteed position
  against the quit inside the keyed `StreamMap`, and an input not yet
  admitted (the bounded-mode window, section 4.3) is invisible to both
  designs. The reroute therefore strictly weakens cancellation
  precedence and strengthens nothing. Under the unbounded default the
  loss is starkest: every pending shared input is already ready, so
  private-channel routing waits for the full drain — F7's 1.30s at ~50k
  backlog is cancel-before-delivery holding under load, not incidental
  starvation — while the rerouted quit would wait for none of it.
- **Reroute only when the private channel is empty** — avoids the
  in-order-delivery violation (INV-L10's ordering half) but not the
  suppression and precedence ones, which are independent of stream
  position; excluded for the same reasons as the identity-carrying
  variant.
- **Drain-then-deliver at the quit branch** — the quit branch, on
  receiving a keyed quit, processes the pending backlog before honoring
  it. Latency-equivalent to the status quo (the backlog is processed
  either way), so it buys nothing and couples the quit branch to app
  input processing.
- **The behind-stream shape** inherits every objection above and adds the
  infeasibility already recorded in open question 7: discovering a quit
  behind unsent items requires look-ahead buffering (unbounded memory,
  contra R1/R2) or an out-of-band signal that abandons the strict
  stream-order consumption RFC 0003's model describes — whose keyed
  delivery-side form is the ordering INV-L10 pins.

Consequences:

- **Two new invariants pin the decision.** INV-L10 (keyed quit is routed
  through, and delivered in order from, its command's private channel)
  and INV-L11 (a ready shared input is delivered before a ready keyed
  quit at every pull point) make the properties this resolution rests on
  explicit and checkable — INV-L5 alone does not carry them, because RFC
  0003's INV-9 covers only post-dispatch suppression (behavioral anchor:
  `cancelling_buffered_quit_suppresses_it`) and states no delivery-FIFO
  or quit-specific precedence invariant. Enforcement classes and checks
  are declared with the invariants (section 5).
- **Bounded mode keeps the contract but not the number.** The no-reroute
  contract (INV-L10/INV-L11) is delivery-order and routing, not latency,
  and holds in both modes. F7's full-drain wait, by contrast, is a
  consequence of the unbounded default, where every pending input is
  already ready in the channel. In bounded mode keyed-quit delivery
  becomes capacity-dependent: every pull hands the freed slot to a woken
  producer whose next send is still in flight, and when that leaves the
  channel momentarily empty — at `capacity = 1`, after every pull — a
  pull point that observes it may deliver the buffered keyed quit while
  producer backlog remains. That execution preserves INV-14, INV-L10,
  and INV-L11 (no *ready* input is outrun) and uses no reroute, so
  bounded-mode keyed-quit latency is a measurement to record, never an
  acceptance bound or a reroute detector (section 5.1).
- **Latency, if ever wanted, goes through the fairness question.** A
  fairness policy that bounds keyed-delivery latency would apply to keyed
  quit as to every other keyed output — and preserving
  cancel-before-delivery is already that question's stated constraint.
  There is no quit-specific delivery path to design. Open question 6 has
  since resolved that question against any policy (section 4.7), which
  also settles the quit-shaped form: reopening either is one and the same
  new RFC.
- **Documentation guidance** (lands with open question 1's
  recommended-defaults documentation): use unkeyed `Command::quit()` for
  a prompt unconditional quit; `.cancellable(id)` on a quit buys
  suppression at the cost of waiting behind pending inputs under load.

### 4.7 Cross-channel fairness (open question 6 resolved)

Under shared overload, keyed delivery waits until the shared channel is
momentarily empty: F4 measured the limit of that deferral (p50 9.2s under
sustained overload, max ≈ the run's wall time), and F7 measured its
quit-shaped form. Open question 6 asked whether bounded keyed-delivery
latency is a goal at all and, if so, which scheduling policy could relax
INV-14's shared-first pull while preserving cancel-before-delivery. The
resolution: **not a goal — no fairness policy, in either delivery mode,
and INV-14 stays exactly as RFC 0003 states it.**

The decisive fact is that shared-first pull *is* the cancellation
mechanism, not a scheduling preference layered on top of one. A shared
message is opaque to the runtime — the pull point sees a `Msg` and cannot
know whether processing it will cancel anything — and a cancellation
exists only once `update` has consumed that message and returned a command
whose cancel list reaches `enqueue_command`. "Deliver keyed output unless
a cancel is pending" is therefore not implementable at the pull point: the
only way to honor a cancel still queued as an unprocessed shared input is
to process every ready shared input first, which is precisely INV-14. A
policy that *guarantees* a keyed-delivery latency bound must consequently,
in some executions, deliver a keyed output while ready shared inputs
remain queued — under the unbounded default a backlogged shared channel is
continuously ready until fully drained, and bounded-mode admission windows
(section 4.6) are scheduling-dependent, so no guarantee can be built on
them. Every such policy trades exactly the property keying exists to buy;
the question's constraint — relax INV-14 while preserving
cancel-before-delivery — has no non-vacuous solution.

Candidate policies considered and excluded:

- **Shared-pull quota per batch window** (the example the question named):
  after `q` shared pulls in one batch, poll keyed once. This does bound
  the keyed wait (roughly `q` messages of update work) — at the cost
  above: the keyed output is delivered while every ready shared input past
  the quota position stays queued, so a stale result can reach `update`
  after the input that cancels its command was already queued ahead of it.
  Each quota firing is a literal INV-14 violation, and when the keyed pull
  returns a quit, an INV-L11 violation. A quit-exempting variant (defer a
  quota-pulled quit into a side buffer) restores INV-L11 but not the
  message case — and since the keyed `StreamMap` cannot choose what a poll
  returns, the exemption needs delivery-side buffering whose interaction
  with INV-L10's ordering then has to be designed.
- **Deadline or aging** — deliver a keyed output once it has waited longer
  than `T`: age makes the queued shared inputs no less likely to cancel
  the output; the same violation, plus a delivery order that depends on
  wall time.
- **Weighted or deficit round-robin across channels**: the quota with a
  different `q`; excluded for the same reason.
- **Cancel-aware scan** — inspect the ready shared prefix for cancels
  before delivering keyed output: not implementable, because a cancel is
  not visible in a message; it exists only after `update` runs. This is
  section 4.6's identity-carrying-variant failure one step earlier in the
  pipeline.
- **Per-delivery liveness check**: already exists — cancellation removes
  the keyed entry and drops its buffered output (RFC 0003) — and can see
  only cancels already dispatched, never those still queued as unprocessed
  shared inputs.

Declining the goal itself, not merely each candidate, rests on the
following, with the forgone half of the trade stated first and
explicitly:

- **What is forgone: keyed liveness under sustained shared readiness, in
  both modes.** The unbounded default defers keyed delivery until the
  full shared drain (F4). Bounded capacity does not restore liveness: a
  waiting producer refills each freed slot, so shared readiness — and
  with it the deferral — persists for as long as overload lasts,
  independent of the configured capacity (section 4.3). Backpressure
  bounds memory and shared drain-side latency (INV-L1, INV-L3), not
  keyed deferral. The deferral is also indiscriminate about *why* the
  shared channel is ready: user input arrives as terminal-event
  subscription output, alongside every other subscription's output and
  unkeyed command output (sections 1.1, 4.2), and any shared message —
  not only a user-originated one — can be the input whose `update`
  returns the cancel, which is the one thing the pull point cannot rank
  them by. A keyed output that is still wanted can therefore wait
  indefinitely behind unrelated hot-subscription traffic. This
  resolution accepts that cost knowingly — cancellation correctness over
  keyed liveness — rather than trading the former to buy the latter.
- **Where applications live, keyed delivery needs no policy.** In
  capacity, keyed delivery is already sub-millisecond (`keyed_steady`,
  p50 0.10ms). Under the unbounded default's overload, shared
  input-to-screen latency is itself seconds deep (F3), so faster keyed
  delivery would surface results into a UI that is seconds stale either
  way. Under bounded-mode overload the UI's staleness is bounded by
  queue capacity instead of overload duration (INV-L3, its
  producer-count premise given), which makes the persisting keyed
  deferral the visible cost — that case is exactly the previous bullet's
  explicit trade, remedied per command and at the source as described
  below, not by a scheduling policy. The loop-level fairness that keeps
  the application responsive and quittable — the frame branch and the
  dedicated quit branch — is provided by the unbiased select and
  measured healthy under overload (F5, F6).
- **The API already prices the trade per command.** Unkeyed output travels
  the shared FIFO: under backlog it waits its arrival-order turn rather
  than the full drain, and in bounded mode it inherits INV-L3's
  capacity-bounded drain-side latency. Keying opts into cancellability,
  and with it deferral behind ready shared inputs. Both behaviors exist
  today, chosen per command — the same structure as section 4.6's quit
  guidance, now stated for every keyed output: liveness-critical output
  belongs in an unkeyed command.
- **A policy would add contract surface without adding a remedy the API
  lacks.** A fairness knob in `RuntimeConfig` needs tuning coupled to
  `update` cost and load profile, and weakens RFC 0003's cancellation
  contract for every application. The liveness it would buy is already
  available per command by not keying the output, and the load
  discipline that restores keyed liveness for keyed commands — pacing or
  debouncing hot sources so the shared channel empties intermittently —
  is application-owned, the same trust class as the producer-count
  premise (section 4.5).

Consequences:

- **No new invariant, no new configuration.** The resolution keeps
  contracts already pinned: INV-14 (RFC 0003, imported unchanged by
  INV-L5 — its statement already disclaims bounded shared/keyed fairness)
  and, for quits, INV-L10/INV-L11. No new mechanism exists to pin. The
  absence of a fairness policy is checked structurally, like INV-L7's and
  INV-L8's checks, at the seams a policy would have to occupy: the two
  `AppInputs` pull points INV-L11 names (the blocking `poll_next` path and
  the non-waiting `try_next_ready` path) and the micro-batch loop that
  drives the non-waiting one — a per-batch quota would live in that loop,
  invisible to any single-pull test. Behaviorally, the
  shared-input-wins-the-pull unit tests are regression checks, not proofs
  (a quota with `q` larger than a test's pull count passes any finite
  test — section 5's bounded-test-against-unbounded-parameter argument).
  Those tests cover both pull paths for keyed
  *messages*, as the INV-L11 tests already do for keyed quits, closing the
  seam-coverage gap the pre-review checklist flags. `batch_max_messages`
  (section 4.1) is not a fairness knob and does not become one: ending a
  batch early returns to the select loop, whose next pull is shared-first
  again — even a cap of 1 leaves every keyed output behind every ready
  shared input.
- **Keyed-to-keyed arbitration stays unspecified.** The keyed pull is one
  `StreamMap` poll returning one ready element from a randomized start
  position; no per-key delivery bound is stated or relied on — already the
  reason section 5 excludes delivery from the `keyed_isolation` scenario.
  This resolution adds no per-key claim.
- **The keyed-probe scenario is permanently measurement-only.** F4's
  numbers are the unbounded regression baseline; the bounded run is
  recorded when the implementation lands, under the capacity and load
  the `RuntimeConfig` RFC pins (section 5.1). No keyed-delivery latency cell ever becomes an acceptance
  bound under this contract.
- **Reopening requires a new RFC, not a knob.** If field experience ever
  makes bounded keyed-delivery latency a requirement, the change is a
  deliberate amendment to RFC 0003's cancel-before-delivery contract
  (INV-14), with the candidate inventory above as its starting point and
  the quit exemption's interaction with INV-L10/INV-L11 as the first
  problem it must solve.
- **Documentation guidance** (lands with open question 1's
  recommended-defaults documentation, extending section 4.6's): keying a
  command buys cancellation and suppression at the cost of delivery
  deferral behind ready shared inputs under load; put liveness-critical
  output in unkeyed commands. For keyed outputs, liveness under load
  comes only from the shared channel emptying intermittently — pace or
  debounce hot sources — and not from bounded mode, which bounds memory
  and shared latency but leaves keyed deferral intact (section 4.3).

## 5. Invariants

The invariants below are the load-control implementation contract. Each
states its enforcement class (structural, behavioral, or statistical) and
the check that realizes it; the implementation realizes those checks.

- **INV-L1**: With `app_channel_capacity = n`, the shared channel buffers at
  most `n` messages, and each configured keyed channel buffers at most its
  capacity. This bounds runtime-owned channel buffers, not all pending work:
  each producer task blocked on a full channel additionally holds one
  in-flight message, and keyed channels exist per active `CommandId`, so the
  conceptual total is `shared capacity + number of blocked producers +
  Σ(per-command keyed capacity)`; the dedicated quit channel is exempt from
  capacity configuration and from this total (R1, R4). A single global
  memory bound would require
  a global permit pool or a cap on active producers; open question 3
  resolved against both, so this conceptual total *is* the contract, with
  the producer count — including `m`, the number of active `CommandId`s in
  R1's buffer total — application-owned and observable (section 4.5).
- **INV-L2**: Bounded mode never drops a message to relieve backpressure —
  capacity pressure is resolved only by the producer waiting, never by the
  runtime discarding queued or in-flight output. This does not override RFC
  0003's cancellation contract: explicit cancel, `CancelInFlight` supersede,
  and subscription reconciliation still drop buffered or in-flight output
  exactly as in unbounded mode (RFC 0003 INV-4, INV-5), and shutdown still
  discards anything undelivered. A bare "never drops before shutdown" would
  contradict INV-L5, since RFC 0003 already mandates drops that are not
  shutdown.
- **INV-L3**: With bounded capacity `n`, a message accepted into the shared
  channel is preceded by at most `n - 1` earlier shared messages (FIFO), so
  its drain-side wait — once accepted — is bounded by the drain time of one
  full queue, independent of overload duration. The producer's own wait for
  acceptance is a separate bound, not assumed here: tokio's bounded `mpsc`
  grants permits in FIFO order and each drained message releases exactly
  one permit, so a producer's admission wait is at most `(k + 1)`
  drain-equivalents, where `k` is the number of producers already queued
  for a permit ahead of it — end to end, acceptance plus drain-side is at
  most `(k + 1) + n`. This is only a bound if `k` itself is
  bounded — i.e. the number of concurrent producers is bounded — the same
  premise as INV-L1's per-producer accounting, resolved by open question 3
  as explicit, application-owned, and observable rather than
  runtime-enforced (section 4.5); INV-L3 as a whole does not hold for an
  application that violates it. No such bound exists for keyed delivery —
  open question 6 resolved against a fairness policy, so keyed-delivery
  latency stays deliberately unbounded while ready shared inputs remain
  (section 4.7).
- **INV-L4**: A quit signal already in the dedicated quit channel is
  delivered with latency independent of app-channel backlog. This is not
  automatically a hard worst-case bound under the current implementation:
  the event loop's `tokio::select!` (`run()`) is unbiased, so when the quit
  branch and the app-input branch are both ready in the same poll, which
  one is chosen is a pseudo-random tie-break, not quit-first, and the
  app-input branch's up-to-100µs micro-batch extends how much work a single
  lost tie-break can cost. Two resolutions were candidates: (a) a
  deterministic priority for the quit branch — `biased` select, or checking
  `quit_rx` before entering the app-input branch — reconciled with F5's
  frame-branch fairness, or (b) a statistical formulation with defined
  measurement conditions, since a single harness run cannot validate a
  claim of the form "always independent of backlog." **Resolved:
  formulation (b) is adopted.** F6 removed the empirical
  uncertainty: quit→delivered shows no depth dependence from 0 to ~300k
  queued messages, so the unbiased select already provides what R4 asks
  for, while (a) would restructure the one event loop every configuration
  shares — including the default path R6 and INV-L6 protect — and re-open
  F5's frame-branch fairness, to buy a hard per-run bound no requirement
  demands. The acceptance conditions are normative and scoped to the
  dedicated-channel scenarios — exactly the quits this invariant covers:
  **each of `quit_idle`, `quit_backlog_50k`, `quit_backlog_300k`, and
  `quit_overload` (section 2), run with ≥ 200 trials per scenario on the
  reference machine of section 2 (the environment every latency
  acceptance criterion is scoped to — section 5.1), must show
  quit→delivered p99 ≤ 1 ms at every measured depth, in both
  delivery modes** — the unbounded default (already measured, section 2)
  and the bounded re-run of the section 5.1 matrix, where the same
  criterion applies because the quit channel is never bounded (R4). The
  four names above are the unbounded scenario names; per this section's
  own reproducibility rule (section 5.1), the bounded re-run does not
  reuse `quit_backlog_50k` / `quit_backlog_300k` unchanged — the
  `RuntimeConfig` RFC names and defines the bounded rows this same
  criterion applies to, varying blocked-producer count and channel-full
  churn instead of depth.
  `quit_keyed_backlog_50k` is outside these conditions: it is the keyed
  control — a keyed quit never enters the dedicated channel (sections
  4.2, 4.6) — and its full-drain latency is intended behavior, recorded
  under section 5.1's F7 row, not measured against this invariant.
  quit→delivered is the delivery instant itself (the quit branch's
  tracing event, timestamped as in section 2), not a proxy — so a
  regression in the quit branch's scheduling shows up directly in the
  accepted number. Option (a) remains the recorded fallback if a hard
  per-run bound is ever required; adopting it is an amendment to this
  invariant, not a configuration knob. Quit requests still inside command
  streams are outside this invariant and follow their stream's delivery
  semantics (section 4.2) — a scope kept deliberately by open question
  7's resolution (section 4.6).
- **INV-L5**: All RFC 0003 invariants hold unchanged in bounded mode.
- **INV-L6**: Default configuration (`app_channel_capacity: None`,
  `keyed_channel_capacity: None`, `batch_max_messages: None`) reproduces
  current behavior. This is checked structurally, not by diffing observed
  behavior: the `None` path must construct the same
  `mpsc::unbounded_channel` and never-await send code as today, not a
  bounded channel configured with an unreachably large capacity. The
  testable claim is "the default code path is unchanged," not an empirical
  "outputs are identical under load," which is not practically checkable.
- **INV-L7**: The event loop task never performs an awaiting `send` on a
  channel it is itself responsible for draining. Every send into a
  runtime-owned channel today originates from a worker task (a
  subscription-forwarding task or a command task), never from the event
  loop itself, so an awaiting send under backpressure can never block on
  progress that only the event loop can make. This is a structural
  precondition for bounded-mode soundness, not something the type system
  enforces — a future change that has the event loop inject a message
  directly into a shared or keyed channel it also drains would deadlock as
  soon as that channel fills.
- **INV-L8**: The runtime never blocks, rejects, or defers producer
  admission. Every subscription the reconciliation contract says should
  run is started, and every command returned from `update` (or
  `Application::new`) is dispatched, synchronously and unconditionally —
  load control paces sends, never producer creation (section 4.5). This
  protects the RFC 0005 reconciliation contract and effect delivery from a
  future admission limit, and — because producers are created on the event
  loop task — it is also what keeps INV-L7's no-self-deadlock argument
  closed at spawn sites, not just send sites.
- **INV-L9**: Keyed backpressure is isolated per command. A send blocked on
  one keyed channel's full capacity never delays admission into the shared
  channel or into any other keyed channel; each keyed channel's admission
  depends only on that channel's own occupancy. This is the no-shared-pool
  resolution of open question 3 (section 4.5) in enforceable form — a global
  permit pool would satisfy INV-L1's per-channel capacity cells while
  violating this one, so INV-L9 is what pins the isolation choice. Its
  primary check is structural (channel-local capacity, no cross-channel
  permit sharing), with `keyed_isolation` as a behavioral regression
  scenario — see below for why a scenario alone cannot prove pool absence.
- **INV-L10**: A keyed command's `Action::Quit` is sent into that command
  run's private channel — never into the dedicated quit channel — and is
  delivered only after every output the same run sent before it: a keyed
  quit never overtakes its own run's earlier output. That is this
  invariant's whole scope — it says nothing about ordering among a run's
  messages beyond what the single FIFO channel provides, and unkeyed
  commands are outside it entirely: their quit travels the dedicated
  channel while their messages travel the shared channel, so no
  cross-channel delivery order exists and the always-armed quit branch
  can observe an unkeyed quit before the same command's earlier messages
  (sections 1.1, 4.2). This is what makes a keyed quit
  suppressible after dispatch — RFC 0003 INV-9 works by cancellation
  removing the keyed entry and dropping the buffered quit with it — and
  it is what a producer-side reroute would break (open question 7,
  section 4.6). The routing half's check is structural, like INV-L7's and
  INV-L8's: the keyed task's send loop holds only its run's private
  sender and has no handle to the dedicated quit channel, and it forwards
  `Action::Quit` into that same private channel — checked by review of
  the keyed spawn/send site. The ordering half has a behavioral
  regression check at the keyed-manager layer, the narrowest layer with
  the needed access (docs/testing.md): a unit test in which a keyed stream
  emitting a message and then a quit delivers the message first.
- **INV-L11**: At every `AppInputs` pull point, a ready shared input is
  delivered before a ready keyed quit — keyed `Action::Quit` participates
  in INV-14's shared-first pull like any other keyed output, with no
  quit-specific bypass. This is the precedence the open question 7
  resolution rests on: any ready shared input could be the one whose
  `update` cancels the quit's command, and it is processed first (section
  4.6). Like INV-14, this is a pull-point property over inputs already in
  channels; it makes no claim about inputs not yet admitted (the
  bounded-mode admission window, section 4.3), which is why bounded-mode
  keyed-quit latency is capacity-dependent and carries no acceptance
  bound (section 5.1). Behavioral check at the `AppInputs` layer: unit
  tests in which a queued shared message
  wins the pull over an already-buffered keyed quit on *each* pull point
  the invariant quantifies over — the blocking `poll_next` path and the
  non-waiting `try_next_ready` path — since a quit-specific bypass could
  be introduced on either one alone.
- **INV-L12**: With `batch_max_messages = Some(n)`, one invocation of the
  micro-batch loop pulls at most `n` inputs from `AppInputs`: the input
  that opened the batch counts as the first, at most `n - 1` further
  inputs are drained inside the window, and every pulled input counts
  toward the cap whether or not it invokes `update` —
  `ReceiverEvent::Closed` counts like any other pull, because the cap
  bounds pull work per batch and an input's kind is known only after
  pulling it (section 4.1). The cap composes with the existing exits and
  replaces none of them: the 100µs time cap, input exhaustion, and a
  pulled quit each end the batch earlier. It is scheduling policy, not
  fairness: ending a batch early returns to the select loop, whose next
  pull is shared-first again (section 4.7). With `None` the pull count
  per batch is unbounded and only the time cap applies — the current
  code path, unchanged (INV-L6). Behavioral check at the runtime layer:
  unit tests under the paused tokio clock (the batch deadline already
  uses `tokio::time::Instant` for exactly this) queue `n` ready inputs
  and assert one batch pulls all `n`, then queue `n + 1` and assert the
  batch pulls exactly `n` with the remaining input delivered by the next
  batch — the off-by-one pair — plus a `Some(1)` variant whose *opening*
  input is a `ReceiverEvent::Closed`, asserting that a ready shared input
  is left for the next batch. The opening `Closed` invokes no `update`,
  so a cap that counted `update` calls instead of pulled inputs would go
  on to pull that shared input into the same batch; the leftover
  discriminates the two. The `Closed` is placed in the opening position,
  not among later queued inputs, because a queued `Closed` is not
  deterministically constructible at this layer: `AppInputs` pulls shared
  inputs before keyed (section 4.7), and a `Closed` is surfaced only by
  an already-empty, sender-closed keyed receiver — a closed receiver
  still holding buffered output is removed when its *last* buffered
  output is pulled and it becomes empty, before any `Closed` — so no real
  input is ever pull-ordered after a `Closed` from one source, and a
  second keyed source makes the order non-deterministic because
  `StreamMap` randomizes the poll-start position. `Closed` surfacing
  itself is checked at the keyed-manager layer, where a receiver yields
  `Closed` once its sender closes on an empty buffer.
- **INV-L13**: The runtime emits the load-observability events exactly
  as the section 4.4 schema states — target `tears::runtime::load`, the
  three event kinds with their levels, required fields, and firing
  conditions, in both delivery modes except the bounded-only
  capacity-wait event — and the field values carry the stated meanings:
  `pulled` is INV-L12's counted unit, `shared_pending` the
  shared-channel occupancy at batch end, `wait_us` the blocked send's
  admission wait. The schema is contract surface: renaming, dropping, or
  repurposing any part of it is an amendment to this RFC, not an
  implementation detail. Behavioral check across the runtime batch layer
  and the integration layer: the section 4.4 definition-of-done test —
  the batch event's `Closed` differing-value case asserted at the runtime
  batch layer, and a `tracing` subscriber over a scripted load whose value
  assertions (scripted counts for `pulled`/`updated`, a known leftover for
  `shared_pending`, a controlled wait for `wait_us`, and gauge transitions
  including the cancellation-abort decrement) distinguish an
  implementation that emits the right events with wrong values.

Each invariant gets a regression scenario in `benches/runtime_load.rs` or a
unit, runtime-layer, or integration test. The overload scenario is the acceptance measurement for
INV-L1/L3: bounded queue depth and shared update latency must flatten where
the unbounded baseline grows linearly. The keyed-probe scenario never
becomes an acceptance measurement: open question 6 resolved that no
keyed-delivery latency bound exists under this contract, so its numbers are
recorded as regression baselines only (section 4.7). INV-L4's acceptance
scenarios are the four dedicated-channel `quit_*` trials (section 2; the
keyed control `quit_keyed_backlog_50k` is excluded — its full-drain wait
is intended behavior), under the normative conditions its resolved
statistical formulation states (≥ 200 trials per scenario on the
section 2 reference machine, quit→delivered p99 ≤ 1 ms at every
measured depth, in both delivery modes).
INV-L9's *primary* check is structural, like INV-L7's and INV-L8's: every
bounded channel is constructed with its own capacity, and no permit,
semaphore, or budget is shared across channels — checked by code review of
the channel-construction and send sites. It has to be structural because
no finite scenario can prove the absence of a shared pool: any scenario
that saturates `j` channels is passed by a per-channel-capacity-plus-pool
implementation whose pool exceeds `j × capacity`. The `keyed_isolation`
scenario (section 5.1), added with the implementation, is therefore a
behavioral *regression* check, not a proof, and it is built to catch
bounded pools rather than merely a second key: several keyed channels are
held at capacity with their next sends pending — saturating any modest
pool — while the two probe channels stay untouched until then. It checks
send *admission* only, and exercises the invariant's two halves separately:
(a) keyed→keyed — a previously idle key's first `keyed_channel_capacity`
sends complete, and only its `capacity + 1`-th send is pending, on that
key's own occupancy; (b) keyed→shared — the shared producer's first
`app_channel_capacity` sends complete, with only its next send pending on
the shared channel's own occupancy. Delivery is deliberately outside the
scenario: the event loop's keyed pull goes through one `StreamMap` over
every keyed receiver and cannot drain a chosen key selectively — each poll
returns one ready element from whichever key is picked, so waiting for the
probe's delivery may drain the saturated keys instead — and delivery
latency carries no bound to check (open question 6 resolved against a
fairness policy, section 4.7), which INV-L9 does not answer either way.
INV-L7 and
INV-L8 are structural rather than load-dependent and are checked by code
review of every runtime-internal send and spawn site, not by a bench
scenario; INV-L9 sits in both camps as described above, and so does
INV-L10 — its routing half is structural at the keyed send site, its
ordering half a unit-level test. INV-L11 and INV-L12 are behavioral at
the unit layer, and INV-L13 across the runtime batch layer and the
integration layer (the section 4.4 definition-of-done test); none of
INV-L10 through INV-L13 needs a bench scenario, and in particular the
`quit_keyed_backlog_50k` latency numbers are not a check for either — see
section 5.1's row for why bounded-mode keyed-quit latency cannot serve as
a reroute detector.

### 5.1 Bounded vs. unbounded acceptance matrix

The bounded mode's acceptance measurement is a re-run of the harness under a
bounded `RuntimeConfig`, compared cell by cell against the unbounded
baseline below (measured on the section 2 reference machine). The
unbounded column is fixed now so the implementation has a pinned before/after
comparison; the bounded column states the acceptance criterion each cell
must meet and is filled with measured values when the implementation lands.

Three reproducibility rules make the bounded run an acceptance
measurement rather than an implementation-time choice:

- **Parameters are pinned before the implementation PR.** The bounded
  run's configuration under test (`app_channel_capacity`,
  `keyed_channel_capacity`, and whether `batch_max_messages` is set),
  the backlog depths of its statistical rows, and their trial counts
  are fixed by RFC 0007 (section 6), alongside the recommended defaults
  it sets (open question 1) — which these test values need not
  equal. A bounded column measured under parameters chosen at
  implementation time would not be a reviewable acceptance run. Bounded
  queue depth caps at `capacity + concurrent producers` (`capacity + 1`
  for the single-flood-producer scenarios measured in this section; the
  depth-accounting note below), so the unbounded `quit_backlog_50k` /
  `quit_backlog_300k` scenarios —
  which reach those depths by flooding an unbounded channel before
  quitting — have no bounded-mode counterpart at the same depths, so RFC
  0007 accordingly redefines what the bounded quit scenarios vary, sizing
  them by blocked-producer count and channel-full churn rather than queue
  depth, not reusing the unbounded scenario names or depths unchanged.
- **Every latency acceptance criterion is scoped to the reference
  machine of section 2.** The unbounded baseline column was measured
  there, so the cell-by-cell comparison — and with it INV-L4's
  p99 ≤ 1 ms condition — is defined only there. Acceptance runs execute
  on that machine; replacing the reference machine re-measures the
  unbounded column first and is recorded as an amendment to this
  section. Runs on other machines are regression-informative, never
  acceptance.
- **CI gates on no latency criterion.** CI machines are not the
  reference machine, so no cell of this matrix and no INV-L4 condition
  is evaluated in CI. Whether a smoke profile of the harness —
  compile-and-run with no latency assertion — runs in CI is fixed by
  the `RuntimeConfig` RFC together with the parameters above.

| Scenario | Metric | Unbounded baseline | Bounded acceptance criterion |
| --- | --- | --- | --- |
| `overload` | max queue depth | ~308k, grows linearly with overload duration (F3) | ≤ `app_channel_capacity` + concurrent producers — `capacity + 1` here; see the depth-accounting note below (INV-L1) |
| `overload` | update latency p99 | 7.9s, grows with overload duration (F3) | bounded by the drain time of one full queue plus admission wait (INV-L3, its producer-count premise given) |
| `burst_200k` | peak backlog / drain | 193k peak, drains in 0.43s (F2) | backlog ≤ `capacity + 1` by the same depth accounting; producer waits instead; no message dropped (INV-L1, INV-L2) |
| `keyed_overload` | keyed delivery p50 / max | 9.2s / 13.0s (F4) | no latency criterion — open question 6 resolved: no fairness policy and no keyed-delivery bound under this contract (section 4.7); the bounded run is recorded as a measurement (deferral persists while shared stays ready, and admission-window deliveries are legal, section 4.6), with the unbounded baseline the regression reference for F4 |
| `quit_idle`, `quit_backlog_50k`, `quit_backlog_300k`, `quit_overload` | quit→delivered p99 | ≤ 0.71ms at every measured depth, depth-independent (F6; section 2 table) | quit→delivered p99 ≤ 1 ms at every measured depth over ≥ 200 trials per scenario — INV-L4's resolved statistical conditions, same criterion as the unbounded default because the quit channel is never bounded (R4); named here are the unbounded rows, and the bounded rows this criterion applies to are the `RuntimeConfig` RFC's redefined ones (blocked-producer count and channel-full churn, not the `quit_backlog_*` depths — reproducibility rules above), not these names reused unchanged; the keyed control `quit_keyed_backlog_50k` is outside INV-L4 (next row) |
| `quit_keyed_backlog_50k` | quit→delivered p50 | ≈ full shared drain, 1.30s (F7) | no latency criterion — keyed quit stays in the private channel (open question 7, section 4.6), but that contract is routing and delivery order (INV-L10/INV-L11, checked structurally and at the unit layer), not a latency number: in bounded mode a compliant implementation may deliver the keyed quit while producer backlog remains, because a pull can leave the shared channel momentarily empty while the woken producer's next send is still in flight (at `capacity = 1`, after every pull) — delivering the buffered keyed quit then outruns no *ready* input. F7's full-drain p50 therefore stays a regression check for the **unbounded default only** (re-run unbounded: p50 ≈ full shared drain); the bounded run is recorded as a statistical measurement when the implementation lands, under the capacity, depth, and trial count the `RuntimeConfig` RFC pins (reproducibility rules above) |
| `keyed_isolation` (new scenario, added with the implementation) | probe-key and shared send admission while several unrelated keyed channels are held full | trivially isolated — unbounded sends never wait | with several keyed channels at capacity and their next sends pending (saturating any modest hypothetical pool), two untouched probes are checked separately: a previously idle key's first `keyed_channel_capacity` sends complete with only its `capacity + 1`-th pending on its own occupancy (keyed→keyed), and the shared producer's first `app_channel_capacity` sends complete with only its next send pending on shared occupancy (keyed→shared); admission only, regression check — the pool-absence proof is INV-L9's structural review (section 5), and delivery is excluded (the keyed `StreamMap` cannot drain a chosen key selectively — polling for the probe may drain the saturated keys instead; delivery latency carries no bound, open question 6 resolved, section 4.7) (INV-L9) |
| `steady_20k`, `steady_200k` | default-config code path | current unbounded path | structurally identical default path, checked by code inspection, not by diffing load numbers (INV-L6) |

Queue-depth cells use the harness's depth definition, `produced -
processed`. Raw channel occupancy is not observable through the public API,
so two in-flight positions outside the channel had to be settled
explicitly. A producer blocked in a bounded `send` holds one message
outside the channel — exactly the per-producer accounting INV-L1 already
makes — and the depth *includes* it, because `produced` counts a message
when the source stream yields it. The consumer side holds up to one more
message inside `Application::update`, and the depth *excludes* it, because
`processed` counts a message when `update` begins, i.e. once it has left
the channel; counting at update completion instead would let a compliant
implementation read `capacity + producers + 1` (channel full, one blocked
producer refills the freed slot while the pulled message is still being
processed) and be flagged as a regression. The observable acceptance bound
is therefore `app_channel_capacity + concurrent shared-channel producers`
(`capacity + 1` in these single-flood-producer scenarios); depth exceeding
`capacity + producers` is the regression signal.

## 6. Open questions (all resolved)

Every question below is resolved — in this RFC, or by the now-Accepted
`RuntimeConfig` RFC, which fixes the public `RuntimeConfig` API (and with
it questions 1, 5, and the default-value half of 2), the section 5.1
bounded-run parameters (configuration under test, backlog depths, trial
counts), and the CI smoke-profile decision. No open question remains as a
prerequisite of the implementation PR (section 3.2).

1. Default capacity values to recommend in documentation (app capacity a
   measurement-informed margin choice; keyed capacity sized from the
   absorption-versus-memory trade, not measurement-derived).
   **Resolved by RFC 0007.** Recommended defaults are documentation of
   that RFC's surface: it sets the starting values (RFC 0007 §3.1), and
   the documentation-guidance notes of sections 4.5, 4.6, and 4.7 belong
   with them.
2. Whether `batch_max_messages` needs a default even in unbounded mode (F5
   suggests the 100µs cap already protects the frame branch).
   **Resolved.** The cap's semantics are pinned here as INV-L12 (sections
   4.1, 5). The default-value half — whether a non-`None` default is
   recommended — is a default-value question like question 1 and is
   resolved by RFC 0007 with it: RFC 0007 recommends `batch_max_messages`
   unset (RFC 0007 §3.1), and F5 is the evidence that `None` is safe.
3. Producer-count bounding: whether and how to bound the number of
   concurrent producers (active subscriptions, running commands) that
   INV-L1's and INV-L3's per-producer accounting assumes is bounded — for
   example an admission limit on active subscriptions or `CommandId`s — and,
   within that, whether keyed channels share one capacity pool or are
   bounded per command (per-command is simpler and matches per-command
   FIFO; a pool bounds total memory more tightly). Without an answer, R1's
   and R3's boundable-memory and bounded-latency claims hold only for
   channel buffers, not total pending-work memory or latency.
   **Resolved.** No admission limit. Producer creation is
   synchronous on the event loop task, so a waiting limit is INV-L7's
   self-deadlock at the spawn site, and the non-waiting alternatives
   (reject, drop, defer) break the RFC 0005 reconciliation contract or
   silently discard effects. R1 is explicitly weakened to per-channel
   bounds: the buffer total is `app_channel_capacity + m ×
   keyed_channel_capacity` with `m` (active `CommandId`s) an
   application-owned contract input, part of the producer-count premise
   that becomes an explicit, observable responsibility (producer gauges in
   section 4.4). Keyed channels are bounded per command rather than by a
   shared pool, pinned as testable isolation (INV-L9, `keyed_isolation`
   scenario in section 5.1). Full rationale in section 4.5; the
   no-admission-limit contract itself is INV-L8.
4. Where backpressure-wait telemetry lives (`tracing` only, or counters
   exposed by future profiling-hook work).
   **Resolved.** `tracing` only for the initial
   implementation, with the normative minimal schema — target, levels,
   fields, firing conditions, and the value-verifying definition of
   done — fixed in section 4.4 and pinned as INV-L13 (section 5).
   Profiling-hook counters remain future additive work; adding them
   does not change the tracing schema.
5. Restart-rate-control interaction: whether a future restart-rate-control
   feature consumes the same `RuntimeConfig` surface or stays a
   subscription-level policy (current position: subscription-level).
   **Resolved by RFC 0007 §4.** Restart-rate control stays a
   subscription-level policy; `RuntimeConfig` carries no restart-rate
   field and reserves no name for one. This RFC keeps the
   subscription-level position and adds no restart-rate control to the
   section 4.1 requirements.
6. Cross-channel fairness: whether bounded keyed-delivery latency is a goal
   at all and, if so, which scheduling policy (for example a shared-pull
   quota per batch window) relaxes INV-14 while preserving
   cancel-before-delivery (F4, section 4.3).
   **Resolved.** Not a goal — no fairness policy is added,
   and INV-14 stays as RFC 0003 states it in both delivery modes. The
   question's constraint has no non-vacuous solution: cancellation is an
   `update` decision, so a pull point cannot know which ready shared
   inputs are cancels, and processing them all first — INV-14 — is the
   only implementable form of cancel-before-delivery; any policy that
   guarantees a keyed-delivery bound (quota, aging, weighted round-robin)
   must deliver keyed output past inputs that could still cancel it,
   trading exactly what keying buys. The forgone half is explicit: keyed
   liveness under sustained shared readiness is given up in both modes —
   bounded capacity does not restore it, since a waiting producer refills
   each freed slot (section 4.3), and the deferring shared traffic is any
   mix of user input, subscription output, and unkeyed command output,
   not user controls alone (section 1.1). Liveness-critical output
   already has the unkeyed path (arrival-order shared FIFO, INV-L3-bounded
   in bounded mode), in-capacity keyed latency is already sub-ms
   (`keyed_steady`), and keyed liveness under load is restored by
   source pacing, not by a scheduling policy or by bounded mode. No new
   invariant is introduced: the pinned content is
   INV-14 (via INV-L5) plus INV-L10/INV-L11, with policy absence checked
   structurally at the pull seams and the shared-wins unit tests extended
   to cover both pull paths for keyed messages. The keyed-probe scenario
   stays measurement-only (section 5.1); reopening is a new RFC amending
   RFC 0003's INV-14. Full rationale, including the candidate policies
   considered and excluded, in section 4.7.
7. Quit routing: whether a keyed `Action::Quit` should be re-routed to the
   dedicated quit channel at the producer side — matching what unkeyed
   `Action::Quit` already does — or stay in its private channel under
   INV-L4's narrower scope (section 4.2). This question has two different
   shapes with different feasibility. Re-routing a quit only once it
   reaches the front of its own stream — i.e. changing which channel it is
   sent to, with no change to when the task discovers it — is a small,
   deliverable change (unkeyed commands already do exactly this, section
   1.1). Re-routing a quit that is *behind* not-yet-sent earlier items in
   the same stream is a different and harder proposal: both keyed and
   unkeyed command tasks consume their stream strictly one item at a time
   and cannot discover a later action before every earlier one has been
   sent (or, for unkeyed quit, dispatched to the dedicated channel).
   Bypassing stream order in that sense would require either look-ahead
   buffering of unsent items — reintroducing unbounded memory and
   contradicting R1/R2 — or an out-of-band quit signal that does not depend
   on stream position at all, which is a deliberate semantic change
   relative to the strict stream-order consumption RFC 0003's model
   describes (its keyed delivery-side form stated as an invariant only
   later, by INV-L10). Any resolution of this question should say which
   of the two shapes it delivers.
   **Resolved.** Neither shape: a keyed `Action::Quit` stays
   in its command's private channel. Keying a quit is a request for
   cancellability, which decomposes into post-dispatch suppression (RFC
   0003 INV-9) and precedence for the ready shared inputs that could
   still dispatch a cancel — the one class of cancelling input the
   runtime orders ahead of a keyed quit (INV-L11; ready keyed and
   un-admitted inputs are ordered by neither routing). A reroute to the
   always-armed dedicated branch (with or without an identity-and-liveness
   check, which can only see cancels already dispatched) delivers the
   quit ahead of exactly that class. The front-of-stream shape
   additionally lets the quit overtake its own run's earlier buffered
   messages, and the behind-stream shape was already infeasible as stated
   above. Prompt unconditional quit remains available as unkeyed
   `Command::quit()` (R4, F6). The properties the decision rests on are
   pinned as INV-L10 (private-channel routing and keyed-quit ordering —
   the quit never overtakes its own run's earlier output; unkeyed quit,
   which travels a different channel from its command's messages, is
   explicitly outside — structural at the keyed send site, plus a
   unit-level ordering test) and INV-L11 (a ready shared input wins the pull over a
   ready keyed quit: unit-level tests on both pull paths, blocking and
   non-waiting) — INV-L5 alone does not carry them,
   since RFC 0003 states neither a delivery-FIFO invariant that includes
   quit nor a quit-specific precedence. F7's full-drain wait is a
   regression check for the unbounded default only; bounded-mode
   keyed-quit latency is capacity-dependent and carries no acceptance
   bound (section 5.1). Full rationale, including the adversarial
   variants considered, in section 4.6.
8. Harness follow-up: add a quit-under-backlog scenario (F6/F7) and a bounded
   vs. unbounded comparison matrix before implementation.
   **Resolved.** `benches/runtime_load.rs` now runs the
   `quit_*` trial scenarios — unkeyed quit at three backlog depths plus an
   actively refilling overload, and a keyed-quit control — with per-trial
   tail statistics (section 2, F6/F7). The bounded-vs-unbounded comparison
   matrix is defined in section 5.1 with the unbounded column measured; the
   bounded column is filled when the implementation lands, under the
   parameters the `RuntimeConfig` RFC pins (section 5.1). F6 is the
   measurement basis on which INV-L4 has since been resolved to its
   statistical formulation (section 5); F7 is the quantified
   status quo that open question 7 has since resolved to keep (section
   4.6).

## 7. References

- `benches/runtime_load.rs` — full-loop load harness and reference numbers
  (section 2).
- RFC 0002 — redraw suppression (frame-branch behavior under load).
- RFC 0003 — command cancellation (INV-14 shared-first pull,
  cancel-before-delivery, INV-9 buffered-quit suppression; it states no
  FIFO invariant — keyed-quit delivery ordering is this RFC's INV-L10).
- RFC 0005 — structural lifecycle identity (subscription reconciliation the
  load path feeds).
