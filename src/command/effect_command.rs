//! The single effect carrier, and the keying modifiers that live only on it.
//!
//! Split out from `core.rs` for the same reason `core.rs` is split out from
//! `command.rs`: the parent module stays `pub` to host opt-in vocabulary
//! while this path stays closed, so the type has exactly one public spelling
//! (`tears::EffectCommand`).

use std::hash::Hash;
use std::time::Duration;

use futures::stream::BoxStream;

use crate::structural_key::StructuralKey;

use super::Action;
use super::cancellation::{CancelPolicy, CancellableCommand, CommandId};
use super::core::Command;
use super::effect::Leaf;
use super::runtime_directives::RuntimeDirectives;

/// One effect carrier: the value every effect constructor returns, and the
/// only value a spawn key can be attached to.
///
/// A [`Command`] may hold any number of carriers — that is what
/// [`Command::batch`] builds — so a key attached to one would have to reach
/// all of them or pick one arbitrarily. Neither is a meaning worth having,
/// which is why RFC 0014 §3.4 requires that keying a batch not be
/// constructible at all. This type is how: it holds exactly one carrier, by
/// construction rather than by convention, and
/// [`cancellable`](Self::cancellable) /
/// [`cancellable_with`](Self::cancellable_with) exist here and nowhere else.
/// `Command::batch` therefore has no key to spread and no method to acquire
/// one, and `Command::quit` — which is not an effect constructor — takes no
/// key either, matching the synchronous quit's lack of any run to name
/// (RFC 0014 §3.3).
///
/// It converts into `Command` and not back: a `Command` is precisely the
/// thing that may hold more than one carrier, so there is nothing general to
/// convert back to. Most code never names the type, because the conversion
/// sits at the end of a chain:
///
/// ```
/// use tears::prelude::*;
/// use tears::command::CommandId;
///
/// enum Message { Loaded(String) }
///
/// fn update() -> Command<Message> {
///     Command::perform(async { "data".to_string() }, Message::Loaded)
///         .cancellable(CommandId::new("load"))
///         .into()
/// }
/// ```
#[must_use = "Commands represent side effects and runtime directives in the Elm Architecture and must be handled by the runtime."]
pub struct EffectCommand<Msg: Send + 'static> {
    leaf: Leaf<Msg>,
    directives: RuntimeDirectives,
}

impl<Msg: Send + 'static> EffectCommand<Msg> {
    /// The carrier an effect constructor in [`Command`] hands back.
    pub(super) fn from_action_stream(stream: BoxStream<'static, Action<Msg>>) -> Self {
        Self {
            leaf: Leaf::effect(stream),
            directives: RuntimeDirectives::DEFAULT,
        }
    }

    /// Runs this effect under `id`, replacing whatever deliverable work the
    /// id currently names.
    ///
    /// Equivalent to [`cancellable_with`](Self::cancellable_with) under
    /// [`CancelPolicy::CancelInFlight`].
    ///
    /// # Examples
    ///
    /// ```
    /// use tears::prelude::*;
    /// use tears::command::CommandId;
    ///
    /// enum Message { Loaded(String) }
    ///
    /// let cmd: Command<Message> =
    ///     Command::perform(async { "data".to_string() }, Message::Loaded)
    ///         .cancellable(CommandId::new("load"))
    ///         .into();
    /// ```
    #[must_use = "cancellable consumes the command and returns the modified value"]
    pub fn cancellable(self, id: CommandId) -> Self {
        self.cancellable_with(id, CancelPolicy::CancelInFlight)
    }

    /// Runs this effect under `id` using the supplied same-id policy.
    ///
    /// [`CancelPolicy::CancelInFlight`] replaces current deliverable work;
    /// [`CancelPolicy::KeepInFlight`] discards this effect while the id is
    /// occupied.
    ///
    /// # Ordering with `scoped`
    ///
    /// [`scoped`](Self::scoped) only qualifies lifecycle ids already present
    /// when it is called; it is a boundary operation, not a mode inherited by
    /// later modifiers. Calling `cancellable_with` *after* `scoped`
    /// therefore installs a new, unscoped, root-global key — a scoped effect
    /// participating in an application-wide slot, which is an intentional
    /// composition rather than an accident of ordering:
    ///
    /// ```
    /// use tears::prelude::*;
    /// use tears::command::{CancelPolicy, CommandId};
    ///
    /// // Scoped key: `scoped` qualifies the id already attached here.
    /// let scoped_first = Command::message(1)
    ///     .cancellable_with(CommandId::new("load"), CancelPolicy::KeepInFlight)
    ///     .scoped("pane-1");
    ///
    /// // Root-global key: `cancellable_with` runs after `scoped`.
    /// let scoped_then_global = Command::message(1)
    ///     .scoped("pane-1")
    ///     .cancellable_with(CommandId::new("load"), CancelPolicy::KeepInFlight);
    /// ```
    #[must_use = "cancellable_with consumes the command and returns the modified value"]
    pub fn cancellable_with(mut self, id: CommandId, policy: CancelPolicy) -> Self {
        self.leaf.key = Some(CancellableCommand { id, policy });
        self
    }

    /// Qualifies this effect's lifecycle ids with one structural scope
    /// segment, expressing that it belongs to a distinct child composition
    /// boundary (RFC 0005 §4.3).
    ///
    /// The unkeyed half matters too: an effect with no key still receives the
    /// scope attribution a prefix teardown selects it by (RFC 0014 INV-RC7).
    #[must_use = "scoped consumes the command and returns the modified value"]
    pub fn scoped<Scope>(mut self, scope: Scope) -> Self
    where
        Scope: Eq + Hash + Send + Sync + 'static,
    {
        self.leaf.scoped(&StructuralKey::new(scope));
        self
    }

    /// Adds an overall deadline, starting when the effect is first polled.
    ///
    /// Messages produced before the deadline flow normally; the deadline
    /// itself emits `on_timeout` once.
    #[must_use = "timeout consumes the command and returns the modified value"]
    pub fn timeout(
        mut self,
        duration: Duration,
        on_timeout: impl FnOnce() -> Msg + Send + 'static,
    ) -> Self {
        self.leaf = self.leaf.timeout(duration, on_timeout);
        self
    }

    /// Transforms this effect's messages, preserving its identity metadata
    /// (RFC 0003 INV-12, RFC 0005 INV-17).
    #[must_use = "map consumes the command and returns the modified value"]
    pub fn map<T>(self, f: impl Fn(Msg) -> T + Send + 'static) -> EffectCommand<T>
    where
        T: Send + 'static,
    {
        EffectCommand {
            leaf: self.leaf.map(f),
            directives: self.directives,
        }
    }

    /// Suppresses the redraw this effect's dispatch would otherwise request.
    #[must_use = "without_redraw consumes the command and returns the modified value"]
    pub const fn without_redraw(mut self) -> Self {
        self.directives = self.directives.without_redraw();
        self
    }

    /// The conversion, spelled so a test can chain straight into `Command`'s
    /// crate-internal surface.
    ///
    /// `.into()` leaves the target type to inference, which cannot run
    /// backwards through a method call on the result, so an assertion that
    /// reads a `Command` member off a converted effect would have to name the
    /// type. This says it once instead. Tests only: applications convert at a
    /// site that already knows the type they are producing.
    #[cfg(test)]
    pub(crate) fn into_command(self) -> Command<Msg> {
        self.into()
    }
}

impl<Msg: Send + 'static> From<EffectCommand<Msg>> for Command<Msg> {
    fn from(effect: EffectCommand<Msg>) -> Self {
        Self::from_carrier(effect.leaf, effect.directives)
    }
}
