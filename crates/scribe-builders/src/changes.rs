//! Changelog builder (Phase 25).
//!
//! Upstream `sphinx/builders/changes.py` renders three HTML files into the
//! output directory: `index.html` (a frameset), `changes.html` (the actual
//! summary of `versionadded` / `versionchanged` / `deprecated` /
//! `versionremoved` directives in the configured `version`) and one
//! `rst/<docname>.html` per source document (a highlighted copy of the raw
//! source). Entries are grouped into "Library changes", "C API changes" and
//! "Other changes" exactly as upstream does.
//!
//! `changes_titles_*` configuration keys mentioned in older Sphinx releases
//! do not exist in the pinned oracle (`sphinx/builders/changes.py` and the
//! built-in config registry); scribe therefore ignores them, which is
//! documented as a divergence in `ARCHITECTURE.md` §31.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use scribe_ast::{Document, NodeId, NodeKind};
use scribe_config::Config;
use scribe_core::BuildEnvironment;
use serde::{Deserialize, Serialize};

use crate::build_common::{prepare, PreparedBuild};
use crate::error::{OutputError, OutputResult};
use crate::html::{BuildOptions, BuildResult};

/// A single changeset, matching `sphinx.domains.changeset.ChangeSet`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeSet {
    /// Normalized directive type (`versionadded`, `versionchanged`,
    /// `deprecated`, `versionremoved`).
    pub kind: String,
    /// Document containing the directive.
    pub docname: String,
    /// 1-based source line of the directive marker.
    pub lineno: i64,
    /// Python module in scope, when the directive is nested in a module.
    pub module: Option<String>,
    /// Object description name in scope, when nested in an object directive.
    pub descname: String,
    /// Rendered text of the `versionmodified` node (label + description).
    pub content: String,
}

/// Builder for `-b changes`.
pub struct ChangesBuilder;

impl ChangesBuilder {
    /// Build the changelog summary files.
    pub fn build(
        options: &BuildOptions,
        config: &Config,
        env: &mut BuildEnvironment,
    ) -> OutputResult<BuildResult> {
        let PreparedBuild { docnames, parsed, warnings, .. } =
            prepare(options, config, env, "changes")?;
        let version = config.get_str("version").unwrap_or("");
        let mut sources = BTreeMap::new();
        for docname in &docnames {
            if let Some(path) = env.found_docs.get(docname) {
                if let Ok(text) = fs::read_to_string(path) {
                    sources.insert(docname.clone(), text);
                }
            }
        }
        let changesets = collect_changesets_with_sources(&parsed, &sources, version);

        fs::create_dir_all(&options.output_dir)
            .map_err(|error| OutputError::io("changes", &options.output_dir, error))?;

        let mut written = Vec::new();
        if changesets.is_empty() {
            return Ok(BuildResult { written, built_docnames: docnames, warnings });
        }

        let ctx = ChangesContext::from_config(config, version, &parsed, changesets);
        emit("changes", &options.output_dir, Path::new("changes.html"), &render_changes_html(&ctx), &mut written)?;
        emit("changes", &options.output_dir, Path::new("index.html"), &render_index_html(&ctx), &mut written)?;

        // A highlighted source page per document mirrors upstream's
        // `changes/rstsource.html` rendering (the frameset's "src" frame).
        let rst_dir = options.output_dir.join("rst");
        fs::create_dir_all(&rst_dir).map_err(|error| OutputError::io("changes", &rst_dir, error))?;
        let source_root = fs::canonicalize(&options.source_dir).unwrap_or_else(|_| options.source_dir.clone());
        for docname in &docnames {
            let Some(path) = env.found_docs.get(docname) else {
                continue;
            };
            let source = fs::read_to_string(path)
                .map_err(|error| OutputError::io("changes", path, error))?;
            let text = highlight_source(&source, version);
            let filename = path
                .strip_prefix(&source_root)
                .unwrap_or(path)
                .to_string_lossy()
                .into_owned();
            let rel = Path::new("rst").join(format!("{docname}.html"));
            let out = options.output_dir.join(&rel);
            if let Some(parent) = out.parent() {
                fs::create_dir_all(parent).map_err(|error| OutputError::io("changes", parent, error))?;
            }
            fs::write(&out, render_rst_source(&ctx, &filename, &text))
                .map_err(|error| OutputError::io("changes", &out, error))?;
            written.push(rel);
        }

        // Upstream copies the default theme's two stylesheets. The scribe
        // theme ships an equivalent `basic.css`; emit a minimal deterministic
        // `default.css` so the generated HTML is self-contained.
        emit("changes", &options.output_dir, Path::new("basic.css"), BASIC_CSS, &mut written)?;
        emit("changes", &options.output_dir, Path::new("default.css"), DEFAULT_CSS, &mut written)?;

        Ok(BuildResult { written, built_docnames: docnames, warnings })
    }
}

const DEFAULT_CSS: &str = "/* scribe changes builder default theme placeholder */\n";
const BASIC_CSS: &str = "/* scribe changes builder basic theme placeholder */\n";

fn emit(
    builder: &'static str,
    outdir: &Path,
    relative: &Path,
    text: &str,
    written: &mut Vec<std::path::PathBuf>,
) -> OutputResult<()> {
    let out = outdir.join(relative);
    fs::write(&out, text).map_err(|error| OutputError::io(builder, &out, error))?;
    written.push(relative.to_path_buf());
    Ok(())
}

/// Collect all version-modified nodes whose `version` matches `version`,
/// walking each document in deterministic arena order.
pub fn collect_changesets(parsed: &BTreeMap<String, Document>, version: &str) -> Vec<ChangeSet> {
    collect_changesets_with_sources(parsed, &BTreeMap::new(), version)
}

/// Collect changesets, correcting source line numbers and module context from
/// the raw source when available.
///
/// Nested directives in the frozen AST carry content-relative line numbers
/// (§10.3), while upstream's `node.line` is absolute. The changes builder
/// reads the raw sources anyway (for `rst/*.html`), so it recovers absolute
/// lines and the `py:module` context by scanning directive lines in order.
pub fn collect_changesets_with_sources(
    parsed: &BTreeMap<String, Document>,
    sources: &BTreeMap<String, String>,
    version: &str,
) -> Vec<ChangeSet> {
    let mut changesets = Vec::new();
    for (docname, document) in parsed {
        let source_hints = source_directive_hints(sources.get(docname).map(String::as_str).unwrap_or(""));
        let mut used_hints = vec![false; source_hints.len()];
        // First pass: record the module in scope at each node.
        let mut module_at: BTreeMap<NodeId, String> = BTreeMap::new();
        let mut current_module: Option<String> = None;
        for id in document.preorder_ids() {
            let Ok(node) = document.get(id) else {
                continue;
            };
            let objtype = node.attrs.get_str("objtype").unwrap_or_default().unwrap_or("");
            if node.kind == NodeKind::Target && matches!(objtype, "module" | "currentmodule") {
                if let Some(signature) = node.attrs.get_str("py:signature").unwrap_or_default() {
                    if !signature.is_empty() {
                        current_module = Some(signature.to_owned());
                    }
                }
            }
            if current_module.as_deref().is_some_and(|m| !m.is_empty()) {
                module_at.insert(id, current_module.clone().unwrap_or_default());
            }
        }
        // Second pass: the changesets themselves.
        for id in document.preorder_ids() {
            let Ok(node) = document.get(id) else {
                continue;
            };
            if !node.kind.is_version_modified() {
                continue;
            }
            let node_version = node.attrs.get_str("version").unwrap_or_default().unwrap_or("");
            if node_version != version {
                continue;
            }
            let kind = normalize_kind(
                node.attrs.get_str("type").unwrap_or_default().or_else(|| node.kind.version_type()).unwrap_or(""),
            );
            let mut lineno = node.source.as_ref().and_then(|s| s.line).map(i64::from).unwrap_or(0);
            let descname = find_descname(document, id).unwrap_or_default();
            let mut module = module_at.get(&id).cloned();
            if let Some(index) = source_hints.iter().enumerate().find_map(|(i, hint)| {
                (!used_hints[i] && hint.kind == kind && hint.version == node_version).then_some(i)
            }) {
                let hint = &source_hints[index];
                // Only trust the source-derived line when the AST's line is
                // clearly relative (smaller than the absolute marker line).
                if lineno < hint.line || lineno <= 0 {
                    lineno = hint.line;
                }
                if hint.module.is_some() {
                    module = hint.module.clone();
                }
                if let Some(flag) = used_hints.get_mut(index) {
                    *flag = true;
                }
            }
            let content = node.astext(document).replace('\n', " ");
            changesets.push(ChangeSet { kind, docname: docname.clone(), lineno, module, descname, content });
        }
    }
    changesets
}

#[derive(Clone, Debug)]
struct SourceHint {
    kind: String,
    version: String,
    line: i64,
    module: Option<String>,
}

/// Scan raw source lines for version directives and module declarations.
fn source_directive_hints(source: &str) -> Vec<SourceHint> {
    let mut hints = Vec::new();
    let mut module: Option<String> = None;
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix(".. ") else {
            continue;
        };
        let (name, argument) = match rest.split_once("::") {
            Some(parts) => parts,
            None => continue,
        };
        let name = name.trim();
        let argument = argument.trim();
        if matches!(name, "module" | "currentmodule") {
            if let Some(target) = argument.split_whitespace().next() {
                if !target.is_empty() {
                    module = Some(target.to_owned());
                }
            }
            continue;
        }
        let normalized = normalize_kind(name);
        if !matches!(normalized.as_str(), "versionadded" | "versionchanged" | "deprecated" | "versionremoved") {
            continue;
        }
        let version = argument.split_whitespace().next().unwrap_or("").to_owned();
        hints.push(SourceHint { kind: normalized, version, line: (index + 1) as i64, module: module.clone() });
    }
    hints
}

fn normalize_kind(kind: &str) -> String {
    match kind {
        "version-added" => "versionadded".to_owned(),
        "version-changed" => "versionchanged".to_owned(),
        "version-deprecated" => "deprecated".to_owned(),
        "version-removed" => "versionremoved".to_owned(),
        other => other.to_owned(),
    }
}

fn find_descname(document: &Document, id: NodeId) -> Option<String> {
    for ancestor in document.ancestors(id) {
        let node = document.get(ancestor).ok()?;
        if node.kind == NodeKind::Desc {
            for (key, value) in node.attrs.iter() {
                if let Some(signature) = key.strip_suffix(":signature") {
                    if let scribe_ast::AttributeValue::Str(sig) = value {
                        let normalized = if signature == "c" { normalize_c_signature(sig) } else { sig.to_string() };
                        return Some(normalized);
                    }
                }
            }
        }
    }
    None
}

/// Upstream's C domain renders `void* Test_Malloc` as `void *Test_Malloc`.
fn normalize_c_signature(signature: &str) -> String {
    let mut out = String::with_capacity(signature.len());
    let chars: Vec<char> = signature.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if ch == '*' && i > 0 && chars[i - 1].is_alphanumeric() {
            out.push(' ');
            out.push('*');
            if i + 1 < chars.len() && chars[i + 1].is_whitespace() {
                i += 1;
            }
        } else {
            out.push(ch);
        }
        i += 1;
    }
    out
}

/// Fully assembled data for the changelog templates.
#[derive(Clone, Debug)]
pub struct ChangesContext {
    pub project: String,
    pub version: String,
    pub docstitle: String,
    pub shorttitle: String,
    pub libchanges: Vec<(String, Vec<Entry>)>,
    pub apichanges: Vec<Entry>,
    pub otherchanges: Vec<((String, String), Vec<Entry>)>,
}

/// One rendered changelog entry plus its source location.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Entry {
    pub html: String,
    pub docname: String,
    pub lineno: i64,
}

impl ChangesContext {
    /// Group changesets the same way upstream's `ChangesBuilder.write_documents` does.
    pub fn from_config(
        config: &Config,
        version: &str,
        parsed: &BTreeMap<String, Document>,
        changesets: Vec<ChangeSet>,
    ) -> Self {
        let mut lib: BTreeMap<String, Vec<Entry>> = BTreeMap::new();
        let mut api: Vec<Entry> = Vec::new();
        let mut other: BTreeMap<(String, String), Vec<Entry>> = BTreeMap::new();
        for change in changesets {
            let ttext = match change.kind.as_str() {
                "versionadded" => "added",
                "versionchanged" => "changed",
                "deprecated" => "deprecated",
                "versionremoved" => "removed",
                _ => change.kind.as_str(),
            };
            let context = change.content.replace('\n', " ");
            let with_desc = |name: &str| -> String {
                if context.is_empty() {
                    format!("<b>{name}</b>: <i>{ttext}</i>.")
                } else {
                    format!("<b>{name}</b>: <i>{ttext}:</i> {context}")
                }
            };
            let entry = Entry { html: String::new(), docname: change.docname.clone(), lineno: change.lineno };
            if !change.descname.is_empty() && change.docname.starts_with("c-api") {
                let mut e = entry;
                e.html = with_desc(&change.descname);
                api.push(e);
            } else if !change.descname.is_empty() || change.module.is_some() {
                let module = change.module.clone().unwrap_or_else(|| "Builtins".to_owned());
                let name = if change.descname.is_empty() { "Module level" } else { &change.descname };
                let mut e = entry;
                e.html = with_desc(name);
                lib.entry(module).or_default().push(e);
            } else {
                if context.is_empty() {
                    continue;
                }
                let mut e = entry;
                e.html = format!("<i>{}:</i> {}", capitalize(ttext), context);
                let title = parsed
                    .get(&change.docname)
                    .and_then(|doc| doc.metadata.title.clone())
                    .unwrap_or_else(|| change.docname.clone());
                other.entry((change.docname.clone(), title)).or_default().push(e);
            }
        }
        api.sort();
        let docstitle = config.get_str("html_title").unwrap_or("").to_owned();
        let shorttitle = config.get_str("html_short_title").unwrap_or(&docstitle).to_owned();
        Self {
            project: config.get_str("project").unwrap_or("").to_owned(),
            version: version.to_owned(),
            docstitle,
            shorttitle,
            libchanges: lib.into_iter().collect(),
            apichanges: api,
            otherchanges: other.into_iter().collect(),
        }
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
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

fn render_entries(changes: &[Entry], out: &mut String) {
    out.push_str("<ul>\n");
    for (index, entry) in changes.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push_str(&format!(
            "<li><a href=\"rst/{}.html#L{}\" target=\"src\">{}</a></li>\n",
            entry.docname,
            entry.lineno - 10,
            entry.html
        ));
    }
    out.push_str("</ul>\n");
}

/// Render the `changes/versionchanges.html` template.
pub fn render_changes_html(ctx: &ChangesContext) -> String {
    let title = format!("Changes in Version {} &#8212; {}", escape_html(&ctx.version), escape_html(&ctx.docstitle));
    let mut out = String::new();
    out.push_str("<!DOCTYPE HTML PUBLIC \"-//W3C//DTD HTML 4.01 Transitional//EN\"\n  \"http://www.w3.org/TR/html4/loose.dtd\">\n");
    out.push_str("<html lang=\"\">\n  <head>\n    <link rel=\"stylesheet\" href=\"default.css\">\n    <meta http-equiv=\"Content-Type\" content=\"text/html; charset=utf-8\">\n");
    out.push_str(&format!("    <title>{title}</title>\n  </head>\n  <body>\n    <div class=\"document\">\n      <div class=\"body\">\n"));
    out.push_str(&format!("    <h1>Automatically generated list of changes in version {}</h1>\n", escape_html(&ctx.version)));
    out.push_str("    <h2>Library changes</h2>\n    \n");
    for (module, changes) in &ctx.libchanges {
        out.push_str(&format!("    <h4>{}</h4>\n    \n", escape_html(module)));
        render_entries(changes, &mut out);
        out.push('\n');
    }
    out.push_str("    \n    <h2>C API changes</h2>\n    \n");
    render_entries(&ctx.apichanges, &mut out);
    out.push_str("\n    <h2>Other changes</h2>\n    \n");
    for ((docname, heading), changes) in &ctx.otherchanges {
        out.push_str(&format!(
            "    <h4>{} <span style=\"font-size: 50%\">({})</span></h4>\n    \n",
            escape_html(heading),
            escape_html(docname)
        ));
        render_entries(changes, &mut out);
        out.push_str("\n    \n");
    }
    out.push_str("      </div>\n    </div>\n  </body>\n</html>");
    out
}

/// Render the `changes/frameset.html` template.
pub fn render_index_html(ctx: &ChangesContext) -> String {
    let title = format!("Changes in Version {} &#8212; {}", escape_html(&ctx.version), escape_html(&ctx.docstitle));
    format!(
        "<!DOCTYPE HTML PUBLIC \"-//W3C//DTD HTML 4.01 Frameset//EN\"\n  \"http://www.w3.org/TR/html4/frameset.dtd\">\n<html lang=\"\">\n  <head>\n    <title>{title}</title>\n  </head>\n  <frameset cols=\"45%,*\">\n    <frame name=\"main\" src=\"changes.html\">\n    <frame name=\"src\" src=\"about:blank\">\n  </frameset>\n</html>"
    )
}

/// Render the `changes/rstsource.html` template (the source frame).
pub fn render_rst_source(_ctx: &ChangesContext, filename: &str, text: &str) -> String {
    // Upstream's `rstsource.html` template references only `filename` and
    // `text`; `docstitle` is undefined and therefore renders empty.
    let title = format!("{} &#8212; ", escape_html(filename));
    format!(
        "<!DOCTYPE HTML PUBLIC \"-//W3C//DTD HTML 4.01 Transitional//EN\"\n  \"http://www.w3.org/TR/html4/loose.dtd\">\n<html lang=\"\">\n  <head>\n    <title>{title}</title>\n    <style type=\"text/css\">\n      .hl {{ background-color: yellow }}\n    </style>\n  </head>\n  <body style=\"font-size: 90%\">\n    <pre>\n      {text}\n    </pre>\n  </body>\n</html>"
    )
}

/// Highlight version-directive lines in a raw source file, mirroring the
/// `hl()` helper in upstream's changes builder.
pub fn highlight_source(source: &str, version: &str) -> String {
    let directives = [
        "versionchanged",
        "versionadded",
        "deprecated",
        "versionremoved",
        "version-changed",
        "version-added",
        "version-deprecated",
        "version-removed",
    ];
    let mut out = String::new();
    for (i, line) in source.split_inclusive('\n').enumerate() {
        let mut escaped = format!("<a name=\"L{}\"> </a>{}", i + 1, escape_html(line));
        for directive in directives {
            let needle = format!(".. {directive}:: {version}");
            if escaped.contains(&needle) {
                escaped = format!("<span class=\"hl\">{escaped}</span>");
                break;
            }
        }
        out.push_str(&escaped);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_kind_handles_aliases() {
        assert_eq!(normalize_kind("version-added"), "versionadded");
        assert_eq!(normalize_kind("versionremoved"), "versionremoved");
        assert_eq!(normalize_kind("unknown"), "unknown");
    }

    #[test]
    fn c_signature_normalization_adds_space() {
        assert_eq!(normalize_c_signature("void* Test_Malloc(size_t n)"), "void *Test_Malloc(size_t n)");
    }

    #[test]
    fn highlight_source_marks_version_directives() {
        let src = "Title\n=====\n\n.. versionadded:: 0.6\n   Text.\n";
        let out = highlight_source(src, "0.6");
        assert!(out.contains("<span class=\"hl\"><a name=\"L4\"> </a>.. versionadded:: 0.6\n</span>"));
        assert!(out.contains("<a name=\"L1\"> </a>Title"));
    }

    #[test]
    fn render_changes_html_has_all_sections() {
        let ctx = ChangesContext {
            project: "P".into(), version: "1.0".into(), docstitle: "T".into(), shorttitle: "T".into(),
            libchanges: vec![], apichanges: vec![], otherchanges: vec![],
        };
        let html = render_changes_html(&ctx);
        assert!(html.contains("<h2>Library changes</h2>"));
        assert!(html.contains("<h2>C API changes</h2>"));
        assert!(html.contains("<h2>Other changes</h2>"));
        assert!(html.contains("Changes in Version 1.0 &#8212; T"));
    }
}
