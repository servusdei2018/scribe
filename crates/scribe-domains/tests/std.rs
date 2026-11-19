//! Standard-domain unit tests: object types, roles, inventories and fallback.

mod common;

use std::collections::BTreeMap;

use scribe_ast::{AttributeValue, Document, Node, NodeKind};
use scribe_config::ConfigValue;
use scribe_domains::{
    resolve_pending_xrefs, DiagnosticSink, Domain, DomainContext, DomainRegistry,
    ReferenceNodeKind, StandardDomain,
};

use common::{add_label, add_xref, config_with, context, TestEnv};

fn fig_tree(label_id: &str) -> Document {
    let mut doc = Document::new(Some("index.rst".to_owned()));
    let figure = doc.add(NodeKind::Figure, Some(doc.root)).expect("figure");
    let mut node = Node::new(NodeKind::Figure);
    node.attrs.set_list("ids", vec![label_id.to_owned()]);
    node.attrs.set_str("fignumber", "1.2");
    doc.replace_node(figure, node).expect("replace figure");
    let caption = doc.add(NodeKind::Caption, Some(figure)).expect("caption");
    doc.new_text("The Caption", Some(caption)).expect("caption text");
    doc
}

fn resolve_one(
    domain: &StandardDomain,
    ctx: &DomainContext<'_>,
    typ: &str,
    target: &str,
    node: scribe_ast::NodeId,
    content: scribe_ast::NodeId,
    doc: &Document,
) -> Option<scribe_domains::ResolvedReference> {
    domain.resolve_xref(ctx, typ, target, node, content, doc).expect("resolve")
}

#[test]
fn metadata_matches_upstream_standard_domain() {
    let domain = StandardDomain::new();
    assert_eq!(domain.name(), "std");
    assert_eq!(domain.label(), "Default");
    assert_eq!(domain.data_version(), 1);
    let names: Vec<&str> = domain.object_types().iter().map(|object| object.name.as_str()).collect();
    assert_eq!(names, ["term", "token", "label", "confval", "envvar", "cmdoption", "doc"]);
    assert_eq!(domain.object_types()[2].roles, ["ref", "keyword"]);
    assert_eq!(
        domain.directive_names(),
        &["program", "cmdoption", "option", "confval", "envvar", "glossary", "productionlist"]
    );
    for role in ["option", "confval", "envvar", "token", "term", "ref", "numref", "keyword", "doc", "download", "label", "any"] {
        assert!(domain.role_names().contains(&role), "missing role {role}");
    }
    assert_eq!(domain.get_type_name("label", false), "reference label");
    assert_eq!(domain.get_type_name("label", true), "reference label");
}

#[test]
fn ref_resolution_explicit_and_implicit_labels_lowercase() {
    let domain = StandardDomain::new();
    let env = TestEnv::default();
    let config = ConfigValue::Bool(true);
    let config = config_with(&[("nitpicky", config)]);
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");

    domain.note_label(&ctx, "my-label", "section-id", Some("My Section"), None);
    domain.note_label(&ctx, "anon-label", "anon-id", None, None);

    let mut doc = Document::new(Some("index.rst".to_owned()));
    let (node, inner, _) = add_xref(&mut doc, "std", "ref", "MY-LABEL", false, true, "MY-LABEL");
    let resolved = resolve_one(&domain, &ctx, "ref", "my-label", node, inner, &doc).expect("named label");
    assert_eq!(resolved.refid.as_deref(), Some("section-id"));
    assert_eq!(resolved.title.as_deref(), Some("My Section"));
    assert_eq!(resolved.classes, ["std", "std-ref"]);

    let (node2, inner2, _) = add_xref(&mut doc, "std", "ref", "ANON-LABEL", true, true, "Explicit title");
    // An explicit ref uses anonlabels, with the content as title.
    let resolved2 = domain.resolve_xref(&ctx, "ref", "anon-label", node2, inner2, &doc).expect("ok").expect("anon label");
    assert_eq!(resolved2.refid.as_deref(), Some("anon-id"));
    assert_eq!(resolved2.title.as_deref(), Some("Explicit title"));

    // Lowercase normalization is done by the resolver even when the parser
    // did not do it (upstream XRefRole(lowercase=True) does it at parse time).
    let (node3, inner3, _) = add_xref(&mut doc, "std", "ref", "MY-LABEL", false, true, "MY-LABEL");
    let lowered = resolve_one(&domain, &ctx, "ref", "MY-LABEL", node3, inner3, &doc).expect("uppercase target is lowered");
    assert_eq!(lowered.refid.as_deref(), Some("section-id"));
}

#[test]
fn keyword_uses_named_label_and_keeps_content() {
    let domain = StandardDomain::new();
    let env = TestEnv::default();
    let config = ConfigValue::Bool(false);
    let config = config_with(&[("nitpicky", config)]);
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");
    domain.note_label(&ctx, "keyword-label", "kw-id", Some("Keyword Label"), None);
    let mut doc = Document::new(None);
    let (node, inner, _) = add_xref(&mut doc, "std", "keyword", "keyword-label", false, true, "keyword-label");
    let resolved = resolve_one(&domain, &ctx, "keyword", "keyword-label", node, inner, &doc).expect("keyword");
    assert_eq!(resolved.refid.as_deref(), Some("kw-id"));
    assert!(resolved.title.is_none(), "keyword keeps the content node");
}

#[test]
fn doc_refs_relative_docnames_titles_and_fragments() {
    let domain = StandardDomain::new();
    let env = TestEnv::default().with_doc("index", "Index").with_doc("other", "Other Title").with_doc("dir/child", "Child");
    let config = config_with(&[]);
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");
    let mut doc = Document::new(None);
    let (node, inner, _) = add_xref(&mut doc, "std", "doc", "other", false, true, "other");
    let resolved = resolve_one(&domain, &ctx, "doc", "other", node, inner, &doc).expect("doc ref");
    assert_eq!(resolved.refuri.as_deref(), Some("other.pseudoxml"));
    assert_eq!(resolved.title.as_deref(), Some("Other Title"));
    assert_eq!(resolved.classes, ["doc"]);

    let (node2, inner2, _) = add_xref(&mut doc, "std", "doc", "dir/child", true, true, "Child custom");
    let resolved2 = resolve_one(&domain, &ctx, "doc", "dir/child", node2, inner2, &doc).expect("relative");
    assert_eq!(resolved2.refuri.as_deref(), Some("dir/child.pseudoxml"));
    assert_eq!(resolved2.title.as_deref(), Some("Child custom"));

    let (self_node, self_inner, _) = add_xref(&mut doc, "std", "doc", "index", false, true, "index");
    let self_ref = resolve_one(&domain, &ctx, "doc", "index", self_node, self_inner, &doc).expect("self doc");
    assert_eq!(self_ref.refuri.as_deref(), Some(""), "upstream self-link URI is empty");

    // Spec-requested `#anchor` fragment support (a Phase 11 extension over
    // upstream 9.x, which warns "unknown document" for such targets).
    let (node3, inner3, _) = add_xref(&mut doc, "std", "doc", "other#section", false, true, "other#section");
    let resolved3 = domain.resolve_xref(&ctx, "doc", "other#section", node3, inner3, &doc).expect("ok").expect("fragment");
    assert_eq!(resolved3.refuri.as_deref(), Some("other.pseudoxml#section"));

    let (node4, inner4, _) = add_xref(&mut doc, "std", "doc", "missing", false, true, "missing");
    assert!(resolve_one(&domain, &ctx, "doc", "missing", node4, inner4, &doc).is_none());
}

#[test]
fn download_files_resolve_to_generated_uris() {
    let domain = StandardDomain::new();
    let env = TestEnv::default();
    let config = config_with(&[]);
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");
    domain.note_download("index", "files/example.txt");
    let mut doc = Document::new(None);
    let (node, inner, _) = add_xref(&mut doc, "std", "download", "files/example.txt", false, false, "example.txt");
    let resolved = resolve_one(&domain, &ctx, "download", "files/example.txt", node, inner, &doc).expect("download");
    assert_eq!(resolved.node_kind, ReferenceNodeKind::DownloadReference);
    assert_eq!(resolved.refuri.as_deref(), Some("_downloads.pseudoxml/example.txt"));
    let (node2, inner2, _) = add_xref(&mut doc, "std", "download", "https://example.org/a.zip", false, false, "a.zip");
    let external = resolve_one(&domain, &ctx, "download", "https://example.org/a.zip", node2, inner2, &doc).expect("external");
    assert_eq!(external.refuri.as_deref(), Some("https://example.org/a.zip"));
}

#[test]
fn numref_formats_number_and_name() {
    let domain = StandardDomain::new();
    let doctree = fig_tree("figure-id");
    let env = TestEnv::default().with_doctree("index", "Index", doctree);
    let mut formats = BTreeMap::new();
    formats.insert("figure".to_owned(), ConfigValue::Str("Fig. {number}: {name}".to_owned()));
    let config = config_with(&[
        ("numfig", ConfigValue::Bool(true)),
        ("numfig_format", ConfigValue::Dict(formats)),
    ]);
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");
    domain.note_label(&ctx, "my-figure", "figure-id", Some("The Caption"), None);
    let mut doc = Document::new(None);
    let (node, inner, _) = add_xref(&mut doc, "std", "numref", "my-figure", false, true, "my-figure");
    let resolved = resolve_one(&domain, &ctx, "numref", "my-figure", node, inner, &doc).expect("numref");
    assert_eq!(resolved.node_kind, ReferenceNodeKind::NumberReference);
    assert_eq!(resolved.title.as_deref(), Some("Fig. 1.2: The Caption"));
    assert_eq!(resolved.reftitle.as_deref(), Some("Fig. {number}: {name}"));
    assert_eq!(resolved.refid.as_deref(), Some("figure-id"));

    // Explicit title wins over the configured format.
    let (node2, inner2, _) = add_xref(&mut doc, "std", "numref", "my-figure", true, true, "Custom {number}");
    let explicit = resolve_one(&domain, &ctx, "numref", "my-figure", node2, inner2, &doc).expect("explicit numref");
    assert_eq!(explicit.title.as_deref(), Some("Custom 1.2"));

    // Without numfig the resolver requests passthrough (no missing-reference).
    let no_numfig = config_with(&[("numfig", ConfigValue::Bool(false))]);
    let ctx2 = context(&env, &no_numfig, &sink, "index");
    let (node3, inner3, _) = add_xref(&mut doc, "std", "numref", "my-figure", false, true, "my-figure");
    let disabled = resolve_one(&domain, &ctx2, "numref", "my-figure", node3, inner3, &doc).expect("disabled");
    assert!(disabled.passthrough);
}

#[test]
fn glossary_terms_are_case_insensitive() {
    let domain = StandardDomain::new();
    let env = TestEnv::default();
    let config = config_with(&[]);
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");
    domain.note_term(&ctx, "Apple", "term-apple", None);
    let mut doc = Document::new(None);
    let (node, inner, _) = add_xref(&mut doc, "std", "term", "APPLE", false, true, "APPLE");
    let resolved = resolve_one(&domain, &ctx, "term", "APPLE", node, inner, &doc).expect("case-insensitive term");
    assert_eq!(resolved.refid.as_deref(), Some("term-apple"));
}

#[test]
fn program_options_context_aliases_and_values() {
    let domain = StandardDomain::new();
    let env = TestEnv::default();
    let config = config_with(&[]);
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");
    domain.note_program_option("index", Some("git"), "--verbose", "option-git-verbose");
    domain.note_program_option("index", Some("git-commit"), "--amend", "option-git-commit-amend");
    domain.note_program_option("index", None, "--global", "option-global");
    let mut doc = Document::new(None);

    let (node, inner, _) = add_xref(&mut doc, "std", "option", "--verbose", false, true, "--verbose");
    let resolved = domain.resolve_xref(&ctx.with_program(Some("git")), "option", "--verbose", node, inner, &doc).expect("ok").expect("program option");
    assert_eq!(resolved.refid.as_deref(), Some("option-git-verbose"));

    // Option values are stripped from the lookup key.
    let (node2, inner2, _) = add_xref(&mut doc, "std", "option", "--verbose=1", false, true, "--verbose=1");
    let with_value = domain.resolve_xref(&ctx.with_program(Some("git")), "option", "--verbose=1", node2, inner2, &doc).expect("ok").expect("value");
    assert_eq!(with_value.refid.as_deref(), Some("option-git-verbose"));

    // Subcommand folding: `git commit --amend` → program `git-commit`.
    let (node3, inner3, _) = add_xref(&mut doc, "std", "option", "commit --amend", false, true, "commit --amend");
    let folded = domain.resolve_xref(&ctx.with_program(Some("git")), "option", "commit --amend", node3, inner3, &doc).expect("ok").expect("subcommand");
    assert_eq!(folded.refid.as_deref(), Some("option-git-commit-amend"));

    // Without program context, global options still resolve.
    let (node4, inner4, _) = add_xref(&mut doc, "std", "option", "--global", false, true, "--global");
    let global = resolve_one(&domain, &ctx, "option", "--global", node4, inner4, &doc).expect("global");
    assert_eq!(global.refid.as_deref(), Some("option-global"));

    // `get_full_qualified_name` mirrors upstream's option handling.
    let mut option_node = Node::new(NodeKind::PendingXref);
    option_node.attrs.set_str("reftype", "option");
    option_node.attrs.set_str("reftarget", "commit --amend");
    option_node.attrs.set_str("std:program", "git");
    assert_eq!(domain.get_full_qualified_name(&option_node).as_deref(), Some("git-commit.--amend"));
}

#[test]
fn generic_object_roles_envvar_confval_token() {
    let domain = StandardDomain::new();
    let env = TestEnv::default();
    let config = config_with(&[]);
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");
    domain.note_object(&ctx, "envvar", "HOME", "envvar-home", None);
    domain.note_object(&ctx, "confval", "nitpicky", "confval-nitpicky", None);
    domain.note_object(&ctx, "token", "stmt", "token-stmt", None);
    let mut doc = Document::new(None);
    for (typ, target, expected) in [
        ("envvar", "HOME", "envvar-home"),
        ("confval", "nitpicky", "confval-nitpicky"),
        ("token", "stmt", "token-stmt"),
    ] {
        let (node, inner, _) = add_xref(&mut doc, "std", typ, target, false, true, target);
        let resolved = resolve_one(&domain, &ctx, typ, target, node, inner, &doc).unwrap_or_else(|| panic!("{typ}"));
        assert_eq!(resolved.refid.as_deref(), Some(expected));
    }
}

#[test]
fn any_dispatch_prefers_std_and_reports_ambiguity() {
    let domain = StandardDomain::new();
    let env = TestEnv::default();
    let config = config_with(&[]);
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");
    domain.note_label(&ctx, "target", "label-id", Some("Target"), None);
    domain.note_object(&ctx, "envvar", "target", "envvar-id", None);
    let mut doc = Document::new(None);
    let (node, inner, _) = add_xref(&mut doc, "", "any", "target", false, true, "target");
    let candidates = domain.resolve_any_xref(&ctx, "target", node, inner, &doc).expect("any");
    assert!(!candidates.is_empty());
    assert_eq!(candidates[0].0, "std:ref");
    assert!(candidates.iter().any(|(role, _)| role == "std:envvar"));
}

#[test]
fn get_objects_is_deterministic_and_sorted() {
    let domain = StandardDomain::new();
    let env = TestEnv::default();
    let config = config_with(&[]);
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");
    domain.note_object(&ctx, "envvar", "ZED", "envvar-zed", None);
    domain.note_object(&ctx, "envvar", "ALPHA", "envvar-alpha", None);
    domain.note_label(&ctx, "zeta", "label-zeta", Some("Zeta"), None);
    domain.note_program_option("index", Some("git"), "--verbose", "option-git-verbose");
    let objects = domain.get_objects();
    assert_eq!(objects, domain.get_objects(), "ordering is stable");
    let keys: Vec<(&str, &str)> = objects.iter().map(|entry| (entry.objtype.as_str(), entry.name.as_str())).collect();
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    assert_eq!(keys, sorted);
    assert!(objects.iter().any(|entry| entry.name == "--verbose" || entry.name == "git.--verbose"));
}

#[test]
fn clear_doc_and_merge_domaindata_are_consistent() {
    let registry = DomainRegistry::with_standard();
    let env = TestEnv::default();
    let config = config_with(&[]);
    let sink = DiagnosticSink::default();
    let mut ctx = context(&env, &config, &sink, "one");
    let mut doc = Document::new(None);
    let id = add_label(&mut doc, "one-label", "one-id");
    assert!(doc.get(id).is_ok());
    registry.process_document(&ctx, "one", &doc).expect("process one");

    // A `target` node alone is registered as an explicit label.
    let objects = registry.get_objects();
    assert!(objects.iter().any(|entry| entry.name == "one-label"));

    registry.clear_doc("one");
    assert!(!registry.get_objects().iter().any(|entry| entry.name == "one-label"));

    // Merge from another inventory for a subset of docnames.
    ctx = context(&env, &config, &sink, "two");
    let other_domain = StandardDomain::new();
    other_domain.note_object(&ctx, "envvar", "OTHER", "envvar-other", None);
    let data = other_domain.export_data().expect("export");
    let mut other = BTreeMap::new();
    other.insert("std".to_owned(), data);
    registry.merge_domaindata(&["two".to_owned()], &other);
    assert!(registry.get_objects().iter().any(|entry| entry.name == "OTHER"));

    // Data for a docname not in the merge set is ignored.
    registry.clear_doc("two");
    assert!(!registry.get_objects().iter().any(|entry| entry.name == "OTHER"));
}

#[test]
fn duplicate_labels_emit_structured_diagnostics() {
    let domain = StandardDomain::new();
    let env = TestEnv::default();
    let config = config_with(&[]);
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");
    domain.note_label(&ctx, "dup", "id-1", Some("One"), None);
    let other_ctx = context(&env, &config, &sink, "other");
    domain.note_label(&other_ctx, "dup", "id-2", Some("Two"), None);
    let diagnostics = sink.snapshot();
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0].code, scribe_domains::codes::DUPLICATE_OBJECT);
    assert_eq!(diagnostics[0].target.as_deref(), Some("dup"));
    assert!(diagnostics[0].message.contains("duplicate label dup, other instance in index"));
}

#[test]
fn malformed_xrefs_never_panic() {
    let domain = StandardDomain::new();
    let env = TestEnv::default();
    let config = config_with(&[]);
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");
    let mut doc = Document::new(None);
    let (node, inner, _) = add_xref(&mut doc, "std", "ref", "", false, true, "");
    assert!(resolve_one(&domain, &ctx, "ref", "", node, inner, &doc).is_none());
    assert!(domain.resolve_xref(&ctx, "unknown-type", "", scribe_ast::NodeId::NULL, scribe_ast::NodeId::NULL, &doc).is_ok());
    assert!(domain.resolve_any_xref(&ctx, "", scribe_ast::NodeId::NULL, scribe_ast::NodeId::NULL, &doc).is_ok());

    // A full resolution pass on a malformed document must not panic either.
    let registry = DomainRegistry::with_standard();
    let report = resolve_pending_xrefs(&registry, &ctx, &mut doc).expect("resolve pass");
    assert_eq!(report.unresolved, 1);
}

#[test]
fn process_doc_registers_glossary_and_production_tokens() {
    let registry = DomainRegistry::with_standard();
    let env = TestEnv::default();
    let config = config_with(&[]);
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");
    let mut doc = Document::new(None);
    let glossary = doc.add(NodeKind::Glossary, Some(doc.root)).expect("glossary");
    let item = doc.add(NodeKind::DefinitionListItem, Some(glossary)).expect("item");
    let term = doc.add(NodeKind::Term, Some(item)).expect("term");
    doc.new_text("boson", Some(term)).expect("term text");
    let productionlist = doc.add(NodeKind::Productionlist, Some(doc.root)).expect("productions");
    let production = doc.add(NodeKind::Production, Some(productionlist)).expect("production");
    let token = doc.add(NodeKind::LiteralStrong, Some(production)).expect("token target");
    if let Ok(node) = doc.get_mut(token) {
        node.attrs.set_list("ids", vec!["grammar-token-stmt".to_owned()]);
    }
    doc.new_text("stmt", Some(token)).expect("token text");
    if let Ok(node) = doc.get_mut(production) {
        node.attrs.set_str("tokenname", "stmt");
    }
    registry.process_document(&ctx, "index", &doc).expect("process");
    let objects = registry.get_objects();
    assert!(objects.iter().any(|entry| entry.name == "boson" && entry.objtype == "term"));
    assert!(objects.iter().any(|entry| entry.name == "stmt" && entry.objtype == "token"));
}

#[test]
fn pipeline_resolves_pending_xrefs_in_place() {
    use scribe_core::{apply_transforms, Phase, TransformContext};

    let env = TestEnv::default();
    let config = config_with(&[]);
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");
    let domain = StandardDomain::new();
    domain.note_label(&ctx, "target", "target-id", Some("Target Title"), None);
    let registry = DomainRegistry::new();
    registry.register(domain);

    let mut doc = Document::new(None);
    doc.metadata.docname = Some("index".to_owned());
    let _paragraph = doc.add(NodeKind::Paragraph, Some(doc.root)).expect("paragraph");
    let (xref, _, _) = add_xref(&mut doc, "std", "ref", "target", false, true, "TARGET");
    let xref_index = xref.0 as usize;

    let transform_ctx = TransformContext::new(Some(&config), None, "pseudoxml", "pseudoxml").with_domains(&registry);
    let report = apply_transforms(&mut doc, &transform_ctx, Phase::Read);
    assert!(!report.has_diagnostics(), "{:?}", report.diagnostics);
    assert_eq!(doc.nodes[xref_index].kind, NodeKind::Reference);
    assert_eq!(
        doc.nodes[xref_index].attrs.get("refid"),
        Some(&AttributeValue::Str("target-id".to_owned()))
    );
    let pseudo = doc.to_pseudoxml().expect("pseudoxml");
    assert!(pseudo.contains("<reference"), "{pseudo}");
    assert!(pseudo.contains("Target Title"), "{pseudo}");
}
