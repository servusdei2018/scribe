//! Differential and structural tests for the Phase 25 XML / pseudoxml builders.
//!
//! Both builders are compared byte-for-byte against the pinned oracle on
//! `test-basic` after the documented normalization (`source` path and the
//! Docutils generator version comment).

mod phase25_common;

use std::process::Command;

use phase25_common::{config, normalize_doctree, options, temp_dir, workspace_root};
use scribe_builders::{PseudoXmlBuilder, XmlBuilder};
use scribe_core::BuildEnvironment;

fn oracle(builder: &str, fixture: &std::path::Path, out: &std::path::Path) {
    let binary = workspace_root().join(".oracle-venv/bin/sphinx-build");
    let status = Command::new(binary)
        .args(["-b", builder, "-E", "-q"])
        .arg(fixture)
        .arg(out)
        .status()
        .expect("oracle must be executable");
    assert!(status.success(), "oracle {builder} build failed");
}

#[test]
fn xml_matches_oracle_on_test_basic() {
    let fixture = workspace_root().join("sphinx/tests/roots/test-basic");
    let oracle_out = temp_dir("oracle-xml");
    let scribe_out = temp_dir("scribe-xml");
    oracle("xml", &fixture, &oracle_out);

    let config = config(&fixture, &[]);
    let opts = options(&fixture, &scribe_out, "xml");
    let mut env = BuildEnvironment::with_config(&fixture, &config);
    let result = XmlBuilder::build(&opts, &config, &mut env).expect("xml build");
    assert_eq!(result.built_docnames, vec!["index".to_owned()]);

    let oracle_text = std::fs::read_to_string(oracle_out.join("index.xml")).expect("oracle xml");
    let scribe_text = std::fs::read_to_string(scribe_out.join("index.xml")).expect("scribe xml");
    assert_eq!(normalize_doctree(&oracle_text), normalize_doctree(&scribe_text));
}

#[test]
fn pseudoxml_matches_oracle_on_test_basic() {
    let fixture = workspace_root().join("sphinx/tests/roots/test-basic");
    let oracle_out = temp_dir("oracle-pseudo");
    let scribe_out = temp_dir("scribe-pseudo");
    oracle("pseudoxml", &fixture, &oracle_out);

    let config = config(&fixture, &[]);
    let opts = options(&fixture, &scribe_out, "pseudoxml");
    let mut env = BuildEnvironment::with_config(&fixture, &config);
    let result = PseudoXmlBuilder::build(&opts, &config, &mut env).expect("pseudoxml build");
    assert_eq!(result.built_docnames, vec!["index".to_owned()]);

    let oracle_text = std::fs::read_to_string(oracle_out.join("index.pseudoxml")).expect("oracle pseudoxml");
    let scribe_text = std::fs::read_to_string(scribe_out.join("index.pseudoxml")).expect("scribe pseudoxml");
    assert_eq!(normalize_doctree(&oracle_text), normalize_doctree(&scribe_text));
}

#[test]
fn xml_output_is_well_formed_and_has_doctype() {
    let fixture = workspace_root().join("sphinx/tests/roots/test-basic");
    let out = temp_dir("xml-wellformed");
    let config = config(&fixture, &[]);
    let opts = options(&fixture, &out, "xml");
    let mut env = BuildEnvironment::with_config(&fixture, &config);
    XmlBuilder::build(&opts, &config, &mut env).expect("xml build");
    let text = std::fs::read_to_string(out.join("index.xml")).expect("xml file");
    assert!(text.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?>"));
    assert!(text.contains("<!DOCTYPE document PUBLIC"));
    let options = roxmltree::ParsingOptions { allow_dtd: true, ..roxmltree::ParsingOptions::default() };
    roxmltree::Document::parse_with_options(&text, options).expect("well-formed XML");
}

#[test]
fn xml_pretty_false_emits_compact_tags() {
    let fixture = workspace_root().join("sphinx/tests/roots/test-basic");
    let out = temp_dir("xml-compact");
    let config = config(&fixture, &[("xml_pretty", "0")]);
    let opts = options(&fixture, &out, "xml");
    let mut env = BuildEnvironment::with_config(&fixture, &config);
    XmlBuilder::build(&opts, &config, &mut env).expect("xml build");
    let text = std::fs::read_to_string(out.join("index.xml")).expect("xml file");
    assert!(text.contains("<section"));
    assert!(text.contains("</section>"));
    assert!(text.contains("</document>"));
}
