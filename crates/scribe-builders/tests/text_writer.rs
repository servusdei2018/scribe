//! Unit + differential + robustness tests for the Phase 24 plain-text writer.

mod output_common;

use std::path::Path;

use scribe_ast::{Document, NodeKind};
use scribe_builders::text::{column_width, render, render_with_numbers, wrap, TextOptions};
use scribe_config::{Config, ConfigValue};

use output_common::{normalize_text, oracle_build, scribe_build, temp_dir, workspace_root};

fn defaults() -> TextOptions {
    TextOptions::upstream_defaults()
}

fn render_ast(build: impl FnOnce(&mut Document), options: &TextOptions) -> String {
    let mut doc = Document::new(Some("index".to_owned()));
    doc.metadata.docname = Some("index".to_owned());
    build(&mut doc);
    render(&doc, options)
}

fn text_node(doc: &mut Document, parent: scribe_ast::NodeId, value: &str) {
    let id = doc.new_text(value, Some(parent)).expect("fresh node id");
    let _ = id;
}

#[test]
fn wrap_respects_width_and_breaks_long_words() {
    let lines = wrap("one two three four", 9);
    assert_eq!(lines, vec!["one two", "three", "four"]);
    let long = wrap("abcdefghij", 4);
    assert_eq!(long, vec!["abcd", "efgh", "ij"]);
}

#[test]
fn column_width_handles_cjk_and_combining() {
    assert_eq!(column_width("abc"), 3);
    assert_eq!(column_width("中文"), 4);
    assert_eq!(column_width("e\u{0301}"), 1); // e + combining acute
}

#[test]
fn empty_document_is_safe() {
    assert_eq!(render_ast(|_| {}, &defaults()), "");
}

#[test]
fn section_title_is_underlined_with_level_char() {
    let out = render_ast(
        |doc| {
            let section = doc.add(NodeKind::Section, Some(doc.root)).expect("section");
            let title = doc.add(NodeKind::Title, Some(section)).expect("title");
            text_node(doc, title, "Introduction");
        },
        &defaults(),
    );
    assert_eq!(out, "Introduction\n************\n");
}

#[test]
fn nested_section_uses_second_underline_char() {
    let out = render_ast(
        |doc| {
            let outer = doc.add(NodeKind::Section, Some(doc.root)).expect("outer");
            let outer_title = doc.add(NodeKind::Title, Some(outer)).expect("title");
            text_node(doc, outer_title, "Top");
            let inner = doc.add(NodeKind::Section, Some(outer)).expect("inner");
            let inner_title = doc.add(NodeKind::Title, Some(inner)).expect("title");
            text_node(doc, inner_title, "Sub");
        },
        &defaults(),
    );
    assert!(out.contains("Top\n***\n"), "{out}");
    assert!(out.contains("Sub\n===\n"), "{out}");
}

#[test]
fn literal_block_is_indented_without_wrapping() {
    let out = render_ast(
        |doc| {
            let block = doc.add(NodeKind::LiteralBlock, Some(doc.root)).expect("literal");
            text_node(doc, block, "a  b\nccc");
        },
        &defaults(),
    );
    assert!(out.contains("a  b"), "{out}");
    assert!(out.contains("ccc"), "{out}");
}

#[test]
fn bullet_and_enumeration_markers() {
    let out = render_ast(
        |doc| {
            let list = doc.add(NodeKind::BulletList, Some(doc.root)).expect("list");
            let item = doc.add(NodeKind::ListItem, Some(list)).expect("item");
            let para = doc.add(NodeKind::Paragraph, Some(item)).expect("para");
            text_node(doc, para, "first");
            let enum_list = doc.add(NodeKind::EnumeratedList, Some(doc.root)).expect("enum");
            doc.get_mut(enum_list).expect("enum").attrs.set_int("start", 3);
            let enum_item = doc.add(NodeKind::ListItem, Some(enum_list)).expect("item");
            let enum_para = doc.add(NodeKind::Paragraph, Some(enum_item)).expect("para");
            text_node(doc, enum_para, "third");
        },
        &defaults(),
    );
    assert!(out.contains("* first"), "{out}");
    assert!(out.contains("3. third"), "{out}");
}

#[test]
fn note_admonition_has_label() {
    let out = render_ast(
        |doc| {
            let note = doc.add(NodeKind::Note, Some(doc.root)).expect("note");
            let para = doc.add(NodeKind::Paragraph, Some(note)).expect("para");
            text_node(doc, para, "Remember this");
        },
        &defaults(),
    );
    assert!(out.contains("Note:"), "{out}");
    assert!(out.contains("Remember this"), "{out}");
}

#[test]
fn table_renders_box_and_unicode_variant() {
    fn build(doc: &mut Document) {
        let table = doc.add(NodeKind::Table, Some(doc.root)).expect("table");
        let group = doc.add(NodeKind::Tgroup, Some(table)).expect("group");
        let tbody = doc.add(NodeKind::Tbody, Some(group)).expect("tbody");
        let row = doc.add(NodeKind::Row, Some(tbody)).expect("row");
        for value in ["left", "right"] {
            let entry = doc.add(NodeKind::Entry, Some(row)).expect("entry");
            let para = doc.add(NodeKind::Paragraph, Some(entry)).expect("para");
            text_node(doc, para, value);
        }
    }
    let ascii = render_ast(build, &defaults());
    assert!(ascii.contains("+------+-------+"), "{ascii}");
    assert!(ascii.contains("| left | right |"), "{ascii}");

    let mut unicode = defaults();
    unicode.unicode_tables = true;
    let unicode = render_ast(build, &unicode);
    assert!(unicode.contains('┌'), "{unicode}");
    assert!(unicode.contains('│'), "{unicode}");
}

#[test]
fn unicode_and_empty_inputs_do_not_panic() {
    let out = render_ast(
        |doc| {
            let para = doc.add(NodeKind::Paragraph, Some(doc.root)).expect("para");
            text_node(doc, para, "中文 emoji 🚀 — naïve");
        },
        &defaults(),
    );
    assert!(out.contains("中文"), "{out}");
    assert_eq!(render_ast(|_| {}, &defaults()), "");
}

#[test]
fn text_newlines_windows_is_honoured() {
    let mut config = Config::new();
    let _ = config.set_explicit("text_newlines", ConfigValue::from("windows"));
    let options = TextOptions::from_config(&config);
    assert_eq!(options.newline, "\r\n");
}

#[test]
fn section_numbers_can_be_injected() {
    use scribe_core::{SectionNumbers, ToctreeCollection};
    let mut doc = Document::new(Some("index".to_owned()));
    doc.metadata.docname = Some("index".to_owned());
    let section = doc.add(NodeKind::Section, Some(doc.root)).expect("section");
    doc.get_mut(section).expect("section").set_list("ids", vec!["intro".to_owned()]);
    let title = doc.add(NodeKind::Title, Some(section)).expect("title");
    text_node(&mut doc, title, "Intro");
    let mut numbers = SectionNumbers::default();
    numbers.by_doc.entry("index".to_owned()).or_default().insert("#intro".to_owned(), vec![1, 2]);
    let collection = ToctreeCollection {
        section_numbers: numbers,
        ..ToctreeCollection::default()
    };
    let out = render_with_numbers(&doc, &defaults(), Some(&collection.section_numbers), "index");
    assert!(out.contains("1.2. Intro"), "{out}");
}

#[test]
fn differential_test_build_text_normalized() {
    let fixture = workspace_root().join("sphinx/tests/roots/test-build-text");
    let oracle_out = temp_dir("oracle-text");
    let status = oracle_build("text", &fixture, &oracle_out);
    assert!(status.status.success(), "oracle text build failed: {}", String::from_utf8_lossy(&status.stderr));

    let scribe_out = temp_dir("scribe-text");
    let result = scribe_build("text", &fixture, &scribe_out, &[("text_width", "70")]);
    assert!(result.is_ok(), "scribe text build failed: {:?}", result.err());

    for file in ["doc1.txt", "lineblock.txt", "listitems.txt", "maxwidth.txt", "table.txt"] {
        let expected = std::fs::read_to_string(oracle_out.join(file)).expect("oracle output");
        let actual = std::fs::read_to_string(scribe_out.join(file)).expect("scribe output");
        assert_eq!(
            normalize_text(&expected),
            normalize_text(&actual),
            "normalized content differs for {file}\n--- oracle ---\n{expected}\n--- scribe ---\n{actual}"
        );
    }
    // Structural invariants that normalization intentionally ignores.
    for file in ["doc1.txt", "index.txt", "table.txt"] {
        let text = std::fs::read_to_string(scribe_out.join(file)).expect("scribe output");
        assert!(!text.trim().is_empty(), "{file} must not be empty");
        assert!(text.ends_with('\n'), "{file} must end with a newline");
    }
    let _ = Path::new(".");
}
