//! The kernel-facing reading of one lowered command.
//!
//! `Command` has a single lowering boundary — `into_runtime_parts()` — and
//! [`RuntimeCommandParts`](super::runtime_parts::RuntimeCommandParts) is the
//! single type it produces (RFC 0008 INV-T3). What differs between consumers
//! is how they *read* that type: the runtime and `TestStore` take the
//! execution 3-tuple, and the kernel takes the buckets below, which name the
//! phases RFC 0014 §3.4 orders — a cancel phase (`cancels`, `teardowns`)
//! that precedes every spawn of the same command, then a spawn phase
//! (`spawns`) in declaration order.

use futures::stream::BoxStream;

use crate::structural_key::ScopePath;

use super::Action;
use super::cancellation::{CancellableCommand, CommandId};

/// One command, read as the kernel's phase buckets.
pub(crate) struct KernelParts<Msg: Send + 'static> {
    /// Whether the command asks for a redraw (RFC 0011 INV-LC1's mark).
    pub(crate) redraw: bool,
    /// Whether the command carried an `update`-returned quit, which applies
    /// synchronously at the completion of this dispatch rather than being
    /// spawned (RFC 0014 §3.3, §3.4).
    pub(crate) quit_now: bool,
    /// Explicit cancel ids, applied in the cancel phase.
    pub(crate) cancels: Vec<CommandId>,
    /// Scope prefixes to tear down, applied in the cancel phase alongside
    /// `cancels`, with which teardown commutes (RFC 0013 §3.3).
    pub(crate) teardowns: Vec<ScopePath>,
    /// Producer runs to start, in the command's flattened declaration order
    /// (RFC 0008 §4.1).
    pub(crate) spawns: Vec<SpawnEntry<Msg>>,
}

/// One producer run the spawn phase starts.
///
/// `key` is what separates the two run kinds the kernel tracks in one
/// registry: `Some` opens a keyed entry whose slot a later same-id spawn may
/// replace under its [`CancelPolicy`](super::cancellation::CancelPolicy),
/// `None` an anonymous run addressed only by its scope (RFC 0014 INV-RC7).
/// `scope` is set either way, so a prefix teardown selects both kinds.
pub(crate) struct SpawnEntry<Msg: Send + 'static> {
    pub(crate) key: Option<CancellableCommand>,
    pub(crate) scope: ScopePath,
    pub(crate) stream: BoxStream<'static, Action<Msg>>,
}
