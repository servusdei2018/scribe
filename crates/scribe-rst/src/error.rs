//! Structured errors and user-facing diagnostics for the RST parser.
//!
//! Two layers are provided:
//!
//! * [`RstError`] is the `thiserror` error type returned by the fallible
//!   internal parser helpers. It also implements [`miette::Diagnostic`] so it
//!   can be rendered directly, carrying a byte [`SourceSpan`](miette::SourceSpan)
//!   plus an actionable help label.
//! * [`RstDiagnostic`] is the user-facing wrapper stored in
//!   [`ParseOutput::diagnostics`](crate::ParseOutput::diagnostics). It binds an
//!   `RstError` to the source text and name so miette can compute the 1-based
//!   line/column and render a full snippet with `miette::Report`.

use miette::{Diagnostic, LabeledSpan, SourceCode, SourceSpan};
use scribe_ast::AstError;
use thiserror::Error;

/// Severity of a recoverable parse diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// A fatal problem that prevented parsing a construct (recovery still attempted).
    Error,
    /// A problem where the parser guessed at the author's intent.
    Warning,
    /// A purely informational note (duplicate implicit target, etc.).
    Info,
}

impl From<Severity> for miette::Severity {
    fn from(value: Severity) -> Self {
        match value {
            Severity::Error => miette::Severity::Error,
            Severity::Warning => miette::Severity::Warning,
            Severity::Info => miette::Severity::Advice,
        }
    }
}

/// Internal parser error, convertible into a rendered [`RstDiagnostic`].
#[derive(Clone, Debug, Error, Diagnostic)]
pub enum RstError {
    /// A line or construct is indented inconsistently with its neighbours.
    #[error("inconsistent indentation: {message}")]
    #[diagnostic(
        code(scribe::rst::indentation),
        help("check indentation of list items and nested blocks"),
        severity(Error)
    )]
    Indentation {
        /// Human-readable detail.
        message: String,
        /// Byte range in the original source.
        #[label("unexpected indentation here")]
        span: SourceSpan,
    },

    /// Section adornment (overline/underline) is malformed or mismatched.
    #[error("bad section adornment: {message}")]
    #[diagnostic(
        code(scribe::rst::adornment),
        help("use a consistent overline/underline character and length for each section level"),
        severity(Error)
    )]
    Adornment {
        /// Human-readable detail.
        message: String,
        /// Byte range in the original source.
        #[label("adornment here")]
        span: SourceSpan,
    },

    /// A table's markup is malformed (unaligned borders, missing bottom, ...).
    #[error("malformed table: {message}")]
    #[diagnostic(
        code(scribe::rst::table),
        help("check that table borders align and that every cell is closed"),
        severity(Error)
    )]
    Table {
        /// Human-readable detail.
        message: String,
        /// Byte range in the original source.
        #[label("table markup here")]
        span: SourceSpan,
    },

    /// An explicit markup construct was started but never completed.
    #[error("unterminated explicit markup: {message}")]
    #[diagnostic(
        code(scribe::rst::explicit_markup),
        help("close the construct with a blank line or dedent to column zero"),
        severity(Error)
    )]
    UnterminatedExplicitMarkup {
        /// Human-readable detail.
        message: String,
        /// Byte range in the original source.
        #[label("explicit markup starts here")]
        span: SourceSpan,
    },

    /// A construct requires an indented body that was not found.
    #[error("missing body: {message}")]
    #[diagnostic(
        code(scribe::rst::missing_body),
        help("indent the body relative to the introducing line"),
        severity(Error)
    )]
    MissingBody {
        /// Human-readable detail.
        message: String,
        /// Byte range in the original source.
        #[label("introducer here")]
        span: SourceSpan,
    },

    /// A non-fatal note about otherwise well-formed input.
    #[error("{message}")]
    #[diagnostic(code(scribe::rst::note), severity(Advice))]
    Note {
        /// Human-readable detail.
        message: String,
        /// Byte range in the original source.
        #[label("here")]
        span: SourceSpan,
    },

    /// An arena operation failed. This signals an internal invariant violation
    /// (the block parser only ever passes live ids), never bad user input.
    #[error("AST construction failed: {0}")]
    #[diagnostic(code(scribe::rst::ast))]
    Ast(#[from] AstError),
}

impl RstError {
    /// The stable diagnostic code of this error.
    pub fn code(&self) -> &'static str {
        match self {
            RstError::Indentation { .. } => "scribe::rst::indentation",
            RstError::Adornment { .. } => "scribe::rst::adornment",
            RstError::Table { .. } => "scribe::rst::table",
            RstError::UnterminatedExplicitMarkup { .. } => "scribe::rst::explicit_markup",
            RstError::MissingBody { .. } => "scribe::rst::missing_body",
            RstError::Note { .. } => "scribe::rst::note",
            RstError::Ast(_) => "scribe::rst::ast",
        }
    }

    /// Severity used for this error when it becomes a [`RstDiagnostic`].
    pub fn severity(&self) -> Severity {
        match self {
            RstError::Note { .. } | RstError::Ast(_) => Severity::Info,
            _ => Severity::Error,
        }
    }

    /// Human-readable help text, independent of miette rendering.
    pub fn help_text(&self) -> &'static str {
        match self {
            RstError::Indentation { .. } => "check indentation of list items and nested blocks",
            RstError::Adornment { .. } => {
                "use a consistent overline/underline character and length for each section level"
            }
            RstError::Table { .. } => "check that table borders align and that every cell is closed",
            RstError::UnterminatedExplicitMarkup { .. } => {
                "close the construct with a blank line or dedent to column zero"
            }
            RstError::MissingBody { .. } => "indent the body relative to the introducing line",
            RstError::Note { .. } | RstError::Ast(_) => "",
        }
    }

    /// Byte span of the error, or `None` for AST errors without source info.
    pub fn span(&self) -> Option<SourceSpan> {
        match self {
            RstError::Indentation { span, .. }
            | RstError::Adornment { span, .. }
            | RstError::Table { span, .. }
            | RstError::UnterminatedExplicitMarkup { span, .. }
            | RstError::MissingBody { span, .. }
            | RstError::Note { span, .. } => Some(*span),
            RstError::Ast(_) => None,
        }
    }

    /// Wraps this error in a full, renderable [`RstDiagnostic`].
    pub fn into_diagnostic(self, source_name: Option<String>, source: String) -> RstDiagnostic {
        RstDiagnostic::new(self, source_name, source)
    }
}

/// A user-facing diagnostic: [`RstError`] bound to its source text.
///
/// Because it implements [`miette::Diagnostic`] and exposes
/// [`source_code`](Diagnostic::source_code), `miette::Report::new(diag)` renders
/// the code, message, 1-based line/column, labelled span, and help text.
#[derive(Clone, Debug, Error)]
#[error("{error}")]
pub struct RstDiagnostic {
    /// Source text plus document name, used by miette for line/column and the
    /// snippet label.
    source: miette::NamedSource<String>,
    /// The underlying parse error.
    #[source]
    error: RstError,
}

impl RstDiagnostic {
    /// Creates a diagnostic from an error and the source it refers to.
    pub fn new(error: RstError, source_name: Option<String>, source: String) -> Self {
        let name = source_name.unwrap_or_else(|| "<rst>".to_owned());
        Self {
            source: miette::NamedSource::new(name, source),
            error,
        }
    }

    /// Borrows the underlying error.
    pub fn error(&self) -> &RstError {
        &self.error
    }

    /// The stable diagnostic code.
    pub fn code(&self) -> &'static str {
        self.error.code()
    }

    /// The diagnostic severity.
    pub fn severity(&self) -> Severity {
        self.error.severity()
    }

    /// The labelled byte span, when the error carries one.
    pub fn span(&self) -> Option<SourceSpan> {
        self.error.span()
    }

    /// Builds a miette report that includes the annotated source snippet.
    pub fn to_report(self) -> miette::Report {
        miette::Report::new(self)
    }
}

impl Diagnostic for RstDiagnostic {
    fn code<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        Some(Box::new(self.error.code()))
    }

    fn severity(&self) -> Option<miette::Severity> {
        Some(miette::Severity::from(self.error.severity()))
    }

    fn help<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        let help = self.error.help_text();
        (!help.is_empty()).then(|| Box::new(help) as Box<dyn std::fmt::Display + 'a>)
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        self.error.span().map(|span| {
            let label = match &self.error {
                RstError::Indentation { .. } => "unexpected indentation",
                RstError::Adornment { .. } => "adornment",
                RstError::Table { .. } => "table markup",
                RstError::UnterminatedExplicitMarkup { .. } => "explicit markup",
                RstError::MissingBody { .. } => "introducer",
                RstError::Note { .. } => "here",
                RstError::Ast(_) => "here",
            };
            Box::new(std::iter::once(LabeledSpan::new_with_span(
                Some(label.to_owned()),
                span,
            ))) as Box<dyn Iterator<Item = LabeledSpan> + '_>
        })
    }

    fn source_code(&self) -> Option<&dyn SourceCode> {
        // `NamedSource` is stored inline via a leak-free approach: miette
        // requires a `&dyn SourceCode`, and `&str` already implements it. We
        // return the source string; the name is surfaced through `Diagnostic`
        // code/severity and the parse output's `source_name`.
        Some(&self.source)
    }
}

/// Builds a report with the document source attached.
///
/// This is a convenience for callers that want miette to render the annotated
/// source snippet. The document name is stored on the diagnostic and shown as
/// the snippet's source label.
pub fn named_report(
    source_name: Option<String>,
    source: String,
    error: RstError,
) -> miette::Report {
    miette::Report::new(RstDiagnostic::new(error, source_name, source))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> RstError {
        RstError::Indentation {
            message: "expected 2 spaces".into(),
            span: SourceSpan::new(4usize.into(), 3),
        }
    }

    #[test]
    fn error_carries_stable_code_help_and_span() {
        let err = sample();
        assert_eq!(err.code(), "scribe::rst::indentation");
        assert!(err.help_text().contains("check indentation"));
        assert_eq!(err.span().map(|s| s.offset()), Some(4));
        assert_eq!(err.severity(), Severity::Error);
    }

    #[test]
    fn diagnostic_renders_with_source() {
        let diag = sample().into_diagnostic(Some("a.rst".into()), "aaa bbb\n".into());
        assert_eq!(diag.code(), "scribe::rst::indentation");
        assert!(diag.labels().is_some());
        assert!(diag.source_code().is_some());
        let rendered = format!("{:?}", diag.to_report());
        assert!(rendered.contains("inconsistent indentation"), "{rendered}");
    }
}
