//! Metadata builders (Phase 19): `json`, `htmlhelp`, `qthelp`, `devhelp`.
//!
//! Upstream these are `StandaloneHTMLBuilder` subclasses that emit additional
//! project files during `finish()`. Scribe runs the shared HTML-family
//! pipeline (see [`crate::html::builder::build_with_flavor`]) and then writes
//! the deterministic project files from the emitted file list, the config and
//! the build environment's toctree hierarchy. No Sphinx version strings or
//! timestamps are embedded, so the bytes are reproducible.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use flate2::write::GzEncoder;
use flate2::Compression;
use scribe_config::Config;
use scribe_core::BuildEnvironment;

use crate::error::{HtmlError, Result};
use crate::html::builder::{build_with_flavor, BuildFlavor};
use crate::html::{BuildOptions, BuildResult};

pub const JSON_BUILDER_NAME: &str = "json";
pub const HTMLHELP_BUILDER_NAME: &str = "htmlhelp";
pub const QTHELP_BUILDER_NAME: &str = "qthelp";
pub const DEVHELP_BUILDER_NAME: &str = "devhelp";

const DEFAULT_PROJECT: &str = "Project name not set";

// ---------------------------------------------------------------------------
// Public builders
// ---------------------------------------------------------------------------

/// `json` (sphinxcontrib.serializinghtml) builder.
#[derive(Debug, Default)]
pub struct JsonBuilder;

impl JsonBuilder {
    #[must_use]
    pub fn new() -> Self { Self }
    pub fn build(options: &BuildOptions, config: &Config, env: &mut BuildEnvironment) -> Result<BuildResult> {
        build_with_flavor(options, config, env, &BuildFlavor::json())
    }
}

/// Windows HTML Help (`htmlhelp`) builder.
#[derive(Debug, Default)]
pub struct HtmlHelpBuilder;

impl HtmlHelpBuilder {
    #[must_use]
    pub fn new() -> Self { Self }
    pub fn build(options: &BuildOptions, config: &Config, env: &mut BuildEnvironment) -> Result<BuildResult> {
        let mut result = build_with_flavor(options, config, env, &BuildFlavor::metadata(HTMLHELP_BUILDER_NAME))?;
        write_htmlhelp_files(config, env, &options.output_dir, &mut result)?;
        Ok(result)
    }
}

/// Qt help (`qthelp`) builder.
#[derive(Debug, Default)]
pub struct QtHelpBuilder;

impl QtHelpBuilder {
    #[must_use]
    pub fn new() -> Self { Self }
    pub fn build(options: &BuildOptions, config: &Config, env: &mut BuildEnvironment) -> Result<BuildResult> {
        let mut result = build_with_flavor(options, config, env, &BuildFlavor::metadata(QTHELP_BUILDER_NAME))?;
        write_qthelp_files(config, env, &options.output_dir, &mut result)?;
        Ok(result)
    }
}

/// GNOME Devhelp builder.
#[derive(Debug, Default)]
pub struct DevHelpBuilder;

impl DevHelpBuilder {
    #[must_use]
    pub fn new() -> Self { Self }
    pub fn build(options: &BuildOptions, config: &Config, env: &mut BuildEnvironment) -> Result<BuildResult> {
        let mut result = build_with_flavor(options, config, env, &BuildFlavor::metadata(DEVHELP_BUILDER_NAME))?;
        write_devhelp_files(config, env, &options.output_dir, &mut result)?;
        Ok(result)
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// `sphinx.util.osutil.make_filename` (non-alphanumerics removed, fallback).
#[must_use]
pub fn make_filename(value: &str) -> String {
    let filtered: String = value.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
    if filtered.is_empty() { "sphinx".to_owned() } else { filtered }
}

/// `make_filename_from_project`: drop a trailing ` Documentation`, lowercase.
#[must_use]
pub fn make_filename_from_project(project: &str) -> String {
    make_filename(project.strip_suffix(" Documentation").unwrap_or(project)).to_lowercase()
}

fn html_title(config: &Config) -> String {
    let project = config.get_str("project").filter(|s| !s.is_empty()).unwrap_or(DEFAULT_PROJECT);
    let release = config.get_str("release").unwrap_or("");
    let configured = config.get_str("html_title").filter(|s| !s.is_empty());
    configured.map_or_else(|| format!("{project} {release} documentation"), str::to_owned)
}

fn doc_title(config: &Config, env: &BuildEnvironment, docname: &str) -> String {
    if docname == config.master_doc() {
        return html_title(config);
    }
    env.all_docs.get(docname)
        .and_then(|meta| meta.title.clone())
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| docname.to_owned())
}

/// Depth-first section hierarchy from the environment's toctree edges.
fn doc_hierarchy(env: &BuildEnvironment, master: &str) -> Vec<(String, usize)> {
    fn visit(env: &BuildEnvironment, name: &str, depth: usize, seen: &mut BTreeSet<String>, out: &mut Vec<(String, usize)>) {
        if name.is_empty() || !seen.insert(name.to_owned()) { return; }
        out.push((name.to_owned(), depth));
        if let Some(children) = env.toctree_includes.get(name) {
            for child in children {
                visit(env, child, depth + 1, seen, out);
            }
        }
    }
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    visit(env, master, 0, &mut seen, &mut out);
    out
}

/// Files referenced by a help project: every emitted HTML page plus the
/// non-JavaScript files under `_static`.
fn project_files(result: &BuildResult, slash: char) -> Vec<String> {
    let mut files = BTreeSet::new();
    for path in &result.written {
        let name = path.to_string_lossy().replace('\\', "/");
        let is_html = name.ends_with(".html");
        let in_static = name.starts_with("_static/");
        if is_html || (in_static && !name.ends_with(".js")) {
            files.insert(name.replace('/', &slash.to_string()));
        }
    }
    files.into_iter().collect()
}

/// HTML escaping matching Python's `html.escape(s, quote=True)` including the
/// decimal apostrophe form used by the CHM project files.
fn chm_escape(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn write(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| HtmlError::io(parent, e))?;
    }
    fs::write(path, content).map_err(|e| HtmlError::io(path, e))
}

// ---------------------------------------------------------------------------
// htmlhelp
// ---------------------------------------------------------------------------

/// Write `<basename>.hhp`, `.hhc`, `.hhk` and `.stp` into `outdir`.
pub fn write_htmlhelp_files(config: &Config, env: &BuildEnvironment, outdir: &Path, result: &mut BuildResult) -> Result<()> {
    let project = config.get_str("project").filter(|s| !s.is_empty()).unwrap_or(DEFAULT_PROJECT);
    let basename = config.get_str("htmlhelp_basename").map(str::to_owned)
        .unwrap_or_else(|| format!("{}doc", make_filename_from_project(project)));
    let title = html_title(config);
    let master = config.master_doc();
    let files = project_files(result, '\\');

    let mut hhp = format!(
        "[OPTIONS]\nBinary TOC=No\nBinary Index=No\nCompiled file={basename}.chm\nContents file={basename}.hhc\nDefault Window={basename}\nDefault topic={master}.html\nDisplay compile progress=No\nFull text search stop list file={basename}.stp\nFull-text search=Yes\nIndex file={basename}.hhk\nLanguage=0x409\nTitle={title}\n\n[WINDOWS]\n{basename}=\"{title}\",\"{basename}.hhc\",\"{basename}.hhk\",\"{master}.html\",\"{master}.html\",,,,,0x63520,220,0x10384e,[0,0,1024,768],,,,,,,0\n\n[FILES]\n"
    );
    for file in &files {
        hhp.push_str(file);
        hhp.push('\n');
    }
    write(&outdir.join(format!("{basename}.hhp")), &hhp)?;
    result.written.push(PathBuf::from(format!("{basename}.hhp")));

    // Table of contents (`.hhc`).
    let mut hhc = String::from(
        "<!DOCTYPE HTML PUBLIC \"-//IETF//DTD HTML//EN\">\n<HTML>\n<HEAD>\n<meta name=\"GENERATOR\" content=\"Microsoft&reg; HTML Help Workshop 4.1\">\n<!-- Sitemap 1.0 -->\n</HEAD><BODY>\n<OBJECT type=\"text/site properties\">\n        <param name=\"Window Styles\" value=\"0x801227\">\n        <param name=\"ImageType\" value=\"Folder\">\n</OBJECT>\n<UL>\n"
    );
    hhc.push_str(&format!("<LI> <OBJECT type=\"text/sitemap\">\n    <param name=\"Name\" value=\"{}\">\n    <param name=\"Local\" value=\"{master}.html\">\n</OBJECT>\n", chm_escape(&title)));
    for (docname, depth) in doc_hierarchy(env, master) {
        if docname == master { continue; }
        if depth > 1 { hhc.push_str("<UL>\n"); }
        hhc.push_str(&format!("<LI> <OBJECT type=\"text/sitemap\">\n    <param name=\"Name\" value=\"{}\">\n    <param name=\"Local\" value=\"{docname}.html\">\n</OBJECT>\n", chm_escape(&doc_title(config, env, &docname))));
        if depth > 1 { hhc.push_str("</UL>\n"); }
    }
    hhc.push_str("</UL></BODY></HTML>");
    write(&outdir.join(format!("{basename}.hhc")), &hhc)?;
    result.written.push(PathBuf::from(format!("{basename}.hhc")));

    // Empty keyword index (no explicit index entries are produced by scribe).
    write(&outdir.join(format!("{basename}.hhk")), "<UL>\n</UL>\n")?;
    result.written.push(PathBuf::from(format!("{basename}.hhk")));
    write(&outdir.join(format!("{basename}.stp")), "")?;
    result.written.push(PathBuf::from(format!("{basename}.stp")));
    Ok(())
}

// ---------------------------------------------------------------------------
// qthelp
// ---------------------------------------------------------------------------

/// Write `<basename>.qhp` and `.qhcp` into `outdir`.
pub fn write_qthelp_files(config: &Config, env: &BuildEnvironment, outdir: &Path, result: &mut BuildResult) -> Result<()> {
    let project = config.get_str("project").filter(|s| !s.is_empty()).unwrap_or(DEFAULT_PROJECT);
    let basename = config.get_str("qthelp_basename").map(str::to_owned)
        .unwrap_or_else(|| make_filename(project));
    let version = config.get_str("version").unwrap_or("");
    let title = html_title(config);
    let master = config.master_doc();
    let namespace = config.get_str("qthelp_namespace").map(|raw| {
        let cleaned: String = raw.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '-').collect();
        let collapsed = cleaned.split('.').filter(|part| !part.is_empty()).collect::<Vec<_>>().join(".");
        collapsed.trim_matches('.').to_lowercase()
    }).unwrap_or_else(|| {
        let parts: Vec<&str> = ["org", "sphinx", basename.as_str(), version].into_iter().filter(|part| !part.is_empty()).collect();
        parts.join(".").to_lowercase()
    });
    let files = project_files(result, '/');

    let mut qhp = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\" ?>\n<QtHelpProject version=\"1.0\">\n    <namespace>{namespace}</namespace>\n    <virtualFolder>doc</virtualFolder>\n    <customFilter name=\"{title}\">\n        <filterAttribute>{basename}</filterAttribute>\n        <filterAttribute></filterAttribute>\n    </customFilter>\n    <filterSection>\n        <filterAttribute>{basename}</filterAttribute>\n        <filterAttribute></filterAttribute>\n        <toc>\n"
    );
    for (docname, depth) in doc_hierarchy(env, master) {
        let indent = " ".repeat(12 + depth * 4);
        qhp.push_str(&format!("{indent}<section title=\"{}\" ref=\"{docname}.html\"></section>\n", chm_escape(&doc_title(config, env, &docname))));
    }
    qhp.push_str("        </toc>\n        <keywords>\n\n        </keywords>\n        <files>\n");
    for file in &files {
        qhp.push_str(&format!("            <file>{file}</file>\n"));
    }
    qhp.push_str("        </files>\n    </filterSection>\n</QtHelpProject>");

    let qhcp = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\" ?>\n<QHelpCollectionProject version=\"1.0\">\n    <assistant>\n        <title>{title}</title>\n        <homePage>qthelp://{namespace}/doc/{master}.html</homePage>\n        <startPage>qthelp://{namespace}/doc/index.html</startPage>\n    </assistant>\n    <docFiles>\n        <generate>\n            <file>\n                <input>{basename}.qhp</input>\n                <output>{basename}.qch</output>\n            </file>\n        </generate>\n        <register>\n            <file>{basename}.qch</file>\n        </register>\n    </docFiles>\n</QHelpCollectionProject>"
    );
    write(&outdir.join(format!("{basename}.qhp")), &qhp)?;
    write(&outdir.join(format!("{basename}.qhcp")), &qhcp)?;
    result.written.push(PathBuf::from(format!("{basename}.qhp")));
    result.written.push(PathBuf::from(format!("{basename}.qhcp")));
    Ok(())
}

// ---------------------------------------------------------------------------
// devhelp
// ---------------------------------------------------------------------------

/// Write the Devhelp XML (`.devhelp.gz`, plus an uncompressed `.devhelp2` for
/// tool interoperability) into `outdir`.
pub fn write_devhelp_files(config: &Config, env: &BuildEnvironment, outdir: &Path, result: &mut BuildResult) -> Result<()> {
    let project = config.get_str("project").filter(|s| !s.is_empty()).unwrap_or(DEFAULT_PROJECT);
    let basename = config.get_str("devhelp_basename").map(str::to_owned)
        .unwrap_or_else(|| make_filename(project));
    let title = html_title(config);
    let version = config.get_str("version").unwrap_or("");
    let master = config.master_doc();

    let mut xml = format!(
        "<book title=\"{}\" name=\"{}\" link=\"{master}.html\" version=\"{}\"><chapters>",
        chm_escape(&title), chm_escape(project), chm_escape(version)
    );
    for (docname, depth) in doc_hierarchy(env, master) {
        if docname == master { continue; }
        if depth > 1 { xml.push_str("<sub>"); }
        xml.push_str(&format!("<sub link=\"{docname}.html\" name=\"{}\"/>", chm_escape(&doc_title(config, env, &docname))));
        if depth > 1 { xml.push_str("</sub>"); }
    }
    xml.push_str("</chapters><functions/></book>");

    // Deterministic gzip: mtime = 0 and a fixed OS byte. flate2's GzEncoder
    // writes mtime 0 by default, which is what upstream uses.
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(xml.as_bytes()).map_err(|e| HtmlError::io(outdir, e))?;
    let compressed = encoder.finish().map_err(|e| HtmlError::io(outdir, e))?;
    let gz_path = outdir.join(format!("{basename}.devhelp.gz"));
    if let Some(parent) = gz_path.parent() {
        fs::create_dir_all(parent).map_err(|e| HtmlError::io(parent, e))?;
    }
    fs::write(&gz_path, &compressed).map_err(|e| HtmlError::io(&gz_path, e))?;
    result.written.push(PathBuf::from(format!("{basename}.devhelp.gz")));

    // The task spec also names `.devhelp2`; emit the plain XML under that name.
    let xml_path = outdir.join(format!("{basename}.devhelp2"));
    write(&xml_path, &xml)?;
    result.written.push(PathBuf::from(format!("{basename}.devhelp2")));
    Ok(())
}

