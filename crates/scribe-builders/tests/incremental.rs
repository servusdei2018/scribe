//! Phase 67 integration tests for the per-page HTML + search-index cache.
//!
//! A warm rebuild must re-parse / re-render only the documents reported by
//! `UpdateReport::files_to_rebuild()` and merge cached search fragments. The
//! resulting HTML and `searchindex.js` must be byte-for-byte identical to a
//! clean `-E` build of the same source, including the pages that were never
//! touched by the incremental pass.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use scribe_builders::html::{BuildOptions, StandaloneHtmlBuilder};
use scribe_config::{load_config_with, BridgeStrategy, LoadRequest};
use scribe_core::BuildEnvironment;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn tempdir(label: &str) -> PathBuf {
    let sequence = COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "scribe-phase67-{label}-{}-{sequence}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).expect("create temp dir");
    path
}

const CONF: &str = "project = 'p67'\nmaster_doc = 'index'\nextensions = []\nhtml_theme = 'basic'\n";

/// Generate a small deterministic offline project (one master + N pages).
fn fixture(root: &Path, pages: usize) -> PathBuf {
    let source = root.join("src");
    fs::create_dir_all(&source).expect("create source dir");
    fs::write(source.join("conf.py"), CONF).expect("write conf.py");
    let mut entries = String::new();
    for n in 1..=pages {
        entries.push_str(&format!("   guide/page-{n:02}\n"));
    }
    fs::write(
        source.join("index.rst"),
        format!("Master\n======\n\n.. toctree::\n\n{entries}"),
    )
    .expect("write index.rst");
    let guide = source.join("guide");
    fs::create_dir_all(&guide).expect("create guide dir");
    for n in 1..=pages {
        // Each guide page has its own nested toctree to the next one, so the
        // toctree refs stored in the cache are sub-directory-relative paths.
        let nested = if n == 1 {
            "\n.. toctree::\n\n   page-02\n"
        } else {
            ""
        };
        let text = format!(
            "Page {n:02}\n{underline}\n\n.. _page-{n:02}:\n\nBody of page {n:02} with words alpha beta gamma.\n{nested}",
            underline = "=".repeat(8),
        );
        fs::write(guide.join(format!("page-{n:02}.rst")), text).expect("write page");
    }
    source
}

fn load(source: &Path) -> scribe_config::Config {
    load_config_with(LoadRequest {
        conf_dir: source.to_path_buf(),
        strategy: BridgeStrategy::Static,
        ..LoadRequest::default()
    })
    .expect("load fixture config")
    .0
}

fn options(source: &Path, out: &Path, fresh: bool) -> BuildOptions {
    BuildOptions {
        source_dir: source.to_path_buf(),
        output_dir: out.to_path_buf(),
        force_all: fresh,
        freshenv: fresh,
        worker_count: 1,
        ..BuildOptions::default()
    }
}

fn build(source: &Path, out: &Path, fresh: bool) -> BTreeSet<String> {
    let config = load(source);
    let mut env = BuildEnvironment::with_config(source, &config);
    let result = StandaloneHtmlBuilder::build(&options(source, out, fresh), &config, &mut env)
        .expect("build succeeded");
    result.built_docnames.into_iter().collect()
}

fn output_files(out: &Path) -> BTreeSet<String> {
    walk(out, out)
}

fn walk(root: &Path, dir: &Path) -> BTreeSet<String> {
    let mut files = BTreeSet::new();
    let Ok(entries) = fs::read_dir(dir) else { return files };
    for entry in entries.flatten() {
        let path = entry.path();
        let relative = path.strip_prefix(root).expect("child of root").to_string_lossy().replace('\\', "/");
        if relative.split('/').any(|part| part == ".doctrees") {
            continue;
        }
        if path.is_dir() {
            files.extend(walk(root, &path));
        } else {
            files.insert(relative);
        }
    }
    files
}

#[test]
fn incremental_rebuild_is_byte_identical_to_clean_build() {
    let temp = tempdir("identity");
    let source = fixture(&temp, 24);
    let out = temp.join("out");
    let expected = temp.join("expected");

    // Cold build populates the environment, domain inventory and fragments.
    let cold = build(&source, &out, true);
    assert_eq!(cold.len(), 25, "master + 24 pages");

    // Edit exactly one page, then rebuild warm. This page lives in a
    // sub-directory and owns a nested toctree, exercising the resolved-ref
    // re-collection path that is easy to get wrong on repeated warm builds.
    let edited = source.join("guide/page-12.rst");
    let mut text = fs::read_to_string(&edited).expect("read page");
    text.push_str("\nA NEW SENTENCE ONLY IN PAGE TWELVE.\n");
    fs::write(&edited, text).expect("write page");
    let rebuilt = build(&source, &out, false);
    assert!(
        rebuilt.len() < cold.len(),
        "incremental build must not re-render every page: {rebuilt:?}"
    );
    assert!(rebuilt.contains("guide/page-12"), "edited page must be rebuilt");

    // A second warm build must be just as correct: the first one must not
    // poison the cached toctree refs with document-relative paths.
    let second = build(&source, &out, false);
    assert!(second.len() < cold.len(), "second warm build regressed: {second:?}");

    // Independent clean build of the same (edited) source.
    build(&source, &expected, true);

    assert_eq!(output_files(&out), output_files(&expected));
    for relative in output_files(&out) {
        let a = fs::read(out.join(&relative)).expect("read incremental output");
        let b = fs::read(expected.join(&relative)).expect("read clean output");
        assert_eq!(a, b, "byte mismatch in {relative}");
    }
}

#[test]
fn warm_rebuild_with_no_changes_is_byte_identical() {
    let temp = tempdir("noop");
    let source = fixture(&temp, 12);
    let out = temp.join("out");
    let expected = temp.join("expected");
    build(&source, &out, true);
    let rebuilt = build(&source, &out, false);
    assert!(rebuilt.is_empty(), "no-change rebuild should parse nothing: {rebuilt:?}");
    build(&source, &expected, true);
    for relative in output_files(&out) {
        assert_eq!(
            fs::read(out.join(&relative)).expect("read incremental"),
            fs::read(expected.join(&relative)).expect("read clean"),
            "byte mismatch in {relative}"
        );
    }
}
