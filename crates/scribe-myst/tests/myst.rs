//! MyST-specific extensions: frontmatter, directives, roles, targets, math,
//! footnotes and definition lists.

mod common;

use common::{assert_all_spans_in_bounds, assert_span, first_of, nodes_of, text};
use scribe_ast::NodeKind;
use scribe_myst::{parse, parse_and_resolve};
use scribe_rst::pending::PendingDirective;

#[test]
fn yaml_frontmatter_is_parsed_and_exposed() {
    let source = "---\ntitle: Hello\ndraft: true\n---\n\n# T\n";
    let out = parse(source).unwrap();
    let fm = out.frontmatter.as_ref().expect("frontmatter");
    assert_eq!(fm.get_str("title"), Some("Hello"));
    assert_eq!(fm.value["draft"], serde_json::Value::Bool(true));
    assert_eq!(out.frontmatter_str("title"), Some("Hello"));
    assert!(!out.has_diagnostics());
}

#[test]
fn invalid_frontmatter_is_diagnostic_not_panic() {
    let out = parse("---\na: [1, 2\n---\n\nBody.\n").unwrap();
    assert!(out.has_diagnostics());
    assert_eq!(out.diagnostics[0].code(), "scribe::myst::frontmatter");
    let _ = out.document.to_pseudoxml().unwrap();
}

#[test]
fn fenced_directive_emits_pending_contract() {
    let source = "```{note} arg one\n:class: foo\n:name: bar\n\nBody here.\n```\n";
    let d = parse(source).unwrap().document;
    let (_, pending) = first_of(&d, NodeKind::Pending);
    let view = PendingDirective::new(pending).expect("directive pending");
    assert_eq!(view.name().unwrap(), Some("note"));
    assert_eq!(view.arguments().unwrap(), Some("arg one"));
    assert_eq!(view.content().unwrap(), Some("Body here."));
    assert_eq!(view.line().unwrap(), Some(1));
    let options = view.options();
    assert_eq!(options.get("class").map(String::as_str), Some("foo"));
    assert_eq!(options.get("name").map(String::as_str), Some("bar"));
    assert_span(source, pending, "```{note} arg one\n:class: foo\n:name: bar\n\nBody here.\n```");
}

#[test]
fn colon_fenced_directive_matches_backtick_fence() {
    let backtick = parse("```{warning}\nHi.\n```\n").unwrap().document;
    let colon = parse(":::{warning}\nHi.\n:::\n").unwrap().document;
    assert_eq!(
        common::canonical_doc(&backtick),
        common::canonical_doc(&colon)
    );
}

#[test]
fn directive_resolves_through_shared_registry() {
    let (out, report) = parse_and_resolve("```{note}\nHi.\n```\n", Default::default()).unwrap();
    assert!(report.is_clean(), "{report:?}");
    let (_, note) = first_of(&out.document, NodeKind::Note);
    assert_eq!(text(&out.document, nodes_of(&out.document, NodeKind::Note)[0].0), "Hi.");
    assert_eq!(note.kind, NodeKind::Note);
    assert!(nodes_of(&out.document, NodeKind::Pending).is_empty());
}

#[test]
fn nested_directives_resolve_recursively() {
    let source = "````{note}\nOuter.\n\n:::{warning}\nInner.\n:::\n````\n";
    let (out, report) = parse_and_resolve(source, Default::default()).unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(nodes_of(&out.document, NodeKind::Note).len(), 1);
    assert_eq!(nodes_of(&out.document, NodeKind::Warning).len(), 1);
    assert_eq!(text(&out.document, nodes_of(&out.document, NodeKind::Warning)[0].0), "Inner.");
}

#[test]
fn direct_roles_emit_concrete_nodes() {
    let d = parse("{math}`x^2` and {literal}`code`.\n").unwrap().document;
    let (_, math) = first_of(&d, NodeKind::Math);
    assert_eq!(text(&d, nodes_of(&d, NodeKind::Math)[0].0), "x^2");
    assert_eq!(math.kind, NodeKind::Math);
    let (_, _literal) = first_of(&d, NodeKind::Literal);
    assert_eq!(text(&d, nodes_of(&d, NodeKind::Literal)[0].0), "code");
}

#[test]
fn xref_roles_emit_pending_xref_and_meta() {
    let source = "See {ref}`label`.\n";
    let out = parse_with_source_name(source, "doc1.md");
    let (id, xref) = first_of(&out.document, NodeKind::PendingXref);
    assert_eq!(xref.attrs.get_str("reftarget").unwrap(), Some("label"));
    assert_eq!(xref.attrs.get_str("refdomain").unwrap(), Some("std"));
    assert_eq!(xref.attrs.get_str("reftype").unwrap(), Some("ref"));
    assert_eq!(xref.attrs.get_str("refdoc").unwrap(), Some("doc1.md"));
    assert_span(source, xref, "{ref}`label`");
    let meta = out.xrefs.iter().find(|(xref_id, _)| *xref_id == id).map(|(_, m)| m).expect("meta");
    assert_eq!(meta.role, "ref");
    assert_eq!(meta.target, "label");
    let inner = out.document.get(out.document.children(id).unwrap()[0]).unwrap();
    assert_eq!(inner.kind, NodeKind::Inline);
}

#[test]
fn domain_prefixed_role_maps_to_domain() {
    let out = parse("{py:func}`some.func`\n").unwrap();
    let (_, xref) = first_of(&out.document, NodeKind::PendingXref);
    assert_eq!(xref.attrs.get_str("refdomain").unwrap(), Some("py"));
    assert_eq!(xref.attrs.get_str("reftype").unwrap(), Some("func"));
}

#[test]
fn explicit_title_role_splits_target_and_title() {
    let out = parse("{ref}`Custom Title <target>`\n").unwrap();
    let (_, xref) = first_of(&out.document, NodeKind::PendingXref);
    assert_eq!(xref.attrs.get_str("reftarget").unwrap(), Some("target"));
    assert_eq!(xref.attrs.get_bool("refexplicit").unwrap(), Some(true));
}

#[test]
fn target_before_heading_overrides_section_and_records_anchor() {
    let source = "(my-label)=\n## Anchored\n";
    let out = parse(source).unwrap();
    let (section_id, section) = first_of(&out.document, NodeKind::Section);
    assert_eq!(section.attrs.get_list("ids").unwrap(), Some(vec!["my-label".to_owned()]));
    assert_eq!(section.attrs.get_list("names").unwrap(), Some(vec!["my-label".to_owned()]));
    assert_eq!(out.anchors.len(), 1);
    assert_eq!(out.anchors[0].label, "my-label");
    assert_eq!(out.anchors[0].node_id, section_id);
    assert_eq!(out.anchors[0].kind, scribe_myst::AnchorKind::Heading);
    let span = out.anchors[0].span;
    assert_eq!(&source[span.start as usize..span.end as usize], "(my-label)=");
}

#[test]
fn target_before_paragraph_emits_target_node() {
    let out = parse("(standalone)=\n\nA paragraph.\n").unwrap();
    let (id, target) = first_of(&out.document, NodeKind::Target);
    assert_eq!(target.attrs.get_list("names").unwrap(), Some(vec!["standalone".to_owned()]));
    assert_eq!(out.anchors.len(), 1);
    assert_eq!(out.anchors[0].node_id, id);
    assert_eq!(out.anchors[0].kind, scribe_myst::AnchorKind::Target);
}

#[test]
fn inline_and_block_math() {
    let d = parse("Value $x^2$.\n\n$$z^2$$\n").unwrap().document;
    assert_eq!(nodes_of(&d, NodeKind::Math).len(), 1);
    assert_eq!(nodes_of(&d, NodeKind::MathBlock).len(), 1);
    let (_, _block) = first_of(&d, NodeKind::MathBlock);
    assert_eq!(text(&d, nodes_of(&d, NodeKind::MathBlock)[0].0), "z^2");
    let parent = d.parent(nodes_of(&d, NodeKind::MathBlock)[0].0).ok().flatten().unwrap();
    assert_eq!(d.get(parent).unwrap().kind, d.root().kind, "block math is a document-level node");
}

#[test]
fn escaped_dollar_is_literal_text() {
    let d = parse("Cost is \\$5.\n").unwrap().document;
    assert!(nodes_of(&d, NodeKind::Math).is_empty());
    let paragraph_text = text(&d, nodes_of(&d, NodeKind::Paragraph)[0].0);
    assert!(paragraph_text.contains("$5"));
}

#[test]
fn definition_list_extension() {
    let d = parse("term\n: definition\n").unwrap().document;
    let (list_id, list) = first_of(&d, NodeKind::DefinitionList);
    assert_eq!(list.kind, NodeKind::DefinitionList);
    let item_id = d.children(list_id).unwrap()[0];
    let item = d.get(item_id).unwrap();
    assert_eq!(item.kind, NodeKind::DefinitionListItem);
    let kinds: Vec<NodeKind> = d
        .children(item_id)
        .unwrap()
        .iter()
        .map(|id| d.get(*id).unwrap().kind)
        .collect();
    assert!(kinds.contains(&NodeKind::Term));
    assert!(kinds.contains(&NodeKind::Definition));
}

#[test]
fn unclosed_directive_and_role_are_diagnosed() {
    let out = parse("```{note}\nnever closed\n").unwrap();
    assert!(out.has_diagnostics());
    assert!(out
        .diagnostics
        .iter()
        .any(|d| d.code() == "scribe::myst::unclosed_directive"));

    let out = parse("See {ref}`unclosed\n").unwrap();
    assert!(out
        .diagnostics
        .iter()
        .any(|d| d.code() == "scribe::myst::unclosed_role"));

    // A valid role must not be flagged.
    let out = parse("See {ref}`label`.\n").unwrap();
    assert!(!out.has_diagnostics());
}

#[test]
fn malformed_inputs_never_panic_and_spans_stay_valid() {
    let cases = [
        "{ref}unclosed\n",
        "```{note}\nnever closed\n",
        ":::{admonition}\nnested\n",
        "---\n: broken\n---\n",
        "(*\u{1f600}** foo\n",
        "$(x$\n",
        "((((()))))\n",
    ];
    for source in cases {
        let out = parse(source).expect("parse must not fail");
        assert_all_spans_in_bounds(source, &out.document);
        let _ = out.document.to_pseudoxml().unwrap();
    }
}

fn parse_with_source_name(source: &str, name: &str) -> scribe_myst::ParseOutput {
    scribe_myst::parse_with_options(source, scribe_myst::ParseOptions::new(name)).unwrap()
}
