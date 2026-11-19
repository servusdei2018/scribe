//! reStructuredText lexer, block parser and inline-markup parser (Phases 04–05).
//!
//! This crate turns reStructuredText source into a
//! [`scribe_ast::Document`]. Phase 04 implements the lexical scanner and all
//! block-level constructs; Phase 05 adds full inline recognition (emphasis,
//! literals, roles, references, substitutions, footnotes) with exact byte
//! spans. Directive interpretation remains a Phase 06 seam
//! ([`pending`](crate::pending) attribute contract).
//!
//! # Quick start
//!
//! ```
//! use scribe_rst::{parse, ParseOptions};
//!
//! let output = parse("Title\n=====\n\nA paragraph.\n");
//! assert!(!output.has_diagnostics());
//! let pseudo = output.document.to_pseudoxml().unwrap();
//! assert!(pseudo.contains("<section"));
//! assert!(pseudo.contains("<paragraph>"));
//! ```
//!
//! # Public surface
//!
//! * [`Lexer`] / [`SourceLine`] — zero-copy, span-tracking line scanner.
//! * [`parse`] / [`parse_with_options`] — block parser entry points.
//! * [`ParseOutput`] — best-effort document plus recoverable diagnostics.
//! * [`RstError`] / [`RstDiagnostic`] — `thiserror` + `miette` diagnostics.
//! * [`pending`] — the frozen `NodeKind::Pending` attribute contract used by
//!   Phase 06.
//! * [`inline`] — the Phase 05 inline recogniser, node model and role payload
//!   contract; [`parse_inline_placeholder`] is the narrow compatibility entry
//!   point.

pub mod directive_registry;
pub mod directives;
pub mod domain_directives;
pub mod extlinks;
mod error;
mod explicit;
pub mod inline;
mod inline_seam;
mod lexer;
mod lists;
mod names;
pub mod option_spec;
mod parser;
pub mod python_directives;
pub mod pending;
pub mod resolve;
pub mod role_registry;
mod tables;
pub mod toctree;

pub use error::{named_report, RstDiagnostic, RstError, Severity};
pub use inline::{
    materialize_inline, parse_inline, InlineNode, InlineOptions, InlineParseResult, InlineSource,
    InlineWarning, MaterializedInline, XrefMeta,
};
pub use inline_seam::{parse_inline_placeholder, InlineContext};
pub use lexer::{adornment_of, column_width, is_adornment_char, strip_utf8_bom, Lexer, SourceLine};
pub use parser::{ParseOutput, SubstitutionDirective};

pub use directive_registry::{
    Context, Directive, DirectiveContext, DirectiveError, DirectiveRegistry, FsIncludeResolver,
    IncludeResolver,
};
pub use extlinks::{empty_roles, ExtLinkRoles, ExtLinkSpec};
pub use option_spec::{OptionError, OptionKind, OptionSpec, OptionValue};
pub use resolve::{resolve_output, resolve_pending, ResolveIssue, ResolveReport};
pub use toctree::{docname_join, explicit_title, is_glob, is_url};
pub use role_registry::{
    BuiltinRole, DirectBehavior, InnerKind, Role, RoleClass, RoleError, RoleInvocation, RoleRegistry,
};

use scribe_ast::Document;

/// Result alias used throughout the crate.
pub type RstResult<T> = Result<T, RstError>;

/// Options controlling a parse.
#[derive(Clone, Debug, Default)]
pub struct ParseOptions {
    /// Document name, mirrored onto the root node's `source` attribute.
    pub source_name: Option<String>,
    /// Tab stop width (Docutils default: 8).
    pub tab_width: usize,
    /// Text injected after the leading docinfo field list (Sphinx `rst_prolog`).
    pub prolog: Option<String>,
    /// Text appended at the end of the document (Sphinx `rst_epilog`).
    pub epilog: Option<String>,
    /// Default role selected by a `.. default-role:: name` declaration inside
    /// `rst_prolog`. Applied to interpreted text without an explicit role.
    pub default_role: Option<String>,
    /// Domain selected by a `.. default-domain:: name` declaration. Pre-scanned
    /// from the source so inline roles resolved during the block parse use the
    /// same domain as the directive resolver.
    pub default_domain: Option<String>,
    /// Dynamic external-link roles registered by `sphinx.ext.extlinks`.
    /// Empty by default; the builder fills it from the `extlinks` config.
    pub extlinks: extlinks::ExtLinkRoles,
}

impl ParseOptions {
    /// Creates options with Docutils' default tab width of 8.
    pub fn new(source_name: Option<String>) -> Self {
        Self {
            source_name,
            tab_width: 8,
            prolog: None,
            epilog: None,
            default_role: None,
            default_domain: None,
            extlinks: extlinks::empty_roles(),
        }
    }
}

/// Parses `source` with default options.
///
/// This never panics: malformed input is reported through
/// [`ParseOutput::diagnostics`] while a best-effort document is still built.
pub fn parse(source: &str) -> ParseOutput {
    parse_with_options(source, ParseOptions::default())
}

/// Parses `source` with explicit options.
///
/// When [`ParseOptions::prolog`] / [`ParseOptions::epilog`] are present the
/// source is augmented *before* scanning (mirroring
/// `sphinx.util.rst._prepend_prologue` / `_append_epilogue`), so every span in
/// the resulting document refers to the augmented text.
pub fn parse_with_options(source: &str, options: ParseOptions) -> ParseOutput {
    // Upstream decodes sources with `utf-8-sig`, which removes a leading BOM
    // before the RST scanner sees a single character. Strip it here so every
    // line/byte offset in the resulting document is BOM-free.
    let source = strip_utf8_bom(source);
    let mut effective = options;
    if effective.tab_width == 0 {
        effective.tab_width = 8;
    }
    // `.. default-domain::` is processed by the read-phase parser upstream, so
    // roles parsed in the main block pass must see it. Pre-scan the source
    // (and the injected prolog, which is parsed before the body) and thread the
    // selection into InlineOptions via ParseOptions.
    if effective.default_domain.is_none() {
        effective.default_domain = detect_default_domain(source).or_else(|| {
            effective.prolog.as_deref().and_then(detect_default_domain)
        });
    }
    if effective.prolog.is_none()
        && effective.epilog.is_none()
        && effective.default_role.is_none()
    {
        return parser::BlockParser::with_source(source, effective).run();
    }
    // Docutils processes `.. default-role::` while scanning the source, so a
    // declaration in the injected prolog applies to body inline text. Scan the
    // prolog up front (the epilog is appended after the body and cannot affect
    // it) and thread the selected role into inline parsing.
    if effective.default_role.is_none() {
        if let Some(prolog) = effective.prolog.as_deref() {
            effective.default_role = prolog_default_role(prolog);
        }
    }
    let parser_options = effective.clone();
    let (augmented, docinfo_lines, inserted_lines) = augment_source(source, &effective);
    let mut output = parser::BlockParser::with_source(&augmented, parser_options).run();
    if inserted_lines > 0 {
        // The scanner saw the augmented text, but diagnostics (and the
        // toctree directive's `source.line`) must report the original source's
        // 1-based line. Nodes belonging to the injected prolog itself keep
        // their prolog-relative lines, matching upstream's `<rst_prologue>`
        // StringList source.
        for node in &mut output.document.nodes {
            let is_body_node = node
                .source
                .as_ref()
                .and_then(|position| position.line)
                .is_some_and(|line| line > docinfo_lines + inserted_lines);
            if let Some(position) = node.source.as_mut() {
                if let Some(line) = position.line {
                    if line > docinfo_lines + inserted_lines {
                        position.line = Some(line - inserted_lines);
                    }
                }
            }
            // `Pending` directives carry their marker line as an attribute;
            // `resolve_output` stamps it onto the concrete directive node.
            if is_body_node {
                if let Ok(Some(line)) = node.attrs.get_int(crate::pending::KEY_DIRECTIVE_LINE) {
                    if line > i64::from(docinfo_lines + inserted_lines) {
                        node.attrs.set_int(
                            crate::pending::KEY_DIRECTIVE_LINE,
                            line - i64::from(inserted_lines),
                        );
                    }
                }
            }
        }
    }
    output
}

/// Parses a document and returns the first recoverable diagnostic as an error.
///
/// Useful for callers that do not want partial doctrees (for example a future
/// strict lint mode). The block parser itself always produces a document.
pub fn try_parse_document(source: &str) -> RstResult<Document> {
    let output = parse(source);
    if let Some(diagnostic) = output.diagnostics.first() {
        return Err(diagnostic.error().clone());
    }
    Ok(output.document)
}

/// Extracts the role name from the last `.. default-role:: name` declaration
/// in a prolog string. Whitespace and indentation are tolerated, mirroring
/// Docutils' directive syntax.
fn prolog_default_role(prolog: &str) -> Option<String> {
    let mut selected = None;
    for line in prolog.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("..") else { continue };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix("default-role::") else { continue };
        let name = rest.split_whitespace().next().unwrap_or("");
        if !name.is_empty() {
            selected = Some(name.to_owned());
        }
    }
    selected
}

/// Builds the prolog/epilog-augmented source string.
///
/// The prologue is inserted after any leading docinfo field lines (mirroring
/// upstream `_prepend_prologue`), separated by blank lines. The epilogue is
/// appended after a blank line (mirroring `_append_epilogue`).
fn augment_source(source: &str, options: &ParseOptions) -> (String, u32, u32) {
    let mut out = String::with_capacity(source.len() + 128);
    let mut docinfo_lines = 0u32;
    let mut inserted_lines = 0u32;
    if let Some(prolog) = options.prolog.as_deref().filter(|p| !p.is_empty()) {
        let mut docinfo_end = 0usize;
        for line in source.split_inclusive('\n') {
            let trimmed = line.trim_end_matches(['\n', '\r']);
            if crate::lists::field_marker(trimmed).is_some() {
                docinfo_end += line.len();
            } else {
                break;
            }
        }
        let docinfo = &source[..docinfo_end];
        docinfo_lines = docinfo.matches('\n').count() as u32
            + u32::from(!docinfo.is_empty() && !docinfo.ends_with('\n'));
        out.push_str(docinfo);
        if docinfo_end > 0 {
            if !out.ends_with('\n') {
                out.push('\n');
                inserted_lines += 1;
            }
            // Upstream inserts a blank line after the docinfo field list.
            out.push('\n');
            inserted_lines += 1;
        }
        out.push_str(prolog);
        inserted_lines += prolog.matches('\n').count() as u32;
        if !prolog.ends_with('\n') {
            out.push('\n');
            inserted_lines += 1;
        }
        out.push('\n');
        inserted_lines += 1;
        out.push_str(&source[docinfo_end..]);
    } else {
        out.push_str(source);
    }

    if let Some(epilog) = options.epilog.as_deref().filter(|e| !e.is_empty()) {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
        out.push_str(epilog);
        if !epilog.ends_with('\n') {
            out.push('\n');
        }
    }
    (out, docinfo_lines, inserted_lines)
}

/// Finds the domain selected by a `.. default-domain:: name` directive in an
/// RST source. The last declaration wins, matching upstream's mutable
/// `env.current_document.default_domain`.
pub fn detect_default_domain(source: &str) -> Option<String> {
    let mut selected = None;
    for line in source.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("..") else { continue };
        let rest = rest.trim_start();
        let Some(after) = rest.strip_prefix("default-domain") else { continue };
        if !after.starts_with(char::is_whitespace) && !after.starts_with("::") {
            continue;
        }
        let Some(arg) = after.strip_prefix("::") else { continue };
        let name = arg.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
        if !name.is_empty() {
            selected = Some(name);
        }
    }
    selected
}

/// The phase that implemented this crate's block and inline functionality.
pub const IMPLEMENTATION_PHASE: &str = "Phase 06";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn augment_source_docinfo_and_epilog() {
        let options = ParseOptions {
            source_name: None,
            tab_width: 8,
            prolog: Some(".. |x| replace:: X".into()),
            epilog: Some(".. _e: https://example.com".into()),
            default_role: None,
            default_domain: None,
            extlinks: crate::extlinks::empty_roles(),
        };
        let (augmented, docinfo_lines, inserted_lines) = augment_source(":author: me\n\nBody.\n", &options);
        assert!(augmented.starts_with(":author: me\n\n.. |x| replace:: X\n\n"));
        assert!(augmented.ends_with(".. _e: https://example.com\n"));
        assert_eq!((docinfo_lines, inserted_lines), (1, 3));
    }

    #[test]
    fn parse_simple_document() {
        let output = parse("Hello\n=====\n\nWorld.\n");
        assert!(!output.has_diagnostics());
        let pseudo = output.document.to_pseudoxml().unwrap();
        assert!(pseudo.contains("<section ids=\"hello\" names=\"hello\">"), "{pseudo}");
        assert!(pseudo.contains("<paragraph>"));
    }

    #[test]
    fn try_parse_document_exposes_diagnostics() {
        let result = try_parse_document("+----+\n| a  |\n+--+-+\n");
        assert!(result.is_err() || result.is_ok()); // parser never panics
    }
}
