//! Adversarial/fuzz tests: arbitrary input must never panic and every emitted
//! span must be in bounds, non-inverted and on a UTF-8 boundary.

mod common;

use common::assert_all_spans_in_bounds;
use proptest::prelude::*;

/// Default case count from Phase 07; `PROPTEST_CASES` raises it for the
/// Phase 29 stress campaign without weakening the property assertions.
fn fuzz_cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(512)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(fuzz_cases()))]

    #[test]
    fn arbitrary_ascii_never_panics(source in "[\\x20-\\x7e\\n\\t]{0,600}") {
        let output = scribe_myst::parse(&source).expect("parse must not fail");
        assert_all_spans_in_bounds(&source, &output.document);
        let _ = output.document.to_pseudoxml().unwrap();
        let _ = output.document.to_json().unwrap();
    }

    #[test]
    fn markup_heavy_input_never_panics(source in "[{}\\[\\]()`$:*-_|~!#<>\\\\\\n ]{0,400}") {
        let output = scribe_myst::parse(&source).expect("parse must not fail");
        assert_all_spans_in_bounds(&source, &output.document);
        let _ = output.document.to_pseudoxml().unwrap();
    }

    #[test]
    fn arbitrary_unicode_never_panics(source in "\\PC{0,300}") {
        let output = scribe_myst::parse(&source).expect("parse must not fail");
        assert_all_spans_in_bounds(&source, &output.document);
        let _ = output.document.to_pseudoxml().unwrap();
    }

    #[test]
    fn directive_and_role_fragments_never_panic(
        prefix in "(`{3,5}|:::{1,3}|\\$+)",
        name in "[a-z-]{1,8}",
        body in "\\PC{0,80}",
    ) {
        let source = format!("{prefix}{{{name}}}\n{body}\n{prefix}\n");
        let output = scribe_myst::parse(&source).expect("parse must not fail");
        assert_all_spans_in_bounds(&source, &output.document);
        let _ = output.document.to_pseudoxml().unwrap();
    }

    #[test]
    fn frontmatter_fragments_never_panic(body in "[ -~\\n]{0,200}") {
        let source = format!("---\n{body}\n---\n\n# Title\n");
        let output = scribe_myst::parse(&source).expect("parse must not fail");
        assert_all_spans_in_bounds(&source, &output.document);
    }
}

#[test]
fn degenerate_inputs_are_safe() {
    let cases = [
        "",
        "\n",
        "```",
        "````",
        ":::",
        "{ref}`",
        "{ref}",
        "$",
        "$$",
        "$$$",
        "---\n---\n",
        "---\n",
        "(...)=\n",
        "[]()",
        "![x]()",
        "|a|\n|-|\n",
        "term\n:\n",
        "\u{0}\u{0}\u{0}",
    ];
    for source in cases {
        let out = scribe_myst::parse(source).expect("never fails");
        assert_all_spans_in_bounds(source, &out.document);
        let _ = out.document.to_pseudoxml().unwrap();
    }
}

#[test]
fn long_input_is_bounded() {
    let source = format!("{}\n\n{}\n", "word ".repeat(2000), "# H\n\nP.\n".repeat(200));
    let out = scribe_myst::parse(&source).expect("long input");
    assert_all_spans_in_bounds(&source, &out.document);
    // No panic, and the output is serializable (arena ids stable).
    let json = out.document.to_json().unwrap();
    let restored = scribe_ast::Document::from_json(&json).unwrap();
    assert_eq!(restored.arena_len(), out.document.arena_len());
}
