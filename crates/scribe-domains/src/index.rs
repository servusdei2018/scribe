//! The index domain (`index`) — port of `sphinx/domains/index.py` (Phase 13).
//!
//! Upstream's index domain owns no objects, directives or roles; it provides
//! the `genindex` / `modindex` entry points consumed by writers.  We expose a
//! deterministic, empty inventory surface here.

use scribe_ast::{Document, NodeId};

use crate::error::DomainResult;
use crate::traits::{Domain, DomainContext, ObjectTypeAlias};
use crate::types::{DomainData, ObjectEntry, ResolvedReference};

/// Upstream object types (none).
pub fn object_types() -> &'static [ObjectTypeAlias] {
    &[]
}

/// The `index` domain implementation.
#[derive(Debug, Default)]
pub struct IndexDomain;

impl IndexDomain {
    /// Creates an empty domain.
    pub fn new() -> Self { Self }
}

impl Domain for IndexDomain {
    fn name(&self) -> &str { "index" }
    fn label(&self) -> &str { "index" }
    fn object_types(&self) -> &[ObjectTypeAlias] { object_types() }
    fn directive_names(&self) -> &[&'static str] { &[] }
    fn role_names(&self) -> &[&'static str] { &[] }
    fn initial_data(&self) -> DomainData { DomainData::default() }
    fn data_version(&self) -> u32 { 1 }
    fn clear_doc(&mut self, _docname: &str) {}
    fn merge_domaindata(&mut self, _docnames: &[String], _other: &DomainData) {}
    fn process_doc(&mut self, _ctx: &DomainContext<'_>, _docname: &str, _document: &Document) -> DomainResult<()> { Ok(()) }
    fn resolve_xref(
        &self, _ctx: &DomainContext<'_>, _typ: &str, _target: &str,
        _node_id: NodeId, _contnode_id: NodeId, _document: &Document,
    ) -> DomainResult<Option<ResolvedReference>> { Ok(None) }
    fn resolve_any_xref(
        &self, _ctx: &DomainContext<'_>, _target: &str,
        _node_id: NodeId, _contnode_id: NodeId, _document: &Document,
    ) -> DomainResult<Vec<(String, ResolvedReference)>> { Ok(Vec::new()) }
    fn get_objects(&self) -> Vec<ObjectEntry> { Vec::new() }
    fn export_data(&self) -> DomainResult<DomainData> { DomainData::from_payload(self.data_version(), &serde_json::json!({})) }
    fn replace_data(&mut self, _data: &DomainData) -> DomainResult<()> { Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_matches_upstream() {
        assert_eq!(IndexDomain::new().name(), "index");
        assert_eq!(IndexDomain::new().label(), "index");
        assert!(object_types().is_empty());
    }
}
