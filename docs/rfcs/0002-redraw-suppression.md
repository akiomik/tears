# RFC 0002: Redraw Suppression (message / redraw separation)

- Status: Approved
- Target: additive, non-breaking (next minor)
- Scope: an opt-out API for an `update` to declare "this message does not need a redraw"
- Feature flag: none (core runtime)
- CHANGELOG: `Added` (one PR, contract tests added, existing invariants unchanged)

## Summary

Today every processed message unconditionally forces a redraw. On
high-message-rate applications (e.g. a Nostr TUI client) most incoming events
change off-screen or background state and do not affect the visible viewport,
yet each one triggers a full `view()` rebuild on the next frame tick. This RFC
adds an **opt-out** path so an `update` can declare that the message it just
handled needs no redraw, gating the runtime's `needs_redraw` flag.

The change is purely additive: the default behavior (every message redraws) is
unchanged, so no existing application is affected. This is the TUI analogue of
Iced 0.14's `RedrawRequest::Wait` (reactive rendering) — a default-render,
opt-out-when-idle model, chosen over egui's opt-in `request_repaint()` model
because the opt-out side is the safe one.

## 1. Background and Motivation

A `samply` + flamegraph profile of a real application (the
[`nostui`](https://github.com/akiomik/nostui) Nostr client) integrating the
runtime showed rendering dominates the main thread:
`process_frame_tick` (render) = 89% of main-thread samples, of which `view()`
build is the bulk and ~11% is `/dev/tty` size queries that ratatui issues once
per `terminal.draw()` (a subset of the render cost, not additive to it). The
whole 89% scales with the **number of frames rendered**, so gating the redraw
shrinks the `view()` build and the `/dev/tty` queries together.

The root is in `process_message_batch` (`src/runtime.rs`): after calling
`update()` it sets `needs_redraw = true` unconditionally. The application has no
way to say "this message did not change the visible view." Relay events stream
in continuously; most append to off-screen timeline, update metadata, or feed
background aggregation, but every one still forces a full `view()` rebuild.
ratatui's terminal flush is diffed (`BufferDiff`), so the *write* is already
skipped when nothing changes — but the `view()` **build** that produces the
buffer runs in full regardless. Render cost is therefore proportional to *all*
messages, not to *visible-changing* messages.

The earlier idle-wakeup work (frame branch gated by `should_process_frame`)
removed wasted frames while **idle**. This RFC removes over-rendering while
**active**. The two are independent and complementary.

### 1.1 Applicability study (nostui)

A source-level study of the profiled app confirmed the win is real but reframed
*which* messages drive it:

- **The dominant redraw source is nostui's own frame-rate `Tick`, not relay
  events.** A `Timer` subscription fires ~16×/s and its update returns
  `Command::none()`, so today's unconditional `needs_redraw` re-dirties **every
  frame regardless of Nostr activity**. This also **defeats the
  `should_process_frame` idle gating**: the loop never goes idle even on a
  static screen. So `Tick` is the poster-child opt-out — and it is exactly
  §5.1's `Command::none().without_redraw()` (no side effect), confirming that
  example is a primary use case, not a curiosity.

- **Tick suppression is the precondition; off-screen relay suppression is an
  additive second win.** `needs_redraw` is a persistent per-frame flag (§5.2),
  and `Tick` re-sets it every frame, so opting out relay events **without** also
  opting out `Tick` yields almost nothing. Once `Tick` is opted out, off-screen
  events (e.g. notes routed to a non-active tab) can additionally opt out; a few
  categories that can touch the visible view stay redrawing (INV-7 recovery
  covers rare stale cases).

Estimated effect (best case, idle/low-activity — the common "left open" TUI
state; no change while scrolling or updating the visible timeline): render rate
drops from a constant ~16 FPS toward the rate of the one remaining per-frame
change, with further savings proportional to the off-screen fraction during
event bursts. Caveat: suppressing `Tick` freezes any FPS overlay while idle;
since FPS measures render rate and we are deliberately not rendering, that metric
loses meaning and such an overlay should be reconsidered alongside adopting the
opt-out.

## 2. Non-negotiables

**(A) Default behavior is unchanged.** Returning `Command::none()` /
`perform` / `message` from `update` must still redraw exactly as today.
Suppression is opt-out only, so no existing application changes behavior.

**(B) Redraw suppression and subscription re-evaluation are separate
concerns.** `subscriptions()` is a pure function of application state and may
change even for a message that does not alter the visible view. Suppressing a
redraw must **not** suppress subscription re-evaluation. This RFC gates only
`needs_redraw`; `subscriptions_dirty` remains unconditional (§5.3). A future,
separately-opted-out subscription-skip is possible but out of scope (§9).

## 3. Goals and Non-Goals

### Goals

- Let an `update` declare "no redraw needed" for the message it just handled.
- Make render count proportional to visible-changing messages, not all
  messages.
- Keep the public `Action` / `Command` contract additive (no breaking change).
- Update the public docs of `Command` / `none` / `is_none` / `is_some` / `batch`
  so "no side-effect stream" is not conflated with "does nothing" once a
  stream-less command can carry a runtime directive (§5.6). This is a required
  deliverable of the PR, not a follow-up.

### Non-Goals

- **Partial memoization** (Elm `Html.Lazy` equivalent — caching sub-widget
  builds). This RFC does whole-frame suppression only; partial memoization is a
  heavier, separable future step (§9).
- **Subscription re-evaluation skipping.** Out of scope by non-negotiable (B).
- **Adaptive / capped frame rate.** `FrameRate` already exists; tuning it is an
  application concern and orthogonal to this RFC.

## 4. Why the signal is a `Command` modifier (not an `Action`)

`Command` is not merely "a side effect." It is already the channel through which
`update` returns **runtime directives**: `Command::effect(Action::Quit)` tells
the runtime to stop — a control instruction, not a side effect. So a redraw
policy ("this update did not change the visible view") is the same *category* as
`Quit`: a runtime directive carried by the value `update` returns. It belongs on
`Command` natively, not as a bolted-on foreign concern.

But `Quit` and a redraw policy differ in *shape*, and shape decides the
container:

- **`Quit` is a discrete, terminal action** — it happens or it does not, and it
  is mutually exclusive with continuing. That fits a value the command's stream
  *yields* (the internal `Action::Quit` item).
- **A redraw policy is an attribute of the update's outcome** — a boolean that
  must **compose with side effects** (`perform(fetch)` *and* "do not redraw
  this update"). That fits a field/modifier on the command itself, not a stream
  item.

This is why an **`Action::SkipRedraw` variant is rejected** (see §8), and the
rejection is mechanical, not aesthetic:

- **Timing.** `needs_redraw` is set **synchronously** right after `update()`
  returns in `process_message_batch` (`src/runtime.rs`), *before* the command's
  stream is spawned. An `Action` is drained **asynchronously** later in the
  detached task inside `enqueue_command`. A skip signal delivered as an `Action`
  arrives after the redraw decision (and possibly the frame) has already
  happened.
- **Composition.** Modeling "run a fetch *and* skip redraw" would require
  `batch([perform(...), effect(SkipRedraw)])`, but the `SkipRedraw` item is then
  async (the timing problem) and cannot gate the synchronous flag. An attribute
  cannot be expressed as a mutually-exclusive stream variant.
- **Interleaving.** A stream yielding `[Message(a), SkipRedraw, Message(b)]` has
  no well-defined meaning: each `Message` re-enters `update` and sets its own
  redraw; what the intervening `SkipRedraw` suppresses is undefined.

`Quit` legitimately lives in the stream precisely because it *is* a discrete
event; a redraw policy is not, so it lives on the command as a field.

> **Out of scope (mentioned for context).** Unifying the *public* vocabulary so
> that every runtime directive is a `Command` constructor — abolishing the
> public `Action` type and replacing `Command::effect(Action::Quit)` with
> `Command::quit()` — is a natural consistency cleanup that this design's
> reasoning motivates. It is a **separate breaking change** and not required for
> `without_redraw()` (which is additive on its own), so it is deliberately left
> out of this RFC. See §9.

## 5. Design

### 5.1 API surface

`Command<Msg>` gains a private `redraw: bool` field (default `true`) and a
consuming builder:

```rust
impl<Msg: Send + 'static> Command<Msg> {
    /// Declare that the `update` returning this command did not change the
    /// visible view, so the runtime may skip the redraw it would otherwise
    /// perform. The command's side effects (if any) still run.
    ///
    /// This is an optimization hint, not a guarantee: the runtime is free to
    /// redraw anyway (e.g. an initial frame or a concurrently-dirtied frame).
    #[must_use]
    pub fn without_redraw(mut self) -> Self {
        self.redraw = false;
        self
    }
}
```

`without_redraw()` composes with every existing constructor and does not
conflate "suppress redraw" with "do nothing":

```rust
// suppress redraw, no side effect
Command::none().without_redraw()
// suppress redraw for THIS update, but still run a background fetch;
// the message the fetch produces gets its own update + its own redraw decision
Command::perform(fetch_offscreen(), Msg::Loaded).without_redraw()
```

The field is private, so the addition is non-breaking and the default
(`redraw = true`) preserves current behavior. `Action` is **unchanged**.

**Implementation invariant (independence from `stream`).** `redraw` must be
tracked **independently of whether `stream` is `Some`/`None`**. Every place that
short-circuits on `stream.is_none()` — the `filter_map(|cmd| cmd.stream)` in
`batch` (`command.rs:196`) and the `map_or_else(Command::none, …)` in `map`
(`command.rs:298`) — currently rebuilds via `Command::none()` and would silently
reset `redraw` to the default `true`. Both must instead carry the `redraw` bit
through. This single rule is the root fix for §5.4 (`batch`) and §5.5 (`map`).

**Single default-initialization point.** `redraw` is the first field beyond
`stream`, so adding it touches every constructor (`none` / `future` / `message`
/ `effect` / `stream` / `run` / `batch` / `map`). To keep the default in one
place — and make the *next* Axis-A attribute (e.g. `subscription`, §9) a
one-line change instead of another sweep — funnel construction through a single
private helper (e.g. `fn with_stream(stream: Option<BoxStream<…>>) -> Self`
setting `redraw: true`, or a `Default`-based `Self { stream, ..Self::default() }`)
rather than repeating `redraw: true` at each `Self { … }` site. `without_redraw()`
then merely flips the field on an already-constructed value.

### 5.1.1 Naming and API naturalness

The mechanism lives on `Command` because `Command` is `update`'s runtime-
directive channel (§4), and the modifier form is chosen because a redraw policy
is an attribute that composes with side effects. Given that, the name is chosen
to keep the API predictable ("Simple & Predictable"):

- **`without_redraw()`** names the thing being suppressed (the redraw), so
  `Command::none().without_redraw()` reads directly as "no side effect, no
  redraw."
- A shorter `silent()` was rejected: it does not say *what* is silenced and can
  be misread as suppressing the message rather than the redraw.
- A dedicated no-side-effect constructor (e.g. `Command::quiet()`) was rejected:
  it does not compose with `perform`/`message`, so it would force a second
  mechanism for the "effect + no redraw" case, against the minimal-API goal.

Once a redraw attribute exists, a `Command` is no longer *only* a side-effect
stream: `Command::none().without_redraw()` has no stream yet still carries a
runtime directive. That makes the existing "does nothing" framing of
`none`/`is_none` inaccurate, so this RFC **requires the public docs to be
updated** to separate "has no side-effect stream" from "has no runtime
attributes" (§5.6). Leaving them unchanged would let users read `is_none()` as
"droppable," which is no longer true.

### 5.2 Runtime gating

In `process_message_batch`, replace the unconditional
`self.needs_redraw = true` with an OR over the batch: a redraw is needed if
**any** message in the micro-batch returned a command that still redraws.

```rust
// per processed message:
let cmd = self.state.app.update(msg);
if cmd.redraw {                       // read before enqueue_command consumes cmd
    self.needs_redraw = true;         // (bool is Copy, so `if cmd.redraw` is fine)
}
self.state.enqueue_command(cmd);

// ... after the batch loop, unchanged from today (§5.3):
self.subscriptions_dirty = true;      // still unconditional — NOT gated by redraw
```

Only the `needs_redraw = true` line becomes conditional; the existing
unconditional `self.subscriptions_dirty = true` at the end of the batch
(`runtime.rs:302-304`) stays exactly as-is (§5.3).

`needs_redraw` is a persistent flag cleared after render in
`process_frame_tick`; leaving it untouched when all messages opt out means a
batch whose every message returned `without_redraw()` performs no redraw, while
a mixed batch redraws (correct: one visible change in the batch warrants a
frame).

### 5.3 Subscriptions stay unconditional

`subscriptions_dirty` continues to be set to `true` for every processed batch,
per non-negotiable (B). `without_redraw()` never touches it. This keeps the
subscription set always-correct and confines this RFC to the redraw axis.

### 5.4 `Command::batch`

`batch` combines child command streams. Its `redraw` is the **OR of all
children's `redraw`, computed independently of the stream filtering** (per the
§5.1 independence rule). Two rules that must not be conflated:

- **OR over children:** `redraw = any(child.redraw)`. So
  `Command::batch([a.without_redraw(), b])` redraws iff `b` still redraws; an
  opted-out batch requires *every* child to have opted out.
- **Empty fallback applies only to a truly empty input** (zero children):
  `Command::batch([])` keeps the default `true` (matching `Command::none()`).

The distinction matters because a child can carry `redraw = false` while having
`stream == None` (e.g. `Command::none().without_redraw()`). The current
implementation (`command.rs:196-204`) filters children by `stream` and falls
back to `Self::none()` when `streams.is_empty()`, which would return
`redraw = true` for `Command::batch([Command::none().without_redraw()])` —
**violating INV-5b**. The fix computes `redraw` over the *children* before the
stream filter, and only the zero-children case falls back to the default:

```rust
pub fn batch(commands: impl IntoIterator<Item = Self>) -> Self {
    let mut redraw = false;
    let mut any_child = false;
    let mut streams = Vec::new();
    for cmd in commands {
        any_child = true;
        redraw |= cmd.redraw;                 // OR, independent of stream
        if let Some(s) = cmd.stream {
            streams.push(s);
        }
    }
    if !any_child {
        return Self::none();                  // truly empty → default redraw = true
    }
    Command {
        stream: (!streams.is_empty()).then(|| select_all(streams).boxed()),
        redraw,                               // children present → OR result, even if all None
    }
}
```

### 5.5 `Command::map`

`map` rebuilds the `Command`, so it must **carry `redraw` through both
branches** (§5.1). The current implementation
(`command.rs:298-306`) drops it: the `None` branch resets to `Command::none()`
(`redraw = true`) and the `Some` branch rebuilds `Command { stream: … }` without
the field. Both must preserve `self.redraw`, so that `without_redraw().map(f)`
and `map(f).without_redraw()` both end with `redraw = false`:

```rust
pub fn map<T>(self, f: …) -> Command<T> {
    let redraw = self.redraw;
    match self.stream {
        None => Command { stream: None, redraw },      // NOT Command::none()
        Some(stream) => Command {
            stream: Some(stream.map(/* map Action */).boxed()),
            redraw,
        },
    }
}
```

### 5.6 `is_none` / `is_some` and the public docs

`is_none()` / `is_some()` **stay stream-based** and are *not* changed to consider
`redraw`. This is deliberate: the two fields are read by different consumers —
`enqueue_command` inspects `stream` to decide whether to spawn a task
(`command.rs:536`), and `process_message_batch` inspects `redraw` to decide
whether to draw. They are independent by design.

The consequence is a new, valid state: a command with **`stream == None` and
`redraw == false`** (e.g. `Command::none().without_redraw()`, or
`Command::batch([Command::none().without_redraw()])`). For such a command
`is_none() == true` **and yet it carries a runtime directive** (suppress the
redraw that would otherwise happen).

Because of that, this RFC requires updating the doc comments that currently
equate `none`/`is_none` with "does nothing", so a stream-less command is not
read as droppable:

- **`Command` (type):** frame it as "a side-effect stream *plus* runtime
  directives returned by `update`", not "asynchronous side effects" only.
- **`none` / `is_none` / `is_some`:** reword "does nothing" / "is none" to
  "has no side-effect **stream**" (i.e. nothing to spawn). Explicitly note that
  a command with no stream may still carry attributes such as `without_redraw()`,
  so `is_none()` must not be treated as "has no effect on runtime behavior."
- **`batch`:** note that the combined command's `redraw` is the OR of children
  (§5.4) and is independent of whether any child contributed a stream.

## 6. Invariants (contract tests)

- **INV-1 (default preserved).** For every existing constructor
  (`none`, `message`, `perform`, `future`, `effect`, `batch` without opt-out),
  processing the resulting command sets `needs_redraw = true`. No existing
  application changes behavior.
- **INV-2 (opt-out suppresses redraw).** A batch in which every processed
  message returns a `without_redraw()` command leaves `needs_redraw` unchanged
  (no redraw is forced).
- **INV-3 (mixed batch redraws).** A micro-batch containing at least one command
  that still redraws sets `needs_redraw = true`, regardless of order.
- **INV-4 (subscriptions unaffected).** A `without_redraw()` command still marks
  `subscriptions_dirty = true`; redraw suppression never suppresses
  subscription re-evaluation.
- **INV-5a (batch OR).** `Command::batch([...]).redraw == any(child.redraw)`,
  computed over children independently of `stream` presence.
- **INV-5b (batch: all-opted-out vs. empty).** `Command::batch([])` has
  `redraw == true` (empty fallback), but
  `Command::batch([Command::none().without_redraw()])` has `redraw == false` —
  a batch of children that are *all* opted out (even when every child has
  `stream == None`) does not silently revert to `true`. Guards §5.4's root fix.
- **INV-6 (map propagation).** `map` preserves `redraw` on both branches, so
  `Command::perform(…).without_redraw().map(f).redraw == false` and, symmetrically,
  `Command::none().without_redraw().map(f).redraw == false` (the `stream == None`
  branch must not reset to the default). Guards §5.5.
- **INV-7 (recovery).** Suppression is an approximation of "state changed": if
  an application wrongly opts a visible change out of redraw, the next command
  that still redraws recovers the view. (Documented behavior, asserted at the
  batch-gating level via INV-3.)
- **INV-8 (`is_none` / `redraw` independence).** `is_none()` reflects only
  `stream`, so `Command::none().without_redraw()` (and INV-5b's
  `batch([none().without_redraw()])`) satisfies `is_none() == true` *and*
  `redraw == false` simultaneously. This pins the two fields as independent and
  guards against a future regression that folds `redraw` into `is_none()`.
  INV-5b's fixture already exercises this pairing.

## 7. Testing strategy

- **Internal unit tests (deterministic, primary).** `redraw` is a **private
  field**, so these live in `src/command.rs`'s `#[cfg(test)]` module (which can
  read it) — they are white-box tests of the algebra, not public-API tests. They
  assert: each constructor defaults to `true`; `without_redraw()` flips it;
  `batch` ORs over children independently of `stream` (INV-1, INV-5a); the two
  edge cases the algebra gets wrong today — `batch([none().without_redraw()])`
  and `…without_redraw().map(f)` — are `redraw == false` (INV-5b, INV-6); and
  `is_none() == true` coexists with `redraw == false` (INV-8, via the public
  `is_none()` plus the private field). Timing-independent, most stable.
- **Runtime integration (deterministic).** The **user-observable** contract —
  that a `without_redraw()` command actually suppresses the draw while still
  re-evaluating subscriptions — is covered here, without touching the private
  field. Drive `process_message_batch` with crafted `update` implementations
  returning opted-out / redrawing / mixed commands and assert `needs_redraw` and
  `subscriptions_dirty` transitions (INV-2, INV-3, INV-4). Follows the existing
  "exercise the runtime method, assert the flag" pattern used by the idle-wakeup
  tests.
- **Division of labor:** the private-field algebra is pinned by internal unit
  tests; the public, observable redraw behavior is pinned by runtime
  integration. (§6 states the invariants; where each is checked is noted above.)

## 8. Alternatives considered

- **`Action::SkipRedraw` variant (rejected).** A redraw policy is an attribute
  of the update outcome, not a discrete stream event, so it cannot be an
  `Action`: the flag is set synchronously before the stream is drained (wrong
  timing), it must compose with side effects (a mutually-exclusive variant
  cannot), and interleaving it with `Message` items is undefined. Full argument
  in §4.
- **egui-style opt-in (`request_repaint`, default no redraw) (rejected).**
  Maximal savings but unsafe: forgetting to request a repaint after a state
  change leaves the UI stale (a known egui pitfall). The opt-out
  (`without_redraw`) model degrades to "an extra redraw" on mistakes, not "a
  stale UI."
- **Change `update`'s return type to carry a redraw bit (rejected).** A larger
  redesign (a dedicated update-outcome type) than this problem warrants;
  `Command` already *is* the update-outcome/directive channel (§4), so the
  redraw policy fits there without a new type.

## 9. Related and future work

Additive (like this RFC):

- **Subscription re-evaluation skip.** A separate opt-out (e.g.
  `Command::without_subscription_update()`) for messages that change neither the
  view nor the subscription set. Kept out of this RFC by non-negotiable (B).
- **Partial memoization (Elm `Html.Lazy` equivalent).** Skip rebuilding
  sub-widgets whose inputs are unchanged, rather than whole frames. Heavier
  (a rendered-buffer cache) and separable; whole-frame suppression is the first
  step.

Breaking (separate scope, motivated but not required by this RFC):

- **Unify the public directive vocabulary on `Command`.** Abolish the public
  `Action` type and replace `Command::effect(Action::Quit)` with
  `Command::quit()`, keeping the stream's item type private. This RFC's framing
  (§4 — `Command` is the runtime-directive channel; `Quit` and a redraw policy
  are directives of different shapes) is the argument *for* that cleanup, but
  `without_redraw()` is additive on its own and does not depend on it. Because
  the project is pre-1.0 with few users, the migration (a mechanical
  `Command::effect(Action::Quit)` → `Command::quit()` rewrite, with no loss of
  expressiveness — there is no asynchronous `Quit` path today) is cheap and
  better done as its own change.

### The modifier form as a deliberate extension point

`without_redraw()` is the first of a family: a **modifier** is an attribute of
*how the runtime treats a command*, applied as `Command<Msg> -> Command<Msg>`.
The `-> Self` call-site shape is the *only* thing the family shares — the two
axes are represented **oppositely**, and conflating them is the one trap to
avoid.

- **Axis A — output treatment (`without_redraw`, future `without_subscription_update`)
  = passive field.** The flag is read synchronously *before* the stream is
  spawned (§4 timing), so it cannot be baked into the stream; it must be a
  field. It generalizes cleanly: `batch` OR-folds it, `map` carries it
  (§5.4/§5.5), and the private-bool + builder + INV pattern reuses at ~zero cost.
  `without_subscription_update` is the direct confirmation — `subscriptions_dirty`
  is set at the same synchronous point (`runtime.rs:302-304`) and folds
  identically. **This is the axis the "minimal `bool`/enum attribute set" image
  describes; it does not describe Axis B.**
- **Axis B — execution lifecycle (`timeout`, `retry`, `cancellable`) = eager
  stream transformation, NOT a field.** `.timeout(d)` must wrap `self.stream`
  at call time (e.g. `tokio::time::timeout`), because `batch` combines child
  streams via `select_all` (`command.rs:196-204`) and per-child timeouts must
  stay independent — a single outer `Option<Duration>` field cannot represent
  `batch([a.timeout(1s), b.timeout(2s)])`. A bonus of the eager-wrap form: `map`
  and `batch` preserve it for free (it is already in the stream), unlike Axis A
  fields which must be threaded explicitly. **Caution:** do not read "attribute
  set" as "make everything a field" — field-ifying Axis B silently breaks
  `batch`.
- **`cancellable` stresses the model most (P3).** `.cancellable(id)` is only
  half a modifier: assigning the id is `-> Self`, but *cancelling* a running
  task needs runtime machinery keyed by id (today's `command_tasks` `JoinSet`
  has no id map). It may require **opening the internal directive set beyond
  `Message | Quit`** (a third internal action or a side-channel), which is in
  tension with the "closed set" claim (§4). P3's RFC should address that
  head-on.
- **Modifiers are not composition combinators.** `then`/`chain` (iced `Task`)
  sequence effects and are a *separate* category; a future async-composability
  evolution of `Command` is orthogonal to this family. (iced's `Command` →
  `Task` move concerns async composability, not redraw, so it does not bear on
  this RFC.)

The concrete future modifiers above are **left to their own RFCs (P2/P3/P11);**
cf. TCA's `.cancellable(id:cancelInFlight:)` and `.animation()` as prior art for
the two axes. This RFC only records that the modifier form is their agreed home,
notes the A/B representation split so a `timeout` implementer does not field-ify
and break `batch`, and adds `without_redraw()`; it does not design them.

## References

- iced 0.14 reactive rendering / `RedrawRequest::Wait`:
  <https://docs.rs/iced/latest/iced/window/enum.RedrawRequest.html>
- iced `Task` API (async composability, cf. §9 modifier axes):
  <https://docs.iced.rs/iced/struct.Task.html>
- egui reactive mode / `request_repaint`:
  <https://github.com/emilk/egui/discussions/2937>
- Bubble Tea `standard_renderer.go` (flush-skip, same layer as ratatui's
  `BufferDiff`):
  <https://github.com/charmbracelet/bubbletea/blob/v0.27.1/standard_renderer.go>
