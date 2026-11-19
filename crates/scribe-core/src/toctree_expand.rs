//! Phase 38 — Sphinx `TocTree` doctree expansion for exported doctrees.
//!
//! Upstream resolves every `addnodes.toctree` in
//! `BuildEnvironment.get_and_resolve_doctree()` before a builder writes the
//! doctree. The result replaces the raw directive node with
//!
//! ```text
//! compact_paragraph [toctree="1"]
//!   title?                     # `:caption:`
//!   bullet_list
//!     list_item                # one per entry
//!       compact_paragraph
//!         reference [internal, refuri, anchorname, secnumber?]
//!       bullet_list?           # nested sections / child toctrees
//!         ...
//! ```
//!
//! Scribe keeps the raw `toctree` node in the working AST (the HTML5 writer
//! renders it directly as `toctree-wrapper`), so this pass is applied only to
//! the *exported* doctree by the `xml` / `pseudoxml` builders. That keeps the
//! frozen §7 AST and the §12 `BuildEnvironment` contract untouched, and avoids
//! double-wrapping in HTML5 (the wrapper `compound` is already emitted by the
//! directive).

use std::collections::HashSet;

use scribe_ast::{AstError, AttributeValue, Document, Node, NodeId, NodeKind};

use crate::{BuildEnvironment, ToctreeCollection};

/// Outcome of one expansion pass.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToctreeExpansion {
    /// Number of `toctree` nodes replaced with a resolved structure.
    pub replaced: usize,
    /// Number of `toctree` nodes removed (hidden or empty).
    pub removed: usize,
}

/// Replace every reachable `toctree` node in `document` with Sphinx's resolved
/// nested structure. Returns the number of nodes replaced / removed.
///
/// This function is additive: it never changes the frozen AST node kinds and
/// leaves the document's `metadata` and parent arena bookkeeping consistent.
pub fn expand_toctrees(
    document: &mut Document,
    docname: &str,
    env: &BuildEnvironment,
    collection: &ToctreeCollection,
) -> Result<ToctreeExpansion, AstError> {
    // Collect ids first: nodes created below are never `Toctree` nodes, and a
    // removed node becomes orphaned (parent == None) before the next iteration.
    let ids: Vec<NodeId> = document
        .preorder_ids()
        .into_iter()
        .filter(|&id| document.get(id).is_ok_and(|node| node.kind == NodeKind::Toctree))
        .collect();

    let mut result = ToctreeExpansion::default();
    for id in ids {
        let Some(parent) = document.parent(id)? else {
            continue; // orphaned by an earlier replacement
        };
        let index = document
            .get(parent)?
            .children
            .iter()
            .position(|&child| child == id)
            .ok_or_else(|| AstError::InvalidMutation("toctree node missing from parent".into()))?;

        let node = document.get(id)?;
        let hidden = bool_attr(node, "hidden", false);
        let includehidden = bool_attr(node, "includehidden", false);
        let entries = read_entries(node);
        if (hidden && !includehidden) || entries.is_empty() {
            document.splice_children(parent, index..index + 1, Vec::new())?;
            result.removed += 1;
            continue;
        }

        let maxdepth = int_attr(node, "maxdepth", -1);
        let titlesonly = bool_attr(node, "titlesonly", false);
        let caption = node.attrs.get_str("caption").ok().flatten().unwrap_or_default().to_owned();
        let rawcaption = node
            .attrs
            .get_str("rawcaption")
            .ok()
            .flatten()
            .unwrap_or(caption.as_str())
            .to_owned();

        let mut visiting = HashSet::new();
        visiting.insert(docname.to_owned());

        let para_id = document.add_node(
            Node::new(NodeKind::CompactParagraph).with_attr("toctree", AttributeValue::Bool(true)),
            None,
        )?;

        if !rawcaption.is_empty() {
            let title_id = document.add_node(Node::new(NodeKind::Title), None)?;
            let text_id = document.new_text(rawcaption, None)?;
            document.replace_children(title_id, vec![text_id])?;
            document.append_child(para_id, title_id)?;
        }

        let mut top_items = Vec::with_capacity(entries.len());
        for entry in &entries {
            top_items.push(make_entry(
                document, entry, docname, maxdepth, titlesonly, includehidden, env, collection,
                &mut visiting,
            )?);
        }
        let list_id = document.add_node(Node::new(NodeKind::BulletList), None)?;
        document.replace_children(list_id, top_items)?;
        document.append_child(para_id, list_id)?;

        document.splice_children(parent, index..index + 1, vec![para_id])?;
        result.replaced += 1;
    }
    Ok(result)
}

// ---------------------------------------------------------------------------
// Entry model
// ---------------------------------------------------------------------------

/// One flat directive entry, parallel to upstream `toctree['entries']`.
#[derive(Clone, Debug)]
struct DirectiveEntry {
    title: String,
    href: String,
    kind: String,
}

/// One resolved node in a document's local ToC tree.
#[derive(Clone, Debug)]
struct ChildEntry {
    title: String,
    href: String,
    anchor: String,
    number: Vec<i64>,
    external: bool,
    /// Upstream wraps domain-object ToC labels in a `literal` node.
    literal_title: bool,
    /// `true` for a link to another document whose own children still need to
    /// be resolved (a nested toctree entry); `false` for a section, whose
    /// children were built directly from its doctree subtree.
    needs_expand: bool,
    children: Vec<ChildEntry>,
}

fn read_entries(node: &Node) -> Vec<DirectiveEntry> {
    let refs = list_attr(node, "entry_refs");
    let kinds = list_attr(node, "entry_kinds");
    let titles = list_attr(node, "entry_titles");
    refs.into_iter()
        .enumerate()
        .map(|(index, href)| DirectiveEntry {
            title: titles.get(index).cloned().unwrap_or_default(),
            href,
            kind: kinds.get(index).cloned().unwrap_or_else(|| "doc".to_owned()),
        })
        .collect()
}

/// Build one top-level `list_item` for a directive entry.
#[allow(clippy::too_many_arguments)]
fn make_entry(
    document: &mut Document,
    entry: &DirectiveEntry,
    current_doc: &str,
    maxdepth: i64,
    titlesonly: bool,
    includehidden: bool,
    env: &BuildEnvironment,
    collection: &ToctreeCollection,
    visiting: &mut HashSet<String>,
) -> Result<NodeId, AstError> {
    let external = entry.kind == "external";
    let is_self = entry.kind == "self";
    let docname = if is_self { current_doc } else { entry.href.as_str() };
    let title = if !entry.title.is_empty() {
        entry.title.clone()
    } else if external {
        entry.href.clone()
    } else {
        doc_title(env, docname).unwrap_or_else(|| docname.to_owned())
    };
    let refuri = docname.to_owned();
    let secnumber = if external {
        None
    } else {
        number_attr(collection, docname, "")
    };
    let children = if external {
        Vec::new()
    } else {
        collect_children(docname, env, collection, includehidden, visiting)
    };
    build_list_item(
        document, &title, &refuri, "", !external, secnumber, &children, 1, maxdepth, titlesonly,
        false,
    )
}

/// Materialise `list_item → compact_paragraph → reference` plus an optional
/// nested `bullet_list` of children. Depth/maxdepth pruning mirrors upstream's
/// `_toctree_copy` pass.
#[allow(clippy::too_many_arguments)]
fn build_list_item(
    document: &mut Document,
    title: &str,
    refuri: &str,
    anchorname: &str,
    internal: bool,
    secnumber: Option<String>,
    children: &[ChildEntry],
    depth: i64,
    maxdepth: i64,
    titlesonly: bool,
    literal_title: bool,
) -> Result<NodeId, AstError> {
    let item = document.add_node(Node::new(NodeKind::ListItem), None)?;
    let para = document.add_node(Node::new(NodeKind::CompactParagraph), None)?;
    let mut reference = Node::new(NodeKind::Reference)
        .with_attr("internal", AttributeValue::Bool(internal))
        .with_attr("refuri", AttributeValue::Str(refuri.to_owned()))
        .with_attr("anchorname", AttributeValue::Str(anchorname.to_owned()));
    if let Some(number) = secnumber {
        reference.attrs.insert("secnumber", AttributeValue::Str(number));
    }
    let reference_id = document.add_node(reference, None)?;
    if literal_title {
        // Domain-object signatures are rendered as `<literal>` (upstream adds
        // `nodes.literal` as the sole child of the ToC reference).
        let literal_id = document.add_node(Node::new(NodeKind::Literal), None)?;
        let text_id = document.new_text(title.to_owned(), None)?;
        document.replace_children(literal_id, vec![text_id])?;
        document.replace_children(reference_id, vec![literal_id])?;
    } else {
        let text_id = document.new_text(title.to_owned(), None)?;
        document.replace_children(reference_id, vec![text_id])?;
    }
    document.replace_children(para, vec![reference_id])?;
    document.append_child(item, para)?;

    let pruned = titlesonly || (maxdepth > 0 && depth >= maxdepth);
    if !pruned && !children.is_empty() {
        let mut nested_items = Vec::with_capacity(children.len());
        for child in children {
            let child_refuri = format!("{}{}", child.href, child.anchor);
            let child_secnumber = (!child.number.is_empty()).then(|| number_display(&child.number));
            nested_items.push(build_list_item(
                document,
                &child.title,
                &child_refuri,
                &child.anchor,
                !child.external,
                child_secnumber,
                &child.children,
                depth + 1,
                maxdepth,
                titlesonly,
                child.literal_title,
            )?);
        }
        let nested = document.add_node(Node::new(NodeKind::BulletList), None)?;
        document.replace_children(nested, nested_items)?;
        document.append_child(item, nested)?;
    }
    Ok(item)
}

// ---------------------------------------------------------------------------
// Local ToC tree of a referenced document
// ---------------------------------------------------------------------------

/// Collect the local ToC tree of `docname`, expanding nested toctree entries.
/// `visiting` bounds recursion on circular toctrees.
fn collect_children(
    docname: &str,
    env: &BuildEnvironment,
    collection: &ToctreeCollection,
    includehidden: bool,
    visiting: &mut HashSet<String>,
) -> Vec<ChildEntry> {
    if !visiting.insert(docname.to_owned()) {
        return Vec::new(); // circular toctree reference
    }
    let mut counter = 0usize;
    let mut out = Vec::new();
    if let Some(document) = env.get_doctree(docname) {
        for id in document.children(document.root).unwrap_or_default().to_vec() {
            collect_node_entries(
                document, id, docname, &mut counter, collection, includehidden, &mut out,
            );
        }
        // Expand links to *other* documents (nested toctree entries) one level
        // at a time. Sections already carry their subtree and must not be
        // re-collected (that would loop forever on the same docname).
        let mut expanded: Vec<ChildEntry> = Vec::with_capacity(out.len());
        for mut child in out.drain(..) {
            if child.needs_expand && !child.external && !child.href.starts_with("http") {
                child.children = collect_children(&child.href, env, collection, includehidden, visiting);
            }
            child.needs_expand = false;
            expanded.push(child);
        }
        out = expanded;
    }
    visiting.remove(docname);
    out
}

/// Convert one doctree node into local ToC entries (in document order).
#[allow(clippy::too_many_arguments)]
fn collect_node_entries(
    document: &Document,
    id: NodeId,
    docname: &str,
    counter: &mut usize,
    collection: &ToctreeCollection,
    includehidden: bool,
    out: &mut Vec<ChildEntry>,
) {
    let Ok(node) = document.get(id) else {
        return;
    };
    match node.kind {
        NodeKind::Section => {
            let title = node
                .children
                .iter()
                .find(|child| document.get(**child).is_ok_and(|c| c.kind == NodeKind::Title))
                .map(|child| document.get(*child).map(|t| t.astext(document)).unwrap_or_default())
                .unwrap_or_default();
            let id_value = node.ids().ok().and_then(|ids| ids.first().cloned()).unwrap_or_default();
            let anchor = if *counter == 0 {
                String::new()
            } else {
                format!("#{id_value}")
            };
            *counter += 1;
            let mut children = Vec::new();
            for child in node.children.clone() {
                if document.get(child).is_ok_and(|c| c.kind == NodeKind::Title) {
                    continue;
                }
                collect_node_entries(document, child, docname, counter, collection, includehidden, &mut children);
            }
            if anchor.is_empty() {
                // The first section supplies the document title, already used
                // as the entry's reference; upstream nests its sub-sections.
                out.extend(children);
            } else {
                let number = collection
                    .section_numbers
                    .get(docname, &anchor)
                    .map(<[i64]>::to_vec)
                    .unwrap_or_default();
                out.push(ChildEntry {
                    title,
                    href: docname.to_owned(),
                    anchor,
                    number,
                    external: false,
                    literal_title: false,
                    needs_expand: false,
                    children,
                });
            }
        }
        NodeKind::Desc => {
            // Upstream `TocTreeCollector.build_tocs` adds domain object
            // signatures to the document's local ToC as
            // `reference(literal(name), internal=True, anchorname=#id)`.
            collect_desc_entries(document, id, docname, collection, out);
        }
        NodeKind::Toctree => {
            let hidden = bool_attr(node, "hidden", false);
            if hidden && !includehidden {
                return;
            }
            for entry in read_entries(node) {
                let external = entry.kind == "external";
                let is_self = entry.kind == "self";
                let href = if is_self { docname.to_owned() } else { entry.href.clone() };
                let title = if !entry.title.is_empty() {
                    entry.title.clone()
                } else if external {
                    entry.href.clone()
                } else {
                    out.iter().find(|e| e.href == href && e.number.is_empty())
                        .map(|e| e.title.clone())
                        .unwrap_or_else(|| href.clone())
                };
                out.push(ChildEntry {
                    title,
                    href,
                    anchor: String::new(),
                    number: Vec::new(),
                    external,
                    literal_title: false,
                    needs_expand: !external,
                    children: Vec::new(),
                });
            }
        }
        _ => {
            for child in node.children.clone() {
                collect_node_entries(document, child, docname, counter, collection, includehidden, out);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Domain object signatures in the local ToC
// ---------------------------------------------------------------------------

/// Add one ToC entry per indexable `desc_signature` of a domain object,
/// mirroring upstream's `memo_parents` nesting of object members.
fn collect_desc_entries(
    document: &Document,
    desc_id: NodeId,
    docname: &str,
    collection: &ToctreeCollection,
    out: &mut Vec<ChildEntry>,
) {
    let Ok(desc) = document.get(desc_id) else {
        return;
    };
    let no_contents = str_attr(desc, "no-contents-entry")
        .or_else(|| str_attr(desc, "no_contents_entry"))
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
    if no_contents {
        return;
    }
    for &child in &desc.children {
        let Ok(sig) = document.get(child) else {
            continue;
        };
        if sig.kind != NodeKind::DescSignature {
            continue;
        }
        // Some domains (notably JS) put the anchor on the enclosing `desc`
        // instead of the `desc_signature`; upstream still indexes the object.
        let ids = sig
            .ids()
            .ok()
            .and_then(|ids| ids.first().cloned())
            .or_else(|| desc.ids().ok().and_then(|ids| ids.first().cloned()))
            .unwrap_or_default();
        if ids.is_empty() {
            continue; // `:no-index:` objects are not in the ToC
        }
        let mut title = str_attr(sig, "_toc_name")
            .or_else(|| str_attr(sig, "fullname"))
            .or_else(|| str_attr(sig, "py:signature"))
            .unwrap_or_default()
            .to_owned();
        if title.is_empty() {
            title = sig.children.iter().find_map(|&id| {
                let node = document.get(id).ok()?;
                if node.kind == NodeKind::DescName {
                    Some(node.astext(document))
                } else {
                    None
                }
            }).unwrap_or_default();
        }
        if title.is_empty() {
            continue;
        }
        let callable = str_attr(desc, "desctype").or_else(|| str_attr(desc, "objtype")).is_some_and(
            |kind| {
                matches!(
                    kind,
                    "function" | "method" | "staticmethod" | "classmethod" | "func" | "meth"
                )
            },
        );
        if callable && !title.ends_with("()") {
            title.push_str("()");
        }
        let anchor = format!("#{ids}");
        let number = collection
            .section_numbers
            .get(docname, &anchor)
            .map(<[i64]>::to_vec)
            .unwrap_or_default();
        let mut children = Vec::new();
        for &content in &desc.children {
            if document.get(content).is_ok_and(|n| n.kind == NodeKind::DescContent) {
                collect_nested_desc(document, content, docname, collection, &mut children);
            }
        }
        out.push(ChildEntry {
            title,
            href: docname.to_owned(),
            anchor,
            number,
            external: false,
            literal_title: true,
            needs_expand: false,
            children,
        });
    }
}

fn collect_nested_desc(
    document: &Document,
    id: NodeId,
    docname: &str,
    collection: &ToctreeCollection,
    out: &mut Vec<ChildEntry>,
) {
    let Ok(node) = document.get(id) else {
        return;
    };
    if node.kind == NodeKind::Desc {
        collect_desc_entries(document, id, docname, collection, out);
        return;
    }
    for &child in &node.children {
        collect_nested_desc(document, child, docname, collection, out);
    }
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn doc_title(env: &BuildEnvironment, docname: &str) -> Option<String> {
    env.get_doctree(docname)
        .and_then(|document| {
            document.metadata.title.clone().or_else(|| {
                let mut counter = 0usize;
                find_first_title(document, document.root, &mut counter)
            })
        })
        .or_else(|| env.all_docs.get(docname).and_then(|meta| meta.title.clone()))
}

fn find_first_title(document: &Document, id: NodeId, counter: &mut usize) -> Option<String> {
    let node = document.get(id).ok()?;
    if node.kind == NodeKind::Title {
        let text = node.astext(document);
        if !text.is_empty() {
            return Some(text);
        }
    }
    // Avoid treating nested section titles as the document title: only walk
    // the first section chain.
    if node.kind == NodeKind::Section && *counter > 0 {
        return None;
    }
    if node.kind == NodeKind::Section {
        *counter += 1;
    }
    for child in &node.children {
        if let Some(title) = find_first_title(document, *child, counter) {
            return Some(title);
        }
    }
    None
}

fn number_attr(collection: &ToctreeCollection, docname: &str, anchor: &str) -> Option<String> {
    collection
        .section_numbers
        .get(docname, anchor)
        .map(<[i64]>::to_vec)
        .filter(|numbers| !numbers.is_empty())
        .map(|numbers| number_display(&numbers))
}

fn number_display(numbers: &[i64]) -> String {
    numbers.iter().map(i64::to_string).collect::<Vec<_>>().join(" ")
}

fn list_attr(node: &Node, key: &str) -> Vec<String> {
    node.attrs.get_list(key).ok().flatten().unwrap_or_default()
}

fn str_attr<'a>(node: &'a Node, key: &str) -> Option<&'a str> {
    node.attrs.get_str(key).ok().flatten()
}

fn int_attr(node: &Node, key: &str, default: i64) -> i64 {
    match node.attrs.get(key) {
        Some(AttributeValue::Int(value)) => *value,
        Some(AttributeValue::Str(value)) => value.parse().unwrap_or(default),
        _ => default,
    }
}

fn bool_attr(node: &Node, key: &str, default: bool) -> bool {
    match node.attrs.get(key) {
        Some(AttributeValue::Bool(value)) => *value,
        Some(AttributeValue::Int(value)) => *value != 0,
        Some(AttributeValue::Str(value)) => matches!(value.as_str(), "1" | "True" | "true" | "yes"),
        _ => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bool_and_int_attr_helpers_handle_docutils_spellings() {
        let node = Node::new(NodeKind::Toctree)
            .with_attr("hidden", AttributeValue::Bool(true))
            .with_attr("off", AttributeValue::Str("0".to_owned()))
            .with_attr("maxdepth", AttributeValue::Int(3));
        assert!(bool_attr(&node, "hidden", false));
        assert!(bool_attr(&node, "maxdepth", false));
        assert!(!bool_attr(&node, "off", true));
        assert_eq!(int_attr(&node, "maxdepth", -1), 3);
        assert_eq!(int_attr(&node, "missing", -1), -1);
    }
}
