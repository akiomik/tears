//! Commands for performing asynchronous side effects and returning runtime
//! directives.
//!
//! Commands represent asynchronous operations that produce messages or actions,
//! plus attributes that tell the runtime how to handle the update that returned
//! them. They are the primary way to perform side effects in the Elm
//! Architecture, such as HTTP requests, file I/O, or any other async operation.
//!
//! # Examples
//!
//! ```
//! use tears::prelude::*;
//!
//! enum Message {
//!     DataLoaded(String),
//! }
//!
//! async fn load_data() -> String {
//!     // Perform async operation
//!     "data".to_string()
//! }
//!
//! // Create a command that performs an async operation
//! let cmd = Command::perform(load_data(), Message::DataLoaded);
//! ```

mod cancellation;
mod cleanup;
pub(crate) mod core;
mod effect;
mod kernel_parts;
mod retry;
mod runtime_directives;
mod runtime_parts;

pub(crate) use cancellation::CancellableCommand;
pub use cancellation::{CancelPolicy, CommandId};
pub(crate) use cleanup::CleanupRegistration;
pub(crate) use core::Command;
pub(crate) use kernel_parts::{KernelParts, SpawnEntry};
pub use retry::{RetryBackoff, RetryContext, RetryError, RetryPolicy, RetryStopReason};
pub(crate) use runtime_parts::{RuntimeCommandParts, fold_leaves};

/// An internal runtime directive produced by a command's effect stream.
///
/// This type is not part of the public API. Use
/// [`Command::message`](crate::Command::message) and
/// [`Command::quit`](crate::Command::quit) to construct the corresponding
/// commands.
pub(crate) enum Action<Msg> {
    /// Send a message to the application's update function.
    Message(Msg),

    /// Request the application to quit.
    Quit,
}
