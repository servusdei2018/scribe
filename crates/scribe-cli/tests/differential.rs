//! Differential test harness for the upstream Python Sphinx oracle.
//!
//! Phase 01 cannot compare scribe output yet (there is no parser), but the
//! harness mechanism is exercised end-to-end: locate the pinned oracle, run it
//! on a real upstream fixture, capture stdout/stderr and generated artifacts,
//! normalize paths, and compare against committed baseline markers. Later
//! phases will add the scribe side of the comparison.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const ORACLE_RELATIVE: &str = ".oracle-venv/bin/sphinx-build";
const FIXTURE_RELATIVE: &str = "sphinx/tests/roots/test-basic";
const MARKERS_RELATIVE: &str = "crates/scribe-cli/tests/fixtures/basic_pseudoxml.markers";

/// Workspace root, derived from the compile-time manifest directory.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn oracle_binary(root: &Path) -> PathBuf {
    root.join(ORACLE_RELATIVE)
}

fn require_oracle(root: &Path) -> PathBuf {
    let oracle = oracle_binary(root);
    assert!(
        oracle.is_file(),
        "Python Sphinx oracle not found at {} — install it before running differential tests",
        oracle.display()
    );
    oracle
}

/// Captured result of one oracle invocation.
#[derive(Debug)]
struct OracleCapture {
    status_code: i32,
    stdout: String,
    stderr: String,
    files: Vec<PathBuf>,
}

impl OracleCapture {
    /// Run `sphinx-build -b pseudoxml -E -q SOURCE OUTPUT`, capturing output.
    fn run(oracle: &Path, source: &Path, output: &Path) -> Self {
        let result = Command::new(oracle)
            .arg("-b")
            .arg("pseudoxml")
            .arg("-E")
            .arg("-q")
            .arg(source)
            .arg(output)
            .output()
            .unwrap_or_else(|error| panic!("failed to spawn {}: {error}", oracle.display()));

        let mut files = Vec::new();
        collect_files(output, &mut files);
        files.sort();

        Self {
            status_code: result.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&result.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&result.stderr).into_owned(),
            files,
        }
    }
}

fn collect_files(directory: &Path, accumulator: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, accumulator);
        } else {
            accumulator.push(path);
        }
    }
}

/// Replace the absolute source path so snapshots are machine-independent.
fn normalize(generated: &str, source: &Path) -> String {
    let source = source.to_string_lossy();
    generated.replace(source.as_ref(), "<SOURCE>")
}

fn temporary_output(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("scribe-oracle-{tag}-{}", std::process::id()))
}

#[test]
fn oracle_baseline_pseudoxml_matches_expected_markers() {
    let root = workspace_root();
    let oracle = require_oracle(&root);
    let source = root.join(FIXTURE_RELATIVE);
    assert!(
        source.is_dir(),
        "fixture missing at {}",
        source.display()
    );

    let output = temporary_output("baseline");
    let _ = fs::remove_dir_all(&output);
    fs::create_dir_all(&output).unwrap_or_else(|error| panic!("cannot create {}: {error}", output.display()));

    let capture = OracleCapture::run(&oracle, &source, &output);
    assert_eq!(
        capture.status_code, 0,
        "oracle exited with {}: stdout={} stderr={}",
        capture.status_code, capture.stdout, capture.stderr
    );
    assert!(
        capture
            .files
            .iter()
            .any(|path| path.ends_with("index.pseudoxml")),
        "oracle did not produce a pseudoxml doctree: {:?}",
        capture.files
    );

    let generated = fs::read_to_string(output.join("index.pseudoxml"))
        .unwrap_or_else(|error| panic!("cannot read pseudoxml output: {error}"));
    let normalized = normalize(&generated, &source);
    assert!(
        normalized.contains("<document"),
        "unexpected pseudoxml output:\n{normalized}"
    );

    let markers = fs::read_to_string(root.join(MARKERS_RELATIVE))
        .unwrap_or_else(|error| panic!("cannot read baseline markers: {error}"));
    for marker in markers
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        assert!(
            normalized.contains(marker),
            "baseline marker `{marker}` missing from oracle output:\n{normalized}"
        );
    }

    // Keep the capture fields observable so the harness scaffolding stays
    // meaningful as scribe output is added in later phases.
    assert!(
        capture.stdout.is_empty() || capture.stdout.contains("build"),
        "unexpected stdout: {}",
        capture.stdout
    );
    assert!(capture.stderr.is_empty(), "unexpected stderr: {}", capture.stderr);
}

#[test]
fn oracle_help_exposes_the_flags_we_mirror() {
    let root = workspace_root();
    let oracle = require_oracle(&root);
    let result = Command::new(&oracle)
        .arg("--help")
        .output()
        .unwrap_or_else(|error| panic!("failed to spawn {}: {error}", oracle.display()));
    assert!(result.status.success(), "sphinx-build --help failed");
    let help = String::from_utf8_lossy(&result.stdout);
    // `--keep-going` is intentionally absent: upstream registers it with
    // `help=argparse.SUPPRESS`, so it is accepted but never shown in --help.
    for flag in [
        "--builder",
        "--jobs",
        "--doctree-dir",
        "--conf-dir",
        "--define",
        "--html-define",
        "--warning-file",
        "--fail-on-warning",
    ] {
        assert!(help.contains(flag), "sphinx-build help missing `{flag}`");
    }
}
