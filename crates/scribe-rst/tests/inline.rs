//! Focused Phase 05 inline-markup tests.
//!
//! Every construct is checked at three levels: (a) the pure recogniser's node
//! kinds, (b) the materialised arena tree / pseudo-XML, and (c) exact byte
//! spans pointing into the original source.

use scribe_ast::{NodeKind, Span};
use scribe_rst::inline::{parse_inline, InlineSource};
use scribe_rst::{parse, parse_with_options, ParseOptions};

mod common;

fn tags(text: &str) -> Vec<&'static str> {
    parse_inline(&InlineSource::single(text, 0), &Default::default())
        .nodes
        .iter()
        .map(|n| n.kind.tag_name())
        .collect()
}

fn pseudo(src: &str) -> String {
    parse(src).document.to_pseudoxml().expect("pseudo-XML")
}

// ----- emphasis / strong -----------------------------------------------------

#[test]
fn emphasis_basic_and_boundaries() {
    assert_eq!(tags("*em*"), vec!["emphasis"]);
    assert_eq!(tags("a*b*"), vec!["#text"]); // no whitespace before start
    assert_eq!(tags("* em*"), vec!["#text"]); // whitespace immediately inside
    // Whitespace before the end delimiter invalidates that candidate; with no
    // later valid closing `*`, upstream emits a problematic node.
    assert_eq!(tags("*em *"), vec!["problematic", "#text"]);
    assert_eq!(tags("(*em*)"), vec!["#text", "emphasis", "#text"]);
    assert_eq!(tags("*em*."), vec!["emphasis", "#text"]); // punctuation suffix
}

#[test]
fn strong_not_emphasis() {
    assert_eq!(tags("**strong**"), vec!["strong"]);
    // Docutils treats `***mix***` as a single strong node whose content is
    // the literal `*mix*` (end-string suffix rules reject the inner `**`).
    assert_eq!(tags("***mix***"), vec!["strong"]);
}

#[test]
fn quoted_start_is_not_markup() {
    // The char immediately after the start delimiter must be the matching
    // closer of the char before it. For `"*em*"` the post-start char is `e`,
    // so upstream *does* recognise emphasis.
    assert_eq!(tags("\"*em*\""), vec!["#text", "emphasis", "#text"]);
    // But `'*'` (quote directly around the delimiter) is quoted and skipped.
    assert_eq!(tags("'*' and text"), vec!["#text"]);
}

#[test]
fn unmatched_start_is_problematic() {
    assert_eq!(tags("*unclosed"), vec!["problematic", "#text"]);
}

// ----- literals & escapes ----------------------------------------------------

#[test]
fn inline_literal_rules() {
    let r = parse_inline(&InlineSource::single("``a `` b``", 0), &Default::default());
    assert_eq!(r.nodes[0].kind, NodeKind::Literal);
    // Backslashes are restored inside literals; whitespace is preserved.
    let r = parse_inline(&InlineSource::single(r"``a\*b``", 0), &Default::default());
    assert_eq!(r.nodes[0].children[0].text.as_deref(), Some(r"a\*b"));
}

#[test]
fn backslash_escapes_become_literal_text() {
    let out = parse(r"\*a\* \`b\` \\ \|c\| \[d\]\_");
    let p = out.document.to_pseudoxml().unwrap();
    assert!(p.contains("*a* `b` \\ |c| [d]_"), "{p}");
}

#[test]
fn inline_spans_are_exact() {
    let r = parse_inline(&InlineSource::single("x **bold** y", 100), &Default::default());
    // leading text, strong, trailing text
    let strong = r.nodes.iter().find(|n| n.kind == NodeKind::Strong).unwrap();
    assert_eq!(strong.span, Span::new(102, 110)); // "**bold**"
    assert_eq!(strong.children[0].span, Span::new(104, 108)); // "bold"
}

// ----- roles -----------------------------------------------------------------

#[test]
fn direct_roles_map_to_nodes() {
    let p = pseudo(":math:`x` :literal:`y` :emphasis:`z` :strong:`w`");
    assert!(p.contains("<math>"), "{p}");
    assert!(p.contains("<literal>"), "{p}");
    assert!(p.contains("<emphasis>"), "{p}");
    assert!(p.contains("<strong>"), "{p}");
}

#[test]
fn default_role_is_title_reference_node() {
    // Phase 34: the default role emits a first-class `title_reference` node,
    // matching `sphinx-build -b pseudoxml` exactly (no synthetic CSS class).
    let p = pseudo("`title`");
    assert!(p.contains("<title_reference>\n            title"), "{p}");
    assert!(!p.contains("<inline"), "{p}");
}

#[test]
fn xref_roles_and_modifiers() {
    let out = parse(":py:func:`~os.path.join` :py:class:`.Foo` :ref:`Label <target>`");
    let p = out.document.to_pseudoxml().unwrap();
    assert!(p.contains("refdomain=\"py\""), "{p}");
    assert!(p.contains("reftarget=\"os.path.join\""), "{p}");
    assert!(p.contains("reftarget=\"Foo\""), "{p}");
    assert!(p.contains("refexplicit=\"1\""), "{p}");
    assert!(p.contains("reftarget=\"target\""), "{p}");
    let metas = out.inline_xrefs();
    assert_eq!(metas.len(), 3);
    assert_eq!(metas[0].1.role, "py:func");
    assert!(metas[1].1.specific);
    assert!(metas[2].1.explicit_title);
    assert_eq!(metas[2].1.target, "target");
}

#[test]
fn unknown_role_is_problematic() {
    let p = pseudo(":not-a-role:`x`");
    assert!(p.contains("<problematic>"), "{p}");
}

#[test]
fn role_suffix_form() {
    let p = pseudo("`text`:literal:");
    assert!(p.contains("<literal>"), "{p}");
}

// ----- references ------------------------------------------------------------

#[test]
fn named_and_anonymous_references_resolve() {
    let p = pseudo("See link_ and other__.\n\n.. _link: https://example.org/a\n.. __: https://example.org/b\n");
    assert!(p.contains("<reference name=\"link\" refuri=\"https://example.org/a\">"), "{p}");
    assert!(p.contains("<reference anonymous=\"1\" name=\"other\" refuri=\"https://example.org/b\">"), "{p}");
}

#[test]
fn phrase_reference_with_embedded_link() {
    let p = pseudo("A `Title <https://example.org/t>`_ here.");
    assert!(p.contains("<reference name=\"Title\" refuri=\"https://example.org/t\">"), "{p}");
    assert!(p.contains("<target ids=\"title\" names=\"title\" refuri=\"https://example.org/t\">"), "{p}");
}

#[test]
fn standalone_uri_and_email() {
    let p = pseudo("Visit https://example.com/a?q=1#f or mail me@example.co.uk.");
    assert!(p.contains("<reference refuri=\"https://example.com/a?q=1#f\">"), "{p}");
    assert!(p.contains("<reference refuri=\"mailto:me@example.co.uk\">"), "{p}");
}

#[test]
fn trailing_uri_punctuation_is_not_part_of_link() {
    let p = pseudo("See https://example.com/a.");
    assert!(p.contains("refuri=\"https://example.com/a\""), "{p}");
    assert!(!p.contains("refuri=\"https://example.com/a.\""), "{p}");
}

// ----- substitutions / footnotes --------------------------------------------

#[test]
fn substitution_reference_expands() {
    let p = pseudo("Use |date| here.\n\n.. |date| replace:: 2026-01-01\n");
    assert!(p.contains("2026-01-01"), "{p}");
    assert!(!p.contains("<substitution_reference"), "{p}");
}

#[test]
fn undefined_substitution_stays_as_reference() {
    let p = pseudo("Use |missing| here.");
    assert!(p.contains("<substitution_reference"), "{p}");
}

#[test]
fn footnote_and_citation_references() {
    let p = pseudo("Refs [1]_ [#]_ [*]_ [CIT]_.");
    assert!(p.contains("<footnote_reference refname=\"1\">"), "{p}");
    assert!(p.contains("auto=\"1\""), "{p}");
    assert!(p.contains("auto=\"*\""), "{p}");
    assert!(p.contains("<citation_reference refname=\"cit\">"), "{p}");
}

// ----- integration with block contexts --------------------------------------

#[test]
fn inline_in_titles_terms_fields_lines_and_tables() {
    let src = "A *Fancy* Title\n===============\n\nterm *with* emphasis\n    definition with ``code``\n\n:field: a **bold** value\n\n| a *line*\n\n+---+\n| *c* |\n+---+\n";
    let p = pseudo(src);
    assert_eq!(p.matches("<emphasis>").count(), 4, "{p}"); // title, term, line, table
    assert!(p.contains("<strong>"), "{p}");
    assert!(p.contains("<literal>"), "{p}");
}

#[test]
fn prolog_and_epilog_content_is_inline_parsed() {
    let opts = ParseOptions {
        prolog: Some(".. |x| replace:: **PRO**".into()),
        epilog: Some(".. _t: https://example.org".into()),
        ..ParseOptions::default()
    };
    let out = parse_with_options("Text |x| and t_.\n", opts);
    let p = out.document.to_pseudoxml().unwrap();
    assert!(p.contains("<strong>"), "{p}"); // from prolog substitution definition
    assert!(p.contains("PRO"), "{p}");
    assert!(p.contains("refuri=\"https://example.org\""), "{p}");
}

// ----- span invariants through full parse -----------------------------------

#[test]
fn all_inline_spans_are_within_source() {
    let src = "Title *em* ``lit`` :math:`x` [1]_ |s| `title` https://e.com/a\n\n.. |s| replace:: s\n";
    let out = parse(src);
    let len = src.len() as u32;
    for (_, node) in out.document.iter() {
        if let Some(span) = node.span {
            assert!(span.start <= span.end, "inverted span on {:?}", node.kind);
            assert!(span.end <= len, "span {span:?} outside source of {len} bytes on {:?}", node.kind);
        }
    }
}
