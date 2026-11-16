//! Deterministic incremental build cache.
//!
//! Planned for Phases 08 and 30. Uses `blake3` for content digests and
//! `bincode`/`rkyv` for stable serialization of cached doctrees.

/// The phase range that will implement this crate's functionality.
pub const IMPLEMENTATION_PHASE: &str = "Phases 08, 30";
