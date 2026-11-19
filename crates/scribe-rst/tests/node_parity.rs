//! Phase 34 parity tests for the three upstream node gaps:
//! `title_reference`, `classifier` and `option_group`.
//!
//! Every fixture is compared byte-for-byte (after the shared post-parse
//! attribute normalization) against `sphinx-build -b pseudoxml`, so the tests
//! fail if scribe regresses to a synthetic `inline` representation or emits a
//! different tree shape / attribute set.

mod common;

use common::{normalize_ours, oracle_pseudoxml, pseudo};

fn assert_parity(source: &str) {
    let expected = oracle_pseudoxml(source);
    let actual = normalize_ours(&pseudo(source));
    assert_eq!(actual, expected, "\n--- scribe ---\n{actual}\n--- oracle ---\n{expected}");
}

#[test]
fn title_reference_pseudoxml_matches_oracle() {
    assert_parity("Title\n=====\n\nDefault `interpreted text` here.\n");
}

#[test]
fn multiple_title_references_pseudoxml_matches_oracle() {
    assert_parity("Refs\n====\n\nOne `first` and two `second`.\n");
}

#[test]
fn classifier_pseudoxml_matches_oracle() {
    assert_parity(
        "Terms\n=====\n\nterm : classifier one : classifier two\n    Definition body.\n",
    );
}

#[test]
fn option_group_pseudoxml_matches_oracle() {
    assert_parity(
        "Opts\n====\n\n-a, --all <file>  Description of all.\n-b             Other option.\n",
    );
}

#[test]
fn option_group_delimiters_pseudoxml_matches_oracle() {
    assert_parity(
        "Flags\n=====\n\n--file=NAME  equals delimiter\n-o FILE      space delimiter\n",
    );
}
