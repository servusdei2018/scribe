//! Phase 10 transform-pipeline integration tests.
//!
//! These tests exercise every public transform through the priority-ordered
//! engine, plus determinism, enable/disable, diagnostics and malformed-input
//! robustness. Differential comparison against the pinned oracle lives in
//! `transform_differential.rs`.

use scribe_ast::{Document, NodeId, NodeKind};
use scribe_rst::parse;
use scribe_config::{Config, ConfigValue, Tags};
use scribe_core::{
    apply_transforms, Phase, Transform, TransformContext, TransformPipeline, TransformRegistry,
    TransformResult,
};

fn config(entries: &[(&str, ConfigValue)]) -> Config {
    let mut config = Config::new();
    for (key, value) in entries {
        config.set(key, value.clone()).expect("known config key");
    }
    config
}

fn paragraph(doc: &mut Document, content: &str) -> NodeId {
    let para = doc.add(NodeKind::Paragraph, Some(doc.root)).expect("arena");
    doc.new_text(content, Some(para)).expect("arena");
    para
}

fn run_read(doc: &mut Document, cfg: &Config) -> scribe_core::TransformReport {
    let ctx = TransformContext::new(Some(cfg), None, "pseudoxml", "pseudoxml");
    apply_transforms(doc, &ctx, Phase::Read)
}

// ---------------------------------------------------------------------------
// Determinism / registry
// ---------------------------------------------------------------------------

struct Dummy(&'static str, u32);

impl Transform for Dummy {
    fn name(&self) -> &'static str {
        self.0
    }
    fn default_priority(&self) -> u32 {
        self.1
    }
    fn apply(&self, _doc: &mut Document, _ctx: &TransformContext<'_>) -> TransformResult {
        TransformResult::changed()
    }
}

#[test]
fn priority_ordering_is_stable_and_independent_of_registration_order() {
    let mut registry = TransformRegistry::new();
    // Deliberately register out of order, including a priority tie.
    registry.register_read_with_priority(Dummy("Zebra", 999), 999);
    registry.register_read_with_priority(Dummy("Alpha", 10), 10);
    registry.register_read_with_priority(Dummy("Beta", 210), 210);
    registry.register_read_with_priority(Dummy("Aardvark", 10), 10);
    let pipeline = TransformPipeline::new(registry.clone());
    let names = pipeline.registry().read_plan_names();
    assert_eq!(names, vec!["Aardvark", "Alpha", "Beta", "Zebra"]);

    let mut reversed = TransformRegistry::new();
    reversed.register_read_with_priority(Dummy("Aardvark", 10), 10);
    reversed.register_read_with_priority(Dummy("Beta", 210), 210);
    reversed.register_read_with_priority(Dummy("Alpha", 10), 10);
    reversed.register_read_with_priority(Dummy("Zebra", 999), 999);
    assert_eq!(TransformPipeline::new(reversed).registry().read_plan_names(), names);
}

#[test]
fn enable_disable_and_report_are_deterministic() {
    let mut registry = TransformRegistry::new();
    registry.register_read_with_priority(Dummy("Alpha", 10), 10);
    registry.register_read_with_priority(Dummy("Beta", 20), 20);
    registry.disable("Alpha");
    assert!(!registry.is_enabled("Alpha"));
    let mut doc = Document::new(None);
    let config = config(&[]);
    let ctx = TransformContext::new(Some(&config), None, "html", "html");
    let report = TransformPipeline::new(registry.clone()).run(&mut doc, &ctx, Phase::Read);
    assert_eq!(report.applied_names(), vec!["Beta"]);
    assert_eq!(report.skipped_names(), vec!["Alpha"]);
    registry.enable("Alpha");
    let report = TransformPipeline::new(registry).run(&mut doc, &ctx, Phase::Read);
    assert_eq!(report.applied_names(), vec!["Alpha", "Beta"]);
    assert!(report.skipped.is_empty());
}

#[test]
fn builtin_plan_uses_canonical_priorities() {
    let registry = TransformRegistry::builtin();
    let plan = registry.plan(Phase::Read);
    let priorities: Vec<u32> = plan.iter().map(|entry| entry.priority).collect();
    let mut sorted = priorities.clone();
    sorted.sort_unstable();
    assert_eq!(priorities, sorted, "read plan must be priority-sorted");
    assert_eq!(plan.first().map(|entry| entry.name), Some("ApplySourceWorkaround"));
    let post = registry.post_plan_names();
    assert_eq!(post.first().copied(), Some("ReferencesResolver"));
    assert_eq!(post.iter().find(|&&name| name == "OnlyNodeTransform"), Some(&"OnlyNodeTransform"));
}

// ---------------------------------------------------------------------------
// DefaultSubstitutions
// ---------------------------------------------------------------------------

#[test]
fn default_substitutions_resolve_from_config() {
    let cfg = config(&[
        ("version", ConfigValue::from("1.2")),
        ("release", ConfigValue::from("1.2.3")),
        ("today", ConfigValue::from("2026-01-02")),
    ]);
    let mut doc = Document::new(None);
    let para = doc.add(NodeKind::Paragraph, Some(doc.root)).expect("arena");
    for name in ["version", "release", "today"] {
        let reference = doc.add(NodeKind::SubstitutionReference, Some(para)).expect("arena");
        doc.get_mut(reference).expect("arena").attrs.set_str("refname", name);
    }
    let report = run_read(&mut doc, &cfg);
    assert!(report.changed_names().contains(&"DefaultSubstitutions"));
    let texts: Vec<String> = doc
        .children(para)
        .expect("arena")
        .iter()
        .filter_map(|&id| doc.get(id).ok())
        .filter_map(|node| node.text_content().map(str::to_owned))
        .collect();
    assert_eq!(texts, vec!["1.2", "1.2.3", "2026-01-02"]);
}

#[test]
fn default_substitutions_respect_explicit_definitions() {
    let cfg = config(&[("version", ConfigValue::from("1.2"))]);
    let mut doc = Document::new(None);
    let definition = doc.add(NodeKind::SubstitutionDefinition, Some(doc.root)).expect("arena");
    doc.get_mut(definition).expect("arena").attrs.set_list("names", vec!["version".to_owned()]);
    let reference = doc.add(NodeKind::SubstitutionReference, Some(doc.root)).expect("arena");
    doc.get_mut(reference).expect("arena").attrs.set_str("refname", "version");
    run_read(&mut doc, &cfg);
    assert!(doc.get(reference).expect("arena").kind == NodeKind::SubstitutionReference);
}

// ---------------------------------------------------------------------------
// Smart quotes
// ---------------------------------------------------------------------------

#[test]
fn smart_quotes_educates_text_and_skips_literal_nodes() {
    let cfg = config(&[]);
    let mut doc = Document::new(None);
    let para = paragraph(&mut doc, "\"Hello\" -- world... it's fine");
    let literal = doc.add(NodeKind::LiteralBlock, Some(doc.root)).expect("arena");
    doc.new_text("\"Raw\" -- text", Some(literal)).expect("arena");
    let report = run_read(&mut doc, &cfg);
    assert!(report.changed_names().contains(&"SphinxSmartQuotes"));
    let text = doc
        .children(para).expect("arena")[0]
        .pipe(|id| doc.get(id).expect("arena").text_content().expect("text").to_owned());
    assert_eq!(text, "\u{201c}Hello\u{201d} \u{2013} world\u{2026} it\u{2019}s fine");
    let literal_text = doc
        .children(literal).expect("arena")[0]
        .pipe(|id| doc.get(id).expect("arena").text_content().expect("text").to_owned());
    assert_eq!(literal_text, "\"Raw\" -- text");
}

#[test]
fn smart_quotes_language_variations_and_config_disable() {
    let mut doc = Document::new(None);
    let para = paragraph(&mut doc, "\"Hallo\"");
    let cfg = config(&[("language", ConfigValue::from("de"))]);
    let ctx = TransformContext::new(Some(&cfg), None, "html", "html");
    apply_transforms(&mut doc, &ctx, Phase::Read);
    let text = doc.get(doc.children(para).expect("arena")[0]).expect("arena").text_content().expect("text");
    assert_eq!(text, "\u{201e}Hallo\u{201c}");

    let mut doc = Document::new(None);
    let para = paragraph(&mut doc, "\"Hello\"");
    let cfg = config(&[("smartquotes", ConfigValue::Bool(false))]);
    run_read(&mut doc, &cfg);
    let text = doc.get(doc.children(para).expect("arena")[0]).expect("arena").text_content().expect("text");
    assert_eq!(text, "\"Hello\"");
}

trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}
impl<T> Pipe for T {}

// ---------------------------------------------------------------------------
// Footnotes / citations
// ---------------------------------------------------------------------------

#[test]
fn footnotes_autonumber_symbol_and_backlinks() {
    let cfg = config(&[]);
    let mut doc = Document::new(None);
    // [1]_ manual footnote.
    let ref1 = doc.add(NodeKind::FootnoteReference, Some(doc.root)).expect("arena");
    doc.get_mut(ref1).expect("arena").attrs.set_str("refname", "1");
    doc.fill_ref_text(ref1, "[1]");
    let note1 = doc.add(NodeKind::Footnote, Some(doc.root)).expect("arena");
    doc.get_mut(note1).expect("arena").attrs.set_int("number", 1);
    doc.get_mut(note1).expect("arena").attrs.set_list("ids", vec!["fn-1".to_owned()]);
    // [#]_ auto footnote.
    let ref_auto = doc.add(NodeKind::FootnoteReference, Some(doc.root)).expect("arena");
    doc.get_mut(ref_auto).expect("arena").attrs.set_str("auto", "1");
    doc.fill_ref_text(ref_auto, "[#]");
    let note_auto = doc.add(NodeKind::Footnote, Some(doc.root)).expect("arena");
    doc.get_mut(note_auto).expect("arena").attrs.set_str("auto", "0");
    doc.get_mut(note_auto).expect("arena").attrs.set_list("ids", vec!["fn-auto".to_owned()]);
    // [*]_ symbol footnote.
    let ref_sym = doc.add(NodeKind::FootnoteReference, Some(doc.root)).expect("arena");
    doc.get_mut(ref_sym).expect("arena").attrs.set_str("auto", "*");
    doc.fill_ref_text(ref_sym, "[*]");
    let note_sym = doc.add(NodeKind::Footnote, Some(doc.root)).expect("arena");
    doc.get_mut(note_sym).expect("arena").attrs.set_str("auto", "*");
    doc.get_mut(note_sym).expect("arena").attrs.set_list("ids", vec!["fn-sym".to_owned()]);

    let report = run_read(&mut doc, &cfg);
    assert!(report.changed_names().contains(&"Footnotes"));
    assert_eq!(ref_text(&doc, ref1), "1");
    assert_eq!(ref_text(&doc, ref_auto), "2");
    assert_eq!(ref_text(&doc, ref_sym), "*");
    assert_eq!(refid(&doc, ref1), "fn-1");
    assert_eq!(refid(&doc, ref_auto), "fn-auto");
    assert_eq!(refid(&doc, ref_sym), "fn-sym");
    for note in [note1, note_auto, note_sym] {
        let backrefs = doc.get(note).expect("arena").get_list("backrefs").expect("list").expect("backrefs");
        assert_eq!(backrefs.len(), 1, "every footnote must have one backlink");
    }
    assert_eq!(label_text(&doc, note1).as_deref(), Some("1"));
    assert_eq!(label_text(&doc, note_auto).as_deref(), Some("2"));
    assert_eq!(label_text(&doc, note_sym).as_deref(), Some("*"));
}

#[test]
fn citations_link_by_name() {
    let cfg = config(&[]);
    let mut doc = Document::new(None);
    let reference = doc.add(NodeKind::CitationReference, Some(doc.root)).expect("arena");
    doc.get_mut(reference).expect("arena").attrs.set_str("refname", "smith");
    doc.fill_ref_text(reference, "Smith");
    let citation = doc.add(NodeKind::Citation, Some(doc.root)).expect("arena");
    doc.get_mut(citation).expect("arena").attrs.set_list("names", vec!["smith".to_owned()]);
    doc.get_mut(citation).expect("arena").attrs.set_list("ids", vec!["cite-smith".to_owned()]);
    run_read(&mut doc, &cfg);
    assert_eq!(refid(&doc, reference), "cite-smith");
    let backrefs = doc.get(citation).expect("arena").get_list("backrefs").expect("list").expect("backrefs");
    assert_eq!(backrefs.len(), 1);
    assert_eq!(label_text(&doc, citation).as_deref(), Some("smith"));
}

trait DocTestExt {
    fn fill_ref_text(&mut self, reference: NodeId, text: &str);
}
impl DocTestExt for Document {
    fn fill_ref_text(&mut self, reference: NodeId, label: &str) {
        let child = self.new_text(label, None).expect("arena");
        self.append_child(reference, child).expect("arena");
    }
}

fn ref_text(doc: &Document, id: NodeId) -> String {
    doc.get(id).expect("arena").astext(doc)
}

fn refid(doc: &Document, id: NodeId) -> String {
    doc.get(id).expect("arena").get_str("refid").expect("str").expect("refid").to_owned()
}

fn label_text(doc: &Document, note: NodeId) -> Option<String> {
    let label = doc.children(note).expect("arena").iter().copied().find(|&child| {
        doc.get(child).is_ok_and(|node| node.kind == NodeKind::Label)
    })?;
    Some(doc.get(label).expect("arena").astext(doc))
}

// ---------------------------------------------------------------------------
// only filtering (post phase)
// ---------------------------------------------------------------------------

#[test]
fn only_filter_nested_tags_and_builders() {
    let tags = Tags::with_tags(["html"]);
    let mut doc = Document::new(None);
    // only:: html -> keep children.
    let keep = doc.add(NodeKind::Only, Some(doc.root)).expect("arena");
    doc.get_mut(keep).expect("arena").attrs.set_str("expr", "html");
    let keep_para = paragraph(&mut doc, "kept");
    doc.append_child(keep, keep_para).expect("arena");
    // nested only:: latex inside keep -> drop.
    let nested = doc.add(NodeKind::Only, Some(keep)).expect("arena");
    doc.get_mut(nested).expect("arena").attrs.set_str("expr", "latex");
    paragraph_in(&mut doc, nested, "dropped child");
    // only:: html and draft -> drop (draft not set).
    let drop = doc.add(NodeKind::Only, Some(doc.root)).expect("arena");
    doc.get_mut(drop).expect("arena").attrs.set_str("expr", "html and draft");
    paragraph_in(&mut doc, drop, "dropped");

    let ctx = TransformContext::new(None, Some(&tags), "html", "html");
    let report = TransformPipeline::with_builtins().run(&mut doc, &ctx, Phase::Resolved);
    assert!(report.changed_names().contains(&"OnlyNodeTransform"));
    let pseudoxml = doc.to_pseudoxml().expect("pseudoxml");
    assert!(pseudoxml.contains("kept"), "{pseudoxml}");
    assert!(!pseudoxml.contains("dropped"), "{pseudoxml}");
    assert!(!pseudoxml.contains("<only"), "{pseudoxml}");
}

fn paragraph_in(doc: &mut Document, parent: NodeId, content: &str) -> NodeId {
    let para = doc.add(NodeKind::Paragraph, Some(parent)).expect("arena");
    doc.new_text(content, Some(para)).expect("arena");
    para
}

// ---------------------------------------------------------------------------
// Node normalization transforms
// ---------------------------------------------------------------------------

#[test]
fn sort_ids_rotates_generated_first_id() {
    let cfg = config(&[]);
    let mut doc = Document::new(None);
    let section = doc.add(NodeKind::Section, Some(doc.root)).expect("arena");
    doc.get_mut(section).expect("arena").attrs.set_list("ids", vec!["id3".to_owned(), "real".to_owned(), "other".to_owned()]);
    run_read(&mut doc, &cfg);
    let ids = doc.get(section).expect("arena").get_list("ids").expect("list").expect("ids");
    assert_eq!(ids, vec!["real", "other", "id3"]);
}

#[test]
fn reorder_targets_and_indexes_stably() {
    let cfg = config(&[]);
    let mut doc = Document::new(None);
    let target1 = doc.add(NodeKind::Target, Some(doc.root)).expect("arena");
    let index1 = doc.add(NodeKind::Index, Some(doc.root)).expect("arena");
    let target2 = doc.add(NodeKind::Target, Some(doc.root)).expect("arena");
    let index2 = doc.add(NodeKind::Index, Some(doc.root)).expect("arena");
    run_read(&mut doc, &cfg);
    let order: Vec<NodeKind> = doc.children(doc.root).expect("arena").iter().map(|&id| doc.get(id).expect("arena").kind).collect();
    assert_eq!(order, vec![NodeKind::Index, NodeKind::Index, NodeKind::Target, NodeKind::Target]);
    assert!(doc.is_live(target1) && doc.is_live(target2) && doc.is_live(index1) && doc.is_live(index2));
}

#[test]
fn target_propagation_moves_ids_and_names() {
    let cfg = config(&[]);
    let mut doc = Document::new(None);
    let target = doc.add(NodeKind::Target, Some(doc.root)).expect("arena");
    doc.get_mut(target).expect("arena").attrs.set_list("ids", vec!["the-anchor".to_owned()]);
    doc.get_mut(target).expect("arena").attrs.set_list("names", vec!["the anchor".to_owned()]);
    let para = paragraph(&mut doc, "content");
    run_read(&mut doc, &cfg);
    let ids = doc.get(para).expect("arena").get_list("ids").expect("list").expect("ids");
    assert_eq!(ids, vec!["the-anchor"]);
    assert_eq!(doc.get(target).expect("arena").get_str("refid").expect("str"), Some("the-anchor"));
    assert!(doc.get(target).expect("arena").get_list("ids").expect("list").is_none());
}

#[test]
fn auto_numbering_assigns_ids_to_captioned_nodes() {
    let cfg = config(&[]);
    let mut doc = Document::new(None);
    let figure = doc.add(NodeKind::Figure, Some(doc.root)).expect("arena");
    let caption = doc.add(NodeKind::Caption, Some(figure)).expect("arena");
    doc.new_text("A caption", Some(caption)).expect("arena");
    let table = doc.add(NodeKind::Table, Some(doc.root)).expect("arena");
    let title = doc.add(NodeKind::Title, Some(table)).expect("arena");
    doc.new_text("A table", Some(title)).expect("arena");
    let container = doc.add(NodeKind::Container, Some(doc.root)).expect("arena");
    let code_caption = doc.add(NodeKind::Caption, Some(container)).expect("arena");
    doc.new_text("A code block", Some(code_caption)).expect("arena");
    let code = doc.add(NodeKind::LiteralBlock, Some(container)).expect("arena");
    doc.new_text("let x = 1;", Some(code)).expect("arena");
    run_read(&mut doc, &cfg);
    assert!(!doc.get(figure).expect("arena").get_list("ids").expect("list").expect("ids").is_empty());
    assert!(!doc.get(table).expect("arena").get_list("ids").expect("list").expect("ids").is_empty());
    assert!(!doc.get(code).expect("arena").get_list("ids").expect("list").expect("ids").is_empty());
}

#[test]
fn doctest_and_filter_system_messages() {
    let cfg = config(&[("keep_warnings", ConfigValue::Bool(false))]);
    let mut doc = Document::new(None);
    let doctest = doc.add(NodeKind::DoctestBlock, Some(doc.root)).expect("arena");
    doc.new_text(">>> 1 + 1", Some(doctest)).expect("arena");
    let low = doc.add(NodeKind::SystemMessage, Some(doc.root)).expect("arena");
    doc.get_mut(low).expect("arena").attrs.set_int("level", 1);
    let high = doc.add(NodeKind::SystemMessage, Some(doc.root)).expect("arena");
    doc.get_mut(high).expect("arena").attrs.set_int("level", 5);
    run_read(&mut doc, &cfg);
    let classes = doc.get(doctest).expect("arena").get_list("classes").expect("list").expect("classes");
    assert!(classes.iter().any(|class| class == "doctest"));
    assert!(!doc.is_live(low));
    assert!(doc.is_live(high));
}

#[test]
fn glossary_sorter_orders_terms_nfd_casefold() {
    let cfg = config(&[]);
    let mut doc = Document::new(None);
    let glossary = doc.add(NodeKind::Glossary, Some(doc.root)).expect("arena");
    doc.get_mut(glossary).expect("arena").attrs.set_bool("sorted", true);
    let dlist = doc.add(NodeKind::DefinitionList, Some(glossary)).expect("arena");
    for word in ["Zebra", "apple", "Äpfel"] {
        let item = doc.add(NodeKind::DefinitionListItem, Some(dlist)).expect("arena");
        let term = doc.add(NodeKind::Term, Some(item)).expect("arena");
        doc.new_text(word, Some(term)).expect("arena");
    }
    run_read(&mut doc, &cfg);
    let terms: Vec<String> = doc.children(dlist).expect("arena").iter().map(|&item| {
        let term = doc.children(item).expect("arena")[0];
        doc.get(term).expect("arena").astext(&doc)
    }).collect();
    // NFD puts the combining diaeresis (U+0308) after `p`, so upstream's
    // `normalize('NFD', term.lower())` ordering is apple < Äpfel < Zebra.
    assert_eq!(terms, vec!["apple", "Äpfel", "Zebra"]);
}

// ---------------------------------------------------------------------------
// Robustness
// ---------------------------------------------------------------------------

#[test]
fn malformed_documents_never_panic() {
    let cfg = config(&[]);
    let tags = Tags::new();
    let ctx = TransformContext::new(Some(&cfg), Some(&tags), "html", "html");
    let mut doc = Document::new(Some("index.rst".to_owned()));
    // Wrong attribute types, dangling children and unusual kinds.
    let node = doc.add(NodeKind::Section, Some(doc.root)).expect("arena");
    doc.get_mut(node).expect("arena").attrs.set_str("ids", "not-a-list");
    let ref_node = doc.add(NodeKind::SubstitutionReference, Some(doc.root)).expect("arena");
    doc.get_mut(ref_node).expect("arena").attrs.set_int("refname", 42);
    let only = doc.add(NodeKind::Only, Some(doc.root)).expect("arena");
    doc.get_mut(only).expect("arena").attrs.set_bool("expr", false);
    let _ = apply_transforms(&mut doc, &ctx, Phase::Read);
    let _ = apply_transforms(&mut doc, &ctx, Phase::Resolved);
    let _ = doc.to_pseudoxml();
}

// ---------------------------------------------------------------------------
// Phase 41: docinfo field stripping (Sphinx MetadataCollector)
// ---------------------------------------------------------------------------

#[test]
fn docinfo_preamble_is_collected_and_removed() {
    let cfg = config(&[]);
    let mut doc = parse(
        ":tocdepth: 2\n:orphan:\n:nocomments:\n:nosearch:\n:author: Jane Doe\n\nTitle\n=====\n\nBody.\n",
    )
    .document;

    // A leading field list is present before the read transforms.
    assert!(doc.children(doc.root).expect("arena").iter().any(|&id| {
        doc.get(id).is_ok_and(|node| node.kind == NodeKind::FieldList)
    }));

    let report = run_read(&mut doc, &cfg);
    assert!(report.applied_names().contains(&"MetadataCollector"));

    // The docinfo node is gone from the doctree, exactly as upstream.
    assert!(!doc.children(doc.root).expect("arena").iter().any(|&id| {
        doc.is_live(id) && doc.get(id).is_ok_and(|node| node.kind == NodeKind::FieldList)
    }));
    assert!(!doc.to_pseudoxml().expect("pseudoxml").contains("tocdepth"));

    // Sphinx-specific fields are available as document metadata.
    assert_eq!(doc.metadata.tocdepth(), Some(2));
    assert!(doc.metadata.is_orphan());
    assert!(doc.metadata.nocomments());
    assert!(doc.metadata.nosearch());
    assert_eq!(doc.metadata.docinfo.get("author").map(String::as_str), Some("Jane Doe"));
}

#[test]
fn non_preamble_field_list_is_preserved() {
    let cfg = config(&[]);
    let mut doc = parse("Title\n=====\n\n:tocdepth: 2\n\nBody.\n").document;
    run_read(&mut doc, &cfg);
    // Docutils only recognises bibliographic fields at the top of a document;
    // a field list inside a section is ordinary body content.
    assert!(doc.to_pseudoxml().expect("pseudoxml").contains("field_list"));
    assert!(!doc.metadata.is_orphan());
    assert_eq!(doc.metadata.tocdepth(), None);
}

#[test]
fn invalid_tocdepth_coerces_to_zero() {
    let cfg = config(&[]);
    let mut doc = parse(":tocdepth: bogus\n\nTitle\n=====\n").document;
    run_read(&mut doc, &cfg);
    assert_eq!(doc.metadata.tocdepth(), Some(0));
}
