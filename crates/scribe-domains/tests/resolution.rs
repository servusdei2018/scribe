//! Phase 14 resolution-engine tests: local, `:any:`, intersphinx, fuzzy
//! suggestions, `:external:` roles, disabled reftypes and parallel documents.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use scribe_ast::{Document, NodeKind};
use scribe_config::ConfigValue;
use scribe_domains::intersphinx::load::{FetchOptions, MappingEntry};
use scribe_domains::intersphinx::resolve::inventory_not_found_diagnostic;
use scribe_domains::{
    fetch_one, DumpConfig, DiagnosticSink, DomainContext, DomainRegistry, ExternalRole, Inventory,
    InventoryCache, IntersphinxResolver, ObjectEntry,
};
use scribe_domains::xref::XrefResolver;

use common::{add_label, add_xref, config_with, context, TestEnv};

const BASE_URI: &str = "https://docs.example.com/";

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("scribe-phase14-{tag}-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn external_inventory_bytes() -> Vec<u8> {
    let mut objects = BTreeMap::new();
    objects.insert("py".to_owned(), vec![
        ObjectEntry::new("external_func", "external_func", "function", "api", "external_func", 1),
        ObjectEntry::new("ExternalClass", "External Class Guide", "class", "api", "ExternalClass", 1),
    ]);
    objects.insert("std".to_owned(), vec![
        ObjectEntry::new("external-label", "external-label", "label", "guide", "external-label", -1),
        ObjectEntry::new("External Term", "External Term", "term", "glossary", "term-external-term", -1),
    ]);
    objects.insert("c".to_owned(), vec![
        ObjectEntry::new("c_external", "c_external", "function", "c-api", "c_external", 1),
    ]);
    Inventory::dump(
        &objects,
        &DumpConfig { project: "Ext Project", version: "1.0" },
        |docname| format!("{docname}.html"),
    )
}

fn write_inventory(dir: &Path) -> PathBuf {
    let path = dir.join("objects.inv");
    std::fs::write(&path, external_inventory_bytes()).expect("write inventory");
    path
}

fn mapping(location: String) -> MappingEntry {
    MappingEntry {
        name: "ext".to_owned(),
        target_uri: BASE_URI.to_owned(),
        locations: vec![Some(location)],
    }
}

fn fetch(dir: &Path, options: FetchOptions) -> scribe_domains::FetchOutcome {
    let path = write_inventory(dir);
    fetch_one(&mapping(path.display().to_string()), &options, None)
}

#[test]
fn resolves_local_xref_first() {
    let env = TestEnv::default();
    let config = config_with(&[]);
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");
    let registry = DomainRegistry::with_standard();

    let mut doc = Document::new(Some("index.rst".to_owned()));
    add_label(&mut doc, "local-label", "local-label-id");
    let (xref, _, _) = add_xref(&mut doc, "std", "ref", "local-label", true, true, "Local Label");
    registry.process_document(&ctx, "index", &doc).expect("process");

    let engine = XrefResolver::new(&registry, None);
    let report = engine.resolve_document(&ctx, &mut doc).expect("resolve");
    assert_eq!(report.resolved_local, 1);
    assert_eq!(report.resolved, 1);
    assert_eq!(report.unresolved, 0);
    assert_eq!(doc.get(xref).expect("node").kind, NodeKind::Reference);
    assert_eq!(doc.get(xref).expect("node").attrs.get_str("refid").ok().flatten(), Some("local-label-id"));
}

#[test]
fn any_xref_falls_back_across_domains() {
    let env = TestEnv::default();
    let config = config_with(&[]);
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");
    let registry = DomainRegistry::with_standard();

    let mut doc = Document::new(Some("index.rst".to_owned()));
    add_label(&mut doc, "any-label", "any-label-id");
    let (xref, _, _) = add_xref(&mut doc, "", "any", "any-label", true, true, "Any Label");
    registry.process_document(&ctx, "index", &doc).expect("process");

    let report = XrefResolver::new(&registry, None)
        .resolve_document(&ctx, &mut doc)
        .expect("resolve");
    assert_eq!(report.resolved_any, 1);
    assert_eq!(doc.get(xref).expect("node").kind, NodeKind::Reference);
}

#[test]
fn resolves_external_xref_from_local_directory() {
    let dir = temp_dir("dir");
    let outcome = fetch(&dir, FetchOptions::default());
    let inventory = outcome.inventory.as_ref().expect("inventory loaded");
    assert_eq!(inventory.get("py:function", "external_func").unwrap().uri,
        "https://docs.example.com/api.html#external_func");

    let resolver = IntersphinxResolver::new(vec![outcome], &[]);
    let env = TestEnv::default();
    let config = config_with(&[]);
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");
    let registry = DomainRegistry::builtin();

    let mut doc = Document::new(Some("index.rst".to_owned()));
    let (xref, _, _) = add_xref(&mut doc, "py", "func", "external_func", false, true, "external_func()");
    registry.process_document(&ctx, "index", &doc).expect("process");

    let report = XrefResolver::new(&registry, Some(&resolver))
        .resolve_document(&ctx, &mut doc)
        .expect("resolve");
    assert_eq!(report.resolved_intersphinx, 1, "expected intersphinx resolution");
    let node = doc.get(xref).expect("node");
    assert_eq!(node.kind, NodeKind::Reference);
    assert_eq!(node.attrs.get_str("refuri").ok().flatten(), Some("https://docs.example.com/api.html#external_func"));
    assert_eq!(node.attrs.get_str("reftitle").ok().flatten(), Some("(in Ext Project v1.0)"));
    assert_eq!(node.attrs.get_bool("internal").ok().flatten(), Some(false));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn resolves_external_xref_from_file_uri() {
    let dir = temp_dir("file-uri");
    let path = write_inventory(&dir);
    let location = format!("file://{}", path.display());
    let outcome = fetch_one(&mapping(location), &FetchOptions::default(), None);
    assert!(outcome.warnings.is_empty(), "warnings: {:?}", outcome.warnings);
    assert!(outcome.inventory.is_some());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn fuzzy_suggestion_only_touches_help() {
    let dir = temp_dir("fuzzy");
    let outcome = fetch(&dir, FetchOptions::default());
    let resolver = IntersphinxResolver::new(vec![outcome], &[]);
    let env = TestEnv::default();
    let config = config_with(&[("nitpicky", ConfigValue::Bool(true))]);
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");
    let registry = DomainRegistry::builtin();

    let mut doc = Document::new(Some("index.rst".to_owned()));
    let (_, _, _) = add_xref(&mut doc, "py", "func", "external_functon", false, true, "external_functon()");
    registry.process_document(&ctx, "index", &doc).expect("process");

    let report = XrefResolver::new(&registry, Some(&resolver))
        .resolve_document(&ctx, &mut doc)
        .expect("resolve");
    assert_eq!(report.unresolved, 1);
    assert!(!report.suggestions.is_empty());
    let diagnostic = report.diagnostics.iter().find(|diagnostic| diagnostic.is_missing_reference()).expect("diagnostic");
    assert_eq!(diagnostic.message, "py:func reference target not found: external_functon");
    assert!(diagnostic.help.as_deref().is_some_and(|help| help.contains("Did you mean: 'external_func'")));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn nitpicky_message_format_is_unchanged() {
    let env = TestEnv::default();
    let config = config_with(&[("nitpicky", ConfigValue::Bool(true))]);
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");
    let registry = DomainRegistry::with_standard();
    let mut doc = Document::new(Some("index.rst".to_owned()));
    let (_, _, _) = add_xref(&mut doc, "std", "doc", "does-not-exist", false, false, "nope");
    registry.process_document(&ctx, "index", &doc).expect("process");

    let report = XrefResolver::new(&registry, None)
        .resolve_document(&ctx, &mut doc)
        .expect("resolve");
    let diagnostic = report.diagnostics.iter().find(|diagnostic| diagnostic.is_missing_reference()).expect("diagnostic");
    assert_eq!(diagnostic.message, "unknown document: 'does-not-exist'");
    assert_eq!(diagnostic.code, scribe_domains::codes::MISSING_REFERENCE);
}

#[test]
fn external_role_selects_named_inventory() {
    let dir = temp_dir("external-role");
    let outcome = fetch(&dir, FetchOptions::default());
    let resolver = IntersphinxResolver::new(vec![outcome], &[]);
    let env = TestEnv::default();
    let config = config_with(&[]);
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");
    let registry = DomainRegistry::builtin();

    let role = ExternalRole::parse("external+ext:py:func").expect("parse role");
    let mut doc = Document::new(Some("index.rst".to_owned()));
    let (xref, _, _) = add_xref(&mut doc, "py", "func", "external_func", false, true, "external_func()");
    role.annotate(&mut doc, xref, &registry).expect("annotate");
    registry.process_document(&ctx, "index", &doc).expect("process");

    let report = XrefResolver::new(&registry, Some(&resolver))
        .resolve_document(&ctx, &mut doc)
        .expect("resolve");
    assert_eq!(report.resolved_intersphinx, 1);
    assert_eq!(doc.get(xref).expect("node").attrs.get_str("refuri").ok().flatten(),
        Some("https://docs.example.com/api.html#external_func"));

    // A missing external target uses upstream's distinct message.
    let mut missing = Document::new(Some("index.rst".to_owned()));
    let (xref, _, _) = add_xref(&mut missing, "py", "func", "not_there", false, true, "not_there()");
    role.annotate(&mut missing, xref, &registry).expect("annotate");
    let report = XrefResolver::new(&registry, Some(&resolver))
        .resolve_document(&ctx, &mut missing)
        .expect("resolve");
    let diagnostic = report.diagnostics.iter().find(|diagnostic| diagnostic.is_missing_reference()).expect("diagnostic");
    assert_eq!(diagnostic.message, "external py:func reference target not found: not_there");

    // An unknown inventory produces a dedicated intersphinx warning.
    let unknown = inventory_not_found_diagnostic(&ctx, "nope", xref, &missing);
    assert!(unknown.message.contains("not found: 'nope'"));
    assert_eq!(unknown.code, scribe_domains::codes::INTERSPHINX);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn disabled_reftypes_skip_intersphinx() {
    let dir = temp_dir("disabled");
    let outcome = fetch(&dir, FetchOptions::default());
    let resolver = IntersphinxResolver::new(vec![outcome], &["py:function".to_owned()]);
    let env = TestEnv::default();
    let config = config_with(&[("nitpicky", ConfigValue::Bool(true))]);
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");
    let registry = DomainRegistry::builtin();
    let mut doc = Document::new(Some("index.rst".to_owned()));
    let (_, _, _) = add_xref(&mut doc, "py", "func", "external_func", false, true, "external_func()");
    registry.process_document(&ctx, "index", &doc).expect("process");

    let report = XrefResolver::new(&registry, Some(&resolver))
        .resolve_document(&ctx, &mut doc)
        .expect("resolve");
    assert_eq!(report.resolved_intersphinx, 0);
    assert_eq!(report.unresolved, 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn resolves_multiple_documents_in_parallel() {
    let env = TestEnv::default();
    let config = config_with(&[]);
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");
    let registry = DomainRegistry::with_standard();
    let mut documents = BTreeMap::new();
    for index in 0..8 {
        let mut doc = Document::new(Some(format!("doc{index}.rst")));
        add_label(&mut doc, &format!("label-{index}"), &format!("id-{index}"));
        add_xref(&mut doc, "std", "ref", &format!("label-{index}"), true, true, &format!("Label {index}"));
        registry.process_document(&ctx, &format!("doc{index}"), &doc).expect("process");
        documents.insert(format!("doc{index}"), doc);
    }
    let report = XrefResolver::new(&registry, None)
        .resolve_documents(&ctx, &mut documents)
        .expect("resolve");
    assert_eq!(report.resolved_local, 8);
    assert_eq!(report.per_document.len(), 8);
    assert!(documents.values().all(|doc| doc.preorder_ids().iter().all(|&id| doc.get(id).is_ok_and(|node| node.kind != NodeKind::PendingXref))));
}

#[test]
fn cache_reuses_inventory_after_source_disappears() {
    let dir = temp_dir("cache");
    let cache_dir = temp_dir("cache-store");
    let path = write_inventory(&dir);
    let options = FetchOptions {
        timeout: Some(Duration::from_secs(1)),
        cache_limit_days: 5,
        cache_dir: Some(cache_dir.clone()),
        now: 1_700_000_000,
        ..FetchOptions::default()
    };
    let location = format!("file://{}", path.display());
    let first = fetch_one(&mapping(location.clone()), &options, Some(&InventoryCache::new(cache_dir.clone(), 5)));
    assert!(first.inventory.is_some());

    std::fs::remove_file(&path).expect("remove source inventory");
    let second = fetch_one(&mapping(location), &options, Some(&InventoryCache::new(cache_dir.clone(), 5)));
    assert!(second.from_cache, "expected a cache hit: {:?}", second.warnings);
    assert!(second.inventory.is_some());

    // An expired cache is ignored when offline fallback is disabled (Phase 33
    // added opt-in stale-cache reuse on network failure); a panic is never
    // possible either way.
    let expired_options = FetchOptions {
        now: 1_700_000_000 + 6 * 86_400,
        offline_fallback: false,
        ..options
    };
    let expired = fetch_one(&mapping("https://docs.example.com/objects.inv".to_owned()), &expired_options, Some(&InventoryCache::new(cache_dir.clone(), 5)));
    assert!(expired.inventory.is_none());
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(cache_dir);
}

#[test]
fn malformed_inventory_never_panics() {
    let dir = temp_dir("malformed");
    let path = dir.join("objects.inv");
    std::fs::write(&path, b"not an inventory").expect("write garbage");
    let outcome = fetch_one(&mapping(path.display().to_string()), &FetchOptions::default(), None);
    assert!(outcome.inventory.is_none());
    assert!(!outcome.warnings.is_empty());
    let missing = fetch_one(&mapping(dir.join("missing.inv").display().to_string()), &FetchOptions::default(), None);
    assert!(missing.inventory.is_none());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn cyclic_or_ambiguous_mapping_degrades_gracefully() {
    // Duplicate target URIs and empty names are rejected by from_config, but
    // manually constructed entries never panic during fetch.
    let entries = vec![
        ("a".to_owned(), BASE_URI.to_owned(), vec![Some("missing-a".to_owned())]),
        ("a".to_owned(), BASE_URI.to_owned(), vec![Some("missing-b".to_owned())]),
    ];
    let mapping = scribe_domains::IntersphinxMapping::from_entries(entries);
    let options = FetchOptions { timeout: Some(Duration::from_millis(50)), ..FetchOptions::default() };
    let outcomes = scribe_domains::fetch_all(&mapping, &options);
    assert_eq!(outcomes.len(), 1);
    assert!(outcomes[0].inventory.is_none());
}

#[test]
fn http_fetch_errors_are_non_fatal() {
    // A closed local port exercises the retry path without external network.
    let entry = mapping("http://127.0.0.1:1/objects.inv".to_owned());
    let options = FetchOptions {
        timeout: Some(Duration::from_millis(50)),
        retries: 1,
        ..FetchOptions::default()
    };
    let outcome = fetch_one(&entry, &options, None);
    assert!(outcome.inventory.is_none());
    assert!(outcome.warnings.iter().any(|warning| warning.contains("not fetchable") || warning.contains("attempt")));
}

#[test]
fn any_xref_searches_all_inventories() {
    let dir = temp_dir("any-inventory");
    let outcome = fetch(&dir, FetchOptions::default());
    let resolver = IntersphinxResolver::new(vec![outcome], &[]);
    let env = TestEnv::default();
    let config = config_with(&[]);
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");
    let registry = DomainRegistry::builtin();
    let mut doc = Document::new(Some("index.rst".to_owned()));
    let (xref, _, _) = add_xref(&mut doc, "", "any", "c_external", false, true, "c_external");
    registry.process_document(&ctx, "index", &doc).expect("process");

    let report = XrefResolver::new(&registry, Some(&resolver))
        .resolve_document(&ctx, &mut doc)
        .expect("resolve");
    assert_eq!(report.resolved_intersphinx, 1);
    assert_eq!(doc.get(xref).expect("node").attrs.get_str("refuri").ok().flatten(),
        Some("https://docs.example.com/c-api.html#c_external"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn attached_registry_resolver_is_consulted_by_standard_entry_point() {
    let dir = temp_dir("registry-seam");
    let outcome = fetch(&dir, FetchOptions::default());
    let resolver = IntersphinxResolver::new(vec![outcome], &[]);
    let env = TestEnv::default();
    let config = config_with(&[]);
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");
    let registry = DomainRegistry::builtin();
    registry.set_intersphinx(resolver);

    let mut doc = Document::new(Some("index.rst".to_owned()));
    let (xref, _, _) = add_xref(&mut doc, "py", "func", "external_func", false, true, "external_func()");
    registry.process_document(&ctx, "index", &doc).expect("process");
    // The frozen Phase 11 entry point (used by the Phase 10 transforms) must
    // now consult the attached intersphinx service without any signature
    // change.
    let report = scribe_domains::resolve_pending_xrefs(&registry, &ctx, &mut doc).expect("resolve");
    assert_eq!(report.resolved_intersphinx, 1);
    assert_eq!(doc.get(xref).expect("node").kind, NodeKind::Reference);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn async_fetch_wrapper_runs_in_parallel() {
    let dir = temp_dir("async");
    let path = write_inventory(&dir);
    let mapping = scribe_domains::IntersphinxMapping::from_entries(vec![
        ("ext".to_owned(), BASE_URI.to_owned(), vec![Some(path.display().to_string())]),
    ]);
    let outcomes = scribe_domains::fetch_all_async(&mapping, &FetchOptions::default())
        .await
        .expect("async fetch");
    assert_eq!(outcomes.len(), 1);
    assert!(outcomes[0].inventory.is_some());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn domain_context_is_available_for_external_resolution() {
    // The helper used by resolve.rs reads the docname from the context; make
    // sure the public constructor keeps it accessible.
    let env = TestEnv::default();
    let config = config_with(&[]);
    let sink = DiagnosticSink::default();
    let ctx: DomainContext<'_> = context(&env, &config, &sink, "sub/page");
    assert_eq!(ctx.docname, "sub/page");
}
