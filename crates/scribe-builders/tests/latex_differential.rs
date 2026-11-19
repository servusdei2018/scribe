//! Differential tests for the Phase 23 LaTeX writer against the Sphinx oracle.
//!
//! Method (documented in `ARCHITECTURE.md` §29.7):
//!
//! 1. Run `.oracle-venv/bin/sphinx-build -b latex -E -q` on the upstream
//!    `test-latex-*` fixtures into a temporary directory.
//! 2. Run scribe's [`LatexBuilder`] on the same fixture into a second directory.
//! 3. Normalise both `.tex` files to a set of *structural markers* (canonical
//!    command/environment names) and assert that the marker sets relevant to
//!    each fixture are present in both. Byte equality is deliberately not
//!    attempted: upstream emits a much richer preamble (`sphinx.sty`,
//!    `fncychap`, Babel, xindy), while scribe emits a self-contained preamble,
//!    and the shared parser lowers some constructs differently.
//!
//! The oracle is optional: when the venv is missing the scribe-side structural
//! assertions still run.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use scribe_builders::latex::{BuildOptions, LatexBuilder};
use scribe_config::{load_config_with, BridgeStrategy, LoadRequest};
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
    let path = std::env::temp_dir().join(format!("scribe-latex-{label}-{}-{sequence}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create temp dir");
    path
}

fn oracle(fixture_name: &str) -> Option<PathBuf> {
    let binary = workspace().join(".oracle-venv/bin/sphinx-build");
    if !binary.is_file() { return None; }
    let out = tempdir("oracle");
    let result = Command::new(&binary)
        .arg("-b").arg("latex").arg("-E").arg("-q")
        .arg(fixture(fixture_name)).arg(&out)
        .output().expect("run oracle sphinx-build");
    if !result.status.success() {
        eprintln!("oracle latex build failed for {fixture_name}: {}", String::from_utf8_lossy(&result.stderr));
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
    let result = LatexBuilder::build(&options, &config, &mut env).expect("scribe latex build succeeded");
    assert!(!result.built_docnames.is_empty(), "no documents built for {fixture_name}");
    out
}

fn first_tex(dir: &Path) -> PathBuf {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("read output dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "tex"))
        .collect();
    paths.sort();
    paths.into_iter().next().expect("at least one .tex file was written")
}

/// Normalise a `.tex` file into a set of canonical structural markers.
///
/// Aliases are collapsed (e.g. `tabularx`/`longtable`/`tabularity` →
/// `TABLE_ENV`, `chapter`/`section` → `HEADING`) so the comparison focuses on
/// structure rather than package-level choices.
fn markers(path: &Path) -> BTreeSet<String> {
    let tex = std::fs::read_to_string(path).expect("read .tex");
    let mut out = BTreeSet::new();
    let add = |out: &mut BTreeSet<String>, needle: &str, marker: &str| {
        if tex.contains(needle) { out.insert(marker.to_owned()); }
    };
    add(&mut out, "\\documentclass", "DOCUMENTCLASS");
    add(&mut out, "\\usepackage", "PACKAGE");
    add(&mut out, "\\makeindex", "MAKEINDEX");
    add(&mut out, "\\begin{document}", "BEGIN_DOCUMENT");
    add(&mut out, "\\end{document}", "END_DOCUMENT");
    add(&mut out, "\\title{", "TITLE");
    add(&mut out, "\\author{", "AUTHOR");
    if tex.contains("\\maketitle") || tex.contains("\\sphinxmaketitle") { out.insert("MAKETITLE".to_owned()); }
    if tex.contains("\\tableofcontents") || tex.contains("\\sphinxtableofcontents") { out.insert("TOC".to_owned()); }
    add(&mut out, "\\printindex", "PRINTINDEX");
    if tex.contains("\\chapter{") || tex.contains("\\section{") { out.insert("HEADING".to_owned()); }
    add(&mut out, "\\label{", "LABEL");
    if tex.contains("\\begin{itemize}") { out.insert("ITEMIZE".to_owned()); }
    if tex.contains("\\begin{enumerate}") { out.insert("ENUMERATE".to_owned()); }
    if tex.contains("\\begin{sphinxVerbatim}") || tex.contains("\\begin{Verbatim}") || tex.contains("\\begin{verbatim}") {
        out.insert("VERBATIM".to_owned());
    }
    if tex.contains("\\begin{sphinxadmonition}") || tex.contains("\\begin{notice}") || tex.contains("admonition") {
        out.insert("ADMONITION".to_owned());
    }
    add(&mut out, "\\index{", "INDEX_ENTRY");
    if tex.contains("\\hyperref[") || tex.contains("\\href{") || tex.contains("\\ref{") {
        out.insert("REFERENCE".to_owned());
    }
    if tex.contains("\\[") || tex.contains("\\(") || tex.contains("\\begin{equation")
        || tex.contains("\\begin{align") || tex.contains("\\begin{split}") || tex.contains("$") {
        out.insert("MATH".to_owned());
    }
    if tex.contains("\\includegraphics") { out.insert("IMAGE".to_owned()); }
    if tex.contains("\\begin{tabularx}") || tex.contains("\\begin{longtable}") || tex.contains("\\begin{tabulary}") || tex.contains("\\begin{tabular}") {
        out.insert("TABLE_ENV".to_owned());
    }
    if tex.contains("\\item") { out.insert("ITEM".to_owned()); }
    out
}

fn assert_common(fixture_name: &str, required: &[&str]) {
    let ours = build_ours(fixture_name);
    let our_markers = markers(&first_tex(&ours));
    for marker in required {
        assert!(our_markers.contains(*marker), "scribe .tex for {fixture_name} missing marker {marker}; got {our_markers:?}");
    }

    if let Some(oracle_dir) = oracle(fixture_name) {
        let oracle_markers = markers(&first_tex(&oracle_dir));
        // The marker sets need not be equal (upstream emits a richer preamble);
        // every marker we require for this fixture must also appear upstream.
        for marker in required {
            assert!(oracle_markers.contains(*marker), "oracle .tex for {fixture_name} missing marker {marker}; got {oracle_markers:?}");
        }
    }
}

#[test]
fn basic_document_structure_matches_oracle() {
    // Note: `:index:` roles are lowered to inline text by the shared parser,
    // not to `Index` nodes, so `INDEX_ENTRY` is intentionally not required
    // here (see ARCHITECTURE.md §29.8). The translator's `\index` handling is
    // covered by `tests/latex_translator.rs`.
    assert_common("test-latex-index", &[
        "DOCUMENTCLASS", "BEGIN_DOCUMENT", "END_DOCUMENT", "MAKEINDEX",
        "TITLE", "AUTHOR", "MAKETITLE", "MATH", "PRINTINDEX",
    ]);
}

#[test]
fn unicode_document_structure_matches_oracle() {
    assert_common("test-latex-unicode", &[
        "DOCUMENTCLASS", "BEGIN_DOCUMENT", "END_DOCUMENT", "TITLE", "ITEMIZE", "ITEM",
    ]);
}

#[test]
fn table_document_structure_matches_oracle() {
    assert_common("test-latex-table", &[
        "DOCUMENTCLASS", "BEGIN_DOCUMENT", "END_DOCUMENT", "TABLE_ENV",
    ]);
}

#[test]
fn numfig_document_structure_matches_oracle() {
    assert_common("test-latex-numfig", &[
        "DOCUMENTCLASS", "BEGIN_DOCUMENT", "END_DOCUMENT", "HEADING", "LABEL",
    ]);
}

#[test]
fn equations_document_structure_matches_oracle() {
    assert_common("test-latex-equations", &[
        "DOCUMENTCLASS", "BEGIN_DOCUMENT", "END_DOCUMENT", "MATH",
    ]);
}

#[test]
fn generated_tex_is_deterministic() {
    let a = build_ours("test-latex-index");
    let b = build_ours("test-latex-index");
    let ta = std::fs::read_to_string(first_tex(&a)).expect("read a");
    let tb = std::fs::read_to_string(first_tex(&b)).expect("read b");
    assert_eq!(ta, tb, "LaTeX output must be deterministic across runs");
}

#[test]
fn support_files_and_makefiles_are_emitted() {
    let out = build_ours("test-latex-unicode");
    for name in ["Makefile", "make.bat", "latexmkrc", "sphinx.sty", "sphinxhowto.cls", "sphinxmanual.cls"] {
        assert!(out.join(name).is_file(), "missing support file {name}");
    }
}
