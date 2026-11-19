//! Structured errors and user-facing diagnostics for the MyST parser.
//!
//! Two layers are provided, mirroring `scribe-rst`:
//!
//! * [`MystError`] is the `thiserror` error type returned by fallible internal
//!   helpers. It implements [`miette::Diagnostic`] and carries a byte span.
//! * [`MystDiagnostic`] binds an error to the source text and name so miette can
//!   render the 1-based line/column and an annotated snippet.

use miette::{Diagnostic, LabeledSpan, SourceCode, SourceSpan};
use scribe_ast::AstError;
use thiserror::Error;

/// Severity of a recoverable MyST parse diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// A fatal problem that prevented parsing a construct (recovery attempted).
    Error,
    /// A problem where the parser guessed at the author's intent.
    Warning,
    /// A purely informational note.
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

/// Internal MyST parse error, convertible into a rendered [`MystDiagnostic`].
#[derive(Clone, Debug, Error, Diagnostic)]
pub enum MystError {
    /// YAML/TOML frontmatter could not be parsed.
    #[error("invalid frontmatter: {message}")]
    #[diagnostic(
        code(scribe::myst::frontmatter),
        help("frontmatter must be a YAML mapping delimited by --- lines"),
        severity(Error)
    )]
    Frontmatter {
        /// Human-readable detail.
        message: String,
        /// Byte range of the frontmatter block.
        #[label("frontmatter here")]
        span: SourceSpan,
    },

    /// A directive fence was opened but never closed.
    #[error("unclosed directive fence: {message}")]
    #[diagnostic(
        code(scribe::myst::unclosed_directive),
        help("close the directive with a matching ``` or ::: fence"),
        severity(Error)
    )]
    UnclosedDirective {
        /// Human-readable detail (directive name).
        message: String,
        /// Byte range covering the opening fence.
        #[label("fence starts here")]
        span: SourceSpan,
    },

    /// An inline role was not terminated with a closing backtick.
    #[error("unclosed role: {message}")]
    #[diagnostic(
        code(scribe::myst::unclosed_role),
        help("close the role payload with a backtick, e.g. {{role}}`payload`"),
        severity(Error)
    )]
    UnclosedRole {
        /// Human-readable detail.
        message: String,
        /// Byte range of the offending text.
        #[label("role starts here")]
        span: SourceSpan,
    },

    /// A directive's option line was malformed.
    #[error("malformed directive option: {message}")]
    #[diagnostic(
        code(scribe::myst::directive_option),
        help("option lines look like ':name: value' at the start of the directive body"),
        severity(Warning)
    )]
    DirectiveOption {
        /// Human-readable detail.
        message: String,
        /// Byte range of the option line.
        #[label("option line here")]
        span: SourceSpan,
    },

    /// A non-fatal note about otherwise well-formed input.
    #[error("{message}")]
    #[diagnostic(code(scribe::myst::note), severity(Advice))]
    Note {
        /// Human-readable detail.
        message: String,
        /// Byte range of the note.
        #[label("here")]
        span: SourceSpan,
    },

    /// An arena operation failed (internal invariant violation, never bad input).
    #[error("AST construction failed: {0}")]
    #[diagnostic(code(scribe::myst::ast))]
    Ast(#[from] AstError),
}

impl MystError {
    /// Stable diagnostic code of this error.
    pub fn code(&self) -> &'static str {
        match self {
            MystError::Frontmatter { .. } => "scribe::myst::frontmatter",
            MystError::UnclosedDirective { .. } => "scribe::myst::unclosed_directive",
            MystError::UnclosedRole { .. } => "scribe::myst::unclosed_role",
            MystError::DirectiveOption { .. } => "scribe::myst::directive_option",
            MystError::Note { .. } => "scribe::myst::note",
            MystError::Ast(_) => "scribe::myst::ast",
        }
    }

    /// Severity used when this error becomes a [`MystDiagnostic`].
    pub fn severity(&self) -> Severity {
        match self {
            MystError::Frontmatter { .. }
            | MystError::UnclosedDirective { .. }
            | MystError::UnclosedRole { .. } => Severity::Error,
            MystError::DirectiveOption { .. } => Severity::Warning,
            MystError::Note { .. } | MystError::Ast(_) => Severity::Info,
        }
    }

    fn help_text(&self) -> &'static str {
        match self {
            MystError::Frontmatter { .. } => {
                "frontmatter must be a YAML mapping delimited by --- lines"
            }
            MystError::UnclosedDirective { .. } => {
                "close the directive with a matching ``` or ::: fence"
            }
            MystError::UnclosedRole { .. } => "close the role payload with a backtick",
            MystError::DirectiveOption { .. } => "option lines look like ':name: value'",
            MystError::Note { .. } | MystError::Ast(_) => "",
        }
    }

    fn label(&self) -> &'static str {
        match self {
            MystError::Frontmatter { .. } => "frontmatter",
            MystError::UnclosedDirective { .. } => "fence",
            MystError::UnclosedRole { .. } => "role",
            MystError::DirectiveOption { .. } => "option",
            MystError::Note { .. } | MystError::Ast(_) => "here",
        }
    }

    /// Byte span of the error, or `None` for AST errors without source info.
    pub fn span(&self) -> Option<SourceSpan> {
        match self {
            MystError::Frontmatter { span, .. }
            | MystError::UnclosedDirective { span, .. }
            | MystError::UnclosedRole { span, .. }
            | MystError::DirectiveOption { span, .. }
            | MystError::Note { span, .. } => Some(*span),
            MystError::Ast(_) => None,
        }
    }

    /// Wraps this error in a full, renderable [`MystDiagnostic`].
    pub fn into_diagnostic(self, source_name: Option<String>, source: String) -> MystDiagnostic {
        MystDiagnostic::new(self, source_name, source)
    }
}

/// A user-facing diagnostic: [`MystError`] bound to its source text.
#[derive(Clone, Debug, Error)]
#[error("{error}")]
pub struct MystDiagnostic {
    source: miette::NamedSource<String>,
    #[source]
    error: MystError,
}

impl MystDiagnostic {
    /// Creates a diagnostic from an error and the source it refers to.
    pub fn new(error: MystError, source_name: Option<String>, source: String) -> Self {
        let name = source_name.unwrap_or_else(|| "<myst>".to_owned());
        Self {
            source: miette::NamedSource::new(name, source),
            error,
        }
    }

    /// Borrows the underlying error.
    pub fn error(&self) -> &MystError {
        &self.error
    }

    /// Stable diagnostic code.
    pub fn code(&self) -> &'static str {
        self.error.code()
    }

    /// Diagnostic severity.
    pub fn severity(&self) -> Severity {
        self.error.severity()
    }

    /// Labelled byte span, when the error carries one.
    pub fn span(&self) -> Option<SourceSpan> {
        self.error.span()
    }

    /// Builds a miette report that includes the annotated source snippet.
    pub fn to_report(self) -> miette::Report {
        miette::Report::new(self)
    }
}

impl Diagnostic for MystDiagnostic {
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
            Box::new(std::iter::once(LabeledSpan::new_with_span(
                Some(self.error.label().to_owned()),
                span,
            ))) as Box<dyn Iterator<Item = LabeledSpan> + '_>
        })
    }

    fn source_code(&self) -> Option<&dyn SourceCode> {
        Some(&self.source)
    }
}

/// Builds a miette report with the document source attached.
pub fn named_report(
    source_name: Option<String>,
    source: String,
    error: MystError,
) -> miette::Report {
    miette::Report::new(MystDiagnostic::new(error, source_name, source))
}

/// Result alias for fallible MyST operations.
pub type MystResult<T> = Result<T, MystError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_carries_code_help_and_span() {
        let err = MystError::Frontmatter {
            message: "bad yaml".into(),
            span: SourceSpan::new(0usize.into(), 4),
        };
        assert_eq!(err.code(), "scribe::myst::frontmatter");
        assert_eq!(err.severity(), Severity::Error);
        assert_eq!(err.span().map(|s| s.offset()), Some(0));
    }

    #[test]
    fn diagnostic_renders_with_source() {
        let diag = MystError::UnclosedRole {
            message: "ref".into(),
            span: SourceSpan::new(0usize.into(), 5),
        }
        .into_diagnostic(Some("a.md".into()), "{ref}foo\n".into());
        assert_eq!(diag.code(), "scribe::myst::unclosed_role");
        assert!(diag.labels().is_some());
        let rendered = format!("{:?}", diag.to_report());
        assert!(rendered.contains("unclosed role"), "{rendered}");
    }
}
