//! Prelude module for convenient imports.
//!
//! ```
//! use tears::prelude::*;
//! ```
//!
//! # What's included
//!
//! - [`Application`] - The main application trait
//! - [`Command`] - For side effects and runtime directives
//! - [`Subscription`] - For handling event sources
//! - [`Runtime`] - The runtime for running applications
//! - [`FrameRate`] - Validated runtime frame rate

pub use crate::application::Application;
pub use crate::command::Command;
pub use crate::runtime::Runtime;
pub use crate::runtime::frame_rate::FrameRate;
pub use crate::subscription::Subscription;
