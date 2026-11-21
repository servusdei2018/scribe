//! Owned value types shared by every domain (frozen Phase 11 contract).
//!
//! These types deliberately avoid any reference to `scribe-core` so the
//! dependency graph stays acyclic: `scribe-core` depends on `scribe-domains`,
//! not the other way around.  Build-environment access is provided through the
//! [`DomainEnv`](crate::traits::DomainEnv) trait.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use scribe_ast::NodeKind;

/// Description of one object type exposed by a domain.
///
/// Mirrors upstream `sphinx.domains.ObjType`: a human-readable label, the roles
/// that can refer to objects of this type, and the search priority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectType {
    /// Key used in the domain's object table (e.g. `func`, `label`).
    pub name: String,
    /// Localized human-readable name (e.g. `reference label`).
    pub lname: String,
    /// Roles that resolve to this object type, in upstream declaration order.
    pub roles: Vec<String>,
    /// Search priority (`1` default, `0` important, `2` unimportant, `-1` hidden).
    pub priority: i8,
}

impl ObjectType {
    /// Creates an object type with the normal search priority of `1`.
    pub fn new(name: impl Into<String>, lname: impl Into<String>, roles: &[&str]) -> Self {
        Self {
            name: name.into(),
            lname: lname.into(),
            roles: roles.iter().map(|role| (*role).to_owned()).collect(),
            priority: 1,
        }
    }

    /// Builder: overrides the search priority.
    pub fn with_priority(mut self, priority: i8) -> Self {
        self.priority = priority;
        self
    }
}

/// One object in a domain's inventory (`Domain.get_objects()` item).
///
/// This is the six-uple upstream yields, converted to an owned struct with an
/// explicit, deterministic ordering contract (see `ARCHITECTURE.md` §15).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ObjectEntry {
    /// Fully qualified object name.
    pub name: String,
    /// Name to display when searching/linking.
    pub display_name: String,
    /// Object type key (a key in `Domain::object_types`).
    pub objtype: String,
    /// Document where the object is declared.
    pub docname: String,
    /// Anchor (`id`) inside that document; empty for whole-document entries.
    pub anchor: String,
    /// Search priority (`-1`, `0`, `1`, `2`).
    pub priority: i8,
    /// Module synopsis, used by the Python module index (`py-modindex`).
    pub synopsis: String,
    /// Module platform string, used by the Python module index.
    pub platform: String,
}

impl ObjectEntry {
    /// Creates an object entry.
    pub fn new(
        name: impl Into<String>,
        display_name: impl Into<String>,
        objtype: impl Into<String>,
        docname: impl Into<String>,
        anchor: impl Into<String>,
        priority: i8,
    ) -> Self {
        Self {
            name: name.into(),
            display_name: display_name.into(),
            objtype: objtype.into(),
            docname: docname.into(),
            anchor: anchor.into(),
            priority,
            synopsis: String::new(),
            platform: String::new(),
        }
    }

    /// True when this entry is only an alias of another entry (same
    /// `(objtype, docname, anchor)` triple, different `name`).
    pub fn is_alias_of(&self, other: &Self) -> bool {
        self.objtype == other.objtype
            && self.docname == other.docname
            && self.anchor == other.anchor
            && self.name != other.name
    }
}

/// The AST node kind a resolver wants the pipeline to emit.
///
/// Upstream returns a ready-made `nodes.reference` subclass; our frozen AST
/// cannot construct arbitrary Python classes, so the resolver returns this
/// small descriptor and the pipeline materializes the node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReferenceNodeKind {
    /// `nodes.reference` (the default for nearly every role).
    Reference,
    /// `sphinx.addnodes.number_reference` (`:numref:`).
    NumberReference,
    /// `sphinx.addnodes.download_reference` (`:download:`).
    DownloadReference,
}

impl ReferenceNodeKind {
    /// Maps the descriptor onto a frozen AST node kind.
    pub fn node_kind(self) -> NodeKind {
        match self {
            Self::Reference => NodeKind::Reference,
            Self::NumberReference => NodeKind::NumberReference,
            Self::DownloadReference => NodeKind::DownloadReference,
        }
    }

    /// Upstream pseudo-XML tag name.
    pub fn tag_name(self) -> &'static str {
        self.node_kind().tag_name()
    }
}

/// An owned, fully resolved cross-reference.
///
/// Exactly one of `refid` (same-document anchor) and `refuri` (cross-document
/// or external link) is normally set.  `title`, when present, replaces the
/// pending xref's inner content with a fresh inline node; otherwise the
/// resolver reuses the existing content node (upstream's `contnode`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedReference {
    /// Node class to materialize.
    pub node_kind: ReferenceNodeKind,
    /// Local anchor id, used when `docname == fromdocname`.
    pub refid: Option<String>,
    /// URI to the target, used for other documents and external resources.
    pub refuri: Option<String>,
    /// `reftitle` attribute (e.g. the numref format string).
    pub reftitle: Option<String>,
    /// Replacement display text; `None` reuses the pending xref's content.
    pub title: Option<String>,
    /// CSS classes for the replacement inner inline node.
    pub classes: Vec<String>,
    /// Target docname, retained for diagnostics and ordering.
    pub docname: Option<String>,
    /// When true, emit the original content node unchanged and suppress any
    /// missing-reference diagnostic (upstream returns `contnode` from
    /// `resolve_xref` for a disabled `:numref:`).
    pub passthrough: bool,
    /// Extra attributes copied onto the emitted reference node.
    pub extra_attrs: BTreeMap<String, String>,
}

impl ResolvedReference {
    /// Creates an empty descriptor emitting a plain `reference`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a reference to a local anchor.
    pub fn local(anchor: impl Into<String>, title: Option<String>) -> Self {
        Self {
            node_kind: ReferenceNodeKind::Reference,
            refid: Some(anchor.into()),
            title,
            ..Self::default()
        }
    }

    /// Creates a reference to a URI (another document or an external resource).
    pub fn uri(uri: impl Into<String>, title: Option<String>) -> Self {
        Self {
            node_kind: ReferenceNodeKind::Reference,
            refuri: Some(uri.into()),
            title,
            ..Self::default()
        }
    }

    /// Builder: changes the emitted node kind.
    pub fn with_node_kind(mut self, kind: ReferenceNodeKind) -> Self {
        self.node_kind = kind;
        self
    }

    /// Builder: attaches a `reftitle` attribute.
    pub fn with_reftitle(mut self, title: impl Into<String>) -> Self {
        self.reftitle = Some(title.into());
        self
    }

    /// Builder: attaches extra CSS classes.
    pub fn with_classes(mut self, classes: &[&str]) -> Self {
        self.classes.extend(classes.iter().map(|class| (*class).to_owned()));
        self
    }

    /// Builder: records the target docname.
    pub fn with_docname(mut self, docname: impl Into<String>) -> Self {
        self.docname = Some(docname.into());
        self
    }

    /// Builder: emits the original content node unchanged without a warning.
    pub fn passthrough(mut self) -> Self {
        self.passthrough = true;
        self
    }

    /// Builder: inserts an extra string attribute.
    pub fn with_extra(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_attrs.insert(key.into(), value.into());
        self
    }

    /// True when the descriptor has neither a local anchor nor a URI.
    pub fn is_empty(&self) -> bool {
        self.refid.is_none() && self.refuri.is_none()
    }
}

impl Default for ResolvedReference {
    fn default() -> Self {
        Self {
            node_kind: ReferenceNodeKind::Reference,
            refid: None,
            refuri: None,
            reftitle: None,
            title: None,
            classes: Vec::new(),
            docname: None,
            passthrough: false,
            extra_attrs: BTreeMap::new(),
        }
    }
}

/// Serialized form of a domain's mutable inventory.
///
/// Stored in the additive `BuildEnvironment::domain_data` side table.  The
/// `version` field lets a later phase invalidate stale inventories.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainData {
    /// Domain data format version.
    pub version: u32,
    /// Opaque, domain-specific payload (JSON value).
    pub payload: serde_json::Value,
}

impl DomainData {
    /// Wraps a serializable payload.
    pub fn from_payload<T: Serialize>(version: u32, payload: &T) -> DomainResult<Self> {
        let value = serde_json::to_value(payload).map_err(|error| {
            crate::error::DomainError::InvalidData {
                domain: String::new(),
                detail: error.to_string(),
            }
        })?;
        Ok(Self { version, payload: value })
    }

    /// Decodes the payload into `T`.
    pub fn decode<T: for<'de> Deserialize<'de>>(&self) -> DomainResult<T> {
        serde_json::from_value(self.payload.clone()).map_err(|error| {
            crate::error::DomainError::InvalidData {
                domain: String::new(),
                detail: error.to_string(),
            }
        })
    }
}

use crate::error::DomainResult;

/// A program-context marker emitted by `.. program:: name`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramContext {
    /// Program name, or `None` for `.. program:: None`.
    pub program: Option<String>,
    /// Document order index (used to apply contexts deterministically).
    pub order: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_reference_builders() {
        let local = ResolvedReference::local("label", Some("Title".into()));
        assert_eq!(local.refid.as_deref(), Some("label"));
        assert_eq!(local.title.as_deref(), Some("Title"));
        let external = ResolvedReference::uri("other.html", None)
            .with_node_kind(ReferenceNodeKind::DownloadReference)
            .with_reftitle("Download")
            .with_classes(&["xref", "std-download"]);
        assert_eq!(external.node_kind.tag_name(), "download_reference");
        assert_eq!(external.classes, ["xref", "std-download"]);
    }

    #[test]
    fn domain_data_roundtrip() {
        let map = BTreeMap::from([("a".to_owned(), (1u32, "x".to_owned()))]);
        let data = DomainData::from_payload(2, &map).expect("serialize");
        assert_eq!(data.version, 2);
        let decoded: BTreeMap<String, (u32, String)> = data.decode().expect("decode");
        assert_eq!(decoded, map);
    }
}
