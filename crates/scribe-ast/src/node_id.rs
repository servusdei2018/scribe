//! Compact, `u32`-indexed arena identifiers.
//!
//! A [`NodeId`] is nothing more than an index into [`Document::nodes`](crate::Document::nodes).
//! Because the arena is append-only (nodes are never physically removed, only
//! marked dead), an id remains stable for the entire lifetime of a document.
//! This is what lets the transform, domain and writer phases freely hold on to
//! ids across mutations without invalidating one another.

use serde::{Deserialize, Serialize};

/// A stable index into a [`Document`](crate::Document) arena.
///
/// `NodeId(0)` is always the document root. [`NodeId::NULL`] is a sentinel that
/// is never a valid arena slot and is used for "no node" values such as the
/// parent of the root.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub u32);

impl NodeId {
    /// Sentinel meaning "no node"; equal to `u32::MAX` and never a valid index.
    pub const NULL: NodeId = NodeId(u32::MAX);

    /// The id of the root node in every document.
    pub const ROOT: NodeId = NodeId(0);

    /// Returns `true` if this is the null sentinel.
    pub fn is_null(self) -> bool {
        self == Self::NULL
    }

    /// Returns the raw `u32` index.
    pub fn index(self) -> u32 {
        self.0
    }
}

impl Default for NodeId {
    fn default() -> Self {
        Self::NULL
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_null() {
            write!(f, "NodeId(NULL)")
        } else {
            write!(f, "NodeId({})", self.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::NodeId;

    #[test]
    fn copy_eq_hash() {
        let a = NodeId(7);
        let b = a;
        assert_eq!(a, b);
        let mut set = HashSet::new();
        set.insert(a);
        assert!(set.contains(&b));
        assert!(!a.is_null());
        assert!(NodeId::NULL.is_null());
        assert_eq!(NodeId::ROOT.index(), 0);
    }
}
