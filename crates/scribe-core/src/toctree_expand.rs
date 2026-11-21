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

use crate::{toctree::docname_join, BuildEnvironment, ToctreeCollection};

/// Outcome of one expansion pass.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToctreeExpansion {
    /// Number of `toctree` nodes replaced with a resolved structure.
    pub replaced: usize,
    /// Number of `toctree` nodes removed (hidden or empty).
    pub removed: usize,
    /// Entries dropped because the target document has no title, paired with
    /// the 1-based line of the toctree directive that referenced them.
    ///
    /// Upstream keeps the document in `toctree['includefiles']` but emits no
    /// link and logs `toc.no_title`; callers use these pairs to render that
    /// diagnostic without touching the frozen environment seam.
    pub no_title: Vec<(String, Option<u32>)>,
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

        let toctree_line = node.source.as_ref().and_then(|source| source.line);
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
            // Upstream's `TocTreeCollector.resolve_references` keeps a
            // titleless target in `includefiles` but produces an empty TOC for
            // it, so no reference node is emitted and a `toc.no_title`
            // diagnostic is logged at the toctree directive's line.
            if entry.title.is_empty() && entry.kind == "doc" {
                let target = docname_join(docname, &entry.href);
                // A target that was never discovered is reported as
                // `toc.not_readable` by the collector; `toc.no_title` only
                // applies to an existing document that carries no title.
                if env.found_docs.contains_key(&target) && doc_title(env, &target).is_none() {
                    result.no_title.push((target.clone(), toctree_line));
                    continue;
                }
            }
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
    /// Set for a hidden sub-toctree that upstream leaves unresolved in the
    /// exported doctree: it is emitted as a bare `toctree` node rather than a
    /// list item.
    raw_toctree: Option<Node>,
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
    // When the toctree entry has no explicit title, upstream copies the target
    // document's title *nodes* (filtered by `SphinxContentsFilter`) into the
    // reference instead of a plain-text label. Preserve inline markup such as
    // `:mod:` roles so exported doctrees match the oracle.
    let rich_children = if entry.title.is_empty() && !external {
        rich_title_children(env, docname, document)
    } else {
        None
    };
    let numbered = collection.toctrees.iter().any(|tree| tree.options.numbered != 0);
    build_list_item(
        document, &title, &refuri, "", !external, secnumber, &children, 1, maxdepth, titlesonly,
        false, numbered, rich_children,
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
    numbered: bool,
    rich_children: Option<Vec<NodeId>>,
) -> Result<NodeId, AstError> {
    // Upstream `_toctree_add_classes()` tags each list item and compact
    // paragraph with its depth level; the HTML5 writer surfaces the former on
    // the emitted `<li class="toctree-lN">`.
    let level_class = format!("toctree-l{depth}");
    let item = document.add_node(
        Node::new(NodeKind::ListItem).with_attr(
            "classes",
            scribe_ast::AttributeValue::List(vec![level_class.clone()]),
        ),
        None,
    )?;
    let para = document.add_node(
        Node::new(NodeKind::CompactParagraph).with_attr(
            "classes",
            scribe_ast::AttributeValue::List(vec![level_class]),
        ),
        None,
    )?;
    let mut reference = Node::new(NodeKind::Reference)
        .with_attr("internal", AttributeValue::Bool(internal))
        .with_attr("refuri", AttributeValue::Str(refuri.to_owned()))
        .with_attr("anchorname", AttributeValue::Str(anchorname.to_owned()));
    if let Some(number) = secnumber {
        reference.attrs.insert("secnumber", AttributeValue::Str(number));
    }
    let reference_id = document.add_node(reference, None)?;
    if let Some(children) = rich_children {
        document.replace_children(reference_id, children)?;
    } else if literal_title {
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

    // Domain objects are not sections: upstream's `_toctree_copy` only counts
    // section nesting toward `maxdepth`, so a section at the maximum depth
    // still lists its immediate objects. Nested members *inside* an object
    // are, however, pruned once `maxdepth` is reached.
    let objects_only = !children.is_empty() && children.iter().all(|child| child.literal_title);
    // A numbered toctree promotes a document's sections to top-level entries,
    // so its immediate objects sit one level below them and stay visible at
    // `maxdepth`. Unnumbered toctrees count every nesting level.
    let pruned = titlesonly
        || (maxdepth > 0
            && ((depth >= maxdepth && !(objects_only && !literal_title && numbered))
                || literal_title));
    if !pruned && !children.is_empty() {
        let mut nested_items = Vec::with_capacity(children.len());
        let mut raw_toctrees: Vec<Node> = Vec::new();
        for child in children {
            if let Some(original) = &child.raw_toctree {
                let mut unresolved = Node::new(NodeKind::Toctree);
                for (key, value) in original.attrs.iter() {
                    unresolved.attrs.insert(key.clone(), value.clone());
                }
                raw_toctrees.push(unresolved);
                continue;
            }
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
                numbered,
                None,
            )?);
        }
        let mut nested_ids = nested_items;
        for unresolved in raw_toctrees {
            nested_ids.push(document.add_node(unresolved, None)?);
        }
        let nested = document.add_node(Node::new(NodeKind::BulletList), None)?;
        document.replace_children(nested, nested_ids)?;
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
    // Upstream `_toctree_standard_entry()` derives each referenced document's
    // local ToC depth from `:tocdepth:` metadata (`0` = unlimited).
    let tocdepth = env
        .get_doctree(docname)
        .and_then(crate::document_tocdepth)
        .unwrap_or(0)
        .max(0) as usize;
    if let Some(document) = env.get_doctree(docname) {
        for id in document.children(document.root).unwrap_or_default().to_vec() {
            collect_node_entries(
                document, id, docname, &mut counter, collection, includehidden, &mut out,
                0, tocdepth,
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
    depth: usize,
    tocdepth: usize,
) {
    let Ok(node) = document.get(id) else {
        return;
    };
    match node.kind {
        NodeKind::Section => {
            if tocdepth > 0 && depth >= tocdepth {
                return;
            }
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
                collect_node_entries(document, child, docname, counter, collection, includehidden, &mut children, depth + 1, tocdepth);
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
                    raw_toctree: None,
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
            if tocdepth > 0 && depth >= tocdepth {
                return;
            }
            let hidden = bool_attr(node, "hidden", false);
            if hidden && !includehidden {
                // Upstream keeps a hidden sub-toctree as an unresolved
                // `addnodes.toctree` inside the parent's nested bullet list
                // when `includehidden` is false. Its attributes are copied
                // verbatim; no link is generated for its targets.
                if read_entries(node).is_empty() {
                    return;
                }
                out.push(ChildEntry {
                    title: String::new(),
                    href: String::new(),
                    anchor: String::new(),
                    number: Vec::new(),
                    external: false,
                    literal_title: false,
                    needs_expand: false,
                    raw_toctree: Some(node.clone()),
                    children: Vec::new(),
                });
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
                    raw_toctree: None,
                    children: Vec::new(),
                });
            }
        }
        _ => {
            for child in node.children.clone() {
                collect_node_entries(document, child, docname, counter, collection, includehidden, out, depth + 1, tocdepth);
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
    // Upstream's std-domain objects (options, envvars, programs, generic
    // `object`) carry `_toc_name=""` and are excluded from the global ToC.
    if str_attr(desc, "domain") == Some("std") {
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
            // C++ signatures keep their `desc_name` inside a signature line;
            // search the whole signature subtree.
            fn find_desc_name(document: &Document, id: NodeId) -> Option<String> {
                let node = document.get(id).ok()?;
                if node.kind == NodeKind::DescName {
                    return Some(node.astext(document));
                }
                for &child in &node.children {
                    if let Some(name) = find_desc_name(document, child) {
                        return Some(name);
                    }
                }
                None
            }
            title = find_desc_name(document, child).unwrap_or_default();
        }
        if title.is_empty() {
            if let Some(fullname) = str_attr(desc, "cpp:fullname") {
                title = fullname.rsplit("::").next().unwrap_or(fullname).to_owned();
            }
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
        // Autodoc class members are listed in the page's local ToC but are
        // not part of the master toctree's object hierarchy.
        let autodoc = desc.attrs.get_bool("py:autodoc").ok().flatten() == Some(true);
        let mut children = Vec::new();
        if !autodoc {
            for &content in &desc.children {
                if document.get(content).is_ok_and(|n| n.kind == NodeKind::DescContent) {
                    collect_nested_desc(document, content, docname, collection, &mut children);
                }
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
            raw_toctree: None,
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
    find_first_title_id(document, id, counter)
        .map(|title_id| document.get(title_id).map(|title| title.astext(document)).unwrap_or_default())
        .filter(|text| !text.is_empty())
}

/// Locate the node id of the document's first (section) title, mirroring the
/// traversal in [`find_first_title`] so rich ToC entries copy exactly the same
/// node that supplies the plain-text title.
fn find_first_title_id(document: &Document, id: NodeId, counter: &mut usize) -> Option<NodeId> {
    let node = document.get(id).ok()?;
    if node.kind == NodeKind::Title && !node.astext(document).is_empty() {
        return Some(id);
    }
    if node.kind == NodeKind::Section && *counter > 0 {
        return None;
    }
    if node.kind == NodeKind::Section {
        *counter += 1;
    }
    for child in &node.children {
        if let Some(title) = find_first_title_id(document, *child, counter) {
            return Some(title);
        }
    }
    None
}

/// Clone the children of a target document's title into the document whose
/// ToC is being expanded, applying Sphinx's `SphinxContentsFilter`:
/// `reference` / `target` / `problematic` nodes are unwrapped (their children
/// are kept), footnote and citation references are dropped, and `image` nodes
/// contribute their `alt` text. This is what makes a `:mod:`-role fragment in
/// a title survive as `<literal classes="xref py py-mod">` in a ToC link.
fn clone_title_contents(
    src: &Document,
    src_id: NodeId,
    dst: &mut Document,
) -> Result<Vec<NodeId>, AstError> {
    let Ok(node) = src.get(src_id) else {
        return Ok(Vec::new());
    };
    match node.kind {
        NodeKind::Reference | NodeKind::Target | NodeKind::Problematic | NodeKind::PendingXref => {
            let mut out = Vec::new();
            for &child in &node.children {
                out.extend(clone_title_contents(src, child, dst)?);
            }
            Ok(out)
        }
        NodeKind::FootnoteReference | NodeKind::CitationReference => Ok(Vec::new()),
        NodeKind::Image => {
            let alt = node.attrs.get_str("alt").ok().flatten().unwrap_or("").to_owned();
            if alt.is_empty() {
                Ok(Vec::new())
            } else {
                Ok(vec![dst.new_text(alt, None)?])
            }
        }
        _ => {
            let mut copy = node.clone();
            copy.children.clear();
            let new_id = dst.add_node(copy, None)?;
            let children = node.children.clone();
            let cloned = {
                let mut all = Vec::new();
                for child in children {
                    all.extend(clone_title_contents(src, child, dst)?);
                }
                all
            };
            dst.get_mut(new_id)?.children = cloned;
            Ok(vec![new_id])
        }
    }
}

/// Build the rich content for a ToC reference to `docname`, or `None` when the
/// target has no discoverable title (in which case the caller falls back to a
/// plain-text label).
fn rich_title_children(
    env: &BuildEnvironment,
    docname: &str,
    dst: &mut Document,
) -> Option<Vec<NodeId>> {
    let src = env.get_doctree(docname)?;
    let mut counter = 0usize;
    let title_id = find_first_title_id(src, src.root, &mut counter)?;
    let title = src.get(title_id).ok()?;
    let mut out = Vec::new();
    for &child in &title.children {
        out.extend(clone_title_contents(src, child, dst).ok()?);
    }
    Some(out)
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
