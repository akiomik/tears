use std::fmt;
use std::sync::Arc;

/// A structured key identifying an HTTP query.
///
/// Query keys are compared structurally. Include every request parameter used
/// by the fetcher in the key; changing the effective request requires changing
/// the key.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct QueryKey(Arc<[QueryKeyPart]>);

/// One component of a [`QueryKey`].
#[non_exhaustive]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum QueryKeyPart {
    /// A string component.
    Str(String),
    /// A signed 64-bit integer component.
    I64(i64),
    /// An unsigned 64-bit integer component.
    U64(u64),
    /// A boolean component.
    Bool(bool),
}

impl QueryKey {
    /// Creates a key from pre-built parts.
    #[must_use]
    pub fn from_parts(parts: impl IntoIterator<Item = QueryKeyPart>) -> Self {
        Self(parts.into_iter().collect::<Vec<_>>().into())
    }
}

impl From<&str> for QueryKey {
    fn from(value: &str) -> Self {
        Self::from_parts([QueryKeyPart::Str(value.to_string())])
    }
}

impl From<String> for QueryKey {
    fn from(value: String) -> Self {
        Self::from_parts([QueryKeyPart::Str(value)])
    }
}

impl From<&String> for QueryKey {
    fn from(value: &String) -> Self {
        Self::from(value.as_str())
    }
}

impl<const N: usize> From<[QueryKeyPart; N]> for QueryKey {
    fn from(value: [QueryKeyPart; N]) -> Self {
        Self::from_parts(value)
    }
}

impl<A> From<(A,)> for QueryKey
where
    A: Into<QueryKeyPart>,
{
    fn from(value: (A,)) -> Self {
        Self::from_parts([value.0.into()])
    }
}

impl<A, B> From<(A, B)> for QueryKey
where
    A: Into<QueryKeyPart>,
    B: Into<QueryKeyPart>,
{
    fn from(value: (A, B)) -> Self {
        Self::from_parts([value.0.into(), value.1.into()])
    }
}

impl<A, B, C> From<(A, B, C)> for QueryKey
where
    A: Into<QueryKeyPart>,
    B: Into<QueryKeyPart>,
    C: Into<QueryKeyPart>,
{
    fn from(value: (A, B, C)) -> Self {
        Self::from_parts([value.0.into(), value.1.into(), value.2.into()])
    }
}

impl From<&str> for QueryKeyPart {
    fn from(value: &str) -> Self {
        Self::Str(value.to_string())
    }
}

impl From<String> for QueryKeyPart {
    fn from(value: String) -> Self {
        Self::Str(value)
    }
}

impl From<&String> for QueryKeyPart {
    fn from(value: &String) -> Self {
        Self::Str(value.clone())
    }
}

impl From<i64> for QueryKeyPart {
    fn from(value: i64) -> Self {
        Self::I64(value)
    }
}

impl From<i32> for QueryKeyPart {
    fn from(value: i32) -> Self {
        Self::I64(i64::from(value))
    }
}

impl From<u64> for QueryKeyPart {
    fn from(value: u64) -> Self {
        Self::U64(value)
    }
}

impl From<u32> for QueryKeyPart {
    fn from(value: u32) -> Self {
        Self::U64(u64::from(value))
    }
}

impl From<bool> for QueryKeyPart {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl fmt::Display for QueryKeyPart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Str(value) => f.write_str(value),
            Self::I64(value) => write!(f, "{value}"),
            Self::U64(value) => write!(f, "{value}"),
            Self::Bool(value) => write!(f, "{value}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_keys_are_structural() {
        assert_eq!(QueryKey::from("todos"), QueryKey::from("todos".to_string()));
        assert_ne!(QueryKey::from("todos"), QueryKey::from("users"));
    }

    #[test]
    fn tuple_keys_keep_parts_distinct() {
        let key = QueryKey::from(("todos", 42_u64, false));
        assert_eq!(
            key,
            QueryKey::from_parts([
                QueryKeyPart::Str("todos".to_string()),
                QueryKeyPart::U64(42),
                QueryKeyPart::Bool(false),
            ])
        );
    }

    #[test]
    fn structural_parts_do_not_collapse() {
        let one_part = QueryKey::from("a/b");
        let two_parts = QueryKey::from(("a", "b"));

        assert_ne!(one_part, two_parts);
    }
}
