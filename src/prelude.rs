//! Prelude module for convenient imports.
//!
//! ```
//! use tears::prelude::*;
//! ```
//!
//! # What's included
//!
//! - [`Application`] - The main application trait
//! - [`Command`] - For performing side effects
//! - [`Action`] - Actions that commands can perform
//! - [`Subscription`] - For handling event sources
//! - [`Runtime`] - The application runtime

pub use crate::application::Application;
pub use crate::command::{Action, Command};
pub use crate::runtime::Runtime;
pub use crate::subscription::Subscription;
