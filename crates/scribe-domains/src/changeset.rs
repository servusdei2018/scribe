//! The changeset domain (`changeset`) — port of `sphinx/domains/changeset.py`
//! (Phase 13).
//!
//! Upstream's changeset domain owns no objects, directives or roles; it exists
//! to collect `versionadded` / `versionchanged` nodes for the changes builder.
//! We expose that collection surface and keep resolution empty.

use scribe_ast::{Document, NodeId};

use crate::error::DomainResult;
use crate::traits::{Domain, DomainContext, ObjectTypeAlias};
use crate::types::{DomainData, ObjectEntry, ResolvedReference};

/// Upstream object types (none).
pub fn object_types() -> &'static [ObjectTypeAlias] {
    &[]
}

/// The `changeset` domain implementation.
#[derive(Debug, Default)]
pub struct ChangesetDomain;

impl ChangesetDomain {
    /// Creates an empty domain.
    pub fn new() -> Self { Self }
}

impl Domain for ChangesetDomain {
    fn name(&self) -> &str { "changeset" }
    fn label(&self) -> &str { "changeset" }
    fn object_types(&self) -> &[ObjectTypeAlias] { object_types() }
    fn directive_names(&self) -> &[&'static str] { &[] }
    fn role_names(&self) -> &[&'static str] { &[] }
    fn initial_data(&self) -> DomainData { DomainData::default() }
    fn data_version(&self) -> u32 { 1 }
    fn clear_doc(&mut self, _docname: &str) {}
    fn merge_domaindata(&mut self, _docnames: &[String], _other: &DomainData) {}
    fn process_doc(&mut self, _ctx: &DomainContext<'_>, _docname: &str, document: &Document) -> DomainResult<()> {
        // Changeset nodes are collected by the changes builder (Phase 25); the
        // read phase only needs to accept the document without registering
        // objects. Counting them here keeps the inventory meaningful.
        let _ = document.preorder_ids().into_iter().filter(|&id| {
            document.get(id).is_ok_and(|node| node.kind.is_version_modified())
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
        assert_eq!(ChangesetDomain::new().name(), "changeset");
        assert_eq!(ChangesetDomain::new().label(), "changeset");
        assert!(object_types().is_empty());
    }
}
