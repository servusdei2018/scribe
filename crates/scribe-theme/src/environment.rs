//! minijinja environment with Sphinx-compatible globals, filters and
//! autoescaping.
//!
//! The environment is sandboxed (minijinja exposes no attribute mutation,
//! imports or arbitrary Python objects) and is safe to cache: all per-page
//! data lives in the [`RenderContext`], not in the environment.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use minijinja::value::Value;
use minijinja::{AutoEscape, Environment, UndefinedBehavior};

use crate::context::{RenderContext, RenderStateData};
use crate::error::{convert_minijinja, ThemeError, ThemeResult};
use crate::globals::{self, RENDER_STATE_VAR};
use crate::theme::Theme;

/// A sandboxed Jinja2-compatible template environment configured for Sphinx.
#[derive(Debug)]
pub struct TemplateEnvironment {
    theme: Theme,
    search_paths: Vec<PathBuf>,
    env: Environment<'static>,
}

impl TemplateEnvironment {
    /// Build an environment that searches the theme's template and static
    /// directories (static dirs are needed for `.jinja`/`_t` stylesheets).
    pub fn new(theme: &Theme) -> ThemeResult<Self> {
        let mut paths = theme.template_dirs.clone();
        for dir in &theme.static_dirs {
            if !paths.contains(dir) {
                paths.push(dir.clone());
            }
        }
        Self::with_search_paths(theme, paths)
    }

    /// Build an environment with an explicit template search chain.
    pub fn with_search_paths(theme: &Theme, search_paths: Vec<PathBuf>) -> ThemeResult<Self> {
        let mut env = Environment::new();
        env.set_auto_escape_callback(auto_escape_callback);
        env.set_undefined_behavior(UndefinedBehavior::Chainable);
        env.set_formatter(markupsafe_formatter);
        let mut built = Self {
            theme: theme.clone(),
            search_paths,
            env,
        };
        built.register_sphinx_globals();
        built.install_loader();
        Ok(built)
    }

    /// Register Sphinx's globals and filters.  Idempotent.
    pub fn register_sphinx_globals(&mut self) {
        let env = &mut self.env;

        // Globals from jinja2glue.py.
        env.add_global("accesskey", Value::from_function(globals::accesskey));
        env.add_global("idgen", Value::from_function(globals::idgen));
        env.add_global("warning", Value::from_function(globals::warning));
        env.add_global("debug", Value::from_function(globals::debug));

        // Globals from the HTML builder's `handle_page` / `get_context`.
        env.add_global("pathto", Value::from_function(globals::pathto));
        env.add_global("hasdoc", Value::from_function(globals::hasdoc));
        env.add_global("css_tag", Value::from_function(globals::css_tag));
        env.add_global("js_tag", Value::from_function(globals::js_tag));
        env.add_global("sidebar", Value::from_function(globals::sidebar));
        env.add_global("relbar", Value::from_function(globals::relbar));
        env.add_global("toctree", Value::from_function(globals::toctree));
        env.add_global("get_theme_option", Value::from_function(globals::get_theme_option));

        // Translation helpers (the i18n extension installs `_` upstream).
        env.add_global("_", Value::from_function(globals::translation));
        env.add_global("gettext", Value::from_function(globals::translation));
        env.add_global("translation", Value::from_function(globals::translation));
        env.add_global("ngettext", Value::from_function(globals::ngettext));

        // Sphinx's scalar helpers are exposed both as filters and globals.
        env.add_filter("tobool", globals::to_bool_filter);
        env.add_filter("toint", globals::to_int_filter);
        env.add_filter("todim", globals::to_dim_filter);
        env.add_filter("slice_index", globals::slice_index_filter);
        env.add_filter("striptags", globals::striptags_filter);
        env.add_filter("e", globals::escape_filter);
        env.add_filter("escape", globals::escape_filter);
        env.add_global("tobool", Value::from_function(globals::to_bool_filter));
        env.add_global("toint", Value::from_function(globals::to_int_filter));
        env.add_global("todim", Value::from_function(globals::to_dim_filter));
        env.add_global("slice_index", Value::from_function(globals::slice_index_filter));
    }

    fn install_loader(&mut self) {
        let search_paths = self.search_paths.clone();
        self.env.set_loader(move |name| {
            let loaded = load_template(&search_paths, name)
                .map_err(|error| minijinja::Error::new(minijinja::ErrorKind::InvalidOperation, error.to_string()))?;
            if let Some(source) = loaded {
                return Ok(Some(preprocess_template(&source)));
            }
            Ok(None)
        });
    }

    /// Directories searched for templates, most specific first.
    #[must_use]
    pub fn search_paths(&self) -> &[PathBuf] {
        &self.search_paths
    }

    /// Theme whose templates this environment serves.
    #[must_use]
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Render a named template with the supplied page context.
    pub fn render(&self, name: &str, ctx: &RenderContext) -> ThemeResult<String> {
        let template = self
            .env
            .get_template(name)
            .map_err(|error| convert_minijinja(name, error))?;
        let context = self.build_context(ctx);
        template
            .render(context)
            .map_err(|error| ThemeError::Render {
                name: name.to_owned(),
                detail: error.to_string(),
            })
    }

    /// Render a source string (after trans preprocessing) with a page context.
    pub fn render_string(&self, name: &str, source: &str, ctx: &RenderContext) -> ThemeResult<String> {
        let processed = preprocess_template(source);
        let template = self
            .env
            .template_from_named_str(name, &processed)
            .map_err(|error| convert_minijinja(name, error))?;
        let context = self.build_context(ctx);
        template
            .render(context)
            .map_err(|error| ThemeError::Render {
                name: name.to_owned(),
                detail: error.to_string(),
            })
    }

    /// Build the minijinja context for one page.
    fn build_context(&self, ctx: &RenderContext) -> Value {
        let mut values = BTreeMap::<String, Value>::new();
        let config = &ctx.config;

        // `globalcontext` from the HTML builder.
        put(&mut values, "embedded", false);
        put(&mut values, "project", config.get_str("project").unwrap_or(""));
        put(&mut values, "release", config.get_str("release").unwrap_or(""));
        put(&mut values, "version", config.get_str("version").unwrap_or(""));
        put(&mut values, "copyright", config.get_str("copyright").unwrap_or(""));
        put(&mut values, "master_doc", config.master_doc());
        put(&mut values, "root_doc", config.master_doc());
        put(&mut values, "docstitle", config.get_str("html_title").unwrap_or(""));
        put(
            &mut values,
            "shorttitle",
            config.get_str("html_short_title").unwrap_or(""),
        );
        put(&mut values, "language", ctx.language.clone());
        put(
            &mut values,
            "show_copyright",
            config.get_bool("html_show_copyright").unwrap_or(true),
        );
        put(
            &mut values,
            "show_sphinx",
            config.get_bool("html_show_sphinx").unwrap_or(true),
        );
        put(
            &mut values,
            "show_search_summary",
            config.get_bool("html_show_search_summary").unwrap_or(true),
        );
        put(
            &mut values,
            "has_source",
            config.get_bool("html_copy_source").unwrap_or(true),
        );
        put(
            &mut values,
            "show_source",
            config.get_bool("html_show_sourcelink").unwrap_or(true),
        );
        put(
            &mut values,
            "sourcelink_suffix",
            config.get_str("html_sourcelink_suffix").unwrap_or(".txt"),
        );
        put(&mut values, "file_suffix", ctx.file_suffix.clone());
        put(&mut values, "link_suffix", ctx.link_suffix.clone());
        put(&mut values, "builder", ctx.builder_name.clone());
        put(&mut values, "sphinx_version", "9.3.0");
        put(&mut values, "html5_doctype", true);
        put(&mut values, "use_opensearch", false);
        put(
            &mut values,
            "pageurl",
            config
                .get_str("html_baseurl")
                .filter(|base| !base.is_empty())
                .map(|base| format!("{}/{}", base.trim_end_matches('/'), ctx.target_uri(&ctx.docname)))
                .unwrap_or_default(),
        );
        let logo = config.get_str("html_logo").unwrap_or("");
        let favicon = config.get_str("html_favicon").unwrap_or("");
        put(
            &mut values,
            "logo_url",
            if logo.is_empty() {
                String::new()
            } else {
                format!("_static/{logo}")
            },
        );
        put(
            &mut values,
            "favicon_url",
            if favicon.is_empty() {
                String::new()
            } else {
                format!("_static/{favicon}")
            },
        );
        put(&mut values, "logo_alt", format!("Logo of {}", config.get_str("project").unwrap_or("")));
        let baseuri = ctx.target_uri(&ctx.docname);
        let depth = baseuri.matches('/').count();
        put(
            &mut values,
            "content_root",
            if depth == 0 { "./".to_owned() } else { "../".repeat(depth) },
        );
        put(&mut values, "link", "#");

        // Theme options (both `theme_*` variables and get_theme_option).
        for (key, value) in &self.theme.options {
            put(&mut values, format!("theme_{key}"), value.clone());
        }
        for (key, value) in &ctx.theme_options {
            put(&mut values, format!("theme_{key}"), value.clone());
        }
        put(&mut values, "sidebars", self.theme.sidebars.clone());
        put(&mut values, "styles", self.theme.stylesheets.clone());

        // Per-page data. Safe strings are inserted verbatim by minijinja.
        put(&mut values, "pagename", ctx.docname.clone());
        put(&mut values, "current_page_name", ctx.docname.clone());
        put(&mut values, "encoding", "utf-8");
        put(&mut values, "title", Value::from_safe_string(ctx.title.clone()));
        put(&mut values, "body", Value::from_safe_string(ctx.body.clone()));
        put(&mut values, "toc", Value::from_safe_string(ctx.toc.clone()));
        put(&mut values, "metatags", Value::from_safe_string(ctx.metatags.clone()));
        put(
            &mut values,
            "css_files",
            if ctx.css_files.is_empty() {
                Value::from(
                    self.theme
                        .stylesheets
                        .iter()
                        .map(|name| Value::from(format!("_static/{name}")))
                        .collect::<Vec<_>>(),
                )
            } else {
                Value::from(ctx.css_files.clone())
            },
        );
        put(&mut values, "script_files", Value::from(ctx.script_files.clone()));
        put(
            &mut values,
            "rellinks",
            Value::from(
                ctx.rellinks
                    .iter()
                    .map(|(href, title, accesskey, label)| {
                        Value::from(vec![
                            Value::from(href.clone()),
                            Value::from(title.clone()),
                            Value::from(accesskey.clone()),
                            Value::from(label.clone()),
                        ])
                    })
                    .collect::<Vec<_>>(),
            ),
        );
        put(
            &mut values,
            "parents",
            Value::from(
                ctx.parents
                    .iter()
                    .map(|(link, title)| {
                        Value::from(BTreeMap::from([
                            ("link".to_owned(), Value::from(link.clone())),
                            ("title".to_owned(), Value::from_safe_string(title.clone())),
                        ]))
                    })
                    .collect::<Vec<_>>(),
            ),
        );
        put(&mut values, "prev", relation(ctx.prev.as_ref()));
        put(&mut values, "next", relation(ctx.next.as_ref()));
        put(&mut values, "display_toc", !ctx.toc.is_empty());
        put(&mut values, "sourcename", String::new());
        put(&mut values, "page_source_suffix", ctx.file_suffix.clone());

        // User `html_context` values come before explicit per-page vars.
        if let Some(dict) = config.get_dict("html_context") {
            for (key, value) in dict {
                put(&mut values, key.clone(), value.to_string());
            }
        }
        for (key, value) in &ctx.vars {
            values.insert(key.clone(), value.clone());
        }

        // Hidden render state consumed by the global functions.
        let state: RenderStateData = ctx.render_state();
        values.insert(RENDER_STATE_VAR.to_owned(), Value::from_object(state));

        Value::from(values)
    }
}

fn put(map: &mut BTreeMap<String, Value>, key: impl Into<String>, value: impl Into<Value>) {
    map.insert(key.into(), value.into());
}

fn relation(value: Option<&(String, String)>) -> Value {
    match value {
        Some((link, title)) => Value::from(BTreeMap::from([
            ("link".to_owned(), Value::from(link.clone())),
            // Sphinx exposes relation titles as Markup so the section-number
            // `<span>` emitted by `get_relation()` survives template escaping.
            ("title".to_owned(), Value::from_safe_string(title.clone())),
        ])),
        None => Value::from(()),
    }
}

/// Autoescape formatter matching Jinja2/MarkupSafe: unlike minijinja's default
/// HTML escaper it does **not** escape `/` (Sphinx templates embed URLs in
/// attributes), but it keeps `&`, `<`, `>`, `"` and `'` escaping.
fn markupsafe_formatter(
    out: &mut minijinja::Output<'_>,
    state: &minijinja::State<'_, '_>,
    value: &Value,
) -> Result<(), minijinja::Error> {
    if matches!(state.auto_escape(), AutoEscape::Html) && !value.is_safe() {
        if let Some(text) = value.as_str() {
            return out
                .write_str(&globals::html_escape_markupsafe(text))
                .map_err(minijinja::Error::from);
        }
    }
    minijinja::escape_formatter(out, state, value)
}

fn auto_escape_callback(name: &str) -> AutoEscape {
    let mut name = name;
    for extension in [".jinja", ".jinja2", ".j2"] {
        if let Some(stripped) = name.strip_suffix(extension) {
            name = stripped;
            break;
        }
    }
    match name.rsplit('.').next() {
        Some("html" | "htm" | "xml") => AutoEscape::Html,
        _ => AutoEscape::None,
    }
}

/// Load a template from the search chain, including Sphinx's legacy `_t`
/// suffix mapping for `.jinja` templates (`alabaster.css_t`).
fn load_template(search_paths: &[PathBuf], name: &str) -> ThemeResult<Option<String>> {
    for dir in search_paths {
        if let Some(path) = join_template(dir, name) {
            if path.is_file() {
                let source = fs::read_to_string(&path).map_err(|source| ThemeError::Io {
                    path,
                    detail: source.to_string(),
                })?;
                return Ok(Some(source));
            }
        }
        if let Some(legacy) = name.strip_suffix(".jinja") {
            let legacy_name = format!("{legacy}_t");
            if let Some(path) = join_template(dir, &legacy_name) {
                if path.is_file() {
                    let source = fs::read_to_string(&path).map_err(|source| ThemeError::Io {
                        path,
                        detail: source.to_string(),
                    })?;
                    return Ok(Some(source));
                }
            }
        }
    }
    Ok(None)
}

fn join_template(dir: &Path, name: &str) -> Option<PathBuf> {
    let mut path = dir.to_path_buf();
    for segment in name.split('/') {
        if segment.is_empty() || segment.starts_with('.') || segment.contains('\\') {
            return None;
        }
        path.push(segment);
    }
    Some(path)
}

/// Apply all Jinja2 → minijinja source adaptations used by Sphinx themes.
#[must_use]
pub fn preprocess_template(source: &str) -> String {
    preprocess_trans(&strip_macro_blocks(source))
}

/// Remove `{% block %}` tags that appear *inside* macros.
///
/// Jinja2 permits nested blocks in macros (Sphinx's `basic/layout.html` uses
/// them for `rootrellink`, `relbaritems`, `sidebarlogo`, ...); minijinja rejects
/// them at compile time.  The blocks are unwrapped to their content, which is
/// behaviorally identical for themes that do not override those inner blocks.
fn strip_macro_blocks(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    let mut in_macro = false;
    while let Some(start) = rest.find("{%") {
        out.push_str(&rest[..start]);
        let tag_source = &rest[start..];
        let Some(end) = tag_source.find("%}") else {
            out.push_str(tag_source);
            return out;
        };
        let tag = &tag_source[..end + 2];
        let inner = tag[2..tag.len() - 2].trim().trim_start_matches('-').trim();
        let keyword = inner.split_whitespace().next().unwrap_or("");
        let skip = in_macro && matches!(keyword, "block" | "endblock");
        if !skip {
            out.push_str(tag);
        }
        match keyword {
            "macro" => in_macro = true,
            "endmacro" => in_macro = false,
            _ => {}
        }
        rest = &tag_source[end + 2..];
    }
    out.push_str(rest);
    out
}

/// Rewrite Jinja2 i18n `{% trans %}` blocks into minijinja-compatible markup.
///
/// Sphinx ships templates that use the `jinja2.ext.i18n` extension, which
/// minijinja does not implement.  English output is byte-identical for the
/// subset used by the bundled themes: assignments become `{% set %}` in a
/// one-iteration loop scope and the message body is emitted inline.  The
/// `trimmed` option collapses per-line whitespace the way Jinja2 does.
#[must_use]
pub fn preprocess_trans(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(start) = rest.find("{% trans") {
        let after = &rest[start + "{% trans".len()..];
        if !after.starts_with(char::is_whitespace) && !after.starts_with('%') {
            out.push_str(&rest[..start + 2]);
            rest = &rest[start + 2..];
            continue;
        }
        let Some(open_end) = rest[start..].find("%}") else { break };
        let open_end = start + open_end + 2;
        let header = rest[start + "{% trans".len()..open_end - 2].trim();
        let Some(close_rel) = rest[open_end..].find("{% endtrans") else { break };
        let close_start = open_end + close_rel;
        let Some(close_end_rel) = rest[close_start..].find("%}") else { break };
        let close_end = close_start + close_end_rel + 2;

        out.push_str(&rest[..start]);
        let mut header_tokens: Vec<&str> = header.split(',').map(str::trim).collect();
        let trimmed = header_tokens.first().is_some_and(|token| *token == "trimmed");
        if trimmed {
            header_tokens.remove(0);
        }
        out.push_str("{% for __sphinx_trans in [0] %}");
        for token in header_tokens {
            if token.is_empty() {
                continue;
            }
            if let Some((name, expr)) = token.split_once('=') {
                let name = name.trim();
                if !name.is_empty() && name.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
                    out.push_str(&format!("{{% set {name} = {} %}}", expr.trim()));
                }
            }
        }
        let body = &rest[open_end..close_start];
        if trimmed {
            let lines: Vec<&str> = body
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .collect();
            out.push_str(&lines.join(" "));
        } else {
            out.push_str(body);
        }
        out.push_str("{% endfor %}");
        rest = &rest[close_end..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autoescape_matches_jinja_for_sphinx_suffixes() {
        assert!(matches!(auto_escape_callback("page.html"), AutoEscape::Html));
        assert!(matches!(auto_escape_callback("basic.css.jinja"), AutoEscape::None));
        assert!(matches!(auto_escape_callback("doctools.js.jinja"), AutoEscape::None));
        assert!(matches!(auto_escape_callback("alabaster.css_t"), AutoEscape::None));
    }

    #[test]
    fn trans_preprocessing_preserves_basic_messages() {
        let source = "{% trans docstitle=docstitle|e %}Search within {{ docstitle }}{% endtrans %}";
        let processed = preprocess_trans(source);
        assert!(processed.contains("{% set docstitle = docstitle|e %}"));
        assert!(processed.contains("Search within {{ docstitle }}"));
        assert!(processed.contains("{% endfor %}"));
    }

    #[test]
    fn trans_preprocessing_handles_trimed_blocks() {
        let source = "{% trans trimmed copyright_prefix=copyright_prefix, copyright=copyright|e %}\n  &#169; {{ copyright_prefix }} {{ copyright }}.\n{% endtrans %}";
        let processed = preprocess_trans(source);
        assert!(processed.contains("&#169; {{ copyright_prefix }} {{ copyright }}."));
        assert!(!processed.contains("{% trans"));
    }

    #[test]
    fn theme_option_strings_are_plain_and_safe() {
        use crate::to_bool;
        let theme = crate::ThemeManager::new().get_theme("basic").unwrap();
        assert!(!to_bool(theme.option("nosidebar").unwrap_or("false")));
    }
}
