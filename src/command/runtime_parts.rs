use futures::StreamExt;
use futures::stream::{BoxStream, select_all};

use crate::structural_key::ScopePath;

use super::Action;
use super::cancellation::CommandCancellation;
use super::effect::{Leaf, LeafKind};
use super::kernel_parts::{KernelParts, SpawnEntry};
use super::runtime_directives::RuntimeDirectives;
use super::{CancellableCommand, CommandId};

/// Internal command decomposition consumed by the runtime and by
/// [`TestStore`](crate::testing::TestStore).
///
/// Keeps command-owned directives paired with the effect's leaves so
/// consumers have a single lowering boundary for command execution (RFC 0008
/// INV-T3). The leaves are carried unfolded, in [`Command::batch`]'s
/// flattened declaration order; each consumer folds or drives them at its own
/// consumption site — the runtime merges their streams with [`fold_leaves`]
/// at its spawn site, while `TestStore` keeps them apart to deliver in
/// canonical per-leaf order (RFC 0008 §4.1).
///
/// There is one type and two readings of it:
/// [`into_execution_parts`](Self::into_execution_parts) is the runtime's and
/// the store's, [`into_kernel_parts`](Self::into_kernel_parts) the kernel's.
/// The execution reading returns streams only, so the metadata the kernel
/// reading needs — per-leaf spawn keys and scopes, leaf kinds, teardown
/// prefixes — is not merely unused by the older consumers but structurally
/// out of their reach.
///
/// [`Command::batch`]: super::Command::batch
#[must_use = "Runtime command parts may contain side effects and directives that must be handled by the runtime."]
pub struct RuntimeCommandParts<Msg: Send + 'static> {
    directives: RuntimeDirectives,
    leaves: Vec<Leaf<Msg>>,
    cancels: Vec<CommandId>,
    key: Option<CancellableCommand>,
    teardowns: Vec<ScopePath>,
}

impl<Msg: Send + 'static> RuntimeCommandParts<Msg> {
    pub(super) fn new(
        directives: RuntimeDirectives,
        leaves: Vec<Leaf<Msg>>,
        cancellation: CommandCancellation,
        teardowns: Vec<ScopePath>,
    ) -> Self {
        Self {
            directives,
            leaves,
            cancels: cancellation.cancels,
            key: cancellation.key,
            teardowns,
        }
    }

    pub(crate) const fn requests_redraw(&self) -> bool {
        self.directives.requests_redraw()
    }

    #[cfg(test)]
    pub(crate) fn into_stream(self) -> Option<BoxStream<'static, Action<Msg>>> {
        fold_leaves(self.leaves.into_iter().map(|leaf| leaf.stream).collect())
    }

    /// The runtime's and the store's reading, unchanged: explicit cancels,
    /// the command-level spawn key, and the leaf streams in declaration
    /// order.
    ///
    /// Leaf metadata and teardown prefixes are not returned, so the
    /// consumers of this reading cannot observe them — including an
    /// immediate-quit leaf, which comes back as the ordinary
    /// single-`Action::Quit` stream it has always been.
    pub(crate) fn into_execution_parts(
        self,
    ) -> (
        Vec<CommandId>,
        Option<CancellableCommand>,
        Vec<BoxStream<'static, Action<Msg>>>,
    ) {
        let Self {
            leaves,
            cancels,
            key,
            ..
        } = self;
        let streams = leaves.into_iter().map(|leaf| leaf.stream).collect();
        (cancels, key, streams)
    }

    /// The kernel's reading: the cancel-phase and spawn-phase buckets of
    /// RFC 0014 §3.4.
    ///
    /// Leaf metadata decides each bucket. An
    /// [`ImmediateQuit`](LeafKind::ImmediateQuit) leaf contributes
    /// `quit_now` and is not spawned — the quit applies synchronously at the
    /// dispatch's completion, so its one-item stream has no run to belong to
    /// (RFC 0014 §3.3). Every other leaf becomes a
    /// [`SpawnEntry`] whose key is the leaf's own when
    /// [`Command::batch`](super::Command::batch) pushed one down, and the
    /// command-level key otherwise.
    pub(crate) fn into_kernel_parts(self) -> KernelParts<Msg> {
        let redraw = self.requests_redraw();
        let mut quit_now = false;
        let mut spawns = Vec::with_capacity(self.leaves.len());

        for leaf in self.leaves {
            match leaf.kind {
                LeafKind::ImmediateQuit => quit_now = true,
                LeafKind::Effect => spawns.push(SpawnEntry {
                    key: leaf.key.or_else(|| self.key.clone()),
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
            spawns,
        }
    }

    /// How far a *command-level* spawn key reaches, as `(effect carriers,
    /// immediate-quit carriers)`, or `None` when the command carries no
    /// such key.
    ///
    /// This is the shape probe behind the kernel's not-constructible
    /// placeholders (RFC 0014 §3.4): a key reaching more than one effect
    /// carrier is a keyed batch, and a key reaching an immediate-quit
    /// carrier is a keyed quit. Both remain constructible through today's
    /// `Command` surface, so the check belongs at the lowering site rather
    /// than in the type.
    #[cfg(debug_assertions)]
    pub(crate) fn command_key_reach(&self) -> Option<(usize, usize)> {
        self.key.as_ref()?;
        let quits = self
            .leaves
            .iter()
            .filter(|leaf| leaf.key.is_none() && leaf.kind == LeafKind::ImmediateQuit)
            .count();
        let effects = self
            .leaves
            .iter()
            .filter(|leaf| leaf.key.is_none() && leaf.kind == LeafKind::Effect)
            .count();
        Some((effects, quits))
    }
}

/// Folds per-leaf action streams into the single merged stream a concurrent
/// consumer executes.
///
/// This is the fold `Effect::into_stream()` applied before
/// [`RuntimeCommandParts`] carried per-leaf streams: one leaf is returned
/// as-is (a `select_all` over one stream is observably identical); no leaves
/// yields no stream, which the runtime treats as no work to spawn.
// `pub` (not `pub(crate)`) because `runtime_parts` is a private module, so
// module privacy already caps this to the crate (redundant-`pub(crate)` lint).
pub fn fold_leaves<Msg: Send + 'static>(
    mut leaves: Vec<BoxStream<'static, Action<Msg>>>,
) -> Option<BoxStream<'static, Action<Msg>>> {
    match leaves.len() {
        0 => None,
        1 => leaves.pop(),
        _ => Some(select_all(leaves).boxed()),
    }
}
