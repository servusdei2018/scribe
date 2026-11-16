//! Shared core types for `scribe`.
//!
//! This crate owns the vocabulary that is stable across phases: the crate-wide
//! result alias and the base error type. Phase 08 will add `BuildEnvironment`
//! here (its public API is a frozen cross-phase contract); Phase 03 will add the
//! document AST in [`scribe_ast`](https://docs.rs/scribe-ast).

use thiserror::Error;

/// The result alias used throughout the `scribe` library crates.
pub type ScribeResult<T> = Result<T, ScribeError>;

/// Base error type for library-level failures.
///
/// User-facing diagnostics are rendered with `miette` at the CLI boundary;
/// library code reports structured errors with this `thiserror` enum.
#[derive(Debug, Error)]
pub enum ScribeError {
    /// An I/O operation failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A configuration value was rejected.
    #[error("invalid configuration: {message}")]
    InvalidConfiguration {
        /// Human-readable explanation of the rejection.
        message: String,
    },

    /// A requested operation has not been implemented by the current phase.
    #[error("not implemented: {feature}")]
    NotImplemented {
        /// Name of the unimplemented feature.
        feature: &'static str,
    },
}

#[cfg(test)]
mod tests {
    use super::ScribeError;

    #[test]
    fn error_messages_are_stable() {
        let err = ScribeError::NotImplemented { feature: "builder" };
        assert_eq!(err.to_string(), "not implemented: builder");
    }
}
