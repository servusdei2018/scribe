//! Safe in-place tree rewriting.
//!
//! The arena is index-based and append-only, so a [`NodeId`] survives any
//! structural change. [`Mutator`] is called once per live node in pre-order and
//! receives `&mut Node` for attribute edits. Structural changes to the node
//! itself are requested through the returned [`Mutation`] and applied by
//! [`transform`] after the mutable borrow ends, keeping the API borrow-checker
//! friendly.
//!
//! If a mutator edits `node.children` directly instead of returning
//! [`Mutation::ReplaceChildren`], call [`Document::reindex_parents`]
//! afterwards (transform does this automatically).

use std::ops::Range;

use crate::error::AstResult;
use crate::node::Node;
use crate::{Document, NodeId};

/// A structural change requested by a [`Mutator`] for one node.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Mutation {
    /// No structural change (attribute edits via `&mut Node` are kept).
    #[default]
    None,
    /// Replace the visited node in place, preserving its id.
    ReplaceWith(Node),
    /// Detach and kill the visited node and its subtree.
    Remove,
    /// Replace the node's child list with `children`.
    ReplaceChildren(Vec<NodeId>),
    /// Splice the node's child list.
    SpliceChildren {
        /// Range of existing children to remove.
        range: Range<usize>,
        /// Replacement child ids.
        replacement: Vec<NodeId>,
    },
    /// Unwrap the node: splice its children into its parent at its position.
    ReplaceWithChildren,
}

/// A transform pass over a document.
pub trait Mutator {
    /// Called in pre-order for each live node.
    ///
    /// `node` may be freely mutated (attributes, children). Return a
    /// [`Mutation`] for changes that affect the node's own position in the
    /// tree; those are applied once the borrow of `node` is released.
    fn mutate(&mut self, node: &mut Node, id: NodeId) -> AstResult<Mutation>;
}

/// Applies `mutator` to every live node in pre-order.
///
/// The id list is snapshotted before mutation, so nodes created during a pass
/// are not visited by that same pass (but remain in the arena for the next).
pub fn transform<M: Mutator + ?Sized>(doc: &mut Document, mutator: &mut M) -> AstResult<()> {
    let ids = doc.preorder_ids();
    for id in ids {
        if !doc.is_live(id) {
            continue;
        }
        let mutation = {
            let node = doc.get_mut(id)?;
            mutator.mutate(node, id)?
        };
        apply(doc, id, mutation)?;
    }
    doc.reindex_parents();
    Ok(())
}

/// Applies a single [`Mutation`] to `id`.
pub fn apply_mutation(doc: &mut Document, id: NodeId, mutation: Mutation) -> AstResult<()> {
    let result = apply(doc, id, mutation);
    if result.is_ok() {
        doc.reindex_parents();
    }
    result
}

fn apply(doc: &mut Document, id: NodeId, mutation: Mutation) -> AstResult<()> {
    match mutation {
        Mutation::None => Ok(()),
        Mutation::ReplaceWith(node) => doc.replace_node(id, node),
        Mutation::Remove => doc.remove_node(id),
        Mutation::ReplaceChildren(children) => doc.replace_children(id, children),
        Mutation::SpliceChildren { range, replacement } => {
            doc.splice_children(id, range, replacement)
        }
        Mutation::ReplaceWithChildren => doc.replace_with_children(id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NodeKind;

    struct UppercaseText;
    impl Mutator for UppercaseText {
        fn mutate(&mut self, node: &mut Node, _id: NodeId) -> AstResult<Mutation> {
            if let Some(text) = node.text_content() {
                let upper = text.to_uppercase();
                node.set_text(upper);
            }
            Ok(Mutation::None)
        }
    }

    struct UnwrapPendingXref;
    impl Mutator for UnwrapPendingXref {
        fn mutate(&mut self, node: &mut Node, _id: NodeId) -> AstResult<Mutation> {
            if node.kind == NodeKind::PendingXref {
                Ok(Mutation::ReplaceWithChildren)
            } else {
                Ok(Mutation::None)
            }
        }
    }

    #[test]
    fn transform_edits_attributes_in_place() {
        let mut doc = Document::fixture_document();
        transform(&mut doc, &mut UppercaseText).unwrap();
        let texts: Vec<String> = doc
            .iter()
            .filter(|(_, n)| n.kind == NodeKind::Text)
            .filter_map(|(_, n)| n.text_content().map(str::to_owned))
            .collect();
        assert!(texts.contains(&"SEE ".to_owned()));
        assert!(texts.contains(&"A SECTION".to_owned()));
    }

    #[test]
    fn transform_can_unwrap_nodes() {
        let mut doc = Document::fixture_document();
        let xr_id = doc
            .iter()
            .find(|(_, n)| n.kind == NodeKind::PendingXref)
            .map(|(id, _)| id)
            .expect("fixture has pending_xref");
        let parent = doc.parent(xr_id).unwrap().expect("has parent");
        let before = doc.children(parent).unwrap().len();
        transform(&mut doc, &mut UnwrapPendingXref).unwrap();
        assert!(!doc.is_live(xr_id));
        let after = doc.iter().find(|(id, _)| *id == parent);
        assert!(after.is_some());
        let lit = doc
            .iter()
            .find(|(_, n)| {
                n.kind == NodeKind::Literal
                    && n.children.iter().any(|c| {
                        doc.get(*c)
                            .ok()
                            .and_then(|child| child.text_content())
                            == Some("StringIO")
                    })
            })
            .map(|(id, _)| id);
        assert!(lit.is_some(), "literal node was unwrapped into the paragraph");
        assert_eq!(before, doc.children(parent).unwrap().len());
    }

    #[test]
    fn apply_mutation_replace_and_remove() {
        let mut doc = Document::new(None);
        let p = doc.add(NodeKind::Paragraph, Some(doc.root)).unwrap();
        apply_mutation(&mut doc, p, Mutation::ReplaceWith(Node::text("x"))).unwrap();
        assert_eq!(doc.get(p).unwrap().kind, NodeKind::Text);
        apply_mutation(&mut doc, p, Mutation::Remove).unwrap();
        assert!(!doc.is_live(p));
    }
}
