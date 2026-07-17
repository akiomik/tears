# RFC 0006: Runtime Load Control

- Status: Draft
- Target: release-gate decision for 0.10.0 (section 3); implementation after
  0.10.0 (additive)
- Scope: bounded memory, backpressure, and latency behavior of the runtime
  under load; the delivery contract of every runtime-owned channel
- Feature flag: none
- CHANGELOG: `Added` entry lands at the load-control implementation release
  (opt-in `RuntimeConfig`); 0.10.0 itself needs no CHANGELOG entry for this
  RFC (see section 3)

> **Decision scope.** Section 3 (the release-gate verdict) is the part of this
> draft that 0.10.0 depends on, and it is final unless the contract design in
> sections 4–6 turns out to be unimplementable without breaking the public
> API. Sections 4–6 fix the direction of the load-control contract but their
> details (capacities, batching caps, per-source classes) remain open until
> implementation. In particular, cross-channel fairness (section 4.3), the
> quit delivery path (section 4.2), and the exact scope of the memory bound
> (INV-L1) are contracts to settle before the post-0.10.0 implementation;
> none of them gates the 0.10.0 release.

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
2. **Post-0.10.0 implementation (direction fixed, details open).** A bounded
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

- **R1**: Memory used by pending runtime-owned channel buffers must be
  boundable by configuration. This is a bound on buffered messages waiting
  in a channel, not on all pending-work memory: each producer blocked
  awaiting channel capacity additionally holds one in-flight message
  outside the channel, and the number of concurrent producers (active
  subscriptions, running commands) is controlled by the application, not by
  `RuntimeConfig`. A full memory bound also requires bounding producer
  count, which this RFC's channel-capacity controls do not deliver on their
  own (INV-L1, open question 3).
- **R2**: A configured bound must not silently drop messages by default;
  backpressure (slowing the producer) is the default overload response.
- **R3**: Input-to-screen latency under load must be observable and, with a
  bounded configuration, bounded by queue capacity rather than by overload
  duration — given a bounded number of concurrent producers, the same
  premise INV-L3 and open question 3 depend on.
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
- **R5**: RFC 0003's cancellation and FIFO invariants (per-command FIFO,
  cancel-before-delivery, INV-14) must be preserved or explicitly amended.
- **R6**: The default (unconfigured) behavior of existing applications must
  not change in 0.10.0.

## 2. Measurements

Full-loop load characteristics were measured with `benches/runtime_load.rs`,
which drives the real `Runtime` through the public API: a subscription floods
the shared channel at a configured rate; `update` and `view` simulate CPU cost
(2–25µs and 500µs respectively); a keyed command emits probe messages every
25ms in the `keyed_*` scenarios. Reference run: Apple M1 Max (10 cores),
rustc 1.97.0, `cargo bench --bench runtime_load`, 60 FPS target.

| Scenario | Load | Max queue depth | Update latency p50 / p99 | Render latency p50 / p99 | Keyed latency p50 / max | FPS |
| --- | --- | --- | --- | --- | --- | --- |
| `steady_20k` | 20k/s for 5s, 2µs update | 140 | 0.04ms / 0.77ms | 0.60ms / 2.3ms | — | 60 |
| `steady_200k` | 200k/s for 5s, 2µs update | 400 | 0.26ms / 1.2ms | 0.92ms / 1.7ms | — | 60 |
| `burst_200k` | 200k at t=0, 2µs update | 193,221 | 212ms / 421ms | 210ms / 422ms | — | 61 |
| `overload` | 100k/s for 5s vs ~38k/s capacity, 25µs update | 307,869 | 4.0s / 7.9s | 4.0s / 7.9s | — | 60 |
| `keyed_steady` | as `steady_20k` + keyed probe | 680 | 0.05ms / 2.2ms | 0.63ms / 8.9ms | 0.10ms / 5.2ms | 60 |
| `keyed_overload` | as `overload` + keyed probe | 308,733 | 4.0s / 7.9s | 4.0s / 7.9s | **9.2s / 13.0s** | 60 |

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
  bounded keyed-delivery latency needs a scheduling policy of its own
  (section 4.3, open question 6).
- **F5 — The frame branch survives overload.** The unbiased select plus the
  100µs batch cap kept the frame branch at 60 FPS through every overload
  scenario; no structural event-loop change is required for frame
  scheduling. The harness does **not** measure quit responsiveness under
  backlog: its quit is generated only after the last load message is
  processed. A quit-under-backlog scenario is required before implementation
  to validate INV-L4 and the quit-path contract (open question 8); because
  the select is unbiased, that scenario needs tail latency across many
  trials, not a single run, to say anything about INV-L4.

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

**Yes.** The opt-in surface is a new `RuntimeConfig` (design and ownership
left to a separate `RuntimeConfig` RFC/task) consumed by a new constructor
(for example `Runtime::with_config(flags, frame_rate, config)`), with
`Runtime::new` unchanged and equivalent to the default configuration.
Bounded behavior activates only through that surface:

- Capacity limits replace the unbounded channels inside the runtime; no
  public channel type is exposed today, so this is internal.
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

`RuntimeConfig` (design and ownership left to a separate `RuntimeConfig`
RFC/task; the fields this RFC requires are listed below) with at least:

- `app_channel_capacity: Option<NonZeroUsize>` — `None` (default) keeps the
  unbounded shared channel; `Some(n)` bounds it.
- `keyed_channel_capacity: Option<NonZeroUsize>` — same for each keyed
  command's private channel.
- `batch_max_messages: Option<NonZeroUsize>` — count cap for one micro-batch
  window, complementing the 100µs time cap.

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
  subject to that ordering today). Whether keyed / in-stream quit should
  instead be re-routed to the dedicated channel — matching what unkeyed
  `Action::Quit` already does — is open question 7, which also covers why
  that reroute is not a trivial channel swap for a quit that follows
  not-yet-sent earlier items in the same stream. User-initiated quit itself
  is not part of that open question: as an unkeyed command with no earlier
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

- **Per-command and per-subscription FIFO** (RFC 0003): unchanged; awaiting a
  send preserves order.
- **INV-14 shared-first pull**: unchanged, and bounded mode adds no fairness
  guarantee on top of it. A full shared channel is refilled by a waiting
  producer each time the loop pulls one message, so shared readiness — and
  with it F4's keyed starvation — can persist for as long as overload
  lasts, independent of the configured capacity. Bounded capacity controls
  backlog and memory only. If bounded keyed-delivery latency becomes a
  requirement, it needs an explicit scheduling policy (for example a
  shared-pull quota per batch window) defined as a deliberate relaxation of
  INV-14 that preserves cancel-before-delivery; that policy is open
  question 6 and is not part of the initial bounded mode.
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

### 4.4 Observability

Bounded or not, the runtime should expose load signals under `tracing`
targets (`tears::runtime::load` or similar): queue depth high-water marks,
batch sizes, and time spent awaiting capacity. Whether more than `tracing`
is needed (for example dedicated profiling-hook counters) is a decision for
that future work; the load harness already demonstrates what to measure.

## 5. Invariants (draft)

To be finalized as contract tests before implementation:

- **INV-L1**: With `app_channel_capacity = n`, the shared channel buffers at
  most `n` messages, and each configured keyed channel buffers at most its
  capacity. This bounds runtime-owned channel buffers, not all pending work:
  each producer task blocked on a full channel additionally holds one
  in-flight message, and keyed channels exist per active `CommandId`, so the
  conceptual total is `shared capacity + number of blocked producers +
  Σ(per-command keyed capacity)`. A single global memory bound would require
  a global permit pool or a cap on active producers (open question 3).
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
  grants permits in FIFO order, so a producer's admission wait is at most
  `(k + n)` drain-equivalents, where `k` is the number of producers already
  queued for a permit ahead of it. This is only a bound if `k` itself is
  bounded — i.e. the number of concurrent producers is bounded — which is
  the same open premise as INV-L1's per-producer accounting (open
  question 3); INV-L3 as a whole does not hold without it. No such bound
  exists for keyed delivery unless the fairness policy (open question 6)
  defines one.
- **INV-L4**: A quit signal already in the dedicated quit channel is
  delivered with latency independent of app-channel backlog. This is not
  automatically a hard worst-case bound under the current implementation:
  the event loop's `tokio::select!` (`run()`) is unbiased, so when the quit
  branch and the app-input branch are both ready in the same poll, which
  one is chosen is a pseudo-random tie-break, not quit-first, and the
  app-input branch's up-to-100µs micro-batch extends how much work a single
  lost tie-break can cost. Resolving INV-L4 needs either (a) a deterministic
  priority for the quit branch — `biased` select, or checking `quit_rx`
  before entering the app-input branch — reconciled with F5's frame-branch
  fairness, or (b) a statistical formulation with defined measurement
  conditions (trial count, load profile, and the percentile/threshold that
  counts as passing), since a single harness run cannot validate a claim of
  the form "always independent of backlog." Quit requests still inside
  command streams are outside this invariant and follow their stream's
  delivery semantics (section 4.2). The new quit-under-backlog scenario
  (open question 8) needs to settle which of the two this invariant is
  before it becomes an acceptance measurement.
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

Each invariant gets a regression scenario in `benches/runtime_load.rs` or an
integration test. The overload scenario is the acceptance measurement for
INV-L1/L3: bounded queue depth and shared update latency must flatten where
the unbounded baseline grows linearly. The keyed-probe scenario becomes an
acceptance measurement only once the fairness policy (open question 6) fixes
what keyed latency bound, if any, to expect; INV-L4 needs the new
quit-under-backlog scenario (open question 8). INV-L7 is structural rather
than load-dependent and is checked by code review of every runtime-internal
send site, not by a bench scenario.

## 6. Open questions (to resolve before implementation)

1. Default capacity values to recommend in documentation (measurement-driven;
   the harness's burst scenario sizes the absorption/latency trade-off).
2. Whether `batch_max_messages` needs a default even in unbounded mode (F5
   suggests the 100µs cap already protects the frame branch).
3. Producer-count bounding: whether and how to bound the number of
   concurrent producers (active subscriptions, running commands) that
   INV-L1's and INV-L3's per-producer accounting assumes is bounded — for
   example an admission limit on active subscriptions or `CommandId`s — and,
   within that, whether keyed channels share one capacity pool or are
   bounded per command (per-command is simpler and matches per-command
   FIFO; a pool bounds total memory more tightly). Without an answer, R1's
   and R3's boundable-memory and bounded-latency claims hold only for
   channel buffers, not total pending-work memory or latency.
4. Where backpressure-wait telemetry lives (`tracing` only, or counters
   exposed by future profiling-hook work).
5. Restart-rate-control interaction: whether a future restart-rate-control
   feature consumes the same `RuntimeConfig` surface or stays a
   subscription-level policy (current position: subscription-level).
6. Cross-channel fairness: whether bounded keyed-delivery latency is a goal
   at all and, if so, which scheduling policy (for example a shared-pull
   quota per batch window) relaxes INV-14 while preserving
   cancel-before-delivery (F4, section 4.3).
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
   relative to RFC 0003's FIFO. Any resolution of this question should say
   which of the two shapes it delivers.
8. Harness follow-up: add a quit-under-backlog scenario (F5) and a bounded
   vs. unbounded comparison matrix before implementation.

## 7. References

- `benches/runtime_load.rs` — full-loop load harness and reference numbers
  (section 2).
- RFC 0002 — redraw suppression (frame-branch behavior under load).
- RFC 0003 — command cancellation (INV-14 shared-first pull, FIFO,
  cancel-before-delivery).
- RFC 0005 — structural lifecycle identity (subscription reconciliation the
  load path feeds).
