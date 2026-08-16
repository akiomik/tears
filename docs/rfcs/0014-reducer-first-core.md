# RFC 0014: Reducer-First Core

- Status: Accepted (2026-08-16) — accepted at the **spike tier** §13.1
  defines: the four kernel claims (the grant handshake, the
  delivery-accounting soundness with its concurrency check, revocation
  filtering, driver topology) are demonstrated on a prototype kernel,
  and the twelve-series conformance suite is green and repeat-stable —
  nine series pass-unit driven, three on the park-boundary probe §7.2
  names. The remaining behavioral checks of §12 —
  cleanup hooks, the full combinator surface, the observability
  vocabulary, the production arbitration policy — are
  **implementation acceptance criteria**: they gate implementation
  mainlining, which this acceptance does not grant. The §9
  supersessions and amendments to owner RFCs land with this
  acceptance, in §13.1's order, ahead of that mainlining.
- Target: 0.11.0 — the breaking window reserved for composition
  (RFC 0010 §1.8)
- Scope: the reducer-first core protocol (`Reducer`/`Program`) and the
  `Application` facade adapter, with the facade-preservation decision
  (§2.4); composition combinators with automatic scope application and
  removal journals (§2.5); the kernel delivery contract — one
  origin-tagged data lane, revocation filtering, the control-lane and
  synchronous quit routes (§3); multi-keyed `batch` lowering and the
  modifier interaction rules (§3.4); the kernel side of RFC 0013's
  scope teardown, with cleanup hooks (§4); subscription
  execution under the new core — barrier scope, the teardown stop
  cause, the stopping-pass defer rule (§5); the lifecycle mapping with
  the bootstrap-quit change and the removal of configured frame pacing
  (§6); the two-layer test surface — the non-executing store and the
  stage-3 driver with its determinism contract (§7); the composition
  requirements register (§8); the supersession and amendment register
  for owner RFCs (§9)
- Feature flag: none
- CHANGELOG: `Changed` (breaking) — the runtime core becomes
  reducer-first: `Application` is executed through a single-feature
  adapter over the same kernel that runs composed reducers.
  `Runtime::new` and `RuntimeConfig` lose their frame-rate parameter
  and field (configured wall-clock frame pacing is removed, §6.3);
  `RuntimeConfig` also loses `keyed_channel_capacity`: with one data
  lane there are no per-command channels, so per-command backpressure
  isolation is not preserved — every producer awaiting capacity awaits
  the same lane's (§3.1, §9 row 2);
  `Command::batch` no longer ignores child spawn keys (they lower to
  independent keyed entries, §3.4); `Command::quit` returned from
  `update` applies synchronously at dispatch (observation order
  changes, §3.3); a keyed quit travels the control lane and no longer
  orders behind its own run's earlier output (§3.3); shared-first pull
  ordering is not preserved (§3.2). `run`'s signature and result
  contract for `Application` users are unchanged (§2.4). `Added` —
  `Reducer`, `Program`, composition combinators (`scope`, `for_each`,
  `presented`, `into_program`) with `Keyed`/`Slot`,
  `Command::on_teardown`, `ProgramRuntime`, `Exit`, and the stage-3
  `TestDriver` (the RFC 0008 amendment §7.2 delegates); the
  `Command::teardown` these combinators invoke is RFC 0013's surface and
  is entered there, not here. Lands with the implementation, after
  §13.1's gate.

## Summary

The consolidation audit's composition axiom (RFC 0010 §5.1) said the
composition core would implement `Application` as a single aggregate
adapter. The counterexample procedure of RFC 0010 §1.9 has since been
exercised across five roots (`root-CMP`, `root-A1`, `root-SCHED`,
`root-B7`, `root-K51J46`) by an architecture-selection comparison, and
the selected architecture inverts the axiom's direction while keeping
its intent: the core is a **reducer-first kernel** — `Program` (a
`Reducer` plus `init` and `view`) is the unit the runtime drives, and
`Application` is a single-feature adapter over it. One
kernel, one delivery path, one lifecycle; the facade adds no second
runtime, no second identity model, no second phase machine.

Eight decisions:

1. **Core protocol and facade** (§2). `Reducer`/`Program` are the
   composable core; `Application` and its `Runtime` entry point are
   preserved as a facade — same trait, same `run` signature, same
   result classification — executed through the adapter (§2.4).
2. **Composition with automatic scoping** (§2.5). `for_each`,
   `presented`, and `scope` apply scopes structurally; removal journals
   in `Keyed`/`Slot` turn state removal into teardown automatically.
   User code writes no manual scoping.
3. **Unified delivery with revocation filtering** (§3). Producer
   output travels one origin-tagged lane; cancellation and teardown
   revoke at the delivery decision point, so a revoked run's
   undelivered output — subscription output included — is never
   delivered. Shared-first pull and the private keyed channels are
   superseded; the properties they protected are re-checked in §3.2.
4. **Two quit routes** (§3.3). An `update`-returned quit applies
   synchronously at its dispatch; a producer-originated quit travels a
   dedicated control lane, backlog-independent and origin-checked
   (cancel still beats a buffered quit).
5. **Complete scope teardown** (§4). One teardown reaches keyed
   commands, anonymous effects, subscriptions, and cleanup hooks —
   RFC 0013's R1, closed by kernel-side scope tracking, multi-keyed
   `batch` lowering, and delivery-side retraction.
6. **Subscription execution integrated, not redesigned** (§5).
   RFC 0012's uniform barrier stays, scoped to subscription runs; a
   teardown-issued stop becomes a fourth stop cause whose quiescence
   marks dirt; a stop-issuing re-evaluation admits nothing in the same
   pass.
7. **Lifecycle preserved, two changes** (§6). RFC 0011's phase
   contract, two-stage postconditions, panic split, and driver
   exclusivity all hold on the new kernel. The two changes: an
   init-dispatched quit short-circuits bootstrap synchronously, and
   configured wall-clock frame pacing is removed — render cadence is
   pass-bounded, and time-driven redraw is an application `Timer`
   subscription.
8. **Two-layer testing** (§7). The existing store keeps its
   non-execution contract (RFC 0008 stages 1–2, unchanged); a stage-3
   `TestDriver` drives the production kernel itself — same
   construction, same task bookkeeping, same lanes, same termination —
   with the driving differential confined to two seams (pass-initiation
   arbitration, send-intent grants) plus application-side inputs.

Mechanism — the kernel's registries, counters, and seam types — is
informative (§10). The kernel exists today as the prototype §13.1's
spike tier was demonstrated on, not as crate code; this RFC states the
contract that tier verified, and §13.1 pins what still gates
mainlining.

## 1. Scope

### 1.1 In scope

- The `Reducer`/`Program` protocol and the `Application` adapter, with
  the facade-preservation decision (§2).
- The composition combinators, their journals, and automatic scope
  application (§2.5).
- The kernel delivery contract: unified lane, revocation, quit routes,
  multi-keyed lowering, modifier interaction (§3).
- Scope teardown: selection, ordering, strictness, cleanup hooks,
  retraction, and the RFC 0013 successor mapping (§4).
- The subscription-execution integration clauses (§5).
- The lifecycle mapping, the bootstrap-quit change, and the frame
  pacing removal (§6).
- The two-layer test surface and the driving determinism contract (§7).
- The composition requirements register (§8) and the owner-RFC
  supersession/amendment register (§9).

### 1.2 Out of scope

- **Identity law bodies.** What makes two identities equal — typed and
  tagged segments, ordered nesting, structural equality, collision
  safety — stays RFC 0005 (INV-14–INV-21); §2.5 and §4 state how the
  new surfaces satisfy those laws, and §9 lists the two clauses that
  need amendment (INV-18's coverage, INV-20's batch statement).
- **Subscription execution bodies.** The source template, the three
  boundaries, purity, and the effect-DI negative space stay RFC 0012;
  §5 adds to that contract and redesigns none of it.
- **Time.** Clock discipline is RFC 0009. The kernel reads no wall
  clock (§6.3); `Timer` semantics are unchanged.
- **Restart rate policy.** RFC 0012 §8's delegation frame stands.
- **Graceful drain.** Termination stays zero-grace (RFC 0011 §4.5);
  cleanup hooks run under strict abort (§4.4), and a bounded grace
  window remains the graceful-drain delegation's future work.
- **Supervision and diagnostics.** RFC 0011 §5.1's negative space
  stands; this RFC adds no diagnostic schema.
- **An external/host-driven public step surface.** Driver exclusivity
  permits one additively (RFC 0011 §6); its API is future work
  (§13.4).
- **The load-observability measurement re-derivation.** The schema
  vocabulary amendment is §9's; re-deriving RFC 0006's statistical
  acceptance numbers on the new topology is implementation-stage work
  (§13.5).

## 2. Core protocol and facade

### 2.1 `Reducer` and `Program`

```rust
pub trait Reducer {
    type State;
    type Message: Send + 'static;

    fn reduce(
        &self,
        state: &mut Self::State,
        message: Self::Message,
    ) -> Command<Self::Message>;

    fn subscriptions(&self, _state: &Self::State) -> Vec<Subscription<Self::Message>> {
        Vec::new()
    }
}

pub trait Program: Reducer {
    type Flags;

    fn init(&self, flags: Self::Flags) -> (Self::State, Command<Self::Message>);
    fn view(&self, state: &Self::State, frame: &mut Frame<'_>);
}
```

Contract, stated observably:

- A reducer value is stateless with respect to the runtime: the runtime
  never mutates it, and all application state lives in
  `Reducer::State`. A reducer may hold child reducers as fields; that
  is composition structure, not state.
- `reduce` is the only state-transition entry point, and
  `subscriptions` is a pure function of state exactly as RFC 0012
  INV-SE6 states for `Application::subscriptions` — the obligation
  transfers to the trait verbatim, and the runtime may evaluate it at
  any re-evaluation frequency.
- **Views are root-level by design.** `Reducer` deliberately has no
  `view`; only `Program` does. Composing child *views* is ordinary
  function calls inside the root `view` over the root state — pane and
  modal layout, draw order, and area allocation are application code,
  served by docs and examples, not by combinators. This is a decision,
  not a gap: a view-composition surface would couple layout policy into
  the core for no correctness gain (§11 records the rejected
  alternative).
- The `Message: Send + 'static` boundary is unchanged from
  `Application` (RFC 0010 §7.1's freeze holds through the new traits;
  no `Clone`/`PartialEq` bound is added).

### 2.2 The `Application` adapter

```rust
pub struct AppProgram<A>(PhantomData<fn() -> A>);

impl<A: Application> Reducer for AppProgram<A> {
    type State = A;
    type Message = A::Message;
    fn reduce(&self, state: &mut A, m: A::Message) -> Command<A::Message> {
        state.update(m)
    }
    fn subscriptions(&self, state: &A) -> Vec<Subscription<A::Message>> {
        state.subscriptions()
    }
}

impl<A: Application> Program for AppProgram<A> {
    type Flags = A::Flags;
    fn init(&self, flags: A::Flags) -> (A, Command<A::Message>) {
        A::new(flags)
    }
    fn view(&self, state: &A, frame: &mut Frame<'_>) {
        state.view(frame)
    }
}
```

The adapter is a mapping and nothing else: the application value *is*
the state, `update` *is* `reduce`. Every kernel concern and every
phase step executes identical code for `Application` programs and
composed programs (INV-RC1 enumerates the concern set); the facade has
no dedicated channel, branch, or phase.

### 2.3 Advanced entry point

```rust
pub struct ProgramRuntime<P: Program> { /* program, flags, config */ }

impl<P: Program> ProgramRuntime<P> {
    pub fn new(program: P, flags: P::Flags) -> Self;
    pub fn with_config(program: P, flags: P::Flags, config: RuntimeConfig) -> Self;
    pub async fn run<B: Backend>(
        self,
        terminal: &mut ratatui::Terminal<B>,
    ) -> Result<Exit, B::Error>;
}

#[non_exhaustive]
pub enum Exit { Quit }
```

Construction is inert (RFC 0011 INV-LC3 applies to both entry types).
`run` consumes the runtime; a quit of either route returns
`Ok(Exit::Quit)`, a render error returns `Err`. `RuntimeConfig`
remains move-only (`Clone`, never `Copy`) — RFC 0007's compatibility
property is preserved.

### 2.4 Facade preservation: decided, adopted in part

The question this RFC decides: does the existing public `Runtime<App>`
surface survive, with an internal exit type converted at the facade?

**Adopted for the entry point and result contract.**
`Application` (unchanged), `Runtime<App: Application>` (the existing
type), and

```rust
pub async fn run<B: Backend>(
    self,
    terminal: &mut ratatui::Terminal<B>,
) -> Result<(), B::Error>
```

are preserved: the facade runs `AppProgram<A>` on the kernel and
converts `Ok(Exit::Quit)` to `Ok(())`, `Err` passing through. RFC 0011
INV-LC5's classification — `Ok(())` for either quit form, `Err`
carrying the render error — therefore continues to hold verbatim at
this entry point. The conversion is total and lossless, and it spares
the `run` call and result handling of every existing application;
correctness gains nothing from breaking that surface (the priority
order: correctness first, then smallness — and here they agree).

**Not adopted for the constructors.** `Runtime::new(flags,
frame_rate)` and the `RuntimeConfig` frame-rate field do not survive,
because §6.3 removes configured frame pacing: after removal the
parameter would be accepted and ignored, which is exactly the silent
mismatch RFC 0007 INV-C5 exists to prohibit (a config carrying one
frame rate must not produce a scheduler paced at another — a config
carrying a frame rate and producing no pacing at all is the degenerate
worst case). The constructors become `Runtime::new(flags)` /
`Runtime::with_config(flags, config)`; this is part of the pacing
supersession (§9 row 4), not a facade decision.

### 2.5 Composition combinators

```rust
pub trait ScopeValue: PartialEq + Eq + Hash + Clone + Send + Sync + 'static {}

pub struct Keyed<K: ScopeValue, V> { /* map + removal journal */ }
pub struct Slot<S> { /* Option<S> + dismissal journal */ }

pub trait ReducerExt: Reducer + Sized {
    fn scope<Seg, C>(self, child: C, seg: Seg,
        state: fn(&mut Self::State) -> &mut C::State,
        extract: fn(Self::Message) -> Result<C::Message, Self::Message>,
        embed: fn(C::Message) -> Self::Message,
    ) -> Scoped<Self, C, Seg>
    where C: Reducer, Seg: ScopeValue;

    fn for_each<C, K>(self, child: C,
        rows: fn(&mut Self::State) -> &mut Keyed<K, C::State>,
        extract: fn(Self::Message) -> Result<(K, C::Message), Self::Message>,
        embed: fn(K, C::Message) -> Self::Message,
    ) -> ForEach<Self, C, K>
    where C: Reducer, K: ScopeValue;

    fn presented<C, Seg>(self, child: C, seg: Seg,
        slot: fn(&mut Self::State) -> &mut Slot<C::State>,
        extract: fn(Self::Message) -> Result<C::Message, Self::Message>,
        embed: fn(C::Message) -> Self::Message,
    ) -> Presented<Self, C, Seg>
    where C: Reducer, Seg: ScopeValue;

    /// Closes a combinator stack into a runnable `Program`.
    fn into_program<Flags>(self,
        init: fn(Flags) -> (Self::State, Command<Self::Message>),
        view: fn(&Self::State, &mut Frame<'_>),
    ) -> IntoProgram<Self, Flags>;
}

// IntoProgram<R, Flags> implements
// Program<State = R::State, Message = R::Message, Flags = Flags>.
```

Each combinator's parent state and message types are `Self`'s
associated types, written directly into the signatures — no free type
parameter whose equality with `Self::State`/`Self::Message` is left
implicit — and each returned combinator implements
`Reducer<State = Self::State, Message = Self::Message>`, so stacks
nest. `into_program` is the closing surface: a combinator stack plus a
root `init` and a root `view` is a `Program` the runtime (§2.3) or the
facade-equivalent test layers (§7) can drive. A same-domain sequential
combinator (`combine`) is deliberately absent — §11 records why.

Contract:

- **Automatic scope application (INV-RC2).** A combinator routes a
  child message through `extract`, reduces the child, and qualifies
  the returned command — spawn keys, explicit cancel IDs, teardown
  prefixes, cleanup registrations — with its segment (`seg`, or the
  collection key), exactly as `Command::scoped` does (RFC 0005 §4.3);
  child subscription declarations are aggregated with the same
  qualification. User code writes no `.scoped(...)` and cannot omit or
  double-apply one. RFC 0005's scope laws (INV-14–INV-21) hold through
  the combinators — the laws' bodies are unchanged; the two clauses
  that must be amended to *cover* the new carriers (teardown prefixes
  under INV-18, `batch` under INV-20) are §9's rows 6 and 3.
- **Removal journals (INV-RC3).** `Keyed::remove`, `Slot::dismiss`,
  and occupied-slot replacement (`Keyed::insert` over an occupied key,
  `Slot::present` over an occupied slot) record removals; after the
  parent's `reduce` returns, the combinator drains the journal and
  merges one `Command::teardown` per removed key into the returned
  command. Replacement means: the old instance is torn down and the
  new one starts fresh — a same-update remove-and-reinsert (or
  replace) yields the old instance's teardown *and* the new instance's
  fresh spawns in one returned command, which §3.4's lowering makes
  work at batch granularity (RFC 0013 R4).
  The journal records removals rather than diffing states, so
  same-key reinsertion is not mistaken for continuity.
- **Message routing is typed.** `extract` either claims a message for
  the child or returns it unchanged to the parent; a message for a
  child key absent from the collection is routed to nothing and
  discarded by the combinator. External input addressed to a removed
  key that has since been re-inserted reaches the *new* instance —
  key-addressed input has no run identity, and this boundary is
  documented negative space (only producer output carries origins,
  §3.1).

## 3. Kernel delivery contract

### 3.1 One lane, origin-tagged, revocation-filtered

All producer *message* output — keyed command output, unkeyed
(anonymous) command output, subscription output — is delivered to
`update` through **one FIFO data lane**, each item tagged with its
producing run's origin; the one producer output that does not travel
it is a producer-originated quit, which takes the control lane
(§3.3). The runtime decides deliverability at the **delivery decision
point** (the moment an item would next be handed to `update`), by the
producing run's liveness:

- **Revocation is strict and exact (INV-RC5).** Explicit cancel,
  `CancelInFlight` supersession, scope teardown, and termination all
  *revoke* the run; from the revocation's application point, none of
  that run's output — buffered before or sent after — is ever
  delivered. This extends RFC 0003's strict-cancel family (INV-3,
  INV-4, INV-6) from the keyed delivery class to every producer,
  subscription output included: retraction of a torn-down scope's
  undelivered output is now contract (INV-RC6) — RFC 0013 §9's
  fourth resolution.
- **Natural finish is not revocation.** A run that finishes on its own
  while output is still queued remains deliverable until its output is
  delivered; finishing does not discard (the RFC 0003 INV-6 property,
  preserved on the new topology).
- **No stale resurrection.** A run's identity slot is free for a
  successor immediately after the revocation's application point; the
  old run's late exit and late sends are inert with respect to the
  successor (RFC 0003 INV-8's token discipline, preserved).
- Bounded and unbounded lane modes remain configuration
  (`RuntimeConfig`); bounded-mode capacity, blocking sends, and the
  capacity-wait observability follow RFC 0006's bounded-mode contract
  unchanged except as §9 amends its vocabulary. The configured capacity
  is the data lane's, and it is the only one: there are no per-command
  channels left to size or to isolate from one another, so the
  per-command capacity control and its isolation property do not
  survive (§9 row 2).

### 3.2 What the superseded delivery contracts protected

The private keyed channels and shared-first pull (RFC 0003 INV-14,
RFC 0006's two delivery classes) are superseded. Their properties,
re-checked one by one:

- *Cancel beats buffered output* — **preserved, in its applied form**:
  a cancel that has been applied wins against everything undelivered
  (INV-RC5). The **broad** form is **not preserved**: shared-first
  pull gave a simultaneously ready shared input priority over an
  already-buffered keyed result, so its `update` could cancel that
  result before delivery; under one FIFO, whichever enqueued first
  delivers first. This is a deliberate property loss, recorded in §9
  (row 2), not an oversight.
- *Liveness-critical output does not starve* — **preserved for
  enqueued items, and only for them**: a FIFO admits no starvation
  among the items already in the lane, each delivered after exactly
  the prefix ahead of it, within a backlog-proportional bound (no
  priority, bounded delay). Two halves are not preserved and are not
  claimed anywhere else either: a producer still awaiting admission in
  bounded mode is outside the property (the same carve-out RFC 0006
  §5.2 makes), and the per-command routing advice this property carried
  — put liveness-critical output in an unkeyed command — has no
  successor, because one lane offers no faster class to move it to
  (§9 rows 2 and 10).
- *Redraw cadence survives flood* — **preserved, deterministically**:
  the batch cap — always finite under this RFC (§3.5, superseding the
  time-capped default window; §9 row 4) — forces pass boundaries, a
  pass renders at most once with render preceding re-evaluation
  (RFC 0011 INV-LC1/INV-LC2, unchanged), and §3.5's pass structure
  runs the frame stage before the next batch begins; a flooding
  source cannot suppress rendering.
- *Quit responsiveness* — re-derived in §3.3.

### 3.3 Quit: synchronous lowering and the control lane

Two physical routes replace the previous two:

- **`update`-returned quit** (`Command::quit()` from `reduce`/
  `update`, the init command included): applied **synchronously at the
  dispatch of the returning update**, before the next input is pulled.
  It no longer travels any channel, so no later input, cancel, or
  arbitration can intervene between the update that returned it and
  termination. The observation-order change is breaking: today an
  unkeyed quit is an effect-stream item whose delivery arbitrates with
  other branches.
- **Producer-originated quit** (a quit emitted by a keyed or anonymous
  effect task): travels a dedicated **control lane** — never bounded,
  exactly as RFC 0006 R4 pins for today's dedicated quit channel —
  drained as a **mandatory stage of every pass, before the pass's
  input batch** (§3.5), never behind the data lane's backlog or its
  capacity-wait queue (R4's backlog independence, preserved for this
  route), and applied only if its origin is still live. *Cancel beats a buffered quit*
  (RFC 0003 INV-9's intent) is therefore preserved through origin
  revocation rather than through private-channel drop.

**Not preserved**: RFC 0006 INV-L10's keyed-quit ordering — a keyed
run emitting `[Message(saved), Quit]` no longer guarantees `saved`
reaches `update` before termination, because the quit travels the
control lane while the message travels the data lane. INV-L11's
shared-first precedence falls with shared-first pull (§3.2). Both are
recorded as supersessions with this successor statement: a
producer-originated quit is backlog-independent and cancellable until
applied; ordering against its own run's earlier data-lane output is
not guaranteed. An application needing "deliver then quit" returns the
quit from the `update` that observes the final message — the
synchronous route, which pins exactly that order.

Quit latency, stated honestly: a producer quit is applied before any
input batch that begins after its arrival — a quit already arrived
when a pass begins is applied with no further input processed at all,
and one arriving mid-batch is preceded only by that batch's
remainder, bounded by the batch cap (constructive bounds; §3.5). An
`update`-returned quit adds zero hops
beyond the delivery of the input that triggers it — and *that*
delivery has no hard contract bound in bounded mode (senders awaiting
capacity are unbounded in number; RFC 0006's bounded-mode behavior,
unchanged). The statistical re-formulation of RFC 0006 INV-L4's
acceptance on the new topology is §13.5.

### 3.4 Multi-keyed `batch` lowering and modifier interaction

`Command::batch` stops folding: each child's spawn key lowers to its
own independent keyed entry (superseding RFC 0003 INV-11's
ignore-with-warning), cancel and teardown entries from all children
apply in one cancel phase that precedes every spawn from the same
command (RFC 0003 §4.3's phase order, extended), and directives fold
as today. Interaction rules, each with its excluding counterexample
in §11:

- `map(f)` distributes over children and preserves identity metadata
  (RFC 0003 INV-12 / RFC 0005 INV-17, unchanged).
- `scoped(s)` distributes over children: spawn keys, explicit cancel
  IDs, teardown prefixes, and cleanup registrations are all qualified
  (the INV-18/INV-20 amendments, §9).
- A spawn key attaches to a single effect carrier only; keying a batch
  itself is not constructible in the new API.
- Two same-key spawns in one batch apply in declaration order as two
  consecutive dispatches: the second is a replacement under its
  `CancelPolicy`.
- A teardown and a same-prefix spawn in one command: teardown applies
  in the cancel phase, the spawn starts fresh under every policy
  (RFC 0013 INV-ST3, preserved).
- Cleanup registrations (`on_teardown`) apply in the spawn phase —
  after the same command's cancel phase — so a teardown-and-reregister
  command consumes the old occupant's hooks and leaves the new
  registration armed (§4.4).
- A quit in a batch applies at the dispatch's completion like any
  `update`-returned quit; already-spawned siblings are then torn down
  by termination.

### 3.5 Pass structure: constructive bounds

A steady-state processing pass consists of **four fixed stages in
fixed order**:

1. **Exit reflection.** Producer exits the executor has completed are
   reflected into the run bookkeeping: exit-observed lifecycle facts,
   entry retirement where the delivery accounting permits it (§3.1),
   and subscription dirt exactly per §5.2's sources (the quiescence
   of a stopped subscription run marks dirt; a natural finish, a
   command run, a cleanup run mark none). This stage processes no
   input and applies no quit. Its position is normative, not
   mechanism: it determines which quiescence facts the same pass's
   control drain, admissions, and frame stage observe — the idle-wake
   path (an exit waking a parked kernel) is exactly this stage at the
   head of the woken pass, consumed by the same pass's frame stage.
2. **Control-lane drain.** Every quit that has arrived is drained —
   **before the pass's input batch** — and applied if its origin is
   live, discarded if revoked (§3.3).
3. **At most one input batch** — always count-bounded: the configured
   `batch_max_messages` when set (its counting semantics stay
   RFC 0006 INV-L12's), a finite kernel default when unset. The
   wall-clock batching window (RFC 0006 INV-L6's time-capped default)
   is superseded — the driving loop reads no wall clock (§6.3), and a
   time-windowed batch would make the bounds below
   wall-clock-relative (§9 row 4). The default cap's value is
   mechanism. What is contract is **finite-prefix eventual progress**:
   every batch ends after a finite prefix of the ready input, so the
   pass reaches its frame stage and the next pass its control drain,
   however much input stays ready. No wall-clock bound and no practical
   bound on the cap's value is claimed here — a conforming default may
   be far larger than the superseded time window admitted, and RFC 0006
   §5.2's INV-L4 prerequisite is where latency figures are re-derived.
4. **Frame step** — render if a redraw is pending, then re-evaluation
   if subscriptions are dirty.

The stages are not independently arbitrated branches: no sequence of
ready inputs can defer any of them.

**Wake arming.** A kernel with no pass in progress and nothing to make
progress on parks, and a parked kernel holds a registered waker on
**every** source that can create a pass's work. The set is exhaustive:
data-lane readiness (an item enqueued for delivery), control-lane
arrival (a producer-originated quit), and producer-exit or
subscription-quiescence notification (the facts stage 1 reflects). The
arrival of any one of them begins a pass, whose stages then run in the
order above; a kernel that parks while one of them has arrived and
remains unconsumed is non-conforming. This is INV-RC16, and it is what
keeps the bounds below from holding vacuously.

**Pass initiation.** Which of the armed sources begins the pass when
several are ready at once is arbitrated, and the production policy is
**unbiased selection** over them: no source is preferred, and none is
starved by another's continuous readiness. That is the whole claim.
What is *not* claimed is which source is picked on any given occasion,
any fairness bound or ratio, and any initiation order a consumer could
rely on — the policy pins the absence of preference, not a schedule —
and executor scheduling among producers stays unspecified as well
(RFC 0011 §2.3's negative space, at that reduced scope). Enforcement
is **structural**, at the single pass-initiation selection site the
driving loop performs — the seam §7.2's first driving differential
replaces with a script: review that the selection is an unbiased choice
over the armed set, with no per-source priority, quota, or ordering
state beside it. A behavioral test cannot prove the absence of bias
from finitely many draws, and §7.2's citation rule keeps a
driver-scripted initiation order out of evidence for this policy.

One consequence of control-before-input is stated plainly: when a quit
and an input are both ready at pass start, the quit wins — an input
whose `update` would have cancelled the quit's origin does not run
first. That sits inside the shared-first
non-preservation already recorded (§3.2, §9 row 10), made
deterministic rather than arbitrated.

Two deterministic bounds follow and are pinned as contract, each on
the arming above — a pass exists to run because arrival wakes one: a
producer quit is applied before any input batch that begins after its
arrival — a quit that has arrived when a pass begins is applied with
**zero** further inputs processed, and a quit arriving mid-pass is
preceded only by the in-progress batch's remainder (INV-RC9); a
redraw marked by a batch is rendered before the next input batch
begins, the batch that marked it having ended after a finite prefix
(INV-RC10). This narrows RFC 0011
§2.3's stated negative space — the frame pass is no longer freely
interleaved against batches — and is part of §9 row 8's RFC 0011
amendment; RFC 0011 INV-LC1/INV-LC2 themselves are unchanged
(rendering still happens outside batches, at most once per pass,
before re-evaluation, on the pass's current state). The rejected
alternative — keeping frame and control as arbitrated branches and
stating the bounds statistically — is §11's input-priority-scheduler
entry: an arbitration free to prefer ready input can starve quit
application and rendering under continuous readiness, and even an
unbiased pick yields only a probabilistic bound; quit responsiveness
and render liveness are correctness properties here, not load
properties, so the bound is built into the pass shape rather than
measured after the fact.

## 4. Scope teardown

This section states the kernel side of scope teardown, closing the
three-domain reach RFC 0013 R1 requires; the teardown contract
itself — selection, ordering, strictness, INV-ST1–ST8, R1–R9 — is
RFC 0013's (§9 row 7), whose §9 records the same four resolutions
from the contract side:

1. **Public surface and owner** (question 1):
   `Command::teardown(seg)` is the manual primitive; the composition
   machinery invokes the *same* constructor internally (no internal
   twin — RFC 0013 R8's two layers over one surface). Anchoring
   composes through `scoped` exactly as explicit cancel IDs do
   (INV-ST2, preserved). Lowering, selection, and application are the
   kernel's.
2. **Subscription participation** (question 2): immediate stop —
   teardown's application point issues stop requests to the selected
   subscription runs and revokes them; the composition layer removes
   the declarations in the same update, so the stop is never
   self-defeating. The required RFC 0012 amendments are §5's. The
   admission coupling stays the uniform barrier, accepted as
   documented negative space (§5.1).
3. **Scope tree and unkeyed tracking** (question 3): the runtime
   tracks task-by-scope first-class. Anonymous (unkeyed) effects
   spawned through a composition boundary carry scope membership —
   no logical key, no second identity model — and prefix teardown
   selects them (INV-RC7). Auto-keying (a synthetic second identity)
   and prohibiting unkeyed child effects (breaking the
   child-as-standalone-app symmetry) are both rejected; nothing leaks.
4. **Shared-output retraction** (question 4): yes — §3.1's
   revocation filtering is the contract; the orphan window for
   *delivery* ends at the teardown's application point, not at
   quiescence. What filtering cannot do — return input already
   consumed from an external resource by a stop-requested,
   not-yet-quiesced source — is the barrier's job (§5.1).

### 4.1 Selection, ordering, strictness, totality, reuse

RFC 0013 §3.1–§3.6 own the operation's contract; on this kernel it
reads: selection matches, by complete prefix path over structural
segment equality, **every run under the prefix** — keyed commands,
anonymous effects, subscription runs — plus the prefix's unfired
cleanup registrations
(consumed, §4.4). Local keys never participate in selection; shorter,
reordered, subset, and deeper-position paths are not selected;
teardown applies in the cancel phase before same-command spawns,
commutes with explicit cancels, is total and idempotent, and scope
reuse observes nothing stale (per-run tokens and the fresh-slot rule,
no scope-generation state). Strictness is §3.1's revocation: no
selected run's output is delivered after the application point, a
buffered producer quit under the prefix included.

### 4.2 The RFC 0013 successor mapping

Each of RFC 0013's INV-ST1–ST8 is classified by what this kernel has
to supply for the requirement that clause pins — RFC 0013's R1–R9 and
the questions RFC 0005 §4.5 deferred — not against any earlier text:
*preserved* (the clause holds as stated, with no kernel-specific
reading needed), *re-derived* (the requirement is met, and the clause
is stated in this kernel's terms — scope tracking, revocation, the
delivery accounting — to say how), or *failed* (the behavioral
requirement itself unmet, the only classification that counts as
failure). **Preserved** — INV-ST2, INV-ST3, INV-ST5, INV-ST6,
INV-ST7. **Re-derived** — INV-ST1, INV-ST4, INV-ST8, whose requirement
mapping is below. **Failed** — none:

| Requirement | What this kernel supplies for it (RFC 0013's clause) |
| --- | --- |
| ST1: selection completeness and minimality (R1/R5) | a teardown for a prefix selects every run of every kind whose scope path begins with that prefix, and only those (INV-RC7's scope tracking is what makes "every kind" satisfiable) |
| ST4: no delivery after the application point; a buffered keyed quit does not quit (R1, cancel-beats-quit) | a selected run is revoked; its queued output — control-lane quit included — is never delivered, and the guarantee holds for every queued item regardless of how long the queue is or when the run's task exits (INV-RC5/INV-RC6) |
| ST8: the negative space, as a regression surface | teardown's unreachable set shrinks to two classes: what has crossed delivery or left the runtime's custody — messages already delivered to `update`, state mutations already applied, external side effects already performed, input already consumed by a stop-requested source — and input carrying no run origin (key-addressed routing) (RFC 0013 §3.8). Anonymous-run reachability and undelivered-output retraction are regression-tested, not negative space |

### 4.3 Orphan measures

Three orphan measures are pinned for a torn-down scope's output:
deliveries after the application point — zero (retraction);
instantaneous occupancy in bounded mode — split by lane: the
**data-lane** portion is capped by
the lane capacity (revoked items still occupy until dequeued, and
their dequeue does no `update` work), while a torn-down run's quit
occupies the **control lane, which is never bounded** (§3.3), so
total instantaneous occupancy carries no cap — the mandatory per-pass
drain (§3.5) keeps the control-lane portion transient, but the number
of quits arriving between two drains has no contract bound; total
occupancy in unbounded mode — unbounded over the
request-to-quiescence tail (sends have no contract bound; they are
filtered, not prevented). The composition layer observes none of it
(RFC 0013 R3: combinators return commands and never await quiescence).

### 4.4 Cleanup hooks

```rust
impl<M: Send + 'static> Command<M> {
    pub fn on_teardown(effect: CleanupEffect) -> Command<M>;
}
```

`on_teardown` registers a finalizer against the scope at the call
boundary (qualified by `scoped`/combinators like every other carrier).
Contract (INV-RC8):

- The finalizer runs **at most once**, started at the teardown
  application point that selects its scope (the start sits at the head
  of the stop-requested→quiesced interval RFC 0013 §1.2 places cleanup
  in; it runs concurrently with the torn-down runs' quiescence).
- A cleanup run emits no messages — it cannot orphan.
- Re-applying a teardown does not re-run consumed hooks (idempotence,
  INV-ST5-compatible).
- **Termination always wins**: running cleanup runs are cancelled like
  every runtime-owned task, and unfired registrations are discarded —
  termination is not a teardown and fires no hooks (RFC 0011 §4.4's
  postconditions take precedence; the zero-grace frame applies to
  cleanup itself, and a bounded grace window stays future work).

## 5. Subscription execution under the new core

RFC 0012's contract is consumed, not redesigned. Three clauses.

### 5.1 Barrier scope

The uniform quiescence barrier holds exactly as RFC 0012 §4 states it,
and its subjects are subscription runs only: a stop-requested,
not-yet-quiesced *subscription* task defers every new subscription
admission runtime-wide. Command and cleanup runs neither join the
barrier nor trigger it — they poll no input source, so they cannot
steal input, and extending the barrier to them would create a new
coupling with no hazard to close. The availability trade-off — one
slow-quiescing source defers unrelated children's admissions — remains
the explicitly accepted negative space (RFC 0013 R5's coupling,
RFC 0010's G-6 demand), and narrowing by declared conflict domains
stays rejected: the kernel cannot verify such declarations, and stolen
input cannot be recovered by delivery-side filtering.

### 5.2 The fourth stop cause

A teardown-issued stop joins RFC 0012 §3's stop causes (an amendment
there, §9 row 5). Its dirt classification: the quiescence of a
teardown-stopped task marks subscriptions dirty like any steady-state
stop — termination-driven quiescence stays excluded. The
declaration-removal pairing is structural through combinators; the
manual primitive applied to a still-declared subscription restarts it
at the next re-evaluation (RFC 0005 INV-13, untouched) and is
documented as self-defeating. Dirt sources stay exactly two (RFC 0011
§2.1): a batch that ran `update`, and the quiescence of a subscription
run stopped by a steady-state cause — a re-evaluation's removal or
replacement, or a scope teardown, per this clause. **A natural finish
marks no dirt** — a finished, still-declared subscription restarts at
the next re-evaluation, whenever one occurs (RFC 0005 INV-13 through
RFC 0012 §4.3) — **and the quiescence of command and cleanup runs
marks no dirt**: they are not re-evaluation subjects.

### 5.3 Stopping-pass defer

A re-evaluation that issues stop requests admits nothing in that same
pass, even if a stop quiesces while the pass is still running. This is
a clarification derived from INV-SE4/INV-SE5 (the pass's admissions
would otherwise race its own supersession window), not a semantic
amendment; it binds this kernel's re-evaluation and the §7.2 driver
alike.

## 6. Lifecycle

### 6.1 What is preserved

On the new kernel, unchanged and re-checked rather than re-stated:
construction inertness (INV-LC3, both entry types); the steady-state
phase order — input batches with one-item drain, frame passes with at
most one render then at most one re-evaluation, both observing the
pass's current state, arbitration negative space (INV-LC1, INV-LC2,
RFC 0011 §2.3); the two-stage termination postconditions with the
bounded settle discipline (INV-LC5–INV-LC7); the panic split —
producer panics contained for all producer kinds, cleanup runs now
included; driving-task application panics fail-fast (INV-LC8, §4.3);
driver exclusivity (INV-LC9 — the consuming `run(self)` on both entry
types, transitions serial and non-reentrant).

One clarification on the shutdown path: RFC 0006's
closure-observation guarantee is shutdown-scoped, and in the full
topology a producer whose send is blocked at termination is reclaimed
by the cancellation request itself — its future is dropped at the
await point, reaching both of RFC 0011 §4.4's postcondition stages
without first observing closure and returning an error. The
closure-observed → error → autonomous-stop path is a component-level
obligation of the producer body, verified at that layer (§13.1's
send-failure series states both layers).

### 6.2 Bootstrap: the synchronous init quit (an RFC 0011 amendment)

RFC 0011 §3.2's intake order stands — init dispatch, then initial
reconcile, then first render pending unconditionally — with one
change following from §3.3: an init command whose `Command::quit()`
part is present terminates **during the init dispatch**,
deterministically, before the initial reconcile runs and before any
subscription source starts. Under the previous contract that outcome
was one legal result of bootstrap arbitration; it is now pinned, and
the arbitration clause narrows to the init *effect's* output, initial
subscription output, and the first render. This is the RFC 0011
amendment §9 row 8 names.

### 6.3 Frame pacing removed

The driving loop reads no wall clock and owns no frame-rate
configuration (the capacity-wait event's duration measurement on the
producer send path is observability instrumentation under RFC 0006
§4.4, never a scheduling input).
Render cadence is pass-bounded: a render happens at most once per
pass, promptly after the pass that marked redraw pending — never
coalesced to a configured FPS, and never fired by elapsed time.
Applications that want time-driven redraw declare a `Timer`
subscription (the time axis stays source-side, RFC 0009). What this
supersedes: RFC 0006's frame-branch pacing facts and premises,
RFC 0007 INV-C5 and the frame-rate field, the `FrameRate` constructor
parameter (§2.4), and RFC 0011 §7's non-catch-up pacing premise. What
it preserves: idle costs nothing (a workless kernel parks; no
per-frame wakeups — the property the pacing scheduler's parking
provided), and flood cannot suppress rendering (§3.2). The cadence
*property* change — renders may occur more often than a configured
FPS under moderate input — is a deliberate, breaking behavior change
carried by this RFC's CHANGELOG.

## 7. Testing: two layers

### 7.1 The non-executing store (stages 1–2, preserved)

RFC 0008's TestStore keeps its contract unchanged: pure `update`
transitions, immediately ready effects, stage-2 virtual time, and the
non-execution boundary — the store never starts, polls, or restarts a
subscription source, and never spawns a task. Its command intake
consumes the same lowered parts the kernel consumes, now including
teardown entries and multi-keyed batch children (the RFC 0008
parity extension RFC 0013 §8 names). The store makes no
same-topology claim; that claim belongs to the driver alone.

### 7.2 The stage-3 driver

The `TestDriver` is the opt-in stage-3 driving surface RFC 0012 §6.2
reserves as a future RFC 0008 amendment — this RFC defines its
contract; its API body lands in that amendment. Contract:

- **Same topology (INV-RC13).** The driver constructs the runtime
  through the production construction path and drives the production
  kernel: the same task bookkeeping, the same producer execution path
  (spawn/poll/exit on the executor), the same lanes, the same phase
  machine and termination implementation. The prohibited shapes —
  manual run retention, reimplemented reconciliation, a mirrored quit
  route, manual effect polling, direct kernel injection — are not
  constructible through its API.
- **The driving differential is two seams plus inputs.** What differs
  from production, exhaustively: (i) pass-initiation arbitration —
  which ready wake source begins the next pass (§3.5) — is scripted
  instead of unbiased; (ii)
  producer send grants — a producer's send-intent is released by
  script instead of immediately; (iii) inputs and readiness are
  supplied by the application side (mock sources satisfying RFC 0012
  §6.1's template, test-controlled gates inside application-supplied
  effects). The production implementations of both seams are inert
  (unbiased selection, immediate grant) and observably equivalent to
  the seamless kernel; production scheduling stays uninstrumented.
- **Determinism, scoped (INV-RC14).** The driver guarantees scripted
  reproducibility: one script (inputs, readiness, arbitration
  choices, grants) yields one observation sequence, for a
  deterministic application — the driver introduces no nondeterminism
  of its own. Enqueue-order scripting is guaranteed only through the
  sequential handshake *grant → enqueue-acceptance confirmed → next
  grant*; raw grant order guarantees nothing and is not expressible
  in the API. The guaranteed observation sequence begins at the send
  gate: pre-gate send-intent records are a separate, non-guaranteed
  ledger, and neither ledger is a public transcript surface.
- **Pass-unit driving is the evidence surface for everything the
  driver can reach.** Acceptance and conformance evidence for a
  steady-state property is produced by pass-unit driving only: one
  driver step executes one whole pass through the production stage
  order (§3.5). Stage-granular probes — running a single stage in
  isolation — may exist as component-level white-box instruments, but
  they bypass the fixed stage order and sit outside the same-topology
  evidence surface: nothing observed through them is evidence for
  INV-RC13 or for §13.1's pass-unit series.
- **The park boundary is what the driver cannot reach, and
  `ParkProbe` is its named surface.** A driver step *begins* a pass,
  and a parked kernel is precisely one with no pass running and none
  beginning until a source arrives; the driver's first differential
  above — scripted pass initiation — replaces the very mechanism
  INV-RC16 constrains, so no pass-unit step can witness a park or its
  arming. `ParkProbe` polls the production driving future directly,
  with a waker of its own, and observes whether the loop parks, which
  sources it armed, and which arrival wakes it. It is not a third
  runtime seam and not a second driver: it scripts nothing inside the
  kernel, adds no branch, and drives the same production loop — what
  it supplies is the waker and the poll, and nothing else. **Its
  evidence scope is INV-RC16's arming and wake claims, and nothing
  else**: a `ParkProbe` observation is never evidence for INV-RC13's
  same-topology claim, for INV-RC14's scripted determinism, for the
  pass stage order of §3.5, or for production pass initiation. §13.1
  names the three series it carries; every other series is pass-unit
  driven.
- **The citation rule.** A driver-established order is never evidence
  of a production order — the RFC 0008 §4.2 rule, generalized — and
  the scope above is part of the same rule: a `ParkProbe`-established
  fact is evidence for the park contract alone, never for a pass
  order, a topology claim, or a production arbitration. Which source
  production picks among several ready at once stays unobserved here
  (§3.5 pins the policy, not the occasion).
- **Scope of the determinism claim.** The handshake's acceptance
  confirmation is the post-send acknowledgement, which is the form an
  executor-independent or bounded-lane extension requires; the
  *claim* this RFC makes is nevertheless scoped to a current-thread
  executor and unbounded lanes, the verified range. A bounded
  extension additionally requires **driver progress** (the driver
  stays steppable while a grant's acceptance is outstanding, so a
  capacity-blocked send cannot deadlock the handshake) and **ack
  correlation** (at most one outstanding grant per origin, or an
  explicit correlation of each grant to its exact commit; the next
  grant to an origin only after the previous acceptance) — §13.3.

### 7.3 What each layer claims

The store claims transition-level determinism with no execution; the
driver claims production-topology execution with scripted
arbitration. Neither is a second harness in RFC 0010 §5.2 (c)'s
prohibited sense: the store consumes the kernel's own lowering, and
the driver drives the kernel itself.

## 8. Composition requirements (RFC 0010 §5.2, the C-15 register)

- **(a) Automatic scope application** — satisfied by §2.5 (INV-RC2):
  scoping is structural in the combinators; no user anchor exists to
  forget or double-apply.
- **(b) Identity-law preservation** — satisfied: RFC 0005
  INV-14–INV-21 hold through the adapter and combinators (§2.5); the
  two coverage amendments (INV-18, INV-20) extend the laws to new
  carriers without changing their bodies (§9).
- **(c) TestStore reuse** — satisfied in the two-layer form of §7: the
  existing store tests adapter and composed programs through the same
  intake, unchanged; the stage-3 driver is the additive layer RFC 0012
  §6.2 already reserves, not a second harness.
- **(d) Phase-machine sharing** — satisfied: one phase machine drives
  both entry types (§6.1); the adapter introduces no lifecycle (§2.2).
- **(e) `cancel_scope` precedence** — satisfied by inclusion: the
  teardown contract (§4) accompanies this RFC as its own section and
  the RFC 0013 successor mapping (§4.2); the gating relation is
  discharged by co-landing.
- **(f) Quiescence-barrier non-interference** — satisfied: combinators
  aggregate declarations and neither observe nor await quiescence
  (§4.3, §5.1); the teardown stop cause is an amendment *to* RFC 0012
  made explicitly (§9 row 5), not a silent composition-side need.

## 9. Supersessions and amendments

The spike tier of §13.1 having passed, these rows land on their owner
documents with this RFC's acceptance, ahead of the mainlining that
tier's open half still gates. Each row names the owner document that
edits in place.

| # | Owner | Kind | Object |
| --- | --- | --- | --- |
| 1 | RFC 0003 | supersede | delivery topology: private keyed channels and receiver-based statements (INV-1's shared path, INV-2, the receiver clauses of INV-3/INV-4/INV-6/INV-7) → origin revocation on one lane (§3.1); INV-8's token rule, INV-10, INV-12 preserved; INV-9 → origin-liveness successor (§3.3); INV-16 → kernel park contract |
| 2 | RFC 0003 / RFC 0006 / RFC 0007 | supersede + property loss | INV-14 shared-first pull and RFC 0006's two delivery classes → single FIFO (§3.1); the broad cancel-opportunity property is not preserved (§3.2) — recorded as a user-visible property loss. The private keyed channels go with them, and so does everything stated per channel: RFC 0007's `keyed_channel_capacity` leaves the public surface, RFC 0006 INV-L1's `m × keyed_channel_capacity` term has nothing to sum over, and INV-L9's per-command isolation — one key's full channel never delaying admission into another key's or into the shared channel — is **not preserved**: every producer awaiting capacity awaits the one data lane's. A second user-visible property loss, carried by this RFC's CHANGELOG |
| 3 | RFC 0003 / RFC 0005 | supersede (breaking) | INV-11 batch folding → multi-keyed lowering (§3.4); RFC 0005 INV-20's "scoping does not bypass batch" restated over distribution |
| 4 | RFC 0006 / RFC 0007 / RFC 0011 §7 | supersede (breaking) | the kernel's wall-clock reads: configured frame pacing — frame-branch pacing facts, INV-C5, the frame-rate config field and constructor parameter, the non-catch-up premise → §6.3's pass-bounded cadence — and the time-capped batching window (INV-L6's default) → an always-finite count cap (§3.5; `batch_max_messages = None` comes to mean the kernel's default count cap, an RFC 0007 doc change in the same cluster) |
| 5 | RFC 0012 | additive amendment + clarification | fourth stop cause (teardown) with its dirt classification (§5.2); stopping-pass defer recorded as clarification (§5.3); the barrier's subjects clarified — subscription runs only, command and cleanup runs neither joining nor triggering it (§5.1), the non-participation half enforced as INV-RC12's behavioral row |
| 6 | RFC 0005 | amendment (one clause each) | INV-18 coverage extends to teardown prefixes and cleanup registrations; RFC 0005 INV-17 (map/scope commutation) and RFC 0003 INV-12 (`map` metadata propagation) unchanged |
| 7 | RFC 0013 | successor revision | the teardown contract re-derived on §4's operation: selection over every run kind (its §3), immediate subscription stop (its §4), cleanup participation (its §5), resolved questions (its §9); INV-ST classification and mapping per §4.2 |
| 8 | RFC 0011 | amendment | bootstrap-quit short-circuit (§6.2), narrowing INV-LC4's arbitration clause by that one case; INV-LC5's classification statement scoped to the facade entry with the advanced entry's `Exit` classification added; §2.3's negative space narrowed by §3.5's fixed pass stages (exit reflection, control drain, and the frame step are no longer freely interleaved branches); INV-LC8's producer-kind inventory extended to cleanup runs (§4.4, §6.1); §7 premises re-derived on the seam vocabulary — the always-armed quit branch splitting into a strengthened drain and a superseded arming half whose successor is INV-RC16, which also takes §7's parking premise as contract; pass initiation's production policy stays unbiased and becomes normative (§3.5) |
| 9 | RFC 0006 | vocabulary amendment | INV-L13 schema: `shared_pending` reads as the data lane's residual occupancy; `channel`'s value domain becomes the single value `"data"`; gauge kind counts map (`unkeyed_commands` = anonymous runs, `keyed_commands` = keyed runs); firing conditions unchanged |
| 10 | RFC 0006 | supersede + clarification | INV-L10 keyed-quit ordering and INV-L11 shared-first precedence → §3.3's successor statement (backlog-independent, cancellable-until-applied, no same-run ordering); R4's backlog independence preserved for the control lane; §4.3's shutdown closure-observation guarantee split into its two layers — the full-topology producer reclaimed by the cancellation request, and the component-level obligation of the producer body (§6.1); INV-L4's acceptance re-derivation is §13.5 |
| 11 | RFC 0008 | amendment (additive) | the stage-3 driver (§7.2), gated on this RFC; store parity extension to teardown entries and batch children (§7.1) |
| 12 | RFC 0012 | amendment | INV-SE6's purity obligation generalized from `Application::subscriptions` to the `subscriptions` of every reducer the runtime drives — the adapter's and each composed one's — as one clause with one owner of record: the declared set is a pure function of state, evaluated at any re-evaluation frequency (§2.1) |

Count: twelve rows — five supersessions (rows 1, 2, 3, 4 — the public
constructor change belongs to row 4's cluster and the keyed-capacity
removal to row 2's — and 10), six amendments (rows 5, 6, 8, 9, 11,
12), and one successor revision (row 7); five plus six plus one is
the twelve. Two rows carry a clarification beside their primary kind
— row 10's shutdown closure-observation guarantee read as its two
layers, and row 5's barrier subjects — and neither adds an entry: a
clarification rides the row whose object it belongs to, so the count
above is by primary kind and the row total is what the table shows.

Preserved and worth naming: the effect-DI negative space (RFC 0012
INV-SE8 — the driving seams are not an effect-executor abstraction:
they gate branch choice
and send release, never effect execution or dependency resolution),
`RuntimeConfig` move-only (RFC 0007), the `Message` boundary
(RFC 0010 §7.1), and the uniform barrier (§5.1).

## 10. Premises and mechanism (informative)

Nothing here is contract. The reference kernel: a single driving task
owns application state and a run registry (one entry per producer run:
scope path, token, lifecycle phase, revocation flag, abort handle),
with a join-set for exit observation; per-run delivery accounting
(reservation → commit → release around each send; dequeue-side
decrement; an entry outlives its task until its committed queue
residue drains) is what makes §3.1's "every queued item" claim
implementable — a from-scratch implementation may choose any other
accounting that preserves the observable retraction and quiescence
contracts. The cleanup ledger holds unfired finalizers per scope. The
two driving seams are a pass-initiation arbitration policy
(production: unbiased selection among ready wake sources, the control
lane always armed as one) and a send gate (production: immediate);
the pass stages themselves are fixed (§3.5) and not arbitrated. The
scripted arbitration covers exactly the wake sources §3.5 arms —
data-lane readiness, control-lane arrival, and producer-exit or
subscription-quiescence notification. The frame step is not among
them, being consumed inside the pass that marks its work, and the
exit-observation source is contract there (INV-RC16) rather than the
driver-side extension it was before that arming was stated. Load gauges
follow RFC 0006 §4.4 with §9 row 9's vocabulary. None of these shapes
is pinned; the invariants of §12 are.

## 11. Adversarial models considered

- *Facade special-casing* — a kernel with an `Application` fast path
  passes API-level tests; excluded by INV-RC1's inventory walk (every
  owner row and phase step identical) and the shared-path behavioral
  checks.
- *Diff-based removal detection* — same-update remove-and-reinsert of
  one key produces no state diff, so the old instance's runs leak;
  excluded by the journal contract (INV-RC3 records removals, not
  diffs).
- *Fold-era batch* — a lowering that folds child spawn keys satisfies
  every single-command test; excluded by INV-RC4's batch
  remove-and-reinsert test (old instance torn down, new instance's
  keyed spawn fresh, in one command).
- *Cancel-phase cleanup registration* — registering `on_teardown` in
  the cancel phase lets a same-command teardown consume the *new*
  registration; excluded by §3.4's spawn-phase rule and its test.
- *Filter-at-update instead of retraction* — dropping revoked output
  after pulling it into `update`'s batch fails nothing observable?
  It does: the batch event's `updated` count and the one-item-drain
  dispatch of a revoked item's neighbors differ; the INV-RC5 checks
  assert no `update` invocation, not merely no state change.
- *Tombstone expiry* — an implementation that forgets a revoked run
  after its task exits delivers late-queued output; excluded by
  INV-RC5's "every queued item regardless of when the task exits"
  quantifier and its adversarial test (revoke, let the task exit,
  deliver later).
- *Barrier over-extension* — including command runs in the barrier
  passes every subscription test while coupling admissions to slow
  effects; excluded by §5.1's scope statement and its test (a
  stop-requested command run defers no admission).
- *Driver with a parallel model* — a store-like driver that
  re-implements reconciliation passes transition tests; excluded by
  INV-RC13 (unconstructible prohibited shapes; the driver's checks
  run against production seams).
- *Raw-grant ordering* — `grant(A); grant(B)` without awaiting
  acceptance assumes an enqueue order the executor does not promise;
  excluded by the handshake-only API (INV-RC14).
- *View combinator* (rejected alternative, §2.1): a `view` on
  `Reducer` with layout composition rules — rejected because layout
  policy is application concern; no correctness property needs it,
  and the root-view form keeps the trait surface minimal.
- *Same-domain sequential combinator* (rejected alternative, §2.5): a
  `combine(self, other)` running both reducers on one message
  requires either a `Clone` bound on `Message` (silently widening the
  boundary §2.1 preserves) or a continuation-shaped `reduce` that
  returns the unconsumed message (complicating the core signature for
  one combinator). The pattern is an ordinary function call inside
  one `reduce`; no combinator is provided.
- *Input-priority scheduler* — an arbitration free to prefer a ready
  input branch satisfies every per-message test while starving quit
  application and rendering under continuous input readiness; an
  unbiased pick still yields only a probabilistic bound. Excluded by
  §3.5's fixed pass stages: the quit and render bounds are
  constructive, and INV-RC9/INV-RC10's flood rows exercise them under
  scripted continuous readiness. A capless batch stage is the same
  adversary in another guise — one batch that never ends defers the
  later stages forever — excluded by §3.5's count cap, which every
  configuration has: a batch ends after a finite prefix of the ready
  input, so the same pass reaches its frame stage and the next its
  control drain. What that cap's value is stays mechanism, and no
  wall-clock or practical bound on it is claimed — what this entry
  excludes is the stage that never ends, not a latency figure.
- *Batch-first pass* — an implementation that orders the input batch
  before the control drain satisfies an "applied in the first pass
  after arrival" reading while a quit that was already waiting when
  the pass began is outrun by a full batch; excluded by §3.5's stage
  order and INV-RC9's pass-start row (zero inputs processed). Its
  test-side twin is the stage-granular driver probe, which can
  fabricate exactly that permuted execution — the reason probes are
  outside the evidence surface (§7.2, INV-RC13).

Excluded claims (minimal-contract pass): a per-run
quiescence-follows-request invariant is not restated — RFC 0011 §4.4's
two-stage model owns it; delivery losslessness and backpressure are
not restated — RFC 0006 owns them, and what §3.1 changes there is the
deliverability decision and the lane count backpressure is stated over
(§9 row 2); subscription admission rules are not
restated — RFC 0012 INV-SE2–SE5 own them, §5 adds the fourth cause
and the defer clarification; the configured batch cap's counting
semantics stay RFC 0006 INV-L12's (what changes is only that a cap
always exists — §3.5, §9 row 4). A dedicated "no second phase
machine" invariant
was dropped as implied by INV-RC1 (single execution path) plus
INV-LC9; kept separate is INV-RC13's same-topology claim, which
INV-RC1 does not imply (it quantifies over the test topology, not the
facade).

## 12. Invariants

Enforcement classes per the pre-review checklist. The behavioral
checks divide into two tiers (§13.1): the **spike tier** — the four
kernel claims and the twelve-series conformance suite, which gated this
RFC's acceptance and ran on a prototype kernel — and the
**implementation-acceptance tier** — every remaining behavioral row
below, which gates implementation mainlining, not acceptance. Both
tiers remain the regression suite afterward.

- **INV-RC1 — single execution path.** For every kernel concern —
  state ownership, input delivery, quit delivery, effect-task
  ownership and bookkeeping, cancellation state, subscription-task
  ownership, task body policy, frame and render, observability, time,
  identity — and every phase step of §6, an `Application`-adapted
  program and a composed program execute the same code; the facade
  contributes mapping calls only. Structural (review of the adapter
  and kernel entry: no `Application`-typed branch below the adapter)
  with a behavioral neighbor: identical observable traces for an
  `Application` counter and its hand-written `Program` equivalent
  under one script.
- **INV-RC2 — automatic scope application.** A combinator qualifies
  every identity-bearing carrier of its child's returned command
  (spawn keys, cancel IDs, teardown prefixes, cleanup registrations)
  and its child's subscription declarations with the boundary's
  segment; equal local IDs under sibling scopes never alias.
  Behavioral at the lowering seam: nested-combinator programs assert
  the qualified identities, including the sibling-isolation case.
- **INV-RC3 — journal completeness.** Every removal shape —
  `remove`, `dismiss`, occupied-key insert, occupied-slot present —
  yields exactly one teardown for the removed instance in the same
  update's returned command; same-update reinsertion still yields the
  old instance's teardown and a fresh successor. Behavioral: the
  journal drain is asserted per shape, including the no-diff
  remove-reinsert adversary.
- **INV-RC4 — multi-keyed lowering.** Batch children lower to
  independent entries; the combined cancel phase precedes every spawn
  of the same command; the §3.4 interaction rules hold. Behavioral:
  the batch remove-and-reinsert test, the same-key-twice test, the
  teardown-plus-reregister test.
- **INV-RC5 — strict revocation.** From a revocation's application
  point (cancel, supersession, teardown, termination), no output of
  the revoked run is delivered to `update` — buffered before or sent
  after, message or quit — for every queued item, regardless of queue
  depth or the run's task-exit timing; a naturally finished, live
  run's buffered output is still delivered. Behavioral, on both lane
  modes: revoke-then-deliver-later sequences, the buffered-quit case,
  the late-task-exit adversary, the natural-finish control. The
  bounded-mode half runs as §13.1's `bounded-lane revocation` series,
  which scripts the bounded lane through the grant handshake for
  enqueue order only and claims no further bounded-lane determinism
  (§13.3).
- **INV-RC6 — retraction.** INV-RC5 applied to a torn-down scope's
  subscription output: after the teardown's application point, zero
  deliveries from selected runs. Behavioral (kept separate from
  INV-RC5 because it quantifies over the subscription kind the old
  contract could not reach).
- **INV-RC7 — anonymous-run reachability.** An unkeyed effect spawned
  through a composition boundary is selected by its scope's teardown;
  no composed child effect is unreachable. Behavioral: anonymous
  child effect torn down mid-flight, output retracted.
- **INV-RC8 — cleanup.** §4.4's clauses: at-most-once, started at the
  application point, no messages, consumed-not-rerun, termination
  discards unfired hooks and cancels running ones. Behavioral per
  clause; the termination row reuses the settle-loop discipline.
- **INV-RC9 — quit routes.** An `update`-returned quit terminates at
  its dispatch's completion with no intervening input processed. A
  producer quit is applied at the first control drain at or after its
  arrival (§3.5): a quit that has arrived when a pass begins is
  applied by that pass's control drain with **zero** further inputs
  processed; a quit arriving later in a pass is applied by the next
  pass's control drain, preceded only by the in-progress batch's
  remainder (bounded by the batch cap) — in every case before any
  input batch that begins after its arrival — unless its origin is
  revoked, in which case it is discarded. Producer quits are
  independent of the data lane's backlog and capacity. Behavioral:
  the synchronous case (no interleaved input); the pass-start case
  (quit committed before the pass with a full input batch ready —
  applied with zero inputs processed); the mid-batch case (only the
  remainder precedes it); the cancel-beats-quit case; the
  flooded-data-lane case (bounds hold under scripted continuous
  input readiness).
- **INV-RC10 — flood properties.** Under a continuously ready
  producer: every other producer's enqueued output is delivered after
  exactly the FIFO prefix ahead of it — no starvation, a
  backlog-proportional number of passes, each consuming up to the
  batch cap — and a redraw marked by a batch is rendered before the
  next input batch begins (§3.5: flood cannot suppress rendering,
  deterministically). Behavioral (a scripted flood with an interposed
  probe and a pending redraw); wall-clock latency *numbers* stay
  §13.5's statistical work.
- **INV-RC11 — lifecycle conformance.** The RFC 0011 invariant suite
  (INV-LC1–INV-LC9) passes on the new kernel, with §6.2's amended
  bootstrap row (init quit: reconcile never runs, no source starts)
  and §6.3's premise substitution. Behavioral: RFC 0011's own test
  rows re-run against the kernel, plus the init-quit row.
- **INV-RC12 — barrier scope, defer, and dirt sources.** RFC 0012's
  admission suite passes; additionally (a) a stop-requested command or
  cleanup run defers no subscription admission, (b) dirt is marked
  only by §5.2's two sources — the quiescence of a naturally finished
  subscription run, of a command run, or of a cleanup run marks
  none — and (c) a stop-issuing re-evaluation admits zero in its own
  pass, even when the stop quiesces while that pass is still running.
  Enforcement splits by what a check can construct: (a) and (b) are
  **behavioral** at the reconcile seam, one row per non-participating
  run kind and one per non-dirt source; (c) is **structural** at the
  same seam — the reconcile path takes no second admission attempt
  after issuing its stops, so a quiescence observed while the pass
  runs has no site to admit into — because a mid-pass quiescence is
  not constructible on the single-threaded executor those behavioral
  rows use. RFC 0012 §4.2 states the same split from the owner side.
- **INV-RC13 — driver topology.** The driver constructs through the
  production path and shares bookkeeping, producer execution, lanes,
  and termination with production; the five prohibited shapes are not
  constructible from its public API. Structural (API surface review;
  the prohibited shapes have no constructor) plus behavioral: the
  conformance suite runs through the driver's **pass-unit steps** —
  each executing §3.5's full stage order — against production seams,
  and a stage-sharing review walks the canonical stage set.
  Stage-granular probes contribute nothing here (§7.2: outside the
  evidence surface).
- **INV-RC14 — scripted determinism.** One script yields one
  observation sequence across repeated runs (deterministic
  application premise; current-thread executor, unbounded lanes);
  enqueue-order guarantees exist only through the
  grant-then-acceptance handshake; pre-gate records are excluded from
  the guaranteed sequence. Behavioral: repeat-stability over the
  conformance suite; the raw-grant shape is unrepresentable
  (structural half).
- **INV-RC15 — lane topology.** The runtime owns exactly two delivery
  lanes: one FIFO data lane carrying every producer's message
  output — keyed command, anonymous command, and subscription
  alike — and one control lane carrying producer-originated quits and
  nothing else (§3.1, §3.3). No per-run, per-key, or per-class message
  lane exists, so nothing per producer can be sized, isolated, or
  prioritized — the two property losses §9 row 2 records follow from
  this row. Structural at the construction and send sites (one
  data-lane sender reaching every producer kind; no second message
  channel constructible) with a behavioral neighbor at the
  observability seam: under a bounded lane, blocked sends from a
  subscription, an anonymous command, and a keyed command all report
  the capacity-wait event's `channel` as `"data"` (RFC 0006 §4.4's
  schema as §9 row 9 amends it).
- **INV-RC16 — park and wake.** A parked kernel holds a registered
  waker on every source that can create a pass's work — data-lane
  readiness, control-lane arrival, and producer-exit or
  subscription-quiescence notification — and the arrival of any one of
  them begins a pass (§3.5's wake arming). A kernel that stays parked
  while one of them has arrived and remains unconsumed is
  non-conforming; the workless park §6.3 preserves is the case where
  none has. This is what keeps INV-RC9's quit bound and INV-RC12's
  admission rows from holding vacuously, and it is the successor of
  RFC 0003 INV-16's arming half (§9 row 1) and of the always-armed
  select branch RFC 0006 R4 relied on (§9 row 10). Structural at the
  park site — review that the parked future registers the current
  waker with each member of the set, since no finite test proves a
  registration present for the source it did not exercise — with one
  behavioral row per source, all three on the `ParkProbe` surface
  §7.2 names and each scripted from a genuinely parked kernel with no
  other work pending: `parked data-lane wake`,
  `parked control-quit wake`, and
  `parked subscription-quiescence wake` (§13.1). §13.1's pass-unit
  `idle wake` series exercises the woken pass's exit reflection rather
  than the park boundary, and is not a row of this invariant.

Surface–invariant coverage: `Reducer`/`Program`/adapter (INV-RC1;
purity via RFC 0012 INV-SE6's transfer, §2.1), combinators and
journals (INV-RC2/INV-RC3), `Keyed`/`Slot` (INV-RC3), batch lowering
(INV-RC4), lane topology (INV-RC15), delivery and revocation
(INV-RC5/INV-RC6/INV-RC10), park and wake (INV-RC16),
teardown and `on_teardown` (INV-RC7/INV-RC8, §4.2's successor table),
quit (INV-RC9), the two entry points and `Exit` (INV-RC1, INV-RC11 —
the facade's result contract is RFC 0011 INV-LC5's, preserved),
constructor changes (§9 row 4 — covered by INV-RC11's conformance
rows), barrier clauses (INV-RC12), driver (INV-RC13/INV-RC14). The
per-command capacity control's removal (§9 row 2) needs no invariant
of its own beyond INV-RC15: that row's two-lane topology is what
leaves nothing per command to size or isolate.
`ScopeValue` carries no separate invariant: it is the RFC 0005
segment-value contract restated as a bound.

## 13. Open questions

### 13.1 The acceptance gate: spike tier met, implementation tier open

*Spike tier* — the gate this RFC's acceptance passed, demonstrated on
a prototype kernel, four claims plus the suite: the
send-acknowledgement grant handshake (§7.2); the delivery-accounting
soundness behind retraction (§3.1), including a concurrency check of
the multi-writer accounting (loom or equivalent); revocation filtering
end to end; the driver's same-topology stage sharing; and the
conformance suite — **twelve series in two groups**, all green and
repeat-stable.

**Pass-unit driven** (nine), each driver step executing §3.5's full
stage order (§7.2): cancel vs buffered output; stop/restart safe
window; simultaneous readiness under both script faces; both quit
semantics; both panic classes; shutdown-scoped send failure — full
topology: a blocked sender reclaimed by cancellation with the
two-stage postconditions; component level: closure observation →
error → autonomous stop (§6.1); idle wake; termination under owned
work through every cause; `bounded-lane revocation`, which runs
INV-RC5's bounded-lane half over the single data lane.

**`ParkProbe` driven** (three), each scripted from a genuinely parked
kernel with no other work pending and each evidence for INV-RC16
alone, at the scope §7.2 states: `parked data-lane wake`;
`parked control-quit wake`;
`parked subscription-quiescence wake` — one series per wake source
that invariant arms. The park boundary is unreachable by pass-unit
driving, which is why these three carry their own instrument rather
than a weaker form of the same one; stage-granular probes are outside
both groups. *Implementation-acceptance tier* — open, and what it gates
is mainlining, not acceptance: cleanup hooks (INV-RC8), the full
combinator surface (INV-RC2–INV-RC4), the observability vocabulary
mapping (§9 row 9), the production arbitration policy (§3.5's unbiased
pass initiation, whose check is the structural review named there),
and the remaining §12 behavioral rows.
**Order**: the spike tier precedes acceptance, acceptance precedes
every §9 edit, and the open tier precedes mainlining — so the §9
supersessions stand on the owner documents while the kernel itself
stays outside the crate until that tier closes. A failure in the open
tier stops mainlining and reopens the design of whatever it failed;
whether it also reaches the architecture selection is RFC 0010 §1.9's
counterexample-grade question, as it is for any later finding.

### 13.2 Driver API body

The concrete `TestDriver` surface lands in the RFC 0008 stage-3
amendment (§7.2 pins its contract; §9 row 11). Resolves there.

### 13.3 Bounded-lane scripted determinism

Extending §7.2's determinism claim to bounded lanes and
executor-independent scheduling — the acknowledgement form is already
compatible — needs its own verification pass and the two protocol
conditions §7.2 names: **driver progress** (the driver stays steppable
while a grant's acceptance is outstanding, so a capacity-blocked send
cannot deadlock the handshake — the kernel must be able to drain the
lane the pending send waits on) and **ack correlation** (at most one
outstanding grant per origin, or an explicit correlation of each grant
to its exact commit; the next grant to an origin only after the
previous acceptance). Until then the claim keeps its verified scope:
§13.1's `bounded-lane revocation` series scripts a bounded lane under
exactly the two conditions above and witnesses INV-RC5 there, which is
what that invariant's both-lane-modes check consumes — it is evidence
for revocation under a bounded lane, never for a general bounded-lane
determinism claim. Resolves as an amendment to the driving contract in
the RFC 0008 amendment.

### 13.4 External driving surface

A public host-driven step API (RFC 0011 §6's additive room) — shape,
pacing responsibilities, park/wake integration. Future RFC.

### 13.5 Load-acceptance re-derivation

RFC 0006's statistical acceptance (INV-L4 formulation and the
bounded-mode scenario set) re-measured on the new topology, with the
same reference-environment discipline. Implementation-stage work under
RFC 0006's ownership; until it lands, §3.3's latency statements are
the contract and no numeric threshold is claimed here.

## 14. References

- RFC 0003 — command cancellation: the strict-cancel family, INV-8's
  token rule, INV-9/INV-10/INV-11/INV-14, §4.3's dispatch phases.
- RFC 0005 — structural lifecycle identity: INV-12–INV-21, §4.3–§4.5.
- RFC 0006 — runtime load control: delivery classes, R4/INV-L4,
  INV-L10/INV-L11, INV-L12, INV-L13, §4.2/§4.4.
- RFC 0007 — RuntimeConfig: INV-C5, the move-only property.
- RFC 0008 — TestStore: stages 1–2, INV-T3/INV-T7/INV-T11, §4.2's
  citation rule.
- RFC 0009 — Clock DI: the time axis §6.3 leaves source-side.
- RFC 0010 — runtime consolidation: §1.8/§1.9, §5.1/§5.2 (C-15),
  §7.1.
- RFC 0011 — runtime lifecycle: §2–§6, INV-LC1–INV-LC9, §7's
  premises.
- RFC 0012 — subscription execution: §2–§9, INV-SE1–INV-SE8.
- RFC 0013 — scope teardown: §3–§6, §9 (resolved questions),
  INV-ST1–ST8, R1–R9.
- `src/runtime.rs`, `src/runtime/core.rs`,
  `src/runtime/keyed_commands.rs`, `src/subscription.rs`,
  `src/command/core.rs` — the surfaces §9's supersessions replace.
- `docs/rfcs/pre-review-checklist.md` — enforcement-class definitions.
