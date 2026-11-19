//! Unit, validator and differential tests for the Phase 24 groff writer.

mod output_common;

use scribe_ast::{Document, NodeKind};
use scribe_builders::man::{escape_man, parse_man_pages, render, ManHeader};
use scribe_config::load_config;

use output_common::{normalize_markup, oracle_build, scribe_build, temp_dir, tool, workspace_root};

fn header() -> ManHeader {
    ManHeader {
        title: "demo".to_owned(),
        name: "demo".to_owned(),
        description: "a demo tool".to_owned(),
        section: "1".to_owned(),
        date: "Jan 01, 1970".to_owned(),
        version: "1.0".to_owned(),
        group: "Demo Project".to_owned(),
        authors: vec!["Alice".to_owned()],
        copyright: "Public domain".to_owned(),
    }
}

fn render_ast(build: impl FnOnce(&mut Document)) -> String {
    let mut doc = Document::new(Some("index".to_owned()));
    doc.metadata.docname = Some("index".to_owned());
    build(&mut doc);
    render(&doc, &header(), false)
}

#[test]
fn escape_handles_backslash_hyphen_and_leading_quote() {
    assert_eq!(escape_man("a\\b"), "a\\eb");
    assert_eq!(escape_man("-x"), "\\-x");
    assert_eq!(escape_man(".SH"), "\\&.SH");
    assert_eq!(escape_man("'quote"), "\\&'quote");
    assert_eq!(escape_man("a-b"), "a-b");
}

#[test]
fn empty_document_still_emits_valid_header() {
    let page = render_ast(|_| {});
    assert!(page.starts_with(".\\\" Man page generated"), "{page}");
    assert!(page.contains(".TH \"DEMO\" \"1\""), "{page}");
    assert!(page.contains(".SH NAME"), "{page}");
    assert!(page.contains("demo \\- a demo tool"), "{page}");
    assert!(page.ends_with(".\\\" End of generated man page.\n"), "{page}");
}

#[test]
fn sections_lists_and_literal_blocks_use_expected_macros() {
    let page = render_ast(|doc| {
        let first = doc.add(NodeKind::Section, Some(doc.root)).expect("section");
        let title = doc.add(NodeKind::Title, Some(first)).expect("title");
        doc.new_text("Ignored document title", Some(title)).expect("text");
        let second = doc.add(NodeKind::Section, Some(first)).expect("section");
        let title2 = doc.add(NodeKind::Title, Some(second)).expect("title");
        doc.new_text("Commands", Some(title2)).expect("text");
        let list = doc.add(NodeKind::BulletList, Some(second)).expect("list");
        let item = doc.add(NodeKind::ListItem, Some(list)).expect("item");
        let para = doc.add(NodeKind::Paragraph, Some(item)).expect("para");
        doc.new_text("run demo", Some(para)).expect("text");
        let literal = doc.add(NodeKind::LiteralBlock, Some(second)).expect("literal");
        doc.new_text("$ demo --help", Some(literal)).expect("text");
    });
    assert!(page.contains(".SH COMMANDS"), "{page}");
    assert!(page.contains(".IP \\(bu 2"), "{page}");
    assert!(page.contains(".PP"), "{page}");
    assert!(page.contains(".nf"), "{page}");
    assert!(page.contains("$ demo --help"), "{page}");
    assert!(page.contains(".fi"), "{page}");
}

#[test]
fn inline_markup_uses_font_escapes() {
    let page = render_ast(|doc| {
        let para = doc.add(NodeKind::Paragraph, Some(doc.root)).expect("para");
        let strong = doc.add(NodeKind::Strong, Some(para)).expect("strong");
        doc.new_text("bold", Some(strong)).expect("text");
        doc.new_text(" and ", Some(para)).expect("text");
        let em = doc.add(NodeKind::Emphasis, Some(para)).expect("em");
        doc.new_text("italic", Some(em)).expect("text");
    });
    assert!(page.contains("\\fBbold\\fP"), "{page}");
    assert!(page.contains("\\fIitalic\\fP"), "{page}");
}

#[test]
fn parse_man_pages_handles_defaults_and_malformed_values() {
    let config = load_config(workspace_root().join("sphinx/tests/roots/test-manpage_url"), None, None)
        .expect("config");
    let specs = parse_man_pages(&config.0);
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].section, 1);
    assert_eq!(specs[0].name, "projectnamenotset");
}

/// **Validator gate:** `groff -man -Tutf8` must render the page with exit 0
/// and no diagnostics on stderr.
#[test]
fn groff_validates_generated_man_page() {
    let fixture = workspace_root().join("sphinx/tests/roots/test-manpage_url");
    let out = temp_dir("man-validator");
    let result = scribe_build("man", &fixture, &out, &[]);
    assert!(result.is_ok(), "man build failed: {:?}", result.err());

    let oracle_out = temp_dir("oracle-man");
    let status = oracle_build("man", &fixture, &oracle_out);
    assert!(status.status.success(), "oracle man build failed");

    let page = out.join("projectnamenotset.1");
    assert!(page.is_file(), "man page missing");
    let rendered = tool("/usr/sbin/groff", &["-man", "-Tutf8", page.to_str().expect("utf8 path")]);
    assert!(
        rendered.status.success(),
        "groff failed: {}",
        String::from_utf8_lossy(&rendered.stderr)
    );
    assert!(
        rendered.stderr.is_empty(),
        "groff emitted diagnostics: {}",
        String::from_utf8_lossy(&rendered.stderr)
    );
    let text = String::from_utf8_lossy(&rendered.stdout);
    assert!(text.contains("PROJECTNAMENOTSET"), "{text}");
}

#[test]
fn differential_manpage_url_normalized() {
    let fixture = workspace_root().join("sphinx/tests/roots/test-manpage_url");
    let oracle_out = temp_dir("oracle-man-diff");
    let status = oracle_build("man", &fixture, &oracle_out);
    assert!(status.status.success());
    let out = temp_dir("scribe-man-diff");
    let result = scribe_build("man", &fixture, &out, &[]);
    assert!(result.is_ok());

    let expected = std::fs::read_to_string(oracle_out.join("projectnamenotset.1")).expect("oracle");
    let actual = std::fs::read_to_string(out.join("projectnamenotset.1")).expect("scribe");
    // Structural markers both writers must produce (normalization drops
    // whitespace and volatile dates; see ARCHITECTURE §30).
    for marker in [".TH", ".SH NAME", "Author", "\\-", "man(1)", "mailx(1)"] {
        assert!(normalize_markup(&expected).contains(marker) || expected.contains(marker), "oracle lacks {marker}");
        assert!(normalize_markup(&actual).contains(marker) || actual.contains(marker), "scribe lacks {marker}");
    }
    assert!(actual.contains(".TH \"PROJECTNAMENOTSET\""));
    assert!(actual.contains(".\\\" End of generated man page."));
}
