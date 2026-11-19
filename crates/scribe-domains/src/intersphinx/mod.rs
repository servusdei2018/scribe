//! Intersphinx support: inventory I/O, inventory loading and xref resolution.
//!
//! The layout mirrors upstream's `sphinx.util.inventory` and
//! `sphinx.ext.intersphinx` (`_load` / `_resolve` / `_shared`), but every
//! operation is panic-free and degrades to a warning on malformed input.
//!
//! * [`inventory`] — byte-compatible `objects.inv` v2 reader/writer.
//! * [`cache`] — BLAKE3-keyed on-disk cache with deterministic expiry.
//! * [`load`] — parallel fetching of `intersphinx_mapping` entries with
//!   timeout, retry and support for local paths / `file://` URIs.
//! * [`resolve`] — resolution of missing local xrefs against loaded
//!   inventories, including `:external:` and `:any:` selection.

pub mod cache;
pub mod inventory;
pub mod load;
pub mod resolve;

use thiserror::Error;

/// Failures raised by the intersphinx subsystem.
///
/// These are deliberately non-fatal: a caller converts each variant into a
/// structured warning so a broken inventory never aborts a build, matching
/// upstream's `failed to reach any of the inventories` behavior.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum IntersphinxError {
    /// The inventory header was missing or malformed.
    #[error("invalid inventory header: {0}")]
    InvalidHeader(String),
    /// The inventory announced an unsupported version.
    #[error("unknown or unsupported inventory version: {0}")]
    UnsupportedVersion(String),
    /// The compressed body could not be decoded.
    #[error("inventory decompression failed: {0}")]
    Decompress(String),
    /// A single inventory line was syntactically invalid.
    #[error("invalid inventory entry: {0}")]
    InvalidEntry(String),
    /// An I/O operation on a local inventory or cache file failed.
    #[error("I/O error for `{path}`: {detail}")]
    Io {
        /// Offending path or URI.
        path: String,
        /// Explanation.
        detail: String,
    },
    /// A remote inventory could not be fetched.
    #[error("failed to fetch `{url}`: {detail}")]
    Fetch {
        /// Offending URL.
        url: String,
        /// Explanation.
        detail: String,
    },
    /// `intersphinx_mapping` was structurally invalid.
    #[error("invalid intersphinx mapping: {0}")]
    InvalidMapping(String),
}

/// Result alias for the intersphinx subsystem.
pub type IntersphinxResult<T> = Result<T, IntersphinxError>;

pub use cache::{CacheEntry, InventoryCache};
pub use inventory::{DumpConfig, Inventory, InventoryItem, InventoryObject};
pub use load::{
    fetch_all, fetch_all_async, fetch_all_with_runtime, fetch_one, FetchOptions, FetchOutcome,
    InventoryLocation, IntersphinxMapping, MappingEntry,
};
pub use resolve::{ExternalRole, IntersphinxReference, IntersphinxResolver};
