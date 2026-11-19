//! Phase 19 differential and unit tests for the alternate HTML builders.
//!
//! The oracle is optional at test time: when `.oracle-venv` is unavailable the
//! structural, layout and link-integrity assertions still run against scribe
//! output. Builds use `-E` (fresh environment) and the same fixtures as the
//! oracle so file sets and links are directly comparable.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use scribe_builders::dirhtml::{output_path, target_uri, DirectoryHTMLBuilder};
use scribe_builders::html::{BuildOptions, BuildResult};
use scribe_builders::metadata::{
    make_filename, make_filename_from_project, DevHelpBuilder, HtmlHelpBuilder, JsonBuilder,
    QtHelpBuilder,
};
use scribe_builders::singlehtml::{target_uri as single_target_uri, SingleFileHTMLBuilder};
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
    let path = std::env::temp_dir().join(format!(
        "scribe-phase19-{label}-{}-{sequence}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create temp dir");
    path
}

fn config_for(source: &Path) -> Config {
    load_config_with(LoadRequest {
        conf_dir: source.to_path_buf(),
        strategy: BridgeStrategy::Static,
        ..LoadRequest::default()
    })
    .expect("load fixture config")
    .0
}

fn builder_options(source: &Path, out: &Path, builder: &str) -> BuildOptions {
    BuildOptions {
        source_dir: source.to_path_buf(),
        output_dir: out.to_path_buf(),
        builder_name: builder.to_owned(),
        force_all: true,
        freshenv: true,
        worker_count: 4,
        ..BuildOptions::default()
    }
}

fn oracle_build(builder: &str, source: &Path, out: &Path) -> bool {
    let binary = workspace().join(".oracle-venv/bin/sphinx-build");
    if !binary.is_file() {
        return false;
    }
    Command::new(binary)
        .arg("-b").arg(builder).arg("-E").arg("-q")
        .arg(source).arg(out)
        .output()
        .map(|result| result.status.success())
        .unwrap_or(false)
}

fn build_scribe(
    builder: &str,
    source: &Path,
    out: &Path,
) -> BuildResult {
    let config = config_for(source);
    let options = builder_options(source, out, builder);
    let mut env = BuildEnvironment::with_config(source, &config);
    match builder {
        "dirhtml" => DirectoryHTMLBuilder::build(&options, &config, &mut env),
        "singlehtml" => SingleFileHTMLBuilder::build(&options, &config, &mut env),
        "json" => JsonBuilder::build(&options, &config, &mut env),
        "htmlhelp" => HtmlHelpBuilder::build(&options, &config, &mut env),
        "qthelp" => QtHelpBuilder::build(&options, &config, &mut env),
        "devhelp" => DevHelpBuilder::build(&options, &config, &mut env),
        _ => panic!("unknown builder in test helper"),
    }
    .expect("scribe build succeeded")
}

/// Relative paths of all files under `root` (excluding doctrees).
fn file_set(root: &Path) -> BTreeSet<String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeSet<String>) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name == ".doctrees" { continue; }
                if path.is_dir() { walk(root, &path, out); }
                else if let Ok(rel) = path.strip_prefix(root) {
                    out.insert(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
    }
    let mut out = BTreeSet::new();
    walk(root, root, &mut out);
    out
}

/// Verify every relative `href`/`src` in the given HTML files resolves to a
/// written file or a local id.
fn assert_links_resolve(root: &Path, pages: &[PathBuf]) {
    // Known upstream conventions that are intentionally not resolved to a
    // file/anchor in every builder: bare `#`, Sphinx's `#index` top-of-page
    // shorthand, and the relbar's index/search links on singlehtml (upstream
    // emits those links but writes no corresponding page).
    let allowed_missing = ["genindex.html", "py-modindex.html", "search.html"];
    for page in pages {
        let html = std::fs::read_to_string(page).expect("read page");
        let ids: BTreeSet<&str> = html.split("id=\"").skip(1)
            .filter_map(|tail| tail.split('"').next())
            .collect();
        for attribute in ["href=", "src="] {
            for piece in html.split(attribute).skip(1) {
                let raw = piece.split('"').nth(1).unwrap_or("");
                let clean = raw.split('?').next().unwrap_or("").split('#').next().unwrap_or("");
                if clean.is_empty()
                    || clean.starts_with('#')
                    || clean.contains("://")
                    || clean.starts_with("mailto:")
                    || clean.starts_with("data:")
                {
                    if let Some(anchor) = raw.strip_prefix('#') {
                        if !anchor.is_empty() && anchor != "index" {
                            assert!(ids.contains(anchor), "unresolved anchor #{anchor} in {}", page.display());
                        }
                    }
                    continue;
                }
                if allowed_missing.contains(&clean) {
                    continue;
                }
                let target = page.parent().expect("page parent").join(clean);
                assert!(
                    target.exists(),
                    "dangling link {clean:?} in {}",
                    page.display()
                );
            }
        }
    }
    let _ = root;
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[test]
fn dirhtml_target_uri_and_output_path_rules() {
    assert_eq!(target_uri("index"), "");
    assert_eq!(target_uri("foo/index"), "foo/");
    assert_eq!(target_uri("a/b/index"), "a/b/");
    assert_eq!(target_uri("bar"), "bar/");
    assert_eq!(target_uri("foo/bar"), "foo/bar/");

    assert_eq!(output_path("index", ".html"), PathBuf::from("index.html"));
    assert_eq!(output_path("bar", ".html"), PathBuf::from("bar/index.html"));
    assert_eq!(output_path("foo/index", ".html"), PathBuf::from("foo/index.html"));
    assert_eq!(output_path("foo/foo_1", ".html"), PathBuf::from("foo/foo_1/index.html"));
    assert_eq!(output_path("genindex", ".fjson"), PathBuf::from("genindex/index.fjson"));
}

#[test]
fn singlehtml_target_uri_uses_inpage_anchors() {
    assert_eq!(single_target_uri("foo", true), "#document-foo");
    assert_eq!(single_target_uri("index", true), "#document-index");
    assert_eq!(single_target_uri("genindex", false), "genindex.html");
}

#[test]
fn metadata_filename_helpers_match_upstream() {
    assert_eq!(make_filename("Project name not set"), "Projectnamenotset");
    assert_eq!(make_filename("!!!"), "sphinx");
    assert_eq!(make_filename_from_project("Project name not set"), "projectnamenotset");
    assert_eq!(make_filename_from_project("My Docs Documentation"), "mydocs");
}

#[test]
fn no_panics_on_single_page_without_toctree() {
    let source = tempdir("singlepage-src");
    let out = tempdir("singlepage-out");
    std::fs::write(source.join("index.rst"), "Only page\n=========\n\nHi.\n").expect("write");
    let result = build_scribe("singlehtml", &source, &out);
    assert_eq!(result.built_docnames, vec!["index".to_owned()]);
    assert!(out.join("index.html").is_file());
    assert_links_resolve(&out, &[out.join("index.html")]);
}

#[test]
fn no_panics_on_single_page_with_external_links() {
    let source = tempdir("extlink-src");
    let out = tempdir("extlink-out");
    std::fs::write(
        source.join("index.rst"),
        "Links\n=====\n\n`Sphinx <https://sphinx-doc.org>`_ and mailto:a@b.c.\n",
    )
    .expect("write");
    let result = build_scribe("dirhtml", &source, &out);
    assert_eq!(result.built_docnames, vec!["index".to_owned()]);
    assert!(out.join("index.html").is_file());
    assert_links_resolve(&out, &[out.join("index.html")]);
}

#[test]
fn singlehtml_renumbers_duplicate_section_anchors() {
    let source = tempdir("dup-src");
    let out = tempdir("dup-out");
    std::fs::write(source.join("index.rst"), "Root\n====\n\n.. toctree::\n\n   a\n   b\n").expect("write");
    std::fs::write(source.join("a.rst"), "A\n=\n\nsame\n----\ntext\n").expect("write");
    std::fs::write(source.join("b.rst"), "B\n=\n\nsame\n----\ntext\n").expect("write");
    build_scribe("singlehtml", &source, &out);

    let html = std::fs::read_to_string(out.join("index.html")).expect("read singlehtml");
    assert!(html.contains("id=\"same\""));
    // The second document's colliding id gets a distinct, resolvable anchor.
    assert!(html.contains("id=\"same-b\"") || html.contains("id=\"same-a\""));
    assert_links_resolve(&out, &[out.join("index.html")]);
}

// ---------------------------------------------------------------------------
// Differential: dirhtml
// ---------------------------------------------------------------------------

#[test]
fn dirhtml_layout_and_link_integrity_matches_oracle() {
    let source = fixture("test-builder-dirhtml");
    let ours = tempdir("dirhtml-ours");
    let result = build_scribe("dirhtml", &source, &ours);

    // Exact upstream output paths.
    for path in [
        "index.html", "bar/index.html", "foo/index.html",
        "foo/foo_1/index.html", "foo/foo_2/index.html",
        "genindex/index.html", "search/index.html",
    ] {
        assert!(ours.join(path).is_file(), "missing {path}");
    }
    let pages: Vec<PathBuf> = result.written.iter()
        .filter(|p| p.to_string_lossy().ends_with("index.html"))
        .map(|p| ours.join(p)).collect();
    assert_links_resolve(&ours, &pages);

    let theirs = tempdir("dirhtml-oracle");
    if oracle_build("dirhtml", &source, &theirs) {
        let ours_set = file_set(&ours);
        let theirs_set = file_set(&theirs);
        for path in [
            "index.html", "bar/index.html", "foo/index.html",
            "foo/foo_1/index.html", "foo/foo_2/index.html",
            "genindex/index.html", "search/index.html",
        ] {
            assert!(theirs_set.contains(path), "oracle missing {path}");
        }
        // Every page path upstream wrote must also exist in scribe. Scribe
        // additionally emits `py-modindex/index.html` (a Phase 17 convention
        // shared with the standalone builder), which is allowed.
        let oracle_pages: BTreeSet<String> = theirs_set.iter()
            .filter(|p| p.ends_with("index.html")).cloned().collect();
        for path in &oracle_pages {
            assert!(ours_set.contains(path), "scribe missing oracle page {path}");
        }
        // Clean-URL invariants in both engines.
        for engine in [&ours, &theirs] {
            let index = std::fs::read_to_string(engine.join("index.html")).expect("index");
            assert!(index.contains("href=\"bar/\""), "root toctree link");
            assert!(index.contains("href=\"foo/\""), "root toctree link");
            let nested = std::fs::read_to_string(engine.join("foo/index.html")).expect("foo index");
            assert!(nested.contains("href=\"foo_1/\""), "nested clean link");
            assert!(nested.contains("href=\"../\""), "parent-relative root link");
        }
    }
}

#[test]
fn dirhtml_test_basic_links_resolve() {
    let source = fixture("test-basic");
    let out = tempdir("dirhtml-basic");
    let result = build_scribe("dirhtml", &source, &out);
    let pages: Vec<PathBuf> = result.written.iter()
        .filter(|p| p.to_string_lossy().ends_with("index.html"))
        .map(|p| out.join(p)).collect();
    assert_links_resolve(&out, &pages);
}

// ---------------------------------------------------------------------------
// Differential: singlehtml
// ---------------------------------------------------------------------------

#[test]
fn singlehtml_inlines_toctree_and_rewrites_links() {
    let source = fixture("test-toctree");
    let out = tempdir("single-ours");
    let result = build_scribe("singlehtml", &source, &out);

    // Exactly one HTML file, named index.html.
    let html_files: Vec<_> = file_set(&out).into_iter()
        .filter(|p| p.ends_with(".html")).collect();
    assert_eq!(html_files, vec!["index.html".to_owned()]);
    assert!(out.join("index.html").is_file());
    assert!(!result.built_docnames.is_empty());

    let html = std::fs::read_to_string(out.join("index.html")).expect("read singlehtml");
    assert!(html.contains("id=\"document-foo\""), "inlined foo marker");
    assert!(html.contains("href=\"#document-foo\""), "rewritten foo link");
    assert!(html.contains("id=\"document-bar\""));
    // No cross-document page links for inlined docs remain.
    for doc in ["foo", "bar", "baz"] {
        assert!(!html.contains(&format!("href=\"{doc}.html\"")), "cross-file link to {doc}");
    }
    assert_links_resolve(&out, &[out.join("index.html")]);

    let theirs = tempdir("single-oracle");
    if oracle_build("singlehtml", &source, &theirs) {
        let oracle_html = std::fs::read_to_string(theirs.join("index.html")).expect("oracle singlehtml");
        for marker in ["id=\"document-foo\"", "id=\"document-bar\"", "id=\"document-baz\"",
                       "href=\"#document-foo\"", "href=\"#document-bar\""] {
            assert!(oracle_html.contains(marker), "oracle missing {marker}");
        }
        // Both engines emit a single page.
        assert_eq!(file_set(&theirs).into_iter().filter(|p| p.ends_with(".html")).count(), 1);
    }
}

// ---------------------------------------------------------------------------
// Differential: metadata builders
// ---------------------------------------------------------------------------

#[test]
fn json_builder_emits_fjson_context_and_globalcontext() {
    let source = fixture("test-basic");
    let out = tempdir("json-ours");
    build_scribe("json", &source, &out);

    assert!(out.join("index.fjson").is_file());
    assert!(out.join("globalcontext.json").is_file());
    assert!(out.join("searchindex.json").is_file());
    assert!(out.join("last_build").is_file());

    let page: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(out.join("index.fjson")).expect("read fjson"),
    ).expect("parse fjson");
    for key in ["title", "body", "current_page_name", "sourcename", "rellinks", "toc"] {
        assert!(page.get(key).is_some(), "missing fjson key {key}");
    }
    assert_eq!(page["current_page_name"], "index");

    let global: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(out.join("globalcontext.json")).expect("read globalcontext"),
    ).expect("parse globalcontext");
    assert!(global.get("project").is_some());
    assert!(global.get("master_doc").is_some());
    assert!(global.get("link_suffix").is_some());

    let theirs = tempdir("json-oracle");
    if oracle_build("json", &source, &theirs) {
        let theirs_set = file_set(&theirs);
        for name in ["index.fjson", "globalcontext.json", "searchindex.json", "last_build"] {
            assert!(theirs_set.contains(name), "oracle missing {name}");
        }
        let oracle_page: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(theirs.join("index.fjson")).expect("oracle fjson"),
        ).expect("parse oracle fjson");
        assert_eq!(page["title"], oracle_page["title"]);
        assert_eq!(page["current_page_name"], oracle_page["current_page_name"]);
        assert_eq!(page["sourcename"], oracle_page["sourcename"]);
    }
}

#[test]
fn htmlhelp_project_files_are_well_formed() {
    let source = fixture("test-basic");
    let out = tempdir("htmlhelp-ours");
    build_scribe("htmlhelp", &source, &out);

    let base = "projectnamenotsetdoc";
    let hhp = std::fs::read_to_string(out.join(format!("{base}.hhp"))).expect("hhp");
    assert!(hhp.contains("[OPTIONS]"));
    assert!(hhp.contains("Default topic=index.html"));
    assert!(hhp.contains("[FILES]"));
    assert!(hhp.contains("index.html"));
    assert!(hhp.contains("genindex.html"));
    let hhc = std::fs::read_to_string(out.join(format!("{base}.hhc"))).expect("hhc");
    assert!(hhc.contains("<OBJECT type=\"text/sitemap\">"));
    assert!(hhc.contains("index.html"));
    assert!(out.join(format!("{base}.hhk")).is_file());
    assert!(out.join(format!("{base}.stp")).is_file());

    let theirs = tempdir("htmlhelp-oracle");
    if oracle_build("htmlhelp", &source, &theirs) {
        for name in [format!("{base}.hhp"), format!("{base}.hhc"), format!("{base}.hhk"), format!("{base}.stp")] {
            assert!(theirs.join(&name).is_file(), "oracle missing {name}");
        }
        let oracle_hhp = std::fs::read_to_string(theirs.join(format!("{base}.hhp"))).expect("oracle hhp");
        assert_eq!(hhp.lines().next(), oracle_hhp.lines().next());
    }
}

#[test]
fn qthelp_project_files_are_well_formed() {
    let source = fixture("test-basic");
    let out = tempdir("qthelp-ours");
    build_scribe("qthelp", &source, &out);

    let qhp = std::fs::read_to_string(out.join("Projectnamenotset.qhp")).expect("qhp");
    assert!(qhp.contains("<QtHelpProject version=\"1.0\">"));
    assert!(qhp.contains("<namespace>org.sphinx.projectnamenotset.</namespace>")
        || qhp.contains("<namespace>org.sphinx.projectnamenotset</namespace>"));
    assert!(qhp.contains("<virtualFolder>doc</virtualFolder>"));
    assert!(qhp.contains("<toc>"));
    assert!(qhp.contains("<file>index.html</file>"));
    assert!(qhp.trim_end().ends_with("</QtHelpProject>"));
    let qhcp = std::fs::read_to_string(out.join("Projectnamenotset.qhcp")).expect("qhcp");
    assert!(qhcp.contains("qthelp://"));
    assert!(qhcp.contains("Projectnamenotset.qhp"));

    let theirs = tempdir("qthelp-oracle");
    if oracle_build("qthelp", &source, &theirs) {
        assert!(theirs.join("Projectnamenotset.qhp").is_file());
        assert!(theirs.join("Projectnamenotset.qhcp").is_file());
    }
}

#[test]
fn devhelp_project_file_is_valid_xml() {
    let source = fixture("test-basic");
    let out = tempdir("devhelp-ours");
    build_scribe("devhelp", &source, &out);

    let xml = std::fs::read_to_string(out.join("Projectnamenotset.devhelp2")).expect("devhelp2");
    assert!(xml.starts_with("<book "));
    assert!(xml.contains("<chapters"));
    assert!(xml.contains("link=\"index.html\""));
    assert!(xml.trim_end().ends_with("</book>"));
    // The oracle's gzipped variant is emitted too.
    assert!(out.join("Projectnamenotset.devhelp.gz").is_file());

    let theirs = tempdir("devhelp-oracle");
    if oracle_build("devhelp", &source, &theirs) {
        assert!(theirs.join("Projectnamenotset.devhelp.gz").is_file());
    }
}

#[test]
fn metadata_builders_are_deterministic() {
    let source = fixture("test-basic");
    for builder in ["json", "htmlhelp", "qthelp", "devhelp"] {
        let first = tempdir("det-first");
        let second = tempdir("det-second");
        build_scribe(builder, &source, &first);
        build_scribe(builder, &source, &second);
        assert_eq!(file_set(&first), file_set(&second), "file sets for {builder}");
        for path in file_set(&first) {
            let a = std::fs::read(first.join(&path)).expect("first file");
            let b = std::fs::read(second.join(&path)).expect("second file");
            assert_eq!(a, b, "non-deterministic {path} for {builder}");
        }
    }
}
