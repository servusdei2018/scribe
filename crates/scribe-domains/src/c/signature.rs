//! Expands the RST-layer `c:*` skeleton descriptions into the frozen
//! `desc_*` / `desc_sig_*` signature tree (Phase 13).
//!
//! The RST crate emits lightweight `desc` nodes carrying the raw signature and
//! options (ARCHITECTURE.md §15.1 keeps `scribe-rst` independent of this crate).
//! This module performs the parse and materialization, mirroring upstream's
//! `CDomain` object directives and `Symbol` nesting.

use serde::{Deserialize, Serialize};
use scribe_ast::{Document, NodeId, NodeKind, Span};

use super::parser::{parameter_names, parse_declaration, parse_expression, Decl};
use crate::error::DomainResult;
use crate::sigbuild::{SigBuilder, SigToken};

/// Options for a C object directive (the JSON payload of `c:options`).
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct COptions {
    /// `:no-index:` / `:noindex:`.
    pub no_index: bool,
    /// `:no-index-entry:` / `:noindexentry:`.
    pub no_index_entry: bool,
}

/// Mutable read-time scope while walking a document.
#[derive(Clone, Debug, Default)]
pub struct Scope {
    /// Active `c:namespace` stack (empty at global scope).  `namespace` resets
    /// it, `namespace-push` appends and `namespace-pop` removes the last entry.
    namespace: Vec<String>,
    /// Nested struct/union/enum scope, outermost first.
    nested: Vec<String>,
}

impl Scope {
    fn qualified(&self, name: &str) -> String {
        let mut parts: Vec<&str> = self.namespace.iter().map(String::as_str).collect();
        parts.extend(self.nested.iter().map(String::as_str));
        if !name.is_empty() {
            parts.push(name);
        }
        parts.join(".")
    }

    fn namespace_text(&self) -> String {
        self.namespace.join(".")
    }

    fn anchor(&self, name: &str) -> String {
        format!("c.{}", self.qualified(name))
    }
}

/// Upstream treats these `c:namespace` arguments as "global namespace".
fn is_null_namespace(name: &str) -> bool {
    matches!(name.trim(), "NULL" | "0" | "nullptr")
}

/// Information materialized for one skeleton declaration.
#[derive(Clone, Debug, Default)]
pub struct CSignatureInfo {
    /// Fully qualified name (namespace + nested scope + declared name).
    pub fullname: String,
    /// Anchor id generated for this object.
    pub anchor: String,
    /// Declared short name.
    pub shortname: String,
    /// Function parameter names.
    pub param_names: Vec<String>,
}

/// True when `node` is an unexpanded `c:*` skeleton.
pub fn is_skeleton(doc: &Document, id: NodeId) -> Option<String> {
    let node = doc.get(id).ok()?;
    if node.kind != NodeKind::Desc || node.get_str("domain").ok().flatten() != Some("c") {
        return None;
    }
    node.get_str("c:signature").ok().flatten()?;
    node.get_str("objtype").ok().flatten().map(str::to_owned)
}

/// Expands every C skeleton in `document`.
pub fn prepare_document(document: &mut Document) -> DomainResult<()> {
    let root = document.root;
    let scope = Scope::default();
    prepare_children(document, root, &scope)
}

/// Expands `c:expr` / `c:texpr` skeleton `desc_inline` nodes emitted by the RST
/// layer into their `desc_sig_*` token trees (upstream `CExprRole.run`).
fn expand_expr_inlines(document: &mut Document, node: NodeId) -> DomainResult<()> {
    let is_expr = document.get(node).is_ok_and(|n| {
        n.kind == NodeKind::DescInline && n.get_str("c:expr_text").ok().flatten().is_some()
    });
    if is_expr {
        let (text, source, span) = {
            let n = document.get(node)?;
            (
                n.get_str("c:expr_text").ok().flatten().unwrap_or("").to_owned(),
                n.source.clone(),
                n.span.unwrap_or(Span::new(0, 0)),
            )
        };
        let tokens = parse_expression(&text);
        if let Ok(n) = document.get_mut(node) {
            n.children.clear();
        }
        let mut builder = SigBuilder::new_with_source(document, span, "c", source);
        builder.emit_all(&tokens, node)?;
    }
    let children = document.get(node).map(|n| n.children.clone()).unwrap_or_default();
    for child in children {
        expand_expr_inlines(document, child)?;
    }
    Ok(())
}

/// Rewrites the typed `:param:` fields of a C object into upstream's grouped
/// `Parameters` field (upstream `TypedField` with `typerolename='expr'`).
fn rewrite_fields(document: &mut Document, desc: NodeId) -> DomainResult<()> {
    let content = document.get(desc).ok().and_then(|node| {
        node.children.iter().copied().find(|&id| {
            document.get(id).is_ok_and(|child| child.kind == NodeKind::DescContent)
        })
    });
    let Some(content) = content else { return Ok(()) };
    let fieldlists: Vec<NodeId> = document.get(content).ok().map(|node| {
        node.children.iter().copied().filter(|&id| {
            document.get(id).is_ok_and(|child| child.kind == NodeKind::FieldList)
        }).collect()
    }).unwrap_or_default();
    for fieldlist in fieldlists {
        rewrite_fieldlist(document, fieldlist)?;
    }
    Ok(())
}

fn field_name_text(doc: &Document, field: NodeId) -> String {
    let Ok(node) = doc.get(field) else { return String::new() };
    for &child in &node.children {
        if let Ok(name) = doc.get(child) {
            if name.kind == NodeKind::FieldName {
                return name.astext(doc).trim().to_owned();
            }
        }
    }
    String::new()
}

fn rewrite_fieldlist(document: &mut Document, fieldlist: NodeId) -> DomainResult<()> {
    let fields: Vec<NodeId> = document.get(fieldlist).ok().map(|n| n.children.to_vec()).unwrap_or_default();
    let mut params: Vec<(String, String)> = Vec::new(); // (name, type)
    let mut others: Vec<NodeId> = Vec::new();
    for field in fields {
        let label = field_name_text(document, field);
        let head = label.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
        if matches!(head.as_str(), "param" | "parameter" | "arg" | "argument") {
            let arg = label.split_once(' ').map_or("", |(_, value)| value.trim());
            let (ty, name) = match arg.rsplit_once(char::is_whitespace) {
                Some((ty, name)) => (ty.trim().to_owned(), name.trim().to_owned()),
                None => (String::new(), arg.to_owned()),
            };
            params.push((name, ty));
            let _ = document.remove_node(field);
        } else {
            // Upstream relabels the bare `rtype` field as "Return type".
            if head == "rtype" {
                if let Some(name_id) = document.get(field).ok().and_then(|f| {
                    f.children.iter().copied().find(|&id| {
                        document.get(id).is_ok_and(|n| n.kind == NodeKind::FieldName)
                    })
                }) {
                    if let Ok(node) = document.get_mut(name_id) {
                        node.children.clear();
                    }
                    let _ = document.add_node(scribe_ast::Node::text("Return type"), Some(name_id));
                }
            }
            others.push(field);
        }
    }
    if params.is_empty() {
        return Ok(());
    }
    let span = document.get(fieldlist).ok().and_then(|n| n.span).unwrap_or(Span::new(0, 0));
    let source = document.get(fieldlist).ok().and_then(|n| n.source.clone());
    let mut rebuilt = Vec::new();
    {
        let mut builder = SigBuilder::new_with_source(document, span, "c", source);
        let field = builder.add(NodeKind::Field, fieldlist)?;
        let name_node = builder.add(NodeKind::FieldName, field)?;
        builder.text("Parameters", name_node)?;
        let body = builder.add(NodeKind::FieldBody, field)?;
        let bullets = builder.add(NodeKind::BulletList, body)?;
        for (param, ty) in &params {
            let item = builder.add(NodeKind::ListItem, bullets)?;
            let para = builder.add(NodeKind::Paragraph, item)?;
            let strong = builder.add(NodeKind::LiteralStrong, para)?;
            builder.text(param, strong)?;
            builder.text(" (", para)?;
            let inline = builder.add(NodeKind::Inline, para)?;
            let desc_inline = builder.add(NodeKind::DescInline, inline)?;
            if let Ok(node) = builder.doc.get_mut(desc_inline) {
                node.attrs.set_str("domain", "c");
                node.attrs.set_list("classes", vec![
                    "c-expr".to_owned(), "sig".to_owned(), "sig-inline".to_owned(), "c".to_owned(),
                ]);
            }
            let tokens = parse_expression(ty);
            builder.emit_all(&tokens, desc_inline)?;
            builder.text(")", para)?;
        }
        rebuilt.push(field);
    }
    rebuilt.extend(others);
    if let Ok(node) = document.get_mut(fieldlist) {
        node.children = rebuilt;
    }
    Ok(())
}

fn prepare_children(document: &mut Document, parent: NodeId, scope: &Scope) -> DomainResult<()> {
    let children = document.children(parent).map(<[NodeId]>::to_vec).unwrap_or_default();
    prepare_siblings(document, &children, scope)
}

/// Processes a fixed slice of siblings.  A namespace marker installs its scope
/// on the *remaining* siblings only, so earlier declarations are never
/// re-expanded with a later namespace's scope.
fn prepare_siblings(document: &mut Document, children: &[NodeId], scope: &Scope) -> DomainResult<()> {
    for (position, &child) in children.iter().enumerate() {
        if document.get(child).is_err() {
            continue;
        }
        // Expand C expression roles and annotate all user xrefs under this
        // node with the read-time namespace scope (upstream's
        // `env.ref_context['c:parent_key']`).
        expand_expr_inlines(document, child)?;
        annotate_scope(document, child, &scope.qualified(""));
        // All three namespace directives emit a `target` marker carrying a
        // `c:namespace*` attribute.  Upstream returns no nodes for any of them,
        // so strip the marker and install the new scope for following siblings.
        let marker = document.get(child).ok().and_then(|node| {
            if node.kind != NodeKind::Target {
                return None;
            }
            let kind = node.get_str("objtype").ok().flatten().map(str::to_owned)?;
            if !matches!(kind.as_str(), "namespace" | "namespace-push" | "namespace-pop") {
                return None;
            }
            // Only C namespace markers belong to this pass. The C++ domain
            // uses the same `objtype` values for its own markers; stripping
            // those here would silently discard the C++ read-time scope.
            let attr = match kind.as_str() {
                "namespace" => "c:namespace",
                "namespace-push" => "c:namespace-push",
                "namespace-pop" => "c:namespace-pop",
                _ => return None,
            };
            if kind != "namespace-pop" && node.get_str(attr).ok().flatten().is_none() {
                return None;
            }
            let name = if kind == "namespace-pop" {
                ""
            } else {
                node.get_str(attr).ok().flatten().unwrap_or("")
            };
            Some((kind, name.to_owned()))
        });
        if let Some((kind, name)) = marker {
            let mut next = scope.clone();
            match kind.as_str() {
                "namespace" => {
                    next.namespace.clear();
                    if !is_null_namespace(&name) && !name.is_empty() {
                        next.namespace.push(name);
                    }
                }
                "namespace-push" => {
                    if !is_null_namespace(&name) && !name.is_empty() {
                        next.namespace.push(name);
                    }
                }
                "namespace-pop" => {
                    let _ = next.namespace.pop();
                }
                _ => {}
            }
            let _ = document.remove_node(child);
            return prepare_siblings(document, &children[position + 1..], &next);
        }

        let Some(objtype) = is_skeleton(document, child) else {
            prepare_children(document, child, scope)?;
            continue;
        };
        let (signature, options) = {
            let node = document.get(child)?;
            (
                node.get_str("c:signature").ok().flatten().unwrap_or("").to_owned(),
                parse_options(node.get_str("c:options").ok().flatten()),
            )
        };
        let span = document.get(child).ok().and_then(|node| node.span).unwrap_or(Span::new(0, 0));
        let info = build_signature(document, child, &signature, &objtype, &options, scope, span)?;
        rewrite_fields(document, child)?;
        expand_expr_inlines(document, child)?;
        annotate_scope(document, child, &scope.qualified(""));

        // Struct/union/enum introduce a nested scope for their content; a
        // function's parameters live in the function's own scope.
        let mut inner = scope.clone();
        if matches!(objtype.as_str(), "struct" | "union" | "enum" | "enum-class" | "enum-struct" | "class" | "function" | "macro") {
            inner.nested.push(info.shortname.clone());
        }
        if let Some(content) = document.get(child).ok().and_then(|node| {
            node.children.iter().copied().find(|&id| {
                document.get(id).is_ok_and(|n| n.kind == NodeKind::DescContent)
            })
        }) {
            annotate_scope(document, content, &inner.qualified(""));
        }
        prepare_children(document, child, &inner)?;
    }
    Ok(())
}

fn parse_options(json: Option<&str>) -> COptions {
    json.and_then(|raw| serde_json::from_str(raw).ok()).unwrap_or_default()
}

/// Builds a `desc_signature` for one declaration and annotates its owning
/// `desc` node with the object's full name/anchor.
pub fn build_signature(
    document: &mut Document,
    desc: NodeId,
    signature: &str,
    objtype: &str,
    options: &COptions,
    scope: &Scope,
    span: Span,
) -> DomainResult<CSignatureInfo> {
    let decl = parse_declaration(signature, objtype, "c");
    let fullname = scope.qualified(&decl.name);
    let anchor = scope.anchor(&decl.name);
    let param_names = parameter_names(&decl);

    {
        let node = document.get_mut(desc)?;
        node.attrs.set_str("c:fullname", &fullname);
        node.attrs.set_str("c:shortname", decl.short_name());
        node.attrs.set_str("c:anchor", &anchor);
        node.attrs.set_str("c:namespace", scope.namespace_text());
        node.attrs.set_str("c:param_names", param_names.join("\u{1f}"));
        node.attrs.set_str("domain", "c");
        node.attrs.set_bool("no-index", options.no_index);
        let ids = if options.no_index { Vec::new() } else { vec![anchor.clone()] };
        node.attrs.set_list("ids", ids);
        node.attrs.set_list("classes", vec!["c".to_owned(), objtype.to_owned()]);
    }

    // Populate the sibling index node (upstream `add_target_and_index`).
    let index_id = document.get(desc).ok().and_then(|node| node.attrs.get_node_id("c:index_id").ok().flatten());
    if let Some(index_id) = index_id {
        if let Ok(node) = document.get_mut(index_id) {
            if options.no_index_entry {
                node.attrs.set_str("entries", "[]");
            } else {
                let label = format!("{} ({})", fullname, object_label(objtype));
                node.attrs.set_str("entries", format!("['single', '{label}', '{anchor}', '', None]"));
            }
        }
    }

    // The RST skeleton keeps an empty placeholder `desc_signature` as the
    // first child of `desc`; expand it in place so held arena ids stay valid.
    let signature_id = document
        .get(desc)
        .ok()
        .and_then(|node| node.children.iter().copied().find(|&id| {
            document.get(id).is_ok_and(|child| child.kind == NodeKind::DescSignature)
        }))
        .unwrap_or(NodeId::NULL);
    if signature_id != NodeId::NULL {
        build_signature_into(document, signature_id, &decl, &anchor, objtype, scope, span)?;
    }

    Ok(CSignatureInfo {
        fullname,
        anchor,
        shortname: decl.short_name().to_owned(),
        param_names,
    })
}

/// Object label used in index entries (`C function`, `C struct`, ...).
pub fn object_label(objtype: &str) -> String {
    let name = match objtype {
        "member" => "member",
        "var" => "var",
        "function" => "function",
        "macro" => "macro",
        "struct" => "struct",
        "union" => "union",
        "enum" | "enum-class" | "enum-struct" => "enum",
        "enumerator" => "enumerator",
        "type" => "type",
        "alias" => "alias",
        other => other,
    };
    format!("C {name}")
}

fn build_signature_into(
    document: &mut Document,
    signature_id: NodeId,
    decl: &Decl,
    anchor: &str,
    objtype: &str,
    scope: &Scope,
    span: Span,
) -> DomainResult<()> {
    let source = document.get(signature_id).ok().and_then(|node| node.source.clone());
    let mut builder = SigBuilder::new_with_source(document, span, "c", source);
    // Reset any previous children of the placeholder signature.
    if let Ok(node) = builder.doc.get_mut(signature_id) {
        node.children.clear();
        node.attrs.set_list("ids", vec![anchor.to_owned()]);
        node.attrs.set_list("classes", vec!["sig".to_owned(), "sig-object".to_owned(), "c".to_owned()]);
        // Upstream stamps `_toc_name` on indexable object signatures; the
        // TocTreeCollector uses it for the local ToC `literal()` label. The
        // C domain wraps `desc_name` inside `desc_signature_line`, so the
        // generic `desc_name` fallback in toctree expansion cannot see it.
        node.attrs.set_str("_toc_name", decl.short_name());
    }
    let line = builder.add(NodeKind::DescSignatureLine, signature_id)?;
    if let Ok(node) = builder.doc.get_mut(line) {
        node.attrs.set_bool("add_permalink", true);
        node.attrs.set_str("xml:space", "preserve");
    }

    let prefix = decl.prefix.clone();
    builder.emit_all(&prefix, line)?;
    // Upstream inserts a space between the type specifier and the declarator
    // unless the declarator begins with a pointer marker.
    if let Some(last) = prefix.last() {
        let pointer = matches!(last, SigToken::Punctuation(text) if text == "*" || text == "&");
        if !pointer && !matches!(last, SigToken::Space) {
            builder.sig(NodeKind::DescSigSpace, " ", "w", line)?;
        }
    }

    // Nested-name qualifiers → `desc_addname` entries with reference targets.
    let count = decl.segments.len();
    for (index, segment) in decl.segments.iter().enumerate() {
        if index + 1 == count {
            // Final declared name.
            let name_node = builder.add(NodeKind::DescName, line)?;
            builder.classes(name_node, &["sig-name", "descname"])?;
            if let Ok(node) = builder.doc.get_mut(name_node) {
                node.attrs.set_str("xml:space", "preserve");
            }
            // Upstream renders anonymous declarators (`@data`) as `[anonymous]`.
            let display = if segment.text.starts_with('@') { "[anonymous]" } else { &segment.text };
            let name = builder.sig(NodeKind::DescSigName, display, "n", name_node)?;
            let _ = name;
        } else {
            let addname = builder.add(NodeKind::DescAddname, line)?;
            builder.classes(addname, &["sig-prename", "descclassname"])?;
            if let Ok(node) = builder.doc.get_mut(addname) {
                node.attrs.set_str("xml:space", "preserve");
            }
            let token = SigToken::reference("c", segment.text.clone(), "identifier", "c-identifier");
            builder.emit(&token, addname)?;
            if !segment.sep_after.is_empty() {
                builder.sig(NodeKind::DescSigPunctuation, &segment.sep_after, "p", addname)?;
            }
        }
    }

    // Function / macro parameter list.
    if !decl.params.is_empty() && (decl.is_function || objtype == "macro") {
        let params = builder.add(NodeKind::DescParameterlist, line)?;
        if let Ok(node) = builder.doc.get_mut(params) {
            node.attrs.set_bool("multi_line_parameter_list", false);
            node.attrs.set_str("xml:space", "preserve");
        }
        for param in &decl.params {
            let tokens = param.clone();
            let parameter = builder.add(NodeKind::DescParameter, params)?;
            if let Ok(node) = builder.doc.get_mut(parameter) {
                node.attrs.set_bool("noemph", true);
                node.attrs.set_str("xml:space", "preserve");
            }
            builder.emit_all(&tokens, parameter)?;
        }
    }
    builder.emit_all(&decl.suffix, line)?;

    // Annotate every signature xref with the read scope; the resolver uses it
    // for namespace/nested-scope aware lookup.
    let scope_text = scope.qualified("");
    annotate_scope(builder.doc, signature_id, &scope_text);
    Ok(())
}

/// Recursively sets `c:scope` on every pending xref under `node`.
fn annotate_scope(doc: &mut Document, node: NodeId, scope: &str) {
    if let Ok(current) = doc.get(node) {
        if current.kind == NodeKind::PendingXref {
            if let Ok(mutable) = doc.get_mut(node) {
                mutable.attrs.set_str("c:scope", scope);
            }
        }
    }
    let children = doc.get(node).map(|node| node.children.clone()).unwrap_or_default();
    for child in children {
        annotate_scope(doc, child, scope);
    }
}
