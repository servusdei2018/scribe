//! The JavaScript domain (`js`) — port of `sphinx/domains/javascript.py`
//! (Phase 13).
//!
//! Implements `function` / `method` / `class` / `data` / `attribute` / `module`
//! directives, module context, `this`/prototype nesting and xref resolution
//! with `.` / `~` modifiers and `:any:` support.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use scribe_ast::{Document, NodeId, NodeKind, Span};

use crate::diagnostics::DomainDiagnostic;
use crate::error::DomainResult;
use crate::sigbuild::SigBuilder;
use crate::traits::{Domain, DomainContext, ObjectTypeAlias};
use crate::types::{DomainData, ObjectEntry, ObjectType, ResolvedReference};

static OBJECT_TYPES: OnceLock<Vec<ObjectTypeAlias>> = OnceLock::new();

/// Upstream `JavaScriptDomain.object_types`.
pub fn object_types() -> &'static [ObjectTypeAlias] {
    OBJECT_TYPES.get_or_init(|| {
        vec![
            ObjectType::new("function", "function", &["func"]),
            ObjectType::new("method", "method", &["meth"]),
            ObjectType::new("class", "class", &["class"]),
            ObjectType::new("data", "data", &["data"]),
            ObjectType::new("attribute", "attribute", &["attr"]),
            ObjectType::new("module", "module", &["mod"]),
        ]
    })
}

/// Upstream directive names.
pub const DIRECTIVES: &[&str] = &["function", "method", "class", "data", "attribute", "module"];
/// Upstream role names.
pub const ROLES: &[&str] = &["func", "meth", "class", "data", "attr", "mod"];

/// Object types addressable by a role.
pub fn objtypes_for_role(role: &str) -> Option<Vec<&'static str>> {
    object_types().iter().find(|object| object.roles.iter().any(|candidate| candidate == role))
        .map(|object| vec![object.name.as_str()])
}

/// Serializable JS inventory.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JavaScriptDomainData {
    /// Fully-qualified name → entry.
    pub objects: std::collections::BTreeMap<String, JavaScriptObjectEntry>,
    /// Module name → entry.
    pub modules: std::collections::BTreeMap<String, JavaScriptModuleEntry>,
}

/// One JS object entry (upstream `(docname, node_id, objtype)`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JavaScriptObjectEntry {
    /// Document holding the declaration.
    pub docname: String,
    /// Anchor id.
    pub node_id: String,
    /// Object type.
    pub objtype: String,
}

/// One JS module entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JavaScriptModuleEntry {
    /// Document holding the declaration.
    pub docname: String,
    /// Anchor id.
    pub node_id: String,
}

/// The `js` domain implementation.
pub struct JavaScriptDomain {
    data: parking_lot::RwLock<JavaScriptDomainData>,
}

impl Default for JavaScriptDomain {
    fn default() -> Self { Self::new() }
}

impl std::fmt::Debug for JavaScriptDomain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JavaScriptDomain")
            .field("objects", &self.data.read().objects.len())
            .field("modules", &self.data.read().modules.len())
            .finish_non_exhaustive()
    }
}

impl JavaScriptDomain {
    /// Creates an empty domain.
    pub fn new() -> Self {
        Self { data: parking_lot::RwLock::new(JavaScriptDomainData::default()) }
    }
    /// Creates a domain from a persisted inventory.
    pub fn from_data(data: JavaScriptDomainData) -> Self {
        Self { data: parking_lot::RwLock::new(data) }
    }
    /// Consistent inventory snapshot.
    pub fn snapshot(&self) -> JavaScriptDomainData {
        self.data.read().clone()
    }

    /// Registers an object with upstream's duplicate warning.
    pub fn note_object(&self, ctx: &DomainContext<'_>, fullname: &str, objtype: &str, node_id: &str, span: Option<Span>) {
        let mut data = self.data.write();
        if let Some(other) = data.objects.get(fullname) {
            let message = format!(
                "duplicate {} description of {}, other {} in {}",
                objtype, fullname, objtype, other.docname
            );
            ctx.push_diagnostic(DomainDiagnostic::duplicate_object(
                message, objtype, fullname, Some(ctx.docname.to_owned()), span,
            ));
        }
        data.objects.insert(fullname.to_owned(), JavaScriptObjectEntry {
            docname: ctx.docname.to_owned(), node_id: node_id.to_owned(), objtype: objtype.to_owned(),
        });
    }

    /// Registers a module.
    pub fn note_module(&self, ctx: &DomainContext<'_>, name: &str, node_id: &str) {
        self.data.write().modules.insert(
            name.to_owned(),
            JavaScriptModuleEntry { docname: ctx.docname.to_owned(), node_id: node_id.to_owned() },
        );
    }

    /// Finds JS objects by name with module/object scope fallback.
    pub fn find_obj(&self, target: &str, role: &str, module: &str, object: &str, specific: bool) -> Vec<(String, JavaScriptObjectEntry)> {
        let mut target = target.trim().to_owned();
        if let Some(rest) = target.strip_prefix('.') {
            target = rest.to_owned();
        }
        if matches!(role, "func" | "meth") {
            target = target.strip_suffix("()").unwrap_or(&target).to_owned();
        }
        let allowed = objtypes_for_role(role);
        let data = self.data.read();
        let mut candidates = Vec::new();
        if specific {
            if !object.is_empty() {
                candidates.push(format!("{object}.{target}"));
                if !module.is_empty() {
                    candidates.push(format!("{module}.{object}.{target}"));
                }
            }
            if !module.is_empty() {
                candidates.push(format!("{module}.{target}"));
            }
        } else {
            if !module.is_empty() && !object.is_empty() {
                candidates.push(format!("{module}.{object}.{target}"));
            }
            if !object.is_empty() {
                candidates.push(format!("{object}.{target}"));
            }
            if !module.is_empty() {
                candidates.push(format!("{module}.{target}"));
            }
        }
        candidates.push(target.clone());
        for candidate in candidates {
            if let Some(entry) = data.objects.get(&candidate) {
                if allowed.as_ref().is_none_or(|types| types.contains(&entry.objtype.as_str())) {
                    return vec![(candidate, entry.clone())];
                }
            }
        }
        if specific {
            let suffix = format!(".{target}");
            let mut matches = Vec::new();
            for (name, entry) in data.objects.iter() {
                if name.ends_with(&suffix) && allowed.as_ref().is_none_or(|types| types.contains(&entry.objtype.as_str())) {
                    matches.push((name.clone(), entry.clone()));
                }
            }
            return matches;
        }
        Vec::new()
    }

    fn make_refnode(&self, ctx: &DomainContext<'_>, entry: &JavaScriptObjectEntry, title: String, role: &str) -> ResolvedReference {
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
        let class = format!("js-{role}");
        reference.classes = vec!["xref".to_owned(), "js".to_owned(), class];
        reference
    }
}

impl Domain for JavaScriptDomain {
    fn name(&self) -> &str { "js" }
    fn label(&self) -> &str { "JavaScript" }
    fn object_types(&self) -> &[ObjectTypeAlias] { object_types() }
    fn directive_names(&self) -> &[&'static str] { DIRECTIVES }
    fn role_names(&self) -> &[&'static str] { ROLES }
    fn initial_data(&self) -> DomainData {
        DomainData::from_payload(self.data_version(), &JavaScriptDomainData::default()).unwrap_or_default()
    }
    fn data_version(&self) -> u32 { 1 }
    fn clear_doc(&mut self, docname: &str) {
        let mut data = self.data.write();
        data.objects.retain(|_, entry| entry.docname != docname);
        data.modules.retain(|_, entry| entry.docname != docname);
    }
    fn merge_domaindata(&mut self, docnames: &[String], other: &DomainData) {
        let Ok(other) = other.decode::<JavaScriptDomainData>() else { return };
        let owned: BTreeSet<&str> = docnames.iter().map(String::as_str).collect();
        let mut data = self.data.write();
        for (name, entry) in other.objects {
            if owned.contains(entry.docname.as_str()) {
                data.objects.insert(name, entry);
            }
        }
        for (name, entry) in other.modules {
            if owned.contains(entry.docname.as_str()) {
                data.modules.insert(name, entry);
            }
        }
    }
    fn process_doc(&mut self, ctx: &DomainContext<'_>, _docname: &str, document: &Document) -> DomainResult<()> {
        for id in document.preorder_ids() {
            let Ok(node) = document.get(id) else { continue };
            // The first module's `ismod` target may have been folded into its
            // section by `MoveModuleTargets`; its sibling `index` node still
            // carries `js:module`, so note modules from either carrier.
            if matches!(node.kind, NodeKind::Target | NodeKind::Index)
                && node.get_str("js:module").ok().flatten().is_some()
            {
                let name = node.get_str("js:module").ok().flatten().unwrap_or("").to_owned();
                let anchor = format!("module-{}", name);
                self.note_module(ctx, &name, &anchor);
                continue;
            }
            if node.kind != NodeKind::Desc || node.get_str("domain").ok().flatten() != Some("js") {
                continue;
            }
            let Some(objtype) = node.get_str("objtype").ok().flatten().map(str::to_owned) else { continue };
            if !DIRECTIVES.contains(&objtype.as_str()) {
                continue;
            }
            if node.attrs.get_bool("no-index").ok().flatten().unwrap_or(false) {
                continue;
            }
            let Some(fullname) = node.get_str("js:fullname").ok().flatten().map(str::to_owned) else { continue };
            let anchor = node.get_str("js:anchor").ok().flatten().unwrap_or("").to_owned();
            self.note_object(ctx, &fullname, &objtype, &anchor, node.span);
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
        let (module, object) = document.get(node_id).ok().map(|node| {
            (
                node.get_str("js:module").ok().flatten().unwrap_or("").to_owned(),
                node.get_str("js:object").ok().flatten().unwrap_or("").to_owned(),
            )
        }).unwrap_or_default();
        let specific = document.get(node_id).ok()
            .and_then(|node| node.pending_xref())
            .and_then(|xref| xref.refspecific())
            .unwrap_or(false);
        let explicit = document.get(node_id).ok()
            .and_then(|node| node.pending_xref())
            .and_then(|xref| xref.refexplicit())
            .unwrap_or(false);
        if typ == "mod" {
            let modules = self.snapshot().modules;
            if let Some(entry) = modules.get(target) {
                let mut reference = if ctx.docname == entry.docname {
                    ResolvedReference::local(entry.node_id.clone(), Some(target.to_owned()))
                } else {
                    ResolvedReference::uri(format!("{}#{}", ctx.relative_uri(ctx.docname, &entry.docname), entry.node_id), Some(target.to_owned()))
                };
                reference.docname = Some(entry.docname.clone());
                reference.classes = vec!["xref".to_owned(), "js".to_owned(), "js-mod".to_owned()];
                return Ok(Some(reference));
            }
            return Ok(None);
        }
        let matches = self.find_obj(target, typ, &module, &object, specific);
        let Some((name, entry)) = matches.first() else { return Ok(None) };
        let title = if explicit { name.clone() } else { name.rsplit('.').next().unwrap_or(name).to_owned() };
        Ok(Some(self.make_refnode(ctx, entry, title, typ)))
    }
    fn resolve_any_xref(
        &self,
        ctx: &DomainContext<'_>,
        target: &str,
        node_id: NodeId,
        _contnode_id: NodeId,
        document: &Document,
    ) -> DomainResult<Vec<(String, ResolvedReference)>> {
        let (module, object) = document.get(node_id).ok().map(|node| {
            (
                node.get_str("js:module").ok().flatten().unwrap_or("").to_owned(),
                node.get_str("js:object").ok().flatten().unwrap_or("").to_owned(),
            )
        }).unwrap_or_default();
        let mut results = Vec::new();
        for role in ROLES {
            if let Some((name, entry)) = self.find_obj(target, role, &module, &object, true).into_iter().next() {
                let title = name.rsplit('.').next().unwrap_or(&name).to_owned();
                let mut reference = self.make_refnode(ctx, &entry, title, role);
                reference.classes = vec!["xref".to_owned(), "any".to_owned(), "js".to_owned(), format!("js-{role}")];
                results.push((format!("js:{role}"), reference));
            }
        }
        if let Some(modules) = document.get(node_id).ok().and_then(|node| node.get_str("js:module").ok().flatten()) {
            let _ = modules;
        }
        results.dedup_by(|a, b| a.1.refid == b.1.refid && a.1.refuri == b.1.refuri);
        Ok(results)
    }
    fn get_objects(&self) -> Vec<ObjectEntry> {
        let data = self.data.read();
        let mut entries = Vec::new();
        for (name, module) in &data.modules {
            entries.push(ObjectEntry::new(name.clone(), name.clone(), "module", module.docname.clone(), module.node_id.clone(), 0));
        }
        for (name, object) in &data.objects {
            entries.push(ObjectEntry::new(
                name.clone(), name.rsplit('.').next().unwrap_or(name).to_owned(), object.objtype.clone(),
                object.docname.clone(), object.node_id.clone(), 1,
            ));
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

// ----- signature expansion ---------------------------------------------------

/// Mutable JS read-time scope.
#[derive(Clone, Debug, Default)]
pub struct JsScope {
    /// Active module components.
    pub module: Vec<String>,
    /// Active object (class) nesting.
    pub object: Vec<String>,
}

impl JsScope {
    fn module_text(&self) -> String { self.module.join(".") }
    fn object_text(&self) -> String { self.object.join(".") }
    fn fullname(&self, name: &str) -> String {
        self.module.iter().chain(self.object.iter()).map(String::as_str).chain(std::iter::once(name)).collect::<Vec<_>>().join(".")
    }
}

/// Expands every `js:*` skeleton in `document`.
pub fn prepare_document(document: &mut Document) -> DomainResult<()> {
    prepare_children(document, document.root, &JsScope::default())
}

fn prepare_children(document: &mut Document, parent: NodeId, scope: &JsScope) -> DomainResult<()> {
    let children = document.children(parent).map(<[NodeId]>::to_vec).unwrap_or_default();
    prepare_siblings(document, &children, scope)
}

/// Processes a fixed slice of siblings with `scope`.  A `js:module` marker
/// installs a new module scope for the *remaining* siblings and returns, which
/// also handles consecutive module directives in one section correctly.
fn prepare_siblings(document: &mut Document, children: &[NodeId], scope: &JsScope) -> DomainResult<()> {
    for (position, &child) in children.iter().enumerate() {
        if document.get(child).is_err() { continue; }
        // `js:module` emits both an `index` node and an `ismod` target.  The
        // read transform `MoveModuleTargets` folds the first module target into
        // its section and removes it, so the index node is the durable carrier
        // of the module name for the scope pass.  The surviving target carries
        // a `refid` (its ids were propagated to the following block) and still
        // carries `js:module` too, so both are recognised here.
        let module_marker = document.get(child).ok().and_then(|node| {
            if matches!(node.kind, NodeKind::Target | NodeKind::Index)
                && node.get_str("js:module").ok().flatten().is_some()
            {
                node.get_str("js:module").ok().flatten().map(str::to_owned)
            } else { None }
        });
        if let Some(name) = module_marker {
            let mut next = scope.clone();
            next.module = name.split('.').map(str::to_owned).collect();
            return prepare_siblings(document, &children[position + 1..], &next);
        }
        prepare_one(document, child, scope)?;
    }
    Ok(())
}

fn prepare_one(document: &mut Document, child: NodeId, scope: &JsScope) -> DomainResult<()> {
    let objtype = {
        let node = document.get(child).ok().and_then(|node| {
            if node.kind == NodeKind::Desc && node.get_str("domain").ok().flatten() == Some("js") {
                node.get_str("js:signature").ok().flatten()?;
                node.get_str("objtype").ok().flatten().map(str::to_owned)
            } else { None }
        });
        let Some(objtype) = node else {
            annotate_scope(document, child, scope);
            prepare_children(document, child, scope)?;
            return Ok(());
        };
        objtype
    };
    let signature = document.get(child)
        .ok()
        .and_then(|node| node.get_str("js:signature").ok().flatten())
        .unwrap_or("")
        .to_owned();
    let span = document.get(child).ok().and_then(|node| node.span).unwrap_or(Span::new(0, 0));
    let raw_name = signature.trim().trim_start_matches(".. js:").trim().split(|ch: char| ch.is_whitespace() || ch == '(').next().unwrap_or("").to_owned();
    // Upstream `JSObject.handle_signature` splits a qualified member name
    // (`Class.member`) into its object prefix and short name, then keeps the
    // prefix as the active `js:object` ref-context.  This is what lets a
    // short-name xref such as `:js:meth:`member2`` inside `Class.member1`
    // resolve against `Class.member2` even when both methods are declared at
    // module scope (siblings of the class).  Nestable classes keep the whole
    // name and push it onto the object stack below.
    let mut effective = scope.clone();
    let name = if objtype != "class" {
        if let Some((prefix, short)) = raw_name.rsplit_once('.') {
            if !prefix.is_empty() {
                effective.object.extend(prefix.split('.').map(str::to_owned));
            }
            short.to_owned()
        } else {
            raw_name
        }
    } else {
        raw_name
    };
    let fullname = effective.fullname(&name);
    {
        let node = document.get_mut(child)?;
        node.attrs.set_str("js:fullname", &fullname);
        node.attrs.set_str("js:shortname", &name);
        node.attrs.set_str("js:anchor", &fullname);
        node.attrs.set_str("js:module", effective.module_text());
        node.attrs.set_str("js:object", effective.object_text());
        node.attrs.set_list("ids", vec![fullname.clone()]);
    }
    // Populate the sibling index node.
    let index_id = document.get(child).ok().and_then(|node| node.attrs.get_node_id("js:index_id").ok().flatten());
    if let Some(index_id) = index_id {
        let label = match objtype.as_str() {
            "class" => format!("{name}() (class)"),
            "function" if effective.module_text().is_empty() && effective.object_text().is_empty() => format!("{name}() (built-in function)"),
            "function" => format!("{fullname}() ({} method)", effective.object_text()),
            _ => format!("{fullname} ({objtype})"),
        };
        if let Ok(node) = document.get_mut(index_id) {
            node.attrs.set_str("entries", format!("['single', '{label}', '{fullname}', '', None]"));
        }
    }
    build_signature(document, child, &name, &objtype, &effective, span)?;
    annotate_scope(document, child, &effective);

    let mut inner = effective.clone();
    if objtype == "class" {
        inner.object.push(name);
    }
    prepare_children(document, child, &inner)
}

fn build_signature(document: &mut Document, desc: NodeId, name: &str, objtype: &str, scope: &JsScope, span: Span) -> DomainResult<()> {
    let signature_id = document.get(desc).ok().and_then(|node| node.children.iter().copied().find(|&id| {
        document.get(id).is_ok_and(|child| child.kind == NodeKind::DescSignature)
    })).unwrap_or(NodeId::NULL);
    if signature_id == NodeId::NULL { return Ok(()); }
    let source = document.get(desc).ok().and_then(|node| node.source.clone());
    let mut builder = SigBuilder::new_with_source(document, span, "js", source);
    if let Ok(node) = builder.doc.get_mut(signature_id) {
        node.children.clear();
        node.attrs.set_list("classes", vec!["sig".to_owned(), "sig-object".to_owned(), "js".to_owned()]);
        node.attrs.set_str("fullname", name);
        let module = if scope.module_text().is_empty() { "True".to_owned() } else { scope.module_text() };
        let object = if scope.object_text().is_empty() { "True".to_owned() } else { scope.object_text() };
        node.attrs.set_str("module", module);
        node.attrs.set_str("object", object);
    }
    if objtype == "class" {
        let annotation = builder.add(NodeKind::DescAnnotation, signature_id)?;
        if let Ok(node) = builder.doc.get_mut(annotation) {
            node.attrs.set_str("xml:space", "preserve");
        }
        builder.sig(NodeKind::DescSigKeyword, "class", "k", annotation)?;
        builder.sig(NodeKind::DescSigSpace, " ", "w", annotation)?;
    }
    // Upstream `JSObject.handle_signature` renders `actual_prefix = prefix or mod_name`:
    // the object (class) prefix wins over the module path.  Only when there is no
    // object prefix does the module qualify the name (e.g. a top-level class).
    let qualifiers: Vec<String> = if !scope.object.is_empty() {
        scope.object.clone()
    } else {
        scope.module.clone()
    };
    if !qualifiers.is_empty() {
        let addname = builder.add(NodeKind::DescAddname, signature_id)?;
        builder.classes(addname, &["sig-prename", "descclassname"])?;
        if let Ok(node) = builder.doc.get_mut(addname) { node.attrs.set_str("xml:space", "preserve"); }
        for qualifier in qualifiers {
            builder.sig(NodeKind::DescSigName, &qualifier, "n", addname)?;
            builder.sig(NodeKind::DescSigPunctuation, ".", "p", addname)?;
        }
    }
    let name_node = builder.add(NodeKind::DescName, signature_id)?;
    builder.classes(name_node, &["sig-name", "descname"])?;
    if let Ok(node) = builder.doc.get_mut(name_node) { node.attrs.set_str("xml:space", "preserve"); }
    builder.sig(NodeKind::DescSigName, name, "n", name_node)?;
    let params = builder.add(NodeKind::DescParameterlist, signature_id)?;
    if let Ok(node) = builder.doc.get_mut(params) { node.attrs.set_str("xml:space", "preserve"); }
    Ok(())
}

/// Annotates every pending xref under `node` with the active JS module/object
/// scope (upstream's `env.ref_context`).
fn annotate_scope(document: &mut Document, node: NodeId, scope: &JsScope) {
    if let Ok(current) = document.get(node) {
        if current.kind == NodeKind::PendingXref {
            if let Ok(mutable) = document.get_mut(node) {
                mutable.attrs.set_str("js:module", scope.module_text());
                mutable.attrs.set_str("js:object", scope.object_text());
            }
        }
    }
    let children = document.get(node).map(|node| node.children.clone()).unwrap_or_default();
    for child in children {
        annotate_scope(document, child, scope);
    }
}

/// Generates the anchor of a JS module (`module-<name>`).
pub fn module_anchor(name: &str) -> String {
    format!("module-{name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_matches_upstream() {
        assert_eq!(object_types().len(), 6);
        assert_eq!(objtypes_for_role("func"), Some(vec!["function"]));
        assert_eq!(module_anchor("module_a.submodule"), "module-module_a.submodule");
    }
}
