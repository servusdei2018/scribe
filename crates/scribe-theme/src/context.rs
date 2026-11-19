//! Per-page rendering context supplied by the HTML writer (Phase 17).
//!
//! [`RenderContext`] deliberately carries only data: the template environment
//! turns it into a minijinja context and installs a hidden `__sphinx` object so
//! that Sphinx's global functions (`pathto`, `hasdoc`, ...) can read the same
//! data without capturing render-specific state in the shared environment.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use minijinja::value::{Object, ObjectRepr, Value};
use scribe_config::Config;

/// Immutable state shared by every template/global during one render.
#[derive(Debug)]
pub(crate) struct RenderStateData {
    pub all_docs: BTreeSet<String>,
    pub target_uris: BTreeMap<String, String>,
    pub link_suffix: String,
    pub baseuri: String,
    pub allow_sharp_as_current_path: bool,
    pub search_enabled: bool,
    pub use_index: bool,
    pub theme_options: BTreeMap<String, String>,
    pub translations: BTreeMap<String, String>,
    pub sidebar_html: String,
    pub relbar_html: String,
    pub toctree_html: String,
}

impl Object for RenderStateData {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Plain
    }
}

/// Everything needed to render one page of a Sphinx document.
///
/// The HTML translator (Phase 17) fills `body`, `toc`, `title` and the
/// navigation fragments, then calls
/// [`ThemeManager::render_template`](crate::ThemeManager::render_template).
#[derive(Clone, Debug)]
pub struct RenderContext {
    /// Builder name (`html`, `dirhtml`, `singlehtml`, ...).
    pub builder_name: String,
    /// Theme name used to resolve templates and options.
    pub theme_name: String,
    /// Suffix-free document name of the page being rendered.
    pub docname: String,
    /// Resolved scribe configuration.
    pub config: Config,
    /// All known document names (upstream `env.all_docs`).
    pub all_docs: BTreeSet<String>,
    /// Explicit docname → target URI overrides.
    pub target_uris: BTreeMap<String, String>,
    /// Merged theme options; mirrored as `theme_*` template variables.
    pub theme_options: BTreeMap<String, String>,
    /// Output file suffix (normally `.html`).
    pub file_suffix: String,
    /// Link suffix used by `pathto` (normally `.html`).
    pub link_suffix: String,
    /// Document language tag.
    pub language: String,
    /// Message-id → translation overrides for `_()` / `gettext`.
    pub translations: BTreeMap<String, String>,
    /// Rendered HTML body (treated as safe markup, not escaped).
    pub body: String,
    /// Rendered local table of contents (safe markup).
    pub toc: String,
    /// Page title (may contain inline markup).
    pub title: String,
    /// Extra `<meta>` tags (safe markup).
    pub metatags: String,
    /// Pre-rendered sidebar fragment returned by `sidebar()` (safe markup).
    pub sidebar_html: String,
    /// Pre-rendered related-links fragment returned by `relbar()` (safe markup).
    pub relbar_html: String,
    /// Pre-rendered toctree fragment returned by `toctree()` (safe markup).
    pub toctree_html: String,
    /// Stylesheet entries (strings or `{filename, attributes}` maps).
    pub css_files: Vec<Value>,
    /// Script entries (strings or `{filename, body, attributes}` maps).
    pub script_files: Vec<Value>,
    /// Related links: `(docname, title, accesskey, label)`.
    pub rellinks: Vec<(String, String, String, String)>,
    /// Breadcrumb parents: `(link, title)`.
    pub parents: Vec<(String, String)>,
    /// Previous document: `(link, title)`.
    pub prev: Option<(String, String)>,
    /// Next document: `(link, title)`.
    pub next: Option<(String, String)>,
    /// Whether the search page exists.
    pub search_enabled: bool,
    /// Whether the generated index page exists.
    pub use_index: bool,
    /// `pathto` may return `#` for the current page (upstream default).
    pub allow_sharp_as_current_path: bool,
    /// Additional per-page template variables (highest precedence).
    pub vars: BTreeMap<String, Value>,
}

impl RenderContext {
    /// Build a minimal context for `docname` using default configuration.
    #[must_use]
    pub fn new(docname: impl Into<String>) -> Self {
        Self::for_builder("html", docname, Config::new())
    }

    /// Build a context for a named builder with an explicit configuration.
    #[must_use]
    pub fn for_builder(builder_name: impl Into<String>, docname: impl Into<String>, config: Config) -> Self {
        let theme_name = config
            .get_str("html_theme")
            .unwrap_or("alabaster")
            .to_owned();
        Self {
            builder_name: builder_name.into(),
            theme_name,
            docname: docname.into(),
            config,
            all_docs: BTreeSet::new(),
            target_uris: BTreeMap::new(),
            theme_options: BTreeMap::new(),
            file_suffix: ".html".to_owned(),
            link_suffix: ".html".to_owned(),
            language: "en".to_owned(),
            translations: BTreeMap::new(),
            body: String::new(),
            toc: String::new(),
            title: String::new(),
            metatags: String::new(),
            sidebar_html: String::new(),
            relbar_html: String::new(),
            toctree_html: String::new(),
            css_files: Vec::new(),
            script_files: Vec::new(),
            rellinks: Vec::new(),
            parents: Vec::new(),
            prev: None,
            next: None,
            search_enabled: true,
            use_index: true,
            allow_sharp_as_current_path: true,
            vars: BTreeMap::new(),
        }
    }

    /// Select the theme whose templates should be rendered.
    pub fn with_theme(mut self, name: impl Into<String>) -> Self {
        self.theme_name = name.into();
        self
    }

    /// Replace the theme options map.
    pub fn with_theme_options(mut self, options: BTreeMap<String, String>) -> Self {
        self.theme_options = options;
        self
    }

    /// Add a known document.
    pub fn add_doc(mut self, docname: impl Into<String>) -> Self {
        self.all_docs.insert(docname.into());
        self
    }

    /// Set a single extra template variable.
    pub fn set_var(&mut self, name: impl Into<String>, value: impl Into<Value>) -> &mut Self {
        self.vars.insert(name.into(), value.into());
        self
    }

    /// Construct the hidden render-state object used by the global functions.
    pub(crate) fn render_state(&self) -> RenderStateData {
        let baseuri = self.target_uri(&self.docname);
        RenderStateData {
            all_docs: self.all_docs.clone(),
            target_uris: self.target_uris.clone(),
            link_suffix: self.link_suffix.clone(),
            baseuri,
            allow_sharp_as_current_path: self.allow_sharp_as_current_path,
            search_enabled: self.search_enabled,
            use_index: self.use_index,
            theme_options: self.theme_options.clone(),
            translations: self.translations.clone(),
            sidebar_html: self.sidebar_html.clone(),
            relbar_html: self.relbar_html.clone(),
            toctree_html: self.toctree_html.clone(),
        }
    }

    /// Sphinx's `get_target_uri`: an explicit override, else a quoted docname
    /// plus the link suffix.
    #[must_use]
    pub fn target_uri(&self, docname: &str) -> String {
        self.target_uris
            .get(docname)
            .cloned()
            .unwrap_or_else(|| format!("{}{}", urlquote(docname), self.link_suffix))
    }

    /// Build a stylesheet object understood by `css_tag`.
    #[must_use]
    pub fn css_file(filename: impl Into<String>) -> Value {
        Value::from(BTreeMap::from([
            ("filename".to_owned(), Value::from(filename.into())),
            ("priority".to_owned(), Value::from(500_i64)),
        ]))
    }

    /// Build a script object understood by `js_tag`.
    #[must_use]
    pub fn js_file(filename: impl Into<String>) -> Value {
        Value::from(BTreeMap::from([
            ("filename".to_owned(), Value::from(filename.into())),
            ("priority".to_owned(), Value::from(500_i64)),
        ]))
    }
}

/// `urllib.parse.quote(value, safe='/')` for the ASCII-relevant subset.
#[must_use]
pub fn urlquote(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}
