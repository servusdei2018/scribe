//! CommonMark core lowered to the shared AST, with exact byte-span assertions.

mod common;

use common::{assert_all_spans_in_bounds, assert_span, first_of, nodes_of, text};
use scribe_ast::{Document, NodeKind};
use scribe_myst::parse;

fn doc(src: &str) -> Document {
    parse(src).unwrap().document
}

#[test]
fn heading_becomes_section_with_title_ids_and_span() {
    let source = "# Hello World\n";
    let d = doc(source);
    let (section_id, section) = first_of(&d, NodeKind::Section);
    assert_eq!(section.attrs.get_list("ids").unwrap(), Some(vec!["hello-world".to_owned()]));
    assert_eq!(section.attrs.get_list("names").unwrap(), Some(vec!["hello world".to_owned()]));
    assert_span(source, section, "# Hello World\n");
    let title = d.get(d.children(section_id).unwrap()[0]).unwrap();
    assert_eq!(title.kind, NodeKind::Title);
    let text_node = d.get(d.children(d.children(section_id).unwrap()[0]).unwrap()[0]).unwrap();
    assert_span(source, text_node, "Hello World");
}

#[test]
fn duplicate_headings_get_unique_ids() {
    let d = doc("# Same\n\n# Same\n");
    let ids: Vec<String> = nodes_of(&d, NodeKind::Section)
        .into_iter()
        .map(|(_, n)| n.attrs.get_list("ids").unwrap().unwrap()[0].clone())
        .collect();
    assert_eq!(ids, vec!["same", "same-1"]);
}

#[test]
fn nested_sections_follow_heading_levels() {
    let d = doc("# One\n\n## Two\n\n### Three\n\n# Four\n");
    let sections = nodes_of(&d, NodeKind::Section);
    assert_eq!(sections.len(), 4);
    let parent_of = |id| d.parent(id).ok().flatten();
    // Two is a child of One; Three of Two; Four is back at the root.
    assert_eq!(parent_of(sections[1].0), Some(sections[0].0));
    assert_eq!(parent_of(sections[2].0), Some(sections[1].0));
    assert_eq!(parent_of(sections[3].0), Some(d.root));
}

#[test]
fn paragraph_and_inline_spans_are_exact() {
    let source = "A *short* paragraph.\n";
    let d = doc(source);
    let (_, para) = first_of(&d, NodeKind::Paragraph);
    assert_span(source, para, "A *short* paragraph.\n");
    let (_, em) = first_of(&d, NodeKind::Emphasis);
    assert_span(source, em, "*short*");
    let em_id = nodes_of(&d, NodeKind::Emphasis)[0].0;
    let inner = d.get(d.children(em_id).unwrap()[0]).unwrap();
    assert_span(source, inner, "short");
}

#[test]
fn strong_and_inline_code() {
    let d = doc("**bold** and `code`.\n");
    let (_, strong) = first_of(&d, NodeKind::Strong);
    assert_eq!(text(&d, nodes_of(&d, NodeKind::Strong)[0].0), "bold");
    assert_eq!(strong.kind, NodeKind::Strong);
    let (_, _literal) = first_of(&d, NodeKind::Literal);
    assert_eq!(text(&d, nodes_of(&d, NodeKind::Literal)[0].0), "code");
}

#[test]
fn links_map_to_reference_with_uri_or_fragment() {
    let source = "[site](https://example.com) and [local](#frag).\n";
    let d = doc(source);
    let refs = nodes_of(&d, NodeKind::Reference);
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[0].1.attrs.get_str("refuri").unwrap(), Some("https://example.com"));
    assert_eq!(refs[1].1.attrs.get_str("refid").unwrap(), Some("frag"));
    assert_span(source, refs[0].1, "[site](https://example.com)");
}

#[test]
fn image_has_uri_and_alt() {
    let source = "![a cat](cat.png)\n";
    let d = doc(source);
    let (_, image) = first_of(&d, NodeKind::Image);
    assert_eq!(image.attrs.get_str("uri").unwrap(), Some("cat.png"));
    assert_eq!(image.attrs.get_str("alt").unwrap(), Some("a cat"));
    assert_span(source, image, "![a cat](cat.png)");
}

#[test]
fn bullet_and_enumerated_lists() {
    let d = doc("- one\n- two\n\n1. first\n2. second\n");
    let (bullet_id, bullet) = first_of(&d, NodeKind::BulletList);
    assert_eq!(bullet.attrs.get_str("bullet").unwrap(), Some("-"));
    assert_eq!(d.children(bullet_id).unwrap().len(), 2);
    let (enum_id, enumerated) = first_of(&d, NodeKind::EnumeratedList);
    assert_eq!(enumerated.attrs.get_int("start").unwrap(), Some(1));
    assert_eq!(d.children(enum_id).unwrap().len(), 2);
    for id in d.children(bullet_id).unwrap() {
        assert_eq!(d.get(*id).unwrap().kind, NodeKind::ListItem);
    }
}

#[test]
fn blockquote_wraps_paragraph() {
    let d = doc("> quoted text\n");
    let (quote_id, quote) = first_of(&d, NodeKind::BlockQuote);
    assert_eq!(quote.kind, NodeKind::BlockQuote);
    let child = d.get(d.children(quote_id).unwrap()[0]).unwrap();
    assert_eq!(child.kind, NodeKind::Paragraph);
}

#[test]
fn thematic_break_becomes_transition() {
    let source = "Before.\n\n***\n\nAfter.\n";
    let d = doc(source);
    let (_, transition) = first_of(&d, NodeKind::Transition);
    assert_span(source, transition, "***\n");
}

#[test]
fn fenced_and_indented_code_become_literal_blocks() {
    let d = doc("```rust\nfn main() {}\n```\n\n    indented\n");
    let blocks = nodes_of(&d, NodeKind::LiteralBlock);
    assert_eq!(blocks.len(), 2);
    for (_, block) in &blocks {
        assert_eq!(block.attrs.get_str("xml:space").unwrap(), Some("preserve"));
    }
    assert!(text(&d, blocks[0].0).contains("fn main() {}"));
    assert!(text(&d, blocks[1].0).contains("indented"));
}

#[test]
fn gfm_table_structure() {
    let d = doc("| a | b |\n|---|---|\n| 1 | 2 |\n");
    let (table_id, table) = first_of(&d, NodeKind::Table);
    assert_eq!(table.kind, NodeKind::Table);
    let tgroup_id = d.children(table_id).unwrap()[0];
    let tgroup = d.get(tgroup_id).unwrap();
    assert_eq!(tgroup.kind, NodeKind::Tgroup);
    assert_eq!(tgroup.attrs.get_int("cols").unwrap(), Some(2));
    let kinds: Vec<NodeKind> = d.children(tgroup_id)
        .unwrap()
        .iter()
        .map(|id| d.get(*id).unwrap().kind)
        .collect();
    assert!(kinds.contains(&NodeKind::Colspec));
    assert!(kinds.contains(&NodeKind::Thead));
    assert!(kinds.contains(&NodeKind::Tbody));
    assert_eq!(nodes_of(&d, NodeKind::Entry).len(), 4);
}

#[test]
fn raw_html_passthrough() {
    let d = doc("<div class=\"x\">hi</div>\n");
    let (_, raw) = first_of(&d, NodeKind::Raw);
    assert_eq!(raw.attrs.get_str("format").unwrap(), Some("html"));
    assert!(text(&d, nodes_of(&d, NodeKind::Raw)[0].0).contains("<div"));
}

#[test]
fn footnotes_reference_and_definition() {
    let d = doc("A note[^1].\n\n[^1]: the text\n");
    let (_, reference) = first_of(&d, NodeKind::FootnoteReference);
    assert_eq!(reference.attrs.get_str("refname").unwrap(), Some("1"));
    let (_, footnote) = first_of(&d, NodeKind::Footnote);
    assert_eq!(footnote.attrs.get_list("names").unwrap(), Some(vec!["1".to_owned()]));
}

#[test]
fn all_spans_in_bounds_on_realistic_document() {
    let source = "# Title\n\nA *paragraph* with `code` and [link](https://x).\n\n- a\n- b\n\n> quote\n\n```rust\nfn main() {}\n```\n";
    let d = doc(source);
    assert_all_spans_in_bounds(source, &d);
}
