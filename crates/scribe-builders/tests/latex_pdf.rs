//! PDF compilation and robustness tests for the Phase 23 LaTeX builder.
//!
//! The LaTeX toolchain is available in the development environment; these
//! tests genuinely run `latexmk -pdf` on generated `.tex` files and assert a
//! PDF is produced. When the toolchain is absent the compilation test is
//! skipped (not failed), matching the builder's own graceful degradation.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use scribe_builders::latex::{BuildOptions, LatexBuilder};
use scribe_config::{load_config_with, BridgeStrategy, Config, LoadRequest};
use scribe_core::BuildEnvironment;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("workspace root")
}

fn tempdir(label: &str) -> PathBuf {
    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("scribe-pdf-{label}-{}-{sequence}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create temp dir");
    path
}

fn toolchain_available() -> bool {
    Command::new("latexmk").arg("-version").output().is_ok()
}

fn build_fixture(fixture_name: &str) -> PathBuf {
    let source = workspace().join("sphinx/tests/roots").join(fixture_name);
    let out = tempdir("build");
    let (config, _) = load_config_with(LoadRequest {
        conf_dir: source.clone(),
        strategy: BridgeStrategy::Static,
        ..LoadRequest::default()
    }).expect("load fixture config");
    let options = BuildOptions {
        source_dir: source.clone(),
        output_dir: out.clone(),
        builder_name: "latex".to_owned(),
        force_all: true,
        freshenv: true,
        ..BuildOptions::default()
    };
    let mut env = BuildEnvironment::with_config(&source, &config);
    let result = LatexBuilder::build(&options, &config, &mut env).expect("build succeeded");
    assert!(!result.built_docnames.is_empty());
    out
}

fn first_tex(dir: &Path) -> PathBuf {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir).expect("read dir")
        .filter_map(|e| e.ok()).map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "tex")).collect();
    paths.sort();
    paths.into_iter().next().expect("a .tex file")
}

#[test]
fn generated_tex_compiles_to_pdf_with_latexmk() {
    if !toolchain_available() {
        eprintln!("latexmk unavailable; skipping PDF compilation test");
        return;
    }
    let out = build_fixture("test-latex-unicode");
    let tex = first_tex(&out);
    let pdf = LatexBuilder::compile_pdf(&tex, scribe_builders::LatexEngine::PdfLatex)
        .expect("latexmk compiled the generated .tex");
    assert!(pdf.is_file(), "PDF was not produced at {}", pdf.display());
    let meta = std::fs::metadata(&pdf).expect("PDF metadata");
    assert!(meta.len() > 1000, "PDF looks empty ({} bytes)", meta.len());
    let header = std::fs::read(&pdf).expect("read PDF");
    assert!(header.starts_with(b"%PDF"), "output is not a PDF");
}

#[test]
fn pdf_builder_invokes_latexmk_and_records_pdf() {
    if !toolchain_available() {
        eprintln!("latexmk unavailable; skipping PDF builder test");
        return;
    }
    let source = workspace().join("sphinx/tests/roots/test-latex-unicode");
    let out = tempdir("pdf-builder");
    let (config, _) = load_config_with(LoadRequest {
        conf_dir: source.clone(),
        strategy: BridgeStrategy::Static,
        ..LoadRequest::default()
    }).expect("load config");
    let options = BuildOptions {
        source_dir: source.clone(),
        output_dir: out.clone(),
        builder_name: "pdf".to_owned(),
        force_all: true,
        freshenv: true,
        ..BuildOptions::default()
    };
    let mut env = BuildEnvironment::with_config(&source, &config);
    let result = LatexBuilder::build(&options, &config, &mut env).expect("pdf build succeeded");
    assert!(result.written.iter().any(|p| p.extension().is_some_and(|e| e == "pdf")),
        "pdf builder did not record a PDF: {:?}", result.written);
    assert!(out.join("projectnamenotset.pdf").is_file(), "PDF missing in output dir");
}

#[test]
fn missing_source_is_a_typed_error() {
    let out = tempdir("missing-src");
    let source = out.join("does-not-exist");
    let options = BuildOptions {
        source_dir: source.clone(),
        output_dir: out.join("out"),
        ..BuildOptions::default()
    };
    let error = LatexBuilder::build(&options, &Config::new(), &mut BuildEnvironment::new(&source)).unwrap_err();
    assert!(matches!(error, scribe_builders::LatexError::MissingSource(_)), "{error}");
}
