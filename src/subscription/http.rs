//! HTTP query and mutation support with retained data.
//!
//! This module provides subscription-based HTTP queries and command-based mutations,
//! similar to SWR or TanStack Query.
//!
//! # Capabilities
//!
//! - **Queries**: Subscription-based data fetching with automatic retention and refetching
//! - **Mutations**: Command-based data modifications (POST, PUT, DELETE, etc.)
//! - **Retention management**: Automatic retained-data invalidation and updates
//!
//! # Feature Flag
//!
//! The types in this module need the `http` feature:
//!
//! ```toml
//! [dependencies]
//! tears = { version = "0.11", features = ["http"] }
//! ```
//!
//! # Example
//!
// Not compiled: the `impl` is partial and the types are placeholders. #385
// tracks turning this and its siblings into real doctests, which for this one
// means a complete `Application`.
//! ```rust,ignore
//! use tears::prelude::*;
//! use tears::subscription::http::{Mutation, Query, QueryClient, QueryResult};
//! use std::sync::Arc;
//!
//! struct App {
//!     query_client: Arc<QueryClient>,
//!     user_result: Option<QueryResult<User>>,
//! }
//!
//! impl Application for App {
//!     fn subscriptions(&self) -> Vec<Subscription<Message>> {
//!         vec![
//!             Subscription::new(Query::new(
//!                 "user-123",
//!                 || Box::pin(fetch_user()),
//!                 self.query_client.clone(),
//!             ))
//!             .map(Message::UserQuery)
//!         ]
//!     }
//!
//!     fn update(&mut self, msg: Message) -> Command<Message> {
//!         match msg {
//!             Message::UserQuery(result) => {
//!                 self.user_result = Some(result);
//!                 Command::none()
//!             }
//!             Message::UpdateUser(data) => {
//!                 Mutation::mutate(data, |input| {
//!                     Box::pin(async move { update_user_api(input).await })
//!                 })
//!                 .map(|result| match result {
//!                     Ok(user) => Message::UserUpdated(user),
//!                     Err(e) => Message::UpdateFailed(e.to_string()),
//!                 })
//!                 .into()
//!             }
//!             Message::UserUpdated(_) => {
//!                 self.query_client.invalidate("user-123");
//!                 Command::none()
//!             }
//!         }
//!     }
//! }
//! ```

#[cfg(feature = "http")]
mod cell;
// `loom-core` is a test-only feature (its `CellCore` mirror is exercised solely
// by the `#[cfg(test)]` loom model checks), so gate the module on `test` too:
// a non-test `--all-features` build would otherwise compile it with no callers.
#[cfg(all(feature = "loom-core", test))]
mod cell_core;
#[cfg(feature = "http")]
mod config;
#[cfg(feature = "http")]
mod key;
#[cfg(feature = "http")]
mod mutation;
#[cfg(feature = "http")]
mod query;
#[cfg(feature = "http")]
mod reconcile;
#[cfg(feature = "http")]
mod result;

// Re-export main types
#[cfg(feature = "http")]
pub use config::QueryConfig;
#[cfg(feature = "http")]
pub use key::{QueryKey, QueryKeyPart};
#[cfg(feature = "http")]
pub use mutation::{Mutation, MutationResult, MutationState};
#[cfg(feature = "http")]
pub use query::{Query, QueryClient, QueryError};
#[cfg(feature = "http")]
pub use result::{FetchStatus, QueryResult, QueryStatus};
