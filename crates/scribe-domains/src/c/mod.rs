//! The C domain (`c`) — faithful port of `sphinx/domains/c` (Phase 13).
//!
//! Objects are stored by fully-qualified name (namespace + nested scope) with
//! deterministic `c.<scope>.<name>` anchors.  Signature expansion lives in
//! [`signature`] and the declarator parser in [`parser`].

pub mod parser;
pub mod signature;

use std::collections::BTreeSet;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use scribe_ast::{Document, NodeId, NodeKind};

use crate::diagnostics::DomainDiagnostic;
use crate::error::DomainResult;
use crate::traits::{Domain, DomainContext, ObjectTypeAlias};
use crate::types::{DomainData, ObjectEntry, ObjectType, ResolvedReference};

pub use parser::parse_declaration;
pub use signature::{prepare_document, COptions, CSignatureInfo};

static OBJECT_TYPES: OnceLock<Vec<ObjectTypeAlias>> = OnceLock::new();
static DIRECTIVES: OnceLock<Vec<&'static str>> = OnceLock::new();
static ROLES: OnceLock<Vec<&'static str>> = OnceLock::new();

/// Upstream `CDomain.object_types`, in declaration order.
pub fn object_types() -> &'static [ObjectTypeAlias] {
    OBJECT_TYPES.get_or_init(|| {
        vec![
            ObjectType::new("member", "member", &["var", "member", "data", "identifier"]),
            ObjectType::new("var", "variable", &["var", "member", "data", "identifier"]),
            ObjectType::new("function", "function", &["func", "identifier", "type"]),
            ObjectType::new("macro", "macro", &["macro", "identifier"]),
            ObjectType::new("struct", "struct", &["struct", "identifier", "type"]),
            ObjectType::new("union", "union", &["union", "identifier", "type"]),
            ObjectType::new("enum", "enum", &["enum", "identifier", "type"]),
            ObjectType::new("enumerator", "enumerator", &["enumerator", "identifier"]),
            ObjectType::new("type", "type", &["identifier", "type"]),
            ObjectType::new("functionParam", "function parameter", &["identifier", "var", "member", "data"]),
        ]
    })
}

/// Upstream `CDomain.directives`.
pub fn directive_names() -> &'static [&'static str] {
    DIRECTIVES.get_or_init(|| {
        vec![
            "member", "var", "function", "macro", "struct", "union", "enum", "enumerator",
            "type", "namespace", "namespace-push", "namespace-pop", "alias",
        ]
    })
}

/// Upstream `CDomain.roles`.
pub fn role_names() -> &'static [&'static str] {
    ROLES.get_or_init(|| {
        vec![
            "member", "data", "var", "func", "macro", "struct", "union", "enum", "enumerator",
            "type", "expr", "texpr",
        ]
    })
}

/// Object types addressable by a role (upstream `objtypes_for_role`).
pub fn objtypes_for_role(role: &str) -> Vec<&'static str> {
    if role == "identifier" {
        return object_types().iter().map(|object| object.name.as_str()).collect();
    }
    object_types()
        .iter()
        .filter(|object| object.roles.iter().any(|candidate| candidate == role))
        .map(|object| object.name.as_str())
        .collect()
}

/// Upstream `ObjectEntry` named tuple for the C domain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CObjectEntry {
    /// Document holding the declaration.
    pub docname: String,
    /// Anchor id.
    pub node_id: String,
    /// Object type.
    pub objtype: String,
    /// True for an alias declaration.
    pub aliased: bool,
}

/// Serializable C-domain inventory.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CDomainData {
    /// Fully-qualified name → object entry.
    pub objects: std::collections::BTreeMap<String, CObjectEntry>,
}

/// The `c` domain implementation.
pub struct CDomain {
    data: parking_lot::RwLock<CDomainData>,
}

impl Default for CDomain {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for CDomain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CDomain")
            .field("objects", &self.data.read().objects.len())
            .finish_non_exhaustive()
    }
}

impl CDomain {
    /// Creates an empty domain.
    pub fn new() -> Self {
        Self { data: parking_lot::RwLock::new(CDomainData::default()) }
    }

    /// Creates a domain from a persisted inventory.
    pub fn from_data(data: CDomainData) -> Self {
        Self { data: parking_lot::RwLock::new(data) }
    }

    /// Consistent inventory snapshot.
    pub fn snapshot(&self) -> CDomainData {
        self.data.read().clone()
    }

    /// Registers an object with upstream's duplicate warning behavior.
    pub fn note_object(
        &self,
        ctx: &DomainContext<'_>,
        name: &str,
        objtype: &str,
        node_id: &str,
        aliased: bool,
        span: Option<scribe_ast::Span>,
    ) {
        let mut data = self.data.write();
        if let Some(other) = data.objects.get(name) {
            if other.aliased && !aliased {
                // Real definition supersedes an alias.
            } else if !other.aliased && aliased {
                return;
            } else {
                let message = format!(
                    "duplicate object description of {name}, other instance in {}, use :no-index: for one of them",
                    other.docname
                );
                ctx.push_diagnostic(DomainDiagnostic::duplicate_object(
                    message, objtype, name, Some(ctx.docname.to_owned()), span,
                ));
            }
        }
        data.objects.insert(
            name.to_owned(),
            CObjectEntry { docname: ctx.docname.to_owned(), node_id: node_id.to_owned(), objtype: objtype.to_owned(), aliased },
        );
    }

    /// Finds candidate objects for a target, honoring scope and role.
    pub fn find_obj(&self, target: &str, role: &str, scope: &str) -> Vec<(String, CObjectEntry)> {
        let mut target = target.trim();
        if target.starts_with("::") {
            target = &target[2..];
        }
        if role == "func" {
            target = target.strip_suffix("()").unwrap_or(target);
        }
        if target.is_empty() {
            return Vec::new();
        }
        let allowed = objtypes_for_role(role);
        let data = self.data.read();
        let mut matches = Vec::new();
        // Upstream's symbol lookup walks *up* the parent scope chain until a
        // scope declares the name (`ancestor_lookup_type`).  This makes a
        // `:c:var:`i`` inside a function resolve to the parameter `f.i` or,
        // failing that, to the namespace variable `i`, instead of producing a
        // suffix-match ambiguity across unrelated namespaces.
        let mut current = scope.to_owned();
        loop {
            let scoped = if current.is_empty() { target.to_owned() } else { format!("{current}.{target}") };
            if let Some(entry) = data.objects.get(&scoped) {
                if allowed.is_empty() || allowed.contains(&entry.objtype.as_str()) {
                    matches.push((scoped, entry.clone()));
                    break;
                }
            }
            if current.is_empty() {
                break;
            }
            current = match current.rsplit_once('.') {
                Some((head, _)) => head.to_owned(),
                None => String::new(),
            };
        }
        if matches.is_empty() {
            if let Some(entry) = data.objects.get(target) {
                if allowed.is_empty() || allowed.contains(&entry.objtype.as_str()) {
                    matches.push((target.to_owned(), entry.clone()));
                }
            }
        }
        if matches.is_empty() {
            // Final fallback mirrors `find_declaration(recurseInAnon=True)`:
            // search nested symbols, but only inside the active scope subtree.
            // A global suffix search would let `b` leak across namespaces.
            let suffix = format!(".{target}");
            let prefix = if scope.is_empty() { String::new() } else { format!("{scope}.") };
            for (name, entry) in data.objects.iter() {
                if name.starts_with(&prefix) && name.ends_with(&suffix)
                    && (allowed.is_empty() || allowed.contains(&entry.objtype.as_str()))
                {
                    matches.push((name.clone(), entry.clone()));
                }
            }
        }
        matches
    }

    fn make_refnode(
        &self,
        ctx: &DomainContext<'_>,
        entry: &CObjectEntry,
        title: String,
        role: &str,
        signature: bool,
    ) -> ResolvedReference {
        // Signature-generated xrefs keep their `desc_sig_name` content; user
        // roles replace it with the display title (upstream's `contnode` reuse).
        let display = if signature { None } else { Some(title.clone()) };
        let mut reference = if ctx.docname == entry.docname {
            ResolvedReference::local(entry.node_id.clone(), display)
        } else {
            let mut uri = ctx.relative_uri(ctx.docname, &entry.docname);
            if !entry.node_id.is_empty() {
                uri.push('#');
                uri.push_str(&entry.node_id);
            }
            ResolvedReference::uri(uri, display)
        };
        if signature {
            reference.reftitle = Some(title);
        }
        reference.docname = Some(entry.docname.clone());
        let class = if role == "identifier" { "c-identifier".to_owned() } else { format!("c-{role}") };
        reference.classes = vec!["xref".to_owned(), "c".to_owned(), class];
        reference
    }
}

impl Domain for CDomain {
    fn name(&self) -> &str {
        "c"
    }
    fn label(&self) -> &str {
        "C"
    }
    fn object_types(&self) -> &[ObjectTypeAlias] {
        object_types()
    }
    fn directive_names(&self) -> &[&'static str] {
        directive_names()
    }
    fn role_names(&self) -> &[&'static str] {
        role_names()
    }
    fn initial_data(&self) -> DomainData {
        DomainData::from_payload(self.data_version(), &CDomainData::default()).unwrap_or_default()
    }
    fn data_version(&self) -> u32 {
        1
    }
    fn clear_doc(&mut self, docname: &str) {
        self.data.write().objects.retain(|_, entry| entry.docname != docname);
    }
    fn merge_domaindata(&mut self, docnames: &[String], other: &DomainData) {
        let Ok(other) = other.decode::<CDomainData>() else { return };
        let owned: BTreeSet<&str> = docnames.iter().map(String::as_str).collect();
        let mut data = self.data.write();
        for (name, entry) in other.objects {
            if owned.contains(entry.docname.as_str()) && !data.objects.contains_key(&name) {
                data.objects.insert(name, entry);
            }
        }
    }
    fn process_doc(&mut self, ctx: &DomainContext<'_>, _docname: &str, document: &Document) -> DomainResult<()> {
        for id in document.preorder_ids() {
            let Ok(node) = document.get(id) else { continue };
            if node.kind != NodeKind::Desc || node.get_str("domain").ok().flatten() != Some("c") {
                continue;
            }
            let Some(objtype) = node.get_str("objtype").ok().flatten().map(str::to_owned) else { continue };
            if !directive_names().contains(&objtype.as_str()) {
                continue;
            }
            if node.attrs.get_bool("no-index").ok().flatten().unwrap_or(false) {
                continue;
            }
            let Some(fullname) = node.get_str("c:fullname").ok().flatten().map(str::to_owned) else { continue };
            let anchor = node.get_str("c:anchor").ok().flatten().unwrap_or("").to_owned();
            self.note_object(ctx, &fullname, &objtype, &anchor, false, node.span);
            // Members declared inside an anonymous struct/union are also
            // addressable without the `@name` anonymous segment (upstream's
            // `recurseInAnon` lookup), e.g. `A.@data.a` is also `A.a`.
            let parts: Vec<&str> = fullname.split('.').collect();
            if objtype == "member" {
                if let Some(anon) = parts.iter().position(|part| part.starts_with('@')) {
                    let mut alias_parts = parts.clone();
                    alias_parts.remove(anon);
                    let alias = alias_parts.join(".");
                    if !alias.is_empty() {
                        self.note_object(ctx, &alias, &objtype, &anchor, true, node.span);
                    }
                }
            }
            // Function parameters register as hidden `functionParam` objects.
            if objtype == "function" {
                if let Some(index_id) = node.attrs.get_node_id("c:index_id").ok().flatten() {
                    let _ = index_id;
                }
                if let Some(param_names) = node.get_str("c:param_names").ok().flatten() {
                    for param in param_names.split('\u{1f}') {
                        if !param.is_empty() {
                            let key = format!("{fullname}.{param}");
                            self.note_object(ctx, &key, "functionParam", &anchor, false, node.span);
                        }
                    }
                }
            }
        }
        Ok(())
    }
    fn resolve_xref(
        &self,
        ctx: &DomainContext<'_>,
        typ: &str,
        target: &str,
        node_id: NodeId,
        _contnode_id: NodeId,
        document: &Document,
    ) -> DomainResult<Option<ResolvedReference>> {
        if typ == "expr" || typ == "texpr" {
            // Expression roles render as plain code; no object lookup.
            return Ok(Some(ResolvedReference::default().passthrough()));
        }
        let scope = document
            .get(node_id)
            .ok()
            .and_then(|node| node.get_str("c:scope").ok().flatten())
            .unwrap_or("")
            .to_owned();
        let explicit = document
            .get(node_id)
            .ok()
            .and_then(|node| node.pending_xref())
            .and_then(|xref| xref.refexplicit())
            .unwrap_or(false);
        let mut matches = self.find_obj(target, typ, &scope);
        if matches.is_empty() && typ == "identifier" {
            matches = self.find_obj(target, "type", &scope);
        }
        if matches.is_empty() {
            return Ok(None);
        }
        if matches.len() > 1 {
            let canonical: Vec<_> = matches.iter().filter(|(_, entry)| !entry.aliased).collect();
            if canonical.len() == 1 {
                matches = vec![canonical[0].clone()];
            } else {
                let rendered = matches.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>().join(", ");
                let message = format!("more than one target found for cross-reference '{target}': {rendered}");
                ctx.push_diagnostic(DomainDiagnostic::malformed(
                    message,
                    Some("ref".to_owned()),
                    Some(target.to_owned()),
                    Some(ctx.docname.to_owned()),
                    ctx.span_of(document, node_id),
                ));
            }
        }
        let (name, entry) = &matches[0];
        let title = if explicit {
            name.clone()
        } else {
            // Upstream displays the parsed target name (e.g. `f.i` stays
            // `f.i`), not the resolved object's fully-qualified name.
            target.to_owned()
        };
        let signature = document.get(node_id).ok().and_then(|node| node.get_str("c:scope").ok().flatten()).is_some();
        Ok(Some(self.make_refnode(ctx, entry, title, typ, signature)))
    }
    fn resolve_any_xref(
        &self,
        ctx: &DomainContext<'_>,
        target: &str,
        node_id: NodeId,
        _contnode_id: NodeId,
        document: &Document,
    ) -> DomainResult<Vec<(String, ResolvedReference)>> {
        let scope = document
            .get(node_id)
            .ok()
            .and_then(|node| node.get_str("c:scope").ok().flatten())
            .unwrap_or("");
        let mut results = Vec::new();
        for role in role_names() {
            if matches!(*role, "expr" | "texpr") {
                continue;
            }
            if let Some((name, entry)) = self.find_obj(target, role, scope).into_iter().next() {
                let title = name.rsplit('.').next().unwrap_or(&name).to_owned();
                let mut reference = self.make_refnode(ctx, &entry, title, role, false);
                reference.classes = vec!["xref".to_owned(), "any".to_owned(), "c".to_owned(), format!("c-{role}")];
                results.push((format!("c:{role}"), reference));
            }
        }
        results.dedup_by(|a, b| a.1.refid == b.1.refid && a.1.refuri == b.1.refuri);
        Ok(results)
    }
    fn get_objects(&self) -> Vec<ObjectEntry> {
        self.data
            .read()
            .objects
            .iter()
            .map(|(name, entry)| {
                ObjectEntry::new(
                    name.clone(),
                    name.rsplit('.').next().unwrap_or(name).to_owned(),
                    entry.objtype.clone(),
                    entry.docname.clone(),
                    entry.node_id.clone(),
                    if entry.aliased { -1 } else { 1 },
                )
            })
            .collect()
    }
    fn export_data(&self) -> DomainResult<DomainData> {
        DomainData::from_payload(self.data_version(), &self.snapshot())
    }
    fn replace_data(&mut self, data: &DomainData) -> DomainResult<()> {
        if data.version != self.data_version() {
            return Ok(());
        }
        *self.data.write() = data.decode()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_matches_upstream() {
        assert_eq!(object_types().len(), 10);
        assert_eq!(object_types()[0].roles, ["var", "member", "data", "identifier"]);
        assert!(role_names().contains(&"texpr"));
        assert!(directive_names().contains(&"namespace-pop"));
    }

    #[test]
    fn objtypes_for_identifier_are_all() {
        assert_eq!(objtypes_for_role("identifier").len(), object_types().len());
        assert_eq!(objtypes_for_role("func"), ["function"]);
    }
}
