//! LaTeX writer and PDF builder (Phase 23).
//!
//! This module turns a frozen [`scribe_ast::Document`] into Sphinx-compatible,
//! compilable LaTeX and orchestrates a complete `latex` / `pdf` build. It is a
//! parallel writer to the Phase 17 HTML writer and consumes the same frozen
//! doctree, transform pipeline and domain engine.
//!
//! Public surface:
//!
//! * [`escape::escape_latex`] / [`escape::LatexEngine`] — TeX escaping rules.
//! * [`translator::LatexContext`] — per-document rendering data.
//! * [`LaTeXTranslator::translate`] — render a complete `.tex` document.
//! * [`BuildOptions`] / [`BuildResult`] — clap-independent builder I/O.
//! * [`LatexBuilder::build`] — run a full LaTeX build.

pub mod builder;
pub mod escape;
pub mod support;
pub mod translator;

pub use builder::LatexBuilder;
pub use crate::error::LatexError;
pub use escape::{escape_id, escape_latex, escape_latex_default, escape_latex_unicode, LatexEngine};
pub use translator::{LatexContext, LaTeXTranslator};

use std::path::PathBuf;

/// Stable identity of the LaTeX builder.
pub const BUILDER_NAME: &str = "latex";
/// Stable identity of the PDF builder (LaTeX + compilation).
pub const PDF_BUILDER_NAME: &str = "pdf";

/// Clap-independent options for a LaTeX/PDF builder run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildOptions {
    /// Source directory.
    pub source_dir: PathBuf,
    /// Output directory.
    pub output_dir: PathBuf,
    /// Builder name (`latex` or `pdf`).
    pub builder_name: String,
    /// `-a` / `--write-all`: write every output file.
    pub force_all: bool,
    /// `-E` / `--fresh-env`: discard the saved environment.
    pub freshenv: bool,
    /// `-d` / `--doctree-dir`: explicit doctree cache directory.
    pub doctree_dir: Option<PathBuf>,
    /// Specific files to (re)build.
    pub filenames: Vec<PathBuf>,
    /// `-j` resolved worker count (accepted for CLI parity).
    pub worker_count: usize,
    /// Suppress all output.
    pub really_quiet: bool,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            source_dir: PathBuf::from("."),
            output_dir: PathBuf::from("_build/latex"),
            builder_name: BUILDER_NAME.to_owned(),
            force_all: false,
            freshenv: false,
            doctree_dir: None,
            filenames: Vec::new(),
            worker_count: 1,
            really_quiet: false,
        }
    }
}

/// Result of a LaTeX builder run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuildResult {
    /// Files written, relative to the output directory.
    pub written: Vec<PathBuf>,
    /// Docnames whose `.tex` files were written.
    pub built_docnames: Vec<String>,
    /// Non-fatal warnings gathered during the build.
    pub warnings: Vec<String>,
}

impl BuildResult {
    /// Number of files written.
    #[must_use]
    pub fn file_count(&self) -> usize { self.written.len() }
}

/// Result alias used by the LaTeX builder.
pub type Result<T> = std::result::Result<T, crate::error::LatexError>;
