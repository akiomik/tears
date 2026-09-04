# Composing Reducers

`Application` and the composition API describe the same program. An
`Application` is run through an adapter that makes the application value the
state and `update` the `reduce`, on the same kernel a composed program runs on,
so moving between them changes how a program is *written* and not how it is
executed.

This guide is about when that rewrite pays for itself. The worked example is
one application written both ways: [`examples/dashboard.rs`](../examples/dashboard.rs)
with the root owning the wiring, and
[`examples/dashboard_composed.rs`](../examples/dashboard_composed.rs) with
boundaries owning it. The reference for each item is the
[`tears::reducer`](https://docs.rs/tears/latest/tears/reducer/) module.

## Keep the `Application`

A single `Application` is the right shape while the root can still answer
every question about the whole program:

- One state struct, or a root struct whose child structs are plain data the
  root updates directly.
- A `Message` enum the root matches exhaustively, forwarding child variants by
  hand where it helps (`dashboard.rs` is this, at the size where it still reads
  well).
- Commands and subscriptions whose identities the root can keep distinct by
  choosing distinct values — one timer per interval, one command id per
  concern.

Nothing here is improved by adding boundaries. `TestStore` drives an
`Application` directly, which is the shortest test loop the crate offers.

## Move to composed reducers

The signal is not size, it is **repeated identities**. Compose when a child
feature exists in more than one instance at a time, or comes and goes:

- **A collection of children.** Every row wants the same subscription and the
  same command id, and only the row it belongs to tells two of them apart.
- **An optionally-present child.** A modal, a detail pane, an editor: its
  subscriptions must start when it appears and stop when it is dismissed or
  replaced, and the replacement's runs must not inherit the old occupant's.
- **A child you want to write once and place twice.** A reducer over its own
  state and message type is complete on its own; the boundary supplies the
  projection and the message mapping at each placement.

Hand-written, the first two are where the bugs are. Removing a row means
remembering every command id and subscription that row had; replacing a modal
means tearing down the old occupant *before* the new one starts. Both are
correct-by-omission problems: the code that forgets them still compiles, still
passes single-instance tests, and leaks a run per removal.

## The three boundaries

Each combinator wraps a parent reducer and one child, and the result is still a
reducer over the root's state and message — which is why they chain.

| Combinator | Child state lives in | Segment |
| --- | --- | --- |
| `scope` | a field of the parent's state | a fixed value you choose |
| `for_each` | a `Keyed<K, ChildState>` | the row's key |
| `presented` | a `Slot<ChildState>` | a fixed value you choose |

`into_program` closes the stack with the two root-level functions composition
has no place for: `init` and `view`.

A child with no commands and no subscriptions of its own has no identities to
qualify, so `scope` buys it code organisation rather than separation — which is
a fine reason to reach for it, and a reason not to expect anything more.

The outermost boundary sees a message first. What no boundary claims reaches
the root reducer, so the root keeps exactly the messages that are its own.

## What a boundary does, so you do not

- **It qualifies identities.** Everything identity-bearing in the command a
  child returned — spawn keys, explicit cancels, cleanup registrations — and
  every subscription the child declares is qualified with that boundary's
  segment. Two rows declaring the same timer are two subscriptions; two rows
  keyed on the same command id occupy two slots. Application code writes no
  `.scoped(...)`, and cannot omit or double-apply one.
- **It tears removed instances down.** `Keyed` and `Slot` record a removal when
  one happens — `Keyed::remove`, `Slot::dismiss`, and the two replacing shapes,
  `Keyed::insert` over an occupied key and `Slot::present` over an occupied slot
  — and the boundary turns each recorded removal into one teardown of that
  instance's scope. The removed instance's subscriptions stop, its in-flight
  commands are cancelled, and the cleanup hooks it registered run.

Building initial state records nothing: `Keyed::from_iter` and an insert into an
absent key remove no instance, so growing a collection during `init` is fine.
The four shapes that *do* record belong inside a `reduce`, where the boundary
drains them in the same update.

## Three things stay at the root

**`view` does.** `Reducer` has no `view` and only `Program` does: pane layout,
draw order and area allocation are decisions about the whole frame, so
composing child views is ordinary function calls over the root state.

**`init`'s command does.** It is the root's command and crosses no boundary, so
nothing it starts is scoped to a child. Work that belongs to a child — the
first fetch, a cleanup hook that must anchor at the child's scope — starts as a
message routed *through* the boundary. In the worked example that is
`TaskMessage::Watch`: the root inserts the row and returns
`Command::message(Message::Task(id, TaskMessage::Watch))`, and the row's own
reduce registers its hook.

**Handle such a setup message idempotently.** Nothing guarantees it arrives
once — a second key press decided against a state the first message has not
been applied to yet produces a second — and a teardown fires *every*
registration its scope holds, so a child that arms on each one reports two
teardowns for one removal. A flag on the child's state is enough; the successor
instance a replacement creates gets a fresh one.

The same rule explains `Command::on_teardown`'s placement. A registration
anchors at the scope of the boundary it is built at, and one built at the root
anchors where no teardown reaches it.

**Work that spans two children does.** A child is handed its own projected
state and nothing else, so it can reach neither the collection it sits beside
nor the slot it sits in. Anything that touches two of them is a root message.
In the worked example that is `Message::SaveNotes`: the details pane's edited
notes are written onto the task the pane was opened for by the root, which then
asks the row to sync through the boundary rather than starting that command
itself.

## Testing a composition

`TestStore` takes an `Application`, so a composed program is driven with
`tears::testing::TestDriver` instead: it constructs from the same inputs the
production entry point takes, boots the program, and steps whole passes, with
the sends a producer makes released one grant at a time. The two tests in
`dashboard_composed.rs` drive that example's own stack — the same `Reducer`
value `main` runs, closed with the same `init` and `view`, and started from a
`Setup` carrying a scripted input instead of the binary's seed — and are the
shape to copy.

Two observations are worth designing tests around, because they are what
composition changes:

- The run identities a step started, which is where "these two rows did not
  collide" is visible.
- A cleanup hook's own side effect. `Command::on_teardown` takes a future whose
  `Output` is `()`, so a finalizer sends no message; give it a sink the test can
  read, and let the view render the same sink if the application wants to show
  it.
