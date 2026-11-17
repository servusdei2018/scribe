//! Unified document AST and doctree model for `scribe`.
//!
//! # Frozen cross-phase contract (Phase 03)
//!
//! This crate defines the shared currency of the whole engine. The following
//! public shapes are **frozen** and may not change without orchestrator
//! approval, because Phases 04–07 (parsers), 09–10 (toctree/transforms),
//! 11–14 (domains/xrefs) and 17–25 (writers) all code against them:
//!
//! * [`Document`] — an append-only `Vec` arena of [`Node`]s plus a root
//!   [`NodeId`], a source name and [`DocumentMetadata`].
//! * [`NodeId`] — a `Copy + Eq + Hash` newtype over `u32`; `NodeId(0)` is always
//!   the document root and [`NodeId::NULL`] is the "no node" sentinel.
//! * [`Node`] / [`NodeKind`] — a flat record (`kind`, typed [`attrs`](Node::attrs),
//!   `children`, `source`, `span`) and the exhaustive set of Docutils + Sphinx
//!   node kinds.
//! * The [`pending_xref`](NodeKind::PendingXref) shape, exposed through
//!   [`Node::pending_xref`] / [`Node::pending_xref_mut`] and the typed
//!   [`PendingXref`] / [`PendingXrefMut`] views.
//!
//! See `ARCHITECTURE.md` §5 for the full written contract.
//!
//! # Quick start
//!
//! ```
//! use scribe_ast::{Document, NodeKind};
//!
//! let mut doc = Document::new(Some("index.rst".into()));
//! let section = doc.add(NodeKind::Section, Some(doc.root))?;
//! doc.get_mut(section)?.set_list("ids", vec!["intro".into()]);
//! let title = doc.add(NodeKind::Title, Some(section))?;
//! doc.new_text("Introduction", Some(title))?;
//! let pseudo = doc.to_pseudoxml()?;
//! assert!(pseudo.contains("<section ids=\"intro\">"));
//! # Ok::<(), scribe_ast::AstError>(())
//! ```
//!
//! # Traversal and rewriting
//!
//! Read-only passes implement [`Visitor`] and call [`walk`]. Transform passes
//! implement [`Mutator`] and call [`transform`]; structural changes that would
//! clash with the arena borrow are described by [`Mutation`] and applied after
//! the borrow ends. Ids never move, so a writer, domain resolver or xref pass
//! can hold ids across any number of passes.

mod document;
mod error;
mod mutator;
mod node;
mod node_id;
mod serialize;
mod visitor;

pub use document::{Document, DocumentMetadata};
pub use error::{AstError, AstResult};
pub use mutator::{apply_mutation, transform, Mutation, Mutator};
pub use node::{
    AttributeValue, Attributes, Node, NodeKind, PendingXref, PendingXrefMut, SourcePos, Span,
};
pub use node_id::NodeId;
pub use visitor::{walk, walk_postorder, walk_preorder, Visitor};

/// The phase that implemented this crate's functionality.
pub const IMPLEMENTATION_PHASE: &str = "Phase 03";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_contract_types_are_reachable() {
        let doc = Document::new(None);
        let _: NodeId = doc.root;
        let _: &Node = doc.root();
        let _: Option<PendingXref<'_>> = doc.root().pending_xref();
        let _: AstResult<String> = doc.to_json();
        let _: AstResult<String> = doc.to_pseudoxml();
    }
}
