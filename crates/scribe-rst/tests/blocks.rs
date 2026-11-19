//! Unit tests for every block construct implemented in Phase 04.

mod common;

use scribe_ast::NodeKind;
use scribe_rst::pending::{as_pending_directive, is_directive_pending};
use scribe_rst::{parse, parse_with_options, ParseOptions};

use common::{assert_block, parse_clean, pseudo};

#[test]
fn section_hierarchy_and_slugs() {
    let doc = pseudo(
        "Main Title\n==========\n\nIntro.\n\nSub One\n-------\n\nBody.\n\nSub Two\n-------\n\nMore.\n",
    );
    assert_block(
        &doc,
        "    <section ids=\"main-title\" names=\"main\\ title\">\n        <title>\n            Main Title\n        <paragraph>\n            Intro.",
    );
    assert!(doc.contains("<section ids=\"sub-one\""));
    assert!(doc.contains("<section ids=\"sub-two\""));
    assert!(doc.matches("<section ids=\"sub-").count() == 2);
}

#[test]
fn overline_and_underline_sections() {
    let doc = pseudo("=====\nTitle\n=====\n\nSub\n---\n\nText.\n");
    assert_block(&doc, "<section ids=\"title\" names=\"title\">");
    assert!(doc.contains("<section ids=\"sub\""));
}

#[test]
fn duplicate_section_ids_get_suffixes() {
    let output = parse("Same\n====\n\nA\n\nSame\n====\n\nB\n");
    assert!(output.has_diagnostics()); // duplicate implicit target note
    let doc = output.document.to_pseudoxml().unwrap();
    assert!(doc.contains("ids=\"same\""));
    assert!(doc.contains("ids=\"same-1\""));
    assert!(doc.contains("dupnames=\"same\""));
}

#[test]
fn nested_bullet_lists_and_tightness() {
    let src = "* first\n* second\n  * nested a\n  * nested b\n\n* loose item\n";
    let output = parse_clean(src);
    let lists: Vec<_> = output
        .document
        .iter()
        .filter(|(_, node)| node.kind == NodeKind::BulletList)
        .map(|(id, _)| id)
        .collect();
    assert_eq!(lists.len(), 2, "expected outer + nested list");
    assert_eq!(output.list_is_tight(lists[0]), Some(false));
    assert_eq!(output.list_is_tight(lists[1]), Some(true));
    let doc = output.document.to_pseudoxml().unwrap();
    assert_block(
        &doc,
        "        <list_item>\n            <paragraph>\n                second\n            <bullet_list bullet=\"*\">",
    );
    assert!(doc.contains("nested a"));
}

#[test]
fn enumerated_list_formats() {
    let arabic = pseudo("1. one\n2. two\n");
    assert_block(&arabic, "<enumerated_list enumtype=\"arabic\" prefix=\"\" suffix=\".\">");

    let parens = pseudo("(a) alpha\n(b) beta\n");
    assert_block(
        &parens,
        "<enumerated_list enumtype=\"loweralpha\" prefix=\"(\" suffix=\")\">",
    );

    let rparen = pseudo("i) roman\nii) two\n");
    assert_block(
        &rparen,
        "<enumerated_list enumtype=\"lowerroman\" prefix=\"\" suffix=\")\">",
    );

    let auto = pseudo("#. first\n#. second\n");
    assert_block(&auto, "<enumerated_list enumtype=\"arabic\" prefix=\"\" suffix=\".\">");
}

#[test]
fn enumerated_list_non_one_start() {
    let doc = pseudo("3. three\n4. four\n");
    assert!(doc.contains("start=\"3\""), "{doc}");
}

#[test]
fn definition_list_with_classifier() {
    let doc = pseudo("term : classifier\n    Definition body.\n");
    assert_block(
        &doc,
        "    <definition_list>\n        <definition_list_item>\n            <term>\n                term\n            <classifier>\n                classifier\n            <definition>\n                <paragraph>\n                    Definition body.",
    );
}

#[test]
fn field_list_with_multiline_body() {
    let doc = pseudo(":author: Jane Doe\n:version: 1.0\n\n:notes:\n    A body paragraph\n    on two lines.\n");
    assert_block(&doc, "<field_list>");
    assert_block(&doc, "<field_name>\n                author\n            <field_body>");
    assert!(doc.contains("A body paragraph"));
    assert_eq!(doc.matches("<field>").count(), 3);
}

#[test]
fn option_list_short_long_and_arguments() {
    let doc = pseudo(
        "-a  short option\n--long  long option\n--file=NAME  file option\n-o FILE  another\n",
    );
    assert_block(&doc, "<option_list>");
    assert_block(
        &doc,
        "<option_group>\n                <option>\n                    <option_string>\n                        --file\n                    <option_argument delimiter=\"=\">\n                        NAME",
    );
    assert!(doc.contains("-a"));
    assert!(doc.contains("--long"));
    assert!(doc.contains("FILE"));
    assert_eq!(doc.matches("<option_list_item>").count(), 4);
    assert_eq!(doc.matches("<option_group>").count(), 4);
}

#[test]
fn line_blocks() {
    let doc = pseudo("| first line\n| second line\n");
    assert_block(
        &doc,
        "    <line_block>\n        <line>\n            first line\n        <line>\n            second line",
    );
}

#[test]
fn block_quote_with_attribution() {
    let doc = pseudo("    Quoted text here.\n\n    -- Author Name\n");
    assert_block(&doc, "<block_quote>");
    assert_block(&doc, "<attribution>\n            Author Name");
}

#[test]
fn literal_block_preserves_internal_whitespace() {
    let doc = pseudo("Intro::\n\n    first line\n      indented line\n\n    last line\n");
    assert_block(&doc, "<paragraph>\n        Intro:\n    <literal_block xml:space=\"preserve\">");
    assert_block(&doc, "        first line\n          indented line");
    assert!(doc.contains("last line"), "{doc}");
}

#[test]
fn bare_double_colon_has_no_paragraph() {
    let doc = pseudo("::\n\n    only literal\n");
    assert!(!doc.contains("<paragraph>"), "{doc}");
    assert!(doc.contains("<literal_block"));
    assert!(doc.contains("only literal"));
}

#[test]
fn doctest_block() {
    let doc = pseudo(">>> 1 + 1\n2\n>>> print('x')\nx\n");
    assert_block(
        &doc,
        "    <doctest_block xml:space=\"preserve\">\n        >>> 1 + 1\n        2\n        >>> print('x')\n        x",
    );
}

#[test]
fn transitions() {
    let doc = pseudo("Title\n=====\n\nPara.\n\n----\n\nMore.\n");
    assert!(doc.contains("<transition>"), "{doc}");
}

#[test]
fn grid_table_with_head_and_spans() {
    let src = [
        "+---+---+",
        "| a | b |",
        "+===+===+",
        "| 1 | 2 |",
        "+---+---+",
    ]
    .join("\n");
    let doc = pseudo(&src);
    assert_block(&doc, "<table>\n        <tgroup cols=\"2\">");
    assert!(doc.contains("<colspec colwidth=\"3\">"), "{doc}");
    assert!(doc.contains("<thead>"));
    assert!(doc.contains("<tbody>"));
    assert!(doc.contains("<tbody>"), "{doc}");
}

#[test]
fn grid_table_row_and_column_spans() {
    let src = [
        "+---+---+---+",
        "| a | b     |",
        "+---+   +---+",
        "| c |   | d |",
        "+---+---+---+",
    ]
    .join("\n");
    let output = parse_clean(&src);
    let doc = output.document.to_pseudoxml().unwrap();
    assert!(doc.contains("morecols=\"1\""), "{doc}");
    assert!(doc.contains("morerows=\"1\""), "{doc}");
}

#[test]
fn simple_table() {
    let src = "=====  =====\ncol 1  col 2\n=====  =====\n1      Second\n2      Third\n=====  =====\n";
    let doc = pseudo(src);
    assert_block(&doc, "<table>\n        <tgroup cols=\"2\">");
    assert!(doc.contains("col 1"));
    assert!(doc.contains("Second"));
    assert!(doc.contains("<thead>"));
}

#[test]
fn comments_targets_and_substitutions() {
    let src = [
        ".. This is a comment",
        "   continuing here.",
        "",
        ".. _my target: https://example.com",
        "",
        ".. |sub| replace:: substitution text",
        "",
        "Hello.",
    ]
    .join("\n");
    let doc = pseudo(&src);
    assert_block(&doc, "<comment xml:space=\"preserve\">");
    assert!(doc.contains("This is a comment\n        continuing here."), "{doc}");
    assert!(doc.contains("<target ids=\"my-target\" names=\"my\\ target\" refuri=\"https://example.com\">"), "{doc}");
    assert!(doc.contains("<substitution_definition names=\"sub\">"), "{doc}");
    assert!(doc.contains("substitution text"));
}

#[test]
fn directive_emits_pending_with_contract_attributes() {
    let src = [
        ".. note:: inline arguments",
        "   :class: foo",
        "   :name: bar",
        "",
        "   Directive body.",
    ]
    .join("\n");
    let output = parse_clean(&src);
    let pending = output
        .document
        .iter()
        .find(|(_, node)| node.kind == NodeKind::Pending)
        .map(|(id, _)| id)
        .expect("pending node emitted");
    let node = output.document.get(pending).unwrap();
    let view = as_pending_directive(node).expect("directive pending");
    assert_eq!(view.name().unwrap(), Some("note"));
    assert_eq!(view.arguments().unwrap(), Some("inline arguments"));
    assert_eq!(view.content().unwrap(), Some("Directive body."));
    assert_eq!(view.options().get("class").map(String::as_str), Some("foo"));
    assert_eq!(view.options().get("name").map(String::as_str), Some("bar"));
    assert!(is_directive_pending(node));
}

#[test]
fn directive_without_arguments_uses_first_content_line() {
    let output = parse_clean(".. toctree::\n   index other\n");
    let node = output
        .document
        .iter()
        .find(|(_, node)| is_directive_pending(node))
        .map(|(_, node)| node)
        .expect("pending");
    let view = as_pending_directive(node).unwrap();
    assert_eq!(view.name().unwrap(), Some("toctree"));
    assert_eq!(view.arguments().unwrap(), Some("index other"));
}

#[test]
fn ul_bullet_characters() {
    for bullet in ['-', '+', '*', '•'] {
        let doc = pseudo(&format!("{bullet} item\n"));
        assert!(
            doc.contains(&format!("<bullet_list bullet=\"{bullet}\">")),
            "bullet {bullet}: {doc}"
        );
    }
}

#[test]
fn crlf_and_tabs_are_normalized() {
    let doc = pseudo("Title\r\n=====\r\n\r\n\tIndented quote.\r\n");
    assert!(doc.contains("<section"));
    assert!(doc.contains("<block_quote>"));
    assert!(doc.contains("Indented quote."));
}

#[test]
fn prolog_and_epilog_injection() {
    let options = ParseOptions {
        source_name: Some("index.rst".into()),
        tab_width: 8,
        prolog: Some(".. |x| replace:: X".into()),
        epilog: Some(".. _e: https://example.com".into()),
        extlinks: scribe_rst::extlinks::empty_roles(),
    };
    let output = parse_with_options("Body.\n", options);
    let doc = output.document.to_pseudoxml().unwrap();
    assert!(doc.contains("<substitution_definition names=\"x\">"), "{doc}");
    assert!(doc.contains("https://example.com"), "{doc}");
    assert!(doc.contains("<paragraph>"), "{doc}");
}

#[test]
fn prolog_injected_after_docinfo() {
    let options = ParseOptions {
        prolog: Some(".. |x| replace:: X".into()),
        ..ParseOptions::default()
    };
    let output = parse_with_options(":author: me\n\nBody.\n", options);
    let doc = output.document.to_pseudoxml().unwrap();
    let author = doc.find("author").expect("docinfo field");
    let sub = doc.find("substitution_definition").expect("prolog substitution");
    let body = doc.find("Body.").expect("body paragraph");
    assert!(author < sub && sub < body, "{doc}");
}

#[test]
fn empty_document_is_valid() {
    let output = parse_clean("");
    assert!(output.document.is_empty());
    assert_eq!(output.document.to_pseudoxml().unwrap(), "<document>\n");
}

#[test]
fn spans_are_recorded_on_nodes() {
    let output = parse_clean("Title\n=====\n\nBody.\n");
    let section = output
        .document
        .iter()
        .find(|(_, node)| node.kind == NodeKind::Section)
        .map(|(_, node)| node)
        .expect("section");
    let span = section.span.expect("span");
    assert_eq!(span.start, 0);
    assert_eq!(span.end, 11); // "Title\n=====\n"
    assert_eq!(section.source.as_ref().and_then(|s| s.line), Some(1));

    let para = output
        .document
        .iter()
        .find(|(_, node)| node.kind == NodeKind::Paragraph)
        .map(|(_, node)| node)
        .expect("paragraph");
    assert_eq!(para.span.map(|s| s.start), Some(13));
    assert_eq!(para.source.as_ref().and_then(|s| s.line), Some(4));
}
