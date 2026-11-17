//! Structured errors for arena access and tree mutation.
//!
//! The AST crate is intentionally dependency-light and owns its own error type
//! rather than depending on `scribe-core`. Every fallible arena operation
//! returns [`AstResult`] so that malformed or stale [`NodeId`](crate::NodeId)
//! values can never cause a panic in library code.

use thiserror::Error;

/// Result alias used by all fallible `scribe-ast` operations.
pub type AstResult<T> = Result<T, AstError>;

/// Errors produced by the document arena, visitors and mutators.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AstError {
    /// A [`NodeId`](crate::NodeId) pointed outside the arena.
    #[error("invalid node id {id} (arena length {len})")]
    InvalidNodeId {
        /// Raw numeric value of the out-of-range id.
        id: u32,
        /// Number of slots currently in the arena.
        len: u32,
    },

    /// The sentinel [`NodeId::NULL`](crate::NodeId::NULL) was used where a real
    /// node was required.
    #[error("null node id used where a real node is required")]
    NullNodeId,

    /// A node was detached (removed or was never linked) and cannot be used
    /// for an operation that requires a live tree position.
    #[error("node {id} is not live in the document tree")]
    NotLive {
        /// The offending node id.
        id: u32,
    },

    /// A structural mutation would have created a cycle or moved the root.
    #[error("invalid tree mutation: {0}")]
    InvalidMutation(String),

    /// An attribute was read with the wrong [`AttributeValue`](crate::AttributeValue)
    /// type.
    #[error("attribute {key:?} has type {found}, expected {expected}")]
    AttributeType {
        /// Attribute name.
        key: String,
        /// Expected type name.
        expected: &'static str,
        /// Actual type name found.
        found: &'static str,
    },

    /// JSON (de)serialization failed.
    #[error("serialization error: {0}")]
    Serialization(String),
}

impl From<serde_json::Error> for AstError {
    fn from(err: serde_json::Error) -> Self {
        AstError::Serialization(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::AstError;

    #[test]
    fn messages_are_stable() {
        let err = AstError::InvalidNodeId { id: 9, len: 3 };
        assert_eq!(err.to_string(), "invalid node id 9 (arena length 3)");
    }
}
