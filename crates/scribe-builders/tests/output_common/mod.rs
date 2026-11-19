//! Shared helpers for the Phase 24 builder tests: oracle builds, scribe builds,
//! external validator invocation and documented output normalization.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

use scribe_builders::html::BuildOptions;
use scribe_config::load_config;
use scribe_core::BuildEnvironment;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Workspace root derived from the test crate's manifest location.
#[must_use]
pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// A process-unique temporary directory (never removed automatically).
#[must_use]
pub fn temp_dir(label: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("scribe-phase24-{label}-{}-{id}", std::process::id()));
    let _ = std::fs::create_dir_all(&path);
    path
}

/// Path to the pinned Sphinx oracle.
#[must_use]
pub fn oracle() -> PathBuf {
    workspace_root().join(".oracle-venv/bin/sphinx-build")
}

/// Run `sphinx-build -b <builder> -E -q <fixture> <out>`.
pub fn oracle_build(builder: &str, fixture: &Path, out: &Path) -> Output {
    Command::new(oracle())
        .arg("-b")
        .arg(builder)
        .args(["-E", "-q"])
        .arg(fixture)
        .arg(out)
        .output()
        .expect("oracle sphinx-build must be executable")
}

fn build_options(fixture: &Path, out: &Path, builder: &str) -> BuildOptions {
    BuildOptions {
        source_dir: fixture.to_path_buf(),
        output_dir: out.to_path_buf(),
        builder_name: builder.to_owned(),
        freshenv: true,
        worker_count: 2,
        ..BuildOptions::default()
    }
}

/// Run one of scribe's Phase 24 builders with optional `-D` style overrides.
pub fn scribe_build(
    builder: &str,
    fixture: &Path,
    out: &Path,
    overrides: &[(&str, &str)],
) -> scribe_builders::OutputResult<scribe_builders::html::BuildResult> {
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    for (key, value) in overrides {
        map.insert((*key).to_owned(), (*value).to_owned());
    }
    let (config, _) = load_config(fixture, Some(&map), None).expect("fixture configuration must load");
    let options = build_options(fixture, out, builder);
    let mut env = BuildEnvironment::with_config(fixture, &config);
    match builder {
        "epub" | "epub3" => scribe_builders::EpubBuilder::build(&options, &config, &mut env),
        "man" => scribe_builders::ManBuilder::build(&options, &config, &mut env),
        "texinfo" => scribe_builders::TexinfoBuilder::build(&options, &config, &mut env),
        "text" => scribe_builders::TextBuilder::build(&options, &config, &mut env),
        other => Err(scribe_builders::OutputError::config("tests", "builder", other)),
    }
}

/// Run an external validator, returning its `Output`.
pub fn tool(program: &str, args: &[&str]) -> Output {
    Command::new(program)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("validator {program} must be installed: {error}"))
}

/// Normalize plain text for structural differential comparison.
///
/// Normalization rules (documented in `ARCHITECTURE.md` §30):
/// * `\r\n` → `\n`,
/// * runs of whitespace (including line breaks and indentation) → one space,
/// * drop standalone box-drawing / underline-only lines (their exact glyph
///   width is a layout detail, not content),
/// * trim the result.
///
/// This compares the *content token stream* rather than byte-exact layout,
/// which is required because earlier phases' toctree/section-number rendering
/// differs from Sphinx's fully resolved writer input.
#[must_use]
pub fn normalize_text(input: &str) -> String {
    let unified = input.replace("\r\n", "\n").replace('\r', "\n");
    let mut kept: Vec<String> = Vec::new();
    for line in unified.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.chars().all(|c| c == '-' || c == '=' || c == '*' || c == '+' || c == '|' || c == '┌' || c == '─' || c == '┬' || c == '┐' || c == '│') {
            continue;
        }
        kept.push(trimmed.to_owned());
    }
    kept.join(" ").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Normalize structural markup for differential comparison: strip XML
/// declaration/doctype, collapse whitespace and drop volatile date/version
/// metadata values.
#[must_use]
pub fn normalize_markup(input: &str) -> String {
    input
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("<?xml") && !line.starts_with("<!DOCTYPE"))
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
