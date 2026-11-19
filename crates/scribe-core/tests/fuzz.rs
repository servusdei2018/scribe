//! Adversarial / property tests for the Phase 38 `TocTree` doctree expander.
//!
//! The expander consumes untrusted, serialized doctree snapshots (and the
//! toctree entries derived from them) and rebuilds a nested structure. Every
//! generated document must:
//!   * expand without panicking, including cyclic / self / missing / external
//!     and multi-byte entry targets;
//!   * replace or remove exactly the number of top-level toctree nodes it was
//!     given (no toctree stays reachable after the pass);
//!   * emit only in-bounds byte spans and a finite, traversable doctree.
//!
//! Defaults to 1024 cases per property; `PROPTEST_CASES` raises it for the
//! release-campaign sweep. This mirrors the other `tests/fuzz.rs` harnesses.

use proptest::prelude::*;
use scribe_ast::{Document, NodeKind};
use scribe_core::{expand_toctrees, BuildEnvironment, DocumentSnapshot, ToctreeCollection};

fn fuzz_cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(1024)
}

/// Entry targets include a self reference, a missing document, an external
/// URL, an empty target, a cyclic sibling and multi-byte docnames — each
/// stresses a different branch of `make_entry` / `collect_children`.
fn target() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "index".to_owned(),
        "a".to_owned(),
        "b".to_owned(),
        "c".to_owned(),
        "missing".to_owned(),
        "http://example.test/中".to_owned(),
        String::new(),
        "章节".to_owned(),
    ])
}

fn kind() -> impl Strategy<Value = String> {
    prop::sample::select(
        ["doc", "external", "self", "ref", "unknown"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>(),
    )
}

fn entry() -> impl Strategy<Value = (String, String, bool)> {
    (target(), kind(), prop::bool::ANY)
}

/// Populate the environment with a small, possibly cyclic set of child
/// documents so nested expansion (and its cycle guard) is exercised.
fn build_env() -> BuildEnvironment {
    let mut env = BuildEnvironment::new(std::path::PathBuf::from("/tmp"));
    for name in ["a", "b", "c"] {
        let mut child = Document::new(Some(name.to_owned()));
        let root = child.root;
        let section = child.add(NodeKind::Section, Some(root)).unwrap();
        let title = child.add(NodeKind::Title, Some(section)).unwrap();
        let _ = child.new_text(name.to_owned(), Some(title)).unwrap();
        child.get_mut(section).unwrap().attrs.set_list("ids", vec![name.to_owned()]);

        let toc = child.add(NodeKind::Toctree, Some(section)).unwrap();
        // Each child points at the next one and back at index: a cycle.
        let next = match name {
            "a" => "b",
            "b" => "c",
            _ => "index",
        };
        child.get_mut(toc).unwrap().attrs.set_list("entry_refs", vec![next.to_owned(), name.to_owned()]);
        child.get_mut(toc).unwrap().attrs.set_list("entry_kinds", vec!["doc".to_owned(), "self".to_owned()]);
        child.get_mut(toc).unwrap().attrs.set_list("entry_titles", vec![String::new(), String::new()]);

        env.documents.insert(
            name.to_owned(),
            DocumentSnapshot {
                docname: name.to_owned(),
                doctree: Some(child),
                ..Default::default()
            },
        );
    }
    env
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(fuzz_cases()))]

    #[test]
    fn toctree_expansion_never_panics_and_stays_in_bounds(
        entries in prop::collection::vec(entry(), 0..8),
        hidden in prop::bool::ANY,
        includehidden in prop::bool::ANY,
        maxdepth in -2i64..6,
        titlesonly in prop::bool::ANY,
        caption in prop::string::string_regex("[A-Za-z0-9 \u{2014}]{0,24}").unwrap(),
        repeat in 1usize..4,
    ) {
        let mut doc = Document::new(Some("index".to_owned()));
        let root = doc.root;
        for _ in 0..repeat {
            let toc = doc.add(NodeKind::Toctree, Some(root)).unwrap();
            let node = doc.get_mut(toc).unwrap();
            node.attrs.set_list(
                "entry_refs",
                entries.iter().map(|(t, _, _)| t.clone()).collect(),
            );
            node.attrs.set_list(
                "entry_kinds",
                entries.iter().map(|(_, k, _)| k.clone()).collect(),
            );
            node.attrs.set_list(
                "entry_titles",
                entries
                    .iter()
                    .map(|(t, _, titled)| if *titled { t.clone() } else { String::new() })
                    .collect(),
            );
            node.attrs.set_bool("hidden", hidden);
            node.attrs.set_bool("includehidden", includehidden);
            node.attrs.set_bool("titlesonly", titlesonly);
            node.attrs.set_int("maxdepth", maxdepth);
            node.attrs.set_str("caption", caption.clone());
        }

        let env = build_env();
        let collection = ToctreeCollection::default();
        let result = expand_toctrees(&mut doc, "index", &env, &collection)?;

        // Each top-level toctree is either replaced or removed; none is left
        // reachable, and the pass never fabricates more work than it received.
        prop_assert!(result.replaced + result.removed == repeat);
        let reachable_toctrees = doc
            .preorder_ids()
            .into_iter()
            .filter(|&id| doc.get(id).is_ok_and(|n| n.kind == NodeKind::Toctree))
            .count();
        prop_assert_eq!(reachable_toctrees, 0, "a toctree remained reachable");

        // Every emitted span is within the (short) generated source, and the
        // arena remains traversable in a finite pre-order walk.
        let source_len = 5u32; // Some("index")
        let ids = doc.preorder_ids();
        for id in &ids {
            let node = doc.get(*id)?;
            if let Some(span) = node.span {
                prop_assert!(span.start <= span.end, "inverted span on {:?}", node.kind);
                prop_assert!(span.end <= source_len, "out-of-bounds span on {:?}", node.kind);
            }
        }
        prop_assert_eq!(ids.len(), ids.iter().copied().collect::<std::collections::HashSet<_>>().len());
    }
}

// ----- deterministic regression ---------------------------------------------

/// The exact cycle that used to recurse without a guard: a child toctree that
/// points back at its own document and at `index`. The pass must terminate and
/// leave no reachable toctree, with every span in bounds.
#[test]
fn regression_cyclic_child_toctrees_terminate() {
    let mut doc = Document::new(Some("index".to_owned()));
    let root = doc.root;
    let toc = doc.add(NodeKind::Toctree, Some(root)).unwrap();
    let node = doc.get_mut(toc).unwrap();
    node.attrs.set_list("entry_refs", vec!["a".to_owned(), "index".to_owned()]);
    node.attrs.set_list("entry_kinds", vec!["doc".to_owned(), "self".to_owned()]);
    node.attrs.set_list("entry_titles", vec![String::new(), String::new()]);
    node.attrs.set_int("maxdepth", -1);

    let env = build_env();
    let collection = ToctreeCollection::default();
    let result = expand_toctrees(&mut doc, "index", &env, &collection).unwrap();
    assert_eq!(result.replaced + result.removed, 1);
    assert_eq!(
        doc.preorder_ids()
            .into_iter()
            .filter(|&id| doc.get(id).is_ok_and(|n| n.kind == NodeKind::Toctree))
            .count(),
        0
    );
    for id in doc.preorder_ids() {
        let node = doc.get(id).unwrap();
        if let Some(span) = node.span {
            assert!(span.start <= span.end && span.end <= 5);
        }
    }
}
