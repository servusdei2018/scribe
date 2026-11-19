//! MyST Markdown parser integration (Phase 07).
//!
//! This crate turns MyST-flavoured Markdown into the same frozen
//! [`scribe_ast::Document`] model as `scribe-rst`. CommonMark core is parsed
//! with [`pulldown-cmark`] and lowered directly into arena nodes with exact
//! byte spans; MyST extensions (frontmatter, directive fences, roles, targets,
//! math, footnotes, definition lists) are layered on top.
//!
//! # Quick start
//!
//! ```
//! use scribe_myst::{parse, parse_and_resolve};
//!
//! let output = parse("# Title\n\nSee {ref}`label`.\n").unwrap();
//! assert!(output.frontmatter.is_none());
//! assert_eq!(output.pending_xrefs().count(), 1);
//! let pseudo = output.document.to_pseudoxml().unwrap();
//! assert!(pseudo.contains("<section"), "{pseudo}");
//!
//! // Directives can be resolved with the shared RST registry.
//! let (output, report) = parse_and_resolve("```{note}\nHi.\n```\n", Default::default()).unwrap();
//! assert!(report.is_clean(), "{:?}", report.diagnostics());
//! ```
//!
//! # Compatibility contract
//!
//! * Fenced directives emit `NodeKind::Pending` with the exact Phase 06
//!   attribute shape (§8.5 of `ARCHITECTURE.md`), so
//!   [`scribe_rst::resolve_pending`] resolves them without re-reading source.
//! * Inline roles emit the same concrete / `PendingXref` nodes as RST roles
//!   (§9.4), with a matching [`XrefMeta`] side table.
//! * Node kinds, nesting, ids and names mirror RST for equivalent markup; see
//!   `ARCHITECTURE.md` §11 for the full mapping.

pub mod directives;
mod error;
mod frontmatter;
mod lower;
mod names;
mod preprocessor;
mod roles;

use scribe_ast::{Document, NodeId};
use scribe_rst::{resolve_pending, Context, ResolveReport};

pub use error::{named_report, MystDiagnostic, MystError, MystResult, Severity};
pub use frontmatter::Frontmatter;
pub use scribe_rst::{DirectiveRegistry, XrefMeta};

/// A cross-reference target discovered in a MyST document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Anchor {
    /// Normalized label used by `{ref}` links.
    pub label: String,
    /// Node the target is attached to (a `Section` or a `Target`).
    pub node_id: NodeId,
    /// Byte span of the `(label)=` marker in the source.
    pub span: scribe_ast::Span,
    /// Whether the anchor came from a heading or a standalone target.
    pub kind: AnchorKind,
}

/// Source of a MyST cross-reference anchor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnchorKind {
    /// `(label)=` immediately before a heading.
    Heading,
    /// `(label)=` before any other block (emits a `Target` node).
    Target,
}

/// Options controlling a MyST parse.
#[derive(Clone, Debug, Default)]
pub struct ParseOptions {
    /// Document name, mirrored onto the root node and used for `refdoc`.
    pub source_name: Option<String>,
}

impl ParseOptions {
    /// Creates options for a named document.
    pub fn new(source_name: impl Into<String>) -> Self {
        Self {
            source_name: Some(source_name.into()),
        }
    }
}

/// Result of a MyST parse.
///
/// Besides the best-effort [`Document`], additive side tables expose MyST
/// features that have no frozen AST attribute: parsed YAML frontmatter, the
/// pending-xref contract, and discovered cross-reference targets. These are
/// consumed by later build/domain phases (08/11–14).
#[derive(Debug)]
pub struct ParseOutput {
    /// The lowered doctree.
    pub document: Document,
    /// Recoverable diagnostics (malformed frontmatter, ...).
    pub diagnostics: Vec<MystDiagnostic>,
    /// Parsed YAML frontmatter, when a leading `---` block was present.
    pub frontmatter: Option<Frontmatter>,
    /// `(NodeId, XrefMeta)` side table for every `PendingXref`.
    pub xrefs: Vec<(NodeId, XrefMeta)>,
    /// Cross-reference targets discovered in the document.
    pub anchors: Vec<Anchor>,
    source_name: Option<String>,
    source: String,
}

impl ParseOutput {
    /// `true` when at least one recoverable diagnostic was produced.
    pub fn has_diagnostics(&self) -> bool {
        !self.diagnostics.is_empty()
    }

    /// Consumes the output and returns just the document.
    pub fn into_document(self) -> Document {
        self.document
    }

    /// Iterates over the pending-xref side table.
    pub fn pending_xrefs(&self) -> impl Iterator<Item = &(NodeId, XrefMeta)> {
        self.xrefs.iter()
    }

    /// Looks up the top-level frontmatter value for `key`, if present.
    pub fn frontmatter_str(&self, key: &str) -> Option<&str> {
        self.frontmatter.as_ref()?.get_str(key)
    }

    /// Source text this output was produced from.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Document name used for this parse.
    pub fn source_name(&self) -> Option<&str> {
        self.source_name.as_deref()
    }
}

/// Parses MyST Markdown with default options.
///
/// Never panics: malformed constructs become recoverable diagnostics while a
/// best-effort document is still returned.
pub fn parse(source: &str) -> MystResult<ParseOutput> {
    parse_with_options(source, ParseOptions::default())
}

/// Parses MyST Markdown with explicit options.
pub fn parse_with_options(source: &str, options: ParseOptions) -> MystResult<ParseOutput> {
    let mut document = Document::new(options.source_name.clone());
    let lowered = lower::lower(source, &options, &mut document)?;
    Ok(ParseOutput {
        diagnostics: lowered
            .diagnostics
            .into_iter()
            .map(|error| {
                error.into_diagnostic(options.source_name.clone(), source.to_owned())
            })
            .collect(),
        frontmatter: lowered.frontmatter,
        xrefs: lowered.xrefs,
        anchors: lowered.anchors,
        document,
        source_name: options.source_name.clone(),
        source: source.to_owned(),
    })
}

/// Parses and immediately resolves directive `Pending` nodes with the shared
/// RST registry and default build context.
///
/// The context is seeded with the document's name and source so `include` and
/// diagnostics behave as they do for RST. Cross-reference `PendingXref` nodes
/// are intentionally left for Phases 11–14. Nested MyST directives are
/// supported because nested fenced blocks are converted to RST directive
/// syntax in the outer directive's `content` payload (see
/// [`directives::myst_content_to_rst`]).
pub fn parse_and_resolve(
    source: &str,
    options: ParseOptions,
) -> MystResult<(ParseOutput, ResolveReport)> {
    let mut output = parse_with_options(source, options)?;
    let report = resolve(&mut output, &DirectiveRegistry::builtin());
    Ok((output, report))
}

/// Resolves directive `Pending` nodes in an existing MyST parse output.
///
/// Callers that own a build context (Phase 08+) should prefer this entry point
/// so tags, config, and include resolution are shared with RST documents.
pub fn resolve(output: &mut ParseOutput, registry: &DirectiveRegistry) -> ResolveReport {
    let context = Context {
        source_name: output.source_name.clone(),
        source_text: Some(std::sync::Arc::new(output.source.clone())),
        ..Context::default()
    };
    resolve_pending(&mut output.document, registry, &context)
}

/// Resolves directive `Pending` nodes with a caller-supplied build context.
pub fn resolve_with_context(
    output: &mut ParseOutput,
    registry: &DirectiveRegistry,
    context: &Context,
) -> ResolveReport {
    resolve_pending(&mut output.document, registry, context)
}

/// The phase that implemented this crate's functionality.
pub const IMPLEMENTATION_PHASE: &str = "Phase 07";
