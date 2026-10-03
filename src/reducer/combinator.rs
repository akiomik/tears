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
//! - **Reports its occupancies** through
//!   [`instances`](Reducer::instances): the parent composition's, then the
//!   child's under the boundary — a [`Scoped`] child under its segment,
//!   each [`ForEach`] row under its key, a [`Presented`] occupant under the
//!   boundary's segment (INV-RC3a). A boundary originates no teardown of
//!   its own. After every update, live-instance reconciliation compares
//!   the report with the previous one and tears down the paths that
//!   disappeared, one teardown for a path and those beneath it (INV-RC3),
//!   whichever reducer changed the state and whichever route the message
//!   took.
//! - **Routes typed messages**: `extract` either claims a message for the
//!   child or hands it back to the parent, and a message addressed to a key
//!   or slot with no instance is routed to nothing and discarded.
//!
//! # Read/write projection pairs
//!
//! Each boundary takes its projection as a pair, because the `Reducer`
//! methods borrow the parent differently: `reduce` needs the mutable
//! projection, while `subscriptions` and `instances` need the shared one. A
//! combinator holding only the mutable accessor could not aggregate its
//! child's declarations or report from the `&Self::State` those methods are
//! given — it would have to fabricate an aliasing mutable borrow or drop the
//! child's, and INV-RC2's aggregation would be unimplementable. Both are
//! projections of state the caller already holds, so RFC 0012 INV-SE6's
//! purity is untouched. Two `fn` items per boundary is the whole cost; no
//! lens trait is introduced and none is needed.
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

use ratatui::Frame;

use crate::command::Command;
use crate::subscription::Subscription;

use super::collection::{Keyed, ScopeValue, Slot};
use super::{Instances, Program, Reducer};

/// The composition combinators, on every [`Reducer`].
///
/// Each takes a pair of projections from the parent's state to the child's,
/// one to read and one to write. Both are expected to select the same
/// state, and to select it every time. What the read half does not select
/// is never reported, so it originates no teardown of its own; and a pair
/// that switches between states — the active one of several tabs, say —
/// stops reporting the instances inside the state it left, which are torn
/// down although the parent still holds them.
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
    ///
    /// Replacing a row — [`Keyed::insert`] over an occupied key — tears the
    /// outgoing instance down before the successor runs anything it
    /// declares. While any subscription run is still stopping the runtime
    /// admits no subscription at all, so a row whose replacement stops one
    /// declares again in a later pass rather than in the pass that replaced
    /// it. That run's exit is itself a wake source, so what ends the gap is
    /// its quiescence and not an unrelated arrival. Where both instances
    /// declare the same subscription, its identity leaves and returns rather
    /// than staying.
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
    ///
    /// Replacing the occupant — [`Slot::present`] over an occupied slot —
    /// starts the successor's declarations on the timing
    /// [`for_each`](ReducerExt::for_each) states for a replaced row.
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

    /// The parent's report, then the child's under this boundary's segment.
    ///
    /// The boundary is not an occupancy of its own: it composes one child
    /// in place, whose state is always there, so replacing that state
    /// continues the prefix and only the occupancies inside it are
    /// reconciled.
    fn instances(&self, state: &P::State, out: &mut Instances<'_>) {
        self.parent.instances(state, out);
        out.scoped(self.seg.clone(), |out| {
            self.child.instances((self.state)(state), out);
        });
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

    /// Routes the message to its row.
    ///
    /// A message addressed to a key the collection does not hold reaches no
    /// reducer and is discarded (RFC 0014 §2.5's routing boundary).
    fn reduce(&self, state: &mut P::State, message: P::Message) -> Command<P::Message> {
        match (self.extract)(message) {
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
        }
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

    /// The parent's report, then each row under its key with its child's
    /// report beneath it.
    fn instances(&self, state: &P::State, out: &mut Instances<'_>) {
        self.parent.instances(state, out);
        out.keyed((self.rows)(state), |row, out| {
            self.child.instances(row, out);
        });
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

    /// Routes the message to the occupant.
    ///
    /// A claimed message reaching an empty slot is discarded, for the reason
    /// a message for an absent key is.
    fn reduce(&self, state: &mut P::State, message: P::Message) -> Command<P::Message> {
        match (self.extract)(message) {
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
        }
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

    /// The parent's report, then the occupant, if there is one, under this
    /// boundary's segment with its child's report beneath it.
    fn instances(&self, state: &P::State, out: &mut Instances<'_>) {
        self.parent.instances(state, out);
        out.slot(self.seg.clone(), (self.slot)(state), |occupant, out| {
            self.child.instances(occupant, out);
        });
    }
}

/// A closed combinator stack ([`ReducerExt::into_program`]).
///
/// Its `Reducer` half is the stack's, delegated verbatim: the same `reduce`,
/// `subscriptions`, and `instances` a composed stack has when it is not
/// closed, so closing a stack adds no execution path of its own. What it adds
/// is the two root-level functions a [`Program`] needs.
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

    fn instances(&self, state: &R::State, out: &mut Instances<'_>) {
        self.reducer.instances(state, out);
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
    use std::mem;

    use futures::stream;

    use crate::command::{CommandId, KernelParts};
    use crate::reducer::instances::{self, LiveInstances};
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
                    Command::stream(stream::pending())
                        .cancellable(CommandId::new("work"))
                        .into(),
                    Command::stream(stream::pending()).into(),
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

        fn instances(&self, _state: &Self::State, _out: &mut Instances<'_>) {}
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum Message {
        /// Addressed to the `scope` boundary's child.
        Left(ChildMessage),
        /// Addressed to the second `scope` boundary's child.
        Right(ChildMessage),
        /// Addressed to one row of the collection.
        Row(&'static str, ChildMessage),
        /// Addressed to the modal slot's occupant.
        Modal(ChildMessage),
        /// Addressed to the sheet slot's occupant.
        Sheet(ChildMessage),
        // The rest are handled by the root.
        /// Removes one row.
        Close(&'static str),
        /// Removes every row, the last inserted first.
        CloseAll,
        /// Removes one row and returns a teardown of its path.
        CloseAndTeardown(&'static str),
        /// Removes one row and returns the root's keyed work.
        CloseAndWork(&'static str),
        /// Removes one row and opts out of the redraw.
        CloseWithoutRedraw(&'static str),
        /// Inserts a fresh row, over an occupied key too.
        Insert(&'static str),
        /// Removes and re-inserts one row.
        Recreate(&'static str),
        /// Replaces one row and the modal occupant in place.
        Refresh(&'static str),
        /// Assigns a collection built anew under the same keys.
        Rebuild,
        /// Takes the rows into the unreported stash.
        Stash,
        /// Restores the stash into the rows.
        Unstash,
        /// Takes the rows and restores them in one update.
        Juggle,
        /// Dismisses the modal slot.
        Dismiss,
        /// Presents a fresh occupant in the modal slot.
        Present,
        /// Swaps the modal and the sheet slots.
        Swap,
        /// Returns the root's own keyed command.
        RootWork,
        /// Returns nothing.
        Idle,
    }

    struct RootState {
        left: ChildState,
        right: ChildState,
        rows: Keyed<&'static str, ChildState>,
        /// Composed by no boundary, so never reported.
        stash: Keyed<&'static str, ChildState>,
        modal: Slot<ChildState>,
        sheet: Slot<ChildState>,
    }

    impl RootState {
        fn new() -> Self {
            Self::with_rows(&[])
        }

        fn with_rows(keys: &[&'static str]) -> Self {
            Self {
                left: ChildState::new(true),
                right: ChildState::new(true),
                rows: keys
                    .iter()
                    .map(|key| (*key, ChildState::new(true)))
                    .collect(),
                stash: Keyed::new(),
                modal: Slot::empty(),
                sheet: Slot::empty(),
            }
        }
    }

    struct Root;

    impl Root {
        fn work() -> Command<Message> {
            Command::stream(stream::pending())
                .cancellable(CommandId::new("root"))
                .into()
        }
    }

    impl Reducer for Root {
        type State = RootState;
        type Message = Message;

        fn reduce(&self, state: &mut RootState, message: Message) -> Command<Message> {
            match message {
                Message::Close(key) => {
                    state.rows.remove(&key);
                    Command::none()
                }
                Message::CloseAll => {
                    let keys: Vec<_> = state.rows.keys().copied().collect();
                    for key in keys.into_iter().rev() {
                        state.rows.remove(&key);
                    }
                    Command::none()
                }
                Message::CloseAndTeardown(key) => {
                    state.rows.remove(&key);
                    Command::teardown(key)
                }
                Message::CloseAndWork(key) => {
                    state.rows.remove(&key);
                    Self::work()
                }
                Message::CloseWithoutRedraw(key) => {
                    state.rows.remove(&key);
                    Command::none().without_redraw()
                }
                Message::Insert(key) => {
                    state.rows.insert(key, ChildState::new(true));
                    Command::none()
                }
                Message::Recreate(key) => {
                    state.rows.remove(&key);
                    state.rows.insert(key, ChildState::new(true));
                    Command::none()
                }
                Message::Refresh(key) => {
                    *state.rows.get_mut(&key).expect("the row is held") = ChildState::new(false);
                    *state.modal.get_mut().expect("the slot is occupied") = ChildState::new(false);
                    Command::none()
                }
                Message::Rebuild => {
                    state.rows = state
                        .rows
                        .keys()
                        .map(|key| (*key, ChildState::new(true)))
                        .collect();
                    Command::none()
                }
                Message::Stash => {
                    state.stash = mem::take(&mut state.rows);
                    Command::none()
                }
                Message::Unstash => {
                    state.rows = mem::take(&mut state.stash);
                    Command::none()
                }
                Message::Juggle => {
                    let taken = mem::take(&mut state.rows);
                    state.rows = taken;
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
                Message::Swap => {
                    mem::swap(&mut state.modal, &mut state.sheet);
                    Command::none()
                }
                Message::RootWork => Self::work(),
                Message::Idle => Command::none(),
                Message::Left(_)
                | Message::Right(_)
                | Message::Row(..)
                | Message::Modal(_)
                | Message::Sheet(_) => unreachable!("a boundary claims it first"),
            }
        }

        fn subscriptions(&self, _state: &RootState) -> Vec<Subscription<Message>> {
            vec![Subscription::new(MockSource::<Message>::new())]
        }

        fn instances(&self, _state: &Self::State, _out: &mut Instances<'_>) {}
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

    fn sheet_extract(message: Message) -> Result<ChildMessage, Message> {
        match message {
            Message::Sheet(child) => Ok(child),
            other => Err(other),
        }
    }

    /// The stack most rows below reduce through: two sibling `scope`
    /// boundaries, a `for_each` over the rows, and two `presented` slots.
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
        .presented(
            Child,
            "sheet",
            |state: &RootState| &state.sheet,
            |state: &mut RootState| &mut state.sheet,
            sheet_extract,
            Message::Sheet,
        )
    }

    /// One slot reported under two paths: two `presented` boundaries over
    /// the same projection.
    fn mirrored() -> impl Reducer<State = RootState, Message = Message> {
        Root.presented(
            Child,
            "modal",
            |state: &RootState| &state.modal,
            |state: &mut RootState| &mut state.modal,
            modal_extract,
            Message::Modal,
        )
        .presented(
            Child,
            "mirror",
            |state: &RootState| &state.modal,
            |state: &mut RootState| &mut state.modal,
            modal_extract,
            Message::Modal,
        )
    }

    /// The root one level above [`stack`]: [`nested`] composes a whole stack
    /// under a fixed segment and one per pane, and `Outer` mutates both from
    /// above those boundaries.
    struct Outer;

    struct OuterState {
        inner: RootState,
        panes: Keyed<&'static str, RootState>,
    }

    impl OuterState {
        fn new() -> Self {
            Self {
                inner: RootState::new(),
                panes: Keyed::new(),
            }
        }
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum OuterMessage {
        /// Claimed by the `scope` boundary and handed to the inner stack.
        Inner(Message),
        /// Claimed by the `for_each` boundary and handed to one pane's stack.
        Pane(&'static str, Message),
        // The rest are handled by `Outer` itself.
        /// Removes one row of the inner stack's rows.
        CloseInner(&'static str),
        /// Assigns the inner stack an empty collection of rows.
        ClearInner,
        /// Replaces the inner stack's whole state.
        ResetInner,
        /// Moves the inner stack's rows into one pane.
        MoveRows(&'static str),
        /// Inserts a fresh pane holding a row, over an occupied key too.
        ReplacePane(&'static str),
    }

    impl Reducer for Outer {
        type State = OuterState;
        type Message = OuterMessage;

        fn reduce(&self, state: &mut OuterState, message: OuterMessage) -> Command<OuterMessage> {
            match message {
                OuterMessage::CloseInner(key) => {
                    state.inner.rows.remove(&key);
                }
                OuterMessage::ClearInner => state.inner.rows = Keyed::new(),
                OuterMessage::ResetInner => state.inner = RootState::new(),
                OuterMessage::MoveRows(pane) => {
                    state.panes.get_mut(&pane).expect("the pane is held").rows =
                        mem::take(&mut state.inner.rows);
                }
                OuterMessage::ReplacePane(pane) => {
                    state.panes.insert(pane, RootState::with_rows(&["row-x"]));
                }
                OuterMessage::Inner(_) | OuterMessage::Pane(..) => {
                    unreachable!("a boundary claims it first")
                }
            }
            Command::none()
        }

        fn instances(&self, _state: &Self::State, _out: &mut Instances<'_>) {}
    }

    fn outer_extract(message: OuterMessage) -> Result<Message, OuterMessage> {
        match message {
            OuterMessage::Inner(inner) => Ok(inner),
            other => Err(other),
        }
    }

    fn pane_extract(message: OuterMessage) -> Result<(&'static str, Message), OuterMessage> {
        match message {
            OuterMessage::Pane(key, inner) => Ok((key, inner)),
            other => Err(other),
        }
    }

    /// [`stack`] composed once per pane and once more under `"outer"`.
    fn nested() -> impl Reducer<State = OuterState, Message = OuterMessage> {
        Outer
            .for_each(
                stack(),
                |state: &OuterState| &state.panes,
                |state: &mut OuterState| &mut state.panes,
                pane_extract,
                OuterMessage::Pane,
            )
            .scope(
                stack(),
                "outer",
                |state: &OuterState| &state.inner,
                |state: &mut OuterState| &mut state.inner,
                outer_extract,
                OuterMessage::Inner,
            )
    }

    /// [`stack`]'s slot below its rows: the `for_each` boundary's parent is
    /// a `presented` one, so the slot is reported only if `for_each`
    /// forwards its parent's report.
    fn slot_beneath_rows() -> impl Reducer<State = RootState, Message = Message> {
        Root.presented(
            Child,
            "modal",
            |state: &RootState| &state.modal,
            |state: &mut RootState| &mut state.modal,
            modal_extract,
            Message::Modal,
        )
        .for_each(
            Child,
            |state: &RootState| &state.rows,
            |state: &mut RootState| &mut state.rows,
            row_extract,
            Message::Row,
        )
    }

    /// A root whose slot's occupant is a whole [`stack`], so the occupant's
    /// rows are reported only if `presented` forwards the occupant's report.
    struct Host;

    struct HostState {
        modal: Slot<RootState>,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum HostMessage {
        /// Claimed by the `presented` boundary and handed to the occupant.
        Modal(Message),
    }

    impl Reducer for Host {
        type State = HostState;
        type Message = HostMessage;

        fn reduce(&self, _state: &mut HostState, _message: HostMessage) -> Command<HostMessage> {
            unreachable!("the boundary claims every message")
        }

        fn instances(&self, _state: &Self::State, _out: &mut Instances<'_>) {}
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "`presented` takes an extract that can hand a message back"
    )]
    fn host_extract(message: HostMessage) -> Result<Message, HostMessage> {
        let HostMessage::Modal(inner) = message;
        Ok(inner)
    }

    fn hosted() -> impl Reducer<State = HostState, Message = HostMessage> {
        Host.presented(
            stack(),
            "modal",
            |state: &HostState| &state.modal,
            |state: &mut HostState| &mut state.modal,
            host_extract,
            HostMessage::Modal,
        )
    }

    /// Two tabs of rows, and a `for_each` whose projections select the
    /// active one — the switching pair `ReducerExt` warns about.
    struct Tabs;

    struct TabsState {
        first_active: bool,
        first: Keyed<&'static str, ChildState>,
        second: Keyed<&'static str, ChildState>,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum TabsMessage {
        /// Claimed by the `for_each` boundary and handed to one row.
        Row(&'static str, ChildMessage),
        /// Handled by `Tabs` itself: makes the other tab active.
        Switch,
    }

    impl Reducer for Tabs {
        type State = TabsState;
        type Message = TabsMessage;

        fn reduce(&self, state: &mut TabsState, message: TabsMessage) -> Command<TabsMessage> {
            match message {
                TabsMessage::Switch => state.first_active = !state.first_active,
                TabsMessage::Row(..) => unreachable!("the boundary claims it first"),
            }
            Command::none()
        }

        fn instances(&self, _state: &Self::State, _out: &mut Instances<'_>) {}
    }

    fn tabs_extract(message: TabsMessage) -> Result<(&'static str, ChildMessage), TabsMessage> {
        match message {
            TabsMessage::Row(key, child) => Ok((key, child)),
            other @ TabsMessage::Switch => Err(other),
        }
    }

    fn tabs() -> impl Reducer<State = TabsState, Message = TabsMessage> {
        Tabs.for_each(
            Child,
            |state: &TabsState| {
                if state.first_active {
                    &state.first
                } else {
                    &state.second
                }
            },
            |state: &mut TabsState| {
                if state.first_active {
                    &mut state.first
                } else {
                    &mut state.second
                }
            },
            tabs_extract,
            TabsMessage::Row,
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

    fn spawn_scopes<M: Send + 'static>(parts: &KernelParts<M>) -> Vec<ScopePath> {
        parts
            .spawns
            .iter()
            .map(|spawn| spawn.scope.clone())
            .collect()
    }

    fn cleanup_scopes<M: Send + 'static>(parts: &KernelParts<M>) -> Vec<ScopePath> {
        parts
            .cleanups
            .iter()
            .map(|registration| registration.scope.clone())
            .collect()
    }

    /// Drives updates through the seam the kernel and the store share:
    /// `reduce`, the report, then reconciliation against the previous report,
    /// seeded from the starting state.
    struct Driven<R: Reducer> {
        reducer: R,
        state: R::State,
        live: LiveInstances,
    }

    impl<R: Reducer> Driven<R> {
        fn new(reducer: R, state: R::State) -> Self {
            let live = LiveInstances::new(&reducer, &state);
            Self {
                reducer,
                state,
                live,
            }
        }

        /// One update, lowered from the command reconciliation dispatches.
        fn send(&mut self, message: R::Message) -> KernelParts<R::Message> {
            self.live
                .update(&self.reducer, &mut self.state, message)
                .into_runtime_parts()
                .into_kernel_parts()
        }

        /// The paths the current state reports, in report order.
        fn reported(&self) -> Vec<ScopePath> {
            instances::report(&self.reducer, &self.state)
                .into_iter()
                .map(|(path, _)| path)
                .collect()
        }
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

    // INV-RC2 at a collection boundary: a `for_each` row's child is reached
    // under the row key, and a teardown the child itself returned is
    // qualified by that key too — so a carrier the child had already
    // scoped is re-anchored rather than left alone. (Two boundaries stacked
    // over one another are the separate row below.)
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

        assert!(parts.spawns.is_empty(), "no child ran");
        assert!(parts.teardowns.is_empty());
        assert!(parts.cleanups.is_empty());
    }

    // INV-RC3, read from the command reconciliation dispatches
    // (`Driven::send`), never from `reduce`'s return value. First, one row
    // per way a pair disappears.
    #[test]
    fn removing_a_row_yields_that_row_s_teardown() {
        let mut driven = Driven::new(stack(), RootState::with_rows(&["row-a", "row-b"]));

        let parts = driven.send(Message::Close("row-a"));

        assert_eq!(parts.teardowns, vec![path(&["row-a"])]);
        assert!(parts.redraw, "the update's own redraw is untouched");
    }

    #[test]
    fn dismissing_the_slot_yields_its_occupant_s_teardown() {
        let mut state = RootState::new();
        state.modal.present(ChildState::new(true));
        let mut driven = Driven::new(stack(), state);

        assert_eq!(
            driven.send(Message::Dismiss).teardowns,
            vec![path(&["modal"])]
        );
    }

    #[test]
    fn replacing_a_row_yields_the_old_instance_s_teardown() {
        let mut driven = Driven::new(stack(), RootState::with_rows(&["row-a"]));

        assert_eq!(
            driven.send(Message::Insert("row-a")).teardowns,
            vec![path(&["row-a"])]
        );
    }

    #[test]
    fn presenting_over_an_occupied_slot_yields_the_old_occupant_s_teardown() {
        let mut state = RootState::new();
        state.modal.present(ChildState::new(true));
        let mut driven = Driven::new(stack(), state);

        assert_eq!(
            driven.send(Message::Present).teardowns,
            vec![path(&["modal"])]
        );
    }

    // Equal keys before and after: a key-only comparison sees no change.
    #[test]
    fn assigning_a_collection_with_the_same_keys_tears_down_every_row() {
        let mut driven = Driven::new(stack(), RootState::with_rows(&["row-a", "row-b"]));

        assert_eq!(
            driven.send(Message::Rebuild).teardowns,
            vec![path(&["row-a"]), path(&["row-b"])]
        );
    }

    #[test]
    fn taking_a_collection_tears_down_its_rows() {
        let mut driven = Driven::new(stack(), RootState::with_rows(&["row-a", "row-b"]));

        assert_eq!(
            driven.send(Message::Stash).teardowns,
            vec![path(&["row-a"]), path(&["row-b"])]
        );
    }

    // Each slot keeps its identity across the swap; only its path changes.
    #[test]
    fn swapping_two_occupied_slots_tears_down_both_paths() {
        let mut state = RootState::new();
        state.modal.present(ChildState::new(true));
        state.sheet.present(ChildState::new(true));
        let mut driven = Driven::new(stack(), state);

        assert_eq!(
            driven.send(Message::Swap).teardowns,
            vec![path(&["modal"]), path(&["sheet"])]
        );
    }

    // An occupancy that disappears inside one that stays: the inner path is
    // torn down on its own, since no enclosing path disappeared with it.
    #[test]
    fn closing_a_row_inside_a_pane_tears_down_that_row_s_path() {
        let mut state = OuterState::new();
        state
            .panes
            .insert("pane-a", RootState::with_rows(&["row-x"]));
        let mut driven = Driven::new(nested(), state);

        assert_eq!(
            driven
                .send(OuterMessage::Pane("pane-a", Message::Close("row-x")))
                .teardowns,
            vec![path(&["pane-a", "row-x"])]
        );
    }

    // `for_each` forwards its parent's report: the slot below it is reported,
    // so dismissing it is torn down.
    #[test]
    fn a_slot_beneath_rows_is_torn_down_through_the_rows_boundary() {
        let mut state = RootState::with_rows(&["row-a"]);
        state.modal.present(ChildState::new(true));
        let mut driven = Driven::new(slot_beneath_rows(), state);

        assert_eq!(
            driven.send(Message::Dismiss).teardowns,
            vec![path(&["modal"])]
        );
    }

    // `presented` forwards its occupant's report beneath the slot's segment:
    // a row the occupant closes is torn down under the slot.
    #[test]
    fn a_row_closed_inside_an_occupant_is_torn_down_beneath_the_slot() {
        let mut modal = Slot::empty();
        modal.present(RootState::with_rows(&["row-x"]));
        let mut driven = Driven::new(hosted(), HostState { modal });

        assert_eq!(
            driven
                .send(HostMessage::Modal(Message::Close("row-x")))
                .teardowns,
            vec![path(&["modal", "row-x"])]
        );
    }

    // A pair that switches between states stops reporting the rows of the
    // state it left, and they are torn down although the parent holds them.
    #[test]
    fn switching_the_projected_tab_tears_down_the_rows_it_left() {
        let mut state = TabsState {
            first_active: true,
            first: Keyed::new(),
            second: Keyed::new(),
        };
        state.first.insert("row-a", ChildState::new(true));
        state.second.insert("row-b", ChildState::new(true));
        let mut driven = Driven::new(tabs(), state);

        assert_eq!(
            driven.send(TabsMessage::Switch).teardowns,
            vec![path(&["row-a"])]
        );
    }

    #[test]
    fn moving_a_collection_to_another_path_tears_down_its_old_paths() {
        let mut state = OuterState::new();
        state.inner = RootState::with_rows(&["row-a"]);
        state.panes.insert("pane-a", RootState::new());
        let mut driven = Driven::new(nested(), state);

        let parts = driven.send(OuterMessage::MoveRows("pane-a"));

        assert_eq!(parts.teardowns, vec![path(&["outer", "row-a"])]);
        assert_eq!(
            driven.reported(),
            vec![path(&["pane-a"]), path(&["pane-a", "row-a"])],
            "the row is reported at its new path, under the same identity"
        );
    }

    // A fixed `scope` boundary is no occupancy: its own path is not torn
    // down, the occupancies inside the replaced state are.
    #[test]
    fn replacing_a_scope_boundary_s_child_state_tears_down_the_occupancies_inside() {
        let mut state = OuterState::new();
        state.inner = RootState::with_rows(&["row-a"]);
        state.inner.modal.present(ChildState::new(true));
        let mut driven = Driven::new(nested(), state);

        assert_eq!(
            driven.send(OuterMessage::ResetInner).teardowns,
            vec![path(&["outer", "row-a"]), path(&["outer", "modal"])]
        );
    }

    #[test]
    fn replacing_an_occupancy_that_holds_occupancies_tears_down_only_its_path() {
        let mut state = OuterState::new();
        state
            .panes
            .insert("pane-a", RootState::with_rows(&["row-x"]));
        let mut driven = Driven::new(nested(), state);

        assert_eq!(
            driven.send(OuterMessage::ReplacePane("pane-a")).teardowns,
            vec![path(&["pane-a"])],
            "[pane-a, row-x] disappears too, and the outer teardown selects it"
        );
    }

    // A reducer above an enclosing `scope` boundary changes the state the
    // boundary projects, in the same update it is torn down in.
    #[test]
    fn a_removal_above_an_enclosing_scope_is_torn_down_in_the_same_update() {
        let mut state = OuterState::new();
        state.inner = RootState::with_rows(&["row-a", "row-b"]);
        let mut driven = Driven::new(nested(), state);

        assert_eq!(
            driven.send(OuterMessage::CloseInner("row-a")).teardowns,
            vec![path(&["outer", "row-a"])]
        );
    }

    #[test]
    fn an_assignment_above_an_enclosing_scope_is_torn_down_in_the_same_update() {
        let mut state = OuterState::new();
        state.inner = RootState::with_rows(&["row-a", "row-b"]);
        let mut driven = Driven::new(nested(), state);

        assert_eq!(
            driven.send(OuterMessage::ClearInner).teardowns,
            vec![path(&["outer", "row-a"]), path(&["outer", "row-b"])]
        );
    }

    // RFC 0014 §11's key-only removal detection adversary: the keys are
    // equal before and after, and the old instance is still torn down.
    #[test]
    fn a_same_update_remove_and_reinsert_still_yields_the_old_instance_s_teardown() {
        let mut driven = Driven::new(stack(), RootState::with_rows(&["row-a"]));

        let parts = driven.send(Message::Recreate("row-a"));

        assert_eq!(parts.teardowns, vec![path(&["row-a"])]);
        assert!(
            driven.state.rows.contains_key(&"row-a"),
            "the key is occupied"
        );
    }

    // A stack closed with `into_program` forwards its report, so an occupancy
    // in the state its `init` returns is torn down by the first update. That
    // the kernel and the store read the first report from that state is
    // their rows' to pin.
    #[test]
    fn an_occupancy_in_init_s_state_is_torn_down_by_the_first_update() {
        let program = stack().into_program(
            |()| (RootState::with_rows(&["row-a"]), Command::none()),
            |_state: &RootState, _frame: &mut Frame<'_>| {},
        );
        let (state, _) = program.init(());
        let mut driven = Driven::new(program, state);

        assert_eq!(
            driven.send(Message::Close("row-a")).teardowns,
            vec![path(&["row-a"])]
        );
    }

    #[test]
    fn a_slot_reported_under_two_paths_is_torn_down_under_each() {
        let mut state = RootState::new();
        state.modal.present(ChildState::new(true));
        let mut driven = Driven::new(mirrored(), state);

        assert!(
            driven.send(Message::Idle).teardowns.is_empty(),
            "an unrelated message tears neither path down"
        );
        assert_eq!(
            driven.send(Message::Dismiss).teardowns,
            vec![path(&["modal"]), path(&["mirror"])]
        );
    }

    // Two occupancies reported under one path — two slots under one segment
    // here, two sibling `for_each`s over one key type in an application
    // (#424) — disappear together and yield one teardown of that path. The
    // segment type is not `Clone`: `Instances` asks of a segment only what
    // `Command::scoped` does.
    #[test]
    fn two_pairs_gone_from_one_path_yield_one_teardown_of_it() {
        #[derive(PartialEq, Eq, Hash)]
        struct Pane;

        struct Twins;

        impl Reducer for Twins {
            type State = (Slot<()>, Slot<()>);
            type Message = ();

            fn reduce(&self, state: &mut Self::State, (): ()) -> Command<()> {
                state.0.dismiss();
                state.1.dismiss();
                Command::none()
            }

            fn instances(&self, state: &Self::State, out: &mut Instances<'_>) {
                out.slot(Pane, &state.0, |(), _| {});
                out.slot(Pane, &state.1, |(), _| {});
            }
        }

        let mut state = (Slot::empty(), Slot::empty());
        state.0.present(());
        state.1.present(());
        let mut driven = Driven::new(Twins, state);

        assert_eq!(
            driven.send(()).teardowns,
            vec![ScopePath::empty().prefixed(Pane)]
        );
    }

    // Rows that tear nothing down.

    // A row and an occupant replaced while the starting state was built —
    // in `init`, in an application — were never reported, so the first
    // update tears neither down. Recording replacements as they are made
    // would tear down the path the successor holds at the first update
    // through the boundary; this row is what fails if that comes back.
    #[test]
    fn a_replacement_made_before_the_first_report_tears_nothing_down() {
        let mut state = RootState::with_rows(&["row-a"]);
        state.rows.insert("row-a", ChildState::new(true));
        state.modal.present(ChildState::new(true));
        state.modal.present(ChildState::new(true));
        let mut driven = Driven::new(stack(), state);

        assert!(driven.send(Message::Idle).teardowns.is_empty());
    }

    #[test]
    fn an_update_that_removes_nothing_yields_no_teardown() {
        let mut state = RootState::with_rows(&["row-a"]);
        state.modal.present(ChildState::new(true));
        let mut driven = Driven::new(stack(), state);

        let parts = driven.send(Message::Idle);

        assert!(parts.teardowns.is_empty());
        assert!(parts.spawns.is_empty());
    }

    #[test]
    fn replacing_a_value_in_place_tears_nothing_down() {
        let mut state = RootState::with_rows(&["row-a"]);
        state.modal.present(ChildState::new(true));
        let mut driven = Driven::new(stack(), state);

        assert!(driven.send(Message::Refresh("row-a")).teardowns.is_empty());
    }

    #[test]
    fn a_take_and_restore_within_one_update_tears_nothing_down() {
        let mut driven = Driven::new(stack(), RootState::with_rows(&["row-a"]));

        assert!(driven.send(Message::Juggle).teardowns.is_empty());
    }

    #[test]
    fn restoring_a_collection_taken_in_an_earlier_update_tears_nothing_down() {
        let mut driven = Driven::new(stack(), RootState::with_rows(&["row-a"]));
        assert_eq!(
            driven.send(Message::Stash).teardowns,
            vec![path(&["row-a"])]
        );

        assert!(driven.send(Message::Unstash).teardowns.is_empty());
    }

    // The merge adds teardown entries and nothing else.
    #[test]
    fn an_update_s_own_teardown_of_a_reconciled_path_is_kept_beside_it() {
        let mut driven = Driven::new(stack(), RootState::with_rows(&["row-a"]));

        assert_eq!(
            driven.send(Message::CloseAndTeardown("row-a")).teardowns,
            vec![path(&["row-a"]), path(&["row-a"])]
        );
    }

    // One command, so the cancel phase precedes the spawn (RFC 0013 R4).
    #[test]
    fn a_removal_and_the_update_s_own_command_travel_together() {
        let mut driven = Driven::new(stack(), RootState::with_rows(&["row-a"]));

        let parts = driven.send(Message::CloseAndWork("row-a"));

        assert_eq!(parts.teardowns, vec![path(&["row-a"])]);
        assert_eq!(spawn_scopes(&parts), vec![ScopePath::empty()]);
        assert_eq!(
            parts.spawns[0].key.as_ref().expect("keyed").id,
            CommandId::new("root"),
            "the update's own spawn keeps its key"
        );
    }

    #[test]
    fn a_removal_in_an_update_without_redraw_stays_without_redraw() {
        let mut driven = Driven::new(stack(), RootState::with_rows(&["row-a"]));

        let parts = driven.send(Message::CloseWithoutRedraw("row-a"));

        assert_eq!(parts.teardowns, vec![path(&["row-a"])]);
        assert!(!parts.redraw);
    }

    // The baseline advances: the reinserted row is the next comparison's.
    #[test]
    fn a_key_reinserted_in_a_later_update_is_not_torn_down_again() {
        let mut driven = Driven::new(stack(), RootState::with_rows(&["row-a"]));

        assert_eq!(
            driven.send(Message::Close("row-a")).teardowns,
            vec![path(&["row-a"])]
        );
        assert!(driven.send(Message::Insert("row-a")).teardowns.is_empty());
        assert!(driven.send(Message::Idle).teardowns.is_empty());
    }

    // INV-RC14: one script, one teardown sequence — the previous report's
    // order, not a hash set's, and not the order the update removed them in,
    // which is the reverse here.
    #[test]
    fn removing_sibling_rows_yields_one_teardown_sequence_per_script() {
        const KEYS: [&str; 8] = [
            "row-h", "row-c", "row-f", "row-a", "row-e", "row-b", "row-g", "row-d",
        ];
        let run = || {
            Driven::new(stack(), RootState::with_rows(&KEYS))
                .send(Message::CloseAll)
                .teardowns
        };

        let first = run();

        assert_eq!(first, run());
        assert_eq!(first, KEYS.map(|key| path(&[key])).to_vec());
    }

    // INV-RC3a: a stack reports each occupancy under the path its child's
    // carriers are qualified with.
    #[test]
    fn nested_stacks_report_each_occupancy_where_its_carriers_are_placed() {
        let mut state = OuterState::new();
        state.inner = RootState::with_rows(&["row-a"]);
        state.inner.modal.present(ChildState::new(true));
        state
            .panes
            .insert("pane-a", RootState::with_rows(&["row-x"]));
        let mut driven = Driven::new(nested(), state);

        let placed: Vec<ScopePath> = [
            OuterMessage::Pane("pane-a", Message::RootWork),
            OuterMessage::Pane("pane-a", Message::Row("row-x", ChildMessage::Carriers)),
            OuterMessage::Inner(Message::Row("row-a", ChildMessage::Carriers)),
            OuterMessage::Inner(Message::Modal(ChildMessage::Carriers)),
        ]
        .into_iter()
        .map(|message| spawn_scopes(&driven.send(message))[0].clone())
        .collect();

        assert_eq!(driven.reported(), placed);
        assert_eq!(
            placed,
            vec![
                path(&["pane-a"]),
                path(&["pane-a", "row-x"]),
                path(&["outer", "row-a"]),
                path(&["outer", "modal"]),
            ]
        );
    }

    #[test]
    fn an_occupancy_the_last_message_did_not_reach_stays_reported() {
        let mut state = OuterState::new();
        state.inner = RootState::with_rows(&["row-a"]);
        state
            .panes
            .insert("pane-a", RootState::with_rows(&["row-x"]));
        let mut driven = Driven::new(nested(), state);
        let before = driven.reported();

        let parts = driven.send(OuterMessage::Inner(Message::Row(
            "row-a",
            ChildMessage::Quiet,
        )));

        assert!(parts.teardowns.is_empty());
        assert_eq!(driven.reported(), before);
        assert_eq!(before.len(), 3, "the pane, its row, and the inner row");
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

    // The outer `scope` composes a whole stack as its child: every carrier
    // the inner boundary qualified with a row key is re-anchored under the
    // outer segment, and so is the path a removal inside it is torn down at.
    #[test]
    fn two_stacked_boundaries_compose_their_segments() {
        let mut state = OuterState::new();
        state.inner = RootState::with_rows(&["row-a", "row-b"]);
        let mut driven = Driven::new(nested(), state);

        let parts = driven.send(OuterMessage::Inner(Message::Row(
            "row-b",
            ChildMessage::Carriers,
        )));

        assert_eq!(
            spawn_scopes(&parts),
            vec![path(&["outer", "row-b"]); 2],
            "the inner boundary placed the runs under the row key, the outer under its segment"
        );
        assert_eq!(parts.teardowns, vec![path(&["outer", "row-b", "inner"])]);
        assert_eq!(cleanup_scopes(&parts), vec![path(&["outer", "row-b"])]);

        assert_eq!(
            driven
                .send(OuterMessage::Inner(Message::Close("row-a")))
                .teardowns,
            vec![path(&["outer", "row-a"])]
        );
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
