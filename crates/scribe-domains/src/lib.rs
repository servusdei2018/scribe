//! Domain architecture (`std`, `py`, `c`, `cpp`, `js`, `rst`, `math`).
//!
//! **Frozen cross-phase contract (Phase 11):** the `Domain` trait with
//! `resolve_xref` and `resolve_any_xref` is defined here and shared with the
//! Phase 10 transform engine. Its signature may not change without
//! orchestrator approval.

/// The phase that will implement this crate's functionality.
pub const IMPLEMENTATION_PHASE: &str = "Phases 11-14";
