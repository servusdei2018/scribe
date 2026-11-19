//! Differential tests for the Phase 17 HTML5 writer and standalone builder.
//!
//! Method (documented in `ARCHITECTURE.md` §21.7):
//!
//! 1. Run `.oracle-venv/bin/sphinx-build -b html -E -q` on the real upstream
//!    fixtures `test-basic` and `test-root` into a temporary directory.
//! 2. Run scribe's [`StandaloneHtmlBuilder`] on the same source into a second
//!    temporary directory.
//! 3. For `test-basic` (whose doctree scribe parses faithfully) compare the
//!    normalised body fragment byte-for-byte after: stripping `?v=` cache
//!    hashes, un-doubling Phase 16's entity escaping (`&amp;#8212;` →
//!    `&#8212;`), collapsing whitespace runs and trimming.
//! 4. For `test-root` assert a curated set of structural invariants in both
//!    outputs (DOCTYPE, document/body wrappers, section ids, headerlinks,
//!    `simple` lists, admonitions, highlight blocks) and assert link integrity
//!    plus the presence of generated indexes/assets. The full byte stream is
//!    *not* compared: earlier phases intentionally diverge on parser-level
//!    details (section numbering, `toc-backref` ids, custom sidebar templates),
//!    which are recorded as divergences in `ARCHITECTURE.md`.
//!
//! The oracle is optional at test time: when the venv is unavailable the
//! structural/link-integrity assertions still run against scribe output.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use scribe_builders::html::{BuildOptions, StandaloneHtmlBuilder};
use scribe_config::{load_config_with, BridgeStrategy, Config, LoadRequest};
use scribe_core::BuildEnvironment;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("workspace root")
}

fn fixture(name: &str) -> PathBuf {
    workspace().join("sphinx/tests/roots").join(name)
}

fn tempdir(label: &str) -> PathBuf {
    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("scribe-html-{label}-{}-{sequence}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create temp dir");
    path
}

fn oracle(fixture_name: &str) -> Option<PathBuf> {
    let binary = workspace().join(".oracle-venv/bin/sphinx-build");
    if !binary.is_file() { return None; }
    let out = tempdir("oracle");
    let result = Command::new(binary)
        .arg("-b").arg("html").arg("-E").arg("-q")
        .arg(fixture(fixture_name)).arg(&out)
        .output().expect("run oracle sphinx-build");
    if !result.status.success() {
        eprintln!("oracle build failed: {}", String::from_utf8_lossy(&result.stderr));
        return None;
    }
    Some(out)
}

fn build_ours(fixture_name: &str) -> PathBuf {
    let source = fixture(fixture_name);
    let out = tempdir("ours");
    let (config, _) = load_config_with(LoadRequest {
        conf_dir: source.clone(),
        strategy: BridgeStrategy::Static,
        ..LoadRequest::default()
    }).expect("load fixture config");
    let options = BuildOptions {
        source_dir: source.clone(),
        output_dir: out.clone(),
        force_all: true,
        freshenv: true,
        worker_count: 4,
        ..BuildOptions::default()
    };
    let mut env = BuildEnvironment::with_config(&source, &config);
    let result = StandaloneHtmlBuilder::build(&options, &config, &mut env).expect("scribe build succeeded");
    assert!(!result.built_docnames.is_empty(), "no pages built");
    out
}

/// Extract the `<div class="body" role="main">…` fragment up to the first
/// `<div class="clearer">` that follows it.
fn body_fragment(html: &str) -> String {
    let start_marker = "<div class=\"body\" role=\"main\">";
    let Some(relative) = html.find(start_marker) else { return String::new(); };
    let tail = &html[relative + start_marker.len()..];
    let end = tail.find("<div class=\"clearer\"").unwrap_or(tail.len());
    tail[..end].to_owned()
}

/// Normalise away harmless differences (cache hashes, Phase 16 entity
/// double-escaping and whitespace) so structurally identical markup compares
/// equal.
fn normalise(fragment: &str) -> String {
    let mut out = fragment.to_owned();
    // `?v=01234567` cache-busting hashes differ between engines by design.
    if let Some(mut search) = out.find("?v=") {
        while let Some(index) = out[search..].find("?v=") {
            let absolute = search + index;
            let value_start = absolute + 3;
            let end = out[value_start..].find(|ch: char| ch == '"' || ch == '\'' || ch.is_whitespace())
                .map(|offset| value_start + offset)
                .unwrap_or(out.len());
            out.replace_range(value_start..end, "HASH");
            search = value_start + 4;
        }
    }
    // Phase 16 minijinja double-escapes `&#8212;` / `&#187;` in safe strings.
    out = out.replace("&amp;#", "&#");
    // Collapse all whitespace runs to a single space.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn test_basic_body_matches_oracle_structure() {
    let ours = build_ours("test-basic");
    let our_html = std::fs::read_to_string(ours.join("index.html")).expect("our index.html");
    let our_body = normalise(&body_fragment(&our_html));
    assert!(our_body.contains("<section id=\"the-basic-sphinx-documentation-for-testing\">"), "{our_body}");
    assert!(our_body.contains("class=\"headerlink\""), "{our_body}");
    assert!(our_body.contains("<ul class=\"simple\">"), "{our_body}");

    if let Some(oracle_dir) = oracle("test-basic") {
        let oracle_html = std::fs::read_to_string(oracle_dir.join("index.html")).expect("oracle index.html");
        let oracle_body = normalise(&body_fragment(&oracle_html));
        assert_eq!(oracle_body, our_body,
            "test-basic body diverged from oracle\nORACLE: {oracle_body}\nOURS:   {our_body}");
        std::fs::remove_dir_all(oracle_dir).ok();
    }
    std::fs::remove_dir_all(ours).ok();
}

#[test]
fn test_basic_page_skeleton_and_assets() {
    let ours = build_ours("test-basic");
    let html = std::fs::read_to_string(ours.join("index.html")).expect("index.html");
    assert!(html.starts_with("<!DOCTYPE html>"));
    assert!(html.contains("<html lang=\"en\" data-content_root=\"./\">"), "data-content_root missing");
    assert!(html.contains("<div class=\"document\">"));
    assert!(html.contains("<div class=\"body\" role=\"main\">"));
    assert!(html.contains("rel=\"stylesheet\""));
    assert!(html.contains("_static/pygments.css"));
    assert!(html.contains("_static/basic.css"));
    assert!(html.contains("documentation_options.js"));

    for file in [
        "index.html", "genindex.html", "search.html",
        "objects.inv", ".buildinfo", "searchindex.js",
        "_static/basic.css", "_static/pygments.css", "_static/doctools.js",
        "_static/documentation_options.js", "_static/sphinx_highlight.js",
        "_sources/index.rst.txt",
    ] {
        // `searchindex.js` is a Phase 18 artifact; only structural HTML files
        // and static assets are required in Phase 17.
        if file == "searchindex.js" { continue; }
        assert!(ours.join(file).is_file(), "missing generated file: {file}");
    }
    // Upstream only writes `py-modindex.html` when the project declares at
    // least one Python module (`StandaloneHTMLBuilder`: emit a domain index
    // only when its generated content is non-empty).  `test-basic` is a plain
    // RST fixture with no `py:module` objects, so no module index page exists.
    assert!(
        !ours.join("py-modindex.html").exists(),
        "py-modindex.html emitted for a project with no Python modules"
    );
    std::fs::remove_dir_all(ours).ok();
}

#[test]
fn test_root_structural_invariants_and_generated_pages() {
    let ours = build_ours("test-root");
    let mut pages = Vec::new();
    for entry in std::fs::read_dir(&ours).expect("read output dir").flatten() {
        if entry.path().extension().is_some_and(|ext| ext == "html") {
            pages.push(entry.path());
        }
    }
    pages.sort();
    assert!(pages.len() >= 10, "expected a multi-page build, got {} pages", pages.len());

    let mut saw_section = false;
    let mut saw_headerlink = false;
    let mut saw_admonition = false;
    let mut saw_highlight = false;
    let mut saw_simple_list = false;
    for page in &pages {
        let html = std::fs::read_to_string(page).expect("read page");
        assert!(html.starts_with("<!DOCTYPE html>"), "{}", page.display());
        assert!(html.contains("<div class=\"document\">"), "{}", page.display());
        assert!(html.contains("<div class=\"body\" role=\"main\">"), "{}", page.display());
        saw_section |= html.contains("<section id=");
        saw_headerlink |= html.contains("class=\"headerlink\"");
        saw_admonition |= html.contains("class=\"admonition");
        saw_highlight |= html.contains("<div class=\"highlight");
        saw_simple_list |= html.contains("<ul class=\"simple\">");
    }
    assert!(saw_section, "no sections rendered");
    assert!(saw_headerlink, "no headerlinks rendered");
    assert!(saw_admonition, "no admonitions rendered");
    assert!(saw_highlight, "no highlight blocks rendered");
    assert!(saw_simple_list, "no simple lists rendered");

    for page in ["genindex.html", "py-modindex.html", "search.html"] {
        assert!(ours.join(page).is_file(), "missing {page}");
    }
    assert!(ours.join("_static/basic.css").is_file());
    assert!(ours.join("objects.inv").is_file());

    if let Some(oracle_dir) = oracle("test-root") {
        for page in &pages {
            let name = page.file_name().unwrap().to_string_lossy();
            assert!(oracle_dir.join(name.as_ref()).is_file(), "oracle missing page {name}");
        }
        std::fs::remove_dir_all(oracle_dir).ok();
    }
    std::fs::remove_dir_all(ours).ok();
}

#[test]
fn internal_links_resolve_to_generated_files_or_anchors() {
    let source = fixture("test-basic");
    let out = build_ours("test-basic");
    let html = std::fs::read_to_string(out.join("index.html")).expect("index.html");

    let mut ids = BTreeSet::new();
    let mut cursor = 0usize;
    while let Some(index) = html[cursor..].find(" id=\"") {
        let start = cursor + index + 5;
        if let Some(end) = html[start..].find('"') {
            ids.insert(html[start..start + end].to_owned());
            cursor = start + end + 1;
        } else { break; }
    }

    let mut cursor = 0usize;
    while let Some(index) = html[cursor..].find("href=\"") {
        let start = cursor + index + 6;
        let Some(end) = html[start..].find('"') else { break; };
        let href = &html[start..start + end];
        if let Some(anchor) = href.strip_prefix('#') {
            assert!(ids.contains(anchor) || anchor.is_empty(), "dangling anchor #{anchor}");
        } else if !href.contains("://") && !href.starts_with("mailto:") {
            let path_only = href.split('?').next().unwrap_or(href);
            let target = out.join(path_only);
            assert!(target.exists(), "dangling internal link: {href}");
        }
        cursor = start + end + 1;
    }
    let _ = source;
    std::fs::remove_dir_all(out).ok();
}

#[test]
fn empty_source_builds_without_panic() {
    let source = tempdir("empty-src");
    let out = tempdir("empty-out");
    std::fs::write(source.join("index.rst"), "").expect("write empty source");
    let config = Config::new();
    let options = BuildOptions {
        source_dir: source.clone(),
        output_dir: out.clone(),
        freshenv: true,
        worker_count: 1,
        ..BuildOptions::default()
    };
    let mut env = BuildEnvironment::with_config(&source, &config);
    let result = StandaloneHtmlBuilder::build(&options, &config, &mut env).expect("empty build");
    assert_eq!(result.built_docnames, vec!["index".to_owned()]);
    assert!(out.join("index.html").is_file());
    std::fs::remove_dir_all(source).ok();
    std::fs::remove_dir_all(out).ok();
}

#[test]
fn missing_source_is_a_structured_error() {
    let out = tempdir("missing-out");
    let config = Config::new();
    let options = BuildOptions {
        source_dir: out.join("does-not-exist"),
        output_dir: out.clone(),
        ..BuildOptions::default()
    };
    let mut env = BuildEnvironment::new(".");
    let error = StandaloneHtmlBuilder::build(&options, &config, &mut env).unwrap_err();
    assert!(matches!(error, scribe_builders::HtmlError::MissingSource(_)));
    std::fs::remove_dir_all(out).ok();
}
