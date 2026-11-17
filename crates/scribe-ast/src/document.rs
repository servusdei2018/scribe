//! The arena-backed [`Document`] and its safe construction/mutation API.
//!
//! # Arena invariants
//!
//! * `nodes[id.0]` is the node identified by `id`; the vector is **append-only**
//!   in the sense that removals never shift or reuse indices. A removed node is
//!   marked dead in `alive` but its slot is retained so all existing ids stay
//!   valid (dangling, not reused).
//! * `parents[id.0]` is the parent of `id`, or `None` for the root and for
//!   orphaned nodes. It is kept in sync by every structural method and can be
//!   rebuilt from scratch with [`Document::reindex_parents`].
//! * `root` is always `NodeId::ROOT` (`NodeId(0)`) and is the first node
//!   inserted. Attempts to remove or replace the root are rejected.
//!
//! These invariants make the arena cheap to mutate from the transform phases
//! without ever invalidating an id held by a domain resolver or writer.

use serde::{Deserialize, Serialize};

use crate::error::{AstError, AstResult};
use crate::node::{Node, NodeKind};
use crate::NodeId;

/// Extra document-level information that does not belong on a single node.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentMetadata {
    /// Sphinx docname (source path without extension), when known.
    pub docname: Option<String>,
    /// Resolved document title, when known.
    pub title: Option<String>,
    /// Document language code (`en`, `ja`, ...).
    pub language: Option<String>,
}

/// An arena of nodes forming one doctree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Document {
    /// Node arena. Index `n` is `NodeId(n)`.
    pub nodes: Vec<Node>,
    /// Parent id for each arena slot. Parallel to `nodes`.
    parents: Vec<Option<NodeId>>,
    /// Liveness flag for each arena slot. Parallel to `nodes`.
    alive: Vec<bool>,
    /// Always [`NodeId::ROOT`].
    pub root: NodeId,
    /// Source name of the document (also mirrored on the root node's
    /// `source` attribute for pseudo-XML parity).
    pub source_name: Option<String>,
    /// Additional document metadata.
    pub metadata: DocumentMetadata,
}

impl Document {
    /// Creates a document with a single `document` root node.
    ///
    /// If `source` is `Some`, the root's `source` attribute is set so that
    /// `to_pseudoxml()` emits `<document source="...">`, matching Docutils.
    pub fn new(source: Option<String>) -> Self {
        let mut root = Node::new(NodeKind::Document);
        if let Some(src) = &source {
            root.set_str("source", src.clone());
        }
        Self {
            nodes: vec![root],
            parents: vec![None],
            alive: vec![true],
            root: NodeId::ROOT,
            source_name: source,
            metadata: DocumentMetadata::default(),
        }
    }

    /// Returns a reference to the root node.
    pub fn root(&self) -> &Node {
        // Invariant: the root always exists.
        &self.nodes[NodeId::ROOT.0 as usize]
    }

    /// Returns a mutable reference to the root node.
    pub fn root_mut(&mut self) -> &mut Node {
        &mut self.nodes[NodeId::ROOT.0 as usize]
    }

    /// Number of arena slots (including dead ones).
    pub fn arena_len(&self) -> u32 {
        self.nodes.len() as u32
    }

    /// `true` when the arena contains only the root.
    pub fn is_empty(&self) -> bool {
        self.nodes.len() == 1
    }

    /// Checks whether `id` is a valid arena index (ignoring liveness).
    pub fn contains(&self, id: NodeId) -> bool {
        !id.is_null() && (id.0 as usize) < self.nodes.len()
    }

    /// Checks whether `id` refers to a live node.
    pub fn is_live(&self, id: NodeId) -> bool {
        self.contains(id) && self.alive[id.0 as usize]
    }

    /// Returns the node at `id`, or a structured error if out of range.
    pub fn get(&self, id: NodeId) -> AstResult<&Node> {
        self.check(id)?;
        Ok(&self.nodes[id.0 as usize])
    }

    /// Returns a mutable reference to the node at `id`, or a structured error.
    pub fn get_mut(&mut self, id: NodeId) -> AstResult<&mut Node> {
        self.check(id)?;
        Ok(&mut self.nodes[id.0 as usize])
    }

    /// Returns the parent of `id`, if any.
    pub fn parent(&self, id: NodeId) -> AstResult<Option<NodeId>> {
        self.check(id)?;
        Ok(self.parents[id.0 as usize])
    }

    /// Returns the child ids of `id`.
    pub fn children(&self, id: NodeId) -> AstResult<&[NodeId]> {
        Ok(&self.get(id)?.children)
    }

    /// Returns `true` if `ancestor` is an ancestor of `node`.
    pub fn is_ancestor(&self, ancestor: NodeId, node: NodeId) -> bool {
        let mut cur = self.parents.get(node.0 as usize).copied().flatten();
        while let Some(id) = cur {
            if id == ancestor {
                return true;
            }
            if id == NodeId::ROOT {
                break;
            }
            cur = self.parents.get(id.0 as usize).copied().flatten();
        }
        false
    }

    /// Iterates from `id` up to the root, yielding `id` first.
    pub fn ancestors(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        let mut cur = Some(id);
        std::iter::from_fn(move || {
            let here = cur?;
            cur = self.parents.get(here.0 as usize).copied().flatten();
            Some(here)
        })
    }

    /// Creates a `Text` leaf and appends it to `parent` (or leaves it orphaned
    /// when `parent` is `None`), returning its id.
    pub fn new_text(&mut self, text: impl Into<String>, parent: Option<NodeId>) -> AstResult<NodeId> {
        self.add_node(Node::text(text), parent)
    }

    /// Creates an element node of `kind`, attaches it to `parent` (if any) and
    /// returns its id.
    pub fn add(&mut self, kind: NodeKind, parent: Option<NodeId>) -> AstResult<NodeId> {
        self.add_node(Node::new(kind), parent)
    }

    /// Appends `node` to the arena and, when `parent` is `Some`, to that
    /// parent's children. Returns the new node's id.
    pub fn add_node(&mut self, node: Node, parent: Option<NodeId>) -> AstResult<NodeId> {
        if let Some(p) = parent {
            self.check(p)?;
        }
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(node);
        self.parents.push(None);
        self.alive.push(true);
        if let Some(p) = parent {
            self.nodes[p.0 as usize].children.push(id);
            self.parents[id.0 as usize] = Some(p);
        }
        Ok(id)
    }

    /// Appends `child` (already in the arena) to `parent`'s children.
    pub fn append_child(&mut self, parent: NodeId, child: NodeId) -> AstResult<()> {
        self.check(parent)?;
        self.check(child)?;
        if parent == child || self.is_ancestor(child, parent) {
            return Err(AstError::InvalidMutation("cycle in append_child".into()));
        }
        if let Some(old) = self.parents[child.0 as usize] {
            self.detach_from_parent(child, old)?;
        }
        self.nodes[parent.0 as usize].children.push(child);
        self.parents[child.0 as usize] = Some(parent);
        Ok(())
    }

    /// Inserts `child` at position `index` in `parent`'s children.
    pub fn insert_child(&mut self, parent: NodeId, index: usize, child: NodeId) -> AstResult<()> {
        self.check(parent)?;
        self.check(child)?;
        if parent == child || self.is_ancestor(child, parent) {
            return Err(AstError::InvalidMutation("cycle in insert_child".into()));
        }
        let old_parent = self.parents[child.0 as usize];
        if let Some(old) = old_parent {
            self.detach_from_parent(child, old)?;
        }
        let children = &mut self.nodes[parent.0 as usize].children;
        if index > children.len() {
            return Err(AstError::InvalidMutation(format!(
                "insert index {index} out of range for node {parent}"
            )));
        }
        children.insert(index, child);
        self.parents[child.0 as usize] = Some(parent);
        Ok(())
    }

    /// Removes and returns the child id at position `index` in `parent`.
    pub fn remove_child(&mut self, parent: NodeId, index: usize) -> AstResult<NodeId> {
        self.check(parent)?;
        let children = &mut self.nodes[parent.0 as usize].children;
        if index >= children.len() {
            return Err(AstError::InvalidMutation(format!(
                "remove index {index} out of range for node {parent}"
            )));
        }
        let child = children.remove(index);
        self.parents[child.0 as usize] = None;
        Ok(child)
    }

    /// Replaces `parent`'s entire child list with `children`.
    pub fn replace_children(&mut self, parent: NodeId, children: Vec<NodeId>) -> AstResult<()> {
        self.check(parent)?;
        for &child in &children {
            self.check(child)?;
            if child == parent || self.is_ancestor(child, parent) {
                return Err(AstError::InvalidMutation("cycle in replace_children".into()));
            }
        }
        for &old in &self.nodes[parent.0 as usize].children {
            if !children.contains(&old) {
                self.parents[old.0 as usize] = None;
            }
        }
        for &child in &children {
            self.parents[child.0 as usize] = Some(parent);
        }
        self.nodes[parent.0 as usize].children = children;
        Ok(())
    }

    /// Splices `parent`'s children: removes `range` and inserts `replacement`.
    pub fn splice_children(
        &mut self,
        parent: NodeId,
        range: std::ops::Range<usize>,
        replacement: Vec<NodeId>,
    ) -> AstResult<()> {
        self.check(parent)?;
        {
            let children = &self.nodes[parent.0 as usize].children;
            if range.start > range.end || range.end > children.len() {
                return Err(AstError::InvalidMutation(format!(
                    "splice range {range:?} out of range for node {parent}"
                )));
            }
            for &child in replacement.iter().chain(children[range.clone()].iter()) {
                if child == parent || self.is_ancestor(child, parent) {
                    return Err(AstError::InvalidMutation("cycle in splice_children".into()));
                }
            }
        }
        for &old in self.nodes[parent.0 as usize].children[range.clone()].iter() {
            self.parents[old.0 as usize] = None;
        }
        for &child in &replacement {
            self.parents[child.0 as usize] = Some(parent);
        }
        self.nodes[parent.0 as usize].children.splice(range, replacement);
        Ok(())
    }

    /// Replaces the node at `id` with `new_node`, keeping `id` stable.
    ///
    /// The old node's children become orphaned (still in the arena, but no
    /// longer reachable); the new node's children are linked to `id`.
    pub fn replace_node(&mut self, id: NodeId, new_node: Node) -> AstResult<()> {
        self.check(id)?;
        if id == NodeId::ROOT {
            return Err(AstError::InvalidMutation("cannot replace the root node".into()));
        }
        let new_children = new_node.children.clone();
        for &child in &new_children {
            self.check(child)?;
            if child == id || self.is_ancestor(child, id) {
                return Err(AstError::InvalidMutation("cycle in replace_node".into()));
            }
        }
        let old_children = std::mem::take(&mut self.nodes[id.0 as usize].children);
        for child in old_children {
            if self.parents[child.0 as usize] == Some(id) {
                self.parents[child.0 as usize] = None;
            }
        }
        self.nodes[id.0 as usize] = new_node;
        for child in new_children {
            self.parents[child.0 as usize] = Some(id);
        }
        Ok(())
    }

    /// Detaches and marks dead `id` and all of its descendants.
    pub fn remove_node(&mut self, id: NodeId) -> AstResult<()> {
        self.check(id)?;
        if id == NodeId::ROOT {
            return Err(AstError::InvalidMutation("cannot remove the root node".into()));
        }
        if let Some(parent) = self.parents[id.0 as usize] {
            self.detach_from_parent(id, parent)?;
        }
        let ids = self.subtree_ids(id);
        for dead in ids {
            self.alive[dead.0 as usize] = false;
            self.parents[dead.0 as usize] = None;
        }
        Ok(())
    }

    /// Replaces `id` in its parent's child list with `id`'s own children,
    /// effectively "unwrapping" it. Used heavily by transforms.
    pub fn replace_with_children(&mut self, id: NodeId) -> AstResult<()> {
        self.check(id)?;
        if id == NodeId::ROOT {
            return Err(AstError::InvalidMutation("cannot unwrap the root node".into()));
        }
        let parent = self.parents[id.0 as usize].ok_or_else(|| {
            AstError::InvalidMutation(format!("node {id} has no parent to unwrap into"))
        })?;
        let index = self.nodes[parent.0 as usize]
            .children
            .iter()
            .position(|&c| c == id)
            .ok_or(AstError::NotLive { id: id.0 })?;
        let children = self.nodes[id.0 as usize].children.clone();
        self.splice_children(parent, index..index + 1, children.clone())?;
        self.alive[id.0 as usize] = false;
        self.parents[id.0 as usize] = None;
        Ok(())
    }

    /// Returns all live node ids in depth-first pre-order, starting at the root.
    pub fn preorder_ids(&self) -> Vec<NodeId> {
        let mut out = Vec::with_capacity(self.nodes.len());
        self.collect_preorder(self.root, &mut out);
        out
    }

    /// Returns all live node ids in depth-first post-order (children before parent).
    pub fn postorder_ids(&self) -> Vec<NodeId> {
        let mut out = Vec::with_capacity(self.nodes.len());
        self.collect_postorder(self.root, &mut out);
        out
    }

    /// Returns the ids of the subtree rooted at `id` in pre-order.
    pub fn subtree_ids(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        if self.is_live(id) {
            self.collect_preorder(id, &mut out);
        }
        out
    }

    /// Iterates over all live nodes in arena order (id order).
    pub fn iter(&self) -> impl Iterator<Item = (NodeId, &Node)> {
        self.nodes
            .iter()
            .enumerate()
            .filter(|(i, _)| self.alive.get(*i).copied().unwrap_or(false))
            .map(|(i, node)| (NodeId(i as u32), node))
    }

    /// Rebuilds the `parents` array from the tree reachable from the root.
    ///
    /// Call this after editing a node's `children` vector directly (for
    /// example inside a [`Mutator`](crate::Mutator)). It does not change
    /// liveness.
    pub fn reindex_parents(&mut self) {
        self.parents.fill(None);
        self.reindex_from(self.root);
    }

    /// Builds a document from the canonical programmatic fixture used by the
    /// pseudo-XML tests. Keeping this in the library makes the Rust and oracle
    /// fixtures impossible to drift apart within a test binary.
    ///
    /// # Safety of internal `expect` calls
    ///
    /// Every `expect` below is applied to an id that was just returned by
    /// `add` / `new_text` within this same function, or to
    /// [`Node::pending_xref_mut`](Node::pending_xref_mut) on a node whose kind
    /// is set to [`NodeKind::PendingXref`] two lines earlier. The arena cannot
    /// have changed between those calls, so failure is statically impossible;
    /// the `expect`s are documented invariants, not error handling.
    pub fn fixture_document() -> Self {
        let mut doc = Document::new(Some("test.rst".into()));

        let section = doc.add(NodeKind::Section, Some(doc.root)).expect("arena");
        doc.get_mut(section).expect("arena").set_list("ids", vec!["a-section".into()]);
        doc.get_mut(section).expect("arena").set_list("names", vec!["a section".into()]);
        {
            let title = doc.add(NodeKind::Title, Some(section)).expect("arena");
            doc.new_text("A Section", Some(title)).expect("arena");
        }
        {
            let para = doc.add(NodeKind::Paragraph, Some(section)).expect("arena");
            doc.new_text("See ", Some(para)).expect("arena");
            let xr = doc.add(NodeKind::PendingXref, Some(para)).expect("arena");
            {
                let node = doc.get_mut(xr).expect("arena");
                let mut xr_view = node.pending_xref_mut().expect("pending xref");
                xr_view.set_refdomain(Some("py".into()));
                xr_view.set_reftype(Some("func".into()));
                xr_view.set_reftarget(Some("io.StringIO".into()));
                xr_view.set_refexplicit(Some(false));
                xr_view.set_refspecific(Some(false));
                xr_view.set_refwarn(Some(true));
                xr_view.set_refdoc(Some("index".into()));
                xr_view.set_refid(Some(String::new()));
                xr_view.set_refuri(Some(String::new()));
                xr_view.set_reftitle(Some(String::new()));
                xr_view.set_refname(Some("StringIO".into()));
                xr_view.set_classes(vec!["xref".into(), "py".into(), "func".into()]);
            }
            let lit = doc.add(NodeKind::Literal, Some(xr)).expect("arena");
            doc.new_text("StringIO", Some(lit)).expect("arena");
            doc.new_text(" done.", Some(para)).expect("arena");
        }

        {
            let para = doc.add(NodeKind::Paragraph, Some(section)).expect("arena");
            doc.new_text("This is ", Some(para)).expect("arena");
            let em = doc.add(NodeKind::Emphasis, Some(para)).expect("arena");
            doc.new_text("emphasis", Some(em)).expect("arena");
            doc.new_text(" and ", Some(para)).expect("arena");
            let lit = doc.add(NodeKind::Literal, Some(para)).expect("arena");
            doc.new_text("literal", Some(lit)).expect("arena");
            doc.new_text(" and ", Some(para)).expect("arena");
            let strong = doc.add(NodeKind::Strong, Some(para)).expect("arena");
            doc.new_text("strong", Some(strong)).expect("arena");
            doc.new_text(".", Some(para)).expect("arena");
        }

        {
            let list = doc.add(NodeKind::BulletList, Some(section)).expect("arena");
            doc.get_mut(list).expect("arena").set_str("bullet", "-");
            for item_text in ["first item", "second item"] {
                let item = doc.add(NodeKind::ListItem, Some(list)).expect("arena");
                let para = doc.add(NodeKind::Paragraph, Some(item)).expect("arena");
                doc.new_text(item_text, Some(para)).expect("arena");
            }
        }

        {
            let note = doc.add(NodeKind::Note, Some(section)).expect("arena");
            let para = doc.add(NodeKind::Paragraph, Some(note)).expect("arena");
            doc.new_text("This is a note admonition.", Some(para)).expect("arena");
        }

        doc
    }

    // ----- internal helpers -------------------------------------------------

    fn check(&self, id: NodeId) -> AstResult<()> {
        if id.is_null() {
            return Err(AstError::NullNodeId);
        }
        if id.0 as usize >= self.nodes.len() {
            return Err(AstError::InvalidNodeId {
                id: id.0,
                len: self.nodes.len() as u32,
            });
        }
        Ok(())
    }

    fn detach_from_parent(&mut self, child: NodeId, parent: NodeId) -> AstResult<()> {
        self.check(parent)?;
        let children = &mut self.nodes[parent.0 as usize].children;
        if let Some(pos) = children.iter().position(|&c| c == child) {
            children.remove(pos);
        }
        self.parents[child.0 as usize] = None;
        Ok(())
    }

    fn collect_preorder(&self, id: NodeId, out: &mut Vec<NodeId>) {
        if !self.is_live(id) {
            return;
        }
        out.push(id);
        for &child in &self.nodes[id.0 as usize].children {
            self.collect_preorder(child, out);
        }
    }

    fn collect_postorder(&self, id: NodeId, out: &mut Vec<NodeId>) {
        if !self.is_live(id) {
            return;
        }
        for &child in &self.nodes[id.0 as usize].children {
            self.collect_postorder(child, out);
        }
        out.push(id);
    }

    fn reindex_from(&mut self, id: NodeId) {
        if !self.contains(id) {
            return;
        }
        let children = self.nodes[id.0 as usize].children.clone();
        for child in children {
            if self.contains(child) {
                self.parents[child.0 as usize] = Some(id);
                self.reindex_from(child);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_document_has_live_root() {
        let doc = Document::new(Some("a.rst".into()));
        assert_eq!(doc.root(), &doc.nodes[0]);
        assert_eq!(doc.root().kind, NodeKind::Document);
        assert_eq!(doc.root().get_str("source").unwrap(), Some("a.rst"));
        assert_eq!(doc.root(), doc.root());
        assert_eq!(doc.parent(doc.root).unwrap(), None);
        assert!(doc.is_live(doc.root));
        assert!(doc.is_empty());
    }

    #[test]
    fn out_of_range_and_null_ids_return_errors_not_panics() {
        let mut doc = Document::new(None);
        let bad = NodeId(999);
        assert!(matches!(doc.get(bad), Err(AstError::InvalidNodeId { .. })));
        assert!(matches!(doc.get_mut(bad), Err(AstError::InvalidNodeId { .. })));
        assert!(matches!(doc.get(NodeId::NULL), Err(AstError::NullNodeId)));
        assert!(doc.parent(bad).is_err());
        assert!(doc.children(bad).is_err());
        assert!(matches!(
            doc.append_child(bad, doc.root),
            Err(AstError::InvalidNodeId { .. })
        ));
    }

    #[test]
    fn add_and_parent_lookup() {
        let mut doc = Document::new(None);
        let section = doc.add(NodeKind::Section, Some(doc.root)).unwrap();
        let title = doc.add(NodeKind::Title, Some(section)).unwrap();
        assert_eq!(doc.parent(section).unwrap(), Some(doc.root));
        assert_eq!(doc.parent(title).unwrap(), Some(section));
        assert_eq!(doc.children(doc.root).unwrap(), &[section]);
        assert!(doc.is_ancestor(section, title));
        assert!(!doc.is_ancestor(title, section));
        assert_eq!(doc.ancestors(title).collect::<Vec<_>>(), vec![title, section, doc.root]);
    }

    #[test]
    fn traversal_orders() {
        let doc = Document::fixture_document();
        let pre = doc.preorder_ids();
        let post = doc.postorder_ids();
        assert_eq!(pre.first(), Some(&doc.root));
        assert_eq!(post.last(), Some(&doc.root));
        assert_eq!(pre.len(), post.len());
        assert_eq!(pre.len(), doc.iter().count());
    }

    #[test]
    fn replace_children_and_splice() {
        let mut doc = Document::new(None);
        let a = doc.add(NodeKind::Paragraph, Some(doc.root)).unwrap();
        let b = doc.add(NodeKind::Paragraph, Some(doc.root)).unwrap();
        let c = doc.add(NodeKind::Paragraph, Some(doc.root)).unwrap();
        doc.replace_children(doc.root, vec![a, c]).unwrap();
        assert_eq!(doc.children(doc.root).unwrap(), &[a, c]);
        assert_eq!(doc.parent(b).unwrap(), None);
        doc.insert_child(doc.root, 1, b).unwrap();
        assert_eq!(doc.children(doc.root).unwrap(), &[a, b, c]);
        doc.splice_children(doc.root, 1..2, vec![]).unwrap();
        assert_eq!(doc.children(doc.root).unwrap(), &[a, c]);
    }

    #[test]
    fn replace_remove_and_unwrap() {
        let mut doc = Document::new(None);
        let p = doc.add(NodeKind::Paragraph, Some(doc.root)).unwrap();
        let t1 = doc.new_text("a", Some(p)).unwrap();
        let t2 = doc.new_text("b", Some(p)).unwrap();

        doc.replace_node(t1, Node::text("A")).unwrap();
        assert_eq!(doc.get(t1).unwrap().text_content(), Some("A"));

        doc.replace_with_children(p).unwrap();
        assert_eq!(doc.children(doc.root).unwrap(), &[t1, t2]);
        assert!(!doc.is_live(p));

        doc.remove_node(t2).unwrap();
        assert!(!doc.is_live(t2));
        assert_eq!(doc.children(doc.root).unwrap(), &[t1]);

        assert!(doc.remove_node(doc.root).is_err());
        assert!(doc.replace_node(doc.root, Node::text("x")).is_err());
    }

    #[test]
    fn cycle_detection() {
        let mut doc = Document::new(None);
        let a = doc.add(NodeKind::Container, Some(doc.root)).unwrap();
        let b = doc.add(NodeKind::Container, Some(a)).unwrap();
        assert!(doc.append_child(b, a).is_err());
        assert!(doc.replace_children(a, vec![a]).is_err());
    }
}
