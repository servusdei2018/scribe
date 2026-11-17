//! Read-only tree traversal.
//!
//! [`Visitor`] provides a generic `visit_node` / `depart_node` pair plus a
//! default [`walk`](Visitor::walk) that performs a depth-first walkabove:
//! `visit_node` is called on entry (pre-order) and `depart_node` after all
//! children (post-order). Free functions [`walk_preorder`] and
//! [`walk_postorder`] call only one of the two hooks. Hooks receive `&Node`, so
//! there is no borrow conflict with the arena, and implementations may freely
//! hold their own accumulators.

use crate::error::AstResult;
use crate::node::Node;
use crate::{Document, NodeId};

/// A read-only visitor over a [`Document`].
pub trait Visitor {
    /// Called when entering `node`.
    fn visit_node(&mut self, _node: &Node, _id: NodeId) {}

    /// Called after all children of `node` have been visited.
    fn depart_node(&mut self, _node: &Node, _id: NodeId) {}

    /// Walks `doc` using this visitor (visit then depart for every live node).
    fn walk(&mut self, doc: &Document) -> AstResult<()> {
        walk(doc, self)
    }
}

/// Performs a depth-first walkabove of `doc` with `visitor`.
pub fn walk<V: Visitor + ?Sized>(doc: &Document, visitor: &mut V) -> AstResult<()> {
    walk_from(doc, doc.root, visitor)
}

fn walk_from<V: Visitor + ?Sized>(doc: &Document, id: NodeId, visitor: &mut V) -> AstResult<()> {
    if !doc.is_live(id) {
        return Ok(());
    }
    let node = doc.get(id)?;
    visitor.visit_node(node, id);
    for &child in &node.children {
        walk_from(doc, child, visitor)?;
    }
    let node = doc.get(id)?;
    visitor.depart_node(node, id);
    Ok(())
}

/// Calls only [`Visitor::visit_node`], in pre-order.
pub fn walk_preorder<V: Visitor + ?Sized>(doc: &Document, visitor: &mut V) -> AstResult<()> {
    for id in doc.preorder_ids() {
        visitor.visit_node(doc.get(id)?, id);
    }
    Ok(())
}

/// Calls only [`Visitor::depart_node`], in post-order.
pub fn walk_postorder<V: Visitor + ?Sized>(doc: &Document, visitor: &mut V) -> AstResult<()> {
    for id in doc.postorder_ids() {
        visitor.depart_node(doc.get(id)?, id);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NodeKind;

    #[derive(Default)]
    struct Trace(Vec<&'static str>);

    impl Visitor for Trace {
        fn visit_node(&mut self, node: &Node, _id: NodeId) {
            self.0.push(match node.kind {
                NodeKind::Document => "doc>",
                NodeKind::Section => "sec>",
                NodeKind::Title => "title>",
                NodeKind::Text => "text",
                _ => "other",
            });
        }
        fn depart_node(&mut self, node: &Node, _id: NodeId) {
            if node.kind == NodeKind::Section {
                self.0.push("sec<");
            }
        }
    }

    #[test]
    fn visit_and_depart_order() {
        let doc = Document::new(None);
        let mut trace = Trace::default();
        trace.walk(&doc).unwrap();
        assert_eq!(trace.0, vec!["doc>"]);
    }

    #[test]
    fn full_fixture_walk_is_pre_then_post() {
        let doc = Document::fixture_document();
        let mut trace = Trace::default();
        walk(&doc, &mut trace).unwrap();
        assert_eq!(trace.0.first(), Some(&"doc>"));
        assert_eq!(trace.0.iter().filter(|t| **t == "sec>").count(), 1);
        assert_eq!(trace.0.iter().filter(|t| **t == "sec<").count(), 1);
        assert!(trace.0.iter().position(|t| *t == "sec>").unwrap()
            < trace.0.iter().position(|t| *t == "sec<").unwrap());

        #[derive(Default)]
        struct Counter(usize);
        impl Visitor for Counter {
            fn visit_node(&mut self, _node: &Node, _id: NodeId) { self.0 += 1; }
            fn depart_node(&mut self, _node: &Node, _id: NodeId) { self.0 += 1; }
        }
        let mut pre = Counter::default();
        walk_preorder(&doc, &mut pre).unwrap();
        let mut post = Counter::default();
        walk_postorder(&doc, &mut post).unwrap();
        assert_eq!(pre.0, doc.preorder_ids().len());
        assert_eq!(post.0, doc.postorder_ids().len());
    }
}
