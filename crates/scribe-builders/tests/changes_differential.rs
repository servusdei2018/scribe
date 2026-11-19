//! Differential tests for the Phase 25 changes builder.
//!
//! `test-changes` is built by both the pinned oracle and scribe; the generated
//! `changes.html`, `index.html` and `rst/*.html` files are compared byte for
//! byte (no normalization is required for this fixture).

mod phase25_common;

use std::process::Command;

use phase25_common::{config, options, temp_dir, workspace_root};
use scribe_builders::ChangesBuilder;
use scribe_core::BuildEnvironment;

fn oracle(fixture: &std::path::Path, out: &std::path::Path) {
    let binary = workspace_root().join(".oracle-venv/bin/sphinx-build");
    let status = Command::new(binary)
        .args(["-b", "changes", "-E", "-q"])
        .arg(fixture)
        .arg(out)
        .status()
        .expect("oracle must be executable");
    assert!(status.success(), "oracle changes build failed");
}

#[test]
fn changes_matches_oracle_on_test_changes() {
    let fixture = workspace_root().join("sphinx/tests/roots/test-changes");
    let oracle_out = temp_dir("oracle-changes");
    let scribe_out = temp_dir("scribe-changes");
    oracle(&fixture, &oracle_out);

    let config = config(&fixture, &[]);
    let opts = options(&fixture, &scribe_out, "changes");
    let mut env = BuildEnvironment::with_config(&fixture, &config);
    let result = ChangesBuilder::build(&opts, &config, &mut env).expect("changes build");
    assert_eq!(result.built_docnames.len(), 4);

    for file in ["changes.html", "index.html", "rst/base.html", "rst/c-api.html", "rst/library/utils.html"] {
        let expected = std::fs::read_to_string(oracle_out.join(file))
            .unwrap_or_else(|e| panic!("oracle missing {file}: {e}"));
        let actual = std::fs::read_to_string(scribe_out.join(file))
            .unwrap_or_else(|e| panic!("scribe missing {file}: {e}"));
        assert_eq!(expected, actual, "byte mismatch in {file}");
    }
}

#[test]
fn changes_html_groups_all_change_categories() {
    let fixture = workspace_root().join("sphinx/tests/roots/test-changes");
    let out = temp_dir("changes-structure");
    let config = config(&fixture, &[]);
    let opts = options(&fixture, &out, "changes");
    let mut env = BuildEnvironment::with_config(&fixture, &config);
    ChangesBuilder::build(&opts, &config, &mut env).expect("changes build");
    let html = std::fs::read_to_string(out.join("changes.html")).expect("changes.html");
    assert!(html.contains("<h2>Library changes</h2>"));
    assert!(html.contains("<h2>C API changes</h2>"));
    assert!(html.contains("<h2>Other changes</h2>"));
    assert!(html.contains("<b>Path</b>: <i>deprecated:</i>"));
    assert!(html.contains("<i>Added:</i> Added in version 0.6:"));
    // An empty `version-added` still produces a summary entry.
    assert!(html.contains("<i>Added:</i> Added in version 0.6."));
}

#[test]
fn no_changes_for_unrelated_version_writes_no_summary() {
    let fixture = workspace_root().join("sphinx/tests/roots/test-changes");
    let out = temp_dir("changes-empty");
    let config = config(&fixture, &[("version", "9.9")]);
    let opts = options(&fixture, &out, "changes");
    let mut env = BuildEnvironment::with_config(&fixture, &config);
    let result = ChangesBuilder::build(&opts, &config, &mut env).expect("changes build");
    assert!(result.written.is_empty());
    assert!(!out.join("changes.html").exists());
}
