#[cfg(test)]
use futures::stream::BoxStream;
#[cfg(test)]
use futures::{StreamExt, stream::select_all};

use crate::structural_key::ScopePath;

use super::CommandId;
use super::cancellation::CommandCancellation;
use super::cleanup::CleanupRegistration;
use super::effect::{Leaf, LeafKind};
use super::kernel_parts::{KernelParts, SpawnEntry};
use super::runtime_directives::RuntimeDirectives;

/// Internal command decomposition consumed by the runtime and by
/// [`TestStore`](crate::testing::TestStore).
///
/// Keeps command-owned directives paired with the effect's leaves so
/// consumers have a single lowering boundary for command execution (RFC 0008
/// INV-T3). The leaves are carried unfolded, in [`Command::batch`]'s
/// flattened declaration order, and each consumer drives them at its own
/// consumption site: the kernel spawns one run per leaf, while `TestStore`
/// keeps them apart to deliver in canonical per-leaf order (RFC 0008 §4.1).
///
/// One type, one reading — [`into_kernel_parts`](Self::into_kernel_parts).
/// The second reading this type carried through the transition existed for
/// the superseded runtime, which folded the leaves into one stream and read
/// a command-level key; both went with it.
///
/// [`Command::batch`]: super::Command::batch
#[must_use = "Runtime command parts may contain side effects and directives that must be handled by the runtime."]
pub struct RuntimeCommandParts<Msg: Send + 'static> {
    directives: RuntimeDirectives,
    leaves: Vec<Leaf<Msg>>,
    cancels: Vec<CommandId>,
    teardowns: Vec<ScopePath>,
    cleanups: Vec<CleanupRegistration>,
}

impl<Msg: Send + 'static> RuntimeCommandParts<Msg> {
    pub(super) fn new(
        directives: RuntimeDirectives,
        leaves: Vec<Leaf<Msg>>,
        cancellation: CommandCancellation,
        teardowns: Vec<ScopePath>,
        cleanups: Vec<CleanupRegistration>,
    ) -> Self {
        Self {
            directives,
            leaves,
            cancels: cancellation.cancels,
            teardowns,
            cleanups,
        }
    }

    pub(crate) const fn requests_redraw(&self) -> bool {
        self.directives.requests_redraw()
    }

    #[cfg(test)]
    pub(crate) fn into_stream(self) -> Option<BoxStream<'static, super::Action<Msg>>> {
        fold_leaves(self.leaves.into_iter().map(|leaf| leaf.stream).collect())
    }

    /// The kernel's reading: the cancel-phase and spawn-phase buckets of
    /// RFC 0014 §3.4.
    ///
    /// Leaf metadata decides each bucket. An
    /// [`ImmediateQuit`](LeafKind::ImmediateQuit) leaf contributes
    /// `quit_now` and is not spawned — the quit applies synchronously at the
    /// dispatch's completion, so its one-item stream has no run to belong to
    /// (RFC 0014 §3.3). Every other leaf becomes a
    /// [`SpawnEntry`] carrying its own key, written there by
    /// [`EffectCommand::cancellable`](super::EffectCommand::cancellable)
    /// while the carrier was still on its own.
    pub(crate) fn into_kernel_parts(self) -> KernelParts<Msg> {
        let redraw = self.requests_redraw();
        let mut quit_now = false;
        let mut spawns = Vec::with_capacity(self.leaves.len());

        for leaf in self.leaves {
            match leaf.kind {
                LeafKind::ImmediateQuit => quit_now = true,
                LeafKind::Effect => spawns.push(SpawnEntry {
                    key: leaf.key,
                    scope: leaf.scope,
                    stream: leaf.stream,
                }),
            }
        }

        KernelParts {
            redraw,
            quit_now,
            cancels: self.cancels,
            teardowns: self.teardowns,
            cleanups: self.cleanups,
            spawns,
        }
    }
}

/// Merges per-leaf action streams into one.
///
/// Test-only, and it is the shape production stopped needing: the kernel
/// spawns one run per carrier, so nothing folds them any more. The command
/// layer's own tests still read a command as a single stream, which is what
/// this is for.
// `pub` (not `pub(crate)`) because `runtime_parts` is a private module, so
// module privacy already caps this to the crate (redundant-`pub(crate)` lint).
#[cfg(test)]
pub fn fold_leaves<Msg: Send + 'static>(
    mut leaves: Vec<BoxStream<'static, super::Action<Msg>>>,
) -> Option<BoxStream<'static, super::Action<Msg>>> {
    match leaves.len() {
        0 => None,
        1 => leaves.pop(),
        _ => Some(select_all(leaves).boxed()),
    }
}
