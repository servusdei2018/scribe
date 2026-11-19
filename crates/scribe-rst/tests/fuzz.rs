//! Adversarial / property tests for the Phase 05 inline parser.
//!
//! Every generated case must (a) not panic in either the pure recogniser or
//! the full block+inline parse, (b) emit only in-bounds byte spans, and (c)
//! emit top-level inline nodes in non-decreasing source order with nested
//! child spans contained in their parent. 1024 cases are generated per
//! property by the explicit `ProptestConfig`.

use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use scribe_ast::{NodeKind, Span};
use scribe_rst::inline::{parse_inline, InlineOptions, InlineSource};
use scribe_rst::{parse, parse_with_options, ParseOptions};

/// Number of generated cases per property. Defaults to the phase-05 historical
/// value of 1024, but honours `PROPTEST_CASES` so the Phase 29 stress campaign
/// can raise it to 10,000+ without editing the source.
fn fuzz_cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(1024)
}

/// Delimiters, escapes, brackets and whitespace in one alphabet — this hits
/// unbalanced/nested markup, backslash runs, backticks and angle brackets.
/// The alphabet intentionally includes multi-byte Unicode (em-dash, CJK,
/// accented Latin, a combining mark and an emoji) so byte-vs-token span bugs
/// surface frequently rather than only under a rare random seed.
fn markup_string() -> impl Strategy<Value = String> {
    let alphabet: Vec<char> = "*`|_[]<>:~.@\\ \n\t!(){}'\"#?=&/+;abcXYZ09"
        .chars()
        .chain(
            "\u{2014}\u{2015}\u{201c}\u{201d}\u{00e9}\u{4e2d}\u{65e5}\u{0301}\u{1f44d}\u{ff01}"
                .chars(),
        )
        .collect();
    prop::collection::vec(prop::sample::select(alphabet), 0..160)
        .prop_map(|chars| chars.into_iter().collect())
}

/// Titles made mostly of multi-byte characters (em-dashes, CJK, accented
/// Latin, combining marks, emoji). Used to build explicit-title phrase
/// references and roles where a byte length must never be used as a token
/// index.
fn unicode_title_fragment() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "———".to_owned(),
        "中文标题".to_owned(),
        "café—東京".to_owned(),
        "éèê".to_owned(),
        "👍🎉".to_owned(),
        "a\u{0301}b\u{0301}c".to_owned(),
        "plain".to_owned(),
    ])
}

/// `` `Title <target>`_ `` / `` `Title <target>`__ `` with a multi-byte title.
fn phrase_ref_string() -> impl Strategy<Value = String> {
    (
        prop::sample::select(vec!["", "lead ", "x", "[a] "]),
        unicode_title_fragment(),
        prop::sample::select(vec!["", " "]),
        prop::sample::select(vec![
            "target".to_owned(),
            "https://example.org/é".to_owned(),
            "alias_".to_owned(),
            "中文".to_owned(),
        ]),
        prop::sample::select(vec!["_", "__"]),
        prop::sample::select(vec!["", "}", ".", ") "]),
    )
        .prop_map(|(lead, title, gap, target, suffix, trail)| {
            format!("{lead}`{title}{gap}<{target}>`{suffix}{trail}")
        })
}

/// Explicit-title cross-reference roles with a multi-byte title/target.
fn xref_role_string() -> impl Strategy<Value = String> {
    (
        prop::sample::select(vec![":ref:", ":py:func:", ":doc:", ":std:term:"]),
        unicode_title_fragment(),
        prop::sample::select(vec!["target", "https://x/中", "mod.func"]),
    )
        .prop_map(|(role, title, target)| format!("{role}`{title} <{target}>`"))
}

/// Arbitrary Unicode, including multi-byte characters and control bytes.
fn unicode_string() -> impl Strategy<Value = String> {
    prop::string::string_regex("[\\p{L}\\p{N}\\p{P}\\p{S}\\p{Z}\\n\\t\\\\]{0,180}").expect("valid regex")
}

fn check_inline_tree(
    nodes: &[scribe_rst::InlineNode],
    source_len: u32,
    parent: Option<scribe_rst::InlineNode>,
) -> Result<(), TestCaseError> {
    let mut prev_start = 0u32;
    for node in nodes {
        if node.kind == NodeKind::Generated {
            continue; // side-table sentinel, never materialised
        }
        let span = node.span;
        prop_assert!(span.start <= span.end, "inverted span on {:?}: {span:?}", node.kind);
        prop_assert!(
            span.end <= source_len,
            "out-of-bounds span {span:?} on {:?} (source len {source_len})",
            node.kind
        );
        prop_assert!(
            span.start >= prev_start,
            "out-of-order spans: {prev_start} then {span:?} on {:?}",
            node.kind
        );
        if let Some(parent) = &parent {
            prop_assert!(
                span.start >= parent.span.start && span.end <= parent.span.end,
                "child span {span:?} outside parent {:?}",
                parent.span
            );
        }
        prev_start = span.start;
        check_inline_tree(&node.children, source_len, Some(node.clone()))?;
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(fuzz_cases()))]

    #[test]
    fn pure_recogniser_never_panics_and_spans_are_valid(text in markup_string()) {
        let source = InlineSource::single(&text, 0);
        let result = parse_inline(&source, &InlineOptions::default());
        check_inline_tree(&result.nodes, text.len() as u32, None)?;
        // Re-run the same input at a nonzero base offset. A synthesised/empty
        // node must carry a span clamped to its enclosing construct, never the
        // global `Span { 0, 0 }`; that only becomes observable with a base > 0.
        const BASE: u32 = 97;
        let shifted = InlineSource::single(&text, BASE);
        let shifted_result = parse_inline(&shifted, &InlineOptions::default());
        check_inline_tree(&shifted_result.nodes, BASE + text.len() as u32, None)?;
    }

    #[test]
    fn full_parse_never_panics_and_spans_are_still_valid(text in markup_string()) {
        let doc = parse(&text).document;
        let len = text.len() as u32;
        for (_, node) in doc.iter() {
            if let Some(span) = node.span {
                prop_assert!(span.start <= span.end, "inverted span on {:?}", node.kind);
                prop_assert!(span.end <= len, "out-of-bounds span {span:?} on {:?}", node.kind);
            }
        }
    }

    #[test]
    fn arbitrary_unicode_never_panics(text in unicode_string()) {
        let source = InlineSource::single(&text, 0);
        let result = parse_inline(&source, &InlineOptions::default());
        check_inline_tree(&result.nodes, text.len() as u32, None)?;
        let _ = parse(&text).document.to_pseudoxml();
    }

    #[test]
    fn prolog_epilog_augmentation_never_panics(
        body in markup_string(),
        prolog in markup_string(),
        epilog in markup_string(),
    ) {
        let options = ParseOptions {
            prolog: Some(prolog),
            epilog: Some(epilog),
            ..ParseOptions::default()
        };
        let output = parse_with_options(&body, options);
        let _ = output.document.to_pseudoxml();
    }

    #[test]
    fn very_long_tokens_do_not_panic(
        ch in prop::sample::select(vec!['*', '`', '|', '_', '[', '\\', 'a']),
        n in 500usize..4000,
    ) {
        let text: String = std::iter::repeat_n(ch, n).collect();
        let source = InlineSource::single(&text, 0);
        let result = parse_inline(&source, &InlineOptions::default());
        check_inline_tree(&result.nodes, text.len() as u32, None)?;
        let _ = parse(&text);
    }

    /// Multi-byte explicit-title phrase references. A byte length used as a
    /// token index here produces a title child past its reference parent; check
    /// at several base offsets so a leaked absolute-0 span cannot hide.
    #[test]
    fn unicode_phrase_references_have_contained_spans(text in phrase_ref_string()) {
        for &base in &[0u32, 97u32, 1000u32] {
            let source = InlineSource::single(&text, base);
            let result = parse_inline(&source, &InlineOptions::default());
            check_inline_tree(&result.nodes, base + text.len() as u32, None)?;
        }
    }

    /// Multi-byte explicit-title xref roles go through the payload/span
    /// helpers; their title/target sub-spans must stay inside the xref node.
    #[test]
    fn unicode_xref_roles_have_contained_spans(text in xref_role_string()) {
        for &base in &[0u32, 97u32, 1000u32] {
            let source = InlineSource::single(&text, base);
            let result = parse_inline(&source, &InlineOptions::default());
            check_inline_tree(&result.nodes, base + text.len() as u32, None)?;
        }
    }

    /// Phase 33+ AST nodes (`TitleReference`, `Classifier`, `OptionGroup`)
    /// are produced by ordinary RST constructs. Generated combinations must
    /// parse without panicking and keep every byte span in bounds, including
    /// at a nonzero base offset (which exposes leaked absolute-zero spans).
    #[test]
    fn new_ast_node_constructs_have_valid_spans(
        fragment in prop::sample::select(vec![
            "A `title reference` and another `one`.\n".to_owned(),
            "term one\n  : classifier A\n  : classifier B\n\n  definition body\n".to_owned(),
            "An option list follows:\n\n--alpha   first option\n-b, --beta  second option\n".to_owned(),
            ":ref:`a title <target>`\n".to_owned(),
        ]),
        repeat in 1usize..6,
    ) {
        let text = fragment.repeat(repeat);
        let doc = parse(&text).document;
        let len = text.len() as u32;
        for (_, node) in doc.iter() {
            if let Some(span) = node.span {
                prop_assert!(span.start <= span.end, "inverted span on {:?}: {span:?}", node.kind);
                prop_assert!(span.end <= len, "out-of-bounds span {span:?} on {:?}", node.kind);
            }
        }
        let _ = doc.to_pseudoxml();
    }
}

// ----- deterministic regressions -------------------------------------------

/// The exact shrunk input from the Phase 05 fuzz failure produced an inline
/// literal `Span { 3, 7 }` whose empty text child had the default `Span { 0, 0 }`,
/// i.e. outside its parent. Assert the child is now clamped to the construct.
#[test]
fn regression_exact_failing_input_child_span_is_contained() {
    let src = InlineSource::single("||[````]", 0);
    let result = parse_inline(&src, &InlineOptions::default());
    let literal = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Literal)
        .expect("input must yield an inline literal");
    assert_eq!(literal.span, Span::new(3, 7));
    assert_eq!(literal.children.len(), 1);
    let child = &literal.children[0];
    assert_eq!(child.span, Span::new(5, 5));
    assert!(
        child.span.start >= literal.span.start && child.span.end <= literal.span.end,
        "child {:?} outside parent {:?}",
        child.span,
        literal.span
    );
}

/// Adversarial neighbours of the failing input: empty/whitespace payloads,
/// unbalanced and adjacent backticks/brackets, empty labels and empty role
/// payloads. Each is checked at base 0 and at a nonzero base so a leaked
/// global-zero span cannot hide.
#[test]
fn regression_empty_synthesised_child_spans_are_contained() {
    const CASES: &[&str] = &[
        "||[````]", // exact failing input
        "``",       // bare literal start
        "````",     // two adjacent literal starts
        "`` ``",    // whitespace-only literal payload
        "**",       // bare strong start
        "****",     // two adjacent strong starts
        "* *",      // whitespace-only emphasis payload
        "`",        // bare interpreted-text start
        "*a*",      // ordinary emphasis (nonzero child span)
        "` `",      // whitespace-only interpreted payload
        "||",       // adjacent substitution delimiters
        "| |",      // whitespace-only substitution
        "|x|_",     // substitution reference suffix
        "|x|__",    // anonymous substitution reference suffix
        "[``]",     // brackets around a literal start
        "[]_",      // empty footnote label
        "[#]_",     // auto footnote label
        "[*]_",     // symbol footnote label
        ":ref:`x`", // xref role
        ":math:``", // empty role payload
        ":py:func:`~a.b`", // modifier-only xref title
        "` <target>`_",    // empty phrase-reference title
        "`Title <>`_",     // empty phrase-reference target
        "_`name`",         // inline target
    ];
    for &text in CASES {
        for &base in &[0u32, 97u32] {
            let source = InlineSource::single(text, base);
            let result = parse_inline(&source, &InlineOptions::default());
            if let Err(err) = check_inline_tree(&result.nodes, base + text.len() as u32, None) {
                panic!("invalid inline spans for {text:?} at base {base}: {err:?}");
            }
        }
    }
}

/// The exact shrunk input behind the second span bug: an em-dash is three
/// bytes but one token. The old code computed the title child's end as
/// `body + raw[..lt].len()` (bytes), overshooting the reference parent.
#[test]
fn regression_unicode_title_byte_length_is_not_a_token_index() {
    const TEXT: &str = "`———<>`_}";
    for &base in &[0u32, 97u32, 1000u32] {
        let source = InlineSource::single(TEXT, base);
        let result = parse_inline(&source, &InlineOptions::default());
        if let Err(err) = check_inline_tree(&result.nodes, base + TEXT.len() as u32, None) {
            panic!("invalid inline spans for {TEXT:?} at base {base}: {err:?}");
        }
        let reference = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Reference)
            .expect("input must yield a phrase reference");
        // 3 em-dashes (9 bytes) + `<` + `>` => target payload spans bytes
        // [1, 12); the `_` suffix extends the reference to [1, 14). The title
        // child covers only the three em-dashes: [1, 10).
        assert_eq!(reference.span, Span::new(base + 1, base + 14));
        assert_eq!(reference.children.len(), 1);
        assert_eq!(reference.children[0].span, Span::new(base + 1, base + 10));
    }
}

/// A corpus of multi-byte Unicode explicit titles, embedded links and xref
/// roles at several absolute base offsets. Every child span must be contained
/// in its parent and every span in-bounds.
#[test]
fn regression_multibyte_explicit_title_corpus() {
    const CASES: &[&str] = &[
        "`———<>`_}",                       // exact failing input
        "`——— <target>`_",                 // em-dash title + explicit target
        "`中文标题 <https://example.org/é>`_", // CJK title + Unicode URI
        "`café—東京 <alias_>`_",           // accented + CJK + em-dash
        "`👍🎉 <target>`__",               // emoji title, anonymous ref
        "`a\u{0301}b\u{0301}c <t>`_",      // combining marks
        ":ref:`中文 <target>`",            // xref role, multi-byte title
        ":py:func:`~模組.函數 <pkg.mod.func>`", // xref role with modifiers
        "lead `——— <t>`_ tail",            // embedded in surrounding text
        "[a] `标题 <t>`_ ) ",               // adversarial delimiters around it
    ];
    for &text in CASES {
        for &base in &[0u32, 97u32, 1000u32, 65_000u32] {
            let source = InlineSource::single(text, base);
            let result = parse_inline(&source, &InlineOptions::default());
            if let Err(err) = check_inline_tree(&result.nodes, base + text.len() as u32, None) {
                panic!("invalid inline spans for {text:?} at base {base}: {err:?}");
            }
        }
    }
}

/// Tab expansion can make a joined multi-line `InlineSource` offset table
/// non-monotonic (a later line maps to a smaller absolute offset than the
/// preceding one). `abs_span` must still emit well-formed, contained spans.
#[test]
fn regression_non_monotonic_offset_table_spans_are_contained() {
    let text = "_||||||||*      `!\n`       ******";
    let offsets: Vec<u32> = vec![
        2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, // tab-expanded line
        16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, // next line jumps back
    ];
    let source = InlineSource::new(text.to_owned(), offsets);
    let result = parse_inline(&source, &InlineOptions::default());
    if let Err(err) = check_inline_tree(&result.nodes, 31, None) {
        panic!("invalid spans on a non-monotonic offset table: {err:?}");
    }
}

/// Full-document regression for the tab-expanded prolog/epilog case that the
/// materialisation invariant caught: no span may be inverted or out-of-bounds.
#[test]
fn regression_prolog_epilog_tab_expansion_spans_in_bounds() {
    let options = ParseOptions {
        prolog: Some(String::new()),
        epilog: Some("_||||||||*\t`!\n`\t******".to_owned()),
        ..ParseOptions::default()
    };
    let output = parse_with_options("", options);
    // "\n\n" + epilog (22 bytes) + "\n" = 25 bytes.
    let len = 25u32;
    for (_, node) in output.document.iter() {
        if let Some(span) = node.span {
            assert!(span.start <= span.end, "inverted span on {:?}: {span:?}", node.kind);
            assert!(span.end <= len, "out-of-bounds span {span:?} on {:?}", node.kind);
        }
    }
}
