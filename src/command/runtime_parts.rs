use futures::stream::BoxStream;

use super::Action;
use super::cancellation::CommandCancellation;
use super::runtime_directives::RuntimeDirectives;
use super::{CancellableCommand, CommandId};

/// Internal command decomposition consumed by the runtime.
///
/// Keeps command-owned directives paired with the action stream so runtime code
/// has a single lowering boundary for command execution.
#[must_use = "Runtime command parts may contain side effects and directives that must be handled by the runtime."]
pub struct RuntimeCommandParts<Msg: Send + 'static> {
    directives: RuntimeDirectives,
    stream: Option<BoxStream<'static, Action<Msg>>>,
    cancels: Vec<CommandId>,
    key: Option<CancellableCommand>,
}

impl<Msg: Send + 'static> RuntimeCommandParts<Msg> {
    pub(super) fn new(
        directives: RuntimeDirectives,
        stream: Option<BoxStream<'static, Action<Msg>>>,
        cancellation: CommandCancellation,
    ) -> Self {
        Self {
            directives,
            stream,
            cancels: cancellation.cancels,
            key: cancellation.key,
        }
    }

    pub(crate) const fn requests_redraw(&self) -> bool {
        self.directives.requests_redraw()
    }

    #[cfg(test)]
    pub(crate) fn into_stream(self) -> Option<BoxStream<'static, Action<Msg>>> {
        self.stream
    }

    pub(crate) fn into_execution_parts(
        self,
    ) -> (
        Vec<CommandId>,
        Option<CancellableCommand>,
        Option<BoxStream<'static, Action<Msg>>>,
    ) {
        (self.cancels, self.key, self.stream)
    }
}
