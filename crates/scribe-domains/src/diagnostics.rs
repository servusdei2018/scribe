//! Domain diagnostics, including the exact nitpicky missing-reference report.
//!
//! `DomainDiagnostic` implements `miette::Diagnostic` manually (rather than via
//! derive) because the diagnostic *code* is data-dependent: the common case is
//! `scribe::domains::missing_reference`, while consistency checks use their own
//! stable codes.

use std::fmt::Display;

use miette::{Diagnostic, LabeledSpan, SourceSpan};
use scribe_ast::{SourcePos, Span};
use thiserror::Error;

/// Stable diagnostic codes emitted by this crate.
pub mod codes {
    /// An unresolved cross-reference reported in nitpicky mode.
    pub const MISSING_REFERENCE: &str = "scribe::domains::missing_reference";
    /// A duplicate or conflicting object registration.
    pub const DUPLICATE_OBJECT: &str = "scribe::domains::duplicate_object";
    /// A malformed object description or xref payload.
    pub const MALFORMED_OBJECT: &str = "scribe::domains::malformed_object";
    /// Incremental inventory inconsistency (`check_consistency`).
    pub const INCONSISTENCY: &str = "scribe::domains::inconsistency";
    /// A malformed intersphinx mapping or missing external inventory.
    pub const INTERSPHINX: &str = "scribe::domains::intersphinx";
}

/// Severity of a domain diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DomainSeverity {
    /// Informational note.
    Info,
    /// Recoverable warning (the usual case for missing references).
    Warning,
    /// Unrecoverable error (reserved for consistency checks).
    Error,
}

impl Display for DomainSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        })
    }
}

/// A user-facing domain diagnostic.
///
/// `message` is the exact upstream text (for example `undefined label: 'x'`),
/// `target` and `typ` are kept structured so builders can suppress warnings via
/// `suppress_warnings = ["ref", "ref.label"]`, and `help` carries the corrective
/// hint rendered by `miette`.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{severity}: {message}")]
pub struct DomainDiagnostic {
    /// Stable machine-readable code, e.g. `scribe::domains::missing_reference`.
    pub code: String,
    /// Exact upstream message.
    pub message: String,
    /// Reference type (`ref`, `doc`, `term`, ...), when applicable.
    pub typ: Option<String>,
    /// Reference target, when applicable.
    pub target: Option<String>,
    /// Document in which the problem was found.
    pub docname: Option<String>,
    /// Byte span of the offending node.
    pub span: Option<Span>,
    /// Source path carried by the offending AST node, when present.
    ///
    /// Additive span plumbing: builders use this (together with `line`) to
    /// render the `path:LINE: WARNING:` prefix upstream emits.
    pub source_path: Option<String>,
    /// 1-based source line carried by the offending AST node, when present.
    pub line: Option<u32>,
    /// Corrective hint shown by `miette`.
    pub help: Option<String>,
    /// Severity.
    pub severity: DomainSeverity,
}

impl Diagnostic for DomainDiagnostic {
    fn code<'a>(&'a self) -> Option<Box<dyn Display + 'a>> {
        Some(Box::new(self.code.as_str()))
    }

    fn help<'a>(&'a self) -> Option<Box<dyn Display + 'a>> {
        self.help.as_deref().map(|help| Box::new(help) as Box<dyn Display + 'a>)
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        self.span.map(|span| {
            let source_span = SourceSpan::new((span.start as usize).into(), span.len() as usize);
            Box::new(std::iter::once(LabeledSpan::new_with_span(
                Some("here".to_owned()),
                source_span,
            ))) as Box<dyn Iterator<Item = LabeledSpan> + '_>
        })
    }

    fn severity(&self) -> Option<miette::Severity> {
        Some(match self.severity {
            DomainSeverity::Info => miette::Severity::Advice,
            DomainSeverity::Warning => miette::Severity::Warning,
            DomainSeverity::Error => miette::Severity::Error,
        })
    }
}

impl DomainDiagnostic {
    /// Creates a missing-reference diagnostic for `typ` / `target`.
    pub fn missing_reference(
        message: impl Into<String>,
        typ: impl Into<String>,
        target: impl Into<String>,
        docname: Option<String>,
        span: Option<Span>,
    ) -> Self {
        let typ = typ.into();
        let target = target.into();
        let help = match typ.as_str() {
            "ref" | "keyword" | "numref" => format!(
                "define an explicit label with `.. _{}:` (lowercase for :ref:) or fix the target",
                target
            ),
            "doc" => format!("create a document named `{target}` or fix the relative path"),
            "term" => format!("add `{target}` to a `.. glossary::` directive"),
            "option" => format!("document `{target}` with `.. option::` inside a `.. program::` block"),
            "envvar" => format!("document `{target}` with `.. envvar::`"),
            "confval" => format!("document `{target}` with `.. confval::`"),
            _ => format!("define `{target}` in the `{typ}` object table or fix the target"),
        };
        Self {
            code: codes::MISSING_REFERENCE.to_owned(),
            message: message.into(),
            typ: Some(typ),
            target: Some(target),
            docname,
            span,
            source_path: None,
            line: None,
            help: Some(help),
            severity: DomainSeverity::Warning,
        }
    }

    /// Creates a duplicate-object diagnostic.
    pub fn duplicate_object(
        message: impl Into<String>,
        typ: impl Into<String>,
        target: impl Into<String>,
        docname: Option<String>,
        span: Option<Span>,
    ) -> Self {
        Self {
            code: codes::DUPLICATE_OBJECT.to_owned(),
            message: message.into(),
            typ: Some(typ.into()),
            target: Some(target.into()),
            docname,
            span,
            source_path: None,
            line: None,
            help: Some("remove or rename the duplicate declaration".to_owned()),
            severity: DomainSeverity::Warning,
        }
    }

    /// Creates a malformed-object diagnostic.
    pub fn malformed(
        message: impl Into<String>,
        typ: Option<String>,
        target: Option<String>,
        docname: Option<String>,
        span: Option<Span>,
    ) -> Self {
        Self {
            code: codes::MALFORMED_OBJECT.to_owned(),
            message: message.into(),
            typ,
            target,
            docname,
            span,
            source_path: None,
            line: None,
            help: Some("check the directive signature or role payload".to_owned()),
            severity: DomainSeverity::Warning,
        }
    }

    /// Creates a consistency-check diagnostic.
    pub fn inconsistency(message: impl Into<String>, docname: Option<String>, span: Option<Span>) -> Self {
        Self {
            code: codes::INCONSISTENCY.to_owned(),
            message: message.into(),
            typ: None,
            target: None,
            docname,
            span,
            source_path: None,
            line: None,
            help: None,
            severity: DomainSeverity::Error,
        }
    }

    /// Attaches the source path and 1-based line of an AST node to the
    /// diagnostic. Builders use these to render upstream's located-warning
    /// prefix. Returns `self` unchanged when the node carries no source.
    pub fn with_source(mut self, source: Option<&SourcePos>) -> Self {
        if let Some(source) = source {
            self.source_path = source.source.clone();
            self.line = source.line;
        }
        self
    }

    /// True when this diagnostic is a missing-reference report.
    pub fn is_missing_reference(&self) -> bool {
        self.code == codes::MISSING_REFERENCE
    }
}

/// A thread-safe sink accumulating domain diagnostics.
#[derive(Debug, Default)]
pub struct DiagnosticSink {
    entries: parking_lot::Mutex<Vec<DomainDiagnostic>>,
}

impl DiagnosticSink {
    /// Pushes a diagnostic.
    pub fn push(&self, diagnostic: DomainDiagnostic) {
        self.entries.lock().push(diagnostic);
    }

    /// Returns a snapshot in emission order.
    pub fn snapshot(&self) -> Vec<DomainDiagnostic> {
        self.entries.lock().clone()
    }

    /// Drains all diagnostics in emission order.
    pub fn take(&self) -> Vec<DomainDiagnostic> {
        std::mem::take(&mut self.entries.lock())
    }

    /// Number of diagnostics currently held.
    pub fn len(&self) -> usize {
        self.entries.lock().len()
    }

    /// True when the sink is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_reference_has_stable_code_and_help() {
        let diagnostic = DomainDiagnostic::missing_reference(
            "undefined label: 'no-label'",
            "ref",
            "no-label",
            Some("index".into()),
            Some(Span::new(10, 4)),
        );
        assert_eq!(diagnostic.code, codes::MISSING_REFERENCE);
        assert_eq!(diagnostic.message, "undefined label: 'no-label'");
        assert_eq!(diagnostic.target.as_deref(), Some("no-label"));
        assert_eq!(diagnostic.span, Some(Span::new(10, 4)));
        assert!(diagnostic.help.as_ref().is_some_and(|help| help.contains("no-label")));
        assert_eq!(
            miette::Diagnostic::code(&diagnostic).map(|code| code.to_string()).as_deref(),
            Some(codes::MISSING_REFERENCE)
        );
        assert!(diagnostic.is_missing_reference());
    }
}
