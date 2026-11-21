//! HTML5 writer and standalone HTML builder (Phase 17).
//!
//! This module turns a frozen [`scribe_ast::Document`] into Sphinx-faithful
//! HTML5 (`translator`) and orchestrates a complete `html` build (`builder`).
//! It consumes the Phase 15 highlight payload, the Phase 10 transform pipeline,
//! the Phase 14 domain/xref engine and the Phase 16 theme/template system.
//!
//! The public surface is intentionally small:
//!
//! * [`HtmlContext`] — everything the translator needs to resolve links and
//!   resources for one page.
//! * [`HTML5Translator::translate`] — render a document body.
//! * [`BuildOptions`] / [`BuildResult`] — clap-independent builder inputs/outputs.
//! * [`StandaloneHtmlBuilder::build`] — run a full build into an output dir.

pub mod assets;
pub mod builder;
pub mod index;
pub mod translator;

pub use assets::{copy_theme_assets, write_buildinfo, write_source_dumps};
pub use builder::StandaloneHtmlBuilder;
pub use index::{generate_genindex, generate_modindex, generate_search_page};
pub use translator::{escape_html, HtmlContext, HTML5Translator};

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use scribe_config::Config;
use scribe_core::{BuildEnvironment, InteropBridge};

/// Re-exported alias mirroring upstream's `StandaloneHTMLBuilder` class name.
pub type StandaloneHTMLBuilder = StandaloneHtmlBuilder;

/// Stable identity of the builder implemented here.
pub const BUILDER_NAME: &str = "html";

/// Options for a builder run.
///
/// This is the clap-independent view of `scribe_cli::BuildOptions`: the CLI
/// converts its parsed options into this struct before calling
/// [`StandaloneHtmlBuilder::build`]. Keeping it in `scribe-builders` avoids a
/// dependency cycle (`scribe-cli` depends on this crate for wiring).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuilderOptions {
    /// Source directory.
    pub source_dir: PathBuf,
    /// Output directory.
    pub output_dir: PathBuf,
    /// Builder name (normally `html`).
    pub builder_name: String,
    /// `-a` / `--write-all`: write every output file.
    pub force_all: bool,
    /// `-E` / `--fresh-env`: discard the saved environment.
    pub freshenv: bool,
    /// `-d` / `--doctree-dir`: explicit doctree cache directory.
    pub doctree_dir: Option<PathBuf>,
    /// Specific files to (re)build.
    pub filenames: Vec<PathBuf>,
    /// `-j` resolved worker count.
    pub worker_count: usize,
    /// Suppress all output.
    pub really_quiet: bool,
    /// Optional third-party interop bridge (event bus + Python/Wasm plugins).
    /// `None` keeps the pre-Phase-27 behaviour byte-for-byte.
    pub bridge: Option<std::sync::Arc<InteropBridge>>,
    /// Phase 67: reuse the caller's live `BuildEnvironment` without reloading
    /// it from the on-disk cache. Used by `scribe serve` for fast rebuilds.
    pub reuse_environment: bool,
}

impl Default for BuilderOptions {
    fn default() -> Self {
        Self {
            source_dir: PathBuf::from("."),
            output_dir: PathBuf::from("_build/html"),
            builder_name: BUILDER_NAME.to_owned(),
            force_all: false,
            freshenv: false,
            doctree_dir: None,
            filenames: Vec::new(),
            worker_count: 1,
            really_quiet: false,
            bridge: None,
            reuse_environment: false,
        }
    }
}

/// Build options (short alias used by [`StandaloneHtmlBuilder::build`]).
pub type BuildOptions = BuilderOptions;

/// Result of a successful builder run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuilderResult {
    /// All files written, relative to the output directory.
    pub written: Vec<PathBuf>,
    /// Docnames whose pages were (re)written.
    pub built_docnames: Vec<String>,
    /// Docnames removed from the source tree this build (Phase 67).
    pub removed_docnames: Vec<String>,
    /// Non-fatal warnings gathered during the build.
    pub warnings: Vec<String>,
}

impl BuilderResult {
    /// Number of files written.
    #[must_use]
    pub fn file_count(&self) -> usize {
        self.written.len()
    }
}

/// Build result (short alias used by the public API).
pub type BuildResult = BuilderResult;

/// Construct a minimal `HtmlContext` for unit tests and direct translator use.
#[must_use]
pub fn test_context(docname: &str, config: &Config, env: &BuildEnvironment) -> HtmlContext {
    let mut target_uris = BTreeMap::new();
    for name in env.found_docnames() {
        target_uris.insert(name.to_owned(), format!("{name}.html"));
    }
    target_uris.entry(docname.to_owned()).or_insert_with(|| format!("{docname}.html"));
    let all_docs = env.found_docnames().iter().map(|s| (*s).to_owned()).collect();
    HtmlContext {
        docname: docname.to_owned(),
        config: Arc::new(config.clone()),
        env: Arc::new(env.clone()),
        titles: Arc::new(BTreeMap::new()),
        target_uris,
        all_docs,
        add_permalinks: true,
        permalinks_icon: "¶".to_owned(),
        image_uris: BTreeMap::new(),
        language: config.get_str("language").unwrap_or("en").to_owned(),
        singlefile: None,
        graphviz: BTreeMap::new(),
    }
}
