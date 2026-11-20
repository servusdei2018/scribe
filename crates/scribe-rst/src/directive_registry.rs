//! Extensible directive trait, context and registry (Phase 06).
//!
//! A [`Directive`] consumes the structured payload of a Phase 04
//! [`Pending`](crate::pending::PendingDirective) node and appends concrete AST
//! nodes to the document. [`DirectiveRegistry`] is a case-insensitive,
//! thread-safe registry; [`DirectiveContext`] exposes arguments, typed options,
//! dedented content, the build [`Context`] (config, tags, include resolver) and
//! callbacks that reuse the Phase 04 block parser and Phase 05 inline parser
//! for nested markup.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use miette::{Diagnostic, SourceSpan};
use scribe_ast::{AstError, AstResult, Document, Node, NodeId, NodeKind, SourcePos, Span};
use scribe_config::{Config, Tags};
use thiserror::Error;

use crate::inline::{parse_inline, InlineOptions, InlineSource, XrefMeta};
use crate::option_spec::{OptionError, OptionSpec, OptionValue};
use crate::{InlineContext, ParseOptions};

/// Shared build context handed to every directive and to the resolve pass.
///
/// It carries the Sphinx configuration (for `only`, `date`, image paths, ...),
/// the enabled [`Tags`], the source directory used to resolve `include` and
/// `raw.file`, and an injectable [`IncludeResolver`] so tests and future
/// builders can provide content without touching the file system.
#[derive(Clone)]
pub struct Context {
    /// Enabled Sphinx tags (`-t draft`).
    pub tags: Arc<Tags>,
    /// Resolved configuration; `None` means upstream defaults.
    pub config: Option<Arc<Config>>,
    /// Directory used to resolve relative include/raw/image paths.
    pub source_dir: Option<PathBuf>,
    /// Source name of the document being resolved (used for `refdoc`).
    pub source_name: Option<String>,
    /// Original source text, attached to diagnostics for line/column rendering.
    pub source_text: Option<Arc<String>>,
    /// Tab stop width forwarded to nested block parses.
    pub tab_width: usize,
    /// Strategy used to read included files.
    pub include_resolver: Arc<dyn IncludeResolver>,
    /// Replacement-directive name for substitution definitions, keyed by the
    /// `NodeId` of the `SubstitutionDefinition`. Populated by
    /// [`crate::resolve_output`](crate::resolve::resolve_output) from the
    /// additive Phase 04/05 side table.
    pub substitution_directives: BTreeMap<u32, crate::SubstitutionDirective>,
    /// Custom roles declared by `.. role::`, mapped to their base role and
    /// forwarded `:language:`/`:class:` options.
    pub custom_roles: Arc<RwLock<HashMap<String, crate::role_registry::CustomRoleInfo>>>,
    /// Role selected by `.. default-role::`.
    pub default_role: Arc<RwLock<Option<String>>>,
    /// Domain selected by `.. default-domain::`. Unprefixed directives and
    /// roles are resolved against this domain first (mirroring
    /// `SphinxDirective`'s `env.current_document.default_domain` lookup).
    pub default_domain: Arc<RwLock<Option<String>>>,
    /// Module selected by `.. currentmodule::` / `.. py:currentmodule::`.
    /// Autodoc directives qualify bare object names with it, mirroring
    /// Sphinx's `env.current_document.current_module`.
    pub current_module: Arc<RwLock<Option<String>>>,
    /// Dynamic external-link roles installed by `sphinx.ext.extlinks`.
    pub extlinks: crate::extlinks::ExtLinkRoles,
}

impl Default for Context {
    fn default() -> Self {
        Self {
            tags: Arc::new(Tags::new()),
            config: None,
            source_dir: None,
            source_name: None,
            source_text: None,
            tab_width: 8,
            include_resolver: Arc::new(FsIncludeResolver),
            substitution_directives: BTreeMap::new(),
            custom_roles: Arc::new(RwLock::new(HashMap::new())),
            default_role: Arc::new(RwLock::new(None)),
            default_domain: Arc::new(RwLock::new(None)),
            current_module: Arc::new(RwLock::new(None)),
            extlinks: crate::extlinks::empty_roles(),
        }
    }
}

impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context")
            .field("tags", &self.tags)
            .field("config", &self.config.is_some())
            .field("source_dir", &self.source_dir)
            .field("source_name", &self.source_name)
            .field("tab_width", &self.tab_width)
            .field("substitution_directives", &self.substitution_directives)
            .finish_non_exhaustive()
    }
}

impl Context {
    /// Creates a context with a source directory and tags.
    pub fn new(source_dir: impl Into<PathBuf>, tags: Tags) -> Self {
        Self {
            source_dir: Some(source_dir.into()),
            tags: Arc::new(tags),
            ..Self::default()
        }
    }

    /// Evaluates an `only` expression using Sphinx's tag grammar.
    pub fn eval_condition(&self, expression: &str) -> Result<bool, scribe_config::TagsError> {
        self.tags.eval_condition(expression)
    }

    /// Reads an included file relative to `source_dir`.
    pub fn read_include(&self, path: &str) -> Result<String, DirectiveError> {
        self.include_resolver
            .resolve(self.source_dir.as_deref(), self.source_name.as_deref(), path)
    }
}

/// Pluggable strategy for resolving `include` / `raw.file` paths.
pub trait IncludeResolver: Send + Sync + std::fmt::Debug {
    /// Returns the decoded text of `path`.
    ///
    /// `source_dir` is the project source directory and `source_name` the
    /// current document name; resolvers may use either to anchor relative
    /// paths.
    fn resolve(
        &self,
        source_dir: Option<&Path>,
        source_name: Option<&str>,
        path: &str,
    ) -> Result<String, DirectiveError>;
}

/// Default [`IncludeResolver`]: reads UTF-8 files relative to the *including
/// document* (Sphinx/docutils semantics), falling back to the source dir.
#[derive(Debug, Clone, Copy)]
pub struct FsIncludeResolver;

impl IncludeResolver for FsIncludeResolver {
    fn resolve(
        &self,
        source_dir: Option<&Path>,
        source_name: Option<&str>,
        path: &str,
    ) -> Result<String, DirectiveError> {
        let candidate = Path::new(path);
        let full = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            // Docutils anchors relative includes at the document containing
            // the directive, not the project root. When a docname is known,
            // try `<source_dir>/<doc-parent>/<path>` first, then fall back to
            // `<source_dir>/<path>` for callers that pass root-relative paths.
            let doc_relative = source_dir.zip(source_name).and_then(|(dir, name)| {
                Path::new(name)
                    .parent()
                    .map(|parent| dir.join(parent).join(path))
            });
            doc_relative
                .or_else(|| source_dir.map(|dir| dir.join(path)))
                .or_else(|| {
                    source_name
                        .and_then(|name| Path::new(name).parent())
                        .map(|parent| parent.join(path))
                })
                .unwrap_or_else(|| candidate.to_path_buf())
        };
        // Prefer the document-relative file when both candidates exist; the
        // docutils behaviour wins over the legacy source-dir fallback.
        let full = if full.is_file() {
            full
        } else if let Some(dir) = source_dir {
            let root_relative = dir.join(path);
            if root_relative.is_file() {
                root_relative
            } else {
                full
            }
        } else {
            full
        };
        std::fs::read_to_string(&full).map_err(|source| DirectiveError::IncludeIo {
            path: full.display().to_string(),
            detail: source.to_string(),
        })
    }
}

/// The handler-side view of a directive invocation.
///
/// Arguments are split according to the directive's
/// [`final_argument_whitespace`](Directive::final_argument_whitespace) flag;
/// options are already converted through the directive's
/// [`option_spec`](Directive::option_spec).
pub struct DirectiveContext<'a> {
    /// Arena being built.
    pub doc: &'a mut Document,
    /// Parent that receives the produced nodes.
    pub parent: NodeId,
    /// Raw argument string from the `Pending` node.
    pub raw_arguments: &'a str,
    /// Split arguments.
    pub arguments: Vec<String>,
    /// Raw string options (retained for `raw` style pass-through).
    pub raw_options: &'a BTreeMap<String, String>,
    /// Converted typed options.
    pub options: BTreeMap<String, OptionValue>,
    /// Dedented directive content.
    pub content: &'a str,
    /// 1-based source line of the directive marker.
    pub line: u32,
    /// Byte span of the whole directive.
    pub span: Span,
    /// For `only`, the number of ancestor sections its content is raised
    /// through (Sphinx `Only.run` section raising). Zero for other directives.
    pub only_raise: i64,
    /// Shared build context.
    pub global: &'a Context,
}

impl<'a> DirectiveContext<'a> {
    /// Returns argument `index`, if present.
    pub fn argument(&self, index: usize) -> Option<&str> {
        self.arguments.get(index).map(String::as_str)
    }

    /// Returns all arguments.
    pub fn arguments(&self) -> &[String] {
        &self.arguments
    }

    /// Returns a string option, if declared and present.
    pub fn option_str(&self, name: &str) -> Option<&str> {
        self.options.get(&crate::option_spec::normalize(name)).and_then(OptionValue::as_str)
    }

    /// Returns an integer option, if declared and present.
    pub fn option_int(&self, name: &str) -> Option<i64> {
        self.options.get(&crate::option_spec::normalize(name)).and_then(OptionValue::as_i64)
    }

    /// Returns a list option, if declared and present.
    pub fn option_list(&self, name: &str) -> Option<&[String]> {
        self.options.get(&crate::option_spec::normalize(name)).and_then(OptionValue::as_list)
    }

    /// `true` when a flag option is present.
    pub fn flag(&self, name: &str) -> bool {
        self.options
            .get(&crate::option_spec::normalize(name))
            .is_some_and(OptionValue::is_flag)
    }

    /// The directive's `classes` list, derived from `:class:`/`:classes:`.
    pub fn classes(&self) -> Vec<String> {
        let mut classes = Vec::new();
        if let Some(values) = self.option_list("class") {
            classes.extend(values.iter().cloned());
        }
        if let Some(values) = self.option_list("classes") {
            classes.extend(values.iter().cloned());
        }
        classes
    }

    /// The directive's `names` list, derived from `:name:`.
    pub fn names(&self) -> Vec<String> {
        self.option_str("name")
            .map(|name| vec![crate::names::fully_normalize_name(name)])
            .unwrap_or_default()
    }

    /// Adds an element under `parent` with source position and span.
    pub fn add(&mut self, kind: NodeKind, parent: NodeId) -> AstResult<NodeId> {
        self.add_detached(kind, parent, true)
    }

    /// Adds an element under `parent`, optionally carrying source metadata.
    /// Detached nodes are used when a node will be re-parented immediately.
    pub fn add_detached(
        &mut self,
        kind: NodeKind,
        parent: NodeId,
        with_source: bool,
    ) -> AstResult<NodeId> {
        let mut node = Node::new(kind);
        if with_source {
            node.source = Some(SourcePos::new(
                self.global.source_name.clone(),
                Some(self.line),
                Some(self.span.start),
            ));
            node.span = Some(self.span);
        }
        self.doc.add_node(node, Some(parent))
    }

    /// Adds a text leaf under `parent`.
    pub fn add_text(
        &mut self,
        text: impl Into<String>,
        parent: NodeId,
        line: u32,
    ) -> AstResult<NodeId> {
        let mut node = Node::text(text);
        node.source = Some(SourcePos::new(
            self.global.source_name.clone(),
            Some(line),
            Some(self.span.start),
        ));
        node.span = Some(self.span);
        self.doc.add_node(node, Some(parent))
    }

    /// Applies the conventional `classes`/`names`/`ids` attributes to `id`.
    pub fn set_common(&mut self, id: NodeId) -> AstResult<()> {
        let classes = self.classes();
        let names = self.names();
        let node = self.doc.get_mut(id)?;
        if !classes.is_empty() {
            node.set_list("classes", classes);
        }
        if !names.is_empty() {
            node.set_list("ids", names.iter().map(|name| crate::names::make_id(name)).collect());
            node.set_list("names", names);
        }
        Ok(())
    }

    /// Parses `text` as inline markup and appends the nodes to `parent`.
    pub fn parse_inline_into(
        &mut self,
        text: &str,
        parent: NodeId,
        line: u32,
    ) -> Result<Vec<NodeId>, DirectiveError> {
        let source = InlineSource::single(text, self.span.start);
        let parsed = parse_inline(
            &source,
            &InlineOptions {
                source_name: self.global.source_name.clone(),
                extlinks: Arc::clone(&self.global.extlinks),
                default_role: self
                    .global
                    .default_role
                    .read()
                    .ok()
                    .and_then(|guard| guard.clone()),
                default_domain: self
                    .global
                    .default_domain
                    .read()
                    .ok()
                    .and_then(|guard| guard.clone()),
                custom_roles: self
                    .global
                    .custom_roles
                    .read()
                    .ok()
                    .map(|guard| guard.clone())
                    .unwrap_or_default(),
            },
        );
        let ctx = InlineContext {
            line,
            span: self.span,
            parent,
            source_name: self.global.source_name.clone(),
        };
        let (ids, _xrefs) = crate::inline::materialize_inline(self.doc, &parsed.nodes, parent, &ctx)?;
        Ok(ids)
    }

    /// Parses the directive content as a nested RST block.
    ///
    /// This reuses the Phase 04 block parser on the already-dedented content.
    /// Spans of cloned nodes refer to the *content string*, so they are
    /// deliberately cleared here; source lines are offset by the directive's
    /// marker line plus its option lines. Nested `Pending` nodes are copied
    /// verbatim and resolved by the outer fixpoint pass.
    pub fn parse_nested(&mut self) -> Result<Vec<NodeId>, DirectiveError> {
        let parent = self.parent;
        self.parse_nested_into(parent)
    }

    /// Parses the content as a nested RST block under an explicit `parent`.
    pub fn parse_nested_into(&mut self, parent: NodeId) -> Result<Vec<NodeId>, DirectiveError> {
        if self.content.trim().is_empty() {
            return Ok(Vec::new());
        }
        let base_line = self.line + 1 + self.raw_options.len() as u32;
        self.parse_block_text(self.content, parent, base_line)
    }

    /// Parses arbitrary RST text into `parent`, offsetting source lines by
    /// `base_line`. Used by `include` to parse an external file in-place.
    pub fn parse_block_text(
        &mut self,
        text: &str,
        parent: NodeId,
        base_line: u32,
    ) -> Result<Vec<NodeId>, DirectiveError> {
        let output = crate::parse_with_options(
            text,
            ParseOptions {
                source_name: self.global.source_name.clone(),
                tab_width: self.global.tab_width,
                prolog: None,
                epilog: None,
                default_role: None,
                default_domain: self
                    .global
                    .default_domain
                    .read()
                    .ok()
                    .and_then(|guard| guard.clone()),
                extlinks: Arc::clone(&self.global.extlinks),
            },
        );
        let mut result = Vec::new();
        let root = output.document.root;
        let ids = output.document.children(root).map_err(DirectiveError::from)?.to_vec();
        for id in ids {
            result.push(clone_subtree(&output.document, self.doc, id, parent, base_line)?);
        }
        Ok(result)
    }
}

/// Recursively clones a subtree from `from` into `to`, offsetting source lines.
pub fn clone_subtree(
    from: &Document,
    to: &mut Document,
    id: NodeId,
    parent: NodeId,
    line_offset: u32,
) -> AstResult<NodeId> {
    let source_node = from.get(id)?;
    let mut clone = source_node.clone();
    clone.children.clear();
    if let Some(pos) = clone.source.as_mut() {
        pos.line = pos.line.map(|line| line.saturating_add(line_offset));
        pos.offset = None;
    }
    // `Pending` directive nodes also carry their marker line as an attribute,
    // which `resolve_output` reads to stamp `DirectiveContext::line`. Offset it
    // as well; otherwise nested directives (and every signature xref they
    // generate) keep the sub-parse's relative line and diagnostics report a
    // wrong 1-based location.
    if let Ok(Some(line)) = clone.attrs.get_int(crate::pending::KEY_DIRECTIVE_LINE) {
        clone
            .attrs
            .set_int(crate::pending::KEY_DIRECTIVE_LINE, line.saturating_add(i64::from(line_offset)));
    }
    // Spans into the nested content string cannot be mapped to the parent
    // source without the original byte offset, which the frozen Pending
    // contract does not carry.
    clone.span = None;
    let new_id = to.add_node(clone, Some(parent))?;
    for child in source_node.children.clone() {
        clone_subtree(from, to, child, new_id, line_offset)?;
    }
    Ok(new_id)
}

/// The handler contract implemented by every built-in and extension directive.
pub trait Directive: Send + Sync {
    /// Canonical directive name (lower-case, hyphenated).
    fn name(&self) -> &'static str;

    /// Number of required arguments.
    fn required_arguments(&self) -> usize {
        0
    }

    /// Number of optional arguments beyond the required ones.
    fn optional_arguments(&self) -> usize {
        0
    }

    /// Whether the final argument may contain whitespace.
    fn final_argument_whitespace(&self) -> bool {
        false
    }

    /// Typed option specification.
    fn option_spec(&self) -> OptionSpec {
        OptionSpec::empty()
    }

    /// Whether the directive accepts an indented content block.
    fn has_content(&self) -> bool {
        false
    }

    /// Produces nodes under `ctx.parent`, returning their ids in order.
    fn run(&self, ctx: &mut DirectiveContext<'_>) -> Result<Vec<NodeId>, DirectiveError>;
}

/// Errors raised while validating or running a directive.
#[derive(Clone, Debug, Error, Diagnostic)]
pub enum DirectiveError {
    /// The directive name is not registered.
    #[error("unknown directive '{name}'")]
    #[diagnostic(
        code(scribe::rst::unknown_directive),
        help("register the directive or check the spelling"),
        severity(Error)
    )]
    UnknownDirective {
        /// Directive name as written.
        name: String,
        /// Byte span of the directive marker.
        #[label("unknown directive here")]
        span: SourceSpan,
    },

    /// A required argument was missing.
    #[error("directive '{name}' requires at least {required} argument(s), got {given}")]
    #[diagnostic(
        code(scribe::rst::missing_argument),
        help("supply the required directive argument on the marker line"),
        severity(Error)
    )]
    MissingArgument {
        /// Directive name.
        name: String,
        /// Required count.
        required: usize,
        /// Supplied count.
        given: usize,
        /// Byte span of the directive marker.
        #[label("arguments missing here")]
        span: SourceSpan,
    },

    /// Too many arguments were supplied.
    #[error("directive '{name}' accepts at most {maximum} argument(s), got {given}")]
    #[diagnostic(
        code(scribe::rst::too_many_arguments),
        help("remove the extra arguments or enable final_argument_whitespace"),
        severity(Error)
    )]
    TooManyArguments {
        /// Directive name.
        name: String,
        /// Maximum accepted count.
        maximum: usize,
        /// Supplied count.
        given: usize,
        /// Byte span of the directive marker.
        #[label("too many arguments here")]
        span: SourceSpan,
    },

    /// The directive requires a content block that was not supplied.
    #[error("directive '{name}' requires content")]
    #[diagnostic(
        code(scribe::rst::missing_content),
        help("indent a body under the directive marker"),
        severity(Error)
    )]
    MissingContent {
        /// Directive name.
        name: String,
        /// Byte span of the directive marker.
        #[label("content missing here")]
        span: SourceSpan,
    },

    /// Option validation failed.
    #[error("invalid directive options: {0}")]
    #[diagnostic(code(scribe::rst::option_value), severity(Error))]
    Option(#[from] OptionError),

    /// A `only` expression could not be evaluated.
    #[error("invalid 'only' expression '{expression}': {detail}")]
    #[diagnostic(
        code(scribe::rst::invalid_only_expression),
        help("use tag names combined with tag, not, and, or and parentheses"),
        severity(Warning)
    )]
    InvalidOnlyExpression {
        /// Expression as written.
        expression: String,
        /// Evaluator detail.
        detail: String,
        /// Byte span of the directive marker.
        #[label("expression here")]
        span: SourceSpan,
    },

    /// An included file could not be read.
    #[error("cannot read included file '{path}': {detail}")]
    #[diagnostic(
        code(scribe::rst::include_io),
        help("check the path and the source directory"),
        severity(Error)
    )]
    IncludeIo {
        /// Resolved path.
        path: String,
        /// I/O detail.
        detail: String,
    },

    /// A custom handler rejected its invocation.
    #[error("directive '{name}' failed: {detail}")]
    #[diagnostic(code(scribe::rst::directive_failed), severity(Error))]
    Failed {
        /// Directive name.
        name: String,
        /// Failure detail.
        detail: String,
        /// Byte span of the directive marker.
        #[label("directive here")]
        span: SourceSpan,
    },

    /// An arena operation failed.
    #[error(transparent)]
    #[diagnostic(code(scribe::rst::ast))]
    Ast(#[from] AstError),
}

impl DirectiveError {
    /// Stable diagnostic code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownDirective { .. } => "scribe::rst::unknown_directive",
            Self::MissingArgument { .. } => "scribe::rst::missing_argument",
            Self::TooManyArguments { .. } => "scribe::rst::too_many_arguments",
            Self::MissingContent { .. } => "scribe::rst::missing_content",
            Self::Option(error) => error.code(),
            Self::InvalidOnlyExpression { .. } => "scribe::rst::invalid_only_expression",
            Self::IncludeIo { .. } => "scribe::rst::include_io",
            Self::Failed { .. } => "scribe::rst::directive_failed",
            Self::Ast(_) => "scribe::rst::ast",
        }
    }

    /// Byte span when available.
    pub fn span(&self) -> Option<SourceSpan> {
        match self {
            Self::IncludeIo { .. } | Self::Ast(_) => None,
            Self::Option(error) => Some(error.span()),
            Self::UnknownDirective { span, .. }
            | Self::MissingArgument { span, .. }
            | Self::TooManyArguments { span, .. }
            | Self::MissingContent { span, .. }
            | Self::InvalidOnlyExpression { span, .. }
            | Self::Failed { span, .. } => Some(*span),
        }
    }
}

/// Thread-safe, case-insensitive registry of directive handlers.
#[derive(Default)]
pub struct DirectiveRegistry {
    entries: RwLock<HashMap<String, Arc<dyn Directive>>>,
}

impl std::fmt::Debug for DirectiveRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names = self.names();
        f.debug_struct("DirectiveRegistry")
            .field("names", &names)
            .finish_non_exhaustive()
    }
}

impl DirectiveRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a handler, replacing any previous handler with the same name.
    pub fn register<D: Directive + 'static>(&self, directive: D) {
        if let Ok(mut entries) = self.entries.write() {
            entries.insert(normalize(directive.name()), Arc::new(directive));
        }
    }

    /// Registers a pre-wrapped handler.
    pub fn register_arc(&self, directive: Arc<dyn Directive>) {
        if let Ok(mut entries) = self.entries.write() {
            entries.insert(normalize(directive.name()), directive);
        }
    }

    /// Looks up a handler by (case-insensitive) name.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Directive>> {
        self.entries
            .read()
            .ok()
            .and_then(|entries| entries.get(&normalize(name)).cloned())
    }

    /// `true` when `name` is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Sorted list of registered names.
    pub fn names(&self) -> Vec<String> {
        let mut names = self
            .entries
            .read()
            .map(|entries| entries.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        names.sort_unstable();
        names
    }

    /// Number of registered directives.
    pub fn len(&self) -> usize {
        self.entries.read().map(|entries| entries.len()).unwrap_or(0)
    }

    /// `true` when no directives are registered.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Registers every built-in directive (see [`crate::directives`]).
    pub fn builtin() -> Self {
        let registry = Self::new();
        crate::directives::register_builtins(&registry);
        registry
    }
}

fn normalize(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace('_', "-")
}

/// Internal helper used by handlers to split arguments.
pub(crate) fn split_arguments(raw: &str, final_whitespace: bool, maximum: usize) -> Vec<String> {
    if !final_whitespace || maximum == 0 {
        return raw.split_whitespace().map(str::to_owned).collect();
    }
    // Docutils uses `arg_text.split(None, required + optional - 1)`: the last
    // argument keeps the *raw* remainder (including embedded newlines), not a
    // whitespace-normalized copy. That distinction matters for directives like
    // `productionlist`, whose single final argument is a multi-line rule list.
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let mut parts = Vec::new();
    let mut rest = trimmed;
    for _ in 0..maximum.saturating_sub(1) {
        let Some(position) = rest.find(char::is_whitespace) else { break };
        parts.push(rest[..position].to_owned());
        rest = rest[position..].trim_start();
        if rest.is_empty() {
            break;
        }
    }
    if !rest.is_empty() {
        parts.push(rest.to_owned());
    }
    parts
}

/// Metadata recorded for an xref produced by a directive (currently unused by
/// built-ins, exposed for extensions). Kept here so Phases 11-14 can consume
/// directive-generated xrefs through the same contract as inline roles.
pub type DirectiveXref = (NodeId, XrefMeta);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_case_insensitive() {
        let registry = DirectiveRegistry::new();
        assert!(registry.get("NoTe").is_none());
        registry.register(crate::directives::AdmonitionDirective::note());
        assert!(registry.contains("NOTE"));
        assert_eq!(registry.get("nOte").map(|d| d.name()), Some("note"));
    }

    #[test]
    fn include_resolver_accepts_absolute_paths() {
        let resolver = FsIncludeResolver;
        let result = resolver.resolve(None, None, "/definitely/not/a/file");
        assert!(result.is_err());
    }
}
