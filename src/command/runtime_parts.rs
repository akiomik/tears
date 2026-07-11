use futures::stream::BoxStream;

use super::Action;
use super::runtime_directives::RuntimeDirectives;

/// Internal command decomposition consumed by the runtime.
///
/// Keeps command-owned directives paired with the action stream so runtime code
/// has a single lowering boundary for command execution.
#[must_use = "Runtime command parts may contain side effects and directives that must be handled by the runtime."]
pub struct RuntimeCommandParts<Msg: Send + 'static> {
    directives: RuntimeDirectives,
    stream: Option<BoxStream<'static, Action<Msg>>>,
}

impl<Msg: Send + 'static> RuntimeCommandParts<Msg> {
    pub(super) const fn new(
        directives: RuntimeDirectives,
        stream: Option<BoxStream<'static, Action<Msg>>>,
    ) -> Self {
        Self { directives, stream }
    }

    pub(crate) const fn requests_redraw(&self) -> bool {
        self.directives.requests_redraw()
    }

    pub(crate) fn into_stream(self) -> Option<BoxStream<'static, Action<Msg>>> {
        self.stream
    }
}
