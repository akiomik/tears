# RFC 0006: Runtime Load Control

- Status: Draft
- Target: release-gate decision for 0.10.0 (section 3); implementation after
  0.10.0 (additive)
- Scope: bounded memory, backpressure, and latency behavior of the runtime
  under load; the delivery contract of every runtime-owned channel
- Feature flag: none
- CHANGELOG: `Added` (opt-in `RuntimeConfig`); no `Changed` entry is required
  for 0.10.0 (see section 3)

> **Decision scope.** Section 3 (the release-gate verdict) is the part of this
> draft that 0.10.0 depends on, and it is final unless the contract design in
> sections 4–6 turns out to be unimplementable without breaking the public
> API. Sections 4–6 fix the direction of the load-control contract but their
> details (capacities, batching caps, per-source classes) remain open until
> implementation.

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
   existing constructors do not change. S4 therefore adds **no breaking
   change** to 0.10.0, and 0.10.0 does not wait for the S4 implementation.
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
| Unkeyed command output | sent into the shared channel by the command task | unbounded | same |
| Keyed command output | one private `mpsc::unbounded_channel` per `CommandId` | unbounded | same |
| Quit | dedicated `mpsc::unbounded_channel` | unbounded | same |

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

- **R1**: Memory used by pending runtime work must be boundable by
  configuration.
- **R2**: A configured bound must not silently drop messages by default;
  backpressure (slowing the producer) is the default overload response.
- **R3**: Input-to-screen latency under load must be observable and, with a
  bounded configuration, bounded by queue capacity rather than by overload
  duration.
- **R4**: Quit must stay responsive under load (it has a dedicated channel and
  select branch; this must remain true).
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
  limit. It is a consequence of *unbounded* shared readiness: with a bounded
  shared queue, the starvation window is bounded by the time to drain one
  queue capacity, so load control addresses it without amending INV-14.
- **F5 — The frame and quit branches survive overload.** The unbiased select
  plus the 100µs batch cap kept the frame branch at 60 FPS through every
  overload scenario, and quit (delivered after the last processed message)
  terminated each run promptly. No structural event-loop change is required
  for frame or quit scheduling.

## 3. Release-gate decision for 0.10.0

Two questions decide whether S4 forces breaking changes into 0.10.0.

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

**Yes.** The opt-in surface is a new `RuntimeConfig` (task P7) consumed by a
new constructor (for example `Runtime::with_config(flags, frame_rate,
config)`), with `Runtime::new` unchanged and equivalent to the default
configuration. Bounded behavior activates only through that surface:

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

**S4 requires no additional breaking change in 0.10.0.** The 0.10.0 release
proceeds with the breaking changes already on `main`; the S4 implementation
(sections 4–6) lands after 0.10.0 behind `RuntimeConfig`.

## 4. Contract design (post-0.10.0, direction fixed)

### 4.1 Configuration surface

`RuntimeConfig` (owned by P7, consumed by this RFC) with at least:

- `app_channel_capacity: Option<NonZeroUsize>` — `None` (default) keeps the
  unbounded shared channel; `Some(n)` bounds it.
- `keyed_channel_capacity: Option<NonZeroUsize>` — same for each keyed
  command's private channel.
- `batch_max_messages: Option<NonZeroUsize>` — count cap for one micro-batch
  window, complementing the 100µs time cap.

The quit channel is never bounded (R4); quit signals are rare and must not
participate in backpressure.

### 4.2 Backpressure semantics per source class

One channel policy is not applied mechanically to all inputs. The bounded
mode's send contract, by source class:

- **Subscriptions (Timer, TerminalEvents, signals, WebSocket, Query,
  user sources)**: the forwarding task awaits channel capacity. Backpressure
  propagates by not polling the source stream. Sources whose upstream cannot
  pause (e.g. a WebSocket peer) buffer or shed in source-specific policy —
  that is P5 (debounce/throttle wrappers) and source-level concerns, not the
  runtime's. Terminal input is a subscription like any other; awaiting simply
  defers reading further input events, which is the desired behavior (typing
  ahead of an overloaded app queues in the terminal, not in unbounded
  memory).
- **Commands (keyed and unkeyed)**: the command task awaits capacity before
  its next send. Command streams are already async pull; no API change.
- **Quit**: never blocked, never dropped (R4).

Delivery in bounded mode remains lossless up to shutdown: the runtime never
drops a message to relieve pressure (R2). Lossy strategies (coalescing,
sampling) are source-level policies layered on top (P5), where the semantics
of "which messages may be merged" are known.

### 4.3 Interaction with existing invariants

- **Per-command and per-subscription FIFO** (RFC 0003): unchanged; awaiting a
  send preserves order.
- **INV-14 shared-first pull**: unchanged. F4's starvation is unbounded only
  because shared readiness is unbounded; with `app_channel_capacity = n`, the
  keyed delivery delay is bounded by the drain time of at most `n` shared
  messages plus producer refills, which the capacity also paces.
- **Cancellation**: cancel-before-delivery and buffered-output suppression
  (RFC 0003) are unaffected; a keyed producer blocked on a full private
  channel is aborted exactly like a running one.
- **Redraw suppression** (RFC 0002) and subscription re-evaluation gating:
  unchanged; they operate downstream of input delivery.
- **Shutdown**: bounded channels close exactly like unbounded ones; senders
  blocked in `send` observe the closed channel and terminate their tasks.

### 4.4 Observability

Bounded or not, the runtime should expose load signals under `tracing`
targets (`tears::runtime::load` or similar): queue depth high-water marks,
batch sizes, and time spent awaiting capacity. P12 (profiling hooks) decides
whether more than `tracing` is needed; the load harness already demonstrates
what to measure.

## 5. Invariants (draft)

To be finalized as contract tests before implementation:

- **INV-L1**: With `app_channel_capacity = n`, at most `n` shared messages
  are pending at any time; total pending runtime work is bounded by the sum
  of configured capacities.
- **INV-L2**: Bounded mode never drops a message before shutdown.
- **INV-L3**: With bounded capacity, emission-to-update latency of the oldest
  pending message is bounded by the time to drain one full queue, independent
  of overload duration.
- **INV-L4**: Quit delivery latency is independent of app-channel backlog.
- **INV-L5**: All RFC 0003 invariants hold unchanged in bounded mode.
- **INV-L6**: Default configuration reproduces current behavior exactly.

Each invariant gets a regression scenario in `benches/runtime_load.rs` or an
integration test; the harness's overload and keyed-probe scenarios are the
acceptance measurements for INV-L1/L3 (bounded queue depth and keyed latency
must flatten where the unbounded baseline grows linearly).

## 6. Open questions (to resolve before implementation)

1. Default capacity values to recommend in documentation (measurement-driven;
   the harness's burst scenario sizes the absorption/latency trade-off).
2. Whether `batch_max_messages` needs a default even in unbounded mode (F5
   suggests the 100µs cap already protects the frame branch).
3. Whether keyed channels share one capacity pool or are bounded per command
   (per-command is simpler and matches per-command FIFO; a pool bounds total
   memory more tightly).
4. Where backpressure-wait telemetry lives (`tracing` only, or counters that
   P12 exposes).
5. S8a interaction: whether restart rate control consumes the same
   `RuntimeConfig` surface or stays a subscription-level policy (current
   position: subscription-level, per RFC backlog).

## 7. References

- `benches/runtime_load.rs` — full-loop load harness and reference numbers
  (section 2).
- RFC 0002 — redraw suppression (frame-branch behavior under load).
- RFC 0003 — command cancellation (INV-14 shared-first pull, FIFO,
  cancel-before-delivery).
- RFC 0005 — structural lifecycle identity (subscription reconciliation the
  load path feeds).
- Backlog tasks: S4 (this RFC), P5 (debounce/throttle), P7 (`RuntimeConfig`),
  P12 (profiling hooks), S8a (restart rate control).
