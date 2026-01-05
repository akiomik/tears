//! HTTP mutation operations for creating, updating, or deleting resources.
//!
//! This module provides the [`Mutation`] type for managing HTTP POST, PUT, PATCH,
//! and DELETE requests, similar to mutations in TanStack Query.
//!
//! # Design Pattern: Transaction-based Operations
//!
//! HTTP mutations use the **transaction-based** pattern. Each mutation operation
//! returns a `Command` because HTTP requests are discrete side effects with clear
//! start and end points. This design maintains TEA principles and makes operations
//! testable and composable.
//!
//! Unlike queries which are subscriptions, mutations are one-off operations that
//! return a Command. After a successful mutation, you typically want to invalidate
//! related queries to trigger refetching.
//!
//! # Example
//!
//! ```rust,ignore
//! use tears::prelude::*;
//! use tears::subscription::http::{Mutation, QueryError};
//!
//! enum Message {
//!     UpdateUser(UserData),
//!     UserUpdated(User),
//!     UpdateFailed(String),
//! }
//!
//! fn update(&mut self, msg: Message) -> Command<Message> {
//!     match msg {
//!         Message::UpdateUser(data) => {
//!             Mutation::mutate(
//!                 data,
//!                 |input| Box::pin(async move {
//!                     update_user_api(input).await
//!                 }),
//!             )
//!             .map(|result| match result {
//!                 Ok(user) => Message::UserUpdated(user),
//!                 Err(e) => Message::UpdateFailed(e.to_string()),
//!             })
//!         }
//!         Message::UserUpdated(_) => {
//!             // Invalidate user query to refetch
//!             self.query_client.invalidate("user-123")
//!         }
//!         Message::UpdateFailed(_) => {
//!             // Handle error
//!             Command::none()
//!         }
//!     }
//! }
//! ```

use std::marker::PhantomData;

use futures::future::BoxFuture;

use crate::Command;

use super::query::QueryError;

/// The state of a mutation result.
#[derive(Debug, Clone)]
pub enum MutationState<T> {
    /// Mutation is idle (not yet started).
    Idle,
    /// Mutation is in progress.
    Loading,
    /// Mutation succeeded with a result.
    Success(T),
    /// Mutation failed with an error.
    Error(String),
}

/// A mutation result containing the current state.
#[derive(Debug, Clone)]
pub struct MutationResult<T> {
    /// The current state of the mutation.
    pub state: MutationState<T>,
}

impl<T> MutationResult<T> {
    /// Returns the result data if the mutation succeeded, otherwise `None`.
    pub const fn data(&self) -> Option<&T> {
        match &self.state {
            MutationState::Success(data) => Some(data),
            _ => None,
        }
    }

    /// Returns `true` if the mutation is currently loading.
    pub const fn is_loading(&self) -> bool {
        matches!(self.state, MutationState::Loading)
    }

    /// Returns `true` if the mutation succeeded.
    pub const fn is_success(&self) -> bool {
        matches!(self.state, MutationState::Success(_))
    }

    /// Returns `true` if the mutation failed.
    pub const fn is_error(&self) -> bool {
        matches!(self.state, MutationState::Error(_))
    }
}

/// A mutation for performing data modifications (POST, PUT, PATCH, DELETE).
///
/// Mutations are one-off operations that return a `Command`. Unlike queries,
/// they don't maintain state or cache results.
///
/// # Example
///
/// ```rust,ignore
/// let cmd = Mutation::mutate(
///     user_data,
///     |input| Box::pin(async move {
///         http_client.put("/api/user").json(&input).send().await
///     }),
///     Message::UserUpdated,
/// );
/// ```
pub struct Mutation<I, O> {
    _phantom: PhantomData<(I, O)>,
}

impl<I, O> Mutation<I, O>
where
    I: Send + 'static,
    O: Send + 'static,
{
    /// Executes a mutation and returns a `Command`.
    ///
    /// The returned command produces `Result<O, QueryError>` which can be mapped
    /// to your application's message type using [`Command::map`].
    ///
    /// # Arguments
    ///
    /// * `input` - The input data for the mutation
    /// * `mutator` - An async function that performs the mutation
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use tears::prelude::*;
    /// use tears::subscription::http::{Mutation, QueryError};
    ///
    /// enum Message {
    ///     UserUpdated(User),
    ///     UpdateFailed(String),
    /// }
    ///
    /// fn update_user(data: UserData) -> Command<Message> {
    ///     Mutation::mutate(
    ///         data,
    ///         |input| Box::pin(async move {
    ///             // Perform HTTP request
    ///             update_user_api(input).await
    ///         }),
    ///     )
    ///     .map(|result| match result {
    ///         Ok(user) => Message::UserUpdated(user),
    ///         Err(e) => Message::UpdateFailed(e.to_string()),
    ///     })
    /// }
    /// ```
    pub fn mutate<F>(input: I, mutator: F) -> Command<Result<O, QueryError>>
    where
        F: FnOnce(I) -> BoxFuture<'static, Result<O, QueryError>> + Send + 'static,
    {
        Command::future(async move { mutator(input).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mutation_result_data() {
        let result = MutationResult {
            state: MutationState::Success(42),
        };
        assert_eq!(result.data(), Some(&42));

        let result: MutationResult<i32> = MutationResult {
            state: MutationState::Idle,
        };
        assert_eq!(result.data(), None);

        let result: MutationResult<i32> = MutationResult {
            state: MutationState::Loading,
        };
        assert_eq!(result.data(), None);

        let result: MutationResult<i32> = MutationResult {
            state: MutationState::Error("error".to_string()),
        };
        assert_eq!(result.data(), None);
    }

    #[test]
    fn test_mutation_result_predicates() {
        let idle: MutationResult<i32> = MutationResult {
            state: MutationState::Idle,
        };
        assert!(!idle.is_loading());
        assert!(!idle.is_success());
        assert!(!idle.is_error());

        let loading: MutationResult<i32> = MutationResult {
            state: MutationState::Loading,
        };
        assert!(loading.is_loading());
        assert!(!loading.is_success());
        assert!(!loading.is_error());

        let success = MutationResult {
            state: MutationState::Success(42),
        };
        assert!(!success.is_loading());
        assert!(success.is_success());
        assert!(!success.is_error());

        let error: MutationResult<i32> = MutationResult {
            state: MutationState::Error("error".to_string()),
        };
        assert!(!error.is_loading());
        assert!(!error.is_success());
        assert!(error.is_error());
    }
}
