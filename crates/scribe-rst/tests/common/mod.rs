//! Shared helpers for the Phase 04 block-parser tests.
//!
//! The differential helpers run the pinned Sphinx oracle with
//! `-b pseudoxml` on tiny generated projects and normalize away attributes
//! that Sphinx adds *after* parsing (post-transforms/writer defaults):
//!
//! * `document`: `source`, `translation_progress`, all `xmlns:*` declarations;
//! * `literal_block`: `force`, `language`, `linenos`;
//! * `doctest_block`: `classes="doctest"`.
//!
//! Everything else — node kinds, nesting, attribute names/values, text nodes —
//! is compared verbatim. Inline-only differences are avoided by construction:
//! the differential fixtures contain no emphasis, literals, roles or
//! substitutions, so Phase 04's raw `Text` leaves match the oracle exactly.
//! Definition-list classifiers and option groups became first-class nodes in
//! Phase 34; their exact upstream shapes are covered by `tests/node_parity.rs`.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use scribe_rst::{parse, ParseOutput};

/// Workspace root, derived from the test crate's manifest directory.
pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("workspace root exists")
}

/// Path to `sphinx-build` in the pinned oracle venv.
pub fn oracle_sphinx_build() -> PathBuf {
    workspace_root()
        .join(".oracle-venv")
        .join("bin")
        .join("sphinx-build")
}

/// Parses RST source, asserting there are no recoverable diagnostics.
pub fn parse_clean(source: &str) -> ParseOutput {
    let output = parse(source);
    assert!(
        !output.has_diagnostics(),
        "unexpected diagnostics: {:?}",
        output.diagnostics
    );
    output
}

/// Returns the pseudo-XML of a cleanly parsed source.
pub fn pseudo(source: &str) -> String {
    parse_clean(source)
        .document
        .to_pseudoxml()
        .expect("pseudo-XML emission")
}

/// Asserts that `needle` appears as a contiguous block of pseudo-XML lines.
pub fn assert_block(pseudo: &str, block: &str) {
    assert!(
        pseudo.contains(block),
        "missing block:\n{block}\n--- in ---\n{pseudo}"
    );
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Runs `sphinx-build -b pseudoxml` on `source` and returns `index.pseudoxml`.
pub fn oracle_pseudoxml(source: &str) -> String {
    let oracle = oracle_sphinx_build();
    assert!(
        oracle.is_file(),
        "oracle not found at {} — install the Sphinx venv before differential tests",
        oracle.display()
    );
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "scribe-rst-diff-{}-{}-{}",
        std::process::id(),
        unique,
        source.len()
    ));
    let src = root.join("src");
    let out = root.join("out");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&src).expect("create source dir");
    fs::write(
        src.join("conf.py"),
        "project = 'scribe diff'\nmaster_doc = 'index'\nextensions = []\n",
    )
    .expect("write conf.py");
    fs::write(src.join("index.rst"), source).expect("write index.rst");

    let result = Command::new(&oracle)
        .arg("-b")
        .arg("pseudoxml")
        .arg("-E")
        .arg("-q")
        .arg(&src)
        .arg(&out)
        .output()
        .expect("spawn sphinx-build");
    assert!(
        result.status.success(),
        "oracle failed ({}):\nstdout: {}\nstderr: {}",
        result.status,
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let generated = fs::read_to_string(out.join("index.pseudoxml"))
        .expect("oracle did not produce index.pseudoxml");
    let _ = fs::remove_dir_all(&root);
    normalize(&generated)
}

/// Strips Sphinx post-parse/writer attributes so the comparison targets the
/// block structure produced by the RST parser itself.
pub fn normalize(pseudoxml: &str) -> String {
    pseudoxml
        .lines()
        .map(|line| {
            let indent_len = line.len() - line.trim_start_matches(' ').len();
            let mut content = line[indent_len..].to_owned();
            if content.starts_with("<document") {
                return format!("{}<document>", " ".repeat(indent_len));
            }
            for attr in [
                "translation_progress",
                "xmlns:c",
                "xmlns:changeset",
                "xmlns:citation",
                "xmlns:cpp",
                "xmlns:index",
                "xmlns:js",
                "xmlns:math",
                "xmlns:py",
                "xmlns:rst",
                "xmlns:std",
                "force",
                "language",
                "linenos",
            ] {
                remove_attribute(&mut content, attr);
            }
            // `classes="doctest"` is added by the doctest post-transform.
            remove_attribute_value(&mut content, "classes", "doctest");
            format!("{}{}", " ".repeat(indent_len), content)
        })
        .map(|line| line.trim_end().to_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Removes ` name="..."` from a start tag, handling escaped quotes.
fn remove_attribute(tag: &mut String, name: &str) {
    let needle = format!(" {name}=\"");
    let Some(start) = tag.find(&needle) else {
        return;
    };
    let value_start = start + needle.len();
    let bytes = tag.as_bytes();
    let mut i = value_start;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == b'"' {
            tag.replace_range(start..=i, "");
            return;
        }
        i += 1;
    }
}

fn remove_attribute_value(tag: &mut String, name: &str, value: &str) {
    let needle = format!(" {name}=\"{value}\"");
    if let Some(start) = tag.find(&needle) {
        tag.replace_range(start..start + needle.len(), "");
    }
}

/// Normalizes our output with the same rules as the oracle output.
pub fn normalize_ours(pseudoxml: &str) -> String {
    normalize(pseudoxml)
}

/// Returns the path of a fixture under `sphinx/tests/roots/`, when present.
pub fn upstream_fixture(name: &str) -> PathBuf {
    workspace_root().join("sphinx").join("tests").join("roots").join(name)
}

/// Reads a file, panicking with a clear message if missing.
pub fn read_fixture(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}
