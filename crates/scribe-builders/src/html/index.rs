//! General index, Python module index and search-page generation.
//!
//! The index pages are rendered with the theme's own `genindex.html` /
//! `modindex.html` templates where available, with a deterministic built-in
//! fallback otherwise. The search page is a Phase 17 placeholder: it contains
//! the same shell upstream emits but no index data (the full `searchindex.js`
//! search index is Phase 18).

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use scribe_domains::{DomainRegistry, ObjectEntry};
use scribe_theme::{RenderContext, ThemeManager};

use crate::error::HtmlError;
use crate::error::Result;

use super::builder::BuildFlavor;

/// Build a `genindex.html` page from the domain object entries.
///
/// `entries` is the flattened object list (from `DomainRegistry::get_objects`).
/// The returned HTML is a complete document produced by the theme when
/// possible, else a minimal deterministic fallback.
pub fn generate_genindex(
    manager: &ThemeManager,
    ctx: &RenderContext,
    entries: &[ObjectEntry],
) -> Result<String> {
    generate_genindex_with(manager, ctx, entries, &|docname| format!("{docname}.html"))
}

/// Flavor-aware variant: object links use the builder's target-URI convention.
pub fn generate_genindex_with(
    manager: &ThemeManager,
    ctx: &RenderContext,
    entries: &[ObjectEntry],
    href_for: &dyn Fn(&str) -> String,
) -> Result<String> {
    let mut ctx = ctx.clone();
    ctx.title = "Index".to_owned();
    ctx.body = render_index_body("Index", entries, href_for);
    ctx.use_index = false;
    ctx.set_var("title", "Index");
    render_page(manager, &ctx, "genindex.html")
}

/// Build a `py-modindex.html` page from Python module object entries.
pub fn generate_modindex(
    manager: &ThemeManager,
    ctx: &RenderContext,
    entries: &[ObjectEntry],
) -> Result<String> {
    generate_modindex_with(manager, ctx, entries, &|docname| format!("{docname}.html"))
}

/// Flavor-aware variant of [`generate_modindex`].
pub fn generate_modindex_with(
    manager: &ThemeManager,
    ctx: &RenderContext,
    entries: &[ObjectEntry],
    href_for: &dyn Fn(&str) -> String,
) -> Result<String> {
    let modules: Vec<ObjectEntry> = entries
        .iter()
        .filter(|entry| entry.objtype == "module" || entry.objtype.starts_with("py:module"))
        .cloned()
        .collect();
    let mut ctx = ctx.clone();
    ctx.title = "Python Module Index".to_owned();
    ctx.body = render_index_body("Python Module Index", &modules, href_for);
    ctx.use_index = false;
    ctx.set_var("title", "Python Module Index");
    render_page(manager, &ctx, "modindex.html")
}

/// Build a functional `search.html` page (Phase 18).
///
/// The bundled `search.html` template is byte-faithful to upstream's: it loads
/// `searchtools.js` and `language_data.js`, defers `searchindex.js`, and
/// provides the `#search-results` container. `searchtools.js` calls
/// `_ready(Search.init)` itself, so no inline script is required.
pub fn generate_search_page(manager: &ThemeManager, ctx: &RenderContext) -> Result<String> {
    let mut ctx = ctx.clone();
    ctx.title = "Search".to_owned();
    ctx.use_index = false;
    ctx.search_enabled = true;
    ctx.set_var("title", "Search");
    manager.render_template("search.html", &ctx).map_err(HtmlError::theme)
}

fn render_page(manager: &ThemeManager, ctx: &RenderContext, _fallback_name: &str) -> Result<String> {
    // Use the generic page template: it is available in every bundled theme and
    // accepts our pre-rendered body without extra domain-index variables.
    manager.render_template("page.html", ctx).map_err(HtmlError::theme)
}

/// Deterministic index body grouped by first letter.
fn render_index_body(title: &str, entries: &[ObjectEntry], href_for: &dyn Fn(&str) -> String) -> String {
    let mut grouped: BTreeMap<char, Vec<&ObjectEntry>> = BTreeMap::new();
    for entry in entries {
        let key = entry.name.chars().next().unwrap_or('#').to_ascii_uppercase();
        grouped.entry(key).or_default().push(entry);
    }
    let mut out = format!("<h1 id=\"{slug}\">{title}</h1>\n", slug = slug(title));
    if entries.is_empty() {
        out.push_str("<p>No entries.</p>\n");
        return out;
    }
    out.push_str("<div class=\"genindexwrapper\">\n<ul class=\"search\">\n");
    for letter in grouped.keys() {
        out.push_str(&format!("<li><a href=\"#letter-{letter}\">{letter}</a></li>\n"));
    }
    out.push_str("</ul>\n");
    for (letter, mut group) in grouped {
        out.push_str(&format!("<section id=\"letter-{letter}\">\n<h2>{letter}</h2>\n<ul>\n"));
        group.sort_by(|a, b| a.name.cmp(&b.name));
        for entry in group {
            let target = if entry.docname.is_empty() {
                "#".to_owned()
            } else {
                let href = href_for(&entry.docname);
                if entry.anchor.is_empty() {
                    href
                } else {
                    format!("{href}#{}", entry.anchor)
                }
            };
            out.push_str(&format!(
                "<li><a href=\"{}\">{}</a></li>\n",
                html_escape(&target),
                html_escape(&entry.display_name)
            ));
        }
        out.push_str("</ul>\n</section>\n");
    }
    out.push_str("</div>\n");
    out
}

fn slug(text: &str) -> String {
    text.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

fn html_escape(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Collect object entries from every registered domain, sorted deterministically.
#[must_use]
pub fn collect_objects(registry: &DomainRegistry) -> Vec<ObjectEntry> {
    let mut entries = registry.get_objects();
    entries.sort_by(|a, b| (&a.name, &a.objtype, &a.docname).cmp(&(&b.name, &b.objtype, &b.docname)));
    entries
}

/// Write all generated index/search pages into `outdir`.
///
/// The output paths follow the flavor's target-URI convention, so dirhtml and
/// the serializing builders emit `genindex/index.html` / `genindex.fjson`
/// rather than flat `genindex.html` files.
pub fn write_index_pages(
    manager: &ThemeManager,
    ctx: &RenderContext,
    registry: &DomainRegistry,
    outdir: &Path,
    flavor: &BuildFlavor,
) -> Result<Vec<std::path::PathBuf>> {
    // The std domain keeps virtual `:ref:` labels (`genindex`, `modindex`,
    // `search`) so those xrefs resolve; they are not real domain objects and
    // upstream's general index never lists them as entries (its genindex body
    // is generated from `IndexEntries.create_index`, which only sees indexable
    // objects/terms).  Filter them here so we neither render phantom index
    // rows nor emit a link to a `py-modindex.html` page that does not exist.
    let objects: Vec<ObjectEntry> = collect_objects(registry)
        .into_iter()
        .filter(|entry| {
            !(entry.objtype == "label"
                && matches!(entry.name.as_str(), "genindex" | "modindex" | "search"))
        })
        .collect();
    let mut pages: Vec<(String, String)> = Vec::new();
    // Each generated page needs its own `docname` so `pathto` computes the
    // correct base URI (crucial for dirhtml's nested `genindex/index.html`).
    // Object links are likewise made relative to that page.
    let href_for_page = |page: &str| {
        let base = flavor.target_uri(page);
        move |docname: &str| {
            let uri = flavor.target_uri(docname);
            let relative = scribe_theme::relative_uri(&base, &uri);
            if relative.is_empty() { "#".to_owned() } else { relative }
        }
    };
    let mut gen_ctx = ctx.clone();
    gen_ctx.docname = "genindex".to_owned();
    let genindex = generate_genindex_with(manager, &gen_ctx, &objects, &href_for_page("genindex"))?;
    pages.push((flavor.output_path("genindex").to_string_lossy().into_owned(), genindex));
    // Upstream only emits a domain index page when that index has content
    // (`StandaloneHTMLBuilder`: `content, collapse = index.generate(); if
    // content: ...`).  The Python module index therefore has no `py-modindex`
    // page at all in projects that declare no `py:module` objects.  Gate the
    // page on real module entries so we never ship an empty phantom index.
    let has_modules = objects
        .iter()
        .any(|entry| entry.objtype == "module" || entry.objtype.starts_with("py:module"));
    if has_modules {
        let mut mod_ctx = ctx.clone();
        mod_ctx.docname = "py-modindex".to_owned();
        let modindex = generate_modindex_with(manager, &mod_ctx, &objects, &href_for_page("py-modindex"))?;
        pages.push((flavor.output_path("py-modindex").to_string_lossy().into_owned(), modindex));
    }
    if flavor.search {
        let mut search_ctx = ctx.clone();
        search_ctx.docname = "search".to_owned();
        pages.push((flavor.output_path("search").to_string_lossy().into_owned(), generate_search_page(manager, &search_ctx)?));
    }
    let mut written = Vec::new();
    for (name, html) in pages {
        let path = outdir.join(&name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| HtmlError::io(parent, e))?;
        }
        fs::write(&path, html).map_err(|e| HtmlError::io(&path, e))?;
        written.push(std::path::PathBuf::from(name));
    }
    Ok(written)
}
