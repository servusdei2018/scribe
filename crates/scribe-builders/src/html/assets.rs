//! Static asset, source dump and build-info helpers for the HTML builder.
//!
//! All copying is deterministic: files are collected into a sorted list and
//! written in that order, and no timestamps are embedded in generated content.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use miette::miette;
use rayon::prelude::*;
use scribe_config::Config;
use scribe_theme::{theme_static_files, Theme, ThemeManager};

use crate::error::HtmlError;
use crate::error::Result;

/// Copy every static file of `theme` into `outdir/_static/`.
///
/// Returns the relative output paths written. `.jinja` / `.css_t` source files
/// are skipped (they are compiled by [`render_asset_templates`] separately).
pub fn copy_theme_assets(theme: &Theme, outdir: &Path) -> Result<Vec<PathBuf>> {
    let static_dir = outdir.join("_static");
    fs::create_dir_all(&static_dir).map_err(|source| HtmlError::io(&static_dir, source))?;
    let mut written = Vec::new();
    for source in theme_static_files(theme) {
        let Some(name) = source.file_name().and_then(|n| n.to_str()) else { continue };
        if name.ends_with(".jinja") || name.ends_with("_t") || name.ends_with(".css_t") {
            continue;
        }
        let target = static_dir.join(name);
        copy_file(&source, &target)?;
        written.push(PathBuf::from("_static").join(name));
    }
    Ok(written)
}

/// Render the theme's `.jinja`/`.css_t` template assets into `outdir/_static/`.
///
/// These are the scripts/stylesheets whose upstream source uses Jinja
/// expressions (`documentation_options.js`, `language_data.js`, `basic.css`,
/// `alabaster.css_t`). Rendering is done once with a minimal context derived
/// from `config`; the output is deterministic.
pub fn render_asset_templates(
    manager: &ThemeManager,
    theme: &Theme,
    config: &Config,
    outdir: &Path,
    language: &str,
    search_enabled: bool,
) -> Result<Vec<PathBuf>> {
    let mut ctx = scribe_theme::RenderContext::for_builder("html", "index", config.clone());
    // `language_data.js.jinja` needs the stop-word set and the (minified)
    // stemmer source. Without these the search page would load but would have
    // no `Stemmer` class and no stop words (Phase 18).
    ctx.set_var("search_language_stop_words", crate::search_language_stop_words_json(language));
    ctx.set_var("search_language_stemming_code", crate::search_language_stemming_code(language));
    ctx.set_var("search_scorer_tool", "");
    ctx.set_var("search_word_splitter_code", "");
    let mut written = Vec::new();
    let env = manager.environment(theme).map_err(HtmlError::theme)?;
    let templates: Vec<String> = theme_static_files(theme)
        .into_iter()
        .filter_map(|path| {
            let name = path.file_name()?.to_str()?.to_owned();
            if name.ends_with(".jinja") || name.ends_with(".css_t") { Some(name) } else { None }
        })
        .collect();

    for template in templates {
        let rendered = env
            .render(&template, &ctx)
            .map_err(HtmlError::theme)?;
        // Target name: strip `.jinja`, and map `foo.css_t` to `foo.css`
        // (the `_t` suffix itself denotes an already-`.css`-named template).
        let mut target_name = template.strip_suffix(".jinja").unwrap_or(&template).to_owned();
        if let Some(base) = target_name.strip_suffix("_t") {
            target_name = base.to_owned();
        }
        let target = outdir.join("_static").join(&target_name);
        fs::create_dir_all(target.parent().unwrap_or(outdir)).map_err(|source| HtmlError::io(&target, source))?;
        let mut file = fs::File::create(&target).map_err(|source| HtmlError::io(&target, source))?;
        file.write_all(rendered.as_bytes()).map_err(|source| HtmlError::io(&target, source))?;
        written.push(PathBuf::from("_static").join(target_name));
    }
    let _ = search_enabled;
    Ok(written)
}

/// Copy source dumps for every built document into `outdir/_sources/`.
///
/// `sources` is a `docname -> (source path, source suffix)` map. Output files
/// are named `<docname><suffix>.txt` exactly like upstream.
pub fn write_source_dumps(
    sources: &BTreeMap<String, (PathBuf, String)>,
    outdir: &Path,
) -> Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    for (docname, (source, suffix)) in sources {
        let relative = format!("{docname}{suffix}.txt");
        let target = outdir.join("_sources").join(&relative);
        fs::create_dir_all(target.parent().unwrap_or(outdir)).map_err(|e| HtmlError::io(&target, e))?;
        let content = fs::read_to_string(source).unwrap_or_default();
        fs::write(&target, content).map_err(|e| HtmlError::io(&target, e))?;
        written.push(PathBuf::from("_sources").join(relative));
    }
    Ok(written)
}

/// Copy a list of image assets from `source_dir` into `outdir/_images/`,
/// returning the `source uri -> output uri` map used by the translator.
pub fn copy_images(
    images: &[String],
    source_dir: &Path,
    outdir: &Path,
) -> Result<BTreeMap<String, String>> {
    let target_dir = outdir.join("_images");
    let mut mapping = BTreeMap::new();
    let unique: std::collections::BTreeSet<&String> = images.iter().collect();
    for uri in unique {
        let source = source_dir.join(uri);
        let name = Path::new(uri)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("image");
        let target = target_dir.join(name);
        if source.is_file() {
            fs::create_dir_all(&target_dir).map_err(|e| HtmlError::io(&target_dir, e))?;
            copy_file(&source, &target)?;
        }
        mapping.insert(uri.clone(), format!("_images/{name}"));
    }
    Ok(mapping)
}

/// Write Sphinx's `.buildinfo` marker (`# Sphinx build info version 1` + hash).
pub fn write_buildinfo(outdir: &Path, config_hash: &str) -> Result<()> {
    let content = format!("# Sphinx build info version 1\n# This file hashes the configuration used when building these files.\n{config_hash} hash\n");
    let path = outdir.join(".buildinfo");
    fs::write(&path, content).map_err(|e| HtmlError::io(&path, e))
}

/// Copy arbitrary `html_static_path` entries into `outdir/_static/`.
pub fn copy_extra_static(paths: &[PathBuf], source_dir: &Path, outdir: &Path) -> Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    let target_dir = outdir.join("_static");
    for raw in paths {
        let base = source_dir.join(raw);
        let Ok(entries) = fs::read_dir(&base) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() { continue; }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let target = target_dir.join(name);
            copy_file(&path, &target)?;
            written.push(PathBuf::from("_static").join(name));
        }
    }
    Ok(written)
}

/// Copy files in parallel where possible (called with a prepared pair list).
pub fn copy_files_parallel(pairs: &[(PathBuf, PathBuf)]) -> Result<()> {
    pairs.par_iter().try_for_each(|(source, target)| -> Result<()> {
        copy_file(source, target)
    })
}

fn copy_file(source: &Path, target: &Path) -> Result<()> {
    if source == target { return Ok(()); }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| HtmlError::io(parent, e))?;
    }
    fs::copy(source, target)
        .map(|_| ())
        .map_err(|e| HtmlError::io(source, e))
}

/// Diagnostic helper for a missing source file (used by builder validation).
pub fn missing_source(path: &Path) -> miette::Report {
    miette!("source file not found: {}", path.display())
}
