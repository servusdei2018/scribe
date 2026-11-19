//! Error type for the domain layer.
//!
//! Domains are best-effort: a malformed xref must never abort a build.  The
//! error type therefore only reports *internal* failures (an invalid arena id,
//! an unserializable side table, ...).  User-facing problems are reported as
//! [`DomainDiagnostic`](crate::diagnostics::DomainDiagnostic)s through the
//! diagnostic sink.

use thiserror::Error;

use crate::types::ObjectEntry;

/// Failures raised by domain registration or resolution.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum DomainError {
    /// A node id was absent from the document or not live.
    #[error("domain: invalid node id {id}")]
    InvalidNode {
        /// Offending arena id.
        id: u32,
    },
    /// A domain with this name is not registered.
    #[error("domain: unknown domain `{name}`")]
    UnknownDomain {
        /// Domain name.
        name: String,
    },
    /// A domain data snapshot could not be decoded.
    #[error("domain: invalid data for `{domain}`: {detail}")]
    InvalidData {
        /// Domain name.
        domain: String,
        /// Explanation.
        detail: String,
    },
    /// A resolution result was structurally inconsistent.
    #[error("domain: malformed resolved reference for `{target}`: {detail}")]
    MalformedReference {
        /// Reference target.
        target: String,
        /// Explanation.
        detail: String,
    },
    /// An object table contained a duplicate conflicting entry.
    #[error("domain: conflicting object entry `{entry:?}`")]
    ConflictingObject {
        /// The conflicting entry.
        entry: Box<ObjectEntry>,
    },
}

/// Result alias used throughout the domain layer.
pub type DomainResult<T> = Result<T, DomainError>;

impl From<scribe_ast::AstError> for DomainError {
    fn from(error: scribe_ast::AstError) -> Self {
        Self::InvalidData {
            domain: String::new(),
            detail: error.to_string(),
        }
    }
}
