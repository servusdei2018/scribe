//! Error and diagnostic types for the native autodoc engine (Phase 21).
//!
//! Library code returns [`AutodocResult`] and never panics. Diagnostics use
//! `miette` source spans so malformed Python is reported with a precise
//! location instead of aborting the build.

use miette::{Diagnostic, SourceSpan};
use thiserror::Error;

/// Errors raised by the autodoc engine.
#[derive(Clone, Debug, Error, Diagnostic)]
pub enum AutodocError {
    /// A module could not be resolved to a Python source file.
    #[error("autodoc: could not find source for module `{module}`")]
    ModuleNotFound {
        /// Fully-qualified module name.
        module: String,
    },

    /// A named object could not be found in the parsed module tree.
    #[error("autodoc: `{name}` is not defined in module `{module}`")]
    ObjectNotFound {
        /// Module name.
        module: String,
        /// Object path that was requested.
        name: String,
    },

    /// Reading a source file failed.
    #[error("autodoc: could not read `{path}`: {detail}")]
    Io {
        /// File path.
        path: String,
        /// I/O detail.
        detail: String,
    },

    /// Python source could not be parsed; the static engine degrades to a
    /// warning and an empty module rather than failing the build.
    #[error("autodoc: failed to parse `{path}`: {detail}")]
    Parse {
        /// File path.
        path: String,
        /// 1-based line number when known.
        line: Option<u32>,
        /// Parser detail.
        detail: String,
        /// Byte span of the offending construct.
        #[label("here")]
        span: Option<SourceSpan>,
    },

    /// A directive option had an invalid value.
    #[error("autodoc: invalid option value for `{option}`: {detail}")]
    InvalidOption {
        /// Option name.
        option: String,
        /// Explanation.
        detail: String,
    },

    /// The optional dynamic introspection bridge failed.
    #[error("autodoc: dynamic introspection failed: {detail}")]
    Dynamic {
        /// Failure detail.
        detail: String,
    },

    /// An arena operation failed while materializing description nodes.
    #[error("autodoc: AST operation failed: {detail}")]
    Ast {
        /// Failure detail.
        detail: String,
    },

    /// A domain operation failed while expanding a signature.
    #[error("autodoc: domain operation failed: {detail}")]
    Domain {
        /// Failure detail.
        detail: String,
    },

    /// An autosummary stub template could not be found or rendered.
    #[error("autosummary: {detail}")]
    #[diagnostic(
        code(scribe::autosummary::template),
        help("check the template name and the templates_path directories"),
        severity(Error)
    )]
    AutosummaryTemplate {
        /// Template name / render detail.
        detail: String,
    },
}

impl AutodocError {
    /// Stable diagnostic code for this error.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::ModuleNotFound { .. } => "scribe::autodoc::module_not_found",
            Self::ObjectNotFound { .. } => "scribe::autodoc::object_not_found",
            Self::Io { .. } => "scribe::autodoc::io",
            Self::Parse { .. } => "scribe::autodoc::parse",
            Self::InvalidOption { .. } => "scribe::autodoc::invalid_option",
            Self::Dynamic { .. } => "scribe::autodoc::dynamic",
            Self::Ast { .. } => "scribe::autodoc::ast",
            Self::Domain { .. } => "scribe::autodoc::domain",
            Self::AutosummaryTemplate { .. } => "scribe::autosummary::template",
        }
    }
}

/// Convenience result alias.
pub type AutodocResult<T> = Result<T, AutodocError>;

impl From<scribe_ast::AstError> for AutodocError {
    fn from(error: scribe_ast::AstError) -> Self {
        Self::Ast { detail: error.to_string() }
    }
}

impl From<scribe_domains::DomainError> for AutodocError {
    fn from(error: scribe_domains::DomainError) -> Self {
        Self::Domain { detail: error.to_string() }
    }
}
