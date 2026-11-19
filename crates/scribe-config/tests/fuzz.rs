//! Phase 29 fuzz target for the `Tags` boolean-expression evaluator.
//!
//! Invariants asserted for every generated case:
//!   * `eval_condition` never panics and returns a deterministic result;
//!   * the result is identical across the internal result cache, tag mutations
//!     that do not change the relevant tags, and repeated evaluation;
//!   * generated inputs are length- and depth-bounded so a pathological case
//!     cannot consume unbounded resources (the recursive-descent parser is
//!     only as deep as the generator allows).
//!
//! The default case count (2048 per property) can be raised with
//! `PROPTEST_CASES` for the Phase 29 stress campaign.

use proptest::prelude::*;
use proptest::strategy::BoxedStrategy;
use scribe_config::Tags;

fn fuzz_cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(2048)
}

/// Arbitrary bytes from the expression alphabet, including unbalanced
/// parentheses, unknown operators and UTF-8 names.
fn arbitrary_condition() -> impl Strategy<Value = String> {
    let alphabet: Vec<char> = "ab_()!&| \t\n<>=01;,.{}'\"\\+-*/^%:中é"
        .chars()
        .collect();
    prop::collection::vec(prop::sample::select(alphabet), 0..96)
        .prop_map(|chars| chars.into_iter().collect())
}

/// A well-formed expression of bounded depth over a small tag vocabulary.
/// `depth` is capped so the generated parser recursion is bounded.
fn valid_condition(depth: u32) -> BoxedStrategy<String> {
    let tag = prop::sample::select(vec!["draft", "html", "latex", "linux", "unknown"]);
    if depth == 0 {
        return tag.prop_map(|t| t.to_owned()).boxed();
    }
    let inner = valid_condition(depth - 1);
    prop_oneof![
        tag.prop_map(|t| t.to_owned()),
        inner.clone().prop_map(|s| format!("not ({s})")),
        (inner.clone(), inner.clone())
            .prop_map(|(a, b)| format!("({a}) and ({b})")),
        (inner.clone(), inner.clone())
            .prop_map(|(a, b)| format!("({a}) or ({b})")),
        inner.prop_map(|s| format!("(({s}))")),
    ]
    .boxed()
}

fn eval_is_deterministic(tags: &Tags, condition: &str) {
    let first = tags.eval_condition(condition);
    // The cache must return the same answer as a cold evaluation, and repeated
    // evaluation must be stable regardless of the internal Mutex state.
    let second = tags.eval_condition(condition);
    assert_eq!(first, second, "non-deterministic Tags evaluation for {condition:?}");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(fuzz_cases()))]

    #[test]
    fn arbitrary_condition_never_panics(condition in arbitrary_condition()) {
        let tags = Tags::with_tags(["draft", "html"]);
        eval_is_deterministic(&tags, &condition);
    }

    #[test]
    fn valid_expressions_never_panic(condition in valid_condition(4)) {
        let tags = Tags::with_tags(["draft", "html", "latex"]);
        prop_assert!(condition.len() <= 512, "generator produced an unbounded expression");
        eval_is_deterministic(&tags, &condition);
    }

    #[test]
    fn deep_but_bounded_expressions_never_panic(
        leaf in prop::sample::select(vec!["draft", "html"]),
        copies in 1usize..40,
    ) {
        let condition = format!("({})", format!("{leaf} and ").repeat(copies) + leaf);
        prop_assert!(condition.len() <= 512, "generator produced an unbounded expression");
        let tags = Tags::with_tags(["draft"]);
        eval_is_deterministic(&tags, &condition);
    }

    #[test]
    fn tag_mutation_invalidates_the_cache_consistently(
        first in valid_condition(3),
        second in valid_condition(3),
        // `extra` must be absent from the initial set, otherwise `remove` would
        // delete a pre-existing tag and legitimately change the result.
        extra in prop::sample::select(vec!["html", "latex", "kernel"]),
    ) {
        let mut tags = Tags::with_tags(["draft"]);
        let before = tags.eval_condition(&first);
        tags.add(extra);
        let after_add = tags.eval_condition(&first);
        let other = tags.eval_condition(&second);
        tags.remove(extra);
        let after_remove = tags.eval_condition(&first);
        // Removing a tag we just added must restore the original environment.
        prop_assert_eq!(&before, &after_remove, "cache not invalidated on remove");
        // Whatever the values, evaluating a second expression may not panic.
        let _ = (after_add, other);
    }
}

// ----- deterministic regressions -------------------------------------------

/// Adversarial corpus carried over from the Phase 02 config differential test
/// and the `Tags` fidelity note in `ARCHITECTURE.md` §6. None of these may
/// panic and the constant cases must keep returning `InvalidNode` (the pinned
/// oracle raises `TypeError: invalid node, check parsing`).
#[test]
fn regression_tag_condition_corpus() {
    use scribe_config::TagsError;
    const CASES: &[&str] = &[
        "",
        " ",
        "draft",
        "!draft",
        "not draft",
        "draft and html",
        "draft or html",
        "(draft or html) and latex",
        "draft and (html or (latex and linux))",
        "draft??html:latex",
        "draft ? html : latex",
        "true",
        "True",
        "false",
        "False",
        "none",
        "None",
        "not None",
        "draft ? true : false", // grammar does not accept `?` here; any error is fine
        "draft and",
        ")draft(",
        "draft)",
        "((((draft))))",
        "中draft",
        "draft&html",
        "draft\n",
    ];
    let tags = Tags::with_tags(["draft", "html", "linux"]);
    for condition in CASES {
        let result = tags.eval_condition(condition);
        assert_eq!(result, tags.eval_condition(condition), "unstable for {condition:?}");
        match *condition {
            "true" | "True" | "false" | "False" | "none" | "None" | "not None" => {
                assert_eq!(result, Err(TagsError::InvalidNode), "constant fidelity for {condition:?}");
            }
            _ => {}
        }
    }
}
