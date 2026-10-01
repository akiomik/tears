//! HTTP query and mutation support with retained data.
//!
//! This module provides subscription-based HTTP queries and one-off mutations,
//! similar to SWR or TanStack Query.
//!
//! # Capabilities
//!
//! - **Queries**: Subscription-based data fetching with automatic retention and refetching
//! - **Mutations**: One-off data modifications (POST, PUT, DELETE, etc.)
//! - **Retention management**: Automatic retained-data invalidation and updates
//!
//! # Feature Flag
//!
//! This module is only available when the `http` feature is enabled:
//!
//! ```toml
//! [dependencies]
//! tears = { version = "0.11", features = ["http"] }
//! ```
//!
//! # Example
//!
//! ```rust
//! # use ratatui::Frame;
//! # use tears::subscription::http::QueryError;
//! # #[derive(Clone)]
//! # struct User;
//! # struct UserData;
//! # async fn fetch_user() -> Result<User, QueryError> { Ok(User) }
//! # async fn update_user_api(_input: UserData) -> Result<User, QueryError> { Ok(User) }
//! # enum Message {
//! #     UserQuery(QueryResult<User>),
//! #     UpdateUser(UserData),
//! #     UserUpdated(User),
//! #     UpdateFailed(String),
//! # }
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
//! #     type Message = Message;
//! #     type Flags = ();
//! #     fn new((): ()) -> (Self, Command<Message>) {
//! #         let app = App {
//! #             query_client: Arc::new(QueryClient::new()),
//! #             user_result: None,
//! #         };
//! #         (app, Command::none())
//! #     }
//! #     fn view(&self, _frame: &mut Frame<'_>) {}
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
//!             Message::UpdateFailed(_) => {
//!                 // Handle error
//!                 Command::none()
//!             }
//!         }
//!     }
//! }
//! ```

mod cell;
mod config;
mod key;
mod mutation;
mod query;
mod reconcile;
mod result;

// Re-export main types
pub use config::QueryConfig;
pub use key::{QueryKey, QueryKeyPart};
pub use mutation::{Mutation, MutationResult, MutationState};
pub use query::{Query, QueryClient, QueryError};
pub use result::{FetchStatus, QueryResult, QueryStatus};
