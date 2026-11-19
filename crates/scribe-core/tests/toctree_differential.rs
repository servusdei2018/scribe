//! Differential tests for the Phase 09 toctree collector.
//!
//! Method (documented in ARCHITECTURE.md §13):
//!
//! 1. Copy a real `test-toctree*` fixture into a process-unique temp dir.
//! 2. Run the pinned oracle with `sphinx-build -b html -E -q` and parse the
//!    authoritative `<link rel="prev|next" href="…">` navigation hints from
//!    every generated page (relative hrefs are resolved with Sphinx's
//!    `docname_join` semantics before comparison).
//! 3. additionally query the same Sphinx application through its Python API
//!    for `env.toctree_includes`, `env.toc_secnumbers` and
//!    `env.collect_relations()` — the exact data structures our collector
//!    derives. This avoids lossy scraping of the resolved ToC markup.
//! 4. Parse the same sources with `scribe-rst` (with the `html` tag enabled so
//!    `.. only:: html` toctrees survive Phase 06's eager tag evaluation), run
//!    `collect_toctrees()` and assert exact equality of edges, section numbers
//!    and navigation relations, plus the HTML prev/next links.
//!
//! Normalization: docnames are suffix-free `/`-separated on both sides;
//! section anchors are Docutils ids; `null` relation entries become `None`.
//! External URLs and `self` entries stay in `entries` but intentionally do not
//! create hierarchy edges (matching upstream `note_toctree()`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use scribe_config::{load_config_with, BridgeStrategy, LoadRequest, Tags};
use scribe_core::{collect_toctrees, BuildEnvironment, NavRelation, ToctreeCollection};
use scribe_rst::{docname_join, parse_with_options, resolve_output, Context, DirectiveRegistry, ParseOptions};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Project {
    root: PathBuf,
    source: PathBuf,
    output: PathBuf,
}

impl Project {
    fn new(fixture: &str) -> Self {
        let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "scribe-toc-diff-{fixture}-{}-{sequence}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let source = root.join("src");
        let output = root.join("out");
        std::fs::create_dir_all(&source).expect("create source dir");
        copy_tree(&workspace().join("sphinx/tests/roots").join(fixture), &source);
        Self { root, source, output }
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn copy_tree(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).expect("read fixture dir") {
        let entry = entry.expect("dir entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            std::fs::create_dir_all(&target).expect("create dir");
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

fn oracle_binary(name: &str) -> PathBuf {
    let path = workspace().join(".oracle-venv/bin").join(name);
    assert!(path.is_file(), "oracle binary not found: {}", path.display());
    path
}

fn run_oracle_html(project: &Project) {
    let output = Command::new(oracle_binary("sphinx-build"))
        .arg("-b")
        .arg("html")
        .arg("-E")
        .arg("-q")
        .arg(&project.source)
        .arg(project.output.join("html"))
        .output()
        .expect("run oracle sphinx-build");
    assert!(
        output.status.success(),
        "oracle html build failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Query the authoritative environment data structures from the pinned Sphinx.
fn oracle_env(project: &Project) -> serde_json::Value {
    let script = project.root.join("oracle_env.py");
    std::fs::write(
        &script,
        r#"
import json, sys
from sphinx.application import Sphinx

src, out = sys.argv[1], sys.argv[2]
app = Sphinx(srcdir=src, confdir=src, outdir=out, doctreedir=out + '/doctrees',
             buildername='html', status=None, warning=None, freshenv=True)
app.build(force_all=True)
env = app.env
secnumbers = {
    doc: {anchor: list(number) for anchor, number in anchors.items()}
    for doc, anchors in env.toc_secnumbers.items()
}
relations = {
    doc: [parent, prev, next] for doc, (parent, prev, next) in env.collect_relations().items()
}
print(json.dumps({
    'toctree_includes': env.toctree_includes,
    'toc_secnumbers': secnumbers,
    'relations': relations,
}))
"#,
    )
    .expect("write oracle env script");
    let output = Command::new(oracle_binary("python"))
        .arg(&script)
        .arg(&project.source)
        .arg(project.output.join("api"))
        .output()
        .expect("run oracle env script");
    assert!(
        output.status.success(),
        "oracle env query failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("oracle env JSON is UTF-8");
    serde_json::from_str(stdout.trim().lines().last().unwrap_or("{}")).expect("parse oracle env JSON")
}

fn html_text(project: &Project, docname: &str) -> String {
    std::fs::read_to_string(project.output.join("html").join(format!("{docname}.html")))
        .unwrap_or_default()
}

/// Parse `<link rel="prev|next" href="X.html">` from generated pages.
fn oracle_html_relations(
    project: &Project,
    docnames: &BTreeSet<String>,
) -> BTreeMap<String, (Option<String>, Option<String>)> {
    let mut result = BTreeMap::new();
    for docname in docnames {
        let html = html_text(project, docname);
        let mut prev = None;
        let mut next = None;
        for fragment in html.split("<link rel=").skip(1) {
            if !fragment.starts_with("\"prev\"") && !fragment.starts_with("\"next\"") {
                continue;
            }
            let Some(href_start) = fragment.find("href=\"") else {
                continue;
            };
            let rest = &fragment[href_start + 6..];
            let Some(href_end) = rest.find('"') else {
                continue;
            };
            let href = &rest[..href_end];
            let relative = href.strip_suffix(".html").unwrap_or(href);
            let link = if relative.contains("://") {
                relative.to_owned()
            } else {
                docname_join(docname, relative)
            };
            if fragment.starts_with("\"prev\"") {
                prev = Some(link);
            } else {
                next = Some(link);
            }
        }
        result.insert(docname.clone(), (prev, next));
    }
    result
}

fn build_ours(project: &Project) -> (BuildEnvironment, ToctreeCollection, BTreeSet<String>) {
    let request = LoadRequest {
        conf_dir: project.source.clone(),
        strategy: BridgeStrategy::Static,
        ..LoadRequest::default()
    };
    let config = load_config_with(request).expect("load fixture config").0;
    let mut env = BuildEnvironment::new(&project.source);
    env.update_defaults(&config, &project.source).expect("update env");

    let discovered: Vec<(String, String)> = env
        .found_docs_sorted()
        .iter()
        .map(|(name, path)| {
            let text = std::fs::read_to_string(path).expect("read source");
            ((*name).to_owned(), text)
        })
        .collect();
    for (name, text) in &discovered {
        let mut output = parse_with_options(text, ParseOptions::new(Some(name.clone())));
        let mut context = Context::new(&project.source, Tags::with_tags(["html".to_owned()]));
        context.config = Some(Arc::new(config.clone()));
        context.source_name = Some(name.clone());
        let report = resolve_output(&mut output, &DirectiveRegistry::builtin(), &context);
        assert!(report.is_clean(), "{name}: resolve issues: {:?}", report.unresolved);
        env.record_document(name, &output.document, BTreeMap::new());
    }
    let collection = collect_toctrees(&mut env, &config);
    let names: BTreeSet<String> = discovered.into_iter().map(|(name, _)| name).collect();
    (env, collection, names)
}

fn compare_fixture(fixture: &str) {
    let project = Project::new(fixture);
    run_oracle_html(&project);
    let oracle = oracle_env(&project);
    let (env, collection, names) = build_ours(&project);

    let expected_edges: BTreeMap<String, Vec<String>> =
        serde_json::from_value(oracle["toctree_includes"].clone()).expect("decode oracle edges");
    assert_eq!(
        env.toctree_includes, expected_edges,
        "toctree_includes mismatch for {fixture}"
    );

    let expected_numbers: BTreeMap<String, BTreeMap<String, Vec<i64>>> =
        serde_json::from_value(oracle["toc_secnumbers"].clone()).expect("decode oracle secnumbers");
    assert_eq!(
        collection.section_numbers.by_doc, expected_numbers,
        "section numbering mismatch for {fixture}"
    );

    let expected_relations: BTreeMap<String, Vec<Option<String>>> =
        serde_json::from_value(oracle["relations"].clone()).expect("decode oracle relations");
    for (docname, relation) in &expected_relations {
        let parent = relation.first().and_then(Option::as_deref).map(str::to_owned);
        let prev = relation.get(1).and_then(Option::as_deref).map(str::to_owned);
        let next = relation.get(2).and_then(Option::as_deref).map(str::to_owned);
        let actual = collection.relations.get(docname).unwrap_or_else(|| {
            panic!("missing navigation relation for {docname} in {fixture}")
        });
        assert_eq!(
            actual,
            &NavRelation {
                parent: parent.clone(),
                prev: prev.clone(),
                next: next.clone(),
            },
            "navigation relation mismatch for {docname} in {fixture}"
        );
    }

    // Independent check against `sphinx-build -b html` output links.
    let html_relations = oracle_html_relations(&project, &names);
    for (docname, (prev, next)) in &html_relations {
        if prev.is_none() && next.is_none() {
            assert!(
                !collection.relations.contains_key(docname),
                "unexpected relation for unreachable doc {docname}"
            );
            continue;
        }
        let relation = collection.relations.get(docname).expect("relation exists");
        assert_eq!(relation.prev, *prev, "HTML prev mismatch for {docname}");
        assert_eq!(relation.next, *next, "HTML next mismatch for {docname}");
    }
}

#[test]
fn differential_basic_toctree_hierarchy_and_navigation() {
    compare_fixture("test-toctree");
}

#[test]
fn differential_glob_toctree_hierarchy_and_navigation() {
    compare_fixture("test-toctree-glob");
}

#[test]
fn differential_maxdepth_toctree_hierarchy_and_navigation() {
    compare_fixture("test-toctree-maxdepth");
}

#[test]
fn differential_multiple_parents_hierarchy_and_navigation() {
    compare_fixture("test-toctree-multiple-parents");
}
