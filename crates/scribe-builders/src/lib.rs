//! Output builders and syntax highlighting for scribe (Phase 15).
//!
//! This crate owns two related responsibilities:
//!
//! * **Syntax highlighting** (this phase): a [`syntect`]-backed highlighter that
//!   emits a deterministic, writer-agnostic token stream whose CSS class names
//!   are Pygments-compatible ([`highlight`]). A matching `pygments.css`
//!   stylesheet generator lives in [`pygments_css`].
//! * **Code directives** (this phase): upgraded `code` / `code-block` /
//!   `sourcecode` / `literalinclude` handlers plus the `highlight` directive
//!   ([`code`]). [`register_code_directives`] installs them into a
//!   [`scribe_rst::DirectiveRegistry`], replacing the Phase 06 `LiteralBlock`
//!   placeholders.
//!
//! The HTML/document writers themselves are **Phase 17**. They consume the
//! [`highlight::HighlightedCode`] structure stored on a `LiteralBlock` node's
//! `__highlight_json` attribute (see `ARCHITECTURE.md` §19 for the rendering
//! contract). This phase deliberately emits no HTML writer code.
//!
//! # Quick start
//!
//! ```
//! use scribe_builders::{highlight_code, HighlightOptions};
//!
//! let out = highlight_code("def f():\n    pass\n", "python", &HighlightOptions::default());
//! assert_eq!(out.language, "python");
//! assert_eq!(out.lines.len(), 2);
//! assert!(out.lines[0].tokens.iter().any(|t| t.classes.iter().any(|c| c == "k")));
//! ```

pub mod build_common;
pub mod changes;
pub mod code;
pub mod dirhtml;
pub mod doctest;
pub mod epub;
pub mod error;
pub mod highlight;
pub mod html;
pub mod i18n;
pub mod latex;
pub mod linkcheck;
pub mod man;
pub mod metadata;
pub mod profile;
pub mod pygments_css;
pub mod search;
pub mod singlehtml;
pub mod texinfo;
pub mod text;
pub mod xml;

pub use changes::{
    collect_changesets, collect_changesets_with_sources, render_changes_html, render_index_html,
    render_rst_source, ChangesBuilder,
    ChangesContext, ChangeSet, Entry,
};
pub use code::{register_code_directives, CodeDirectiveInfo, HighlightSetting};
pub use linkcheck::{
    collect_hyperlinks, contains_anchor, is_ignored, prefix_match, retry_after_delay, unquote,
    write_report as write_linkcheck_report,
    CheckResult, Hyperlink, LinkcheckBuilder, LinkcheckConfig, LinkcheckReport, LinkStatus,
    LocalLinkIndex,
};
pub use xml::{render_xml, PseudoXmlBuilder, XmlBuilder, XmlRenderError, DOCTYPE, XMLNS_DOMAINS};
pub use dirhtml::{output_path as dirhtml_output_path, target_uri as dirhtml_target_uri, DirectoryHTMLBuilder};
pub use doctest::{
    register_config_keys as register_doctest_config_keys, register_directives as register_doctest_directives,
    DoctestBuilder, DoctestError, DoctestResult,
};
pub use error::{HighlightError, HtmlError, LatexError, OutputError, OutputResult};
pub use epub::{
    escape_xml, render_nav_xhtml, render_toc_ncx, strip_external_refs, wrap_xhtml, EpubBuilder,
    EpubMetadata, NavPoint, EPUB_CSS, MIMETYPE,
};
pub use man::{parse_man_pages, ManBuilder, ManHeader};
pub use texinfo::{parse_texinfo_documents, render as render_texinfo, TexinfoBuilder, TexinfoHeader};
pub use text::{render as render_text, render_with_numbers, TextBuilder, TextOptions, ToctreeContext};
pub use latex::{escape_latex, LatexBuilder, LatexContext, LaTeXTranslator, LatexEngine};
pub use metadata::{DevHelpBuilder, HtmlHelpBuilder, JsonBuilder, QtHelpBuilder};
pub use singlehtml::{target_uri as singlehtml_target_uri, SingleFileHTMLBuilder};
pub use highlight::{
    alias_info, highlight_code, highlight_many, parse_line_num_spec, resolve_language,
    HighlightLine, HighlightOptions, HighlightToken, HighlightedCode, LanguageResolution,
};
pub use i18n::{
    catalog_paths, compact_setting, compile_all, docname_to_domain, load_catalog, locale_dirs,
    read_mo, write_mo, GettextBuilder, GettextResult, Message, MoError, PoCatalog, PotContext,
};
pub use pygments_css::{available_styles, pygments_css};
pub use search::{
    algorithm_for, is_filtered, parse_js_index, scribe_envversion, search_language_stemming_code,
    search_language_stop_words_json, stem_word, stopwords_for, tokenize, SearchIndex,
    BASE_STEMMER_JS, ENGLISH_STEMMER_JS, ENGLISH_STOPWORDS, IDENTITY_STEMMER_JS, JS_INDEX_PREFIX,
    JS_INDEX_SUFFIX,
};

use scribe_rst::DirectiveRegistry;

/// The phase range that introduced this crate's highlighting functionality.
/// The remaining builder modules (HTML, LaTeX, text, ...) arrive in Phases 17-25.
pub const IMPLEMENTATION_PHASE: &str = "Phase 15 (highlighting); Phases 17-25 (writers)";

/// Registers the Phase 15 code directives into `registry`, replacing the
/// Phase 06 `LiteralBlock` placeholders with token-producing handlers.
///
/// This is a convenience alias for [`code::register_code_directives`].
pub fn register_highlighting(registry: &mut DirectiveRegistry) {
    code::register_code_directives(registry);
}
