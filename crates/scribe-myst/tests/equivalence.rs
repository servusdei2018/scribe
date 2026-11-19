//! RST ↔ MyST equivalence: the same construct parsed by both engines must
//! produce the same node kinds, nesting and attributes (spans normalized).

mod common;

use common::{canonical_doc, first_of, nodes_of, text};
use scribe_ast::NodeKind;
use scribe_rst::{resolve_pending, Context, DirectiveRegistry};
use scribe_myst::resolve;

fn rst(src: &str) -> scribe_ast::Document {
    scribe_rst::parse(src).document
}

fn myst(src: &str) -> scribe_ast::Document {
    scribe_myst::parse(src).unwrap().document
}

#[test]
fn headings_are_equivalent() {
    let a = rst("Hello World\n===========\n");
    let b = myst("# Hello World\n");
    assert_eq!(canonical_doc(&a), canonical_doc(&b), "\nRST: {}\nMyST: {}", canonical_doc(&a), canonical_doc(&b));
    let (_, section) = first_of(&b, NodeKind::Section);
    assert_eq!(section.attrs.get_list("ids").unwrap(), first_of(&a, NodeKind::Section).1.attrs.get_list("ids").unwrap());
}

#[test]
fn nested_headings_are_equivalent() {
    let a = rst("One\n===\n\nTwo\n---\n\nBody.\n");
    let b = myst("# One\n\n## Two\n\nBody.\n");
    assert_eq!(canonical_doc(&a), canonical_doc(&b));
}

#[test]
fn paragraphs_and_inline_markup_are_equivalent() {
    // RST uses double backticks for inline literals; CommonMark uses single.
    let a = rst("A *short* **strong** ``code`` paragraph.\n");
    let b = myst("A *short* **strong** `code` paragraph.\n");
    assert_eq!(canonical_doc(&a), canonical_doc(&b), "\nRST: {}\nMyST: {}", canonical_doc(&a), canonical_doc(&b));
}

#[test]
fn lists_are_equivalent_after_bullet_normalization() {
    let a = rst("* one\n* two\n");
    let b = myst("- one\n- two\n");
    // `bullet` differs by syntax ("*" vs "-") and is normalized by `canonical`.
    assert_eq!(canonical_doc(&a), canonical_doc(&b));
}

#[test]
fn definition_lists_are_equivalent() {
    let a = rst("term\n    definition\n");
    let b = myst("term\n: definition\n");
    assert_eq!(canonical_doc(&a), canonical_doc(&b), "\nRST: {}\nMyST: {}\n", canonical_doc(&a), canonical_doc(&b));
}

#[test]
fn thematic_break_is_equivalent() {
    let a = rst("Before.\n\n----\n\nAfter.\n");
    let b = myst("Before.\n\n***\n\nAfter.\n");
    assert_eq!(canonical_doc(&a), canonical_doc(&b));
}

#[test]
fn block_quote_is_equivalent() {
    let a = rst("    Quoted.\n");
    let b = myst("> Quoted.\n");
    assert_eq!(canonical_doc(&a), canonical_doc(&b));
}

#[test]
fn math_role_is_equivalent() {
    let a = rst("Value :math:`x^2`.\n");
    let b = myst("Value {math}`x^2`.\n");
    assert_eq!(canonical_doc(&a), canonical_doc(&b));
    assert_eq!(text(&a, nodes_of(&a, NodeKind::Math)[0].0), text(&b, nodes_of(&b, NodeKind::Math)[0].0));
}

#[test]
fn literal_role_is_equivalent() {
    let a = rst("Value :literal:`code`.\n");
    let b = myst("Value {literal}`code`.\n");
    assert_eq!(canonical_doc(&a), canonical_doc(&b));
}

#[test]
fn xref_role_is_equivalent() {
    let a = rst("See :ref:`label`.\n");
    let b = myst("See {ref}`label`.\n");
    let (_, rst_xref) = first_of(&a, NodeKind::PendingXref);
    let (_, myst_xref) = first_of(&b, NodeKind::PendingXref);
    for key in ["refdomain", "reftype", "reftarget", "refwarn", "refexplicit", "refspecific"] {
        assert_eq!(rst_xref.attrs.get(key), myst_xref.attrs.get(key), "attribute {key} differs");
    }
    assert_eq!(
        rst_xref.attrs.get_list("classes").unwrap(),
        myst_xref.attrs.get_list("classes").unwrap()
    );
    assert_eq!(canonical_doc(&a), canonical_doc(&b), "\nRST: {}\nMyST: {}\n", canonical_doc(&a), canonical_doc(&b));
}

#[test]
fn domain_role_is_equivalent() {
    let a = rst("Call :py:func:`some.func`.\n");
    let b = myst("Call {py:func}`some.func`.\n");
    assert_eq!(canonical_doc(&a), canonical_doc(&b));
}

#[test]
fn admonition_directive_is_equivalent_after_resolution() {
    let mut a = rst(".. note::\n\n   Hi there.\n");
    let mut b = myst("```{note}\nHi there.\n```\n");
    let registry = DirectiveRegistry::builtin();
    let context = Context::default();
    let report_a = resolve_pending(&mut a, &registry, &context);
    let report_b = resolve_pending(&mut b, &registry, &context);
    assert!(report_a.is_clean(), "{report_a:?}");
    assert!(report_b.is_clean(), "{report_b:?}");
    assert_eq!(canonical_doc(&a), canonical_doc(&b), "\nRST: {}\nMyST: {}\n", canonical_doc(&a), canonical_doc(&b));
}

#[test]
fn directive_with_options_is_equivalent_after_resolution() {
    let mut a = rst(".. note::\n   :class: foo\n\n   Body.\n");
    let mut b = myst("```{note}\n:class: foo\n\nBody.\n```\n");
    let registry = DirectiveRegistry::builtin();
    let context = Context::default();
    resolve_pending(&mut a, &registry, &context);
    resolve_pending(&mut b, &registry, &context);
    assert_eq!(canonical_doc(&a), canonical_doc(&b), "\nRST: {}\nMyST: {}\n", canonical_doc(&a), canonical_doc(&b));
}

#[test]
fn convenience_resolve_matches_rst_resolve() {
    let mut a = rst(".. warning::\n\n   Careful.\n");
    let registry = DirectiveRegistry::builtin();
    resolve_pending(&mut a, &registry, &Context::default());
    let (b, report) = scribe_myst::parse_and_resolve("```{warning}\nCareful.\n```\n", Default::default()).unwrap();
    assert!(report.is_clean());
    assert_eq!(canonical_doc(&a), canonical_doc(&b.document));
}

#[test]
fn rst_and_myst_targets_share_names() {
    let a = rst(".. _my-label: https://example.com\n");
    let b = myst("(my-label)=\n\nA paragraph.\n");
    let (_, rst_target) = first_of(&a, NodeKind::Target);
    let (_, myst_target) = first_of(&b, NodeKind::Target);
    assert_eq!(rst_target.attrs.get_list("names").unwrap(), myst_target.attrs.get_list("names").unwrap());
    assert_eq!(rst_target.attrs.get_list("ids").unwrap(), myst_target.attrs.get_list("ids").unwrap());
}

#[test]
fn resolve_helper_is_equivalent_to_direct_call() {
    let mut b = scribe_myst::parse("```{tip}\nTip text.\n```\n").unwrap();
    let report = resolve(&mut b, &DirectiveRegistry::builtin());
    assert!(report.is_clean());
    assert_eq!(nodes_of(&b.document, NodeKind::Tip).len(), 1);
}
