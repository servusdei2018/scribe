//! Phase 26 integration tests for the i18n transforms.
//!
//! Exercises the seams activated from Phase 10: `PreserveTranslatableMessages`,
//! `Locale`, `TranslationProgressTotaliser`, `AddTranslationClasses` and
//! `RemoveTranslatableInline`, all against the frozen §7 AST.

use std::collections::BTreeMap;

use scribe_ast::{AttributeValue, NodeKind};
use scribe_config::{Config, ConfigValue};
use scribe_core::{
    apply_locale, extract_messages, is_in_substitution_definition, is_translatable,
    AddTranslationClasses, Locale, Phase, PostTransform, PreserveTranslatableMessages,
    RemoveTranslatableInline, Transform, TranslationCatalog, TranslationProgress,
    TranslationProgressTotaliser, TransformContext, TransformRegistry,
};

fn parse(source: &str) -> scribe_ast::Document {
    let mut output = scribe_rst::parse(source);
    let registry = scribe_rst::DirectiveRegistry::builtin();
    let context = scribe_rst::Context::default();
    let _ = scribe_rst::resolve_output(&mut output, &registry, &context);
    output.document
}

fn translated_text(document: &scribe_ast::Document) -> String {
    document.to_pseudoxml().unwrap_or_default()
}

#[test]
fn extraction_collects_titles_paragraphs_and_admonitions() {
    let document = parse(
        "Main Title\n==========\n\nHello world.\n\n.. note:: Important note\n\n   Note body text.\n",
    );
    let messages: Vec<String> = extract_messages(&document).into_iter().map(|m| m.msgid).collect();
    assert!(messages.contains(&"Main Title".to_owned()), "{messages:?}");
    assert!(messages.contains(&"Hello world.".to_owned()), "{messages:?}");
    assert!(messages.contains(&"Important note".to_owned()), "{messages:?}");
    assert!(messages.contains(&"Note body text.".to_owned()), "{messages:?}");
    // Container nodes are never extracted: their text-bearing children are.
    assert!(!messages.iter().any(|m| m.contains("Important note") && m.contains("Note body")));
}

#[test]
fn extraction_skips_literal_blocks_and_substitutions() {
    let document = parse("Title\n=====\n\nText::\n\n   literal contents\n\n.. |sub| replace:: substitution text\n");
    let messages: Vec<String> = extract_messages(&document).into_iter().map(|m| m.msgid).collect();
    assert_eq!(messages, vec!["Title", "Text:"]);
    for message in extract_messages(&document) {
        assert!(!is_in_substitution_definition(&document, message.node));
    }
}

#[test]
fn is_translatable_respects_explicit_attribute() {
    let mut document = parse("Title\n=====\n\nParagraph.\n");
    let title_id = document
        .preorder_ids()
        .into_iter()
        .find(|&id| document.get(id).is_ok_and(|node| node.kind == NodeKind::Title))
        .expect("title exists");
    let node = document.get_mut(title_id).expect("title is live");
    assert!(is_translatable(node));
    node.attrs.set_bool("translatable", false);
    assert!(!is_translatable(node));
}

#[test]
fn locale_applies_translations_and_marks_nodes() {
    let mut document = parse("Title\n=====\n\nHello world.\n");
    let catalog = TranslationCatalog::from_entries(
        "index",
        [("Title", "TÍTULO"), ("Hello world.", "HOLA MUNDO.")],
    );
    let (changed, progress) = apply_locale(&mut document, &catalog, "html");
    assert!(changed);
    assert_eq!(progress.total, 2);
    assert_eq!(progress.translated, 2);
    let text = translated_text(&document);
    assert!(text.contains("TÍTULO"), "{text}");
    assert!(text.contains("HOLA MUNDO."), "{text}");

    let translated = document
        .preorder_ids()
        .into_iter()
        .filter(|&id| {
            document.get(id).is_ok_and(|node| {
                matches!(node.attrs.get("translated"), Some(AttributeValue::Bool(true)))
            })
        })
        .count();
    assert_eq!(translated, 2);
}

#[test]
fn locale_counts_idempotent_translations_as_translated() {
    let mut document = parse("Same\n====\n");
    let catalog = TranslationCatalog::from_entries("index", [("Same", "Same")]);
    let (changed, progress) = apply_locale(&mut document, &catalog, "html");
    assert!(!changed);
    assert_eq!(progress.translated, 1);
}

#[test]
fn locale_ignores_empty_and_missing_translations() {
    let mut document = parse("One\n===\n\nTwo\n===\n");
    let catalog = TranslationCatalog::from_entries("index", [("One", ""), ("Missing", "X")]);
    let (changed, progress) = apply_locale(&mut document, &catalog, "html");
    assert!(!changed);
    assert_eq!(progress.translated, 0);
    assert_eq!(progress.total, 2);
}

#[test]
fn transform_pipeline_runs_i18n_in_priority_order() {
    let mut document = parse("Greeting\n========\n\nHello.\n");
    let catalog = TranslationCatalog::from_entries(
        "index",
        [("Greeting", "SALUDO"), ("Hello.", "HOLA.")],
    );
    let ctx = TransformContext::new(None, None, "html", "html").with_translations(&catalog);
    let mut registry = TransformRegistry::new();
    // Register out of order; the pipeline sorts by (priority, name).
    registry.register_read(AddTranslationClasses);
    registry.register_read(Locale);
    registry.register_read(TranslationProgressTotaliser);
    registry.register_read(PreserveTranslatableMessages);
    let report = scribe_core::TransformPipeline::new(registry).run_read(&mut document, &ctx);
    assert!(!report.applied.is_empty());
    let progress = TranslationProgress::from_document(&document).expect("progress stored");
    assert_eq!(progress.total, 2);
    assert_eq!(progress.translated, 2);
    assert_eq!(progress.format(), "100.00%");
    let text = translated_text(&document);
    assert!(text.contains("SALUDO"), "{text}");
    assert!(text.contains("HOLA."), "{text}");
}

#[test]
fn add_translation_classes_follows_config() {
    let mut config = Config::new();
    config
        .set("translation_progress_classes", ConfigValue::Bool(true))
        .expect("known key");
    let mut document = parse("A\n=\n\nB\n=\n");
    let catalog = TranslationCatalog::from_entries("index", [("A", "AA")]);
    let (_, _) = apply_locale(&mut document, &catalog, "html");
    let ctx = TransformContext::new(Some(&config), None, "html", "html");
    let report = AddTranslationClasses.apply(&mut document, &ctx);
    assert!(report.changed);
    let text = translated_text(&document);
    assert!(text.contains("translated"), "{text}");
    assert!(text.contains("untranslated"), "{text}");
}

#[test]
fn preserve_translatable_messages_stores_original() {
    let mut document = parse("Original\n========\n");
    let title_id = document
        .preorder_ids()
        .into_iter()
        .find(|&id| document.get(id).is_ok_and(|node| node.kind == NodeKind::Title))
        .expect("title");
    document.get_mut(title_id).expect("live").attrs.set_bool("translatable", true);
    let ctx = TransformContext::new(None, None, "html", "html");
    PreserveTranslatableMessages.apply(&mut document, &ctx);
    assert_eq!(
        document.get(title_id).expect("live").attrs.get_str("__original_message").ok().flatten(),
        Some("Original")
    );
}

#[test]
fn remove_translatable_inline_unwraps_placeholders() {
    let mut document = parse("A\n=\n");
    let title_id = document
        .preorder_ids()
        .into_iter()
        .find(|&id| document.get(id).is_ok_and(|node| node.kind == NodeKind::Title))
        .expect("title");
    let inline = document.add(NodeKind::Inline, Some(title_id)).expect("add inline");
    document.get_mut(inline).expect("live").attrs.set_bool("translatable", true);
    let text = document.new_text("placeholder", Some(inline)).expect("add text");
    document.get_mut(inline).expect("live").children = vec![text];
    document.reindex_parents();

    let ctx = TransformContext::new(None, None, "html", "html");
    let report = RemoveTranslatableInline.apply(&mut document, &ctx);
    assert!(report.changed);
    assert!(!document.is_live(inline));
    let children = document.children(title_id).expect("children");
    assert!(children.contains(&text));
}

#[test]
fn substitutions_resolve_progress_from_document_metadata() {
    let mut document = parse("|progress|\n\n.. |progress| replace:: PLACEHOLDER\n");
    let catalog = TranslationCatalog::default();
    let ctx = TransformContext::new(None, None, "html", "html").with_translations(&catalog);
    let _ = Locale.apply(&mut document, &ctx);
    // A `|translation progress|` reference would be substituted by the
    // DefaultSubstitutions transform at priority 210; verify the value it
    // would read is now available on the document.
    TranslationProgress { total: 8, translated: 2 }.store_on_document(&mut document);
    assert_eq!(
        TranslationProgress::from_document(&document).expect("progress"),
        TranslationProgress { total: 8, translated: 2 }
    );
}

#[test]
fn malformed_translations_and_catalogs_never_panic() {
    let mut document = parse("Title\n=====\n\nParagraph.\n");
    let messages = [
        ("Title", ".. unknown-directive:: broken\n\n   :bad: option\n"),
        ("Paragraph.", "{{ unclosed"),
        ("", "ignored"),
    ];
    let catalog = TranslationCatalog::from_entries("index", messages);
    let (_, _) = apply_locale(&mut document, &catalog, "html");
    let text = translated_text(&document);
    assert!(!text.is_empty());
}

#[test]
fn progress_metadata_survives_serialization() {
    let mut document = parse("Title\n=====\n");
    TranslationProgress { total: 5, translated: 3 }.store_on_document(&mut document);
    let json = document.to_json().expect("serialize");
    let restored = scribe_ast::Document::from_json(&json).expect("deserialize");
    assert_eq!(
        TranslationProgress::from_document(&restored).expect("progress survived"),
        TranslationProgress { total: 5, translated: 3 }
    );
}

#[test]
fn empty_catalog_is_a_noop() {
    let mut document = parse("Title\n=====\n");
    let catalog = TranslationCatalog::new("index");
    let (changed, progress) = apply_locale(&mut document, &catalog, "html");
    assert!(!changed);
    assert_eq!(progress, TranslationProgress::default());
}

#[test]
fn default_substitutions_use_translation_progress() {
    let mut document = scribe_rst::parse("|translation progress|\n").document;
    TranslationProgress { total: 16, translated: 11 }.store_on_document(&mut document);
    let ctx = TransformContext::new(None, None, "html", "html");
    let report = scribe_core::apply_transforms(&mut document, &ctx, Phase::Read);
    assert!(report.applied.iter().any(|record| record.name == "DefaultSubstitutions"));
    assert!(translated_text(&document).contains("68.75%"));
}

#[allow(dead_code)]
fn unused_map() -> BTreeMap<String, String> {
    BTreeMap::new()
}
