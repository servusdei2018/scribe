//! Expands the RST-layer `cpp:*` skeleton descriptions into frozen
//! `desc_*` / `desc_sig_*` nodes (Phase 13).

use serde::{Deserialize, Serialize};
use scribe_ast::{Document, NodeId, NodeKind, Span};

use super::ids::cpp_ids;
use super::parser::{mangle_input, parse_cpp_declaration, CppDecl};
use crate::error::DomainResult;
use crate::sigbuild::{SigBuilder, SigToken};

/// Options for a C++ object directive.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct CppOptions {
    /// `:no-index:` / `:noindex:`.
    pub no_index: bool,
    /// `:no-index-entry:` / `:noindexentry:`.
    pub no_index_entry: bool,
}

/// Mutable read-time C++ scope.
#[derive(Clone, Debug, Default)]
pub struct CppScope {
    /// Active `cpp:namespace`.
    pub namespace: String,
    /// Nested class/struct/union/enum scope.
    pub nested: Vec<String>,
}

impl CppScope {
    /// Fully qualified name in the active scope.
    pub fn qualified(&self, name: &str) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if !self.namespace.is_empty() {
            parts.push(&self.namespace);
        }
        parts.extend(self.nested.iter().map(String::as_str));
        if !name.is_empty() {
            parts.push(name);
        }
        parts.join("::")
    }
}

/// Materialized information for one C++ declaration.
#[derive(Clone, Debug, Default)]
pub struct CppSignatureInfo {
    /// Canonical fully-qualified name.
    pub fullname: String,
    /// Primary anchor id.
    pub anchor: String,
    /// All generated ids (in upstream order).
    pub ids: Vec<String>,
    /// Declared short name.
    pub shortname: String,
}

/// True when `node` is an unexpanded `cpp:*` skeleton.
pub fn is_skeleton(doc: &Document, id: NodeId) -> Option<String> {
    let node = doc.get(id).ok()?;
    if node.kind != NodeKind::Desc || node.get_str("domain").ok().flatten() != Some("cpp") {
        return None;
    }
    node.get_str("cpp:signature").ok().flatten()?;
    node.get_str("objtype").ok().flatten().map(str::to_owned)
}

/// Expands every C++ skeleton in `document`.
pub fn prepare_document(document: &mut Document) -> DomainResult<()> {
    prepare_children(document, document.root, &CppScope::default())
}

fn prepare_children(document: &mut Document, parent: NodeId, scope: &CppScope) -> DomainResult<()> {
    let children = document.children(parent).map(<[NodeId]>::to_vec).unwrap_or_default();
    for child in children {
        if document.get(child).is_err() {
            continue;
        }
        // Upstream's `env.ref_context['cpp:parent_key']` is installed while
        // reading object directives, so xrefs in a description's content are
        // resolved relative to the enclosing namespace/class scope. Annotate
        // every pending xref under this child with the current scope before
        // expanding it (the C domain does the same for `c:parent_key`).
        annotate_scope(document, child, &scope.qualified(""));
        let marker = document.get(child).ok().and_then(|node| {
            if node.kind != NodeKind::Target {
                return None;
            }
            let kind = node.get_str("objtype").ok().flatten().map(str::to_owned)?;
            if !matches!(kind.as_str(), "namespace" | "namespace-push" | "namespace-pop") {
                return None;
            }
            let attr = match kind.as_str() {
                "namespace" => "cpp:namespace",
                "namespace-push" => "cpp:namespace-push",
                "namespace-pop" => "cpp:namespace-pop",
                _ => return None,
            };
            if kind != "namespace-pop" && node.get_str(attr).ok().flatten().is_none() {
                return None;
            }
            let name = node.get_str(attr).ok().flatten().unwrap_or("").to_owned();
            Some((kind, name))
        });
        if let Some((kind, name)) = marker {
            let mut next = scope.clone();
            match kind.as_str() {
                "namespace" => next.namespace = name,
                "namespace-push" => {
                    next.namespace = if next.namespace.is_empty() { name } else { format!("{}::{}", next.namespace, name) };
                }
                "namespace-pop" => {
                    next.namespace = next.namespace.rsplit_once("::").map_or(String::new(), |(head, _)| head.to_owned());
                }
                _ => {}
            }
            let _ = document.remove_node(child);
            prepare_children(document, parent, &next)?;
            return Ok(());
        }
        let Some(objtype) = is_skeleton(document, child) else {
            prepare_children(document, child, scope)?;
            continue;
        };
        let (signature, options) = {
            let node = document.get(child)?;
            (
                node.get_str("cpp:signature").ok().flatten().unwrap_or("").to_owned(),
                parse_options(node.get_str("cpp:options").ok().flatten()),
            )
        };
        let span = document.get(child).ok().and_then(|node| node.span).unwrap_or(Span::new(0, 0));
        let info = build_signature(document, child, &signature, &objtype, &options, scope, span)?;
        let mut inner = scope.clone();
        // Upstream's `cpp:parent_key` for a description's content is the object
        // symbol itself, so nested xrefs resolve relative to the declared name
        // for every object type (functions included).
        if !info.shortname.is_empty() {
            inner.nested.push(info.shortname.clone());
        }
        prepare_children(document, child, &inner)?;
    }
    Ok(())
}

fn parse_options(json: Option<&str>) -> CppOptions {
    json.and_then(|raw| serde_json::from_str(raw).ok()).unwrap_or_default()
}

/// Builds the signature for one C++ declaration.
pub fn build_signature(
    document: &mut Document,
    desc: NodeId,
    signature: &str,
    objtype: &str,
    options: &CppOptions,
    scope: &CppScope,
    span: Span,
) -> DomainResult<CppSignatureInfo> {
    let decl = parse_cpp_declaration(signature, objtype, "cpp");
    let fullname = scope.qualified(&decl.name);
    let mut mangle = mangle_input(&decl);
    if mangle.names.is_empty() {
        mangle.names.push(decl.name.clone());
    }
    let ids = cpp_ids(&mangle);
    let anchor = ids.first().cloned().unwrap_or_else(|| decl.name.clone());

    {
        let node = document.get_mut(desc)?;
        node.attrs.set_str("cpp:fullname", &fullname);
        node.attrs.set_str("cpp:shortname", decl.short_name());
        node.attrs.set_str("cpp:anchor", &anchor);
        node.attrs.set_str("cpp:ids", ids.join(" "));
        node.attrs.set_str("cpp:namespace", &scope.namespace);
        node.attrs.set_str("cpp:declaration", signature.trim());
        node.attrs.set_str("domain", "cpp");
        node.attrs.set_bool("no-index", options.no_index);
        node.attrs.set_list("ids", if options.no_index { Vec::new() } else { ids.clone() });
        node.attrs.set_list("classes", vec!["cpp".to_owned(), objtype.to_owned()]);
    }

    let index_id = document.get(desc).ok().and_then(|node| node.attrs.get_node_id("cpp:index_id").ok().flatten());
    if let Some(index_id) = index_id {
        if let Ok(node) = document.get_mut(index_id) {
            if options.no_index_entry {
                node.attrs.set_str("entries", "[]");
            } else {
                let label = format!("{} (C++ {})", fullname, label_for(objtype));
                node.attrs.set_str("entries", format!("['single', '{label}', '{anchor}', '', None]"));
            }
        }
    }

    let signature_ids: Vec<NodeId> = document
        .get(desc)
        .ok()
        .map(|node| {
            node.children.iter().copied().filter(|&id| {
                document.get(id).is_ok_and(|child| child.kind == NodeKind::DescSignature)
            }).collect()
        })
        .unwrap_or_default();
    let signatures: Vec<String> = document
        .get(desc)
        .ok()
        .and_then(|node| node.get_str("cpp:signatures").ok().flatten())
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_else(|| vec![signature.to_owned()]);
    let mut overloads: Vec<(String, String, String)> = Vec::new();
    let mut all_template_params = Vec::new();
    let mut all_function_params = Vec::new();
    for (index, &signature_id) in signature_ids.iter().enumerate() {
        let text = signatures.get(index).map_or(signature, String::as_str);
        let one = parse_cpp_declaration(text, objtype, "cpp");
        for name in &one.template_params {
            if !all_template_params.contains(name) {
                all_template_params.push(name.clone());
            }
        }
        for name in &one.function_params {
            if !all_function_params.contains(name) {
                all_function_params.push(name.clone());
            }
        }
        build_signature_into(document, signature_id, &one, objtype, scope, span)?;
        let mut mangle = mangle_input(&one);
        if mangle.names.is_empty() {
            mangle.names.push(one.name.clone());
        }
        let overload_ids = cpp_ids(&mangle);
        overloads.push((
            scope.qualified(&one.name),
            overload_ids.first().cloned().unwrap_or_else(|| one.name.clone()),
            text.trim().to_owned(),
        ));
    }
    if let Ok(node) = document.get_mut(desc) {
        node.attrs.set_str("cpp:overloads", serde_json::to_string(&overloads).unwrap_or_default());
        node.attrs.set_str("cpp:template_params", serde_json::to_string(&all_template_params).unwrap_or_default());
        node.attrs.set_str("cpp:function_params", serde_json::to_string(&all_function_params).unwrap_or_default());
    }

    Ok(CppSignatureInfo {
        fullname,
        anchor,
        ids,
        shortname: decl.short_name().to_owned(),
    })
}

fn label_for(objtype: &str) -> String {
    let label = match objtype {
        "member" | "var" => "member",
        "function" => "function",
        "class" | "struct" => "class",
        "union" => "union",
        "enum" | "enum-class" | "enum-struct" => "enum",
        "enumerator" => "enumerator",
        "type" => "type",
        "concept" => "concept",
        other => other,
    };
    label.to_owned()
}

fn build_signature_into(
    document: &mut Document,
    signature_id: NodeId,
    decl: &CppDecl,
    objtype: &str,
    scope: &CppScope,
    span: Span,
) -> DomainResult<()> {
    let source = document.get(signature_id).ok().and_then(|node| node.source.clone());
    let mut builder = SigBuilder::new_with_source(document, span, "cpp", source);
    if let Ok(node) = builder.doc.get_mut(signature_id) {
        node.children.clear();
        node.attrs.set_list("classes", vec!["sig".to_owned(), "sig-object".to_owned(), "cpp".to_owned()]);
    }
    // Upstream renders a leading `template<...>` header as its own signature
    // line, with each introduced parameter as a `desc_name` node.
    if !decl.template_params.is_empty() {
        let tline = builder.add(NodeKind::DescSignatureLine, signature_id)?;
        if let Ok(node) = builder.doc.get_mut(tline) {
            node.attrs.set_str("xml:space", "preserve");
        }
        builder.sig(NodeKind::DescSigKeyword, "template", "k", tline)?;
        builder.sig(NodeKind::DescSigPunctuation, "<", "p", tline)?;
        for (index, name) in decl.template_params.iter().enumerate() {
            if index > 0 {
                builder.sig(NodeKind::DescSigPunctuation, ",", "p", tline)?;
                builder.sig(NodeKind::DescSigSpace, " ", "w", tline)?;
            }
            if let Some(tokens) = decl.template_param_tokens.get(index) {
                let keep = tokens.iter().rposition(|token| match token {
                    SigToken::Name(text) => text == name,
                    SigToken::Ref { text, .. } => text == name,
                    _ => false,
                }).unwrap_or(tokens.len());
                builder.emit_all(&tokens[..keep], tline)?;
                if keep > 0 {
                    builder.sig(NodeKind::DescSigSpace, " ", "w", tline)?;
                }
            }
            let name_node = builder.add(NodeKind::DescName, tline)?;
            builder.classes(name_node, &["sig-name", "descname"])?;
            if let Ok(node) = builder.doc.get_mut(name_node) {
                node.attrs.set_str("xml:space", "preserve");
            }
            builder.sig(NodeKind::DescSigName, name, "n", name_node)?;
        }
        builder.sig(NodeKind::DescSigPunctuation, ">", "p", tline)?;
    }

    let line = builder.add(NodeKind::DescSignatureLine, signature_id)?;
    if let Ok(node) = builder.doc.get_mut(line) {
        node.attrs.set_bool("add_permalink", true);
        node.attrs.set_str("xml:space", "preserve");
    }
    builder.emit_all(&decl.prefix, line)?;
    if let Some(last) = decl.prefix.last() {
        let pointer = matches!(last, SigToken::Punctuation(text) if text == "*" || text == "&" || text == "&&");
        if !pointer && !matches!(last, SigToken::Space) {
            builder.sig(NodeKind::DescSigSpace, " ", "w", line)?;
        }
    }

    let count = decl.segments.len();
    for (index, segment) in decl.segments.iter().enumerate() {
        if index + 1 == count {
            let name_node = builder.add(NodeKind::DescName, line)?;
            builder.classes(name_node, &["sig-name", "descname"])?;
            if let Ok(node) = builder.doc.get_mut(name_node) {
                node.attrs.set_str("xml:space", "preserve");
            }
            if let Some(operator) = segment.text.strip_prefix("operator") {
                // Upstream splits operator names into a keyword and an operator
                // leaf (`operator` + `()` / `+` / `[]`).
                builder.sig(NodeKind::DescSigKeyword, "operator", "k", name_node)?;
                if !operator.is_empty() {
                    builder.sig(NodeKind::DescSigOperator, operator, "o", name_node)?;
                }
            } else {
                builder.sig(NodeKind::DescSigName, &segment.text, "n", name_node)?;
            }
            // Template arguments on the name (`f<int>`).
            if !decl.template_args.is_empty() {
                builder.sig(NodeKind::DescSigPunctuation, "<", "p", line)?;
                for (arg_index, arg) in decl.template_args.iter().enumerate() {
                    if arg_index > 0 {
                        builder.sig(NodeKind::DescSigPunctuation, ",", "p", line)?;
                        builder.sig(NodeKind::DescSigSpace, " ", "w", line)?;
                    }
                    let token = SigToken::reference("cpp", arg.clone(), "type", "cpp-type");
                    builder.emit(&token, line)?;
                }
                builder.sig(NodeKind::DescSigPunctuation, ">", "p", line)?;
            }
        } else {
            let addname = builder.add(NodeKind::DescAddname, line)?;
            builder.classes(addname, &["sig-prename", "descclassname"])?;
            if let Ok(node) = builder.doc.get_mut(addname) {
                node.attrs.set_str("xml:space", "preserve");
            }
            let token = SigToken::reference("cpp", segment.text.clone(), "identifier", "cpp-identifier");
            builder.emit(&token, addname)?;
            if !segment.sep_after.is_empty() {
                builder.sig(NodeKind::DescSigPunctuation, &segment.sep_after, "p", addname)?;
            }
        }
    }

    // Alias target (`type X = Y`).
    if let Some(alias) = &decl.alias {
        builder.sig(NodeKind::DescSigSpace, " ", "w", line)?;
        builder.sig(NodeKind::DescSigOperator, "=", "o", line)?;
        builder.sig(NodeKind::DescSigSpace, " ", "w", line)?;
        builder.emit_all(alias, line)?;
    }

    if !decl.params.is_empty() && decl.is_function {
        let params = builder.add(NodeKind::DescParameterlist, line)?;
        if let Ok(node) = builder.doc.get_mut(params) {
            node.attrs.set_bool("multi_line_parameter_list", false);
            node.attrs.set_str("xml:space", "preserve");
        }
        for param in &decl.params {
            let parameter = builder.add(NodeKind::DescParameter, params)?;
            if let Ok(node) = builder.doc.get_mut(parameter) {
                node.attrs.set_bool("noemph", true);
                node.attrs.set_str("xml:space", "preserve");
            }
            builder.emit_all(param, parameter)?;
            // C++ parameters carry the extra `sig-param` class on their name.
            let children = builder.doc.get(parameter).map(|node| node.children.clone()).unwrap_or_default();
            for child in children {
                if builder.doc.get(child).is_ok_and(|node| node.kind == NodeKind::DescSigName) {
                    if let Ok(node) = builder.doc.get_mut(child) {
                        let mut classes = node.get_list("classes").ok().flatten().unwrap_or_default();
                        if !classes.iter().any(|class| class == "sig-param") {
                            classes.push("sig-param".to_owned());
                        }
                        node.attrs.set_list("classes", classes);
                    }
                    break;
                }
            }
        }
    }
    builder.emit_all(&decl.suffix, line)?;

    // Annotate signature xrefs with the read scope (namespace + nested scope).
    let scope_text = scope.qualified("");
    annotate_scope(builder.doc, signature_id, &scope_text);
    let _ = objtype;
    Ok(())
}

/// Recursively sets `cpp:scope` on every pending xref under `node`.
fn annotate_scope(doc: &mut Document, node: NodeId, scope: &str) {
    if doc.get(node).is_ok_and(|current| current.kind == NodeKind::PendingXref) {
        if let Ok(mutable) = doc.get_mut(node) {
            mutable.attrs.set_str("cpp:scope", scope);
        }
    }
    let children = doc.get(node).map(|node| node.children.clone()).unwrap_or_default();
    for child in children {
        annotate_scope(doc, child, scope);
    }
}
