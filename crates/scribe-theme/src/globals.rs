//! Sphinx's Jinja2 globals, filters and object helpers, implemented for
//! minijinja.
//!
//! Upstream sources inspected: `sphinx/jinja2glue.py` (`_tobool`, `_toint`,
//! `_todim`, `_slice_index`, `accesskey`, `idgen`, `warning`) and
//! `sphinx/builders/html/__init__.py::handle_page` (`pathto`, `hasdoc`,
//! `css_tag`, `js_tag`).  Global functions read the hidden `__sphinx` object
//! installed by [`TemplateEnvironment`](crate::TemplateEnvironment) so a
//! single compiled environment can serve many pages.

use std::collections::HashSet;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use minijinja::value::{Object, ObjectRepr, Value};
use minijinja::{Error, ErrorKind, State};

use crate::context::RenderStateData;
use crate::theme::to_bool;

/// Name of the hidden context variable carrying per-render data.
pub(crate) const RENDER_STATE_VAR: &str = "__sphinx";

fn render_state(state: &State<'_, '_>) -> Option<Arc<RenderStateData>> {
    state
        .lookup(RENDER_STATE_VAR)
        .and_then(|value| value.downcast_object::<RenderStateData>())
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidOperation, message.into())
}

/// Sphinx's `relative_uri(base, to)` (`sphinx/util/osutil.py`).
#[must_use]
pub fn relative_uri(base: &str, to: &str) -> String {
    let sep = '/';
    if to.starts_with(sep) {
        return to.to_owned();
    }
    let strip_fragment = |value: &str| value.split('#').next().unwrap_or("").to_owned();
    let base = strip_fragment(base);
    let to = strip_fragment(to);
    let mut b2: Vec<&str> = base.split(sep).collect();
    let mut t2: Vec<&str> = to.split(sep).collect();
    for _ in 0..b2.len().min(t2.len()).saturating_sub(1) {
        if b2.first() != t2.first() {
            break;
        }
        b2.remove(0);
        t2.remove(0);
    }
    if b2 == t2 {
        return String::new();
    }
    if b2.len() == 1 && t2 == [""] {
        return "./".to_owned();
    }
    format!("{}{}", "../".repeat(b2.len().saturating_sub(1)), t2.join("/"))
}

/// HTML attribute escaping matching `html.escape(value, quote=True)`.
#[must_use]
pub fn html_escape_attr(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            other => out.push(other),
        }
    }
    out
}

/// MarkupSafe-compatible HTML escaping (does not escape `/`).
#[must_use]
pub fn html_escape_markupsafe(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&#34;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

fn value_truth(value: &Value) -> bool {
    match value.as_str() {
        Some(text) => to_bool(text),
        None => value.is_true(),
    }
}

fn map_get(value: &Value, key: &str) -> Option<Value> {
    value
        .get_item(&Value::from(key))
        .ok()
        .filter(|item| !item.is_undefined() && !item.is_none())
}

/// `pathto(otheruri, resource=False, baseuri=...)`.
pub fn pathto(
    state: &State<'_, '_>,
    otheruri: String,
    resource: Option<Value>,
    baseuri: Option<String>,
) -> Result<String, Error> {
    let is_resource = resource.as_ref().is_some_and(value_truth);
    let data = render_state(state);
    let otheruri = if is_resource {
        if otheruri.contains("://") {
            return Ok(otheruri);
        }
        otheruri
    } else {
        let suffix = data.as_ref().map_or(".html", |d| d.link_suffix.as_str());
        let explicit = data
            .as_ref()
            .and_then(|d| d.target_uris.get(&otheruri))
            .cloned();
        explicit.unwrap_or_else(|| format!("{}{}", crate::context::urlquote(&otheruri), suffix))
    };
    let base = baseuri.or_else(|| data.as_ref().map(|d| d.baseuri.clone())).unwrap_or_default();
    let mut uri = relative_uri(&base, &otheruri);
    if uri.is_empty() {
        uri = "#".to_owned();
    }
    let allow_sharp = data.as_ref().is_none_or(|d| d.allow_sharp_as_current_path);
    if uri == "#" && !allow_sharp {
        uri = base;
    }
    Ok(uri)
}

/// `hasdoc(name)`.
pub fn hasdoc(state: &State<'_, '_>, name: String) -> bool {
    let Some(data) = render_state(state) else {
        return false;
    };
    if data.all_docs.contains(&name) {
        return true;
    }
    if name == "search" {
        return data.search_enabled;
    }
    name == "genindex" && data.use_index
}

/// `css_tag(css)` — emits a `<link rel="stylesheet" .../>` element.
pub fn css_tag(state: &State<'_, '_>, css: Value) -> Result<Value, Error> {
    let filename = match map_get(&css, "filename").and_then(|v| v.as_str().map(str::to_owned)) {
        Some(filename) => filename,
        None => return Ok(Value::from_safe_string(String::new())),
    };
    let uri = pathto(state, filename, Some(Value::from(true)), None)?;
    let mut attrs: Vec<(String, String)> = Vec::new();
    if let Some(attributes) = map_get(&css, "attributes") {
        for key in attributes
            .try_iter()
            .map_err(|error| invalid(error.to_string()))?
        {
            let Some(key_str) = key.as_str() else { continue };
            if let Some(value) = map_get(&attributes, key_str) {
                let rendered = state.format(value).unwrap_or_default();
                attrs.push((key_str.to_owned(), rendered));
            }
        }
    }
    for key in ["type", "media", "rel", "id", "class"] {
        if let Some(value) = map_get(&css, key) {
            let rendered = state.format(value).unwrap_or_default();
            attrs.push((key.to_owned(), rendered));
        }
    }
    attrs.sort_by(|a, b| a.0.cmp(&b.0));
    attrs.dedup_by(|a, b| a.0 == b.0);
    let attr_text = attrs
        .iter()
        .map(|(key, value)| format!("{}=\"{}\"", key, html_escape_attr(value)))
        .collect::<Vec<_>>()
        .join(" ");
    let html = if attr_text.is_empty() {
        format!("<link href=\"{uri}\" />")
    } else {
        format!("<link {attr_text} href=\"{uri}\" />")
    };
    Ok(Value::from_safe_string(html))
}

/// `js_tag(js)` — emits a `<script>` element for a filename or inline body.
pub fn js_tag(state: &State<'_, '_>, js: Value) -> Result<Value, Error> {
    if let Some(filename) = js.as_str() {
        let uri = pathto(state, filename.to_owned(), Some(Value::from(true)), None)?;
        return Ok(Value::from_safe_string(format!("<script src=\"{uri}\"></script>")));
    }
    let filename = map_get(&js, "filename").and_then(|v| v.as_str().map(str::to_owned));
    let body = map_get(&js, "body")
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    let mut attrs: Vec<(String, String)> = Vec::new();
    for key in ["type", "async", "defer", "charset", "id", "class"] {
        if let Some(value) = map_get(&js, key) {
            let rendered = state.format(value).unwrap_or_default();
            attrs.push((key.to_owned(), rendered));
        }
    }
    attrs.sort_by(|a, b| a.0.cmp(&b.0));
    let attr_text = attrs
        .iter()
        .map(|(key, value)| format!("{}=\"{}\"", key, html_escape_attr(value)))
        .collect::<Vec<_>>()
        .join(" ");
    let attr_prefix = if attr_text.is_empty() {
        String::new()
    } else {
        format!(" {attr_text}")
    };
    let html = match filename {
        None => format!("<script{attr_prefix}>{body}</script>"),
        Some(filename) => {
            let uri = pathto(state, filename, Some(Value::from(true)), None)?;
            format!("<script{attr_prefix} src=\"{uri}\"></script>")
        }
    };
    Ok(Value::from_safe_string(html))
}

/// Sphinx's `_tobool` filter.
pub fn to_bool_filter(value: &Value) -> bool {
    match value.as_str() {
        Some(text) => to_bool(text),
        None => value.is_true(),
    }
}

/// Sphinx's `_toint` filter (`int(val)` with `ValueError -> 0`).
pub fn to_int_filter(value: &Value) -> i64 {
    if let Some(number) = value.as_i64() {
        return number;
    }
    let text = value.to_string();
    text.trim().parse::<i64>().unwrap_or(0)
}

/// Sphinx's `_todim` filter.
pub fn to_dim_filter(value: &Value) -> Result<String, Error> {
    if value.is_none() || value.is_undefined() {
        return Ok("initial".to_owned());
    }
    if let Some(number) = value.as_i64() {
        return Ok(if number == 0 {
            "0".to_owned()
        } else {
            format!("{number}px")
        });
    }
    let text = value.to_string();
    if !text.is_empty() && text.chars().all(|ch| ch.is_ascii_digit()) {
        return Ok(if text.parse::<i64>().unwrap_or(0) == 0 {
            "0".to_owned()
        } else {
            format!("{text}px")
        });
    }
    Ok(text)
}

fn entry_count(value: &Value) -> usize {
    if let Ok(second) = value.get_item_by_index(1) {
        if let Ok(subitems) = second.get_item_by_index(1) {
            if let Some(len) = subitems.len() {
                return 1 + len;
            }
        }
    }
    1
}

/// Sphinx's `_slice_index` filter for splitting index entries into columns.
pub fn slice_index_filter(values: Value, slices: usize) -> Result<Value, Error> {
    if slices == 0 {
        return Err(invalid("slice_index requires at least one slice"));
    }
    let all: Vec<Value> = values
        .try_iter()
        .map_err(|error| invalid(error.to_string()))?
        .collect();
    let length: usize = all.iter().map(entry_count).sum();
    let items_per_slice = length / slices;
    let mut offset = 0usize;
    let mut result: Vec<Value> = Vec::with_capacity(slices);
    for slice_number in 0..slices {
        let start = offset;
        if slice_number + 1 == slices {
            offset = all.len();
        } else {
            let mut count = 0usize;
            for value in &all[offset..] {
                count += entry_count(value);
                offset += 1;
                if count >= items_per_slice {
                    break;
                }
            }
        }
        result.push(Value::from(all[start..offset].to_vec()));
    }
    Ok(Value::from(result))
}

/// Jinja2/MarkupSafe `escape` / `e` filter.
pub fn escape_filter(_state: &State<'_, '_>, value: &Value) -> Result<Value, Error> {
    if value.is_safe() {
        return Ok(value.clone());
    }
    Ok(Value::from_safe_string(html_escape_markupsafe(&value.to_string())))
}

/// Jinja2's `striptags` filter: remove HTML comments/tags and unescape entities.
pub fn striptags_filter(value: &Value) -> String {
    let text = value.to_string();
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'<' {
            if let Some(rel_end) = text[index..].find('>') {
                index += rel_end + 1;
                continue;
            }
            break;
        }
        let ch = text[index..].chars().next().unwrap_or(' ');
        out.push(ch);
        index += ch.len_utf8();
    }
    unescape_entities(&out)
}

fn unescape_entities(input: &str) -> String {
    let replacements: [(&str, char); 10] = [
        ("&amp;", '&'),
        ("&lt;", '<'),
        ("&gt;", '>'),
        ("&quot;", '"'),
        ("&#34;", '"'),
        ("&#39;", '\''),
        ("&#x27;", '\''),
        ("&apos;", '\''),
        ("&nbsp;", '\u{a0}'),
        ("&hellip;", '\u{2026}'),
    ];
    let mut out = input.to_owned();
    for (entity, ch) in replacements {
        out = out.replace(entity, &ch.to_string());
    }
    out
}

/// `accesskey(context, key)` — emits each access key at most once per render.
pub fn accesskey(state: &State<'_, '_>, key: String) -> Result<Value, Error> {
    if key.is_empty() {
        return Ok(Value::from_safe_string(String::new()));
    }
    let keys = state.get_or_set_temp_object::<AccessKeys, _>(
        "sphinx_accesskeys",
        AccessKeys::default,
    );
    let mut set = keys
        .0
        .lock()
        .map_err(|_| invalid("accesskey state was poisoned"))?;
    if set.insert(key.clone()) {
        Ok(Value::from_safe_string(format!(
            "accesskey=\"{}\"",
            html_escape_attr(&key)
        )))
    } else {
        Ok(Value::from_safe_string(String::new()))
    }
}

#[derive(Debug, Default)]
struct AccessKeys(Mutex<HashSet<String>>);

impl Object for AccessKeys {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Plain
    }
}

/// `idgen` — a callable object with `current()` and `next()` methods.
pub fn idgen() -> Value {
    Value::from_object(IdGen::default())
}

#[derive(Debug, Default)]
struct IdGen(AtomicI64);

impl Object for IdGen {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Plain
    }

    fn call_method(
        self: &Arc<Self>,
        _state: &State<'_, '_>,
        method: &str,
        _args: &[Value],
    ) -> Result<Value, Error> {
        match method {
            "current" => Ok(Value::from(self.0.load(Ordering::Relaxed))),
            "next" | "__next__" | "next_id" => {
                Ok(Value::from(self.0.fetch_add(1, Ordering::Relaxed) + 1))
            }
            other => Err(Error::new(ErrorKind::UnknownMethod, other.to_owned())),
        }
    }
}

/// `get_theme_option(name, default?)`.
pub fn get_theme_option(
    state: &State<'_, '_>,
    name: String,
    default: Option<Value>,
) -> Value {
    if let Some(value) = state.lookup(&format!("theme_{name}")) {
        if !value.is_undefined() {
            return value;
        }
    }
    if let Some(data) = render_state(state) {
        if let Some(value) = data.theme_options.get(&name) {
            return Value::from(value.clone());
        }
    }
    default.unwrap_or_else(|| Value::from(String::new()))
}

/// `translation(message)` / `_()` / `gettext()`.
pub fn translation(state: &State<'_, '_>, message: String) -> String {
    render_state(state)
        .and_then(|data| data.translations.get(&message).cloned())
        .unwrap_or(message)
}

/// `ngettext(singular, plural, n)`.
pub fn ngettext(state: &State<'_, '_>, singular: String, plural: String, count: i64) -> String {
    if count == 1 {
        translation(state, singular)
    } else {
        translation(state, plural)
    }
}

/// `warning(message, ...)` logs conceptually and renders nothing.
pub fn warning(_state: &State<'_, '_>, message: String, rest: minijinja::value::Rest<Value>) -> String {
    let _ = (message, rest);
    String::new()
}

/// `pformat`-like debug helper.
pub fn debug(value: Value) -> String {
    format!("{value:#?}")
}

/// Pre-rendered sidebar fragment.
pub fn sidebar(state: &State<'_, '_>) -> Value {
    Value::from_safe_string(
        render_state(state).map_or_else(String::new, |data| data.sidebar_html.clone()),
    )
}

/// Pre-rendered related-links fragment.
pub fn relbar(state: &State<'_, '_>) -> Value {
    Value::from_safe_string(
        render_state(state).map_or_else(String::new, |data| data.relbar_html.clone()),
    )
}

/// Pre-rendered toctree fragment. Accepts Sphinx's `toctree` kwargs
/// (`includehidden`, `collapse`, `maxdepth`, ...) and ignores them: Phase 17
/// renders the actual tree before constructing the context.
pub fn toctree(state: &State<'_, '_>, _kwargs: minijinja::value::Kwargs) -> Value {
    Value::from_safe_string(
        render_state(state).map_or_else(String::new, |data| data.toctree_html.clone()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_uri_matches_upstream_examples() {
        assert_eq!(relative_uri("index.html", "index.html"), "");
        assert_eq!(relative_uri("sub/index.html", "sub/"), "./");
        assert_eq!(relative_uri("sub/index.html", "index.html"), "../index.html");
        assert_eq!(relative_uri("a/b/c.html", "a/d.html"), "../d.html");
        assert_eq!(relative_uri("index.html", "/abs/path"), "/abs/path");
    }

    #[test]
    fn to_dim_and_bool_match_upstream() {
        assert_eq!(to_dim_filter(&Value::from(0)).unwrap(), "0");
        assert_eq!(to_dim_filter(&Value::from(12)).unwrap(), "12px");
        assert_eq!(to_dim_filter(&Value::from("0")).unwrap(), "0");
        assert_eq!(to_dim_filter(&Value::from("940px")).unwrap(), "940px");
        assert_eq!(to_dim_filter(&Value::from("initial")).unwrap(), "initial");
        assert_eq!(to_dim_filter(&Value::from(() )).unwrap(), "initial");
        assert!(to_bool_filter(&Value::from("YES")));
        assert!(!to_bool_filter(&Value::from("no")));
        assert_eq!(to_int_filter(&Value::from("nope")), 0);
        assert_eq!(to_int_filter(&Value::from("42")), 42);
    }

    #[test]
    fn striptags_removes_tags_and_unescapes() {
        assert_eq!(
            striptags_filter(&Value::from("<b>A &amp; B</b>")),
            "A & B"
        );
        assert_eq!(
            striptags_filter(&Value::from("<!-- c --><i>x</i>")),
            "x"
        );
    }
}
