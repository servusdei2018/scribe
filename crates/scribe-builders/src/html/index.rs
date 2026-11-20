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

/// Deterministic index body grouped by first letter. Command-line options
/// produce two entries in upstream's index: a symbol-keyed entry and a
/// program-keyed group ("git-commit command line option") with nested links.
fn render_index_body(title: &str, entries: &[ObjectEntry], href_for: &dyn Fn(&str) -> String) -> String {
    const SYMBOLS: char = '#';
    #[derive(Clone)]
    struct Logical<'a> {
        key: char,
        entry: &'a ObjectEntry,
        program: Option<String>,
        option: Option<String>,
    }
    let mut logical: Vec<Logical<'_>> = Vec::new();
    for entry in entries {
        if entry.objtype.contains("option") {
            let (program, option) = entry.name.split_once('.').unwrap_or(("", entry.name.as_str()));
            let option = option.to_owned();
            // symbol-keyed entry
            logical.push(Logical { key: SYMBOLS, entry, program: None, option: Some(option.clone()) });
            // program-keyed entry (only when there is a program prefix)
            if !program.is_empty() {
                let first = program.chars().next().unwrap_or('#').to_ascii_uppercase();
                logical.push(Logical {
                    key: if first.is_ascii_alphanumeric() { first } else { SYMBOLS },
                    entry,
                    program: Some(program.to_owned()),
                    option: Some(option),
                });
            }
        } else {
            let short = entry.name.rsplit('.').next().unwrap_or(&entry.name);
            let first = short.chars().next().unwrap_or('#');
            let key = if first.is_ascii_alphanumeric() { first.to_ascii_uppercase() } else { SYMBOLS };
            logical.push(Logical { key, entry, program: None, option: None });
        }
    }
    let mut grouped: BTreeMap<char, Vec<Logical<'_>>> = BTreeMap::new();
    for item in logical { grouped.entry(item.key).or_default().push(item); }
    let label = |key: char| if key == SYMBOLS { "Symbols".to_owned() } else { key.to_string() };
    let target_for = |entry: &ObjectEntry| -> String {
        if entry.docname.is_empty() {
            "#".to_owned()
        } else {
            let href = href_for(&entry.docname);
            if entry.anchor.is_empty() { href } else { format!("{href}#{}", entry.anchor) }
        }
    };
    let mut out = format!("<h1 id=\"{slug}\">{title}</h1>\n", slug = slug(title));
    if entries.is_empty() {
        out.push_str("\n<div class=\"genindex-jumpbox\">\n \n</div>\n");
        return out;
    }
    out.push_str("<div class=\"genindex-jumpbox\">\n ");
    let mut first = true;
    for key in grouped.keys() {
        if !first { out.push_str(" | "); }
        first = false;
        let text = label(*key);
        out.push_str(&format!("<a href=\"#{text}\"><strong>{text}</strong></a>"));
    }
    out.push_str("\n \n</div>\n");
    for (key, mut group) in grouped {
        let text = label(key);
        out.push_str(&format!("<h2 id=\"{text}\">{text}</h2>\n"));
        group.sort_by(|a, b| a.entry.name.cmp(&b.entry.name));
        let midpoint = group.len().div_ceil(2);
        out.push_str("<table style=\"width: 100%\" class=\"indextable genindextable\"><tr>\n");
        for column in [&group[..midpoint], &group[midpoint..]] {
            out.push_str("<td style=\"width: 33%; vertical-align: top;\"><ul>\n");
            if key == SYMBOLS {
                for item in column {
                    let option = item.option.as_deref().unwrap_or(&item.entry.name);
                    out.push_str(&format!("      <li>\n    {}\n\n      <ul>\n", html_escape(option)));
                    out.push_str(&format!(
                        "        <li><a href=\"{}\">{}</a>\n</li>\n",
                        html_escape(&target_for(item.entry)),
                        html_escape(&item.entry.display_name)
                    ));
                    out.push_str("      </ul></li>\n");
                }
            } else {
                // Group program-keyed options so each program has one term
                // with nested option links; other entries are plain links.
                let mut index = 0usize;
                while index < column.len() {
                    let item = &column[index];
                    if let Some(program) = &item.program {
                        out.push_str(&format!("      <li>\n    {} command line option\n\n      <ul>\n", html_escape(program)));
                        while index < column.len() && column[index].program.as_deref() == Some(program.as_str()) {
                            let sub = &column[index];
                            let opt = sub.option.as_deref().unwrap_or("");
                            out.push_str(&format!(
                                "        <li><a href=\"{}\">{}</a>\n</li>\n",
                                html_escape(&target_for(sub.entry)),
                                html_escape(opt)
                            ));
                            index += 1;
                        }
                        out.push_str("      </ul></li>\n");
                    } else {
                        let lbl = if item.entry.objtype == "term" || item.entry.objtype.starts_with("idx_") {
                            format!("<strong>{}</strong>", html_escape(&item.entry.display_name))
                        } else {
                            html_escape(&item.entry.display_name)
                        };
                        out.push_str(&format!(
                            "<li><a href=\"{}\">{}</a></li>\n",
                            html_escape(&target_for(item.entry)),
                            lbl
                        ));
                        index += 1;
                    }
                }
            }
            out.push_str("</ul></td>\n");
        }
        out.push_str("</tr></table>\n");
    }
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

/// Collect index entries from every doctree's `index` nodes (Sphinx's
/// `IndexCollector`). Each `entries` attribute is a Python-repr list of
/// `(type, name, anchor, ...)` tuples; only the first three matter here.
fn collect_doctree_index_entries(env: &scribe_core::BuildEnvironment) -> Vec<ObjectEntry> {
    let mut entries = Vec::new();
    for (docname, snapshot) in &env.documents {
        let Some(doc) = snapshot.doctree.as_ref() else { continue };
        for id in doc.preorder_ids() {
            let Ok(node) = doc.get(id) else { continue };
            if node.kind != scribe_ast::NodeKind::Index { continue; }
            let raw = node.get_str("entries").ok().flatten().unwrap_or("");
            for (kind, name, anchor) in parse_index_entries(raw) {
                let display = name.rsplit(';').next().unwrap_or(&name).trim().to_owned();
                if display.is_empty() { continue; }
                let objtype = if anchor.starts_with("term-") { "term".to_owned() } else { format!("idx_{kind}") };
                entries.push(ObjectEntry::new(name, display, objtype, docname.clone(), anchor, -1));
            }
        }
    }
    entries
}

/// Splits an `entries` attribute into `(type, name, anchor)` triples.
fn parse_index_entries(raw: &str) -> Vec<(String, String, String)> {
    let mut result = Vec::new();
    for part in raw.split("['").skip(1) {
        let inner = part.split("]").next().unwrap_or("");
        let fields: Vec<&str> = inner.split("', '").collect();
        if fields.len() < 3 { continue; }
        let kind = fields[0].trim_matches(|c: char| c == '\'' || c == '"').to_owned();
        let name = fields[1].trim_matches(|c: char| c == '\'' || c == '"').to_owned();
        let anchor = fields[2].trim_matches(|c: char| c == '\'' || c == '"').to_owned();
        if !name.is_empty() { result.push((kind, name, anchor)); }
    }
    result
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
    env: &scribe_core::BuildEnvironment,
    outdir: &Path,
    flavor: &BuildFlavor,
) -> Result<Vec<std::path::PathBuf>> {
    // Sphinx's general index is generated from `IndexEntries.create_index`,
    // which only sees *indexable* objects/terms. `std:label` objects (explicit
    // and implicit `:ref:` targets, including the virtual `genindex`,
    // `modindex` and `search` labels) are never listed there. Filter every
    // label so named code blocks do not leak phantom rows into the index.
    let mut objects: Vec<ObjectEntry> = collect_objects(registry)
        .into_iter()
        .filter(|entry| entry.objtype != "label")
        .collect();
    // Skip index nodes that merely mirror a registered domain object.
    let known_anchors: std::collections::BTreeSet<String> = objects.iter()
        .map(|entry| entry.anchor.clone()).collect();
    let extra = collect_doctree_index_entries(env)
        .into_iter()
        .filter(|entry| !known_anchors.contains(&entry.anchor))
        .collect::<Vec<_>>();
    objects.extend(extra);
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
