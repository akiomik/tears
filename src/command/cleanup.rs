//! The cleanup-hook carrier: what [`Command::on_teardown`] puts on a
//! command.
//!
//! A registration is command *metadata*, like a teardown prefix and unlike
//! an effect leaf: dispatching the command arms it and starts nothing. What
//! starts it is a later teardown whose prefix covers its scope, at that
//! teardown's application point (RFC 0014 §4.4, RFC 0013 §5).
//!
//! The finalizer's `Output = ()` is the signature half of INV-RC8's
//! no-output clause: there is no message for a cleanup run to return, so the
//! path back into the runtime is closed in the type rather than by the
//! kernel remembering to ignore something. The other half is structural and
//! lives at the run's construction site, where the kernel hands a cleanup
//! run no lane sender and no directive capability
//! (`kernel::producer::CleanupHarness`).
//!
//! [`Command::on_teardown`]: super::Command::on_teardown

use futures::FutureExt;
use futures::future::BoxFuture;

use crate::structural_key::{ScopePath, StructuralKey};

/// One armed finalizer and the composition boundary it is registered
/// against.
///
/// It carries no message type: a cleanup run produces none, so a
/// registration survives [`Command::map`](super::Command::map) unchanged
/// rather than being re-wrapped at every boundary that maps a message type.
pub struct CleanupRegistration {
    /// The scope this registration is anchored at — the path a teardown
    /// prefix has to cover to consume it (RFC 0013 INV-ST1).
    ///
    /// Empty at construction and qualified by
    /// [`Command::scoped`](super::Command::scoped) and by the combinators,
    /// exactly as a teardown prefix is (RFC 0005 INV-18's coverage).
    pub scope: ScopePath,
    /// The finalizer itself.
    pub finalizer: BoxFuture<'static, ()>,
}

impl CleanupRegistration {
    /// A registration anchored at the root, for a finalizer under
    /// `Command`'s ordinary effect bounds.
    pub fn new(finalizer: impl Future<Output = ()> + Send + 'static) -> Self {
        Self {
            scope: ScopePath::empty(),
            finalizer: finalizer.boxed(),
        }
    }

    /// Qualifies this registration with one already-erased boundary segment.
    ///
    /// Takes the erased segment rather than the scope value so that one
    /// `scoped` call can share a single erasure across every carrier at its
    /// boundary — the spawn key, the explicit cancels, the teardown
    /// prefixes, and this — without the scope type itself being `Clone`
    /// (RFC 0005 §8.1).
    #[must_use]
    pub fn scoped_with(self, segment: StructuralKey) -> Self {
        Self {
            scope: self.scope.prefixed_key(segment),
            finalizer: self.finalizer,
        }
    }
}
