//! Nitpicky missing-reference diagnostics and warning suppression.

mod common;

use scribe_ast::{Node, NodeKind};
use scribe_config::ConfigValue;
use scribe_core::{Phase, TransformContext};
use scribe_domains::{
    codes, resolve_pending_xrefs, DiagnosticSink, DomainRegistry, StandardDomain,
};

use common::{add_xref, config_with, context, str_list, TestEnv};

#[test]
fn nitpicky_ref_message_code_span_and_help_are_exact() {
    let config = config_with(&[("nitpicky", ConfigValue::Bool(true))]);
    let env = TestEnv::default().with_doc("index", "Index");
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");
    let registry = DomainRegistry::with_standard();
    let mut doc = scribe_ast::Document::new(None);
    let (node, _, _) = add_xref(&mut doc, "std", "ref", "no-label", false, true, "content");
    let report = resolve_pending_xrefs(&registry, &ctx, &mut doc).expect("resolve");
    assert_eq!(report.unresolved, 1);
    assert_eq!(doc.nodes[node.0 as usize].kind, NodeKind::Inline);
    let diagnostics = sink.snapshot();
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    let diagnostic = &diagnostics[0];
    assert_eq!(diagnostic.code, codes::MISSING_REFERENCE);
    assert_eq!(diagnostic.message, "undefined label: 'no-label'");
    assert_eq!(diagnostic.target.as_deref(), Some("no-label"));
    assert_eq!(diagnostic.typ.as_deref(), Some("ref"));
    assert_eq!(diagnostic.docname.as_deref(), Some("index"));
    assert!(diagnostic.span.is_some());
    assert!(diagnostic.help.as_ref().is_some_and(|help| help.contains("no-label")));
}

#[test]
fn anonlabel_without_title_gets_upstream_message() {
    let config = config_with(&[("nitpicky", ConfigValue::Bool(true))]);
    let env = TestEnv::default();
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");
    let domain = StandardDomain::new();
    domain.note_label(&ctx, "label", "label-id", None, None);
    let registry = DomainRegistry::new();
    registry.register(domain);

    let mut doc = scribe_ast::Document::new(None);
    add_xref(&mut doc, "std", "ref", "label", false, true, "content");
    let report = resolve_pending_xrefs(&registry, &ctx, &mut doc).expect("resolve");
    assert_eq!(report.unresolved, 1);
    let diagnostics = ctx.diagnostics().snapshot();
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(
        diagnostics[0].message,
        "Failed to create a cross reference. A title or caption not found: 'label'"
    );
}

#[test]
fn dangling_warning_messages_are_exact_per_type() {
    let cases = [
        ("doc", "missing", "unknown document: 'missing'"),
        ("term", "widget", "term not in glossary: 'widget'"),
        ("option", "--foo", "unknown option: '--foo'"),
        ("numref", "fig", "undefined label: 'fig'"),
        ("keyword", "kw", "unknown keyword: 'kw'"),
    ];
    for (typ, target, expected) in cases {
        let config = config_with(&[("nitpicky", ConfigValue::Bool(true))]);
        let env = TestEnv::default().with_doc("index", "Index");
        let sink = DiagnosticSink::default();
        let ctx = context(&env, &config, &sink, "index");
        let registry = DomainRegistry::with_standard();
        let mut doc = scribe_ast::Document::new(None);
        let (node, _, _) = add_xref(&mut doc, "std", typ, target, false, true, "content");
        let message = registry
            .missing_reference_message(&ctx, Some("std"), typ, target, node, &doc)
            .expect("message");
        assert_eq!(message, expected, "typ {typ}");
    }
}

#[test]
fn nitpick_ignore_by_domain_and_short_type() {
    for ignored in ["std:ref:no-label", "ref:no-label"] {
        let config = config_with(&[
            ("nitpicky", ConfigValue::Bool(true)),
            ("nitpick_ignore", str_list(&[ignored])),
        ]);
        let env = TestEnv::default();
        let sink = DiagnosticSink::default();
        let ctx = context(&env, &config, &sink, "index");
        let registry = DomainRegistry::with_standard();
        let mut doc = scribe_ast::Document::new(None);
        add_xref(&mut doc, "std", "ref", "no-label", false, true, "content");
        let report = resolve_pending_xrefs(&registry, &ctx, &mut doc).expect("resolve");
        assert_eq!(report.unresolved, 1);
        assert!(ctx.diagnostics().snapshot().is_empty(), "ignored by {ignored}");
    }
}

#[test]
fn nitpick_ignore_regex_and_suppress_warnings() {
    let config = config_with(&[
        ("nitpicky", ConfigValue::Bool(true)),
        ("nitpick_ignore_regex", str_list(&["std:ref:no-.*"])),
        ("suppress_warnings", str_list(&["ref"])),
    ]);
    let env = TestEnv::default();
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");
    let registry = DomainRegistry::with_standard();
    let mut doc = scribe_ast::Document::new(None);
    add_xref(&mut doc, "std", "ref", "no-label", false, true, "content");
    let _ = resolve_pending_xrefs(&registry, &ctx, &mut doc).expect("resolve");
    assert!(ctx.diagnostics().snapshot().is_empty());
}

#[test]
fn nitpick_ignore_python_tuple_form() {
    let ignore = ConfigValue::List(vec![ConfigValue::List(vec![
        ConfigValue::Str("std:doc".to_owned()),
        ConfigValue::Str("missing".to_owned()),
    ])]);
    let config = config_with(&[
        ("nitpicky", ConfigValue::Bool(true)),
        ("nitpick_ignore", ignore),
    ]);
    let env = TestEnv::default().with_doc("index", "Index");
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");
    let registry = DomainRegistry::with_standard();
    let mut doc = scribe_ast::Document::new(None);
    add_xref(&mut doc, "std", "doc", "missing", false, true, "missing");
    let _ = resolve_pending_xrefs(&registry, &ctx, &mut doc).expect("resolve");
    assert!(ctx.diagnostics().snapshot().is_empty(), "tuple-form ignore must apply");
}

#[test]
fn non_nitpicky_without_refwarn_is_silent() {
    let config = config_with(&[("nitpicky", ConfigValue::Bool(false))]);
    let env = TestEnv::default();
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");
    let registry = DomainRegistry::with_standard();
    let mut doc = scribe_ast::Document::new(None);
    let (node, _, _) = add_xref(&mut doc, "std", "ref", "no-label", false, false, "content");
    let report = resolve_pending_xrefs(&registry, &ctx, &mut doc).expect("resolve");
    assert_eq!(report.unresolved, 1);
    assert!(ctx.diagnostics().snapshot().is_empty());
    assert_eq!(doc.nodes[node.0 as usize].kind, NodeKind::Inline);
}

#[test]
fn transform_seam_routes_domain_diagnostics_to_phase10_sink() {
    let config = config_with(&[("nitpicky", ConfigValue::Bool(true))]);
    let env = TestEnv::default();
    let sink = DiagnosticSink::default();
    let _ctx = context(&env, &config, &sink, "index");
    let registry = DomainRegistry::with_standard();
    let mut doc = scribe_ast::Document::new(None);
    doc.metadata.docname = Some("index".to_owned());
    let (node, _, _) = add_xref(&mut doc, "std", "ref", "no-label", false, true, "content");
    let _ = node;

    let transform_ctx = TransformContext::new(Some(&config), None, "pseudoxml", "pseudoxml")
        .with_domains(&registry);
    let report = scribe_core::apply_transforms(&mut doc, &transform_ctx, Phase::Resolved);
    let domain_diagnostics = report.domain_diagnostics();
    assert_eq!(domain_diagnostics.len(), 1, "{domain_diagnostics:?}");
    assert_eq!(domain_diagnostics[0].code, codes::MISSING_REFERENCE);
    assert_eq!(domain_diagnostics[0].message, "undefined label: 'no-label'");
    assert_eq!(transform_ctx.domain_diagnostics().len(), 1);
}

#[test]
fn missing_reference_on_empty_target_never_panics() {
    let config = config_with(&[("nitpicky", ConfigValue::Bool(true))]);
    let env = TestEnv::default();
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");
    let registry = DomainRegistry::with_standard();
    let mut doc = scribe_ast::Document::new(None);
    add_xref(&mut doc, "std", "ref", "", false, true, "");
    let report = resolve_pending_xrefs(&registry, &ctx, &mut doc).expect("resolve");
    assert_eq!(report.unresolved, 1);
    assert_eq!(ctx.diagnostics().snapshot()[0].message, "undefined label: ''");
}

#[test]
fn unresolved_content_preserves_text_and_classes() {
    let config = config_with(&[]);
    let env = TestEnv::default();
    let sink = DiagnosticSink::default();
    let ctx = context(&env, &config, &sink, "index");
    let registry = DomainRegistry::with_standard();
    let mut doc = scribe_ast::Document::new(None);
    let (node, _, _) = add_xref(&mut doc, "std", "ref", "missing", false, false, "Fallback text");
    let _ = resolve_pending_xrefs(&registry, &ctx, &mut doc).expect("resolve");
    let replacement = &doc.nodes[node.0 as usize];
    assert_eq!(replacement.kind, NodeKind::Inline);
    let pseudo = doc.to_pseudoxml().expect("pseudoxml");
    assert!(pseudo.contains("Fallback text"), "{pseudo}");
    assert!(pseudo.contains("std-ref"), "{pseudo}");
    let _ = Node::new(NodeKind::Inline);
}
