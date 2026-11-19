//! Jinja2-compatible renderer for autosummary stub pages.
//!
//! Upstream `sphinx/ext/autosummary/generate.py::AutosummaryRenderer` builds a
//! sandboxed Jinja2 environment whose `escape`/`e` filters are
//! `sphinx.util.rst.escape` (not HTML escaping) and whose `underline` filter
//! appends a line of `=` of the title's character length. This module mirrors
//! that contract with `minijinja`, reusing Phase 16's `{% trans %}` source
//! preprocessor for user-supplied templates and vendoring the three system
//! templates verbatim (see `templates.rs` / `NOTICE`).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use minijinja::value::Value;
use minijinja::{Environment, Error as MiniError};

use crate::error::{AutodocError, AutodocResult};

use super::templates::{BASE_TEMPLATE, CLASS_TEMPLATE, MODULE_TEMPLATE};

/// Escapes a string for safe use inside reStructuredText.
///
/// Transcribed from `sphinx/util/rst.py`: `symbols_re = ([!-\-/:-@\[-`{-~])`
/// (every ASCII punctuation character except `.`) plus a leading-dot special
/// case. Non-ASCII characters are left untouched.
#[must_use]
pub fn escape_rst(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 4);
    let mut leading_dot = true;
    for ch in text.chars() {
        let code = ch as u32;
        let punctuation = (0x21..=0x2d).contains(&code) // ! through -
            || ch == '/' // upstream's `!-\-/` class includes '/' but not '.'
            || (0x3a..=0x40).contains(&code) // : through @
            || (0x5b..=0x60).contains(&code) // [ through `
            || (0x7b..=0x7e).contains(&code); // { through ~
        if punctuation || (leading_dot && ch == '.') {
            out.push('\\');
        }
        out.push(ch);
        leading_dot = false;
    }
    out
}

fn rst_escape_filter(value: &Value) -> Result<String, MiniError> {
    Ok(escape_rst(&value.to_string()))
}

fn underline_filter(value: &Value) -> Result<String, MiniError> {
    let title = value.to_string();
    if title.contains('\n') {
        return Err(MiniError::new(
            minijinja::ErrorKind::InvalidOperation,
            "Can only underline single lines",
        ));
    }
    let line = "=".repeat(title.chars().count());
    Ok(format!("{title}\n{line}"))
}

fn translation(value: String) -> String {
    value
}

/// A sandboxed minijinja environment configured like upstream's
/// `AutosummaryRenderer`. It is cheap to construct once per build and safe to
/// reuse across stub pages (all per-page data lives in the render context).
pub struct StubRenderer {
    env: Environment<'static>,
}

impl std::fmt::Debug for StubRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StubRenderer").finish_non_exhaustive()
    }
}

impl Default for StubRenderer {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl StubRenderer {
    /// Creates a renderer. `search_dirs` are checked in order for user
    /// templates; the bundled system templates are always available as a
    /// fallback (and under their `autosummary/<objtype>.rst` names).
    #[must_use]
    pub fn new(search_dirs: Vec<PathBuf>) -> Self {
        let dirs: Arc<Vec<PathBuf>> = Arc::new(search_dirs);
        let mut env = Environment::new();
        env.set_undefined_behavior(minijinja::UndefinedBehavior::Chainable);
        env.add_filter("escape", rst_escape_filter);
        env.add_filter("e", rst_escape_filter);
        env.add_filter("underline", underline_filter);
        env.add_global("_", Value::from_function(translation));
        env.add_global("gettext", Value::from_function(translation));
        env.set_loader(move |name| Ok(load_template(&dirs, name)));
        Self { env }
    }

    /// Renders one stub. `template_name` mirrors the `:template:` option:
    /// upstream tries it verbatim, then `autosummary/<name>.rst`, then falls
    /// back to the objtype-specific template (finally `base.rst`).
    pub fn render(
        &self,
        template_name: Option<&str>,
        objtype: &str,
        vars: &BTreeMap<String, Value>,
    ) -> AutodocResult<String> {
        let mut candidates: Vec<String> = Vec::new();
        if let Some(name) = template_name.filter(|name| !name.trim().is_empty()) {
            let name = name.trim();
            candidates.push(name.to_owned());
            if !name.starts_with("autosummary/") {
                candidates.push(format!("autosummary/{name}.rst"));
            }
        }
        candidates.push(format!("autosummary/{objtype}.rst"));
        candidates.push("autosummary/base.rst".to_owned());

        let template = candidates
            .iter()
            .find_map(|name| self.env.get_template(name).ok().map(|template| (name.clone(), template)));
        let (name, template) = template.ok_or_else(|| AutodocError::AutosummaryTemplate {
            detail: format!("no usable template for object type {objtype:?}"),
        })?;
        let ctx = Value::from(vars.clone());
        template.render(ctx).map_err(|error| AutodocError::AutosummaryTemplate {
            detail: format!("{name}: {error}"),
        })
    }
}

fn load_template(search_dirs: &[PathBuf], name: &str) -> Option<String> {
    if let Some(source) = bundled(name) {
        return Some(source.to_owned());
    }
    let relative = Path::new(name);
    if relative.is_absolute()
        || relative
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::Prefix(_)))
    {
        return None;
    }
    for dir in search_dirs {
        let path = dir.join(relative);
        if let Ok(source) = fs::read_to_string(&path) {
            // Reuse Phase 16's Jinja2 i18n source adaptation so user templates
            // with `{% trans %}` blocks render identically under minijinja.
            return Some(scribe_theme::preprocess_template(&source));
        }
        if let Some(stripped) = name.strip_suffix(".rst") {
            let legacy = dir.join(format!("{stripped}.rst.jinja"));
            if let Ok(source) = fs::read_to_string(legacy) {
                return Some(scribe_theme::preprocess_template(&source));
            }
        }
    }
    None
}

fn bundled(name: &str) -> Option<&'static str> {
    match name {
        "autosummary/base.rst" | "base.rst" => Some(BASE_TEMPLATE),
        "autosummary/module.rst" | "module.rst" => Some(MODULE_TEMPLATE),
        "autosummary/class.rst" | "class.rst" => Some(CLASS_TEMPLATE),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_matches_upstream_punctuation_set() {
        assert_eq!(escape_rst("foo_bar"), "foo\\_bar");
        assert_eq!(escape_rst(".leading"), "\\.leading");
        assert_eq!(escape_rst("a.b"), "a.b");
        assert_eq!(escape_rst("a/b"), "a\\/b");
        assert_eq!(escape_rst("a-b + c"), "a\\-b \\+ c");
        assert_eq!(escape_rst("café ☃"), "café ☃");
    }

    #[test]
    fn bundled_templates_render_with_upstream_filters() {
        let renderer = StubRenderer::default();
        let mut vars = BTreeMap::new();
        vars.insert("fullname".to_owned(), Value::from("pkg.mod"));
        vars.insert("module".to_owned(), Value::from("pkg"));
        vars.insert("objname".to_owned(), Value::from("mod"));
        vars.insert("name".to_owned(), Value::from("mod"));
        vars.insert("objtype".to_owned(), Value::from("module"));
        let rendered = renderer.render(None, "module", &vars).unwrap();
        assert!(rendered.starts_with("pkg.mod\n=======\n\n.. automodule:: pkg.mod"));
        assert!(rendered.contains(".. automodule:: pkg.mod"));
    }
}
