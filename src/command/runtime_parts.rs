use futures::StreamExt;
use futures::stream::{BoxStream, select_all};

use super::Action;
use super::cancellation::CommandCancellation;
use super::runtime_directives::RuntimeDirectives;
use super::{CancellableCommand, CommandId};

/// Internal command decomposition consumed by the runtime and by
/// [`TestStore`](crate::testing::TestStore).
///
/// Keeps command-owned directives paired with the effect's leaf streams so
/// consumers have a single lowering boundary for command execution (RFC 0008
/// INV-T3). The leaves are carried unfolded, in [`Command::batch`]'s flattened
/// declaration order; each consumer folds or drives them at its own
/// consumption site — the runtime merges them with [`fold_leaves`] at its
/// spawn site, while `TestStore` keeps them apart to deliver in canonical
/// per-leaf order (RFC 0008 §4.1).
///
/// [`Command::batch`]: super::Command::batch
#[must_use = "Runtime command parts may contain side effects and directives that must be handled by the runtime."]
pub struct RuntimeCommandParts<Msg: Send + 'static> {
    directives: RuntimeDirectives,
    leaves: Vec<BoxStream<'static, Action<Msg>>>,
    cancels: Vec<CommandId>,
    key: Option<CancellableCommand>,
}

impl<Msg: Send + 'static> RuntimeCommandParts<Msg> {
    pub(super) fn new(
        directives: RuntimeDirectives,
        leaves: Vec<BoxStream<'static, Action<Msg>>>,
        cancellation: CommandCancellation,
    ) -> Self {
        Self {
            directives,
            leaves,
            cancels: cancellation.cancels,
            key: cancellation.key,
        }
    }

    pub(crate) const fn requests_redraw(&self) -> bool {
        self.directives.requests_redraw()
    }

    #[cfg(test)]
    pub(crate) fn into_stream(self) -> Option<BoxStream<'static, Action<Msg>>> {
        fold_leaves(self.leaves)
    }

    pub(crate) fn into_execution_parts(
        self,
    ) -> (
        Vec<CommandId>,
        Option<CancellableCommand>,
        Vec<BoxStream<'static, Action<Msg>>>,
    ) {
        (self.cancels, self.key, self.leaves)
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
