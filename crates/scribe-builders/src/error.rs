//! Errors raised by the scribe output layer.
//!
//! The highlighter itself is infallible (unknown languages degrade to plain
//! text with a warning, matching Sphinx). Errors here cover the few operations
//! that can genuinely fail: decoding a `HighlightedCode` back out of an AST
//! attribute and reading configuration that is present but malformed, plus the
//! I/O and template failures of the Phase 17 HTML builder.

use std::path::PathBuf;

use miette::{Diagnostic, SourceSpan};
use scribe_ast::AstError;
use thiserror::Error;

/// A failure in the Phase 17 HTML builder.
#[derive(Debug, Error, Diagnostic)]
pub enum HtmlError {
    /// An I/O operation on a build path failed.
    #[error("I/O error at {}: {source}", .path.display())]
    #[diagnostic(code(scribe::builders::html::io), severity(Error))]
    Io {
        /// Path involved in the failure.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// A template/theme operation failed.
    #[error(transparent)]
    #[diagnostic(code(scribe::builders::html::theme), severity(Error))]
    Theme(#[from] scribe_theme::ThemeError),

    /// A build-environment operation failed.
    #[error(transparent)]
    #[diagnostic(code(scribe::builders::html::environment), severity(Error))]
    Environment(#[from] scribe_core::ScribeError),

    /// A required source file was missing.
    #[error("missing source file: {}", .0.display())]
    #[diagnostic(code(scribe::builders::html::missing_source), severity(Error))]
    MissingSource(PathBuf),

    /// A transform or configuration value was invalid.
    #[error("{0}")]
    #[diagnostic(code(scribe::builders::html::invalid), severity(Error))]
    Invalid(String),
}

impl HtmlError {
    /// Construct an I/O error with a path label.
    pub fn io(path: &std::path::Path, source: std::io::Error) -> Self {
        Self::Io { path: path.to_path_buf(), source }
    }

    /// Construct a theme error.
    pub fn theme(source: scribe_theme::ThemeError) -> Self {
        Self::Theme(source)
    }
}

/// Result alias used throughout the HTML builder.
pub type Result<T> = std::result::Result<T, HtmlError>;

/// A failure in the Phase 23 LaTeX writer / PDF builder.
#[derive(Debug, Error, Diagnostic)]
pub enum LatexError {
    /// An I/O operation on a build path failed.
    #[error("I/O error at {}: {source}", .path.display())]
    #[diagnostic(code(scribe::builders::latex::io), severity(Error))]
    Io {
        /// Path involved in the failure.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// A required source directory was missing.
    #[error("missing source file: {}", .0.display())]
    #[diagnostic(code(scribe::builders::latex::missing_source), severity(Error))]
    MissingSource(PathBuf),

    /// A build-environment operation failed.
    #[error(transparent)]
    #[diagnostic(code(scribe::builders::latex::environment), severity(Error))]
    Environment(#[from] scribe_core::ScribeError),

    /// A required external tool could not be launched.
    #[error("toolchain `{tool}` is unavailable: {source}")]
    #[diagnostic(
        code(scribe::builders::latex::toolchain),
        help("install latexmk and a LaTeX distribution to enable PDF output"),
        severity(Error)
    )]
    ToolchainUnavailable {
        /// Name of the missing tool.
        tool: String,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// LaTeX compilation failed.
    #[error("LaTeX compilation failed for {}: {}", .path.display(), .detail)]
    #[diagnostic(code(scribe::builders::latex::compilation), severity(Error))]
    Compilation {
        /// The `.tex` file that could not be compiled.
        path: PathBuf,
        /// Tail of the compiler log.
        detail: String,
    },

    /// An invalid value was supplied.
    #[error("{0}")]
    #[diagnostic(code(scribe::builders::latex::invalid), severity(Error))]
    Invalid(String),
}

impl LatexError {
    /// Construct an I/O error with a path label.
    pub fn io(path: &std::path::Path, source: std::io::Error) -> Self {
        Self::Io { path: path.to_path_buf(), source }
    }

    /// Construct a toolchain error.
    pub fn toolchain(tool: &str, source: std::io::Error) -> Self {
        Self::ToolchainUnavailable { tool: tool.to_owned(), source }
    }
}

/// Result alias for the LaTeX writer / builder.
pub type LatexResult<T> = std::result::Result<T, LatexError>;

/// A failure in the highlighting output layer.
#[derive(Clone, Debug, Error, Diagnostic)]
pub enum HighlightError {
    /// The `__highlight_json` attribute of a node could not be decoded.
    #[error("invalid highlighted-code payload on node: {detail}")]
    #[diagnostic(
        code(scribe::builders::highlight_payload),
        help("the node was produced by a different scribe version"),
        severity(Error)
    )]
    InvalidPayload {
        /// serde detail.
        detail: String,
        /// Byte span of the source construct, when known.
        #[label("invalid highlight payload here")]
        span: SourceSpan,
    },

    /// A configuration value had an unexpected type.
    #[error("invalid highlighting configuration for '{name}': {detail}")]
    #[diagnostic(code(scribe::builders::highlight_config), severity(Error))]
    InvalidConfig {
        /// Configuration key.
        name: String,
        /// Why the value was rejected.
        detail: String,
    },

    /// An arena operation failed while attaching highlighted output.
    #[error(transparent)]
    #[diagnostic(code(scribe::builders::highlight_ast))]
    Ast(#[from] AstError),
}

impl HighlightError {
    /// Stable diagnostic code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidPayload { .. } => "scribe::builders::highlight_payload",
            Self::InvalidConfig { .. } => "scribe::builders::highlight_config",
            Self::Ast(_) => "scribe::builders::highlight_ast",
        }
    }
}

/// Result alias for the highlighting layer.
pub type HighlightResult<T> = std::result::Result<T, HighlightError>;

/// A failure in one of the Phase 24 output builders (`epub`, `man`,
/// `texinfo`, `text`).
///
/// These builders share one error vocabulary because their failure modes are
/// identical: a bad build path, a stale environment, a malformed config value
/// or (for EPUB) a ZIP/XML serialization problem. The stable diagnostic code
/// distinguishes the builder via the `builder` field so errors remain
/// actionable without four near-identical enums.
#[derive(Debug, Error, Diagnostic)]
pub enum OutputError {
    /// An I/O operation on a build path failed.
    #[error("[{builder}] I/O error at {}: {source}", .path.display())]
    #[diagnostic(code(scribe::builders::output::io), severity(Error))]
    Io {
        /// Builder that produced the error.
        builder: &'static str,
        /// Path involved in the failure.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// The source directory did not exist.
    #[error("[{0}] missing source directory: {1:?}")]
    #[diagnostic(code(scribe::builders::output::missing_source), severity(Error))]
    MissingSource(&'static str, PathBuf),

    /// A build-environment operation failed.
    #[error("[{builder}] environment error: {source}")]
    #[diagnostic(code(scribe::builders::output::environment), severity(Error))]
    Environment {
        /// Builder that produced the error.
        builder: &'static str,
        /// Underlying environment error.
        #[source]
        source: scribe_core::ScribeError,
    },

    /// A configuration value was present but malformed.
    #[error("[{builder}] invalid configuration for '{name}': {detail}")]
    #[diagnostic(code(scribe::builders::output::invalid_config), severity(Error))]
    InvalidConfig {
        /// Builder that produced the error.
        builder: &'static str,
        /// Configuration key.
        name: String,
        /// Why the value was rejected.
        detail: String,
    },

    /// EPUB ZIP serialization failed.
    #[error("[epub] ZIP error: {0}")]
    #[diagnostic(code(scribe::builders::output::zip), severity(Error))]
    Zip(#[from] zip::result::ZipError),

    /// An XML value could not be serialized (EPUB metadata/navigation).
    #[error("[epub] XML error: {0}")]
    #[diagnostic(code(scribe::builders::output::xml), severity(Error))]
    Xml(String),
}

impl OutputError {
    /// Construct an I/O error tagged with the builder name.
    pub fn io(builder: &'static str, path: &std::path::Path, source: std::io::Error) -> Self {
        Self::Io { builder, path: path.to_path_buf(), source }
    }

    /// Construct an environment error tagged with the builder name.
    pub fn env(builder: &'static str, source: scribe_core::ScribeError) -> Self {
        Self::Environment { builder, source }
    }

    /// Construct an invalid-config error tagged with the builder name.
    pub fn config(builder: &'static str, name: &str, detail: impl Into<String>) -> Self {
        Self::InvalidConfig { builder, name: name.to_owned(), detail: detail.into() }
    }
}

/// Result alias for the Phase 24 output builders.
pub type OutputResult<T> = std::result::Result<T, OutputError>;
