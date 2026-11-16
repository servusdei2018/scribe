//! Unified document AST and doctree model.
//!
//! **Frozen cross-phase contract (Phase 03):** this crate will define
//! `Document`, the node enum, a `u32` arena-backed `NodeId`, and the
//! `pending_xref` node consumed by the transform pipeline (Phase 10) and the
//! domain resolver (Phase 11). Their shapes may not change without
//! orchestrator approval.
//!
//! Phase 01 provides only this documented placeholder.

/// The phase that will implement this crate's functionality.
pub const IMPLEMENTATION_PHASE: &str = "Phase 03";
