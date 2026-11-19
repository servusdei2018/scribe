//! Native implementations of Sphinx's built-in extensions (Phase 20).
//!
//! Upstream wires extensions through the `sphinx.application` event bus. That
//! bus arrives in Phase 27; this module deliberately exposes the *same* set of
//! build-phase hooks as plain functions and data holders so the builder can
//! call them today and a future event emitter can subscribe them without
//! changing any of the frozen contracts (§7 AST, §12 `BuildEnvironment`, §15
//! `Domain`). See `ARCHITECTURE.md` §26 for the full seam description.
//!
//! The extension nodes that upstream adds to Docutils (`todo_node`,
//! `todolist`, `ifconfig`, `graphviz`, `viewcode_anchor`) cannot be new
//! [`NodeKind`](scribe_ast::NodeKind) variants without breaking the frozen AST.
//! They are therefore represented by a [`NodeKind::Generated`] node carrying
//! the [`EXT_MARKER`] attribute with a stable string value. Extension passes
//! lower them to ordinary Docutils nodes before rendering whenever possible
//! (todos → admonitions, viewcode → references, ifconfig → its children), so
//! the HTML writer only needs one special case (graphviz).

use std::collections::{BTreeMap, BTreeSet};

use scribe_config::{Config, ConfigError, ConfigRegistry};
use scribe_rst::DirectiveRegistry;
pub use scribe_rst::ExtLinkRoles;
use std::sync::Arc;

pub mod duration;
pub mod extlinks;
pub mod githubpages;
pub mod graphviz;
pub mod ifconfig;
pub mod todo;
pub mod viewcode;

pub use duration::DurationReport;
pub use extlinks::{hardcoded_link_warnings, parse_extlinks};
pub use githubpages::GithubPagesFiles;
pub use graphviz::{GraphvizHtmlInfo, GraphvizRender};
pub use ifconfig::{EvalError, IfConfigValue};
pub use todo::{TodoCollector, TodoEntry};
pub use viewcode::{ViewcodeCollector, ViewcodeModule};

/// Marker attribute placed on `NodeKind::Generated` extension nodes.
pub const EXT_MARKER: &str = "scribe_ext_node";
/// Marker value for `.. todo::` (todos are lowered to admonitions, but the
/// marker is kept so the collector can find them).
pub const EXT_TODO: &str = "todo";
/// Marker value for `.. todolist::`.
pub const EXT_TODOLIST: &str = "todolist";
/// Marker value for `.. ifconfig::`.
pub const EXT_IFCONFIG: &str = "ifconfig";
/// Marker value for `.. graphviz::`, `.. graph::` and `.. digraph::`.
pub const EXT_GRAPHVIZ: &str = "graphviz";

/// Fully-qualified names of the extensions implemented natively in Phase 20.
pub const BUILTIN_EXTENSIONS: &[&str] = &[
    "sphinx.ext.todo",
    "sphinx.ext.ifconfig",
    "sphinx.ext.extlinks",
    "sphinx.ext.viewcode",
    "sphinx.ext.graphviz",
    "sphinx.ext.githubpages",
    "sphinx.ext.duration",
    "sphinx.ext.autodoc",
    "sphinx.ext.napoleon",
    "sphinx.ext.autosummary",
    "sphinx.ext.doctest",
];

/// The extensions activated for one build, plus activation diagnostics.
#[derive(Clone, Debug, Default)]
pub struct ActivatedExtensions {
    active: BTreeSet<String>,
    /// Non-fatal warnings (unknown extension names, malformed config).
    pub warnings: Vec<String>,
}

impl ActivatedExtensions {
    /// Read the `extensions` config value and activate the matching native
    /// implementations. Unknown names produce an upstream-style warning
    /// ("extension ... not found") instead of a panic.
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        let mut active = BTreeSet::new();
        let mut warnings = Vec::new();
        let configured = config.get_strings("extensions").unwrap_or_default();
        for name in configured {
            let trimmed = name.trim();
            if trimmed.is_empty() {
                continue;
            }
            if BUILTIN_EXTENSIONS.contains(&trimmed) {
                // Normalise `sphinx.ext.foo` to the short `foo` key.
                let short = trimmed.rsplit('.').next().unwrap_or(trimmed);
                active.insert(short.to_owned());
            } else {
                warnings.push(format!("extension {trimmed} not found"));
            }
        }
        Self { active, warnings }
    }

    /// Whether a short extension name is active.
    #[must_use]
    pub fn contains(&self, short_name: &str) -> bool {
        self.active.contains(short_name)
    }

    /// Whether a fully-qualified name is active.
    #[must_use]
    pub fn contains_qualified(&self, qualified: &str) -> bool {
        let short = qualified.rsplit('.').next().unwrap_or(qualified);
        self.active.contains(short)
    }

    /// Sorted list of active short names.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.active.iter().map(String::as_str).collect()
    }

    /// Whether no extension is active.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.active.is_empty()
    }
}

/// Register every config key owned by a Phase 20 extension.
///
/// Existing keys are left untouched, so this is idempotent and can be called
/// after [`ConfigRegistry::builtin`] in both the CLI and tests.
pub fn register_config_keys(registry: &mut ConfigRegistry) -> Result<(), ConfigError> {
    // All keys are already declared in the built-in registry (Phase 02 owns
    // the table); this function is the documented extension entry point and
    // verifies they are present, registering any that a custom registry lacks.
    use scribe_config::{ConfigDefault, ConfigType, ConfigValue};
    let entries: &[(&str, ConfigValue, ConfigType, &str)] = &[
        ("todo_include_todos", ConfigValue::Bool(false), ConfigType::Bool, "html"),
        ("todo_link_only", ConfigValue::Bool(false), ConfigType::Bool, "html"),
        ("todo_emit_warnings", ConfigValue::Bool(false), ConfigType::Bool, "env"),
        ("extlinks_detect_hardcoded_links", ConfigValue::Bool(false), ConfigType::Bool, "env"),
        ("viewcode_import", ConfigValue::None, ConfigType::Any, ""),
        ("viewcode_follow_imported_members", ConfigValue::Bool(true), ConfigType::Bool, ""),
        ("viewcode_line_numbers", ConfigValue::Bool(false), ConfigType::Bool, "env"),
        ("graphviz_dot", ConfigValue::from("dot"), ConfigType::Str, "html"),
        ("graphviz_output_format", ConfigValue::from("png"), ConfigType::Str, "html"),
        ("duration_print_total", ConfigValue::Bool(true), ConfigType::Bool, ""),
        ("duration_print_slowest", ConfigValue::Bool(true), ConfigType::Bool, ""),
        ("duration_n_slowest", ConfigValue::Int(5), ConfigType::Int, ""),
    ];
    // Phase 21: autodoc/napoleon-owned keys (napoleon_* and autodoc_path).
    scribe_autodoc::register_config_keys(registry)?;
    for (name, default, value_type, rebuild) in entries {
        if registry.contains(name) {
            continue;
        }
        registry.register(scribe_config::ConfigEntry {
            name,
            default: ConfigDefault::Value(default.clone()),
            value_type: value_type.clone(),
            rebuild,
            description: "Phase 20 built-in extension setting.",
        })?;
    }
    Ok(())
}

/// Register the directives contributed by the active extensions.
pub fn register_directives(registry: &mut DirectiveRegistry, active: &ActivatedExtensions) {
    if active.contains("todo") {
        registry.register(todo::TodoDirective);
        registry.register(todo::TodoListDirective);
    }
    if active.contains("ifconfig") {
        registry.register(ifconfig::IfConfigDirective);
    }
    if active.contains("graphviz") {
        registry.register(graphviz::GraphvizDirective);
        registry.register(graphviz::GraphDirective);
        registry.register(graphviz::DigraphDirective);
    }
    if active.contains("autodoc") || active.contains("napoleon") {
        scribe_autodoc::register_directives(registry);
    }
    if active.contains("autosummary") {
        scribe_autodoc::autosummary::register_directives(registry);
    }
}

/// Parse the `extlinks` config dict into dynamic role specifications.
///
/// Returns the role map and any diagnostics for malformed entries. Invalid
/// entries are skipped rather than aborting the build.
#[must_use]
pub fn extlink_roles(config: &Config) -> (ExtLinkRoles, Vec<String>) {
    let (specs, warnings) = extlinks::parse_extlinks(config);
    (Arc::new(specs), warnings)
}

/// Sphinx-style relative URI from one document page to another.
///
/// `target_uris` maps docnames to their output URI (`index.html`, `foo/index.html`, ...).
/// Returns `#` for a self-link, mirroring `Builder::get_relative_uri`.
#[must_use]
pub fn relative_doc_uri(target_uris: &BTreeMap<String, String>, from: &str, to: &str) -> String {
    if from == to {
        return "#".to_owned();
    }
    let to_uri = target_uris
        .get(to)
        .cloned()
        .unwrap_or_else(|| format!("{to}.html"));
    if to_uri.starts_with('#') || to_uri.contains("://") {
        return to_uri;
    }
    let from_uri = target_uris
        .get(from)
        .cloned()
        .unwrap_or_else(|| format!("{from}.html"));
    let depth = from_uri
        .rsplit_once('/')
        .map_or(0, |(dir, _)| dir.split('/').filter(|part| !part.is_empty()).count());
    let mut out = String::with_capacity(to_uri.len() + depth * 3);
    for _ in 0..depth {
        out.push_str("../");
    }
    out.push_str(&to_uri);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use scribe_config::ConfigValue;

    #[test]
    fn activation_is_config_driven_and_never_panics() {
        let mut config = Config::new();
        config.set("extensions", ConfigValue::List(vec![
            ConfigValue::from("sphinx.ext.todo"),
            ConfigValue::from("sphinx.ext.unknown"),
            ConfigValue::from("sphinx.ext.ifconfig"),
        ])).unwrap();
        let active = ActivatedExtensions::from_config(&config);
        assert!(active.contains("todo"));
        assert!(active.contains("ifconfig"));
        assert!(!active.contains_qualified("sphinx.ext.unknown"));
        assert_eq!(active.warnings, vec!["extension sphinx.ext.unknown not found".to_owned()]);
    }

    #[test]
    fn relative_doc_uri_handles_nested_pages_and_self_links() {
        let mut uris = BTreeMap::new();
        uris.insert("index".to_owned(), "index.html".to_owned());
        uris.insert("foo".to_owned(), "foo/index.html".to_owned());
        uris.insert("bar".to_owned(), "bar/index.html".to_owned());
        assert_eq!(relative_doc_uri(&uris, "index", "foo"), "foo/index.html");
        assert_eq!(relative_doc_uri(&uris, "foo", "bar"), "../bar/index.html");
        assert_eq!(relative_doc_uri(&uris, "foo", "foo"), "#");
    }

    #[test]
    fn config_key_registration_is_idempotent() {
        let mut registry = ConfigRegistry::builtin();
        register_config_keys(&mut registry).unwrap();
        let len = registry.len();
        register_config_keys(&mut registry).unwrap();
        assert_eq!(registry.len(), len);
    }
}
