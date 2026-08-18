//! The composition combinators: [`ReducerExt`] and the four types it builds.
//!
//! A combinator is a [`Reducer`] wrapping a parent reducer and one child
//! reducer, together with the projections and message mappings that connect
//! them. Because every combinator implements
//! `Reducer<State = Self::State, Message = Self::Message>` for its
//! *parent's* associated types, stacks nest: each call adds one boundary and
//! the result is still a reducer over the root's state and message.
//!
//! # What a boundary does (RFC 0014 §2.5)
//!
//! - **Qualifies every identity-bearing carrier** of the child's returned
//!   command — spawn keys, explicit cancel ids, teardown prefixes, cleanup
//!   registrations — and the child's subscription declarations, with the
//!   boundary's segment. That is one [`Command::scoped`] call and one
//!   [`Subscription::scoped`] call per boundary; user code writes no
//!   `.scoped(...)` and can neither omit nor double-apply one (INV-RC2).
//! - **Drains its removal journal** after the reduce it ran, merging one
//!   [`Command::teardown`] per recorded removal into the returned command
//!   (INV-RC3).
//! - **Routes typed messages**: `extract` either claims a message for the
//!   child or hands it back to the parent, and a message addressed to a key
//!   or slot with no instance is routed to nothing and discarded.
//!
//! # Read/write projection pairs
//!
//! Each boundary takes its projection as a pair, because the two `Reducer`
//! methods borrow the parent differently: `reduce` needs the mutable
//! projection and `subscriptions` the shared one. A combinator holding only
//! the mutable accessor could not aggregate its child's declarations from
//! the `&Self::State` `subscriptions` gives it — it would have to fabricate
//! an aliasing mutable borrow or drop the child's subscriptions, and
//! INV-RC2's aggregation would be unimplementable. Both are projections of
//! state the caller already holds, so RFC 0012 INV-SE6's purity is
//! untouched. Two `fn` items per boundary is the whole cost; no lens trait
//! is introduced and none is needed.
//!
//! # One teardown surface (RFC 0013 R8)
//!
//! Every teardown this module produces is a call to the public
//! [`Command::teardown`] constructor, in exactly one place —
//! [`teardowns_for`], which the three journal drains share. There is no
//! internal twin that builds a teardown entry from a raw prefix, and no
//! other route below the public surface originates one. What the boundaries
//! do to an *already-originated* teardown — `scoped`'s prefix
//! qualification, `batch`'s aggregation, the lowering to runtime parts — is
//! transformation and stays free.
//!
//! [`Subscription::scoped`]: crate::subscription::Subscription::scoped

// The projection and mapping parameters are written exactly as RFC 0014
// §2.5 states them. Factoring `fn(Self::Message) -> Result<C::Message,
// Self::Message>` behind an alias would hide the two things the contract is
// about — that a boundary takes a read/write projection *pair*, and that
// `extract` either claims a message or hands it back — behind a name, and
// the signatures are the surface the invariant quantifies over.
#![expect(
    clippy::type_complexity,
    reason = "RFC 0014 §2.5 fixes these signatures verbatim; an alias would hide the contract"
)]

use std::iter;

use ratatui::Frame;

use crate::command::Command;
use crate::subscription::Subscription;

use super::collection::{Keyed, ScopeValue, Slot};
use super::{Program, Reducer};

/// The composition combinators, on every [`Reducer`].
pub trait ReducerExt: Reducer + Sized {
    /// Composes one child under a fixed segment.
    ///
    /// `state`/`state_mut` project the child's state out of the parent's,
    /// `extract` claims the messages that belong to the child, and `embed`
    /// lifts the child's messages back into the parent's.
    fn scope<Seg, C>(
        self,
        child: C,
        seg: Seg,
        state: fn(&Self::State) -> &C::State,
        state_mut: fn(&mut Self::State) -> &mut C::State,
        extract: fn(Self::Message) -> Result<C::Message, Self::Message>,
        embed: fn(C::Message) -> Self::Message,
    ) -> Scoped<Self, C, Seg>
    where
        C: Reducer,
        Seg: ScopeValue,
    {
        Scoped {
            parent: self,
            child,
            seg,
            state,
            state_mut,
            extract,
            embed,
        }
    }

    /// Composes one child per row of a [`Keyed`] collection, each under its
    /// own key as segment.
    ///
    /// `extract` names the row a message is addressed to; a message for a
    /// key the collection does not hold is discarded.
    fn for_each<C, K>(
        self,
        child: C,
        rows: fn(&Self::State) -> &Keyed<K, C::State>,
        rows_mut: fn(&mut Self::State) -> &mut Keyed<K, C::State>,
        extract: fn(Self::Message) -> Result<(K, C::Message), Self::Message>,
        embed: fn(K, C::Message) -> Self::Message,
    ) -> ForEach<Self, C, K>
    where
        C: Reducer,
        K: ScopeValue,
    {
        ForEach {
            parent: self,
            child,
            rows,
            rows_mut,
            extract,
            embed,
        }
    }

    /// Composes one optionally-present child held in a [`Slot`], under a
    /// fixed segment.
    ///
    /// A message the slot's occupant would have claimed is discarded while
    /// the slot is empty.
    fn presented<C, Seg>(
        self,
        child: C,
        seg: Seg,
        slot: fn(&Self::State) -> &Slot<C::State>,
        slot_mut: fn(&mut Self::State) -> &mut Slot<C::State>,
        extract: fn(Self::Message) -> Result<C::Message, Self::Message>,
        embed: fn(C::Message) -> Self::Message,
    ) -> Presented<Self, C, Seg>
    where
        C: Reducer,
        Seg: ScopeValue,
    {
        Presented {
            parent: self,
            child,
            seg,
            slot,
            slot_mut,
            extract,
            embed,
        }
    }

    /// Closes a combinator stack into a runnable [`Program`].
    ///
    /// The root `init` and the root `view` are the two things a stack has no
    /// place for: composition is over state transitions and declarations,
    /// and rendering is root-level by design (RFC 0014 §2.1).
    fn into_program<Flags>(
        self,
        init: fn(Flags) -> (Self::State, Command<Self::Message>),
        view: fn(&Self::State, &mut Frame<'_>),
    ) -> IntoProgram<Self, Flags> {
        IntoProgram {
            reducer: self,
            init,
            view,
        }
    }
}

impl<R: Reducer + Sized> ReducerExt for R {}

/// One teardown per recorded removal, as the merge a journal drain returns.
///
/// **The one origination site in this module** (RFC 0013 R8): every teardown
/// a combinator produces is built here, by a call to the public
/// [`Command::teardown`] constructor, from a segment value the application
/// itself supplied as a key or as the boundary's `seg`. No raw prefix path
/// is constructed anywhere in this module, and no other function here builds
/// a teardown.
///
/// One entry, one teardown — so a remove-and-reinsert-and-remove within a
/// single update yields two, which is what "one teardown for the removed
/// instance" means when two instances were removed. Repetition is harmless
/// by INV-ST5's idempotence, and the alternative — collapsing them — would
/// make the merge depend on a comparison of prefixes the boundary has no
/// reason to perform.
fn teardowns_for<Msg, Seg>(removed: Vec<Seg>) -> impl Iterator<Item = Command<Msg>>
where
    Msg: Send + 'static,
    Seg: ScopeValue,
{
    removed.into_iter().map(Command::teardown)
}

/// Merges a boundary's teardowns into the command its reduce produced.
///
/// The teardowns come first, which reads as "the removals apply, then this
/// update's own work" — though the phase order does not depend on it: the
/// lowering applies every cancel-phase entry of a command before every spawn
/// of it, whatever order the batch put them in (RFC 0014 §3.4).
///
/// With nothing removed the command is returned untouched rather than
/// wrapped in a one-child batch, so a boundary that removed nothing is not
/// observable in the command it returns.
fn merge<Msg, Seg>(command: Command<Msg>, removed: Vec<Seg>) -> Command<Msg>
where
    Msg: Send + 'static,
    Seg: ScopeValue,
{
    if removed.is_empty() {
        return command;
    }
    Command::batch(teardowns_for(removed).chain(iter::once(command)))
}

/// One child under a fixed segment ([`ReducerExt::scope`]).
pub struct Scoped<P: Reducer, C: Reducer, Seg> {
    parent: P,
    child: C,
    seg: Seg,
    state: fn(&P::State) -> &C::State,
    state_mut: fn(&mut P::State) -> &mut C::State,
    extract: fn(P::Message) -> Result<C::Message, P::Message>,
    embed: fn(C::Message) -> P::Message,
}

impl<P, C, Seg> Reducer for Scoped<P, C, Seg>
where
    P: Reducer,
    C: Reducer,
    Seg: ScopeValue,
{
    type State = P::State;
    type Message = P::Message;

    /// Routes the message, and qualifies whatever the child returned.
    ///
    /// A message the child does not claim goes to the parent, whose command
    /// is *not* qualified: it is the parent's own command at the parent's
    /// own level, and this boundary is not one it crossed.
    fn reduce(&self, state: &mut P::State, message: P::Message) -> Command<P::Message> {
        match (self.extract)(message) {
            Ok(claimed) => {
                let embed = self.embed;
                self.child
                    .reduce((self.state_mut)(state), claimed)
                    .map(embed)
                    .scoped(self.seg.clone())
            }
            Err(unclaimed) => self.parent.reduce(state, unclaimed),
        }
    }

    /// The parent's declarations, then the child's under this boundary.
    fn subscriptions(&self, state: &P::State) -> Vec<Subscription<P::Message>> {
        let mut declared = self.parent.subscriptions(state);
        let embed = self.embed;
        declared.extend(
            self.child
                .subscriptions((self.state)(state))
                .into_iter()
                .map(|declaration| declaration.map(embed).scoped(self.seg.clone())),
        );
        declared
    }
}

/// One child per row of a [`Keyed`] collection ([`ReducerExt::for_each`]).
pub struct ForEach<P: Reducer, C: Reducer, K: ScopeValue> {
    parent: P,
    child: C,
    rows: fn(&P::State) -> &Keyed<K, C::State>,
    rows_mut: fn(&mut P::State) -> &mut Keyed<K, C::State>,
    extract: fn(P::Message) -> Result<(K, C::Message), P::Message>,
    embed: fn(K, C::Message) -> P::Message,
}

impl<P, C, K> Reducer for ForEach<P, C, K>
where
    P: Reducer,
    C: Reducer,
    K: ScopeValue,
{
    type State = P::State;
    type Message = P::Message;

    /// Routes the message to its row, then drains the journal.
    ///
    /// The drain runs on **both** branches, because either can remove a row:
    /// the parent's `update` is where an application closes a pane, and a
    /// row's own update could in principle reach the collection through the
    /// state it was handed. Draining once per reduce is what makes the
    /// journal complete rather than dependent on which branch ran.
    ///
    /// A message addressed to a key the collection does not hold reaches no
    /// reducer and is discarded (RFC 0014 §2.5's routing boundary). The
    /// journal is still drained: a removal recorded by an earlier operation
    /// is owed its teardown whatever this message turned out to address.
    fn reduce(&self, state: &mut P::State, message: P::Message) -> Command<P::Message> {
        let command = match (self.extract)(message) {
            Ok((key, claimed)) => {
                let embed = self.embed;
                let addressed = key.clone();
                (self.rows_mut)(state)
                    .get_mut(&key)
                    .map_or_else(Command::none, |row| {
                        self.child
                            .reduce(row, claimed)
                            .map(move |message| embed(addressed.clone(), message))
                            .scoped(key)
                    })
            }
            Err(unclaimed) => self.parent.reduce(state, unclaimed),
        };
        merge(command, (self.rows_mut)(state).drain_removals())
    }

    /// The parent's declarations, then each row's under its own key.
    fn subscriptions(&self, state: &P::State) -> Vec<Subscription<P::Message>> {
        let mut declared = self.parent.subscriptions(state);
        let embed = self.embed;
        for (key, row) in (self.rows)(state).iter() {
            declared.extend(
                self.child
                    .subscriptions(row)
                    .into_iter()
                    .map(|declaration| {
                        let addressed = key.clone();
                        declaration
                            .map(move |message| embed(addressed.clone(), message))
                            .scoped(key.clone())
                    }),
            );
        }
        declared
    }
}

/// One optionally-present child ([`ReducerExt::presented`]).
pub struct Presented<P: Reducer, C: Reducer, Seg> {
    parent: P,
    child: C,
    seg: Seg,
    slot: fn(&P::State) -> &Slot<C::State>,
    slot_mut: fn(&mut P::State) -> &mut Slot<C::State>,
    extract: fn(P::Message) -> Result<C::Message, P::Message>,
    embed: fn(C::Message) -> P::Message,
}

impl<P, C, Seg> Reducer for Presented<P, C, Seg>
where
    P: Reducer,
    C: Reducer,
    Seg: ScopeValue,
{
    type State = P::State;
    type Message = P::Message;

    /// Routes the message to the occupant, then drains the journal.
    ///
    /// A claimed message reaching an empty slot is discarded, for the reason
    /// a message for an absent key is.
    fn reduce(&self, state: &mut P::State, message: P::Message) -> Command<P::Message> {
        let command = match (self.extract)(message) {
            Ok(claimed) => {
                let embed = self.embed;
                (self.slot_mut)(state)
                    .get_mut()
                    .map_or_else(Command::none, |occupant| {
                        self.child
                            .reduce(occupant, claimed)
                            .map(embed)
                            .scoped(self.seg.clone())
                    })
            }
            Err(unclaimed) => self.parent.reduce(state, unclaimed),
        };
        let dismissals = (self.slot_mut)(state).drain_dismissals();
        merge(command, vec![self.seg.clone(); dismissals])
    }

    /// The parent's declarations, then the occupant's if there is one.
    fn subscriptions(&self, state: &P::State) -> Vec<Subscription<P::Message>> {
        let mut declared = self.parent.subscriptions(state);
        let embed = self.embed;
        if let Some(occupant) = (self.slot)(state).get() {
            declared.extend(
                self.child
                    .subscriptions(occupant)
                    .into_iter()
                    .map(|declaration| declaration.map(embed).scoped(self.seg.clone())),
            );
        }
        declared
    }
}

/// A closed combinator stack ([`ReducerExt::into_program`]).
///
/// Its `Reducer` half is the stack's, delegated verbatim: the same `reduce`
/// and the same `subscriptions` a composed stack has when it is not closed,
/// so closing a stack adds no execution path of its own. What it adds is the
/// two root-level functions a [`Program`] needs.
pub struct IntoProgram<R: Reducer, Flags> {
    reducer: R,
    init: fn(Flags) -> (R::State, Command<R::Message>),
    view: fn(&R::State, &mut Frame<'_>),
}

impl<R: Reducer, Flags> Reducer for IntoProgram<R, Flags> {
    type State = R::State;
    type Message = R::Message;

    fn reduce(&self, state: &mut R::State, message: R::Message) -> Command<R::Message> {
        self.reducer.reduce(state, message)
    }

    fn subscriptions(&self, state: &R::State) -> Vec<Subscription<R::Message>> {
        self.reducer.subscriptions(state)
    }
}

impl<R: Reducer, Flags> Program for IntoProgram<R, Flags> {
    type Flags = Flags;

    fn init(&self, flags: Flags) -> (R::State, Command<R::Message>) {
        (self.init)(flags)
    }

    fn view(&self, state: &R::State, frame: &mut Frame<'_>) {
        (self.view)(state, frame);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashSet;

    use futures::stream;

    use crate::command::{CommandId, KernelParts};
    use crate::structural_key::ScopePath;
    use crate::subscription::mock::MockSource;

    // A child that answers each of its messages with a command carrying one
    // of every identity-bearing carrier, so a boundary's qualification can
    // be read off all four at once.
    #[derive(Clone, Copy)]
    struct Child;

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum ChildMessage {
        /// Return a keyed effect, an explicit cancel, a teardown, and a
        /// registration, all under the child's own local ids.
        Carriers,
        /// Return nothing.
        Quiet,
    }

    struct ChildState {
        /// Whether this child declares its source.
        subscribed: bool,
        /// What the child recorded being asked to do.
        seen: Vec<ChildMessage>,
    }

    impl ChildState {
        const fn new(subscribed: bool) -> Self {
            Self {
                subscribed,
                seen: Vec::new(),
            }
        }
    }

    impl Reducer for Child {
        type State = ChildState;
        type Message = ChildMessage;

        fn reduce(&self, state: &mut ChildState, message: ChildMessage) -> Command<ChildMessage> {
            state.seen.push(message.clone());
            match message {
                ChildMessage::Quiet => Command::none(),
                ChildMessage::Carriers => Command::batch([
                    Command::stream(stream::pending()).cancellable(CommandId::new("work")),
                    Command::stream(stream::pending()),
                    Command::cancel(CommandId::new("other")),
                    Command::teardown("inner"),
                    Command::on_teardown(async {}),
                ]),
            }
        }

        fn subscriptions(&self, state: &ChildState) -> Vec<Subscription<ChildMessage>> {
            if state.subscribed {
                vec![Subscription::new(MockSource::<ChildMessage>::new())]
            } else {
                Vec::new()
            }
        }
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum Message {
        /// Addressed to the `scope` boundary's child.
        Left(ChildMessage),
        /// Addressed to the second `scope` boundary's child.
        Right(ChildMessage),
        /// Addressed to one row of the collection.
        Row(&'static str, ChildMessage),
        /// Addressed to the slot's occupant.
        Modal(ChildMessage),
        /// Handled by the root: removes one row.
        Close(&'static str),
        /// Handled by the root: replaces one row's instance.
        Replace(&'static str),
        /// Handled by the root: removes and re-inserts one row.
        Recreate(&'static str),
        /// Handled by the root: dismisses the slot.
        Dismiss,
        /// Handled by the root: presents a fresh instance in the slot.
        Present,
        /// Handled by the root, returning its own keyed command.
        RootWork,
        /// Handled by the root, returning nothing.
        Idle,
    }

    struct RootState {
        left: ChildState,
        right: ChildState,
        rows: Keyed<&'static str, ChildState>,
        modal: Slot<ChildState>,
    }

    impl RootState {
        fn new() -> Self {
            Self {
                left: ChildState::new(true),
                right: ChildState::new(true),
                rows: Keyed::new(),
                modal: Slot::empty(),
            }
        }
    }

    struct Root;

    impl Reducer for Root {
        type State = RootState;
        type Message = Message;

        fn reduce(&self, state: &mut RootState, message: Message) -> Command<Message> {
            match message {
                Message::Close(key) => {
                    state.rows.remove(&key);
                    Command::none()
                }
                Message::Replace(key) => {
                    state.rows.insert(key, ChildState::new(true));
                    Command::none()
                }
                Message::Recreate(key) => {
                    state.rows.remove(&key);
                    state.rows.insert(key, ChildState::new(true));
                    Command::none()
                }
                Message::Dismiss => {
                    state.modal.dismiss();
                    Command::none()
                }
                Message::Present => {
                    state.modal.present(ChildState::new(true));
                    Command::none()
                }
                Message::RootWork => {
                    Command::stream(stream::pending()).cancellable(CommandId::new("root"))
                }
                _ => Command::none(),
            }
        }

        fn subscriptions(&self, _state: &RootState) -> Vec<Subscription<Message>> {
            vec![Subscription::new(MockSource::<Message>::new())]
        }
    }

    fn left_extract(message: Message) -> Result<ChildMessage, Message> {
        match message {
            Message::Left(child) => Ok(child),
            other => Err(other),
        }
    }

    fn right_extract(message: Message) -> Result<ChildMessage, Message> {
        match message {
            Message::Right(child) => Ok(child),
            other => Err(other),
        }
    }

    fn row_extract(message: Message) -> Result<(&'static str, ChildMessage), Message> {
        match message {
            Message::Row(key, child) => Ok((key, child)),
            other => Err(other),
        }
    }

    fn modal_extract(message: Message) -> Result<ChildMessage, Message> {
        match message {
            Message::Modal(child) => Ok(child),
            other => Err(other),
        }
    }

    /// The stack every row below reduces through: two sibling `scope`
    /// boundaries, a `for_each` over the collection, and a `presented` slot.
    fn stack() -> impl Reducer<State = RootState, Message = Message> {
        Root.scope(
            Child,
            "left",
            |state: &RootState| &state.left,
            |state: &mut RootState| &mut state.left,
            left_extract,
            Message::Left,
        )
        .scope(
            Child,
            "right",
            |state: &RootState| &state.right,
            |state: &mut RootState| &mut state.right,
            right_extract,
            Message::Right,
        )
        .for_each(
            Child,
            |state: &RootState| &state.rows,
            |state: &mut RootState| &mut state.rows,
            row_extract,
            Message::Row,
        )
        .presented(
            Child,
            "modal",
            |state: &RootState| &state.modal,
            |state: &mut RootState| &mut state.modal,
            modal_extract,
            Message::Modal,
        )
    }

    fn path(segments: &[&'static str]) -> ScopePath {
        // Root-first storage: the last `prefixed` call names the outermost
        // segment, so the slice reads root-first when applied in reverse.
        segments
            .iter()
            .rev()
            .fold(ScopePath::empty(), |acc, segment| acc.prefixed(*segment))
    }

    /// One reduce, lowered the way the kernel reads it.
    fn lowered(
        reducer: &impl Reducer<State = RootState, Message = Message>,
        state: &mut RootState,
        message: Message,
    ) -> KernelParts<Message> {
        reducer
            .reduce(state, message)
            .into_runtime_parts()
            .into_kernel_parts()
    }

    /// The id `CommandId::new(local)` becomes under `boundary`, built the
    /// way an application would build it — `Command::scoped` is the only
    /// route to a qualified id from outside the command module, which is
    /// what makes this an independent expectation rather than a restatement
    /// of the combinator's own call.
    fn qualified(local: &'static str, boundary: &'static str) -> CommandId {
        Command::<Message>::cancel(CommandId::new(local))
            .scoped(boundary)
            .into_runtime_parts()
            .into_kernel_parts()
            .cancels
            .into_iter()
            .next()
            .expect("the command carries the one cancel id it was built with")
    }

    fn spawn_scopes(parts: &KernelParts<Message>) -> Vec<ScopePath> {
        parts
            .spawns
            .iter()
            .map(|spawn| spawn.scope.clone())
            .collect()
    }

    fn cleanup_scopes(parts: &KernelParts<Message>) -> Vec<ScopePath> {
        parts
            .cleanups
            .iter()
            .map(|registration| registration.scope.clone())
            .collect()
    }

    // INV-RC2: a boundary qualifies **every** identity-bearing carrier of
    // the child's returned command — the spawn key, the anonymous carrier's
    // placement scope, the explicit cancel id, the teardown prefix, and the
    // cleanup registration — with its segment, and nothing else.
    #[test]
    fn a_boundary_qualifies_every_carrier_of_its_child_s_command() {
        let stack = stack();
        let mut state = RootState::new();

        let parts = lowered(&stack, &mut state, Message::Left(ChildMessage::Carriers));

        assert_eq!(
            spawn_scopes(&parts),
            vec![path(&["left"]), path(&["left"])],
            "the keyed carrier and the anonymous one are both placed under the boundary"
        );
        assert_eq!(
            parts.spawns[0]
                .key
                .as_ref()
                .expect("the first carrier is keyed")
                .id,
            qualified("work", "left"),
            "the spawn key is qualified"
        );
        assert_eq!(
            parts.cancels,
            vec![qualified("other", "left")],
            "and so is the explicit cancel id"
        );
        assert_eq!(
            parts.teardowns,
            vec![path(&["left", "inner"])],
            "and the teardown prefix, with the boundary's segment at the root"
        );
        assert_eq!(
            cleanup_scopes(&parts),
            vec![path(&["left"])],
            "and the cleanup registration's anchor"
        );
    }

    // INV-RC2's sibling clause: equal local ids under sibling scopes never
    // alias. The two boundaries hand the *same* child the same message, and
    // every carrier comes back distinct.
    #[test]
    fn equal_local_ids_under_sibling_boundaries_do_not_alias() {
        let stack = stack();
        let mut state = RootState::new();

        let left = lowered(&stack, &mut state, Message::Left(ChildMessage::Carriers));
        let right = lowered(&stack, &mut state, Message::Right(ChildMessage::Carriers));

        assert_ne!(
            left.spawns[0].key.as_ref().expect("keyed").id,
            right.spawns[0].key.as_ref().expect("keyed").id
        );
        assert_ne!(left.cancels, right.cancels);
        assert_ne!(left.teardowns, right.teardowns);
        assert_ne!(cleanup_scopes(&left), cleanup_scopes(&right));
        assert_eq!(spawn_scopes(&right), vec![path(&["right"]); 2]);
    }

    // INV-RC2 through a *nested* stack: a `for_each` row's child is reached
    // under the row key, and a teardown the child itself returned is
    // qualified by that key too — so the qualification composes rather than
    // stopping at the outermost boundary.
    #[test]
    fn a_row_s_child_is_qualified_by_its_key() {
        let stack = stack();
        let mut state = RootState::new();
        state.rows.insert("row-a", ChildState::new(true));
        state.rows.insert("row-b", ChildState::new(true));

        let parts = lowered(
            &stack,
            &mut state,
            Message::Row("row-b", ChildMessage::Carriers),
        );

        assert_eq!(spawn_scopes(&parts), vec![path(&["row-b"]); 2]);
        assert_eq!(parts.teardowns, vec![path(&["row-b", "inner"])]);
        assert_eq!(cleanup_scopes(&parts), vec![path(&["row-b"])]);
        assert_eq!(
            state.rows.get(&"row-a").expect("row-a is held").seen,
            Vec::new(),
            "the sibling row was not reduced"
        );
    }

    // The parent's own command crosses no boundary and is left alone.
    #[test]
    fn a_message_the_children_do_not_claim_reaches_the_root_unqualified() {
        let stack = stack();
        let mut state = RootState::new();

        let parts = lowered(&stack, &mut state, Message::RootWork);

        assert_eq!(spawn_scopes(&parts), vec![ScopePath::empty()]);
        assert_eq!(
            parts.spawns[0].key.as_ref().expect("keyed").id,
            CommandId::new("root")
        );
    }

    // INV-RC3, the four removal shapes, each read as the teardown the
    // boundary merged into that update's command.
    #[test]
    fn removing_a_row_yields_that_row_s_teardown() {
        let stack = stack();
        let mut state = RootState::new();
        state.rows.insert("row-a", ChildState::new(true));

        let parts = lowered(&stack, &mut state, Message::Close("row-a"));

        assert_eq!(parts.teardowns, vec![path(&["row-a"])]);
    }

    #[test]
    fn replacing_a_row_yields_the_old_instance_s_teardown() {
        let stack = stack();
        let mut state = RootState::new();
        state.rows.insert("row-a", ChildState::new(true));

        let parts = lowered(&stack, &mut state, Message::Replace("row-a"));

        assert_eq!(parts.teardowns, vec![path(&["row-a"])]);
    }

    #[test]
    fn dismissing_the_slot_yields_the_boundary_s_teardown() {
        let stack = stack();
        let mut state = RootState::new();
        state.modal.present(ChildState::new(true));

        let parts = lowered(&stack, &mut state, Message::Dismiss);

        assert_eq!(parts.teardowns, vec![path(&["modal"])]);
    }

    #[test]
    fn presenting_over_an_occupied_slot_yields_the_old_occupant_s_teardown() {
        let stack = stack();
        let mut state = RootState::new();
        state.modal.present(ChildState::new(true));

        let parts = lowered(&stack, &mut state, Message::Present);

        assert_eq!(parts.teardowns, vec![path(&["modal"])]);
    }

    #[test]
    fn an_update_that_removes_nothing_yields_no_teardown() {
        let stack = stack();
        let mut state = RootState::new();

        let parts = lowered(&stack, &mut state, Message::Idle);

        assert!(parts.teardowns.is_empty());
        assert!(parts.spawns.is_empty());
    }

    // RFC 0014 §11's *diff-based removal detection* adversary at the
    // boundary: the collection is identical before and after, and the
    // teardown is still emitted — so the old instance's runs are torn down
    // and the new instance is a fresh occupant of the same key.
    #[test]
    fn a_same_update_remove_and_reinsert_still_yields_the_old_instance_s_teardown() {
        let stack = stack();
        let mut state = RootState::new();
        state.rows.insert("row-a", ChildState::new(true));

        let parts = lowered(&stack, &mut state, Message::Recreate("row-a"));

        assert_eq!(
            parts.teardowns,
            vec![path(&["row-a"])],
            "a diff of the collection would report no change at all"
        );
        assert!(state.rows.contains_key(&"row-a"), "and the key is occupied");
    }

    // A boundary's teardown merges *with* the update's own command rather
    // than replacing it — the removal and the parent's work travel in one
    // command, which is what lets the same dispatch apply the cancel phase
    // before the spawn (RFC 0013 R4).
    #[test]
    fn a_removal_and_the_update_s_own_command_travel_together() {
        let stack = stack();
        let mut state = RootState::new();
        state.modal.present(ChildState::new(true));

        // One message that both dismisses the slot and, through the root,
        // starts work: `Dismiss` returns `Command::none()`, so the row below
        // uses the slot boundary over a root command instead.
        let parts = lowered(&stack, &mut state, Message::Dismiss);
        assert_eq!(parts.teardowns, vec![path(&["modal"])]);

        state.modal.present(ChildState::new(true));
        state.rows.insert("row-a", ChildState::new(true));
        state.rows.remove(&"row-a");
        let parts = lowered(&stack, &mut state, Message::RootWork);

        assert_eq!(
            parts.teardowns,
            vec![path(&["row-a"])],
            "the pending removal is drained by the next reduce whichever branch it took"
        );
        assert_eq!(
            parts.spawns.len(),
            1,
            "and the root's own spawn is in the same command"
        );
    }

    // RFC 0014 §2.5's routing boundary: a message addressed to a key the
    // collection does not hold reaches no reducer and is discarded.
    #[test]
    fn a_message_for_an_absent_key_is_discarded() {
        let stack = stack();
        let mut state = RootState::new();

        let parts = lowered(
            &stack,
            &mut state,
            Message::Row("missing", ChildMessage::Carriers),
        );

        assert!(parts.spawns.is_empty(), "no child ran");
        assert!(parts.teardowns.is_empty());
        assert!(parts.cleanups.is_empty());
    }

    #[test]
    fn a_message_for_an_empty_slot_is_discarded() {
        let stack = stack();
        let mut state = RootState::new();

        let parts = lowered(&stack, &mut state, Message::Modal(ChildMessage::Carriers));

        assert!(parts.spawns.is_empty());
    }

    // A removal recorded before a message addressed to a *different*,
    // now-absent key is still owed its teardown: the drain does not depend
    // on the branch the message took.
    #[test]
    fn a_pending_removal_survives_a_discarded_message() {
        let stack = stack();
        let mut state = RootState::new();
        state.rows.insert("row-a", ChildState::new(true));
        state.rows.remove(&"row-a");

        let parts = lowered(
            &stack,
            &mut state,
            Message::Row("missing", ChildMessage::Quiet),
        );

        assert_eq!(parts.teardowns, vec![path(&["row-a"])]);
    }

    // INV-RC2's subscription half: the child's declarations are aggregated
    // through the boundary's **shared** projection and qualified with the
    // same segment, so two sibling boundaries declaring the same source do
    // not alias.
    #[test]
    fn child_declarations_are_aggregated_and_qualified_per_boundary() {
        let stack = stack();
        let mut state = RootState::new();
        state.rows.insert("row-a", ChildState::new(true));
        state.modal.present(ChildState::new(true));

        let declared = stack.subscriptions(&state);
        let ids: Vec<_> = declared.iter().map(|sub| sub.id().clone()).collect();

        assert_eq!(
            ids.len(),
            5,
            "the root's own, the two sibling boundaries', the row's, and the slot occupant's"
        );
        let unique: HashSet<_> = ids.iter().collect();
        assert_eq!(
            unique.len(),
            5,
            "every one of them is a distinct identity: {ids:?}"
        );
    }

    #[test]
    fn a_row_removed_from_the_collection_declares_nothing() {
        let stack = stack();
        let mut state = RootState::new();
        state.rows.insert("row-a", ChildState::new(true));
        let with_row = stack.subscriptions(&state).len();

        state.rows.remove(&"row-a");

        assert_eq!(
            stack.subscriptions(&state).len(),
            with_row - 1,
            "a removed row's declarations leave the declared set"
        );
    }

    #[test]
    fn an_empty_slot_declares_nothing() {
        let stack = stack();
        let state = RootState::new();

        let declared = stack.subscriptions(&state);

        assert_eq!(
            declared.len(),
            3,
            "the root's own and the two sibling boundaries', with no occupant to add one"
        );
    }

    // A child that declares nothing contributes nothing, so aggregation is
    // not a fixed-arity fold over boundaries.
    #[test]
    fn a_child_declaring_nothing_contributes_nothing() {
        let stack = stack();
        let mut state = RootState::new();
        state.left.subscribed = false;

        assert_eq!(stack.subscriptions(&state).len(), 2);
    }

    // `into_program` closes the stack, and the closed value's reducer half
    // is the stack's own — same routing, same qualification.
    #[test]
    fn a_closed_stack_reduces_exactly_as_the_stack_does() {
        let program = stack().into_program(
            |()| (RootState::new(), Command::none()),
            |_state: &RootState, _frame: &mut Frame<'_>| {},
        );
        let (mut state, init) = program.init(());
        assert!(init.is_none(), "the root init is the one it was given");

        let parts = lowered(&program, &mut state, Message::Left(ChildMessage::Carriers));

        assert_eq!(parts.teardowns, vec![path(&["left", "inner"])]);
        assert_eq!(cleanup_scopes(&parts), vec![path(&["left"])]);
    }
}
