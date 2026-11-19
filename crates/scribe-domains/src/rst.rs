//! The reStructuredText domain (`rst`) — port of `sphinx/domains/rst.py`
//! (Phase 13).
//!
//! Owns the `directive`, `directive:option` and `role` object types with the
//! `dir` / `role` roles and the matching description directives.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use scribe_ast::{Document, NodeId, NodeKind, Span};

use crate::diagnostics::DomainDiagnostic;
use crate::error::DomainResult;
use crate::traits::{Domain, DomainContext, ObjectTypeAlias};
use crate::types::{DomainData, ObjectEntry, ObjectType, ResolvedReference};

static OBJECT_TYPES: OnceLock<Vec<ObjectTypeAlias>> = OnceLock::new();

/// Upstream `ReSTDomain.object_types`.
pub fn object_types() -> &'static [ObjectTypeAlias] {
    OBJECT_TYPES.get_or_init(|| {
        vec![
            ObjectType::new("directive", "directive", &["dir"]),
            ObjectType::new("directive:option", "directive-option", &["dir"]),
            ObjectType::new("role", "role", &["role"]),
        ]
    })
}

/// Upstream directive names.
pub const DIRECTIVES: &[&str] = &["directive", "directive:option", "role"];
/// Upstream roles.
pub const ROLES: &[&str] = &["dir", "role"];

/// Object key: `(objtype, name)` flattened with a unit separator.
fn object_key(objtype: &str, name: &str) -> String {
    format!("{objtype}\u{1f}{name}")
}

/// Serializable rst-domain inventory.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReSTDomainData {
    /// `(objtype, name)` → entry.
    pub objects: std::collections::BTreeMap<String, ReSTObjectEntry>,
}

/// One rst-domain object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReSTObjectEntry {
    /// Document holding the declaration.
    pub docname: String,
    /// Anchor id.
    pub node_id: String,
    /// Object type.
    pub objtype: String,
}

/// The `rst` domain implementation.
pub struct ReSTDomain {
    data: parking_lot::RwLock<ReSTDomainData>,
}

impl Default for ReSTDomain {
    fn default() -> Self { Self::new() }
}

impl std::fmt::Debug for ReSTDomain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReSTDomain").field("objects", &self.data.read().objects.len()).finish_non_exhaustive()
    }
}

impl ReSTDomain {
    /// Creates an empty domain.
    pub fn new() -> Self {
        Self { data: parking_lot::RwLock::new(ReSTDomainData::default()) }
    }
    /// Creates a domain from a persisted inventory.
    pub fn from_data(data: ReSTDomainData) -> Self {
        Self { data: parking_lot::RwLock::new(data) }
    }
    /// Consistent inventory snapshot.
    pub fn snapshot(&self) -> ReSTDomainData {
        self.data.read().clone()
    }
    /// Registers an object with upstream's duplicate warning.
    pub fn note_object(&self, ctx: &DomainContext<'_>, objtype: &str, name: &str, node_id: &str, span: Option<Span>) {
        let mut data = self.data.write();
        if let Some(other) = data.objects.get(&object_key(objtype, name)) {
            let message = format!("duplicate description of {objtype} {name}, other instance in {}", other.docname);
            ctx.push_diagnostic(DomainDiagnostic::duplicate_object(message, objtype, name, Some(ctx.docname.to_owned()), span));
        }
        data.objects.insert(object_key(objtype, name), ReSTObjectEntry {
            docname: ctx.docname.to_owned(), node_id: node_id.to_owned(), objtype: objtype.to_owned(),
        });
    }
    /// Finds an object by role and name.
    pub fn find_obj(&self, typ: &str, target: &str) -> Option<(String, ReSTObjectEntry)> {
        let objtypes: Vec<&str> = object_types().iter()
            .filter(|object| object.roles.iter().any(|role| role == typ))
            .map(|object| object.name.as_str())
            .collect();
        let data = self.data.read();
        for objtype in objtypes {
            let key = object_key(objtype, target);
            if let Some(entry) = data.objects.get(&key) {
                return Some((target.to_owned(), entry.clone()));
            }
        }
        None
    }
    fn refnode(&self, ctx: &DomainContext<'_>, entry: &ReSTObjectEntry, title: String, typ: &str) -> ResolvedReference {
        let mut reference = if ctx.docname == entry.docname {
            ResolvedReference::local(entry.node_id.clone(), Some(title))
        } else {
            let mut uri = ctx.relative_uri(ctx.docname, &entry.docname);
            if !entry.node_id.is_empty() {
                uri.push('#');
                uri.push_str(&entry.node_id);
            }
            ResolvedReference::uri(uri, Some(title))
        };
        reference.docname = Some(entry.docname.clone());
        reference.classes = vec!["xref".to_owned(), "rst".to_owned(), format!("rst-{typ}")];
        reference
    }
}

impl Domain for ReSTDomain {
    fn name(&self) -> &str { "rst" }
    fn label(&self) -> &str { "reStructuredText" }
    fn object_types(&self) -> &[ObjectTypeAlias] { object_types() }
    fn directive_names(&self) -> &[&'static str] { DIRECTIVES }
    fn role_names(&self) -> &[&'static str] { ROLES }
    fn initial_data(&self) -> DomainData {
        DomainData::from_payload(self.data_version(), &ReSTDomainData::default()).unwrap_or_default()
    }
    fn data_version(&self) -> u32 { 1 }
    fn clear_doc(&mut self, docname: &str) {
        self.data.write().objects.retain(|_, entry| entry.docname != docname);
    }
    fn merge_domaindata(&mut self, docnames: &[String], other: &DomainData) {
        let Ok(other) = other.decode::<ReSTDomainData>() else { return };
        let owned: BTreeSet<&str> = docnames.iter().map(String::as_str).collect();
        let mut data = self.data.write();
        for (key, entry) in other.objects {
            if owned.contains(entry.docname.as_str()) && !data.objects.contains_key(&key) {
                data.objects.insert(key, entry);
            }
        }
    }
    fn process_doc(&mut self, ctx: &DomainContext<'_>, _docname: &str, document: &Document) -> DomainResult<()> {
        for id in document.preorder_ids() {
            let Ok(node) = document.get(id) else { continue };
            if node.kind != NodeKind::Desc || node.get_str("domain").ok().flatten() != Some("rst") {
                continue;
            }
            let Some(objtype) = node.get_str("objtype").ok().flatten().map(str::to_owned) else { continue };
            if !DIRECTIVES.contains(&objtype.as_str()) { continue; }
            if node.attrs.get_bool("no-index").ok().flatten().unwrap_or(false) { continue; }
            let Some(name) = node.get_str("rst:name").ok().flatten().map(str::to_owned) else { continue };
            let anchor = node.get_str("rst:anchor").ok().flatten().unwrap_or("").to_owned();
            self.note_object(ctx, &objtype, &name, &anchor, node.span);
        }
        Ok(())
    }
    fn resolve_xref(
        &self, ctx: &DomainContext<'_>, typ: &str, target: &str,
        _node_id: NodeId, _contnode_id: NodeId, _document: &Document,
    ) -> DomainResult<Option<ResolvedReference>> {
        let Some((name, entry)) = self.find_obj(typ, target) else { return Ok(None) };
        Ok(Some(self.refnode(ctx, &entry, name, typ)))
    }
    fn resolve_any_xref(
        &self, ctx: &DomainContext<'_>, target: &str,
        _node_id: NodeId, _contnode_id: NodeId, _document: &Document,
    ) -> DomainResult<Vec<(String, ResolvedReference)>> {
        let mut results = Vec::new();
        for role in ROLES {
            if let Some((name, entry)) = self.find_obj(role, target) {
                results.push((format!("rst:{role}"), self.refnode(ctx, &entry, name, role)));
            }
        }
        Ok(results)
    }
    fn get_objects(&self) -> Vec<ObjectEntry> {
        self.data.read().objects.iter().map(|(key, entry)| {
            let name = key.split('\u{1f}').nth(1).unwrap_or(key);
            ObjectEntry::new(name.to_owned(), name.to_owned(), entry.objtype.clone(), entry.docname.clone(), entry.node_id.clone(), 1)
        }).collect()
    }
    fn export_data(&self) -> DomainResult<DomainData> {
        DomainData::from_payload(self.data_version(), &self.snapshot())
    }
    fn replace_data(&mut self, data: &DomainData) -> DomainResult<()> {
        if data.version != self.data_version() { return Ok(()); }
        *self.data.write() = data.decode()?;
        Ok(())
    }
}

/// Parses an rst directive signature (`.. name:: args`) into `(name, args)`.
pub fn parse_directive(signature: &str) -> (String, String) {
    let trimmed = signature.trim();
    if !trimmed.starts_with('.') {
        return (trimmed.to_owned(), String::new());
    }
    let Some(rest) = trimmed.strip_prefix(".. ") else {
        return (trimmed.to_owned(), String::new());
    };
    let Some((name, args)) = rest.split_once("::") else {
        return (rest.trim().to_owned(), String::new());
    };
    let args = args.trim();
    (name.trim().to_owned(), if args.is_empty() { String::new() } else { format!(" {args}") })
}

/// Expands `rst:*` skeleton descriptions (simple `desc_name` signatures).
pub fn prepare_document(document: &mut Document) -> DomainResult<()> {
    for id in document.preorder_ids() {
        let Ok(node) = document.get(id) else { continue };
        if node.kind != NodeKind::Desc || node.get_str("domain").ok().flatten() != Some("rst") {
            continue;
        }
        let Some(signature) = node.get_str("rst:signature").ok().flatten().map(str::to_owned) else { continue };
        let Some(objtype) = node.get_str("objtype").ok().flatten().map(str::to_owned) else { continue };
        let (name, args) = parse_directive(&signature);
        let anchor = format!("{}-{}", objtype.replace(":", "-"), name);
        let span = node.span.unwrap_or(Span::new(0, 0));
        if let Ok(node) = document.get_mut(id) {
            node.attrs.set_str("rst:name", &name);
            node.attrs.set_str("rst:anchor", &anchor);
            node.attrs.set_list("ids", vec![anchor.clone()]);
        }
        let signature_id = document.get(id).ok().and_then(|node| node.children.iter().copied().find(|&child| {
            document.get(child).is_ok_and(|child| child.kind == NodeKind::DescSignature)
        }));
        if let Some(signature_id) = signature_id {
            let mut builder = crate::sigbuild::SigBuilder::new(document, span, "rst");
            if let Ok(node) = builder.doc.get_mut(signature_id) {
                node.children.clear();
                node.attrs.set_list("classes", vec!["sig".to_owned(), "sig-object".to_owned(), "rst".to_owned()]);
            }
            let displayed = match objtype.as_str() {
                "role" => format!(":{name}:"),
                "directive:option" => format!(":{name}:"),
                _ => format!(".. {name}::"),
            };
            let name_node = builder.add(NodeKind::DescName, signature_id)?;
            builder.text(displayed, name_node)?;
            if !args.is_empty() {
                let addname = builder.add(NodeKind::DescAddname, signature_id)?;
                builder.text(args.trim(), addname)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_matches_upstream() {
        assert_eq!(object_types().len(), 3);
        assert_eq!(ROLES, ["dir", "role"]);
    }

    #[test]
    fn parses_directive_signatures() {
        assert_eq!(parse_directive(".. toctree::"), ("toctree".into(), String::new()));
        assert_eq!(parse_directive(".. note:: arg1"), ("note".into(), " arg1".into()));
        assert_eq!(parse_directive("note"), ("note".into(), String::new()));
    }
}
