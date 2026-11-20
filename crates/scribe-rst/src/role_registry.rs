//! Extensible role registry and the Phase 06 generic-role classification.
//!
//! Phase 05 already recognises roles and emits `pending_xref` / concrete
//! inline nodes through the frozen [`XrefMeta`](crate::inline::XrefMeta)
//! contract. This module makes that classification data *registered* and
//! extensible: [`RoleRegistry`] holds [`Role`] handlers, and the pure inline
//! recogniser asks [`classify_builtin`](crate::role_registry::classify_builtin)
//! for the kind of node to emit, so there is a single source of truth for
//! `:abbr:`, `:ref:`, `:pep:`, ... across Phases 05 and 06.
//!
//! Domain-qualified roles (`:py:func:` and friends) are registered as generic
//! xref emitters; Phase 11-14 will replace them with domain-aware handlers.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use once_cell::sync::Lazy;
use scribe_ast::{AstError, Document, Node, NodeId, NodeKind};
use thiserror::Error;

/// Metadata captured by a `.. role:: name(base)` directive.
///
/// Docutils' `CustomRole` delegates node construction to the base role but
/// forwards `:language:`/`:class:` options onto the emitted element. Sphinx's
/// `sphinx.roles.CodeRole` uses `:language:` to attach a `language` attribute,
/// which the HTML writer turns into a highlighted inline `<code>` element.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CustomRoleInfo {
    /// Name of the base role (`code`, `literal`, `generic`, …).
    pub base: String,
    /// `:language:` option, forwarded onto `code` roles as `language`.
    pub language: Option<String>,
    /// `:class:` option values, merged into the emitted node's classes.
    pub classes: Vec<String>,
}

impl CustomRoleInfo {
    /// Creates the plain `base` form (no forwarded options).
    pub fn new(base: impl Into<String>) -> Self {
        Self { base: base.into(), language: None, classes: Vec::new() }
    }
}

/// The inner-node class used by Sphinx's `XRefRole` and Docutils generic roles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InnerKind {
    /// ``nodes.literal``.
    Literal,
    /// ``nodes.inline``.
    Inline,
    /// ``nodes.emphasis``.
    Emphasis,
    /// ``nodes.strong``.
    Strong,
    /// ``nodes.math``.
    Math,
}

impl InnerKind {
    /// Maps to the frozen AST node kind.
    pub fn node_kind(self) -> NodeKind {
        match self {
            Self::Literal => NodeKind::Literal,
            Self::Inline => NodeKind::Inline,
            Self::Emphasis => NodeKind::Emphasis,
            Self::Strong => NodeKind::Strong,
            Self::Math => NodeKind::Math,
        }
    }
}

/// Special payload handling required by a few Sphinx roles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectBehavior {
    /// Plain text payload.
    Plain,
    /// `abbr`: split a trailing `(explanation)` into an attribute.
    Abbreviation,
    /// `kbd`: split on `-`, `+`, `^` and whitespace into several literals.
    Keyboard,
    /// `menuselection`: replace `-->` with the triangular bullet character.
    MenuSelection,
    /// `manpage`: parse `name(section)` into attributes.
    Manpage,
}

/// The complete node-output classification of a role.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoleClass {
    /// A concrete inline node (Docutils generic role or Sphinx semantic role).
    Direct {
        /// Frozen AST node kind to emit.
        kind: NodeKind,
        /// Inner element kind used by the inline recogniser.
        inner: InnerKind,
        /// CSS classes attached to the emitted element.
        classes: Vec<String>,
        /// Special payload behaviour.
        behavior: DirectBehavior,
    },
    /// A cross-reference, emitted as a frozen `pending_xref`.
    Xref {
        /// Owning domain (`std`, `py`, `c`, ...).
        domain: String,
        /// Role name inside the domain (`ref`, `func`, ...).
        reftype: String,
        /// Inner element kind.
        inner: InnerKind,
        /// Upstream `warn_dangling` flag.
        warn: bool,
    },
    /// An external reference (`pep`, `rfc`) with a generated URI.
    External {
        /// CSS classes on the reference.
        classes: Vec<String>,
        /// Base URL prefix.
        prefix: String,
        /// Text prefix used for implicit titles.
        title_prefix: String,
    },
}

/// A parsed role invocation, mirroring the side-table contract for inline
/// roles and used by [`Role::process`] for stateful/direct use.
#[derive(Clone, Debug)]
pub struct RoleInvocation {
    /// Role name as written (without backticks).
    pub name: String,
    /// Text between the backticks.
    pub payload: String,
    /// Explicit title from `Title <target>`, if present.
    pub explicit_title: Option<String>,
    /// 1-based source line.
    pub line: u32,
}

impl RoleInvocation {
    /// Creates an invocation with no explicit title.
    pub fn new(name: impl Into<String>, payload: impl Into<String>, line: u32) -> Self {
        Self {
            name: name.into(),
            payload: payload.into(),
            explicit_title: None,
            line,
        }
    }

    /// Target portion of the payload.
    pub fn target(&self) -> &str {
        self.explicit_title.as_ref().map_or(&self.payload, |_| {
            self.payload.rsplit_once('<').map_or(self.payload.as_str(), |(_, rest)| rest.trim_end_matches('>'))
        })
    }
}

/// Errors from a role handler.
#[derive(Clone, Debug, Error)]
pub enum RoleError {
    /// The role name is not registered.
    #[error("unknown role '{0}'")]
    UnknownRole(String),
    /// A handler rejected its payload.
    #[error("role '{name}' failed: {detail}")]
    Failed {
        /// Role name.
        name: String,
        /// Failure detail.
        detail: String,
    },
    /// An arena operation failed.
    #[error(transparent)]
    Ast(#[from] AstError),
}

/// Extensible role handler.
pub trait Role: Send + Sync {
    /// Canonical role name.
    fn name(&self) -> &'static str;

    /// Node-output classification consumed by the inline recogniser.
    fn class(&self) -> RoleClass;

    /// Produces nodes under `parent` for a direct call (future stateful passes
    /// and extension tests). The default implementation follows `class()`.
    fn process(
        &self,
        invocation: &RoleInvocation,
        doc: &mut Document,
        parent: NodeId,
    ) -> Result<Vec<NodeId>, RoleError> {
        let line = invocation.line;
        match self.class() {
            RoleClass::Direct { kind, classes, behavior, .. } => {
                let mut node = Node::new(kind);
                node.source = Some(scribe_ast::SourcePos::new(None, Some(line), None));
                if !classes.is_empty() {
                    node.attrs.set_list("classes", classes);
                }
                let text = transform_payload(&behavior, &invocation.payload, &mut node);
                let id = doc.add_node(node, Some(parent))?;
                let text_node = Node::text(text);
                doc.add_node(text_node, Some(id))?;
                Ok(vec![id])
            }
            RoleClass::Xref { domain, reftype, inner, warn } => {
                let mut node = Node::new(NodeKind::PendingXref);
                node.attrs.set_str("refdomain", domain);
                node.attrs.set_str("reftype", reftype);
                node.attrs.set_str("reftarget", crate::names::whitespace_normalize_name(invocation.target()));
                node.attrs.set_bool("refwarn", warn);
                let id = doc.add_node(node, Some(parent))?;
                let inner_id = doc.add_node(Node::new(inner.node_kind()), Some(id))?;
                doc.add_node(Node::text(invocation.payload.clone()), Some(inner_id))?;
                Ok(vec![id])
            }
            RoleClass::External { classes, prefix, title_prefix } => {
                let target = invocation.target();
                let mut node = Node::new(NodeKind::Reference);
                node.attrs.set_str("refuri", format!("{prefix}{target}"));
                node.attrs.set_list("classes", classes);
                let id = doc.add_node(node, Some(parent))?;
                let strong = doc.add_node(Node::new(NodeKind::Strong), Some(id))?;
                let title = invocation
                    .explicit_title
                    .clone()
                    .unwrap_or_else(|| format!("{title_prefix}{target}"));
                doc.add_node(Node::text(title), Some(strong))?;
                Ok(vec![id])
            }
        }
    }
}

fn transform_payload(behavior: &DirectBehavior, payload: &str, node: &mut Node) -> String {
    match behavior {
        DirectBehavior::Abbreviation => {
            if let Some(index) = payload.rfind(" (") {
                if payload.ends_with(')') {
                    node.attrs.set_str("explanation", payload[index + 2..payload.len() - 1].to_owned());
                    return payload[..index].to_owned();
                }
            }
            payload.to_owned()
        }
        DirectBehavior::MenuSelection => payload.replace("-->", "\u{25b8}"),
        DirectBehavior::Manpage => {
            let mut page = payload.to_owned();
            let mut section = String::new();
            if let Some(open) = payload.rfind(['(', '[']) {
                if payload.ends_with([')', ']']) {
                    page = payload[..open].trim_end_matches(['.', ' ']).to_owned();
                    section = payload[open + 1..payload.len() - 1].to_owned();
                }
            }
            node.attrs.set_str("path", payload);
            node.attrs.set_str("page", page);
            node.attrs.set_str("section", section);
            invocation_text(payload)
        }
        DirectBehavior::Keyboard => invocation_text(payload),
        DirectBehavior::Plain => invocation_text(payload),
    }
}

fn invocation_text(payload: &str) -> String {
    payload.to_owned()
}

/// A concrete built-in role.
#[derive(Clone, Debug)]
pub struct BuiltinRole {
    name: &'static str,
    class: RoleClass,
}

impl BuiltinRole {
    /// Creates a built-in role descriptor.
    pub const fn new(name: &'static str, class: RoleClass) -> Self {
        Self { name, class }
    }
}

impl Role for BuiltinRole {
    fn name(&self) -> &'static str {
        self.name
    }
    fn class(&self) -> RoleClass {
        self.class.clone()
    }
}

/// Thread-safe registry of [`Role`] handlers.
#[derive(Default)]
pub struct RoleRegistry {
    entries: RwLock<BTreeMap<String, Arc<dyn Role>>>,
}

impl std::fmt::Debug for RoleRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoleRegistry")
            .field("names", &self.names())
            .finish_non_exhaustive()
    }
}

impl RoleRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers or replaces a role.
    pub fn register<R: Role + 'static>(&self, role: R) {
        if let Ok(mut entries) = self.entries.write() {
            entries.insert(role.name().to_owned(), Arc::new(role));
        }
    }

    /// Looks up a role by name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Role>> {
        self.entries
            .read()
            .ok()
            .and_then(|entries| entries.get(name).cloned())
    }

    /// Classifies a registered role.
    pub fn classify(&self, name: &str) -> Option<RoleClass> {
        self.get(name).map(|role| role.class())
    }

    /// Sorted list of registered role names.
    pub fn names(&self) -> Vec<String> {
        self.entries
            .read()
            .map(|entries| entries.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Number of registered roles.
    pub fn len(&self) -> usize {
        self.entries.read().map(|entries| entries.len()).unwrap_or(0)
    }

    /// `true` when no roles are registered.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Registry containing all built-in generic roles.
    pub fn builtin() -> Self {
        let registry = Self::new();
        for role in builtin_roles() {
            registry.register(role);
        }
        registry
    }
}

static BUILTIN: Lazy<RoleRegistry> = Lazy::new(RoleRegistry::builtin);

/// Classifies a built-in role without constructing a registry. Consumed by the
/// Phase 05 pure inline recogniser so there is one source of truth.
pub fn classify_builtin(name: &str) -> Option<RoleClass> {
    BUILTIN.classify(name)
}

/// The full list of built-in role descriptors.
pub fn builtin_roles() -> Vec<BuiltinRole> {
    use DirectBehavior::*;
    use InnerKind::*;
    let mut roles = vec![
        // Docutils standard roles.
        BuiltinRole::new("math", RoleClass::Direct { kind: NodeKind::Math, inner: Math, classes: vec![], behavior: Plain }),
        BuiltinRole::new("literal", RoleClass::Direct { kind: NodeKind::Literal, inner: Literal, classes: vec![], behavior: Plain }),
        BuiltinRole::new("emphasis", RoleClass::Direct { kind: NodeKind::Emphasis, inner: Emphasis, classes: vec![], behavior: Plain }),
        BuiltinRole::new("strong", RoleClass::Direct { kind: NodeKind::Strong, inner: Strong, classes: vec![], behavior: Plain }),
        BuiltinRole::new("title-reference", RoleClass::Direct { kind: NodeKind::TitleReference, inner: Inline, classes: vec![], behavior: Plain }),
        BuiltinRole::new("code", RoleClass::Direct { kind: NodeKind::Literal, inner: Literal, classes: vec!["code".into()], behavior: Plain }),
        // Sphinx semantic roles (`sphinx/roles.py`).
        BuiltinRole::new("abbr", RoleClass::Direct { kind: NodeKind::Inline, inner: Inline, classes: vec!["abbreviation".into()], behavior: Abbreviation }),
        BuiltinRole::new("command", RoleClass::Direct { kind: NodeKind::Strong, inner: Strong, classes: vec!["command".into()], behavior: Plain }),
        BuiltinRole::new("dfn", RoleClass::Direct { kind: NodeKind::Emphasis, inner: Emphasis, classes: vec!["dfn".into()], behavior: Plain }),
        BuiltinRole::new("file", RoleClass::Direct { kind: NodeKind::Literal, inner: Literal, classes: vec!["file".into()], behavior: Plain }),
        BuiltinRole::new("guilabel", RoleClass::Direct { kind: NodeKind::Inline, inner: Inline, classes: vec!["guilabel".into()], behavior: Plain }),
        BuiltinRole::new("kbd", RoleClass::Direct { kind: NodeKind::Literal, inner: Literal, classes: vec!["kbd".into()], behavior: Keyboard }),
        BuiltinRole::new("mailheader", RoleClass::Direct { kind: NodeKind::Literal, inner: Literal, classes: vec!["mailheader".into()], behavior: Plain }),
        BuiltinRole::new("makevar", RoleClass::Direct { kind: NodeKind::Strong, inner: Strong, classes: vec!["makevar".into()], behavior: Plain }),
        BuiltinRole::new("manpage", RoleClass::Direct { kind: NodeKind::Manpage, inner: Literal, classes: vec!["manpage".into()], behavior: Manpage }),
        BuiltinRole::new("menuselection", RoleClass::Direct { kind: NodeKind::Inline, inner: Inline, classes: vec!["menuselection".into()], behavior: MenuSelection }),
        BuiltinRole::new("mimetype", RoleClass::Direct { kind: NodeKind::Literal, inner: Literal, classes: vec!["mimetype".into()], behavior: Plain }),
        BuiltinRole::new("newsgroup", RoleClass::Direct { kind: NodeKind::Literal, inner: Literal, classes: vec!["newsgroup".into()], behavior: Plain }),
        BuiltinRole::new("program", RoleClass::Direct { kind: NodeKind::Strong, inner: Strong, classes: vec!["program".into()], behavior: Plain }),
        BuiltinRole::new("regexp", RoleClass::Direct { kind: NodeKind::Literal, inner: Literal, classes: vec!["regexp".into()], behavior: Plain }),
        BuiltinRole::new("samp", RoleClass::Direct { kind: NodeKind::Literal, inner: Literal, classes: vec!["samp".into()], behavior: Plain }),
        BuiltinRole::new("pep", RoleClass::External { classes: vec!["pep".into()], prefix: "https://peps.python.org/pep-".into(), title_prefix: "PEP ".into() }),
        BuiltinRole::new("rfc", RoleClass::External { classes: vec!["rfc".into()], prefix: "https://www.rfc-editor.org/rfc/rfc".into(), title_prefix: "RFC ".into() }),
    ];
    // Standard-domain cross references.
    for (name, reftype, inner, warn) in [
        ("ref", "ref", Inline, true),
        ("doc", "doc", Inline, true),
        ("numref", "numref", Literal, true),
        ("envvar", "envvar", Literal, false),
        ("confval", "confval", Literal, true),
        ("event", "event", Literal, true),
        ("label", "label", Literal, false),
        ("option", "option", Literal, true),
        ("token", "token", Literal, false),
        ("term", "term", Inline, true),
        ("keyword", "keyword", Literal, true),
        ("download", "download", Literal, false),
        ("any", "any", Literal, true),
    ] {
        roles.push(BuiltinRole::new(name, RoleClass::Xref {
            domain: if name == "any" { String::new() } else { "std".to_owned() },
            reftype: reftype.to_owned(),
            inner,
            warn,
        }));
    }
    // Math domain.
    roles.push(BuiltinRole::new("eq", RoleClass::Xref {
        domain: "math".to_owned(),
        reftype: "eq".to_owned(),
        inner: Literal,
        warn: true,
    }));
    // Python and standard domain (short names; Phase 11 provides real
    // resolution). Fully-qualified `py:func` / `std:ref` aliases are registered
    // too, because Sphinx's role registry accepts both spellings.
    for name in ["mod", "func", "class", "meth", "attr", "exc", "obj", "data", "const", "deco", "type"] {
        roles.push(BuiltinRole::new(name, RoleClass::Xref {
            domain: "py".to_owned(),
            reftype: name.to_owned(),
            inner: Literal,
            warn: false,
        }));
    }
    for (name, reftype) in [
        ("py:mod", "mod"), ("py:func", "func"), ("py:class", "class"), ("py:meth", "meth"),
        ("py:attr", "attr"), ("py:exc", "exc"), ("py:obj", "obj"), ("py:data", "data"),
        ("py:type", "type"), ("py:const", "const"), ("py:deco", "deco"),
        ("std:ref", "ref"), ("std:doc", "doc"), ("std:option", "option"), ("std:term", "term"),
        ("std:numref", "numref"), ("std:envvar", "envvar"), ("std:token", "token"),
        ("std:confval", "confval"), ("std:label", "label"),
        ("std:event", "event"),
        ("std:keyword", "keyword"), ("std:download", "download"),
    ] {
        let inner = if matches!(reftype, "ref" | "doc" | "term") { Inline } else { Literal };
        roles.push(BuiltinRole::new(name, RoleClass::Xref {
            domain: name.split_once(':').map_or("std", |(domain, _)| domain).to_owned(),
            reftype: reftype.to_owned(),
            inner,
            warn: true,
        }));
    }
    // Phase 13 domains (fully qualified role names). Upstream registers these
    // as `XRefRole` subclasses with `warn_dangling = False`.
    let qualified: &[&str] = &[
        // C.
        "c:member", "c:data", "c:var", "c:func", "c:macro", "c:struct",
        "c:union", "c:enum", "c:enumerator", "c:type", "c:expr", "c:texpr",
        // C++.
        "cpp:any", "cpp:class", "cpp:struct", "cpp:union", "cpp:func",
        "cpp:member", "cpp:var", "cpp:type", "cpp:concept", "cpp:enum",
        "cpp:enumerator", "cpp:expr", "cpp:texpr",
        // JavaScript.
        "js:func", "js:meth", "js:class", "js:data", "js:attr", "js:mod",
        // reStructuredText.
        "rst:dir", "rst:role",
        // Math (`numref` is qualified; `eq` is also registered bare above).
        "math:numref", "math:eq",
    ];
    for name in qualified {
        let (domain, reftype) = name.split_once(':').unwrap_or((name, name));
        roles.push(BuiltinRole::new(name, RoleClass::Xref {
            domain: domain.to_owned(),
            reftype: reftype.to_owned(),
            inner: Literal,
            warn: false,
        }));
    }
    // Legacy Sphinx aliases for the reST-domain roles.
    for (name, reftype) in [("dudir", "dir"), ("durole", "role")] {
        roles.push(BuiltinRole::new(name, RoleClass::Xref {
            domain: "rst".to_owned(),
            reftype: reftype.to_owned(),
            inner: Literal,
            warn: false,
        }));
    }
    roles
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_registry_covers_required_roles() {
        let registry = RoleRegistry::builtin();
        for name in [
            "ref", "doc", "download", "numref", "envvar", "option", "token", "keyword", "term",
            "abbr", "command", "dfn", "file", "guilabel", "kbd", "mailheader", "makevar",
            "manpage", "menuselection", "mimetype", "newsgroup", "program", "regexp", "samp",
            "pep", "rfc", "math", "eq",
        ] {
            assert!(registry.classify(name).is_some(), "missing role {name}");
        }
    }

    #[test]
    fn classification_matches_upstream_node_kinds() {
        assert!(matches!(
            classify_builtin("abbr"),
            Some(RoleClass::Direct { kind: NodeKind::Inline, .. })
        ));
        assert!(matches!(
            classify_builtin("ref"),
            Some(RoleClass::Xref { ref domain, .. }) if domain == "std"
        ));
        assert!(matches!(
            classify_builtin("pep"),
            Some(RoleClass::External { .. })
        ));
    }

    #[test]
    fn process_builds_external_reference() {
        let role = BuiltinRole::new("pep", RoleClass::External {
            classes: vec!["pep".into()],
            prefix: "https://peps.python.org/pep-".into(),
            title_prefix: "PEP ".into(),
        });
        let mut doc = Document::new(None);
        let root = doc.root;
        let ids = role
            .process(&RoleInvocation::new("pep", "8", 1), &mut doc, root)
            .unwrap();
        let pseudo = doc.to_pseudoxml().unwrap();
        assert!(pseudo.contains("<reference classes=\"pep\" refuri=\"https://peps.python.org/pep-8\">"), "{pseudo}");
        assert_eq!(ids.len(), 1);
    }
}
