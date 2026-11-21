//! General index, Python module index and search-page generation.
//!
//! The index pages are rendered with the theme's own `genindex.html` /
//! `modindex.html` templates where available, with a deterministic built-in
//! fallback otherwise. The search page is a Phase 17 placeholder: it contains
//! the same shell upstream emits but no index data (the full `searchindex.js`
//! search index is Phase 18).

use std::collections::{BTreeMap, BTreeSet};
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

/// Renders the Python module index using the theme's `domainindex.html`
/// structure: a letter jumpbox followed by a single table with per-letter
/// caption rows and one row per module.
fn render_modindex_body(
    title: &str,
    entries: &[ObjectEntry],
    href_for: &dyn Fn(&str) -> String,
) -> String {
    let mut grouped: BTreeMap<char, Vec<&ObjectEntry>> = BTreeMap::new();
    for entry in entries {
        let key = entry.name.chars().next().unwrap_or('#').to_ascii_lowercase();
        grouped.entry(key).or_default().push(entry);
    }
    let mut out = format!("<h1>{title}</h1>\n\n   <div class=\"modindex-jumpbox\">\n   ");
    let letters: Vec<char> = grouped.keys().copied().collect();
    for (index, letter) in letters.iter().enumerate() {
        if index > 0 { out.push_str(" | \n   "); }
        out.push_str(&format!("<a href=\"#cap-{letter}\"><strong>{letter}</strong></a>"));
    }
    out.push_str("\n   </div>\n\n   <table class=\"indextable modindextable\">\n");
    for (letter, mut group) in grouped {
        group.sort_by(|a, b| a.name.cmp(&b.name));
        out.push_str("     <tr class=\"pcap\"><td></td><td>&#160;</td><td></td></tr>\n");
        out.push_str(&format!("     <tr class=\"cap\" id=\"cap-{letter}\"><td></td><td>\n       <strong>{letter}</strong></td><td></td></tr>\n"));
        for entry in group {
            let href = href_for(&entry.docname);
            let anchor = if entry.anchor.is_empty() { format!("module-{}", entry.name) } else { entry.anchor.clone() };
            let platform = if entry.platform.is_empty() {
                String::new()
            } else {
                format!(" <em>({})</em>", html_escape(&entry.platform))
            };
            out.push_str(&format!(
                "     <tr>\n       <td></td>\n       <td>\n       <a href=\"{href}#{anchor}\"><code class=\"xref\">{}</code></a>{platform}</td><td>\n       <em>{}</em></td></tr>\n",
                html_escape(&entry.name),
                html_escape(&entry.synopsis)
            ));
        }
    }
    out.push_str("   </table>\n");
    out
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
    ctx.body = render_modindex_body("Python Module Index", &modules, href_for);
    ctx.use_index = false;
    ctx.set_var("title", "Python Module Index");
    // Upstream's `PythonModuleIndex.generate()` reports `collapse=True`; its
    // domain-index template emits this inline override in the page head.
    ctx.metatags.push_str("\n    <script>\n      DOCUMENTATION_OPTIONS.COLLAPSE_INDEX = true;\n    </script>\n");
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
        extra_links: Vec<String>,
    }
    // A single index term can be registered at several locations; Sphinx
    // renders those as `[n]` cross-reference links on the term's one `<li>`.
    // Group duplicates by anchor and attach their hrefs to the primary entry.
    let href_of = |entry: &ObjectEntry| -> String {
        if entry.docname.is_empty() {
            "#".to_owned()
        } else {
            let base = href_for(&entry.docname);
            if entry.anchor.is_empty() { base } else { format!("{base}#{}", entry.anchor) }
        }
    };
    let mut anchor_links: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut seen_anchor: BTreeSet<String> = BTreeSet::new();
    for entry in entries {
        if entry.anchor.is_empty() || entry.objtype.contains("option") { continue; }
        if seen_anchor.insert(entry.anchor.clone()) {
            anchor_links.entry(entry.anchor.clone()).or_default();
        } else if let Some(links) = anchor_links.get_mut(&entry.anchor) {
            if links.is_empty() { links.push(href_of(entry)); }
        }
    }
    // Entries that appear at more than one location get an extra `[n]`
    // cross-reference link (Sphinx's `indexentries` template macro).
    let mut display_counts: BTreeMap<String, usize> = BTreeMap::new();
    for entry in entries {
        *display_counts.entry(entry.display_name.clone()).or_insert(0) += 1;
    }
    let mut seen_display: BTreeMap<String, usize> = BTreeMap::new();
    let mut primary_anchors: BTreeSet<String> = BTreeSet::new();
    let mut logical: Vec<Logical<'_>> = Vec::new();
    for entry in entries {
        let extras = anchor_links.get(&entry.anchor).cloned().unwrap_or_default();
        // Skip anchor duplicates: their hrefs were folded into the primary
        // entry above, so they must not create their own `<li>`.
        if !entry.anchor.is_empty() && !entry.objtype.contains("option") && !extras.is_empty()
            && !primary_anchors.insert(entry.anchor.clone())
        {
            continue;
        }
        if entry.objtype.contains("option") {
            let (program, option) = entry.name.split_once('.').unwrap_or(("", entry.name.as_str()));
            let option = option.to_owned();
            // symbol-keyed entry
            logical.push(Logical { key: SYMBOLS, entry, program: None, option: Some(option.clone()), extra_links: Vec::new() });
            // program-keyed entry (only when there is a program prefix)
            if !program.is_empty() {
                let first = program.chars().next().unwrap_or('#').to_ascii_uppercase();
                logical.push(Logical {
                    key: if first.is_ascii_alphanumeric() { first } else { SYMBOLS },
                    entry,
                    program: Some(program.to_owned()),
                    option: Some(option),
                    extra_links: Vec::new(),
                });
            }
        } else {
            let short = entry.name.rsplit('.').next().unwrap_or(&entry.name);
            let first = short.chars().next().unwrap_or('#');
            let key = if first.is_ascii_alphanumeric() { first.to_ascii_uppercase() } else { SYMBOLS };
            logical.push(Logical { key, entry, program: None, option: None, extra_links: extras });
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
    // Returns the second-and-later `[n]` link for a duplicated display name.
    let mut crossref_for = |entry: &ObjectEntry| -> String {
        if display_counts.get(&entry.display_name).copied().unwrap_or(0) <= 1 {
            return String::new();
        }
        let seen = seen_display.entry(entry.display_name.clone()).or_insert(0);
        *seen += 1;
        if *seen == 1 {
            return String::new();
        }
        format!(", <a href=\"{}\">[{}]</a>", html_escape(&target_for(entry)), *seen - 1)
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
        // Sphinx's `entries|slice_index(2)` renders a letter group in a single
        // column when it consists of one program-option family, one
        // `single: primary; secondary` family, or a lone module-data entry.
        // Those groups get one `<td>`; their two `<ul>` blocks are preserved.
        let single_family = key != SYMBOLS
            && ((!group.is_empty() && group.iter().all(|item| item.program.is_some()))
                || (!group.is_empty() && group.iter().all(|item| item.entry.objtype == "idx_single"))
                || (group.len() == 1 && group[0].entry.objtype == "data"));
        let midpoint = group.len().div_ceil(2);
        out.push_str("<table style=\"width: 100%\" class=\"indextable genindextable\"><tr>\n");
        if single_family {
            out.push_str("<td style=\"width: 33%; vertical-align: top;\">\n");
        }
        let mut group_crossref_used = false;
        for column in [&group[..midpoint], &group[midpoint..]] {
            if single_family {
                out.push_str("<ul>\n");
            } else {
                out.push_str("<td style=\"width: 33%; vertical-align: top;\"><ul>\n");
            }
            if key == SYMBOLS {
                for item in column {
                    let option = item.option.as_deref().unwrap_or(&item.entry.name);
                    // Sphinx folds a value-placeholder option (`--abi={TYPE}`)
                    // into its base flag's term, keeping the link but not a
                    // separate term. Merge its link into the surrounding list
                    // instead of emitting a redundant top-level `<li>`.
                    let is_placeholder = option.contains('{');
                    if is_placeholder {
                        out.push_str("      <ul>\n");
                    } else {
                        out.push_str(&format!("      <li>\n    {}\n\n      <ul>\n", html_escape(option)));
                    }
                    out.push_str(&format!(
                        "        <li><a href=\"{}\">{}</a>\n</li>\n",
                        html_escape(&target_for(item.entry)),
                        html_escape(&item.entry.display_name)
                    ));
                    if is_placeholder {
                        out.push_str("      </ul>\n");
                    } else {
                        out.push_str("      </ul></li>\n");
                    }
                }
            } else {
                // Group program-keyed options so each program has one term
                // with nested option links; other entries are plain links.
                let mut index = 0usize;
                while index < column.len() {
                    let item = &column[index];
                    // Sphinx's `IndexEntries` splits `single: primary; secondary`
                    // entries into a primary term with a nested `<ul>` of
                    // sub-items.  Group them here without changing the entry
                    // inventory (pair cmdoptions are already handled above).
                    if item.program.is_none() && item.entry.objtype == "idx_single" {
                        if let Some((prefix, _)) = item.entry.name.split_once(';') {
                            let prefix = prefix.trim().to_owned();
                            out.push_str(&format!("      <li>\n    {}\n\n      <ul>\n", html_escape(&prefix)));
                            while index < column.len() {
                                let sub = &column[index];
                                let Some((sub_prefix, _)) = sub.entry.name.split_once(';') else { break };
                                if sub_prefix.trim() != prefix { break; }
                                let lbl = if sub.entry.objtype == "term" || sub.entry.objtype.starts_with("idx_") {
                                    format!("<strong>{}</strong>", html_escape(&sub.entry.display_name))
                                } else {
                                    html_escape(&sub.entry.display_name)
                                };
                                // Merge repeated sub-entries (e.g. two `RFC; RFC 1`
                                // index directives) into one `<li>` with `[n]` links.
                                let display = sub.entry.display_name.clone();
                                let mut links = vec![target_for(sub.entry)];
                                index += 1;
                                while index < column.len()
                                    && column[index].entry.display_name == display
                                    && column[index].entry.name.split_once(';')
                                        .is_some_and(|(p, _)| p.trim() == prefix)
                                {
                                    links.push(target_for(column[index].entry));
                                    index += 1;
                                }
                                let extra_cross = if !group_crossref_used
                                    && display_counts.get(&sub.entry.display_name).copied().unwrap_or(0) > 1
                                {
                                    group_crossref_used = true;
                                    crossref_for(sub.entry)
                                } else {
                                    String::new()
                                };
                                out.push_str(&format!(
                                    "        <li><a href=\"{}\">{}</a>{}",
                                    html_escape(&links[0]), lbl, extra_cross
                                ));
                                for (number, href) in links[1..].iter().enumerate() {
                                    out.push_str(&format!(", <a href=\"{}\">[{}]</a>", html_escape(href), number + 1));
                                }
                                out.push_str("</li>\n");
                            }
                            out.push_str("      </ul></li>\n");
                            continue;
                        }
                    }
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
                        let mut extra = String::new();
                        if item.extra_links.is_empty() {
                            extra.push_str(&crossref_for(item.entry));
                        } else {
                            for (number, href) in item.extra_links.iter().enumerate() {
                                extra.push_str(&format!(", <a href=\"{}\">[{}]</a>", html_escape(href), number + 1));
                            }
                        }
                        out.push_str(&format!(
                            "<li><a href=\"{}\">{}</a>{}</li>\n",
                            html_escape(&target_for(item.entry)),
                            lbl,
                            extra
                        ));
                        index += 1;
                    }
                }
            }
            if single_family {
                out.push_str("</ul>\n");
            } else {
                out.push_str("</ul></td>\n");
            }
        }
        if single_family {
            out.push_str("</td>\n");
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
    // Skip index nodes that merely mirror a registered domain object, except
    // when that anchor is indexed more than once — those duplicates become
    // `[n]` cross-reference links on the primary index term.
    let known_anchors: std::collections::BTreeSet<String> = objects.iter()
        .map(|entry| entry.anchor.clone()).collect();
    let doctree_entries = collect_doctree_index_entries(env);
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for entry in &doctree_entries {
        *counts.entry(entry.anchor.clone()).or_insert(0) += 1;
    }
    let extra = doctree_entries
        .into_iter()
        .filter(|entry| !known_anchors.contains(&entry.anchor)
            || counts.get(&entry.anchor).copied().unwrap_or(0) > 1)
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
