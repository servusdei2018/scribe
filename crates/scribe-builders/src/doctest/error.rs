//! Errors raised by the doctest builder (Phase 22).

use std::path::PathBuf;

use miette::Diagnostic;
use thiserror::Error;

/// A failure in the doctest builder.
#[derive(Debug, Error, Diagnostic)]
pub enum DoctestError {
    /// An I/O operation on a build path failed.
    #[error("I/O error at {}: {source}", .path.display())]
    #[diagnostic(code(scribe::builders::doctest::io), severity(Error))]
    Io {
        /// Path involved.
        path: PathBuf,
        /// OS detail.
        #[source]
        source: std::io::Error,
    },

    /// A build-environment operation failed.
    #[error(transparent)]
    #[diagnostic(code(scribe::builders::doctest::environment), severity(Error))]
    Environment(#[from] scribe_core::ScribeError),

    /// The requested source directory does not exist.
    #[error("missing source file: {}", .0.display())]
    #[diagnostic(code(scribe::builders::doctest::missing_source), severity(Error))]
    MissingSource(PathBuf),

    /// A configuration value had an unexpected type.
    #[error("{0}")]
    #[diagnostic(code(scribe::builders::doctest::invalid), severity(Error))]
    Invalid(String),
}

impl DoctestError {
    /// Construct an I/O error carrying the path.
    pub fn io(path: &std::path::Path, source: std::io::Error) -> Self {
        Self::Io { path: path.to_path_buf(), source }
    }
}

/// Result alias for the doctest builder.
pub type Result<T> = std::result::Result<T, DoctestError>;
