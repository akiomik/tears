# RFC 0007: RuntimeConfig public API and load-control acceptance parameters

- Status: Implemented
- Target: the prerequisite RFC 0006 delegated to a separate document
  (RFC 0006 sections 3.2, 6); plus one breaking change for 0.11.0
  decided here — `RuntimeConfig` drops its `Copy` derive (§2.1)
- Scope: the public `RuntimeConfig` surface (type, construction, constructor
  integration), the recommended-defaults documentation, the restart-rate
  interaction position, the RFC 0006 section 5.1 bounded-run parameters,
  and the CI smoke-profile decision
- Feature flag: none
- CHANGELOG: `Added` entries (`RuntimeConfig`, `Runtime::with_config`)
  landed at the load-control implementation release, not with this RFC.
  `Changed` (breaking) — `RuntimeConfig` no longer implements `Copy`;
  `Clone`, `Debug`, `Eq`, and `PartialEq` remain, and `FrameRate` stays
  `Copy`. Lands at 0.11.0 with the §2.1 derive-removal deliverable

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
   wall-clock guards) and sequence-complete drain delivery (every scripted
   message delivered once and in order), never on a latency percentile;
   full runs leave CI and any machine may run them, with
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeConfig {
    frame_rate: FrameRate,
    app_channel_capacity: Option<NonZeroUsize>,
    keyed_channel_capacity: Option<NonZeroUsize>,
    batch_max_messages: Option<NonZeroUsize>,
}

impl RuntimeConfig {
    #[must_use]
    pub const fn new(frame_rate: FrameRate) -> Self;
    #[must_use = "app_channel_capacity consumes the config and returns the modified value"]
    pub const fn app_channel_capacity(self, capacity: NonZeroUsize) -> Self;
    #[must_use = "keyed_channel_capacity consumes the config and returns the modified value"]
    pub const fn keyed_channel_capacity(self, capacity: NonZeroUsize) -> Self;
    #[must_use = "batch_max_messages consumes the config and returns the modified value"]
    pub const fn batch_max_messages(self, max: NonZeroUsize) -> Self;
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
- **Consuming-call misuse guard**: a setter call whose return value is
  discarded must never leave the caller silently running an unintended
  configuration. Without `Copy` (Derives below), the compiler closes
  the main shape itself: a consuming setter *moves* the config, so
  discarding its result and then using the original is a use-after-move
  error, not a silent stale read. Two discard shapes remain
  expressible — a setter called on a `clone()` with the result dropped,
  and a discarded chain whose original is never touched again — and for
  those `new` and every setter carry `#[must_use]`, each setter with an
  explanatory message, matching the crate's existing consuming-modifier
  convention (`RetryPolicy::with_backoff`,
  `RetryPolicy::with_fixed_backoff` — `src/command/retry.rs`);
  `Runtime::with_config` carries `#[must_use]` too, matching
  `Runtime::new`'s existing attribute. Pinned as INV-C6 (§7).
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
- **Derives**: `Clone`, `Debug`, `Eq`, `PartialEq` — deliberately not
  `Copy`. This RFC positions `RuntimeConfig` as the aggregation point
  for future runtime knobs, and a `Copy` config turns every future
  non-`Copy` field — a policy object, a callback — into a breaking
  derive removal deferred onto whoever adds it. 0.11.0 already carries
  committed breaking budget, so the removal is taken there, while the
  config is small and the churn minimal; after it, config growth is
  non-breaking on this axis. What `Copy` bought — harness and test code
  free of explicit clones — is recoverable with `Clone` at the cost of
  a visible `clone()`. `FrameRate` keeps `Copy`: it is word-sized with
  no growth ambition (`src/runtime/frame_rate.rs`). `RuntimeConfig` does
  not derive `Copy` (`src/runtime/config.rs`); its rustdoc describes a
  move-and-return builder, and the public surface differs from the
  `Copy` era only by that implementation's removal.

### 2.2 Constructor integration

```rust
impl<App: Application> Runtime<App> {
    #[must_use]
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
- Re-exports: `tears::RuntimeConfig` at the crate root, as
  `Runtime::with_config`'s companion type, but *not*
  `tears::prelude::RuntimeConfig`. Crate-root placement and prelude
  membership are different tests (`docs/api-guidelines.md`'s "Prelude
  Membership"): the prelude asks whether a *minimal* skeleton app writes
  the item out literally, and a minimal app calling
  `Runtime::new(flags, frame_rate)` never names `RuntimeConfig` — only an
  app that opts into `with_config` does. That is the same reasoning that
  keeps `FrameRateError` out of the prelude despite its crate-root
  re-export, not the reasoning that puts `FrameRate` in it (a minimal app
  always writes `FrameRate::new(...)`, unconditionally). Should load
  control become a minimal skeleton's default path in some future RFC,
  adding `RuntimeConfig` to the prelude then is additive.
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
one exception that the app-channel sizing rule below (§3.1), which is the
only measurement-derived rule here, must stay consistent with RFC 0006's
measurements as long as it is published. The keyed-channel rule is a policy
trade, not measurement-bound, and carries no such consistency obligation.

### 3.1 Sizing rule and starting values

The two capacities size by different rules, because the two channels drain
differently. The latency estimate below is the *shared* channel's alone;
applying it to the keyed channel is a category error the keyed rule below
names explicitly, and §3.2 keeps each rule on its own setter so neither
reads as governing the other.

**`app_channel_capacity` — the latency/burst trade.** A bounded shared
channel's capacity buys burst absorption and costs queueing latency: once
the queue is full, a newly accepted message waits behind at most one full
queue of earlier messages before reaching `update`, so its wait is roughly
`capacity × per-message drain cost`, where the drain cost is the *observed
average* service time of the application's own loop, application-dependent
and measurable (RFC 0006 §2: ~2.2µs/message average at 2µs `update` cost,
~26µs/message at 25µs — drain time tracks `update` cost, which the
application controls). The rule holds because the shared channel drains
FIFO (RFC 0006 INV-L3): a newly accepted message's drain-side wait is
bounded by the drain time of one full queue, independent of overload
duration. The product is an estimate on the measured
workload, not a worst-case bound: an average cannot bound the tail, and
neither RFC 0006 nor this RFC defines a per-message worst-case service time
to build a hard bound on. An application that needs one derives it from its
own measured tail service time, not from this rule.

**`keyed_channel_capacity` — burst absorption and memory, not a latency
guarantee.** The `capacity × drain cost` estimate does *not* transfer to
the keyed channel, and sizing it for a delivery-latency *guarantee* is a
category error: while any shared input stays ready the keyed channel is not
drained at all (shared-first pull, RFC 0006 §4.7), so keyed-delivery
latency stays unbounded independent of the configured capacity (RFC 0006
INV-L3 — "no such bound exists for keyed delivery ... keyed-delivery
latency stays deliberately unbounded while ready shared inputs remain"). A
larger keyed capacity therefore bounds no drain-side delivery latency and
restores no keyed liveness — those stay governed by shared readiness, not
by capacity. What it *can* do in a finite execution is reduce producer-side
admission wait: a keyed command awaits capacity before each next send (RFC
0006 §4.2), so a larger channel lets a burst of up to the channel's
currently free capacity — the full `capacity` only when the channel is
otherwise empty, less whatever it already holds — complete without the
command blocking on admission, and once shared readiness later lapses those
already-buffered outputs become deliverable sooner than ones still waiting
behind a full channel. That is the burst
absorption the capacity buys, and it costs memory (the per-command share of
the `m × capacity` buffer total, RFC 0006 R1). Size the keyed channel for
that absorption-versus-memory trade, never for a delivery-latency
guarantee; the starting value below is chosen on that trade alone.

Documented starting values, each with its basis stated:

- **`app_channel_capacity = 1024`.** A measurement-informed margin choice,
  not a value the measurements pin uniquely. The harness measurements give
  a lower-bound signal, not a proof of sufficiency: `steady_200k`'s queue
  depth, sampled every 5ms by the harness's depth sampler
  (`benches/runtime_load.rs`), reaches at most 400 among its samples — the
  largest *sampled* depth, not a proven instantaneous maximum, since a
  peak narrower than the 5ms sampling interval can fall between samples
  unrecorded. 1024 carries roughly 2.5× headroom over that largest
  sampled depth, not a demonstrated bound the in-capacity regime can never
  cross. It also carries a latency estimate *at* 1024, not a demonstrated
  upper bound: a full queue at that capacity adds ~27ms of estimated
  queueing latency (~1.6 frame periods at 60 FPS) at the harness's
  observed average drain rate under its heavy 25µs `update` cost (an
  estimate per the rule above, not an upper bound). No stated maximum
  acceptable latency makes that figure a ceiling the measurements rule out
  other values against — it is 1024's own cost, not evidence that a
  larger capacity would be wrong. 1024 is the round number chosen above
  the sampled lower-bound signal, at a latency its own estimate shows is a
  small number of frame periods (~1.6 at 60 FPS), not a large multiple of
  one; 512 or 2048 would be defensible on the same measurements.
  Applications with larger expected bursts scale up by the same rule;
  `burst_200k` shows the cost of undersizing is producer wait, not loss.
- **`keyed_channel_capacity = 16`.** A margin choice, stated as such —
  not measurement-derived. Two claims about it must stay separate: what
  this scenario's own numbers show, and a further point RFC 0006 already
  establishes at the mechanism level, independent of any run's length.
  - What the numbers show. `keyed_steady`'s probe (firing every 25ms,
    worst-case delivery 5.2ms) never leaves its keyed channel holding
    more than one message, so that measurement cannot distinguish 16 from
    1. `keyed_overload`'s same 25ms probe waits far longer instead — p50
    9.2s, max 13.0s (RFC 0006 §2) — because shared-first pull leaves the
    keyed channel undrained while the shared channel stays ready (RFC
    0006 F4, §4.7). A 9.2s median wait against a 25ms tick spacing means
    roughly 9.2s / 25ms ≈ 368 further ticks land before a typical probe
    clears, most of them also undelivered under the same starvation —
    enough to say a capacity as small as 16 would very likely have been
    exceeded somewhere in this run. That is as far as this run's own
    numbers reach: its own ticks over its ~13s length (13s / 25ms ≈ 520
    total) never approach 1024, so the same run cannot show whether a
    capacity that large would ever fill, and it does not pin 16 either —
    several smaller capacities are equally consistent with the same data.
  - A further point, independent of this run's own length, but weaker
    than a certainty. RFC 0006's §4.3 refill argument establishes that
    shared readiness — and with it keyed starvation — "can persist for as
    long as overload lasts, independent of the configured capacity":
    nothing bounds how long that persistence runs, so a long-enough
    overload *can* hold any finite keyed capacity full, one probe tick at
    a time, no matter how large. That licenses an existence claim only —
    no finite capacity comes with a guarantee of staying safe — not the
    stronger claim that every execution of sustained overload saturates
    every capacity: bounded mode's admission windows (RFC 0006 §4.6) are
    a real, scheduling-dependent chance for the shared channel to read
    momentarily empty right after a pull and before the woken producer
    refills it, letting a keyed output — and with it some keyed drain —
    through. RFC 0006 §4.7 declines to guarantee a keyed-delivery bound
    from those windows precisely because they are scheduling-dependent,
    not because they cannot occur, and that same fact cuts both ways:
    it is exactly why "eventually saturates" cannot be strengthened to
    "always saturates" either. What follows from §4.3 is only that no
    capacity is guaranteed safe against a long-enough overload, not that
    saturation is certain — a reading of the 13.0s figure this
    particular run happens to have measured would claim more than
    either RFC establishes.

  Absent a measured basis, 16 is fixed as a policy value from the trade
  read on both sides: a command can emit a burst of up to 16 outputs into
  an otherwise-empty channel without awaiting the consumer, and the
  per-command share of the application's `m × capacity` buffer total (RFC
  0006 R1) is bounded at 16 messages. Applications whose keyed commands
  emit larger bursts size up by that same absorption-versus-memory
  reading; adding a keyed-burst harness scenario, should field experience
  demand a measured basis, is additive.
- **`batch_max_messages`: unset.** This resolves the default-value half of
  RFC 0006 open question 2 as delegated: no non-`None` value is
  recommended. F5 is the evidence — the 100µs time cap alone held the frame
  branch at 60 FPS through every overload scenario — so the documentation
  recommends the count cap only as a diagnostic knob, not as a default.

### 3.2 Where the recommendation lives

Rustdoc on `RuntimeConfig` and its three setters: each capacity's sizing
rule and recommended value live on that capacity's own setter — the
latency/burst rule on `app_channel_capacity`, the absorption-versus-memory
rule on `keyed_channel_capacity` (§3.1) — so neither reads as governing the
other channel. The type-level doc carries only the overview and links to
the setters; it does not restate either rule as if it were shared. No
separate guide document is added; crate-level docs link to the type.

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
  only to the blocked-producer and churn rows. A barrier arranged before
  `update` returns
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
  same short window shows that the channel filled again after draining —
  a capacity-wait event fires at the moment its send is accepted, i.e.
  once it has consumed a slot a consumer pull already freed (RFC 0006
  §4.4), so the second event's send was accepted into a slot freed by a
  pull that happened between the two events, not by the first event's
  own completion. That intervening pull-then-refill is what "oscillate at
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
- **This section depends on RFC 0006 §4.4's gauge current-value
  contract.** Every predicate that reads `blocked` (the `quit_blocked_*`
  and `quit_keyed_bounded` rows) reads the *current* value of the
  `blocked` gauge at the quit instant, and the harness's teardown barrier
  (`await_quiescence`, `benches/runtime_load.rs`) reads the current gauge
  sum returning to zero. "Current value" is RFC 0006 §4.4's contract term,
  and it is per runtime instance: the value on the gauge event with the
  greatest `seq` *among events carrying that instance's `runtime_id`*,
  not the value on
  the most recently *arrived* event — the schema does not order gauge
  events by arrival, and a general consumer partitions by `runtime_id`
  before taking the greatest `seq`. This harness runs one runtime at a
  time and its teardown barrier completes before the next runtime
  starts, so exactly one partition is ever active; its scalar
  high-water read is the single-partition degenerate form of that rule,
  and parsing `runtime_id` is unnecessary here
  (`benches/runtime_load.rs`). The harness therefore consumes these
  gauges by
  greatest `seq` within the one active instance, discarding any event
  whose `seq` does not advance, so a
  reordered stale gauge event can corrupt neither a predicate reading nor
  the barrier. This dependency is stated explicitly because it is
  otherwise easy to miss: gauge events are dispatched off the runtime's
  gauge lock (RFC 0006 §4.4), so the schema does not guarantee their
  arrival order matches `seq` order, and a consumer that trusted arrival
  order could read a stale gauge value even if the current implementation
  happens to deliver them in `seq` order. Reading by greatest `seq` is what
  keeps this section correct regardless of dispatch order.
- Counting only valid trials means an implementation may need more
  attempts than the row's required count whenever some attempts fail their
  predicate. To make the retry rule precise — which attempts are retried,
  which fail the row, and how the retries are bounded — start from the
  outcome of a single attempt. Each attempt has exactly one of three
  outcomes, and only one of them is retried:
  - **valid trial** — the run completed, its quit was delivered and timed,
    and the valid-trial predicate held at the quit instant. Its measurement
    enters the row's sample.
  - **predicate miss** — the run completed and delivered its quit, but the
    valid-trial predicate did *not* hold at the quit instant (the intended
    contention did not materialize under this scheduling). This is the
    *only* retryable outcome: the trial did not exercise the intended
    precondition, so it is excluded from the sample and another attempt is
    made. Retries are for predicate misses and nothing else.
  - **quit-contract failure** — the run timed out (exceeded `max_wall`), or
    completed with no recorded quit-delivery event. This is a failure of
    the quit contract itself (RFC 0006 INV-L4: the quit was not delivered,
    or not within the guard), not a benign precondition miss, so it fails
    the row outright and is never retried away. Retrying past a timeout or
    a missing delivery would let an implementation that intermittently
    loses quit assemble a clean sample from the attempts that happened to
    succeed and pass the row — the exact masking this outcome class exists
    to forbid, and why timeout/delivery-loss cannot be folded into the
    predicate-miss retry path.

  Because only predicate misses are retried, and each one forces another
  attempt, a row whose predicate rarely holds — by scheduling misfortune,
  or by an implementation defect that makes the precondition rare or
  unreachable — could retry forever without ever timing out: a per-attempt
  timeout (`max_wall`) only catches an attempt that itself hangs, not a run
  of predicate misses that each complete normally. Every row with a
  non-`none` valid-trial predicate therefore carries an attempt cap: at
  most `10 × trials` attempts, using the row's own required trial count
  from the table above (2,000 for each of the three 200-trial rows with a
  predicate — `quit_blocked_1`, `quit_blocked_64`, `quit_overload`; 200 for
  `quit_keyed_bounded`'s 20). `quit_idle`'s `none` predicate makes every
  *completed* attempt a valid trial by definition (it can still hit a
  quit-contract failure like any row), so it needs no cap and carries none.
  Every attempt counts against the cap, and attempts stop as soon as the
  row's required valid-trial count is reached. The row's three terminating
  outcomes are reported as three distinct classes, never conflated and
  never reported as a shorter but still valid sample:
  - a **quit-contract failure** on any attempt fails the row immediately,
    reported as the timeout / missing-delivery class;
  - **attempt-cap exhaustion** — the cap reached before the required
    valid-trial count — fails the row as its own class, distinct from a
    quit-contract failure;
  - otherwise the row passes once its required valid trials are collected.

  What the cap guarantees is termination, not a wall-clock
  ceiling: it bounds the number of attempts to a fixed, finite count, so
  a row can no longer retry forever waiting on a rare predicate — the
  actual failure mode this rule exists to close. It is not itself a
  bound on the row's total wall time, and `10 × trials × max_wall`
  overstates what is guarded: `max_wall` (`benches/runtime_load.rs`)
  times out only the `runtime.run(&mut terminal)` call inside a single
  attempt (`run_quit_trial`), not the `Runtime`/`Terminal` construction
  before it, the sample extraction after it, or the per-attempt loop
  overhead in `run_quit_scenario` — so `10 × trials × max_wall` bounds
  only the cumulative time those `runtime.run` calls can spend across
  the capped attempts, not the row's actual wall time. A strict
  wall-clock ceiling on the whole row would need a separate, row-level
  aggregate timeout wrapping every attempt together, which this RFC does
  not add: the un-guarded overhead per attempt is expected to be
  negligible next to `max_wall`, and the attempt cap alone already rules
  out the unbounded-retry failure mode. Adding an aggregate guard later,
  should that overhead prove not negligible, is additive. The same
  attempt-count cap applies unchanged to the §6 smoke profile's reduced
  trial counts.

### 5.3 Remaining bounded rows

- **`overload`, `burst_200k`, `keyed_overload`**: re-run under the §5.1
  configuration with their RFC 0006 §2 load parameters unchanged (rates,
  durations, `update`/`view` costs, probe cadence). Their criteria are RFC
  0006 §5.1's cells verbatim — `overload` and `burst_200k` carry bounded
  acceptance criteria (depth/latency and backlog/no-drop respectively),
  while `keyed_overload` is recorded as a measurement with *no* acceptance
  bound (RFC 0006 §5.1's keyed-overload cell: open question 6 resolved
  against a keyed-delivery bound, §4.7). The depth-accounting bound
  (`capacity + concurrent producers`, §5.2's observed-depth quantity)
  instantiates to `1024 + 1` for these single-flood-producer rows.
- **`steady_20k`, `steady_200k`**: not bounded rows, and they carry no
  bounded criterion. Their RFC 0006 §5.1 cell is the *default-config*
  code-path check (INV-L6): the scenarios run under the default
  (load-control-unset, unbounded) configuration and are verified by code
  inspection that the default path stays "structurally identical" to the
  legacy unbounded path, "checked by code inspection, not by diffing load
  numbers" (RFC 0006 §5.1's steady-row cell). A bounded re-run measures
  nothing that check asks for: both scenarios pace at or below drain
  capacity, so their sampled shared-channel depth stays far under
  `app_channel_capacity = 1024` (`steady_200k` reaches at most ~400
  sampled, §3.1), and a bounded re-run exercises the same in-capacity
  regime the default run does — no bounded behavior to gate. They are
  therefore the default-path structural check, not members of the bounded
  matrix. Treating them as bounded rows would assign them a "criteria
  verbatim" that RFC 0006's steady cell cannot supply, because that cell is
  a code-review row (INV-L6), not a bounded acceptance criterion.
- **`keyed_isolation`**: 8 keyed channels are saturated (each holding
  16 messages with its next send pending — 128 buffered messages plus 8
  pending sends, exceeding any modest hypothetical pool), with the two
  probes exactly as RFC 0006 §5.1's row defines them (a previously idle
  key's first 16 sends, and the shared producer's first 1024 sends). The
  keyed probe is a ninth key started only *after* the eight are saturated,
  so it is genuinely previously idle when probed; the shared producer,
  however, runs throughout as the saturation enabler and cannot be staged
  the same way — the shared channel must stay full to keep the keyed
  channels from draining under shared-first pull, so it cannot be idled and
  re-probed later. Its first `app_channel_capacity` sends are therefore
  verified concurrently with the held keyed saturation: the gate is the
  shared occupancy sampled *only while all nine keyed channels are
  simultaneously saturated*, which must reach exactly `app_channel_capacity
  + 1` (the full channel plus its one pending send). This simultaneous
  value, not a whole-run maximum, is load-bearing — a shared pool could
  drive the shared channel to its full occupancy *before* the keyed channels
  start and then shed capacity to hold them, so a historical peak would pass
  a pool that never held the full shared channel and all `9 × capacity`
  keyed messages at once, which is exactly the violation the gate must
  catch. The shared channel is untouched by any keyed producer, so this is
  still the keyed→shared admission the row asks for. The measurement also
  gates that no keyed message is ever delivered to `update` (the keyed
  `StreamMap` never selectively drains a probe) and that every keyed
  channel's yield count is exactly `capacity + 1`, so a drain would fail
  rather than pass as extra admission. Eight
  saturated keys is the scale chosen to defeat a pooled implementation
  sized near per-channel capacity while keeping the row cheap to execute
  in the full acceptance and regression runs it belongs to.
  `keyed_isolation` is not part of the §6 smoke profile: the smoke build
  compiles it along with every other scenario in the one harness binary,
  but never runs it, so the key count is an acceptance-run cost choice,
  not a smoke constraint.

## 6. CI smoke profile

**Resolved: yes — CI runs a smoke profile of the harness, in place of a
full-scenario run.** RFC 0006 fixed that CI gates on no
latency criterion. At this question's resolution CI built and ran the
full harness on every push (`cargo test --bench runtime_load`, whose
custom `main` ignored `cargo test`'s filtering); the
open question was therefore never *whether* the harness runs in CI but
*which profile*: the full scenarios' wall time grows with every
statistical row this RFC adds (200-trial quit runs, the bounded matrix
re-runs) while their latency numbers gate nothing on a CI machine. The
resolution, landed in `ci.yml`: the Benchmarks job runs the
latency-assertion-free smoke profile via `just bench-smoke` (so local
and CI invocations are identical), gating on completion, and the full
scenarios stay out of CI: they run as
deliberate acceptance or regression runs (§5), on any machine, with RFC
0006 §5.1's scoping unchanged — a full run carries acceptance force only
on the reference machine, and runs on other machines are
regression-informative, never acceptance. The job's `subscription` bench
invocation is unchanged.

- **Invocation**: a `--smoke` argument to the harness binary (which already
  takes scenario-name arguments), selecting reduced variants: `steady_20k`
  under the default (load-control-unset) configuration, shortened to 0.5s —
  the same default-path role it has in §5.3, so the smoke run never forks
  its config from its acceptance meaning — a 20k-message bounded burst under
  the §5.1 configuration, `quit_idle_bounded` and `quit_blocked_1` at 5
  *valid* trials each
  — `quit_idle_bounded`'s predicate is `none` (§5.2's `quit_idle` row —
  the bounded-run table names its baseline row `quit_idle`, which the
  harness emits as `quit_idle_bounded` to disambiguate it from the
  default-mode `quit_idle`), so every attempt counts toward its 5;
  `quit_blocked_1` counts only attempts whose §5.2
  predicate (`blocked == 1` at the quit instant) held, under §5.2's
  attempt cap scaled to this row's 5-trial count (a 50-attempt cap,
  `10 × 5`), which fails the smoke run outright rather than retrying past
  it. CI
  invokes it through a `just` recipe (`just bench-smoke`) in the existing
  Benchmarks job, so the local and CI invocations are identical.
- **Pass/fail**: two assertion classes, split by what the observation
  point can actually distinguish. The draining scenarios (`steady_20k`
  under the default configuration, the bounded burst under §5.1) assert,
  after the drain, that the processed messages are exactly the scripted
  sequence — every `Msg::Load` sequence number in `0..total` observed once
  and in order — not merely that `produced == processed == total`. The
  sequence numbers already ride on `Msg::Load` (`benches/runtime_load.rs`),
  and the single flood producer feeds the shared FIFO channel in seq order,
  so a lossless drain's processed seqs must form the contiguous run
  `0, 1, …, total − 1` (first `0`, each step `+1`, last `total − 1`); a
  strictly-increasing-by-one check costs O(1) state and refutes any drop (a
  gap), duplicate (a repeat), reorder (a step backward), or lost tail (a
  short final seq). A total-only assertion is deliberately rejected: it
  passes an implementation that drops one seq and duplicates another, so it
  proves only that no *net* count was lost, never the "every scripted
  message delivered" claim this gate makes (an illegal drop-plus-duplicate
  is exactly the counterexample that motivates the sequence check). The two
  draining scenarios differ in what a caught gap *means*, because they run
  different configurations: on the bounded burst (§5.1) a gap is an illegal
  bounded-mode drop (RFC 0006 INV-L2) made observable; on the
  default-configuration `steady_20k` a gap is a break in the unbounded
  path's delivery integrity, not an INV-L2 finding, since the default path
  is not in bounded mode — the same check, serving INV-L2 on one row and
  default-path completion-and-integrity on the other. The quit scenarios
  assert completion only: after a
  quit, undelivered messages are legally discarded at shutdown (INV-L2's
  shutdown carve-out), and at the harness's observation point a legal
  shutdown discard is indistinguishable from an illegal drop — the
  sequence-integrity assertion there would read a legally discarded tail as
  a gap or a short final seq, either passing illegal drops or failing legal
  discards — so that gate lives on the draining scenarios alone. No latency
  percentile is compared against
  anything — the profile carries no latency assertion. One wall-clock
  condition remains: every smoke scenario carries the harness's
  per-scenario completion guard (`max_wall` in `benches/runtime_load.rs`)
  at 30 s — the existing `steady_20k` and `quit_idle_bounded` keep their
  current value, and the new bounded burst and `quit_blocked_1` (§5.2) take the
  same — and the smoke run fails when any scenario times out. That
  timeout-failure rule is part of the profile's definition, and the two
  paths divide it: on the full-run path, quit trials fail the run on
  timeout while a timed-out load scenario stays report-only; the smoke
  path promotes a timed-out load scenario to a failure —
  the sequence-integrity assertion alone does not cover it, because a run
  that hangs after processing its last scripted message times out with the
  full sequence `0..total` already delivered, so the assertion has nothing
  left to flag. The guard is the completion
  gate itself: a machine slow enough to exceed it fails on
  non-completion, never on a latency criterion. `quit_blocked_1` carries
  a second, independent failure condition on top of that guard: §5.2's
  attempt cap (50 attempts here, `10 × 5`), which fails the run if 5 valid
  trials are not collected within it — reachable even when every
  individual attempt completes well inside `max_wall`, which is exactly
  the case a per-attempt timeout alone cannot catch (§5.2's rationale for
  the cap). Both quit scenarios also inherit §5.2's quit-contract-failure
  class independent of the cap: any attempt that times out or completes
  with no recorded quit-delivery event fails the run outright — the harness
  already fails on either (`benches/runtime_load.rs`) — reported as the
  timeout / missing-delivery class, never folded into a predicate-miss
  retry or into attempt-cap exhaustion. The three failure classes stay
  distinct in the smoke run exactly as §5.2 defines them.
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
  restart-rate field (§4) — and its re-export placement is exactly §2.3's:
  reachable at `tears::RuntimeConfig` and *not* through
  `tears::prelude::*`. Structural, in two parts: review of the type's
  public items for the field/method surface, and review of `src/lib.rs`
  (the root re-export exists) and `src/prelude.rs` (no `RuntimeConfig`
  re-export and no mention in its "What's included" doc comment) for
  placement. The existing `api_surface` test does not check either half:
  `no_public_item_has_two_non_prelude_paths` and
  `prelude_is_a_subset_of_root_level_items` (`tests/api_surface.rs`) both
  check reachability *from* the prelude *toward* the root, never the
  reverse. An item wrongly re-exported through the prelude while also
  present at the root has exactly one non-prelude path (the root itself)
  and is reachable at the root as its prelude membership requires, so it
  satisfies both tests unchanged — a `RuntimeConfig` re-exported from both
  modules would pass the test suite as written. This review step is
  therefore the only check for §2.3's placement decision, not a
  restatement of one the tests already perform.
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
unchanged `new` to INV-C1/C5/C6, and the §2.3 re-export placement
(crate-root only, no prelude) to INV-C4. The frame rate's runtime
semantics are `FrameRate`'s, unchanged by relocation; the load controls'
runtime *semantics* are covered by RFC 0006's invariants (INV-L1, INV-L3,
INV-L6, INV-L12), not duplicated here.

### 7.1 Successor correspondence under the reducer-first kernel

RFC 0014's kernel reads no wall clock and owns no frame-rate
configuration: render cadence becomes pass-bounded, and time-driven
redraw becomes an application `Timer` subscription (RFC 0014 §6.3). The
frame rate therefore leaves this type and the constructors that carry it.
The register that decides this is RFC 0014 §9 row 4, whose landing that
RFC gates on its staged spike (its §13.1); until then every clause above
states the surface in force. Each becomes the following, with the
successor's own enforcement classes staying RFC 0014 §12's.

- **INV-C5 is superseded.** With no frame-rate configuration there is no
  scheduler for a config to disagree with, so the mismatch this invariant
  prohibits is closed by removal rather than by a check. The reason it
  gives is also why the parameter cannot simply be ignored: a frame rate
  accepted and paced at nothing is the degenerate worst case of that same
  mismatch (RFC 0014 §2.4).
- **§2.1's `frame_rate` field and §2.2's `Runtime::new(flags,
  frame_rate)` are superseded.** The constructors become
  `Runtime::new(flags)` and `Runtime::with_config(flags, config)`
  (RFC 0014 §2.4), and `RuntimeConfig::new` loses the parameter with the
  field for INV-C5's reason above. Every other §2.1 decision is
  untouched: private fields, infallible consuming setters, no `Default`,
  the derive set with `Copy` deliberately absent, and §2.3's placement.
- **INV-C1 is preserved.** One construction path still exists:
  `Runtime::new(flags)` delegates to `with_config` with a
  load-control-unset configuration, and that unset configuration selects
  the unbounded lane mode (RFC 0006 §5.2). What the delegation no longer
  carries is a frame rate.
- **§2.1's field set shrinks to two controls.** The frame rate goes
  with the pacing removal above, and `keyed_channel_capacity` goes with
  the private keyed channels it sizes — superseded by RFC 0014 §9 row 2,
  with the per-command isolation loss that follows recorded by its owner
  (RFC 0006 §5.2). What remains is `app_channel_capacity` and
  `batch_max_messages`, one setter each. The no-`Default` decision's
  stated basis — the crate has no default frame rate — goes with the
  frame rate; adding `Default` remains additive, and this RFC decides
  nothing further about it here.
- **INV-C2, INV-C3, INV-C4, and INV-C6 are re-derived over that set.**
  INV-C2: the constructor leaves both surviving controls unset, and each
  setter sets exactly its own field and no other. INV-C3: no
  construction or setter can produce an invalid configuration and none
  returns a `Result` or panics — both surviving capacities are
  `Option<NonZeroUsize>`, so zero stays unrepresentable, and each body
  stays a plain field write; the check is the same two-part review over
  a smaller surface. INV-C4: the public surface is exactly those two
  controls — no frame rate, no per-command capacity, no restart-rate
  field — with §2.3's re-export placement unchanged. INV-C6: `#[must_use]`
  on the constructor, on each surviving setter with its own message, and
  on `Runtime::with_config`. INV-C5 alone has no successor object.
- **§3.1's `batch_max_messages: unset` recommendation stands on new
  ground.** The recommendation not to set a value is unchanged, but
  unset stops meaning "time cap only": the kernel applies its own finite
  count cap (RFC 0014 §3.5). F5, the frame-branch evidence the
  recommendation cites, is superseded with the pacing facts (RFC 0006
  §5.2), so the successor's basis comes from that RFC's re-derivation,
  not from this cell.
- **§3.1's capacity rules follow RFC 0006 §5.2.** The
  `app_channel_capacity` sizing rule reads over the successor's data
  lane, the latency/burst trade unchanged in shape. The
  `keyed_channel_capacity` rule goes with the control it sizes: with no
  per-command channel there is no per-command burst to absorb and no
  `m × capacity` share to bound, so no successor rule replaces it.
  §3.3's guidance notes are carried from RFC 0006 §4.6/§4.7 and follow
  that RFC's own record.
- **§5's bounded-run parameters and §6's smoke profile follow RFC 0006's
  named prerequisite.** They configure a harness measured on the
  superseded topology; re-deriving the acceptance formulation and its
  scenario set is owned by RFC 0006 (its §5.2, tracked as RFC 0014
  §13.5), and the parameters this RFC fixes are re-fixed against
  whatever that re-derivation defines. §4's restart-rate position is
  unaffected: rate policy stays subscription-level (RFC 0012 §8).

## 8. Open questions

None. This RFC exists to close RFC 0006's delegated questions; leaving one
open would re-create the gate it removes. Matters deliberately left out —
future getters, a possible aggregate capacity-wait event, restart-rate
control's own design — are recorded above as additive follow-ups or
explicitly assigned to future RFCs, with no implementation work waiting on
them.

## 9. References

- RFC 0006 — runtime load control: the delegating document; §§3.2, 4.1, 5,
  5.1, 6 name the obligations discharged here, and §5.2 carries the
  successor correspondence §7.1 defers to.
- RFC 0014 — reducer-first core: §2.4's constructor decision, §6.3's
  removal of configured frame pacing, §3.5's count cap, and the
  supersession register §9 whose row 4 names this RFC.
- `benches/runtime_load.rs` — the harness every §5 parameter configures.
- `docs/rfcs/pre-review-checklist.md` — enforcement-class definitions used
  in §7.
