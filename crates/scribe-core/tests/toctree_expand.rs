//! Phase 38 — exported-doctree fidelity tests.

use scribe_ast::{Document, NodeId, NodeKind};
use scribe_core::{
    expand_toctrees, normalize_citation_references, normalize_xref_literals, BuildEnvironment,
    ToctreeCollection,
};

fn add_with_text(doc: &mut Document, kind: NodeKind, parent: Option<NodeId>, text: &str) -> NodeId {
    let id = doc.add(kind, parent).unwrap();
    let text_id = doc.new_text(text, Some(id)).unwrap();
    let _ = text_id;
    id
}

fn doc_with_toctree() -> (Document, NodeId) {
    let mut doc = Document::new(Some("index".to_owned()));
    let root = doc.root;
    let section = doc.add(NodeKind::Section, Some(root)).unwrap();
    let _title = add_with_text(&mut doc, NodeKind::Title, Some(section), "Index");
    let toc = doc.add(NodeKind::Toctree, Some(section)).unwrap();
    doc.get_mut(toc).unwrap().attrs.set_list("entry_refs", vec!["child".to_owned()]);
    doc.get_mut(toc).unwrap().attrs.set_list("entry_kinds", vec!["doc".to_owned()]);
    doc.get_mut(toc).unwrap().attrs.set_list("entry_titles", vec![String::new()]);
    (doc, toc)
}

#[test]
fn toctree_expands_into_compact_paragraph_and_reference() {
    let (mut doc, toc) = doc_with_toctree();
    let mut env = BuildEnvironment::new(std::path::PathBuf::from("/tmp"));
    let mut child = Document::new(Some("child".to_owned()));
    let root = child.root;
    let section = child.add(NodeKind::Section, Some(root)).unwrap();
    let _t = add_with_text(&mut child, NodeKind::Title, Some(section), "Child title");
    child.get_mut(section).unwrap().attrs.set_list("ids", vec!["child".to_owned()]);
    env.documents.insert(
        "child".to_owned(),
        scribe_core::DocumentSnapshot {
            docname: "child".to_owned(),
            doctree: Some(child),
            ..Default::default()
        },
    );
    let collection = ToctreeCollection::default();

    let result = expand_toctrees(&mut doc, "index", &env, &collection).unwrap();
    assert_eq!(result.replaced, 1);
    assert_eq!(result.removed, 0);
    assert_eq!(doc.get(toc).unwrap().kind, NodeKind::Toctree); // orphaned, unreachable

    let root = doc.root;
    let section = doc.children(root).unwrap()[0];
    let para = doc.children(section).unwrap()[1];
    assert_eq!(doc.get(para).unwrap().kind, NodeKind::CompactParagraph);
    assert!(doc.get(para).unwrap().attrs.get("toctree").is_some());
    let list = doc.children(para).unwrap()[0];
    assert_eq!(doc.get(list).unwrap().kind, NodeKind::BulletList);
    let item = doc.children(list).unwrap()[0];
    assert_eq!(doc.get(item).unwrap().kind, NodeKind::ListItem);
    let inner_para = doc.children(item).unwrap()[0];
    let reference = doc.children(inner_para).unwrap()[0];
    assert_eq!(doc.get(reference).unwrap().kind, NodeKind::Reference);
}

#[test]
fn hidden_and_empty_toctrees_are_removed() {
    let (mut doc, toc) = doc_with_toctree();
    doc.get_mut(toc).unwrap().attrs.set_list("entry_refs", Vec::new());
    let env = BuildEnvironment::new(std::path::PathBuf::from("/tmp"));
    let collection = ToctreeCollection::default();
    let result = expand_toctrees(&mut doc, "index", &env, &collection).unwrap();
    assert_eq!(result.removed, 1);
    let reachable_toctrees = doc
        .preorder_ids()
        .into_iter()
        .filter(|&id| doc.get(id).is_ok_and(|node| node.kind == NodeKind::Toctree))
        .count();
    assert_eq!(reachable_toctrees, 0);
}

#[test]
fn citation_reference_and_xref_inline_normalization() {
    let mut doc = Document::new(None);
    let root = doc.root;
    let cite = doc.add(NodeKind::CitationReference, Some(root)).unwrap();
    let _ = doc.new_text("CITE1", Some(cite)).unwrap();
    let reference = doc.add(NodeKind::Reference, Some(root)).unwrap();
    let inline = doc.add(NodeKind::Inline, Some(reference)).unwrap();
    doc.get_mut(inline).unwrap().attrs.set_list("classes", vec!["xref".to_owned(), "py".to_owned()]);
    let _ = doc.new_text("Thing", Some(inline)).unwrap();

    assert_eq!(normalize_citation_references(&mut doc).unwrap(), 1);
    assert_eq!(normalize_xref_literals(&mut doc).unwrap(), 1);
    assert_eq!(doc.get(inline).unwrap().kind, NodeKind::Literal);
}
