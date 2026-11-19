//! Errors raised by theme discovery and template rendering.
//!
//! Every error carries a stable [`miette`] diagnostic code and, where a file
//! was involved, the theme or template path so a builder can point the user at
//! the offending file.  Library code never panics; malformed theme files,
//! missing templates and inheritance cycles are all reported here.

use std::path::PathBuf;

use miette::{Diagnostic, NamedSource, SourceSpan};
use thiserror::Error;

/// Result alias used across `scribe-theme`.
pub type ThemeResult<T> = Result<T, ThemeError>;

/// A failure in the theme / templating layer.
#[derive(Clone, Debug, Error, Diagnostic)]
pub enum ThemeError {
    /// A theme with the requested name is not registered.
    #[error("no theme named '{name}' found (missing theme.toml or theme.conf?)")]
    #[diagnostic(
        code(scribe::theme::unknown_theme),
        help("known themes: {known}; add a theme directory with html_theme_path"),
        severity(Error)
    )]
    UnknownTheme {
        /// Requested name.
        name: String,
        /// Sorted names of the registered themes.
        known: String,
    },

    /// The theme directory has no `theme.toml` or `theme.conf`.
    #[error("no theme configuration file found in {path}")]
    #[diagnostic(code(scribe::theme::missing_config), severity(Error))]
    MissingConfig {
        /// Theme directory that was inspected.
        path: PathBuf,
    },

    /// The `[theme]` table / section is absent or malformed.
    #[error("invalid theme metadata in {path}: {detail}")]
    #[diagnostic(code(scribe::theme::invalid_metadata), severity(Error))]
    InvalidMetadata {
        /// Configuration file path.
        path: PathBuf,
        /// Explanation.
        detail: String,
    },

    /// A TOML file could not be parsed.
    #[error("invalid TOML in {path}: {detail}")]
    #[diagnostic(code(scribe::theme::invalid_toml), severity(Error))]
    InvalidToml {
        /// Configuration file path.
        path: PathBuf,
        /// serde detail.
        detail: String,
        /// Source text for the diagnostic label.
        #[source_code]
        text: std::sync::Arc<NamedSource<String>>,
        /// Byte range of the parse error, when reported by the parser.
        #[label("invalid TOML here")]
        span: Option<SourceSpan>,
    },

    /// A theme inherits from itself, directly or transitively.
    #[error("theme '{name}' has circular inheritance (cycle: {cycle})")]
    #[diagnostic(
        code(scribe::theme::inheritance_cycle),
        help("break the cycle by setting `inherit = \"none\"` on the root theme"),
        severity(Error)
    )]
    InheritanceCycle {
        /// Theme that closed the cycle.
        name: String,
        /// Human-readable `a -> b -> a` chain.
        cycle: String,
    },

    /// A theme chain is longer than upstream's 10-ancestor limit.
    #[error("theme '{name}' has too many ancestors (limit is {limit})")]
    #[diagnostic(code(scribe::theme::too_many_ancestors), severity(Error))]
    TooManyAncestors {
        /// Theme whose chain was too long.
        name: String,
        /// Upstream limit (10).
        limit: usize,
    },

    /// A parent theme named in `inherit` was not found.
    #[error("theme '{name}' inherits from '{parent}', which is not a loaded theme (loaded: {known})")]
    #[diagnostic(code(scribe::theme::missing_parent), severity(Error))]
    MissingParent {
        /// Child theme name.
        name: String,
        /// Missing parent name.
        parent: String,
        /// Sorted known themes.
        known: String,
    },

    /// A template could not be found in any search directory.
    #[error("template '{name}' not found (searched: {searched})")]
    #[diagnostic(code(scribe::theme::template_not_found), severity(Error))]
    TemplateNotFound {
        /// Template name as requested.
        name: String,
        /// Comma-separated search paths.
        searched: String,
    },

    /// minijinja reported a compile error (bad syntax, unknown filter, ...).
    #[error("template error in '{name}': {detail}")]
    #[diagnostic(code(scribe::theme::template_syntax), severity(Error))]
    TemplateSyntax {
        /// Template name.
        name: String,
        /// minijinja detail.
        detail: String,
    },

    /// minijinja reported a render-time error.
    #[error("failed to render template '{name}': {detail}")]
    #[diagnostic(code(scribe::theme::render), severity(Error))]
    Render {
        /// Template name.
        name: String,
        /// minijinja detail.
        detail: String,
    },

    /// An I/O error while reading a theme file or directory.
    #[error("I/O error for {path}: {detail}")]
    #[diagnostic(code(scribe::theme::io), severity(Error))]
    Io {
        /// Path involved.
        path: PathBuf,
        /// Underlying error message (kept as text so the error stays `Clone`).
        detail: String,
    },
}

impl ThemeError {
    /// Stable diagnostic code for this error.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnknownTheme { .. } => "scribe::theme::unknown_theme",
            Self::MissingConfig { .. } => "scribe::theme::missing_config",
            Self::InvalidMetadata { .. } => "scribe::theme::invalid_metadata",
            Self::InvalidToml { .. } => "scribe::theme::invalid_toml",
            Self::InheritanceCycle { .. } => "scribe::theme::inheritance_cycle",
            Self::TooManyAncestors { .. } => "scribe::theme::too_many_ancestors",
            Self::MissingParent { .. } => "scribe::theme::missing_parent",
            Self::TemplateNotFound { .. } => "scribe::theme::template_not_found",
            Self::TemplateSyntax { .. } => "scribe::theme::template_syntax",
            Self::Render { .. } => "scribe::theme::render",
            Self::Io { .. } => "scribe::theme::io",
        }
    }
}

/// Convert a minijinja error into a [`ThemeError`].
pub(crate) fn convert_minijinja(name: &str, error: minijinja::Error) -> ThemeError {
    let detail = format!("{error}");
    if matches!(error.kind(), minijinja::ErrorKind::TemplateNotFound) {
        ThemeError::TemplateNotFound {
            name: name.to_owned(),
            searched: String::new(),
        }
    } else {
        ThemeError::TemplateSyntax {
            name: name.to_owned(),
            detail,
        }
    }
}
