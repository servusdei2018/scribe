//! Differential tests for Phase 05 inline markup.
//!
//! # Exact subset compared (byte-for-byte after normalization)
//!
//! * emphasis, strong, inline literals (incl. embedded backticks and
//!   whitespace preservation);
//! * backslash escapes (`\*`, `` \` ``, `\\`, …);
//! * Docutils built-in roles `:math:`, `:literal:`, `:emphasis:`, `:strong:`
//!   and the default (title-reference) role;
//! * standalone URI / email autolinks;
//! * named, anonymous and phrase hyperlink references, including embedded
//!   `<URI>` targets, resolved against block-level target definitions;
//! * simple substitution references resolved from `replace` definitions.
//!
//! Normalized away: the standard Sphinx post-parse attributes stripped by
//! `common::normalize`. Since Phase 34 scribe emits a real `title_reference`
//! node (matching upstream byte-for-byte); `normalize_inline` additionally
//! maps the legacy `<inline classes="title-reference">` spelling so old
//! snapshots remain comparable.
//!
//! # Profile-only subset (documented limitation)
//!
//! Footnote/citation definitions are still `Pending` nodes (Phase 06) and
//! Sphinx resolves domain roles in post-transforms (Phases 10–14). For those
//! two families we compare the *inline reference profile* — node kinds and
//! text content of references — rather than the full doctree, and document
//! the divergence below.

mod common;

use common::{normalize, normalize_ours, oracle_pseudoxml, pseudo};

fn assert_exact_parity(source: &str) {
    let expected = normalize(&oracle_pseudoxml(source));
    let actual = normalize_inline(&normalize_ours(&pseudo(source)));
    assert_eq!(
        actual, expected,
        "\n--- scribe ---\n{actual}\n--- oracle ---\n{expected}"
    );
}

/// Maps scribe's title-reference representation onto upstream's node name.
fn normalize_inline(text: &str) -> String {
    text.replace(
        "<inline classes=\"title-reference\">",
        "<title_reference>",
    )
}

const EMPHASIS: &str = "Inline\n======\n\nA paragraph with *emphasis*, **strong**, ``literal``, ``a `` b`` and more.\n\nEscapes: \\*not\\* \\`not\\` \\\\ \\|x\\|.\n\nNested: **bold with *inner* text** and ``code with * stars``.\n";

const ROLES: &str = "Roles\n=====\n\nMath :math:`E=mc^2`, literal :literal:`x`, emphasis :emphasis:`y`, strong :strong:`z`.\n\nDefault: `interpreted text`.\n";

const REFS: &str = "Links\n=====\n\nNamed link_ and `phrase link`_ and `Title <https://example.org/t>`_.\n\nStandalone https://example.com/a?q=1#f and user@example.co.uk.\n\n.. _link: https://example.org/a\n.. _phrase link: https://example.org/p\n";

const SUBS: &str = "Subs\n====\n\nDate |date| here.\n\n.. |date| replace:: 2026-01-01\n";

#[test]
fn byte_exact_emphasis_literals_and_escapes() {
    assert_exact_parity(EMPHASIS);
}

#[test]
fn byte_exact_docutils_roles() {
    assert_exact_parity(ROLES);
}

#[test]
fn byte_exact_hyperlink_references() {
    assert_exact_parity(REFS);
}

#[test]
fn byte_exact_substitution_reference() {
    assert_exact_parity(SUBS);
}

/// Extracts reference-like inline nodes (tag + inner text) from a paragraph
/// in a pseudoxml document. This is the profile compared for families whose
/// full doctree is produced by later phases' transforms.
fn reference_profile(pseudoxml: &str) -> Vec<(String, String)> {
    let mut profile = Vec::new();
    let mut current: Option<(String, String)> = None;
    let mut in_first_paragraph = false;
    let mut done = false;
    for line in pseudoxml.lines() {
        let indent = line.len() - line.trim_start_matches(' ').len();
        let content = line[indent..].trim_end();
        if indent == 4 {
            if content.starts_with("<paragraph") && !done {
                in_first_paragraph = true;
                continue;
            }
            if in_first_paragraph {
                // Any other block-level tag closes the paragraph region.
                in_first_paragraph = false;
                done = true;
            }
            continue;
        }
        if !in_first_paragraph {
            continue;
        }
        if content.starts_with('<') && content.ends_with('>') {
            let tag = content.trim_start_matches('<').trim_end_matches('>');
            let tag = tag.split_whitespace().next().unwrap_or(tag);
            if matches!(
                tag,
                "footnote_reference"
                    | "citation_reference"
                    | "reference"
                    | "substitution_reference"
                    | "pending_xref"
                    | "problematic"
                    | "inline"
            ) {
                if let Some(item) = current.take() {
                    profile.push(item);
                }
                current = Some((tag.to_owned(), String::new()));
            }
        } else if indent >= 12 {
            if let Some((_, text)) = current.as_mut() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(content.trim());
            }
        }
    }
    if let Some(item) = current.take() {
        profile.push(item);
    }
    profile
}

#[test]
fn footnote_reference_profile_matches_oracle() {
    let src = "Refs [1]_ and [#]_ and [*]_ and [CIT]_.\n\n.. [1] one\n.. [#] two\n.. [*] three\n.. [CIT] cite\n";
    let oracle_raw = oracle_pseudoxml(src);
    let ours = normalize_profile(reference_profile(&pseudo(src)));
    let oracle = normalize_profile(reference_profile(&oracle_raw));
    assert_eq!(
        ours, oracle,
        "\n--- scribe profile ---\n{ours:?}\n--- oracle profile ---\n{oracle:?}"
    );
}

fn normalize_profile(mut profile: Vec<(String, String)>) -> Vec<(String, String)> {
    for (tag, text) in &mut profile {
        if tag == "footnote_reference" && matches!(text.as_str(), "#" | "*" | "2" | "3") {
            // Sphinx assigns display numbers to auto/symbol footnote refs.
            text.clear();
        }
        // Sphinx turns a resolved citation into an internal `<reference>`
        // wrapping an `<inline>[CIT]</inline>`; map it back to the parse-time
        // citation reference shape.
        if (tag == "reference" || tag == "inline") && text.starts_with('[') && text.ends_with(']') {
            *tag = "citation_reference".to_owned();
            *text = text.trim_matches(|c| c == '[' || c == ']').to_owned();
        }
    }
    profile
        .into_iter()
        .filter(|(tag, text)| !text.is_empty() || tag == "footnote_reference")
        .collect()
}

#[test]
fn sphinx_role_targets_and_titles_match_oracle() {
    // Sphinx resolves xref roles into `<reference>` / `<inline>` / `<literal>`
    // nodes in Phases 10-14. At parse time scribe emits frozen `pending_xref`
    // nodes; the parsed target and displayed title must nevertheless match
    // the oracle's resolved text.
    let src = "Roles :py:class:`~os.path.join`, :py:func:`.mod.func`, :ref:`Title <my-label>`.\n";
    let oracle = oracle_pseudoxml(src);
    let out = scribe_rst::parse(src);
    let metas = out.inline_xrefs();

    assert_eq!(metas.len(), 3);
    assert_eq!(metas[0].1.target, "os.path.join");
    assert_eq!(metas[1].1.target, "mod.func");
    assert!(metas[1].1.specific);
    assert_eq!(metas[2].1.target, "my-label");
    assert!(metas[2].1.explicit_title);

    // Oracle resolves `~os.path.join` to the displayed title `join()` and
    // `.mod.func` to `func()` (add_function_parentheses), while an explicit
    // title is shown verbatim.
    assert!(oracle.contains("join"), "{oracle}");
    assert!(oracle.contains("mod.func()"), "{oracle}");
    assert!(oracle.contains("Title"), "{oracle}");

    let p = out.document.to_pseudoxml().unwrap();
    assert!(p.contains("refdomain=\"py\""), "{p}");
    assert!(p.contains("refexplicit=\"1\""), "{p}");
}
