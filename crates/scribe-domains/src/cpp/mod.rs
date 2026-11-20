//! The C++ domain (`cpp`) — faithful port of `sphinx/domains/cpp` (Phase 13).
//!
//! Objects are keyed by canonical name with an overload list; IDs follow the
//! deterministic Itanium subset in [`ids`].  Signature expansion lives in
//! [`signature`] and the declarator parser in [`parser`].

pub mod ids;
pub mod parser;
pub mod signature;

use std::collections::BTreeSet;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use scribe_ast::{Document, NodeId, NodeKind, Span};

use crate::diagnostics::DomainDiagnostic;
use crate::error::DomainResult;
use crate::traits::{Domain, DomainContext, ObjectTypeAlias};
use crate::types::{DomainData, ObjectEntry, ObjectType, ResolvedReference};

pub use ids::{cpp_ids, primary_id, MangleDecl, MangleParam};
pub use parser::{parse_cpp_declaration, parse_cpp_xref};
pub use signature::{prepare_document, CppOptions, CppSignatureInfo};

static OBJECT_TYPES: OnceLock<Vec<ObjectTypeAlias>> = OnceLock::new();
static DIRECTIVES: OnceLock<Vec<&'static str>> = OnceLock::new();
static ROLES: OnceLock<Vec<&'static str>> = OnceLock::new();

/// Upstream `CPPDomain.object_types`.
pub fn object_types() -> &'static [ObjectTypeAlias] {
    OBJECT_TYPES.get_or_init(|| {
        vec![
            ObjectType::new("class", "class", &["class", "struct", "identifier", "type"]),
            ObjectType::new("union", "union", &["union", "identifier", "type"]),
            ObjectType::new("function", "function", &["func", "identifier", "type"]),
            ObjectType::new("member", "member", &["member", "var", "identifier"]),
            ObjectType::new("type", "type", &["identifier", "type"]),
            ObjectType::new("concept", "concept", &["concept", "identifier"]),
            ObjectType::new("enum", "enum", &["enum", "identifier", "type"]),
            ObjectType::new("enumerator", "enumerator", &["enumerator", "identifier"]),
            ObjectType::new("functionParam", "function parameter", &["identifier", "member", "var"]),
            ObjectType::new("templateParam", "template parameter", &[
                "identifier", "class", "struct", "union", "member", "var", "type",
            ]),
        ]
    })
}

/// Upstream `CPPDomain.directives`.
pub fn directive_names() -> &'static [&'static str] {
    DIRECTIVES.get_or_init(|| {
        vec![
            "class", "struct", "union", "function", "member", "var", "type", "concept", "enum",
            "enum-struct", "enum-class", "enumerator", "namespace", "namespace-push",
            "namespace-pop", "alias",
        ]
    })
}

/// Upstream `CPPDomain.roles`.
pub fn role_names() -> &'static [&'static str] {
    ROLES.get_or_init(|| {
        vec![
            "any", "class", "struct", "union", "func", "member", "var", "type", "concept",
            "enum", "enumerator", "expr", "texpr",
        ]
    })
}

/// Maps a directive `objtype` onto upstream's canonical C++ object type.
/// `.. var::` and `.. member::` both create a `member`; the three enum
/// directives all create an `enum`.
fn canonical_objtype(objtype: &str) -> &str {
    match objtype {
        "var" => "member",
        "enum-struct" | "enum-class" => "enum",
        other => other,
    }
}

/// Object types addressable by a role.
pub fn objtypes_for_role(role: &str) -> Vec<&'static str> {
    if role == "identifier" || role == "any" {
        return object_types().iter().map(|object| object.name.as_str()).collect();
    }
    object_types()
        .iter()
        .filter(|object| object.roles.iter().any(|candidate| candidate == role))
        .map(|object| object.name.as_str())
        .collect()
}

/// One C++ object (upstream `Symbol` flattened to its lookup data).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CppObjectEntry {
    /// Document holding the declaration.
    pub docname: String,
    /// Primary anchor id.
    pub node_id: String,
    /// Object type.
    pub objtype: String,
    /// Human-readable declaration signature.
    pub declaration: String,
    /// True for an alias declaration.
    pub aliased: bool,
    /// 1-based source line of the declaration (for duplicate warnings).
    pub line: Option<u32>,
}

/// Serializable C++ inventory: canonical name → overload list.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CppDomainData {
    /// Canonical name → overload list.
    pub objects: std::collections::BTreeMap<String, Vec<CppObjectEntry>>,
}

/// The `cpp` domain implementation.
pub struct CppDomain {
    data: parking_lot::RwLock<CppDomainData>,
}

impl Default for CppDomain {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for CppDomain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CppDomain")
            .field("objects", &self.data.read().objects.len())
            .finish_non_exhaustive()
    }
}

impl CppDomain {
    /// Creates an empty domain.
    pub fn new() -> Self {
        Self { data: parking_lot::RwLock::new(CppDomainData::default()) }
    }
    /// Creates a domain from a persisted inventory.
    pub fn from_data(data: CppDomainData) -> Self {
        Self { data: parking_lot::RwLock::new(data) }
    }
    /// Consistent inventory snapshot.
    pub fn snapshot(&self) -> CppDomainData {
        self.data.read().clone()
    }

    /// Registers a declaration, keeping overloads in the same bucket.
    #[allow(clippy::too_many_arguments)]
    pub fn note_object(
        &self,
        ctx: &DomainContext<'_>,
        name: &str,
        objtype: &str,
        node_id: &str,
        declaration: &str,
        aliased: bool,
        span: Option<Span>,
        line: Option<u32>,
        directive_type: &str,
    ) {
        let objtype = canonical_objtype(objtype);
        let mut data = self.data.write();
        let entries = data.objects.entry(name.to_owned()).or_default();
        let normalized = declaration.trim_end_matches(';').trim();
        if let Some(other) = entries.iter().find(|entry| entry.declaration.trim_end_matches(';').trim() == normalized && entry.aliased == aliased) {
            // Upstream's C++ domain reports duplicates through the symbol
            // table with a distinct message and `duplicate_declaration.cpp`.
            let message = format!(
                "Duplicate C++ declaration, also defined at {}:{}.\nDeclaration is '.. cpp:{}:: {}'.",
                other.docname,
                other.line.unwrap_or(0),
                directive_type,
                declaration,
            );
            let diagnostic = DomainDiagnostic::duplicate_object(
                message, objtype, name, Some(ctx.docname.to_owned()), span,
            ).with_line(line);
            ctx.push_diagnostic(diagnostic);
        }
        entries.retain(|entry| !(entry.aliased && !aliased));
        if !aliased || !entries.iter().any(|entry| !entry.aliased) {
            entries.push(CppObjectEntry {
                docname: ctx.docname.to_owned(),
                node_id: node_id.to_owned(),
                objtype: objtype.to_owned(),
                declaration: declaration.to_owned(),
                aliased,
                line,
            });
        }
    }

    /// Finds candidate declarations for a C++ target with scope awareness.
    ///
    /// When `filter_by_role` is `true` only object types addressable by `role`
    /// are returned (upstream `resolve_any_xref`). Normal `resolve_xref` uses
    /// `false`: upstream looks the symbol up by name regardless of the role and
    /// emits a separate "targets a …" warning on a type mismatch.
    pub fn find_obj(
        &self,
        target: &str,
        role: &str,
        scope: &str,
        specific: bool,
        filter_by_role: bool,
    ) -> Vec<(String, CppObjectEntry)> {
        let mut target = parse_cpp_xref(target);
        if target.starts_with("::") {
            target = target[2..].to_owned();
        }
        if target.is_empty() {
            return Vec::new();
        }
        let allowed = objtypes_for_role(role);
        let matches_allowed = |entry: &CppObjectEntry| {
            !filter_by_role || allowed.is_empty() || allowed.contains(&entry.objtype.as_str())
        };
        let data = self.data.read();
        // Upstream resolves C++ names relative to `cpp:parent_key`, walking
        // outward through enclosing scopes until a match is found. Try every
        // enclosing scope prefix (innermost first), then the bare name.
        let mut candidates = Vec::new();
        if !scope.is_empty() {
            let mut prefix = scope.to_owned();
            loop {
                candidates.push(format!("{prefix}::{target}"));
                match prefix.rsplit_once("::") {
                    Some((head, _)) => prefix = head.to_owned(),
                    None => break,
                }
            }
        }
        candidates.push(target.clone());
        for candidate in candidates {
            if let Some(entries) = data.objects.get(&candidate) {
                let allowed_entries: Vec<_> = entries
                    .iter()
                    .filter(|entry| matches_allowed(entry))
                    .cloned()
                    .collect();
                if !allowed_entries.is_empty() {
                    return allowed_entries.into_iter().map(|entry| (candidate.clone(), entry)).collect();
                }
            }
        }
        // Bare identifiers from expression tokens refer to template/function
        // parameters registered as `Owner::param`; resolve them by suffix even
        // without the `specific` flag (upstream keeps them addressable).
        if specific || !target.contains("::") {
            let suffix = format!("::{target}");
            let mut matches = Vec::new();
            for (name, entries) in data.objects.iter() {
                if name.ends_with(&suffix) {
                    for entry in entries {
                        if matches_allowed(entry) {
                            matches.push((name.clone(), entry.clone()));
                        }
                    }
                }
            }
            return matches;
        }
        Vec::new()
    }

    /// Resolves a function call target to the best overload by argument count.
    pub fn resolve_overload(candidates: &[(String, CppObjectEntry)], arg_count: usize) -> Option<&(String, CppObjectEntry)> {
        if arg_count == 0 || candidates.len() < 2 {
            return candidates.first();
        }
        candidates.iter().min_by_key(|(_, entry)| {
            let count = entry.declaration.matches(',').count() + usize::from(entry.declaration.contains('(') && !entry.declaration.contains("()"));
            count.abs_diff(arg_count)
        })
    }

    fn make_refnode(
        &self,
        ctx: &DomainContext<'_>,
        entry: &CppObjectEntry,
        title: String,
        role: &str,
        signature: bool,
    ) -> ResolvedReference {
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
        let class = if role == "identifier" || role == "any" { "cpp-identifier" } else { "cpp-type" };
        reference.classes = vec!["xref".to_owned(), "cpp".to_owned(), class.to_owned()];
        reference
    }
}

impl Domain for CppDomain {
    fn name(&self) -> &str { "cpp" }
    fn label(&self) -> &str { "C++" }
    fn object_types(&self) -> &[ObjectTypeAlias] { object_types() }
    fn directive_names(&self) -> &[&'static str] { directive_names() }
    fn role_names(&self) -> &[&'static str] { role_names() }
    fn initial_data(&self) -> DomainData {
        DomainData::from_payload(self.data_version(), &CppDomainData::default()).unwrap_or_default()
    }
    fn data_version(&self) -> u32 { 1 }
    fn clear_doc(&mut self, docname: &str) {
        let mut data = self.data.write();
        for entries in data.objects.values_mut() {
            entries.retain(|entry| entry.docname != docname);
        }
        data.objects.retain(|_, entries| !entries.is_empty());
    }
    fn merge_domaindata(&mut self, docnames: &[String], other: &DomainData) {
        let Ok(other) = other.decode::<CppDomainData>() else { return };
        let owned: BTreeSet<&str> = docnames.iter().map(String::as_str).collect();
        let mut data = self.data.write();
        for (name, entries) in other.objects {
            for entry in entries {
                if owned.contains(entry.docname.as_str()) {
                    let bucket = data.objects.entry(name.clone()).or_default();
                    if !bucket.contains(&entry) {
                        bucket.push(entry);
                    }
                }
            }
        }
    }
    fn process_doc(&mut self, ctx: &DomainContext<'_>, _docname: &str, document: &Document) -> DomainResult<()> {
        for id in document.preorder_ids() {
            let Ok(node) = document.get(id) else { continue };
            if node.kind != NodeKind::Desc || node.get_str("domain").ok().flatten() != Some("cpp") {
                continue;
            }
            let Some(objtype) = node.get_str("objtype").ok().flatten().map(str::to_owned) else { continue };
            if !directive_names().contains(&objtype.as_str()) {
                continue;
            }
            if node.attrs.get_bool("no-index").ok().flatten().unwrap_or(false) {
                continue;
            }
            let Some(fullname) = node.get_str("cpp:fullname").ok().flatten().map(str::to_owned) else { continue };
            let anchor = node.get_str("cpp:anchor").ok().flatten().unwrap_or("").to_owned();
            let declaration = node
                .get_str("cpp:declaration")
                .ok()
                .flatten()
                .map(str::to_owned)
                .unwrap_or_else(|| fullname.clone());
            let line = node.source.as_ref().and_then(|source| source.line);
            let directive_type = node.get_str("desctype").ok().flatten().unwrap_or(&objtype).to_owned();
            self.note_object(
                ctx, &fullname, &objtype, &anchor, &declaration, false,
                node.span, line, &directive_type,
            );
            // Upstream registers template parameters and named function
            // parameters as first-class symbols so roles inside the
            // declaration can refer to them by their bare names.
            if let Some(raw) = node.get_str("cpp:overloads").ok().flatten() {
                if let Ok(overloads) = serde_json::from_str::<Vec<(String, String, String)>>(raw) {
                    for (overload_name, overload_id, overload_decl) in overloads.into_iter().skip(1) {
                        self.note_object(
                            ctx, &overload_name, &objtype, &overload_id, &overload_decl, false,
                            node.span, line, &directive_type,
                        );
                    }
                }
            }
            for (key, kind) in [("cpp:template_params", "templateParam"), ("cpp:function_params", "functionParam")] {
                let Some(raw) = node.get_str(key).ok().flatten() else { continue };
                let Ok(names) = serde_json::from_str::<Vec<String>>(raw) else { continue };
                for name in names {
                    if name.is_empty() {
                        continue;
                    }
                    let qualified = format!("{fullname}::{name}");
                    self.note_object(
                        ctx, &qualified, kind, &anchor, &name, false,
                        node.span, line, kind,
                    );
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
            return Ok(Some(ResolvedReference::default().passthrough()));
        }
        // Upstream raises `NoUri` for names in the `std::` namespace: they are
        // library types, not project symbols, and never warn.
        let bare = target.trim_end_matches(['*', '&']).trim();
        if target.starts_with("std::") || target == "std" || target == "type"
            || (target.contains("::") && matches!(typ, "identifier"))
            // Builtin types and single-letter template parameters carry no
            // documentation target; upstream renders them as plain signature
            // tokens rather than cross references.
            || crate::cpp::parser::BUILTIN_TYPES.contains(&bare)
            || (typ == "type" && bare.len() == 1 && bare.chars().all(|c| c.is_ascii_uppercase()))
        {
            return Ok(Some(ResolvedReference::default().passthrough()));
        }
        let scope = document
            .get(node_id)
            .ok()
            .and_then(|node| node.get_str("cpp:scope").ok().flatten())
            .unwrap_or("");
        let specific = document
            .get(node_id)
            .ok()
            .and_then(|node| node.pending_xref())
            .and_then(|xref| xref.refspecific())
            .unwrap_or(false);
        let explicit = document
            .get(node_id)
            .ok()
            .and_then(|node| node.pending_xref())
            .and_then(|xref| xref.refexplicit())
            .unwrap_or(false);
        let role = if typ == "any" { "identifier" } else { typ };
        // Upstream's C++ symbol lookup is by name, not by role. A mismatched
        // role still resolves and produces a separate "targets a …" warning.
        let matches = self.find_obj(target, role, scope, specific, false);
        if matches.is_empty() {
            return Ok(None);
        }
        let (name, entry) = matches
            .iter()
            .find(|(_, entry)| !entry.aliased)
            .unwrap_or(&matches[0]);
        // Mirror upstream `CPPDomain._check_type`: a role that cannot address
        // the target's object type still resolves, but logs a diagnostic.
        if role != "any" && role != "identifier" && role != "expr" && role != "texpr" {
            let mut allowed = objtypes_for_role(role);
            // A C++ alias is addressable by the same roles as a type/identifier;
            // upstream's declaration objectType for `using X = Y` is `type`.
            if entry.objtype == "alias" {
                allowed.push("alias");
            }
            if !allowed.is_empty() && !allowed.contains(&entry.objtype.as_str()) {
                let message = format!("cpp:{role} targets a {} ({name}).", entry.objtype);
                let source = document.get(node_id).ok().and_then(|node| node.source.as_ref());
                ctx.push_diagnostic(DomainDiagnostic::bare_warning(
                    message,
                    Some(ctx.docname.to_owned()),
                    ctx.span_of(document, node_id),
                ).with_source(source));
            }
        }
        let title = if explicit { name.clone() } else { name.rsplit("::").next().unwrap_or(name).to_owned() };
        let signature = document.get(node_id).ok().and_then(|node| node.get_str("cpp:scope").ok().flatten()).is_some();
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
        let scope = document.get(node_id).ok()
            .and_then(|node| node.get_str("cpp:scope").ok().flatten())
            .unwrap_or("");
        let mut results = Vec::new();
        for role in ["class", "function", "member", "type", "enum", "concept", "enumerator"] {
            if let Some((name, entry)) = self.find_obj(target, role, scope, true, true).into_iter().next() {
                let title = name.rsplit("::").next().unwrap_or(&name).to_owned();
                let mut reference = self.make_refnode(ctx, &entry, title, role, false);
                reference.classes = vec!["xref".to_owned(), "any".to_owned(), "cpp".to_owned(), format!("cpp-{role}")];
                results.push((format!("cpp:{role}"), reference));
            }
        }
        results.dedup_by(|a, b| a.1.refid == b.1.refid && a.1.refuri == b.1.refuri);
        Ok(results)
    }
    fn get_objects(&self) -> Vec<ObjectEntry> {
        let mut entries = Vec::new();
        for (name, overloads) in self.data.read().objects.iter() {
            for (index, entry) in overloads.iter().enumerate() {
                entries.push(ObjectEntry::new(
                    if index == 0 { name.clone() } else { format!("{name}#{index}") },
                    name.rsplit("::").next().unwrap_or(name).to_owned(),
                    entry.objtype.clone(),
                    entry.docname.clone(),
                    entry.node_id.clone(),
                    if entry.aliased { -1 } else { 1 },
                ));
            }
        }
        entries
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_matches_upstream() {
        assert_eq!(object_types().len(), 10);
        assert!(role_names().contains(&"any"));
        assert!(directive_names().contains(&"enum-class"));
    }

    #[test]
    fn role_mapping_includes_aliases() {
        assert!(objtypes_for_role("struct").contains(&"class"));
        assert_eq!(objtypes_for_role("identifier").len(), object_types().len());
    }
}
