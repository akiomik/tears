use super::query::QueryError;

/// The high-level state of a query.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueryStatus {
    Pending,
    Success,
    Error,
}

/// Whether a query currently has an active fetch in progress.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FetchStatus {
    Idle,
    Fetching,
}

/// The current snapshot of a query.
///
/// `is_stale` is a snapshot value computed when the result is emitted. Time
/// passing by itself does not update an already-emitted snapshot.
#[derive(Debug, Clone)]
pub struct QueryResult<T> {
    data: Option<T>,
    error: Option<QueryError>,
    status: QueryStatus,
    fetch_status: FetchStatus,
    is_stale: bool,
}

impl<T> QueryResult<T> {
    pub(crate) const fn pending(fetch_status: FetchStatus) -> Self {
        Self {
            data: None,
            error: None,
            status: QueryStatus::Pending,
            fetch_status,
            is_stale: false,
        }
    }

    pub(crate) const fn success(data: T, is_stale: bool, fetch_status: FetchStatus) -> Self {
        Self {
            data: Some(data),
            error: None,
            status: QueryStatus::Success,
            fetch_status,
            is_stale,
        }
    }

    pub(crate) const fn failed(
        error: QueryError,
        data: Option<T>,
        is_stale: bool,
        fetch_status: FetchStatus,
    ) -> Self {
        Self {
            data,
            error: Some(error),
            status: QueryStatus::Error,
            fetch_status,
            is_stale,
        }
    }

    /// Returns the query data, if this snapshot has data.
    pub const fn data(&self) -> Option<&T> {
        self.data.as_ref()
    }

    /// Returns the query error, if this snapshot is in an error state.
    pub const fn error(&self) -> Option<&QueryError> {
        self.error.as_ref()
    }

    /// Returns the high-level query status.
    pub const fn status(&self) -> QueryStatus {
        self.status
    }

    /// Returns whether a fetch is currently in progress.
    pub const fn fetch_status(&self) -> FetchStatus {
        self.fetch_status
    }

    /// Returns `true` if the query has no successful data yet.
    pub const fn is_loading(&self) -> bool {
        matches!(self.status, QueryStatus::Pending)
    }

    /// Returns `true` if the query currently has successful data.
    pub const fn is_success(&self) -> bool {
        matches!(self.status, QueryStatus::Success)
    }

    /// Returns `true` if the latest fetch failed.
    pub const fn is_error(&self) -> bool {
        matches!(self.status, QueryStatus::Error)
    }

    /// Returns `true` if a fetch is currently in progress.
    pub const fn is_fetching(&self) -> bool {
        matches!(self.fetch_status, FetchStatus::Fetching)
    }

    /// Returns `true` if this snapshot contains stale data.
    pub const fn is_stale(&self) -> bool {
        self.is_stale
    }
}
