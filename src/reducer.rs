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

// The three submodules are file organization, not a hierarchy a user needs
// to navigate: everything public in them is re-exported here, so each item
// has exactly one public path (`docs/api-guidelines.md`, "Single Canonical
// Path" and "Module Visibility").
pub(crate) mod adapter;
pub(crate) mod collection;
pub(crate) mod combinator;

pub use adapter::AppProgram;
pub use collection::{Keyed, ScopeValue, Slot};
pub use combinator::{ForEach, IntoProgram, Presented, ReducerExt, Scoped};

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

/// How a run of a [`Program`] ended, when it ended in the controlled way.
///
/// The production result is `Result<Exit, E>` over the backend's error
/// (RFC 0014 §2.3): a controlled quit — of either physical route — is the
/// `Ok` side, and a render failure is the `Err` side carrying the backend's
/// own error (RFC 0011 INV-LC5's classification, preserved). One variant is
/// deliberate: the two quit routes reach the same end, and the kernel keeps
/// no second controlled reason to report — and `#[non_exhaustive]` keeps
/// that a decision this crate can revisit without a breaking change, which
/// is the form RFC 0014 §2.3 declares.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Exit {
    /// A controlled quit.
    Quit,
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
