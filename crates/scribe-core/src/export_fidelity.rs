//! Phase 38 — node-shape fidelity fixes for *exported* (pseudoxml/XML) doctrees.
//!
//! Upstream's `ReferencesResolver` post-transform rewrites docutils
//! `citation_reference` nodes into `reference` nodes wrapping an `inline`
//! with the bracketed citation label. Scribe keeps the raw node in the working
//! AST because its HTML5/LaTeX writers render it directly as a citation link;
//! this pass applies the upstream shape only when exporting a doctree.

use scribe_ast::{AstResult, AttributeValue, Document, Node, NodeId, NodeKind};

/// Replace each `citation_reference` with `reference → inline → [LABEL]`.
///
/// Returns the number of references rewritten. Idempotent: documents without
/// citation references are left untouched.
pub fn normalize_citation_references(document: &mut Document) -> AstResult<usize> {
    let ids: Vec<NodeId> = document
        .preorder_ids()
        .into_iter()
        .filter(|&id| document.get(id).is_ok_and(|node| node.kind == NodeKind::CitationReference))
        .collect();

    let mut changed = 0usize;
    for id in ids {
        let Some(parent) = document.parent(id)? else {
            continue;
        };
        let index = document
            .get(parent)?
            .children
            .iter()
            .position(|&child| child == id)
            .ok_or_else(|| scribe_ast::AstError::InvalidMutation("citation ref missing from parent".into()))?;

        let label = document.get(id)?.astext(document);
        let label = if label.starts_with('[') && label.ends_with(']') {
            label
        } else {
            format!("[{label}]")
        };
        // Clone every borrowed field before mutating the arena.
        let (attrs, source, span) = {
            let old = document.get(id)?;
            let mut attrs = scribe_ast::Attributes::new();
            for key in ["ids", "names", "dupnames", "classes", "backrefs", "refid", "refuri"] {
                if let Some(value) = old.attrs.get(key) {
                    attrs.insert(key, value.clone());
                }
            }
            attrs.insert("internal", AttributeValue::Bool(true));
            (attrs, old.source.clone(), old.span)
        };

        let inline = document.add_node(Node::new(NodeKind::Inline), None)?;
        let text = document.new_text(label, None)?;
        document.replace_children(inline, vec![text])?;

        let reference = document.add_node(
            Node {
                kind: NodeKind::Reference,
                attrs,
                children: Vec::new(),
                source,
                span,
            },
            None,
        )?;
        document.append_child(reference, inline)?;
        document.splice_children(parent, index..index + 1, vec![reference])?;
        changed += 1;
    }
    Ok(changed)
}

/// Rewrite resolved domain cross-references that wrap their label in an
/// `inline` node into upstream's `literal` node.
///
/// Sphinx's `ReferencesResolver` replaces `pending_xref` with a `reference`
/// containing `nodes.literal` for object roles (`:py:class:`, `:js:func:`, …).
/// Scribe's resolver uses `inline`, which renders identically in HTML5 but
/// differs in pseudoxml. Only `reference → inline` nodes whose classes carry
/// the `xref` marker are rewritten, so unrelated `inline` children (for
/// example `std std-ref`) are left alone.
pub fn normalize_xref_literals(document: &mut Document) -> AstResult<usize> {
    let ids: Vec<NodeId> = document
        .preorder_ids()
        .into_iter()
        .filter(|&id| {
            let Ok(node) = document.get(id) else { return false };
            if node.kind != NodeKind::Inline { return false; }
            let Some(parent) = document.parent(id).ok().flatten() else { return false };
            document.get(parent).is_ok_and(|parent| parent.kind == NodeKind::Reference)
                && node.classes().is_ok_and(|classes| classes.iter().any(|class| class == "xref"))
        })
        .collect();

    let mut changed = 0usize;
    for id in ids {
        if let Ok(node) = document.get_mut(id) {
            node.kind = NodeKind::Literal;
            changed += 1;
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn citation_reference_becomes_reference_with_bracketed_inline() {
        let mut document = Document::new(None);
        let root = document.root;
        let cite = document.add(NodeKind::CitationReference, Some(root)).unwrap();
        let _text = document.new_text("CITE1", Some(cite)).unwrap();
        document.get_mut(cite).unwrap().attrs.set_str("refid", "cite1");

        assert_eq!(normalize_citation_references(&mut document).unwrap(), 1);
        let reference = document.children(root).unwrap()[0];
        assert_eq!(document.get(reference).unwrap().kind, NodeKind::Reference);
        let inline = document.children(reference).unwrap()[0];
        assert_eq!(document.get(inline).unwrap().kind, NodeKind::Inline);
        let text = document.children(inline).unwrap()[0];
        assert_eq!(document.get(text).unwrap().text_content(), Some("[CITE1]"));
        assert_eq!(normalize_citation_references(&mut document).unwrap(), 0);
    }
}
