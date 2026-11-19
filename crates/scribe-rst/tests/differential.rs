//! Differential tests against the pinned Sphinx oracle
//! (`sphinx-build -b pseudoxml`).
//!
//! Compared subset (documented contract — see `ARCHITECTURE.md` §8):
//!
//! * node kinds, nesting and pseudo-XML indentation;
//! * all block attributes emitted by the RST parser (`ids`, `names`, `bullet`,
//!   `enumtype`, `prefix`, `suffix`, `start`, `colwidth`, `cols`, `xml:space`, ...);
//! * text-node content for fixtures containing no inline markup.
//!
//! Normalized away (Sphinx adds these *after* parsing): `source`,
//! `translation_progress` and `xmlns:*` on `document`; `force`, `language`,
//! `linenos` on `literal_block`; `classes="doctest"` on `doctest_block`.
//!
//! Fixtures deliberately avoid inline markup (Phase 05) and directives
//! (Phase 06). `classifier` and `option_group` are covered separately by
//! `tests/node_parity.rs` since Phase 34. Four distinct block-construct
//! families are covered: sections/paragraphs/transitions, lists,
//! quotes/literals/doctests, and tables.

mod common;

use common::{normalize, normalize_ours, oracle_pseudoxml, pseudo};

fn assert_parity(source: &str) {
    let expected = oracle_pseudoxml(source);
    let actual = normalize_ours(&pseudo(source));
    let expected = normalize(&expected);
    assert_eq!(
        actual, expected,
        "\n--- scribe ---\n{actual}\n--- oracle ---\n{expected}"
    );
}

const SECTIONS: &str = "Main Title\n==========\n\nIntro paragraph.\n\nSubsection\n----------\n\nBody paragraph.\n\nAnother\n-------\n\nMore text.\n";

const LISTS: &str = "Lists\n=====\n\n* first item\n* second item\n  with continuation\n\n1. one\n2. two\n\n:author: Jane\n:version: 1.0\n\nterm\n    definition body\n";

const QUOTES_LITERALS: &str = "Blocks\n======\n\n| line one\n| line two\n\n    Quoted paragraph.\n\n    -- Author Name\n\nIntro::\n\n    literal line\n      deeper line\n\n>>> 1 + 1\n2\n";

const TABLES: &str = "Tables\n======\n\n+---+---+\n| a | b |\n+===+===+\n| 1 | 2 |\n+---+---+\n\n=====  =====\ncol 1  col 2\n=====  =====\nthree  four\n=====  =====\n";

#[test]
fn parity_sections_paragraphs_and_section_hierarchy() {
    assert_parity(SECTIONS);
}

#[test]
fn parity_lists_and_definition_lists() {
    assert_parity(LISTS);
}

#[test]
fn parity_line_blocks_quotes_literals_and_doctests() {
    assert_parity(QUOTES_LITERALS);
}

#[test]
fn parity_grid_and_simple_tables() {
    assert_parity(TABLES);
}

#[test]
fn oracle_fixture_subset_uses_real_sphinx_build() {
    // Guards the harness itself: the oracle must produce a normalizable
    // pseudoxml document containing the constructs above.
    let generated = oracle_pseudoxml(SECTIONS);
    assert!(generated.contains("<document"), "{generated}");
    assert!(generated.contains("<section"), "{generated}");
    assert!(generated.contains("<paragraph>"), "{generated}");
    assert!(!normalize(&generated).contains("translation_progress"));
}
