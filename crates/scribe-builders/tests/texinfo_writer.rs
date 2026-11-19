//! Unit, validator and differential tests for the Phase 24 Texinfo writer.

mod output_common;

use scribe_ast::{Document, NodeKind};
use scribe_builders::texinfo::{escape_texinfo, parse_texinfo_documents, render, TexinfoHeader};
use scribe_config::load_config;

use output_common::{oracle_build, scribe_build, temp_dir, tool, workspace_root};

fn header() -> TexinfoHeader {
    TexinfoHeader {
        filename: "demo".to_owned(),
        title: "Demo Manual".to_owned(),
        author: "Alice".to_owned(),
        project_line: "Demo 1.0".to_owned(),
        copyright: "Public domain".to_owned(),
        category: "Miscellaneous".to_owned(),
        direntry: "demo".to_owned(),
        description: "a demo".to_owned(),
        date: "Jan 01, 1970".to_owned(),
        generator: "scribe 0.1.0".to_owned(),
    }
}

fn render_docs(docs: &[&Document]) -> String {
    render(docs, &header())
}

fn empty_doc(name: &str) -> Document {
    let mut doc = Document::new(Some(name.to_owned()));
    doc.metadata.docname = Some(name.to_owned());
    doc
}

#[test]
fn escape_texinfo_special_chars() {
    assert_eq!(escape_texinfo("a@b"), "a@@b");
    assert_eq!(escape_texinfo("{x}"), "@{x@}");
    assert_eq!(escape_texinfo("plain"), "plain");
}

#[test]
fn header_contains_required_texinfo_commands() {
    let page = render_docs(&[&empty_doc("index")]);
    for marker in [
        "\\input texinfo",
        "@setfilename demo.info",
        "@settitle Demo Manual",
        "@titlepage",
        "@title Demo Manual",
        "@insertcopying",
        "@contents",
        "@node Top",
        "@top Demo Manual",
        "@bye",
    ] {
        assert!(page.contains(marker), "missing {marker}");
    }
}

#[test]
fn sections_nodes_menus_and_block_commands() {
    let mut doc = empty_doc("index");
    let section = doc.add(NodeKind::Section, Some(doc.root)).expect("section");
    let title = doc.add(NodeKind::Title, Some(section)).expect("title");
    doc.new_text("First Chapter", Some(title)).expect("text");
    let sub = doc.add(NodeKind::Section, Some(section)).expect("subsection");
    let sub_title = doc.add(NodeKind::Title, Some(sub)).expect("title");
    doc.new_text("Details", Some(sub_title)).expect("text");
    let literal = doc.add(NodeKind::LiteralBlock, Some(sub)).expect("literal");
    doc.new_text("code line", Some(literal)).expect("text");
    let list = doc.add(NodeKind::BulletList, Some(sub)).expect("list");
    let item = doc.add(NodeKind::ListItem, Some(list)).expect("item");
    let para = doc.add(NodeKind::Paragraph, Some(item)).expect("para");
    doc.new_text("item text", Some(para)).expect("text");
    let strong = doc.add(NodeKind::Strong, Some(sub)).expect("strong");
    doc.new_text("bold", Some(strong)).expect("text");

    let page = render_docs(&[&doc]);
    assert!(page.contains("@node First Chapter,,,"), "{page}");
    assert!(page.contains("@chapter First Chapter"), "{page}");
    assert!(page.contains("@menu"), "{page}");
    assert!(page.contains("* Details::"), "{page}");
    assert!(page.contains("@node Details,,,First Chapter"), "{page}");
    assert!(page.contains("@section Details"), "{page}");
    assert!(page.contains("@example"), "{page}");
    assert!(page.contains("@itemize @bullet"), "{page}");
    assert!(page.contains("@strong{bold}"), "{page}");
    assert!(page.contains("@end menu"), "{page}");
}

#[test]
fn cross_references_and_indices() {
    let mut doc = empty_doc("index");
    let para = doc.add(NodeKind::Paragraph, Some(doc.root)).expect("para");
    let reference = doc.add(NodeKind::Reference, Some(para)).expect("reference");
    doc.get_mut(reference).expect("reference").attrs.set_str("refuri", "https://example.com/a,b");
    doc.new_text("site", Some(reference)).expect("text");
    let desc = doc.add(NodeKind::Desc, Some(doc.root)).expect("desc");
    let sig = doc.add(NodeKind::DescSignature, Some(desc)).expect("signature");
    doc.new_text("foo(int a, int b)", Some(sig)).expect("text");

    let page = render_docs(&[&doc]);
    assert!(page.contains("@uref{https://example.com/a@,b,site}"), "{page}");
    assert!(page.contains("@cindex foo(int a, int b)"), "{page}");
    assert!(page.contains("@deftypefn {Function} {} foo(int a@, int b)"), "{page}");
    assert!(page.contains("@end deftypefn"), "{page}");
}

#[test]
fn parse_texinfo_documents_defaults() {
    let config = load_config(workspace_root().join("sphinx/tests/roots/test-toctree"), None, None).expect("config");
    let specs = parse_texinfo_documents(&config.0);
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].category, "Miscellaneous");
    assert_eq!(specs[0].docname, "index");
}

/// **Validator gate:** `makeinfo --no-split -o /dev/null` must accept the
/// generated Texinfo file with exit 0 and no diagnostics.
#[test]
fn makeinfo_validates_generated_texi() {
    let fixture = workspace_root().join("sphinx/tests/roots/test-toctree");
    let out = temp_dir("texinfo-validator");
    let result = scribe_build("texinfo", &fixture, &out, &[]);
    assert!(result.is_ok(), "texinfo build failed: {:?}", result.err());
    let texi = out.join("projectnamenotset.texi");
    assert!(texi.is_file());
    let rendered = tool(
        "/usr/sbin/makeinfo",
        &["--no-split", "-o", "/dev/null", texi.to_str().expect("utf8 path")],
    );
    assert!(
        rendered.status.success(),
        "makeinfo failed: {}",
        String::from_utf8_lossy(&rendered.stderr)
    );
    assert!(
        rendered.stderr.is_empty(),
        "makeinfo emitted diagnostics: {}",
        String::from_utf8_lossy(&rendered.stderr)
    );
}

#[test]
fn differential_toctree_structure() {
    let fixture = workspace_root().join("sphinx/tests/roots/test-toctree");
    let oracle_out = temp_dir("oracle-texinfo-diff");
    let status = oracle_build("texinfo", &fixture, &oracle_out);
    assert!(status.status.success(), "oracle texinfo build failed");
    let out = temp_dir("scribe-texinfo-diff");
    let result = scribe_build("texinfo", &fixture, &out, &[]);
    assert!(result.is_ok());

    let expected = std::fs::read_to_string(oracle_out.join("projectnamenotset.texi")).expect("oracle");
    let actual = std::fs::read_to_string(out.join("projectnamenotset.texi")).expect("scribe");
    for marker in [
        "\\input texinfo",
        "@setfilename projectnamenotset.info",
        "@settitle Project name not set",
        "@titlepage",
        "@node Top",
        "@menu",
        "@end menu",
        "@chapter",
        "@section",
        "@bye",
    ] {
        assert!(expected.contains(marker), "oracle lacks {marker}");
        assert!(actual.contains(marker), "scribe lacks {marker}");
    }
    // The node headings are derived from real section titles, not placeholders.
    assert!(!actual.contains("@chapter section-"), "{actual}");
}
