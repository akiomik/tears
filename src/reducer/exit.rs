//! The controlled-exit reason, in its own module so the crate root can be
//! its one public path.

/// How a run of a [`Program`](super::Program) ended, when it ended in the
/// controlled way.
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
