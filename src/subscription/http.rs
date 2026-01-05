//! HTTP query and mutation support with caching.
//!
//! This module provides subscription-based HTTP queries and command-based mutations,
//! similar to SWR or TanStack Query.
//!
//! # Features
//!
//! - **Queries**: Subscription-based data fetching with automatic caching and refetching
//! - **Mutations**: Command-based data modifications (POST, PUT, DELETE, etc.)
//! - **Cache management**: Automatic cache invalidation and updates
//!
//! # Example
//!
//! ```rust,ignore
//! use tears::prelude::*;
//! use tears::subscription::http::{Query, QueryClient, Mutation};
//! use std::sync::Arc;
//!
//! struct App {
//!     query_client: Arc<QueryClient>,
//!     user_state: QueryState<User>,
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
//!                 self.user_state = result.state;
//!                 Command::none()
//!             }
//!             Message::UpdateUser(data) => {
//!                 Mutation::mutate(data, update_user_api)
//!                     .map(|result| match result {
//!                         Ok(user) => Message::UserUpdated(user),
//!                         Err(e) => Message::UpdateFailed(e.to_string()),
//!                     })
//!             }
//!             Message::UserUpdated(_) => {
//!                 self.query_client.invalidate("user-123")
//!             }
//!         }
//!     }
//! }
//! ```

mod cache;
mod config;
pub mod mutation;
pub mod query;

// Re-export main types
pub use config::QueryConfig;
pub use mutation::{Mutation, MutationResult, MutationState};
pub use query::{Query, QueryClient, QueryError, QueryResult, QueryState};
