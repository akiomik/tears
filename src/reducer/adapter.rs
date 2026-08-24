//! The [`Application`] facade adapter.
//!
//! [`AppProgram`] is a mapping and nothing else: the application value *is*
//! the state, `update` *is* `reduce`. It holds no state of its own — the
//! `PhantomData` fixes `A` without owning a value — so there is nothing for
//! a fast path to hang off.
//!
//! That emptiness is the point. INV-RC1 requires that for every kernel
//! concern and every phase step, an `Application`-adapted program and a
//! composed program execute the same code, with the facade contributing
//! mapping calls only: no dedicated channel, no branch, no phase. The
//! structural half of that invariant is a review of this file and of the
//! kernel's entry — there must be no `Application`-typed branch below the
//! adapter — and this is the whole of the adapter to review.

use std::marker::PhantomData;

use ratatui::Frame;

use crate::application::Application;
use crate::command::Command;
use crate::subscription::Subscription;

use super::{Program, Reducer};

/// Runs an [`Application`] as a [`Program`].
pub struct AppProgram<A>(PhantomData<fn() -> A>);

impl<A> AppProgram<A> {
    /// The adapter for `A`. Carries nothing, so construction is inert.
    #[must_use]
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

impl<A> Default for AppProgram<A> {
    fn default() -> Self {
        Self::new()
    }
}

impl<A: Application> Reducer for AppProgram<A> {
    type State = A;
    type Message = A::Message;

    fn reduce(&self, state: &mut A, message: A::Message) -> Command<A::Message> {
        state.update(message)
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
        state.view(frame);
    }
}
