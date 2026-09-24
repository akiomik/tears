//! The core protocol: [`Reducer`] and [`Program`].
//!
//! A reducer value is stateless with respect to the runtime — the runtime
//! never mutates it, and all application state lives in
//! [`Reducer::State`]. A reducer may hold child reducers as fields; that is
//! composition structure, not state.
//!
//! [`Reducer::reduce`] is the only state-transition entry point, and
//! [`Reducer::subscriptions`] is a pure function of state exactly as
//! RFC 0012 INV-SE6 states for `Application::subscriptions`: the runtime may
//! evaluate it at any re-evaluation frequency, so it must not carry
//! per-evaluation effects of its own.
//!
//! Views are root-level by design. [`Reducer`] deliberately has no `view`;
//! only [`Program`] does. Composing child views is ordinary function calls
//! inside the root view over the root state — pane and modal layout, draw
//! order, and area allocation are application code (RFC 0014 §2.1).
//!
//! ## Composing Reducers
//!
//! [`Application`](crate::Application) and the composition API describe the
//! same program. An `Application` is run through an adapter that makes the
//! application value the state and `update` the `reduce`, on the same kernel a
//! composed program runs on, so moving between them changes how a program is
//! *written* and not how it is executed.
//!
//! The rest of this page is about when that rewrite pays for itself; the items
//! below are the reference for each piece it names. The worked example is one
//! application written both ways —
//! [`examples/dashboard.rs`](https://docs.rs/crate/tears/latest/source/examples/dashboard.rs)
//! with the root owning the wiring, and
//! [`examples/dashboard_composed.rs`](https://docs.rs/crate/tears/latest/source/examples/dashboard_composed.rs)
//! with boundaries owning it. Both ship in the package.
//!
//! ### Keep the `Application`
//!
//! A single [`Application`](crate::Application) is the right shape while the
//! root can still answer every question about the whole program:
//!
//! - One state struct, or a root struct whose child structs are plain data
//!   the root updates directly.
//! - A `Message` enum the root matches exhaustively, forwarding child
//!   variants by hand where it helps (`dashboard.rs` is this, at the size where
//!   it still reads well).
//! - Commands and subscriptions whose identities the root can keep distinct
//!   by choosing distinct values — one timer per interval, one command id per
//!   concern.
//!
//! Nothing here is improved by adding boundaries.
//! [`TestStore`](crate::testing::TestStore) drives an `Application` directly,
//! which is the shortest test loop the crate offers.
//!
//! ### Move to composed reducers
//!
//! The signal is not size, it is **repeated identities**. Compose when a child
//! feature exists in more than one instance at a time, or comes and goes:
//!
//! - **A collection of children.** Every row wants the same subscription and
//!   the same command id, and only the row it belongs to tells two of them
//!   apart.
//! - **An optionally-present child.** A modal, a detail pane, an editor: its
//!   subscriptions must start when it appears and stop when it is dismissed or
//!   replaced, and the replacement's runs must not inherit the old occupant's.
//! - **A child you want to write once and place twice.** A reducer over its
//!   own state and message type is complete on its own; the boundary supplies
//!   the projection and the message mapping at each placement.
//!
//! Hand-written, the first two are where the bugs are. Removing a row means
//! remembering every command id and subscription that row had; replacing a
//! modal means tearing down the old occupant *before* the new one starts. Both
//! are correct-by-omission problems: the code that forgets them still compiles,
//! still passes single-instance tests, and leaks a run per removal.
//!
//! ### The three boundaries
//!
//! Each combinator wraps a parent reducer and one child, and the result is
//! still a reducer over the root's state and message — which is why they chain.
//!
//! | Combinator | Child state lives in | Segment |
//! | --- | --- | --- |
//! | [`scope`](ReducerExt::scope) | a field of the parent's state | a fixed value you choose |
//! | [`for_each`](ReducerExt::for_each) | a [`Keyed<K, ChildState>`](Keyed) | the row's key |
//! | [`presented`](ReducerExt::presented) | a [`Slot<ChildState>`](Slot) | a fixed value you choose |
//!
//! [`into_program`](ReducerExt::into_program) closes the stack with the two
//! root-level functions composition has no place for: `init` and `view`.
//!
//! A child with no commands and no subscriptions of its own has no identities
//! to qualify, so [`scope`](ReducerExt::scope) buys it code organisation rather
//! than separation — which is a fine reason to reach for it, and a reason not
//! to expect anything more.
//!
//! The outermost boundary sees a message first. What no boundary claims reaches
//! the root reducer, so the root keeps exactly the messages that are its own.
//!
//! ### What a boundary does, so you do not
//!
//! - **It qualifies identities.** Everything identity-bearing in the command
//!   a child returned — spawn keys, explicit cancels, cleanup registrations —
//!   and every subscription the child declares is qualified with that
//!   boundary's segment. Two rows declaring the same timer are two
//!   subscriptions; two rows keyed on the same command id occupy two slots.
//!   Application code writes no `.scoped(...)`, and cannot omit or double-apply
//!   one.
//! - **It tears removed instances down.** `Keyed` and `Slot` record a removal
//!   when one happens — [`Keyed::remove`], [`Slot::dismiss`], and the two
//!   replacing shapes, [`Keyed::insert`] over an occupied key and
//!   [`Slot::present`] over an occupied slot — and the boundary turns each
//!   recorded removal into one teardown of that instance's scope. The removed
//!   instance's subscriptions stop, its in-flight commands are cancelled, and
//!   the cleanup hooks it registered run. Stopping a subscription reaches past
//!   the instance that declared it: while any subscription run is stopping the
//!   runtime starts none, so a replacement's successor — and any other row's
//!   new declarations — wait for that run to quiesce. The combinators state the
//!   timing ([`ReducerExt::for_each`], [`ReducerExt::presented`]).
//! - **It discards what it cannot route.** A message addressed to a key the
//!   collection no longer holds, or to a slot with no occupant, reaches no
//!   reducer and is dropped — with no diagnostic, and with no way for the
//!   sender to learn it went nowhere. That is reachable from ordinary code: a
//!   root that returns [`Command::message`](crate::Command::message) for a row,
//!   and an update that removes the row before the message is delivered, leaves
//!   the row's work simply not done. Where that matters, keep the decision and
//!   the work in one reduce rather than splitting them across a message.
//!
//! Building initial state records nothing: [`Keyed::from_iter`] and an insert
//! into an absent key remove no instance, so growing a collection during `init`
//! is fine. The four shapes that *do* record belong inside a `reduce`, where
//! the boundary drains them in the same update.
//!
//! ### What stays at the root
//!
//! **`view` does**, for the reason this page opens with: the frame is one
//! decision, so composing child views is ordinary function calls over the root
//! state.
//!
//! **`init`'s command does.** It is the root's command and crosses no boundary,
//! so nothing it starts is scoped to a child. Work that belongs to a child —
//! the first fetch, a cleanup hook that must anchor at the child's scope —
//! starts as a message routed *through* the boundary. In the worked example
//! that is `TaskMessage::Watch`: the root inserts the row and returns
//! `Command::message(Message::Task(id, TaskMessage::Watch))`, and the row's own
//! reduce registers its hook.
//!
//! Such a setup message has to be handled *idempotently* by the child, which is
//! the child's side of the same rule. Nothing guarantees it arrives once — a
//! second key press decided against a state the first message has not been
//! applied to yet produces a second — and a teardown fires *every* registration
//! its scope holds, so a child that arms on each one reports two teardowns for
//! one removal. A flag on the child's state is enough; the successor instance a
//! replacement creates gets a fresh one.
//!
//! The same rule explains
//! [`Command::on_teardown`](crate::Command::on_teardown)'s placement. A
//! registration anchors at the scope of the boundary it is built at, and one
//! built at the root anchors where no teardown reaches it.
//!
//! **Work that spans two children does.** A child is handed its own projected
//! state and nothing else, so it can reach neither the collection it sits
//! beside nor the slot it sits in. Anything that touches two of them is a root
//! message. In the worked example that is `Message::SaveNotes`: the details
//! pane's edited notes are written onto the task the pane was opened for by the
//! root, which then asks the row to sync through the boundary rather than
//! starting that command itself.
//!
//! ### Testing a composition
//!
//! [`TestStore`](crate::testing::TestStore) takes an `Application`, so a
//! composed program is driven with [`TestDriver`](crate::testing::TestDriver)
//! instead. That is a fact about the store's type rather than about the
//! driver's reach: the driver runs the production kernel, and an `Application`
//! reaches for it too — through [`AppProgram`], the adapter the
//! [`Runtime`](crate::Runtime) facade applies — when a test needs what the
//! store only declares. The driver constructs from the same inputs the
//! production entry point takes, boots the program, and steps whole passes,
//! with the sends a producer makes released one grant at a time. The rows in
//! `dashboard_composed.rs` that build a `TestDriver` drive that example's own
//! stack that way — the same `Reducer` value `main` runs, closed with the same
//! `init` and `view`, and started from a `Setup` carrying a scripted input
//! instead of the binary's seed. They are the shape to copy. The rest of that
//! file's rows call the root reducer and the key decoder directly, which is a
//! cheaper loop and a different claim: it never crosses a boundary, so it would
//! pass with the composition taken out.
//!
//! Two observations are worth designing tests around, because they are what
//! composition changes:
//!
//! - The run identities a step started, which is where "these two rows did
//!   not collide" is visible.
//! - A cleanup hook's own side effect.
//!   [`Command::on_teardown`](crate::Command::on_teardown) takes a future whose
//!   `Output` is `()`, so a finalizer sends no message; give it a sink the test
//!   can read, and let the view render the same sink if the application wants
//!   to show it.

// The three submodules are file organization, not a hierarchy a user needs
// to navigate: everything public in them is re-exported here, so each item
// has exactly one public path (`docs/api-guidelines.md`, "Single Canonical
// Path" and "Module Visibility").
pub(crate) mod adapter;
pub(crate) mod collection;
pub(crate) mod combinator;
// `Exit` is `ProgramRuntime::run`'s success type, so it shares its owner's
// home at the crate root rather than sitting on this module's path — the
// companion rule in `docs/api-guidelines.md`. Its module is `pub(crate)` so
// the root re-export is the only public way to it, which is the same
// private-inner-module pattern `command::core` uses for `Command`.
pub(crate) mod exit;

pub use adapter::AppProgram;
pub use collection::{Keyed, ScopeValue, Slot};
pub use combinator::{ForEach, IntoProgram, Presented, ReducerExt, Scoped};
pub(crate) use exit::Exit;

use ratatui::Frame;

use crate::command::Command;
use crate::subscription::Subscription;

/// A state transition and the subscriptions that state declares.
pub trait Reducer {
    /// The state this reducer owns.
    type State;

    /// The messages it consumes. The `Send + 'static` boundary is the one
    /// `Application` already has (RFC 0010 §7.1's freeze); no `Clone` or
    /// `PartialEq` bound is added.
    type Message: Send + 'static;

    /// Applies one message to the state, returning the command it wants run.
    fn reduce(&self, state: &mut Self::State, message: Self::Message) -> Command<Self::Message>;

    /// The subscriptions this state declares. Pure in the state: equal
    /// states declare equal sets (RFC 0012 INV-SE6).
    fn subscriptions(&self, _state: &Self::State) -> Vec<Subscription<Self::Message>> {
        Vec::new()
    }
}

/// A reducer that can be run: it can produce its initial state and render.
pub trait Program: Reducer {
    /// The construction-time input.
    type Flags;

    /// Produces the initial state and the command dispatched at bootstrap.
    ///
    /// A quit returned here short-circuits bootstrap synchronously — the
    /// initial reconcile does not run (RFC 0014 §6.2).
    fn init(&self, flags: Self::Flags) -> (Self::State, Command<Self::Message>);

    /// Renders the current state.
    fn view(&self, state: &Self::State, frame: &mut Frame<'_>);
}
