//! Phase 69 fuzz target for the frozen document AST.
//!
//! Generated doctrees must survive a JSON and pseudo-XML round-trip without
//! panicking, preserve their arena shape and emit deterministic bytes. This
//! exercises the serialization contract that every writer and the incremental
//! cache rely on.
//!
//! Defaults to 1024 cases per property; `PROPTEST_CASES` raises it for the
//! Phase 69 multi-crate campaign.

use proptest::prelude::*;
use scribe_ast::{Document, NodeId, NodeKind};

fn fuzz_cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(1024)
}

const KINDS: &[NodeKind] = &[
    NodeKind::Section,
    NodeKind::Title,
    NodeKind::Paragraph,
    NodeKind::LiteralBlock,
    NodeKind::BulletList,
    NodeKind::ListItem,
    NodeKind::DefinitionList,
    NodeKind::Note,
    NodeKind::Warning,
    NodeKind::Admonition,
    NodeKind::BlockQuote,
    NodeKind::Container,
    NodeKind::Reference,
    NodeKind::Math,
    NodeKind::Raw,
];

/// Build a small, finite doctree from generated choices. Each new node is
/// attached either to the root or to a previously generated node, which keeps
/// the tree shallow and acyclic while still exercising nested serialization.
fn build_document(choices: &[(u32, u32)], texts: &[String]) -> Document {
    let mut doc = Document::new(Some("index".to_owned()));
    doc.metadata.docname = Some("index".to_owned());
    let root = doc.root;
    let mut ids: Vec<NodeId> = vec![root];
    for (index, &(kind_index, parent_index)) in choices.iter().enumerate() {
        let kind = KINDS[kind_index as usize % KINDS.len()];
        let parent = ids[parent_index as usize % ids.len()];
        let id = doc.add(kind, Some(parent)).unwrap();
        ids.push(id);
        if let Some(text) = texts.get(index % texts.len()) {
            let _ = doc.new_text(text.clone(), Some(id));
        }
    }
    doc
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(fuzz_cases()))]

    #[test]
    fn json_roundtrip_preserves_arena_and_is_deterministic(
        choices in prop::collection::vec((any::<u32>(), any::<u32>()), 0..32),
        texts in prop::collection::vec(prop::string::string_regex("[\\p{L}\\p{N}\\p{P} ]{0,24}").unwrap(), 1..8),
    ) {
        let doc = build_document(&choices, &texts);
        let json = doc.to_json().unwrap();
        let restored = Document::from_json(&json).unwrap();

        prop_assert_eq!(restored.arena_len(), doc.arena_len(), "arena length changed across JSON round-trip");
        prop_assert_eq!(restored.to_json().unwrap(), json, "JSON serialization is not deterministic");

        // Every id in the original arena must remain traversable in the copy.
        for original_id in doc.preorder_ids() {
            prop_assert!(restored.get(original_id).is_ok(), "node {original_id:?} lost in round-trip");
        }
    }

    #[test]
    fn pseudoxml_emission_is_deterministic_and_finite(
        choices in prop::collection::vec((any::<u32>(), any::<u32>()), 0..32),
        texts in prop::collection::vec(prop::string::string_regex("[\\p{L}\\p{N}\\p{P} ]{0,24}").unwrap(), 1..8),
    ) {
        let doc = build_document(&choices, &texts);
        let first = doc.to_pseudoxml().unwrap();
        prop_assert_eq!(&first, &doc.to_pseudoxml().unwrap(), "pseudo-XML emission is not deterministic");
        prop_assert!(first.len() <= (doc.arena_len() as usize + 1) * 256, "pseudo-XML output is not bounded");

        // The pre-order walk must visit each node exactly once (no cycles).
        let ids = doc.preorder_ids();
        prop_assert_eq!(ids.len(), ids.iter().copied().collect::<std::collections::HashSet<_>>().len());
    }
}
