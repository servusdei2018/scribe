//! Errors raised by the deterministic cache layer.

use std::path::PathBuf;
use thiserror::Error;

/// Failures raised while hashing, encoding or accessing the build cache.
///
/// Cache *decoding* failures are deliberately not represented here: they are
/// treated as a cold cache by [`crate::CacheStore::load`] so a stale or corrupt
/// artifact can never abort a build.
#[derive(Debug, Error)]
pub enum CacheError {
    /// A filesystem operation failed.
    #[error("cache I/O error at `{}`: {source}", .path.display())]
    Io {
        /// Path involved in the failure.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// A value could not be encoded for storage.
    #[error("failed to encode cache value: {0}")]
    Encode(String),

    /// A hexadecimal digest string was malformed.
    #[error("invalid content digest `{0}`")]
    InvalidDigest(String),

    /// A cache key contained no usable path components.
    #[error("invalid cache key `{0}`")]
    InvalidKey(String),

    /// A directory could not be prepared for writing.
    #[error("cannot create cache directory `{}`: {source}", .path.display())]
    CreateDir {
        /// Directory that could not be created.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}
