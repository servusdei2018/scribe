//! Deterministic incremental build cache for `scribe`.
//!
//! The crate is a leaf dependency: it knows nothing about documents or the
//! build environment. It provides three things the environment layer builds on:
//!
//! * [`ContentDigest`] — a 256-bit BLAKE3 content hash with a stable hex form.
//! * [`CacheStore`] — a typed, versioned, content-verified and atomically
//!   written store for any `Serialize + DeserializeOwned` value.
//! * [`CACHE_FORMAT_VERSION`] — an explicit on-disk format version, so an
//!   incompatible artifact is discarded rather than misread.
//!
//! Corrupt artifacts are always treated as a cold cache: [`CacheStore::load`]
//! returns `Ok(None)` and never panics.

mod digest;
mod error;
mod store;

pub use digest::{hash_bytes, hash_file, hash_str, ContentDigest};
pub use error::CacheError;
pub use store::{CacheStore, CACHE_FORMAT_VERSION, CACHE_MAGIC};

/// The phase range that owns this crate (kept for the phase ledger).
pub const IMPLEMENTATION_PHASE: &str = "Phases 08, 30";
