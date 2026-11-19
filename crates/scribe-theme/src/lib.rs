//! Theme system and Jinja2-compatible templating for scribe (Phase 16).
//!
//! This crate provides the rendering context and template API consumed by the
//! Phase 17 HTML writer.  It vendors the `basic`, `classic`, `nature` and
//! `alabaster` themes, resolves their inheritance using Sphinx's algorithm,
//! and exposes a sandboxed minijinja environment with Sphinx's globals,
//! filters and autoescaping rules.
//!
//! ```no_run
//! use scribe_theme::{RenderContext, ThemeManager};
//!
//! let manager = ThemeManager::new();
//! let basic = manager.get_theme("basic")?;
//! assert_eq!(basic.name, "basic");
//!
//! let ctx = RenderContext::new("index").with_theme("basic");
//! let html = manager.render_template("page.html", &ctx)?;
//! assert!(html.contains("<!DOCTYPE html>"));
//! # Ok::<(), scribe_theme::ThemeError>(())
//! ```

mod bundled;
mod context;
mod environment;
mod error;
mod globals;
mod theme;

pub use bundled::{BUNDLED_THEME_NAMES, BUNDLED_THEME_ROOT, bundled_theme_dir};
pub use context::{RenderContext, urlquote};
pub use environment::{TemplateEnvironment, preprocess_template, preprocess_trans};
pub use error::{ThemeError, ThemeResult};
pub use globals::{
    accesskey, css_tag, hasdoc, html_escape_attr, idgen, ngettext, pathto,
    relative_uri, relbar, sidebar, slice_index_filter, striptags_filter,
    to_bool_filter, to_dim_filter, to_int_filter, toctree, translation,
};
pub use theme::{Theme, ThemeManager, ThemeManager as HTMLThemeFactory, theme_static_files, to_bool};

/// Version string reported to templates as `sphinx_version`.
pub const SPHINX_COMPAT_VERSION: &str = "9.3.0";
