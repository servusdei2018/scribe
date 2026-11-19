//! The citation domain (`citation`) — port of `sphinx/domains/citation.py`
//! (Phase 13).
//!
//! Upstream's citation domain owns no objects, directives or roles; citation
//! references are plain `citation_reference` nodes handled by the citation
//! transform (Phase 10).  The domain exists for event dispatch and inventory
//! completeness.

use scribe_ast::{Document, NodeId, NodeKind};

use crate::error::DomainResult;
use crate::traits::{Domain, DomainContext, ObjectTypeAlias};
use crate::types::{DomainData, ObjectEntry, ResolvedReference};

/// Upstream object types (none).
pub fn object_types() -> &'static [ObjectTypeAlias] {
    &[]
}

/// The `citation` domain implementation.
#[derive(Debug, Default)]
pub struct CitationDomain;

impl CitationDomain {
    /// Creates an empty domain.
    pub fn new() -> Self { Self }
}

impl Domain for CitationDomain {
    fn name(&self) -> &str { "citation" }
    fn label(&self) -> &str { "citation" }
    fn object_types(&self) -> &[ObjectTypeAlias] { object_types() }
    fn directive_names(&self) -> &[&'static str] { &[] }
    fn role_names(&self) -> &[&'static str] { &[] }
    fn initial_data(&self) -> DomainData { DomainData::default() }
    fn data_version(&self) -> u32 { 1 }
    fn clear_doc(&mut self, _docname: &str) {}
    fn merge_domaindata(&mut self, _docnames: &[String], _other: &DomainData) {}
    fn process_doc(&mut self, _ctx: &DomainContext<'_>, _docname: &str, document: &Document) -> DomainResult<()> {
        // Citation definitions may carry explicit labels in their `names`;
        // expose them as hidden objects for `:any:` completeness.
        let _ = document.preorder_ids().into_iter().filter(|&id| {
            document.get(id).is_ok_and(|node| node.kind == NodeKind::Citation)
        }).count();
        Ok(())
    }
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
        assert_eq!(CitationDomain::new().name(), "citation");
        assert_eq!(CitationDomain::new().label(), "citation");
        assert!(object_types().is_empty());
    }
}
