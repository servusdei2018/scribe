//! The frozen `Domain` contract (Phase 11).
//!
//! This module is the single source of truth Phase 12–14 domains and the
//! Phase 17+ builders code against.  See `ARCHITECTURE.md` §15 for the full
//! rationale, ordering guarantees and extension recipe.

use scribe_ast::{Document, Node, NodeId, Span};
use scribe_config::{Config, ConfigValue};

use crate::diagnostics::{DiagnosticSink, DomainDiagnostic};
use crate::error::DomainResult;
use crate::types::{DomainData, ObjectEntry, ResolvedReference};

/// Read-only view of the frozen build environment needed by domain resolution.
///
/// Implemented by `scribe_core::BuildEnvironment`; tests can provide a tiny
/// in-memory adapter.  Keeping this as a trait avoids a dependency cycle
/// (`scribe-core` depends on `scribe-domains` so the Phase 10 transform can
/// dispatch to the registry).
pub trait DomainEnv: Send + Sync {
    /// Sorted docnames of all known documents (`env.all_docs`).
    fn docnames(&self) -> Vec<String> {
        Vec::new()
    }

    /// True when `docname` is a known source document.
    fn has_doc(&self, _docname: &str) -> bool {
        false
    }

    /// Human-readable title of `docname` (`env.titles`).
    fn title(&self, _docname: &str) -> Option<String> {
        None
    }

    /// Fully parsed doctree snapshot of `docname` (`env.get_doctree`).
    fn doctree(&self, _docname: &str) -> Option<&Document> {
        None
    }

    /// Serialized domain inventory persisted in the additive side table.
    fn domain_data(&self, _domain: &str) -> Option<&serde_json::Value> {
        None
    }
}

/// Everything a domain needs while registering objects or resolving a single
/// cross-reference.
///
/// `DomainContext` is cheap to clone: it only holds borrows, plus the current
/// `std:program` context which a caller can override for one xref via
/// [`with_program`](Self::with_program).
#[derive(Clone, Copy)]
pub struct DomainContext<'a> {
    /// Environment adapter (document names, titles, doctrees, side table).
    pub env: Option<&'a dyn DomainEnv>,
    /// Resolved Sphinx configuration; `None` means upstream defaults.
    pub config: Option<&'a Config>,
    /// Builder name (`html`, `pseudoxml`, ...).
    pub builder_name: &'a str,
    /// Builder format (`html`, `text`, ...).
    pub builder_format: &'a str,
    /// Document the current xref/directive occurs in.
    pub docname: &'a str,
    /// Active `std:program` name, captured from `.. program::`.
    pub program: Option<&'a str>,
    diagnostics: &'a DiagnosticSink,
}

impl<'a> DomainContext<'a> {
    /// Creates a context.
    pub fn new(
        env: Option<&'a dyn DomainEnv>,
        config: Option<&'a Config>,
        docname: &'a str,
        builder_name: &'a str,
        builder_format: &'a str,
        diagnostics: &'a DiagnosticSink,
    ) -> Self {
        Self {
            env,
            config,
            builder_name,
            builder_format,
            docname,
            program: None,
            diagnostics,
        }
    }

    /// Returns a copy with a different `std:program` context.
    pub fn with_program(mut self, program: Option<&'a str>) -> Self {
        self.program = program;
        self
    }

    /// Returns a copy with a different current document.
    pub fn with_docname(mut self, docname: &'a str) -> Self {
        self.docname = docname;
        self
    }

    /// Shared diagnostic sink.
    pub fn diagnostics(&self) -> &'a DiagnosticSink {
        self.diagnostics
    }

    /// Pushes a diagnostic into the shared sink.
    pub fn push_diagnostic(&self, diagnostic: DomainDiagnostic) {
        self.diagnostics.push(diagnostic);
    }

    /// Emits a warning diagnostic (code `scribe::domains::malformed_object`).
    pub fn warn(&self, message: impl Into<String>, target: Option<&str>, span: Option<Span>) {
        self.push_diagnostic(DomainDiagnostic::malformed(
            message,
            None,
            target.map(str::to_owned),
            Some(self.docname.to_owned()),
            span,
        ));
    }

    /// Config string lookup (upstream defaults when `config` is absent).
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.config.and_then(|config| config.get_str(key))
    }

    /// Config bool lookup.
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.config.and_then(|config| config.get_bool(key))
    }

    /// Config string-list lookup.
    pub fn get_strings(&self, key: &str) -> Vec<String> {
        self.config
            .and_then(|config| config.get_strings(key))
            .unwrap_or_default()
    }

    /// String-list lookup that also accepts Python tuples.
    ///
    /// `nitpick_ignore = [('py:class', 'Foo')]` is a list of *lists* in the
    /// config model, so [`Config::get_strings`] alone would drop it. Tuples of
    /// strings are flattened to `"type:target"`; [`parse_ignore_entry`](crate::registry::parse_ignore_entry)
    /// splits them at the last colon.
    pub fn get_strings_flexible(&self, key: &str) -> Vec<String> {
        let Some(config) = self.config else {
            return Vec::new();
        };
        let Some(values) = config.get_list(key) else {
            return self.get_strings(key);
        };
        values
            .iter()
            .map(|value| match value {
                ConfigValue::Str(text) => text.clone(),
                ConfigValue::List(items) => {
                    // Preserve the two-element tuple shape so
                    // `parse_ignore_entry` can split type from target even
                    // when both contain ':' (e.g. `('rst:role', 'c:any')`).
                    let values = items
                        .iter()
                        .map(|item| match item {
                            ConfigValue::Str(text) => text.clone(),
                            other => other.to_string(),
                        })
                        .collect::<Vec<_>>();
                    if values.len() == 2 {
                        format!("[\"{}\", \"{}\"]", values[0], values[1])
                    } else {
                        values.join(":")
                    }
                }
                other => other.to_string(),
            })
            .collect()
    }

    /// True when `nitpicky = true` is configured.
    pub fn nitpicky(&self) -> bool {
        self.get_bool("nitpicky").unwrap_or(false)
    }

    /// Output suffix used by `get_relative_uri` for this builder.
    pub fn uri_suffix(&self) -> String {
        match self.builder_format {
            "html" => self.get_str("html_file_suffix").unwrap_or(".html").to_owned(),
            "latex" => ".tex".to_owned(),
            "text" | "man" => ".txt".to_owned(),
            "" => ".html".to_owned(),
            other => format!(".{other}"),
        }
    }

    /// Relative URI from `from` to `to`, mirroring
    /// `Builder.get_target_uri` + `sphinx.util.relative_uri`.
    pub fn relative_uri(&self, from: &str, to: &str) -> String {
        relative_uri(&format!("{from}{}", self.uri_suffix()), &format!("{to}{}", self.uri_suffix()))
    }

    /// Byte span of a node, when it is live and carries one.
    pub fn span_of(&self, document: &Document, id: NodeId) -> Option<Span> {
        document.get(id).ok().and_then(|node| node.span)
    }
}

/// Sphinx `relative_uri(base, to)` adapted to docname-based targets.
///
/// Nested docnames are handled with POSIX path semantics; a target that is
/// already absolute or prefixed with `./` / `../` is returned unchanged.
pub fn relative_uri(base: &str, to: &str) -> String {
    if to.starts_with('/') {
        return to.to_owned();
    }
    // Mirrors `sphinx.util.osutil.relative_uri`, including its special cases
    // for a self-link (`''`) and a directory target (`'./'`).
    let base_without_fragment = base.split('#').next().unwrap_or(base);
    let to_without_fragment = to.split('#').next().unwrap_or(to);
    let mut base_parts: Vec<&str> = base_without_fragment.split('/').collect();
    let mut target_parts: Vec<&str> = to_without_fragment.split('/').collect();
    let common = base_parts
        .len()
        .saturating_sub(1)
        .min(target_parts.len().saturating_sub(1));
    let mut shared = 0;
    while shared < common && base_parts[shared] == target_parts[shared] {
        shared += 1;
    }
    base_parts.drain(0..shared);
    target_parts.drain(0..shared);
    if base_parts == target_parts {
        return String::new();
    }
    if base_parts.len() == 1 && target_parts == [""] {
        return "./".to_owned();
    }
    format!("{}{}", "../".repeat(base_parts.len().saturating_sub(1)), target_parts.join("/"))
}

/// Sphinx `docname_join(basedocname, docname)` (POSIX normalization).
pub fn docname_join(base: &str, target: &str) -> String {
    let mut parts: Vec<&str> = base.split('/').filter(|part| !part.is_empty()).collect();
    parts.pop();
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// Frozen domain interface.
///
/// Mutating methods take `&mut self`; [`DomainRegistry`](crate::registry::DomainRegistry)
/// serializes access with a `parking_lot::Mutex`.  Resolution methods take
/// `&self` and return owned [`ResolvedReference`] values so the pipeline can
/// apply arena mutations after all borrows end.
pub trait Domain: Send + Sync {
    /// Short unique name (`std`, `py`, ...).
    fn name(&self) -> &str;

    /// Descriptive label used in messages (`Default` for `std`).
    fn label(&self) -> &str;

    /// Object types declared by this domain, in upstream order.
    fn object_types(&self) -> &[ObjectTypeAlias];

    /// Directive names owned by this domain.
    fn directive_names(&self) -> &[&'static str];

    /// Role names owned by this domain.
    fn role_names(&self) -> &[&'static str];

    /// Fresh inventory for a new environment.
    fn initial_data(&self) -> DomainData;

    /// Inventory format version (bumped when the payload shape changes).
    fn data_version(&self) -> u32 {
        1
    }

    /// Removes all traces of `docname` from the inventory.
    fn clear_doc(&mut self, docname: &str);

    /// Merges data for `docnames` from another (parallel-build) inventory.
    fn merge_domaindata(&mut self, docnames: &[String], other: &DomainData);

    /// Registers objects found in a freshly read document.
    fn process_doc(&mut self, ctx: &DomainContext<'_>, docname: &str, document: &Document) -> DomainResult<()>;

    /// Experimental consistency checks; diagnostics are non-fatal.
    fn check_consistency(&self, ctx: &DomainContext<'_>) -> Vec<DomainDiagnostic> {
        let _ = ctx;
        Vec::new()
    }

    /// Resolves a typed cross-reference.
    ///
    /// Returns `Ok(None)` when no object matches; the registry then runs the
    /// `missing-reference` fallback (nitpicky warning / content node).
    fn resolve_xref(
        &self,
        ctx: &DomainContext<'_>,
        typ: &str,
        target: &str,
        node_id: NodeId,
        contnode_id: NodeId,
        document: &Document,
    ) -> DomainResult<Option<ResolvedReference>>;

    /// Resolves an `:any:` cross-reference across this domain's roles.
    ///
    /// Each item is the role that would have created the reference (e.g.
    /// `std:ref`) plus the resolved value.
    fn resolve_any_xref(
        &self,
        ctx: &DomainContext<'_>,
        target: &str,
        node_id: NodeId,
        contnode_id: NodeId,
        document: &Document,
    ) -> DomainResult<Vec<(String, ResolvedReference)>>;

    /// Deterministic inventory listing (see `ObjectEntry`).
    fn get_objects(&self) -> Vec<ObjectEntry>;

    /// Human-readable name for an object type.
    fn get_type_name(&self, objtype: &str, primary: bool) -> String {
        let _ = primary;
        self.object_types()
            .iter()
            .find(|object| object.name == objtype)
            .map_or_else(|| objtype.to_owned(), |object| object.lname.clone())
    }

    /// Fully qualified name for an object description node (upstream
    /// `get_full_qualified_name`), used by `:option:` subcommand folding.
    fn get_full_qualified_name(&self, _node: &Node) -> Option<String> {
        None
    }

    /// Serializes this domain's inventory for the additive env side table.
    fn export_data(&self) -> DomainResult<DomainData>;

    /// Replaces this domain's inventory with a persisted snapshot.
    fn replace_data(&mut self, data: &DomainData) -> DomainResult<()>;
}

/// Alias kept for readability at call sites while `ObjectType` remains the
/// canonical name in [`types`](crate::types).
pub type ObjectTypeAlias = crate::types::ObjectType;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_helpers_match_upstream_examples() {
        assert_eq!(docname_join("index", "other"), "other");
        assert_eq!(docname_join("dir/index", "other"), "dir/other");
        assert_eq!(docname_join("dir/index", "../other"), "other");
        assert_eq!(relative_uri("index.html", "other.html"), "other.html");
        assert_eq!(relative_uri("dir/index.html", "dir/other.html"), "other.html");
        assert_eq!(relative_uri("dir/index.html", "other.html"), "../other.html");
    }
}
