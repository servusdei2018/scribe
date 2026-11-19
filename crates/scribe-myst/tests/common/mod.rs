//! Shared helpers for MyST parser tests.

#![allow(dead_code)]

use scribe_ast::{AttributeValue, Document, Node, NodeId, NodeKind};

/// All live nodes of `kind`, in arena order.
pub fn nodes_of(doc: &Document, kind: NodeKind) -> Vec<(NodeId, &Node)> {
    doc.iter().filter(|(_, node)| node.kind == kind).collect()
}

/// First live node of `kind`.
pub fn first_of(doc: &Document, kind: NodeKind) -> (NodeId, &Node) {
    nodes_of(doc, kind)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no {} node", kind.tag_name()))
}

/// Text content of a node subtree.
pub fn text(doc: &Document, id: NodeId) -> String {
    doc.get(id).map_or_else(|_| String::new(), |n| n.astext(doc))
}

/// Asserts that a node's byte span covers exactly `expected` in `source`.
pub fn assert_span(source: &str, node: &Node, expected: &str) {
    let span = node.span.unwrap_or_else(|| panic!("missing span on {}", node.kind.tag_name()));
    let start = span.start as usize;
    let end = span.end as usize;
    assert!(end <= source.len(), "span end {end} out of bounds for {} bytes", source.len());
    assert!(start <= end, "inverted span {span:?}");
    assert_eq!(&source[start..end], expected, "span text mismatch for {}", node.kind.tag_name());
}

/// Asserts every span in the document is in bounds, non-inverted and on UTF-8
/// character boundaries.
pub fn assert_all_spans_in_bounds(source: &str, doc: &Document) {
    for (id, node) in doc.iter() {
        if let Some(span) = node.span {
            let start = span.start as usize;
            let end = span.end as usize;
            assert!(start <= end, "inverted span on node {id:?}: {span:?}");
            assert!(end <= source.len(), "span out of bounds on node {id:?}: {span:?}");
            assert!(source.is_char_boundary(start), "span start on non-char boundary: {start}");
            assert!(source.is_char_boundary(end), "span end on non-char boundary: {end}");
        }
    }
}

fn fmt_value(value: &AttributeValue) -> String {
    match value {
        AttributeValue::Str(s) => format!("s={s}"),
        AttributeValue::Bool(b) => format!("b={b}"),
        AttributeValue::Int(i) => format!("i={i}"),
        AttributeValue::Float(f) => format!("f={f}"),
        AttributeValue::List(items) => format!("l=[{}]", items.join(",")),
        AttributeValue::NodeId(_) | AttributeValue::NodeIds(_) => "id=<elided>".to_owned(),
        AttributeValue::Null => "null".to_owned(),
    }
}

/// Attributes that are span/source noise or parser-implementation choices and
/// are intentionally normalized away in equivalence comparisons.
fn ignored_attr(key: &str) -> bool {
    matches!(
        key,
        "source" | "bullet" | "start" | "xml:space" | "refdoc" | "force" | "language"
    )
}

/// Produces a canonical, span-free tree signature for differential assertions.
pub fn canonical(doc: &Document, id: NodeId) -> String {
    let node = match doc.get(id) {
        Ok(node) => node,
        Err(_) => return "<dead>".to_owned(),
    };
    if node.kind == NodeKind::Text {
        return format!("#text({})", node.text_content().unwrap_or(""));
    }
    let mut attrs: Vec<String> = node
        .attrs
        .iter()
        .filter(|(key, _)| !ignored_attr(key))
        .map(|(key, value)| format!("{key}={}", fmt_value(value)))
        .collect();
    attrs.sort();

    // Normalize the well-known "tight list / definition" representation gap:
    // Docutils always wraps item content in a `paragraph`, while a streaming
    // Markdown parser emits inline children directly. Both are the same
    // semantic construct, so compare them transparently.
    let children = if matches!(node.kind, NodeKind::ListItem | NodeKind::Definition)
        && node.children.len() == 1
        && doc
            .get(node.children[0])
            .is_ok_and(|child| child.kind == NodeKind::Paragraph)
    {
        let paragraph = node.children[0];
        doc.get(paragraph)
            .map(|p| {
                p.children
                    .iter()
                    .map(|child| canonical(doc, *child))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default()
    } else {
        node.children
            .iter()
            .map(|child| canonical(doc, *child))
            .collect::<Vec<_>>()
            .join("")
    };
    format!(
        "<{}{}>{}</{}>",
        node.kind.tag_name(),
        if attrs.is_empty() {
            String::new()
        } else {
            format!(" {}", attrs.join(" "))
        },
        children,
        node.kind.tag_name()
    )
}

/// Canonical signature of a whole document.
pub fn canonical_doc(doc: &Document) -> String {
    canonical(doc, doc.root)
}
