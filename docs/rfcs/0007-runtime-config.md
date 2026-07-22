# RFC 0007: RuntimeConfig public API and load-control acceptance parameters

- Status: Draft
- Target: sole remaining prerequisite of the RFC 0006 load-control
  implementation PR (RFC 0006 sections 3.2, 6); implementation starts when
  this RFC is Accepted
- Scope: the public `RuntimeConfig` surface (type, construction, constructor
  integration), the recommended-defaults documentation, the restart-rate
  interaction position, the RFC 0006 section 5.1 bounded-run parameters,
  and the CI smoke-profile decision
- Feature flag: none
- CHANGELOG: `Added` entries (`RuntimeConfig`, `Runtime::with_config`) land
  at the load-control implementation release, not with this RFC
- Amendments: none

> **Decision scope.** This RFC fixes only what RFC 0006 delegated to it. The
> load-control semantics themselves — what each control means, the
> backpressure contract, every INV-L invariant — are RFC 0006's and are not
> restated here; where this RFC names a control, RFC 0006's semantics are
> incorporated by reference, so no clause of this RFC can amend them. If a
> statement here and RFC 0006 disagree, RFC 0006 wins and the discrepancy is
> a defect in this RFC.

## Summary

RFC 0006 fixed the direction of the load-control contract and resolved every
open question except those it delegated to "the separate `RuntimeConfig`
RFC": the public API shape, the recommended defaults, the restart-rate
position (its open questions 1, 5, and the default-value half of 2), the
section 5.1 bounded-run parameters, and the CI smoke-profile question. This
RFC is that document. It decides:

1. **Public API**: a `RuntimeConfig` struct carrying the frame rate and the
   three load controls — private fields, `RuntimeConfig::new(frame_rate)`
   as the sole constructor (no `Default`: the crate has no default frame
   rate), three infallible consuming setters — consumed by
   `Runtime::with_config(flags, config)`; `Runtime::new` is unchanged and
   delegates to `with_config` with a load-control-unset configuration.
2. **Recommended defaults**: documentation-only guidance — the runtime's
   defaults stay unbounded (`None`); the documented starting point for
   applications that opt in is `app_channel_capacity = 1024` (a
   measurement-informed margin choice: the RFC 0006 section 2 measurements
   give a lower-bound signal and a latency estimate at that value, not a
   uniquely pinned number), `keyed_channel_capacity = 16` (a margin
   choice, not measurement-derived), and `batch_max_messages` unset
   (evidenced by F5), each basis stated in full in section 3.1.
3. **Restart-rate control**: stays subscription-level; `RuntimeConfig`
   carries no restart-rate field and reserves none.
4. **Bounded acceptance-run parameters**: one configuration under test
   (equal to the recommended starting point), the bounded quit scenarios
   redefined around blocked-producer count and channel-full churn (1 and 64
   blocked producers; churn covered by the bounded `quit_overload` re-run),
   trial counts carried over from RFC 0006's normative conditions, and the
   `keyed_isolation` saturation parameters (8 keys).
5. **CI smoke profile**: yes — a reduced, latency-assertion-free harness
   profile runs in CI and gates on completion (within the harness's
   wall-clock guards) and lossless drain delivery, never on a latency
   percentile; full runs leave CI and any machine may run them, with
   acceptance force reserved to the reference machine per RFC 0006.

## 1. Delegated obligations (inventory)

The complete set of obligations RFC 0006 places on this RFC, each with the
section that discharges it here:

| Obligation | RFC 0006 source | Discharged in |
| --- | --- | --- |
| Public `RuntimeConfig` shape: constructor signature, field set, naming, construction/validation style | §3.2, §4.1 | §2 |
| Recommended default capacity values (documentation; open question 1) | §6 OQ1 | §3 |
| Documentation-guidance notes of §§4.5, 4.6, 4.7 land with that documentation | §6 OQ1 | §3.3 |
| Whether a non-`None` `batch_max_messages` default is recommended (default-value half of open question 2) | §6 OQ2 | §3.1 |
| Restart-rate-control interaction (open question 5) | §6 OQ5 | §4 |
| Bounded-run configuration under test | §5.1 | §5.1 |
| Bounded quit scenarios redefined around blocked-producer count and channel-full churn, not queue depth | §5.1 | §5.2 |
| Backlog depths and trial counts of the statistical rows | §5.1 | §5.2, §5.3 |
| CI smoke-profile decision | §5.1, §6 | §6 |

No other clause of RFC 0006 is delegated here; in particular the semantics
of the three controls (RFC 0006 §4.1, INV-L12) and every acceptance
*criterion* (as opposed to the run *parameters* fixed here) stay in RFC
0006.

## 2. Public API

### 2.1 Type and construction

```rust
/// Construction-time configuration for the runtime: the frame rate and
/// the opt-in load controls (RFC 0006).
///
/// With the load controls unset, the configuration reproduces the
/// unbounded delivery mode exactly (RFC 0006 INV-L6).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeConfig {
    frame_rate: FrameRate,
    app_channel_capacity: Option<NonZeroUsize>,
    keyed_channel_capacity: Option<NonZeroUsize>,
    batch_max_messages: Option<NonZeroUsize>,
}

impl RuntimeConfig {
    #[must_use]
    pub fn new(frame_rate: FrameRate) -> Self;
    #[must_use = "app_channel_capacity returns a modified config and does not mutate in place"]
    pub fn app_channel_capacity(self, capacity: NonZeroUsize) -> Self;
    #[must_use = "keyed_channel_capacity returns a modified config and does not mutate in place"]
    pub fn keyed_channel_capacity(self, capacity: NonZeroUsize) -> Self;
    #[must_use = "batch_max_messages returns a modified config and does not mutate in place"]
    pub fn batch_max_messages(self, max: NonZeroUsize) -> Self;
}
```

- **Scope of the type**: `RuntimeConfig` is the runtime's construction-time
  configuration, not a load-control namespace. The frame rate is, from the
  caller's side, exactly a runtime setting — it already travels every
  constructor call — so it lives in the type the name promises, and future
  runtime knobs accrete here without growing `Runtime`'s constructor
  signatures. RFC 0006's `with_config(flags, frame_rate, config)` was
  explicitly an example ("for example", its §3.2), and its §4.1 delegates
  regrouping to this RFC; the load-control semantics are untouched by the
  grouping.
- **Field set and names**: the frame rate plus exactly RFC 0006 §4.1's
  three controls, under RFC 0006's working names, which this RFC adopts
  unchanged. Adopting the working names keeps every cross-reference in RFC
  0006 (INV-L1, INV-L3, INV-L6, INV-L12, §5.1) literal.
- **Construction style**: `RuntimeConfig::new(frame_rate)` is the sole
  constructor — it takes the one setting that has no meaningful default
  and leaves the three load controls unset — followed by consuming setters
  named after their fields, following the crate's existing combinator
  convention (`Command::timeout`, `Command::cancellable` carry no `with_`
  prefix). Each setter sets exactly its own field. There is deliberately
  no `Default` impl: the crate has no default frame rate — `Runtime::new`
  has always required an explicit, validated `FrameRate` — and inventing
  one inside a `Default` would smuggle a policy value into a derive.
  Should a default frame rate ever be adopted, adding `Default` then is
  additive.
- **Consuming-call misuse guard**: `RuntimeConfig` is `Copy`, so a
  consuming setter call whose return value is discarded compiles silently
  and leaves the original, unmodified value in play — the caller's
  in-hand `config` is untouched, and a chained
  `Runtime::with_config(flags, config)` after a discarded setter call
  silently uses an unbounded (or otherwise unintended) configuration.
  `new` and every setter therefore carry `#[must_use]`, each setter with
  an explanatory message, matching the crate's existing consuming-modifier
  convention (`RetryPolicy::with_backoff`, `RetryPolicy::with_fixed_backoff`
  — `src/command/retry.rs`); `Runtime::with_config` carries `#[must_use]`
  too, matching `Runtime::new`'s existing attribute. Pinned as INV-C6
  (§7).
- **Validation style**: none, by construction. Every capacity is
  `NonZeroUsize`, so a zero capacity is unrepresentable, and the frame
  rate arrives as the already-validated `FrameRate` type — validation
  stays at `FrameRate::new`, whose validity condition is a range a
  `NonZeroU32` cannot express. No `RuntimeConfig` constructor, setter, or
  consuming call returns a `Result`.
- **Fields are private**: literal construction and struct-update syntax are
  unavailable outside the crate, so adding a field later is additive
  without `#[non_exhaustive]`. No getters are provided initially; adding
  them is additive.
- **Derives**: `Copy` is included deliberately — `FrameRate` is `Copy` and
  the load controls are word-sized options, and `Copy` keeps harness and
  test code free of clones. Adding a non-`Copy` field later would require
  removing the derive, which is a breaking change; that cost is accepted
  and recorded here.

### 2.2 Constructor integration

```rust
impl<App: Application> Runtime<App> {
    pub fn new(flags: App::Flags, frame_rate: FrameRate) -> Self;   // unchanged
    #[must_use]
    pub fn with_config(flags: App::Flags, config: RuntimeConfig) -> Self;  // new
}
```

- `Runtime::new` keeps its signature and semantics and is implemented as a
  delegation to `Self::with_config(flags, RuntimeConfig::new(frame_rate))`.
  The delegation is the structural seam RFC 0006's INV-L6 check goes
  through: one code path constructs the channels, and the load-control-
  unset configuration selects the unchanged unbounded construction within
  it.
- `with_config` builds the `FrameScheduler` from `config.frame_rate` at
  that single construction site — the delegation above only relocates
  where the frame rate is supplied from (a parameter to a config field);
  it does not by itself guarantee the value flows through to the
  scheduler. Pinned as INV-C5 (§7): a config carrying one frame rate must
  not silently produce a scheduler paced at another.
- Each constructor takes the frame rate exactly once: `new` as a
  parameter, `with_config` inside the config. Rejected shapes, recorded:
  - `with_config(flags, frame_rate, config)` with a load-control-only
    config (RFC 0006's example shape): leaves the frame rate outside a
    type named `RuntimeConfig`, misnaming the type from the caller's
    perspective, and fixes a third constructor parameter forever while
    every future knob still lands in the config (§2.1's scope rationale).
  - An optional `frame_rate` field with a defaulted value (e.g. 60 FPS):
    introduces a default frame rate the crate deliberately does not have,
    and makes the two constructors disagree on whether the frame rate is
    explicit (§2.1's no-`Default` rationale).
- No other constructor is added and no existing signature changes,
  preserving RFC 0006 §3.2's additivity verdict, which requires only that
  `Runtime::new` stay unchanged.

### 2.3 Placement

- Module: `src/runtime/config.rs` (`pub mod config` under `runtime`),
  matching `runtime::frame_rate`.
- Re-exports: `tears::RuntimeConfig` at the crate root and
  `tears::prelude::RuntimeConfig`, matching `FrameRate`'s pattern.
- The bench harness (`benches/runtime_load.rs`) constructs its bounded runs
  through this public surface only; `bench-internals` gains no
  config-related items.

## 3. Recommended defaults (documentation)

The runtime's own defaults are and remain unset (unbounded) for all three
load controls — `RuntimeConfig::new` leaves them `None`, which is RFC
0006's release-gate verdict (its §3.1) and INV-L6, not this section's
subject. This section fixes the *documented
recommendation* for applications that opt in. The recommendation is
guidance, not contract: no invariant tests these values, and changing the
recommendation later is a documentation change, not an amendment — with the
one exception that the sizing rule below must stay consistent with RFC
0006's measurements as long as it is published.

### 3.1 Sizing rule and starting values

The documented rule: a bounded channel's capacity buys burst absorption and
costs queueing latency — once the queue is full, a newly accepted message
waits roughly `capacity × per-message drain cost` before reaching
`update`, where the drain cost is the *observed average* service time of
the application's own loop, application-dependent and measurable (RFC 0006
§2: ~2.2µs/message average at 2µs `update` cost, ~26µs/message at 25µs —
drain time tracks `update` cost, which the application controls). The
product is an estimate on the measured workload, not a worst-case bound:
an average cannot bound the tail, and neither RFC 0006 nor this RFC
defines a per-message worst-case service time to build a hard bound on.
An application that needs one derives it from its own measured tail
service time, not from this rule.

Documented starting values, each with its basis stated:

- **`app_channel_capacity = 1024`.** A measurement-informed margin choice,
  not a value the measurements pin uniquely. The harness measurements give
  a lower-bound signal — large enough that the in-capacity regime never
  engages backpressure (`steady_200k` peaks at depth 400) — and a latency
  estimate *at* 1024, not a demonstrated upper bound: a full queue at that
  capacity adds ~27ms of estimated queueing latency (~1.6 frame periods at
  60 FPS) at the harness's observed average drain rate under its heavy
  25µs `update` cost (an estimate per the rule above, not an upper bound).
  No stated maximum acceptable latency makes that figure a ceiling the
  measurements rule out other values against — it is 1024's own cost, not
  evidence that a larger capacity would be wrong. 1024 is the round number
  chosen above the lower-bound signal, at a latency its own estimate shows
  is a small number of frame periods (~1.6 at 60 FPS), not a large
  multiple of one; 512 or 2048 would be defensible on the same
  measurements. Applications with larger expected bursts scale up
  by the same rule; `burst_200k` shows the cost of undersizing is producer
  wait, not loss.
- **`keyed_channel_capacity = 16`.** A margin choice, stated as such —
  not measurement-derived: in the measured workloads a keyed channel never
  buffers more than one message (`keyed_steady`'s probe fires every 25ms
  and its observed worst-case delivery is 5.2ms), so those measurements
  cannot distinguish 16 from 1, and the harness has no keyed-burst
  scenario that would size the value. What 16 fixes is the trade read
  from both sides: a command can emit a burst of up to 16 outputs without
  awaiting the consumer, and the per-command share of the application's
  `m × capacity` buffer total (RFC 0006 R1) is bounded at 16 messages.
  Applications whose keyed commands emit larger bursts size up by that
  same absorption-versus-memory reading; adding a keyed-burst harness
  scenario, should field experience demand a measured basis, is additive.
- **`batch_max_messages`: unset.** This resolves the default-value half of
  RFC 0006 open question 2 as delegated: no non-`None` value is
  recommended. F5 is the evidence — the 100µs time cap alone held the frame
  branch at 60 FPS through every overload scenario — so the documentation
  recommends the count cap only as a diagnostic knob, not as a default.

### 3.2 Where the recommendation lives

Rustdoc on `RuntimeConfig` and its three setters: the sizing rule on the
type, each value recommendation on its setter. No separate guide document
is added; crate-level docs link to the type.

### 3.3 Guidance notes carried from RFC 0006

The following documentation obligations from RFC 0006 land in the same
rustdoc, as stated there:

- The blocked-producer anti-pattern — a command spawned per processed
  message converts message backlog into blocked-producer backlog under
  bounded overload, which no channel capacity bounds; the producer gauges
  make it visible (RFC 0006 §4.5).
- Quit guidance — unkeyed `Command::quit()` for a prompt unconditional
  quit; `.cancellable(id)` on a quit buys suppression at the cost of
  waiting behind pending inputs under load (RFC 0006 §4.6).
- Keyed-liveness guidance — keying buys cancellation at the cost of
  deferral behind ready shared inputs; liveness-critical output belongs in
  unkeyed commands, and keyed liveness under load comes from pacing hot
  sources, not from bounded mode (RFC 0006 §4.7).

## 4. Restart-rate control (RFC 0006 open question 5)

**Resolved: restart-rate control does not consume the `RuntimeConfig`
surface.** RFC 0006 already held the position that restart pacing is a
subscription-level policy (a property of one subscription's source and
lifecycle, like debounce/throttle — RFC 0006 §4.2's source-specific-policy
class); this RFC confirms it for the surface question. `RuntimeConfig`
carries no restart-rate field, reserves no name for one, and a future
restart-rate feature is specified in its own RFC at the subscription layer.
If that future RFC concludes a runtime-global knob is needed after all,
adding a field here is mechanically additive (§2.1, private fields), but
the contract decision to do so belongs to that RFC, not this one.

## 5. Bounded acceptance-run parameters (RFC 0006 §5.1)

These parameters make RFC 0006's bounded acceptance run reproducible and
reviewer-checkable rather than implementer-chosen. The criteria each cell
is measured against stay in RFC 0006 §5.1; every latency criterion is
scoped to the RFC 0006 §2 reference machine (Apple M1 Max, 10 cores, rustc
1.97.0), and CI gates on none of them — both rules are RFC 0006's and are
restated here only for locality.

### 5.1 Configuration under test

One configuration is used for every bounded row:

```text
frame_rate             = 60 FPS (the RFC 0006 §2 harness target)
app_channel_capacity   = 1024
keyed_channel_capacity = 16
batch_max_messages     = None
```

- The values equal the documented starting point (§3.1) deliberately: the
  acceptance run then measures the configuration the documentation sends
  new users to. RFC 0006 permits the test values to differ from the
  recommended defaults; this RFC chooses not to differ. Both sets are fixed
  here, before implementation, which is what the reproducibility rule
  requires — the implementer chooses neither.
- `batch_max_messages` stays `None` in the matrix because its contract is
  INV-L12's, checked at the unit layer under the paused clock (RFC 0006
  §§4.1, 5); a count cap in the acceptance run would change every row's
  scheduling while verifying nothing INV-L12's tests do not already verify.
- A second, smaller-capacity configuration is deliberately not added: the
  admission-window behaviors a small capacity surfaces (RFC 0006 §4.6's
  `capacity = 1` discussion) are legal in bounded mode at any capacity and
  carry no acceptance criterion, so a second column would add runs without
  adding a checkable cell.

### 5.2 Bounded quit scenarios

Two bounded depth quantities must not be conflated (RFC 0006 §5.1's
depth-accounting note): shared-channel *occupancy* caps at
`app_channel_capacity`, while the harness's *observed depth* (`produced -
processed`, which counts the one in-flight message each blocked producer
holds outside the channel) caps at `capacity + concurrent producers`.
Either way, depth is capped near capacity, so the unbounded
`quit_backlog_50k` / `quit_backlog_300k` rows have no bounded counterpart
at their ~50k/~300k depths. Per the delegated obligation, the bounded
quit rows vary blocked-producer count and channel-full churn instead —
the independent variable is the blocked-producer *count*, not raw
channel occupancy, per the note on that distinction below the table:

| Scenario (bounded run) | What it varies | Definition | Valid-trial predicate (checked at the quit instant) | Trials | Criterion (RFC 0006 INV-L4) |
| --- | --- | --- | --- | --- | --- |
| `quit_idle` | baseline | unchanged from RFC 0006 §2 | none — no blocked-producer precondition | 200 | quit→delivered p99 ≤ 1 ms |
| `quit_blocked_1` | one blocked producer | one flood subscription is blocked in `send`, awaiting capacity on the shared channel; `update` returns `Command::quit()` while it remains blocked | the RFC 0006 §4.4 producer gauge reads `blocked == 1` | 200 | quit→delivered p99 ≤ 1 ms |
| `quit_blocked_64` | many blocked producers | 64 flood subscriptions, all blocked in `send` awaiting capacity on the shared channel, at quit | the producer gauge reads `blocked == 64` | 200 | quit→delivered p99 ≤ 1 ms |
| `quit_overload` | channel-full churn | unchanged from RFC 0006 §2 (producer at 100k/s): the producer rate is configured to exceed drain capacity, so the channel is intended to oscillate at or near capacity — the churn case | at least two shared-channel capacity-wait events recorded in the 5ms window immediately preceding the quit instant | 200 | quit→delivered p99 ≤ 1 ms |
| `quit_keyed_bounded` | keyed control | the RFC 0006 §2 keyed-quit trial under the §5.1 configuration: a flood subscription is blocked in `send`, awaiting capacity on the shared channel, while a keyed command's stream emits `Action::Quit` | the producer gauge reads `blocked >= 1` | 20 | none — measurement recorded, no acceptance bound (RFC 0006 §§4.6, 5.1) |

- The unbounded `quit_*` rows themselves (including `quit_backlog_50k` /
  `quit_backlog_300k`) are unchanged and stay the unbounded-mode acceptance
  scenarios; this table defines only the bounded re-run.
- `quit_blocked_64`'s producer count: 64 exceeds any plausible per-core
  scheduling artifact on the 10-core reference machine while remaining a
  realistic subscription count; the intent is to show quit→delivered does
  not scale with blocked-producer count, the bounded analogue of F6's
  depth-independence. Its depth accounting: channel occupancy stays
  ≤ 1024, observed depth ≤ `1024 + 64` = 1088 (one in-flight message per
  blocked producer). The row's criterion is quit latency, not depth, but
  any depth it records is read against `capacity + producers`, never
  `capacity + 1`.
- Trial counts are RFC 0006's normative floor for INV-L4 (≥ 200 per
  acceptance scenario; 20 for the keyed control, matching its unbounded
  baseline row) — the per-row figures above are the same floor, restated
  per row so no row is read against another's count.
- Every row with a non-`none` valid-trial predicate is subject to the
  same rule: a trial does not count toward the row's required trial count
  unless its predicate holds at the instant `update` returns
  `Command::quit()`, checked via the RFC 0006 §4.4 producer gauges
  (`blocked`) and capacity-wait events — this applies to `quit_keyed_bounded`
  and its 20 trials exactly as it does to the four 200-trial rows, not
  only to the blocked-producer and churn rows named in earlier drafts of
  this section. A barrier arranged before `update` returns
  `Command::quit()` narrows the scheduling window a trial can land in,
  but does not by itself guarantee the predicate holds at the quit
  instant: RFC 0006 §4.6 already documents an admission window in which a
  pull frees a slot that a woken producer has not yet refilled, so the
  channel state (and with it the `blocked` count) can differ between "the
  moment the barrier released" and "the moment `update` returns" a
  scheduling step later. The predicate is therefore always checked by
  observing the gauges/events at the quit instant itself — a barrier may
  be used to make satisfying it likely, but never as a substitute for the
  observation. `quit_overload`'s predicate is a windowed count, not a
  single reading, because churn is a property of a time interval, not an
  instant: one capacity-wait event only shows the channel was full once,
  which a momentary fill also produces, while a second event within the
  same short window shows a producer's send was accepted into the slot
  the first event's completion freed — the refilling that "oscillate at
  capacity" names. Two is the minimum count that distinguishes the two
  cases; the 5ms window is two orders of magnitude longer than the
  harness's ~26µs per-message drain cost under this scenario's 25µs
  `update` cost (RFC 0006 §2), long enough to contain both events without
  being so short that scheduling jitter could suppress the second one by
  chance. `quit_blocked_1`/`quit_blocked_64`/`quit_keyed_bounded` name only
  a blocked-producer count as their precondition, deliberately not raw
  channel occupancy: a `blocked` reading does not by itself establish
  that the shared channel held `app_channel_capacity` messages at the
  same instant, because the admission window (RFC 0006 §4.6) applies here
  too, not only across a barrier boundary. Once a pull frees a slot, that
  slot is handed to a woken producer whose own send may still be
  in-flight — RFC 0006 §4.4 lowers `blocked` for that producer only when
  its send is accepted, so `blocked` can still read the pre-pull count
  for an instant in which occupancy has already dropped to `capacity -
  1`. A predicate strong enough to rule this out would need the harness's
  depth accounting (`produced - processed`) alongside the gauge —
  `blocked == N && produced - processed == app_channel_capacity + N` —
  but these rows do not require it: their independent variable is the
  blocked-producer count (the mechanism F4/F7 already establish as what
  drives keyed starvation and quit deferral), not a claim about
  instantaneous raw occupancy, so `blocked == N` (or `>= 1` for
  `quit_keyed_bounded`) is the whole predicate. The acceptance run
  reports each row's count as that many *valid* trials, not that many
  attempts.

### 5.3 Remaining bounded rows

- **`overload`, `burst_200k`, `keyed_overload`, `steady_20k`,
  `steady_200k`**: re-run under the §5.1 configuration with their RFC 0006
  §2 load parameters unchanged (rates, durations, `update`/`view` costs,
  probe cadence). Their criteria are RFC 0006 §5.1's cells verbatim; the
  depth-accounting bound (`capacity + concurrent producers`, §5.2's
  observed-depth quantity) instantiates to `1024 + 1` for these
  single-flood-producer rows.
- **`keyed_isolation`**: 8 keyed channels are saturated (each holding
  16 messages with its next send pending — 128 buffered messages plus 8
  pending sends, exceeding any modest hypothetical pool), with the two
  probes exactly as RFC 0006 §5.1's row defines them (a previously idle
  key's first 16 sends, and the shared producer's first 1024 sends). Eight
  saturated keys is the scale chosen to defeat a pooled implementation
  sized near per-channel capacity while keeping the row cheap to execute
  in the full acceptance and regression runs it belongs to.
  `keyed_isolation` is not part of the §6 smoke profile: the smoke build
  compiles it along with every other scenario in the one harness binary,
  but never runs it, so the key count is an acceptance-run cost choice,
  not a smoke constraint.

## 6. CI smoke profile

**Resolved: yes — CI runs a smoke profile of the harness, replacing the
full-scenario run it performs today.** RFC 0006 fixed that CI gates on no
latency criterion. CI already builds and runs the full harness on every
push: `ci.yml`'s Benchmarks job runs `cargo test --bench runtime_load`,
and the harness's custom `main` ignores `cargo test`'s filtering and
executes its full scenarios (the job's own comment records this). The
open question was therefore never *whether* the harness runs in CI but
*which profile*: the full scenarios' wall time grows with every
statistical row this RFC adds (200-trial quit runs, the bounded matrix
re-runs) while their latency numbers gate nothing on a CI machine. The
resolution: the Benchmarks job's `runtime_load` invocation switches to
the smoke profile, and the full scenarios leave CI: they run as
deliberate acceptance or regression runs (§5), on any machine, with RFC
0006 §5.1's scoping unchanged — a full run carries acceptance force only
on the reference machine, and runs on other machines are
regression-informative, never acceptance. The job's `subscription` bench
invocation is unchanged.

- **Invocation**: a `--smoke` argument to the harness binary (which already
  takes scenario-name arguments), selecting reduced variants: `steady_20k`
  shortened to 0.5s, a 20k-message bounded burst under the §5.1
  configuration, `quit_idle` and `quit_blocked_1` at 5 trials each. CI
  invokes it through a `just` recipe (`just bench-smoke`) in the existing
  Benchmarks job, so the local and CI invocations are identical.
- **Pass/fail**: two assertion classes, split by what the observation
  point can actually distinguish. The draining scenarios (`steady_20k`,
  the bounded burst) assert `produced == processed == the scenario's
  scripted total` after the drain — every scripted message delivered —
  which is what makes an illegal bounded-mode drop (RFC 0006 INV-L2)
  observable: a silently dropped message leaves `processed` short of the
  scripted total. The quit scenarios assert completion only: after a
  quit, undelivered messages are legally discarded at shutdown (INV-L2's
  shutdown carve-out), and at the harness's `produced - processed`
  observation point a legal shutdown discard is indistinguishable from an
  illegal drop — a lossless assertion there would either pass illegal
  drops or fail legal discards — so the lossless gate lives on the
  draining scenarios alone. No latency percentile is compared against
  anything — the profile carries no latency assertion. One wall-clock
  condition remains: every smoke scenario carries the harness's
  per-scenario completion guard (`max_wall` in `benches/runtime_load.rs`)
  at 30 s — the existing `steady_20k` and `quit_idle` keep their current
  value, and the new bounded burst and `quit_blocked_1` (§5.2) take the
  same — and the smoke run fails when any scenario times out. That
  timeout-failure rule is part of the profile's definition, not
  something the harness fully provides today: quit trials already fail
  the run on timeout, but a timed-out load scenario is currently
  report-only, so the smoke implementation promotes it to a failure —
  the lossless assertion alone does not cover it, because a run that
  hangs after processing its last scripted message times out with
  `processed` equal to the scripted total. The guard is the completion
  gate itself: a machine slow enough to exceed it fails on
  non-completion, never on a latency criterion.
- **What it is not**: not an acceptance run, not a regression baseline, and
  its numbers are not recorded anywhere. It exists to prove the harness
  still builds (all scenarios compile in the one binary) and the smoke
  scenarios still terminate, at a wall time that stays viable as the
  scenario set grows.

## 7. Invariants

Enforcement classes follow the pre-review checklist's definitions
(structural / behavioral / statistical).

- **INV-C1**: `Runtime::new(flags, frame_rate)` is
  `Runtime::with_config(flags, RuntimeConfig::new(frame_rate))` — a
  literal delegation, so exactly one construction path exists and the
  load-control-unset configuration selects RFC 0006's unchanged unbounded
  path within it. Structural: review of `Runtime::new`'s body and of the
  single channel-construction site `with_config` reaches; this is the seam
  RFC 0006's INV-L6 structural check goes through, and a second parallel
  construction path is the violation to look for.
- **INV-C2**: `RuntimeConfig::new(frame_rate)` carries the given frame
  rate and leaves all three load controls unset, and each setter sets
  exactly its own field and no other. Behavioral: unit tests on
  `RuntimeConfig` (constructed state, one test per setter asserting the
  set field and the unchanged others).
- **INV-C3**: no `RuntimeConfig` construction or setter can produce an
  invalid configuration, and none returns a `Result` or panics on any
  input. Structural, at the signatures *and* the bodies — a signature
  check alone admits a setter that panics on a chosen value while staying
  infallible in its type. Signatures: every capacity field is
  `Option<NonZeroUsize>`, so the sole invalid value (zero) is
  unrepresentable in the argument types, the frame rate arrives as the
  already-validated `FrameRate`, and no fallible signature exists on the
  type. Bodies: `new` and each setter is a plain field write — no
  branching on the argument's value, no arithmetic, no
  `panic!`/`assert!`/`unwrap` path — checked by review of the four
  function bodies.
- **INV-C4**: the public surface of `RuntimeConfig` consists of exactly the
  frame rate and the three RFC 0006 §4.1 controls — in particular, no
  restart-rate field (§4). Structural: review of the type's public items.
  The existing `api_surface` test does not check this (it enforces the
  single-canonical-path and prelude-membership rules, not a surface
  snapshot); it applies to this RFC only in that the §2.3 re-exports must
  satisfy those two rules.
- **INV-C5**: `with_config` constructs its `FrameScheduler` from
  `config.frame_rate` — the frame rate that reaches the scheduler is
  exactly the value the caller supplied to `RuntimeConfig::new`, never a
  hardcoded or unrelated one. Structural: review of the single
  scheduler-construction site `with_config` reaches, confirming the
  `FrameScheduler::new` call reads `config.frame_rate`. Behavioral, at the
  runtime-module unit layer (not an integration test, and not measured
  against wall-clock timing, which the existing
  `test_runtime_frame_scheduler_period_is_accurate` (`src/runtime.rs`)
  already establishes as the crate's pattern for this exact class of
  claim): construct a `Runtime` via `with_config` at a given frame rate
  and assert `runtime.scheduler.frame_period() ==
  config_frame_rate.frame_duration()` for at least two distinct frame
  rates (e.g. 30 and 144 FPS). This is a direct, deterministic value
  comparison — no elapsed-time observation, no tolerance window, and
  nothing for idle-wakeup elision or scheduler jitter to flake — and it
  fails an implementation that ignores `config.frame_rate` in exactly the
  way INV-C1 and INV-C2 cannot: both hold for an implementation where
  `RuntimeConfig` stores one frame rate while `with_config` builds a
  scheduler from a different, fixed one.
- **INV-C6**: `RuntimeConfig::new` and its three setters, plus
  `Runtime::with_config`, carry `#[must_use]` — each `RuntimeConfig`
  setter with an explanatory message — so a chained call that discards a
  setter's return value is a compiler warning, not a silent no-op that
  reaches `with_config` unmodified (§2.1's consuming-call misuse guard).
  Structural: review of the five signatures for the attribute and, on
  each `RuntimeConfig` setter, a non-empty message.

Surface–invariant coverage: the struct and `RuntimeConfig::new` map to
INV-C2/C3/C6, each setter to INV-C2/C3/C4/C6, `with_config` and the
unchanged `new` to INV-C1/C5/C6. The frame rate's runtime semantics are
`FrameRate`'s, unchanged by relocation; the load controls' runtime
*semantics* are covered by RFC 0006's invariants (INV-L1, INV-L3, INV-L6,
INV-L12), not duplicated here.

## 8. Open questions

None. This RFC exists to close RFC 0006's delegated questions; leaving one
open would re-create the gate it removes. Matters deliberately left out —
future getters, a possible aggregate capacity-wait event, restart-rate
control's own design — are recorded above as additive follow-ups or
explicitly assigned to future RFCs, with no implementation work waiting on
them.

## 9. References

- RFC 0006 — runtime load control: the delegating document; §§3.2, 4.1, 5,
  5.1, 6 name the obligations discharged here.
- `benches/runtime_load.rs` — the harness every §5 parameter configures.
- `docs/rfcs/pre-review-checklist.md` — enforcement-class definitions used
  in §7.
