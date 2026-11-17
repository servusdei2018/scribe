//! Doctree serialization round-trip tests.
//!
//! The supported round-trip subset is documented in `common/mod.rs` and in
//! `ARCHITECTURE.md` §7: element start tags, indented text lines, string/bool/
//! list attributes (types inferred from a fixed attribute-name table).
//! Pseudo-XML is a lossy format, so "round-trip" here means *structural*
//! fixpoint: `oracle pseudoxml -> AST -> pseudoxml` reproduces the input bytes.

mod common;

use common::parse_pseudoxml;
use scribe_ast::Document;

const FIXTURE: &str = r#"<document source="test.rst">
    <section ids="a-section" names="a\ section">
        <title>
            A Section
        <paragraph>
            See 
            <pending_xref classes="xref py func" refdoc="index" refdomain="py" refexplicit="0" refid="" refname="StringIO" refspecific="0" reftarget="io.StringIO" reftitle="" reftype="func" refuri="" refwarn="1">
                <literal>
                    StringIO
             done.
        <paragraph>
            This is 
            <emphasis>
                emphasis
             and 
            <literal>
                literal
             and 
            <strong>
                strong
            .
        <bullet_list bullet="-">
            <list_item>
                <paragraph>
                    first item
            <list_item>
                <paragraph>
                    second item
        <note>
            <paragraph>
                This is a note admonition.
"#;

#[test]
fn pseudoxml_to_ast_to_pseudoxml_is_exact() {
    let doc = parse_pseudoxml(FIXTURE);
    assert_eq!(doc.to_pseudoxml().expect("emit"), FIXTURE);
}

#[test]
fn pseudoxml_to_ast_to_json_to_ast_to_pseudoxml_is_exact() {
    let doc = parse_pseudoxml(FIXTURE);
    let json = doc.to_json().expect("serialize");
    let restored = Document::from_json(&json).expect("deserialize");
    assert_eq!(restored, doc);
    assert_eq!(restored.to_pseudoxml().expect("emit"), FIXTURE);
}

#[test]
fn arena_ids_survive_json_round_trip() {
    let doc = Document::fixture_document();
    let xr = doc
        .iter()
        .find(|(_, n)| n.kind == scribe_ast::NodeKind::PendingXref)
        .map(|(id, _)| id)
        .expect("fixture has pending_xref");
    let json = doc.to_json().expect("serialize");
    let back = Document::from_json(&json).expect("deserialize");
    assert_eq!(back.get(xr).expect("id still valid").kind, scribe_ast::NodeKind::PendingXref);
    assert_eq!(back.to_pseudoxml().expect("emit"), doc.to_pseudoxml().expect("emit"));
}
