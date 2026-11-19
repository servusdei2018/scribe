//! Single-file HTML builder (Phase 19).
//!
//! Upstream's `SingleFileHTMLBuilder` inlines every toctree document into one
//! page in traversal order, replaces toctree nodes with the inlined bodies,
//! rewrites cross-document links to `#document-<docname>` in-page anchors and
//! merges section ids to avoid collisions. This module provides the ordering,
//! anchor-rewriting and TOC-assembly primitives used by
//! [`crate::html::builder::build_with_flavor`] with
//! [`crate::html::builder::BuildFlavor::singlefile`].

use std::collections::{BTreeMap, BTreeSet};

use scribe_ast::{Document, NodeId, NodeKind};
use scribe_config::Config;
use scribe_core::BuildEnvironment;
use scribe_theme::RenderContext;

use crate::error::Result;
use crate::html::builder::{build_with_flavor, BuildFlavor};
use crate::html::translator::HtmlContext;
use crate::html::{BuildOptions, BuildResult};

/// Stable identity of this builder.
pub const BUILDER_NAME: &str = "singlehtml";

/// `SingleFileHTMLBuilder.get_target_uri`.
///
/// Documents that take part in the build resolve to in-page anchors; other
/// names (additional pages such as `genindex`) keep a normal suffix.
#[must_use]
pub fn target_uri(docname: &str, in_all_docs: bool) -> String {
    if in_all_docs {
        format!("#document-{docname}")
    } else {
        format!("{docname}.html")
    }
}

/// A prepared page as passed between the build pipeline and this module.
pub type PreparedPage = (String, Document, RenderContext, HtmlContext);

/// Documents in upstream `inline_all_toctrees` order: master first, then a
/// depth-first traversal of each document's toctree entries in source order,
/// visiting each docname at most once.
#[must_use]
pub fn traversal_order(prepared: &[PreparedPage], master: &str) -> Vec<String> {
    fn visit(prepared: &[PreparedPage], docname: &str, seen: &mut BTreeSet<String>, order: &mut Vec<String>) {
        if docname.is_empty() || !seen.insert(docname.to_owned()) {
            return;
        }
        order.push(docname.to_owned());
        let Some(index) = prepared.iter().position(|(name, ..)| name == docname) else {
            return;
        };
        let doc = &prepared[index].1;
        let mut children = Vec::new();
        for id in doc.preorder_ids() {
            let Ok(node) = doc.get(id) else { continue };
            if node.kind != NodeKind::Toctree {
                continue;
            }
            children.extend(toctree_docnames(node));
        }
        for child in children {
            visit(prepared, &child, seen, order);
        }
    }

    let mut seen = BTreeSet::new();
    let mut order = Vec::new();
    visit(prepared, master, &mut seen, &mut order);
    order
}

/// Read the internal docnames of a toctree node (both the List and JSON-string
/// encodings used by the RST directive and the toctree collector are accepted).
fn toctree_docnames(node: &scribe_ast::Node) -> Vec<String> {
    if let Some(values) = node.attrs.get_list("entries").ok().flatten() {
        return values.clone();
    }
    if let Some(raw) = node.attrs.get_str("entries").ok().flatten() {
        if let Ok(values) = serde_json::from_str::<Vec<String>>(raw) {
            return values;
        }
    }
    Vec::new()
}

/// Rewrite every node id in the inlined documents to a globally unique anchor
/// and return a `(docname#original-id) -> final-id` map for link rewriting.
///
/// Ids that do not collide keep their original spelling (matching the oracle
/// for the common case); collisions get a `-<docname>` (then `-<docname>-N`)
/// suffix. The map is later consumed by
/// [`crate::html::translator::HtmlContext::resolve_refuri`] and
/// [`crate::html::translator::HtmlContext::resolve_anchor`].
pub fn rewrite_anchors(prepared: &mut [PreparedPage], order: &[String]) -> BTreeMap<String, String> {
    let mut global: BTreeMap<String, String> = BTreeMap::new();
    let mut used: BTreeSet<String> = BTreeSet::new();

    for docname in order {
        let Some(index) = prepared.iter().position(|(name, ..)| name == docname) else {
            continue;
        };
        let doc = &prepared[index].1;
        let mut id_sets: Vec<(NodeId, Vec<String>)> = Vec::new();
        for id in doc.preorder_ids() {
            if let Ok(node) = doc.get(id) {
                if let Ok(ids) = node.ids() {
                    if !ids.is_empty() {
                        id_sets.push((id, ids));
                    }
                }
            }
        }

        let mut local: BTreeMap<String, String> = BTreeMap::new();
        for (_, ids) in &id_sets {
            for original in ids {
                let original = original.trim();
                if original.is_empty() || local.contains_key(original) {
                    continue;
                }
                let final_id = if used.insert(original.to_owned()) {
                    original.to_owned()
                } else {
                    let mut candidate = format!("{original}-{docname}");
                    let mut counter = 2;
                    while used.contains(&candidate) {
                        candidate = format!("{original}-{docname}-{counter}");
                        counter += 1;
                    }
                    let _ = used.insert(candidate.clone());
                    candidate
                };
                local.insert(original.to_owned(), final_id.clone());
                global.insert(format!("{docname}#{original}"), final_id);
            }
        }

        for (id, ids) in id_sets {
            if let Ok(node) = prepared[index].1.get_mut(id) {
                let rewritten: Vec<String> = ids
                    .iter()
                    .map(|old| local.get(old.trim()).cloned().unwrap_or_else(|| old.clone()))
                    .collect();
                node.attrs.set_list("ids", rewritten);
            }
        }
    }
    global
}

/// Assemble the global TOC shown at the top of the single document.
///
/// Upstream renders `global_toctree_for_doc(root_doc, collapse=False)`, which
/// contains every inlined document with `#document-<docname>` links. We render
/// the same flat skeleton; section numbers are intentionally omitted (see the
/// normalization notes in `ARCHITECTURE.md` §25).
#[must_use]
pub fn global_toc(prepared: &[PreparedPage], order: &[String]) -> String {
    let mut out = String::from("<div class=\"toctree-wrapper compound\">\n<ul>\n");
    for docname in order {
        let title = prepared
            .iter()
            .find(|(name, ..)| name == docname)
            .and_then(|(_, doc, ..)| doc.metadata.title.clone())
            .unwrap_or_else(|| docname.clone());
        out.push_str(&format!(
            "<li class=\"toctree-l1\"><a class=\"reference internal\" href=\"#document-{docname}\">{}</a></li>\n",
            crate::html::escape_html(&title)
        ));
    }
    out.push_str("</ul>\n</div>\n");
    out
}

/// Single-file HTML builder.
#[derive(Debug, Default)]
pub struct SingleFileHTMLBuilder;

impl SingleFileHTMLBuilder {
    /// Create a builder.
    #[must_use]
    pub fn new() -> Self { Self }

    /// Run a complete `singlehtml` build into `options.output_dir`.
    pub fn build(
        options: &BuildOptions,
        config: &Config,
        env: &mut BuildEnvironment,
    ) -> Result<BuildResult> {
        build_with_flavor(options, config, env, &BuildFlavor::singlefile())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_uri_uses_inpage_anchors() {
        assert_eq!(target_uri("foo", true), "#document-foo");
        assert_eq!(target_uri("index", true), "#document-index");
        assert_eq!(target_uri("genindex", false), "genindex.html");
    }
}
