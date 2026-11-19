//! Phase 29 fuzz target for the MyST Markdown parser.
//!
//! Complements the Phase 07 `adversarial.rs` suite with a stress-adjustable
//! campaign that asserts, for every generated Markdown document:
//!   * parsing, resolution, pseudo-XML and JSON round-trip never panic;
//!   * every emitted byte span is in bounds, non-inverted and on a UTF-8
//!     character boundary, and top-level block spans are ordered;
//!   * arena/resource usage is linear in the input length (bounded).
//!
//! Default case count is 2048 per property; `PROPTEST_CASES` raises it for the
//! stress campaign.

mod common;

use common::assert_all_spans_in_bounds;
use proptest::prelude::*;
use scribe_ast::{Document, NodeId};

fn fuzz_cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(2048)
}

fn markdown_string() -> impl Strategy<Value = String> {
    let alphabet: Vec<char> = "#*_`~{}[]()<>!|:-$+ \\n\t0123456789abcXYZ中é\u{2014}"
        .chars()
        .collect();
    prop::collection::vec(prop::sample::select(alphabet), 0..400)
        .prop_map(|chars| chars.into_iter().collect())
}

fn myst_document() -> impl Strategy<Value = String> {
    (
        prop::sample::select(vec!["#", "##", "-", "1.", ">", "```", ":::", "---", "(...)=\n"]),
        markdown_string(),
        prop::sample::select(vec!["", "\n```", "\n:::"]),
    )
        .prop_map(|(prefix, body, suffix)| format!("{prefix} {body}\n{suffix}\n"))
}

/// Top-level block nodes of a document must have non-decreasing span starts.
fn assert_top_level_spans_ordered(source: &str, doc: &Document) {
    let mut previous = 0u32;
    if let Ok(children) = doc.children(doc.root) {
        for &id in children {
            if let Ok(node) = doc.get(id) {
                if let Some(span) = node.span {
                    assert!(
                        span.start >= previous,
                        "out-of-order top-level spans: {previous} then {}",
                        span.start
                    );
                    previous = span.start;
                }
            }
        }
    }
    let _ = source;
}

fn assert_bounded(source: &str, doc: &Document) {
    // A streaming lowerer emits at most a small constant number of arena nodes
    // per source byte. This is the resource-usage invariant: output size is
    // linear in input size, never super-linear (no quadratic expansion).
    let bound = (source.len() as u64) * 8 + 64;
    assert!(
        doc.arena_len() as u64 <= bound,
        "arena {} exceeds bound {bound} for {} input bytes",
        doc.arena_len(),
        source.len()
    );
}

fn exercise(source: &str) {
    let output = scribe_myst::parse(source).expect("MyST parse must not fail");
    assert_all_spans_in_bounds(source, &output.document);
    assert_top_level_spans_ordered(source, &output.document);
    assert_bounded(source, &output.document);
    let _ = output.document.to_pseudoxml().expect("pseudo-XML emission must not fail");
    let json = output.document.to_json().expect("JSON emission must not fail");
    let restored = Document::from_json(&json).expect("JSON round-trip must not fail");
    assert_eq!(restored.arena_len(), output.document.arena_len());
    // Resolution of any emitted directive placeholders must also be panic-free.
    let (_, _report) = scribe_myst::parse_and_resolve(source, Default::default())
        .expect("parse_and_resolve must not fail");
    // Keep NodeId in scope so the structural invariant is documented against
    // the frozen arena id type.
    let _: Option<NodeId> = None;
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(fuzz_cases()))]

    #[test]
    fn arbitrary_markdown_never_panics_and_spans_valid(source in markdown_string()) {
        exercise(&source);
    }

    #[test]
    fn myst_extensions_never_panic(source in myst_document()) {
        prop_assert!(source.len() <= 600, "generator produced an unbounded document");
        exercise(&source);
    }

    #[test]
    fn deeply_nested_blocks_are_bounded(
        depth in 1usize..40,
        marker in prop::sample::select(vec!['>', '-', ' ']),
    ) {
        let line = format!("{}x\n", marker).repeat(depth);
        prop_assert!(line.len() <= 160, "generator produced an unbounded document");
        exercise(&line);
    }

    #[test]
    fn long_flat_input_is_bounded(words in 1usize..1500) {
        let source = format!("{}word\n", "word ".repeat(words));
        prop_assert!(source.len() <= 10_000, "generator produced an unbounded document");
        exercise(&source);
    }
}

// ----- deterministic regressions -------------------------------------------

/// Degenerate and adversarial inputs carried over from the Phase 07
/// `adversarial.rs` suite plus new span-ordering edge cases.
#[test]
fn regression_myst_corpus() {
    const CASES: &[&str] = &[
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
        "# A\n\n# A\n\n# A\n", // duplicate section ids
        "```{note}\n```{warning}\nnested\n```\n```\n",
        "- a\n  - b\n    - c\n",
        ">`a`\n>>**b**\n",
    ];
    for source in CASES {
        exercise(source);
    }
}
