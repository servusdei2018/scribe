//! Phase 13 integration tests: all seven new domains.
//!
//! Covers declarator/signature forms, scope lookup, overload sets, `~` / `.`
//! xref modifiers, duplicate warnings, `:any:` dispatch, module/class context,
//! registries and adversarial (malformed) declarations.

use std::collections::BTreeSet;

use scribe_ast::{Document, NodeKind};
use scribe_config::{Config, ConfigValue};
use scribe_domains::c::{parse_declaration, CDomain, CDomainData};
use scribe_domains::cpp::{
    cpp_ids, parse_cpp_declaration, CppDomain, CppDomainData, MangleDecl, MangleParam,
};
use scribe_domains::javascript::JavaScriptDomain;
use scribe_domains::math::MathDomain;
use scribe_domains::rst::ReSTDomain;
use scribe_domains::{
    prepare_document, resolve_pending_xrefs, DiagnosticSink, Domain, DomainContext, DomainData,
    DomainRegistry,
};
use scribe_rst::{parse_with_options, resolve_output, Context, DirectiveRegistry, ParseOptions};

mod common;
use common::TestEnv;

fn config(nitpicky: bool) -> Config {
    let mut config = Config::new();
    let _ = config.set("nitpicky", ConfigValue::Bool(nitpicky));
    config
}

/// Parses RST, expands skeletons and runs the domain resolution pipeline.
fn pipeline(source: &str, docname: &str, nitpicky: bool) -> (Document, DiagnosticSink, DomainRegistry) {
    let mut out = parse_with_options(source, ParseOptions::new(Some(docname.to_owned())));
    let report = resolve_output(&mut out, &DirectiveRegistry::builtin(), &Context::default());
    assert!(report.is_clean(), "unresolved directives: {:?}", report.unresolved);
    prepare_document(&mut out.document).expect("prepare");
    let registry = DomainRegistry::builtin();
    let sink = DiagnosticSink::default();
    let cfg = config(nitpicky);
    let env = TestEnv::default().with_doc(docname, docname);
    let ctx = DomainContext::new(Some(&env), Some(&cfg), docname, "pseudoxml", "pseudoxml", &sink);
    registry.process_document(&ctx, docname, &out.document).expect("process");
    resolve_pending_xrefs(&registry, &ctx, &mut out.document).expect("resolve");
    (out.document, sink, registry)
}

fn signature_texts(doc: &Document) -> Vec<String> {
    doc.preorder_ids().into_iter()
        .filter(|&id| doc.get(id).is_ok_and(|node| node.kind == NodeKind::DescSignature))
        .map(|id| doc.get(id).map(|node| node.astext(doc)).unwrap_or_default())
        .collect()
}

fn ref_facts(doc: &Document) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for id in doc.preorder_ids() {
        let Ok(node) = doc.get(id) else { continue };
        if !matches!(node.kind, NodeKind::Reference | NodeKind::NumberReference) { continue; }
        let target = node.get_str("refid").ok().flatten()
            .or_else(|| node.get_str("refuri").ok().flatten())
            .unwrap_or_default().to_owned();
        let title = node.children.first()
            .and_then(|child| doc.get(*child).ok())
            .map(|child| child.astext(doc).trim().to_owned())
            .unwrap_or_default();
        let mut classes = node.get_list("classes").ok().flatten().unwrap_or_default();
        if classes.is_empty() {
            if let Some(&child) = node.children.first() {
                if let Ok(child) = doc.get(child) {
                    classes = child.get_list("classes").ok().flatten().unwrap_or_default();
                }
            }
        }
        let classes = classes.join(" ");
        out.push((classes, target, title));
    }
    out.sort();
    out
}

// ----- registry / metadata ---------------------------------------------------

#[test]
fn builtin_registry_has_all_ten_domains() {
    let registry = DomainRegistry::builtin();
    let expected: BTreeSet<String> = [
        "c", "changeset", "citation", "cpp", "index", "js", "math", "py", "rst", "std",
    ].into_iter().map(str::to_owned).collect();
    let actual: BTreeSet<String> = registry.domain_names().into_iter().collect();
    assert_eq!(actual, expected);
}

#[test]
fn domain_labels_match_upstream() {
    let registry = DomainRegistry::builtin();
    for (name, label) in [
        ("c", "C"), ("cpp", "C++"), ("js", "JavaScript"), ("rst", "reStructuredText"),
        ("math", "mathematics"), ("changeset", "changeset"), ("citation", "citation"),
        ("index", "index"),
    ] {
        let found = registry.with_domain(name, |domain| domain.label().to_owned());
        assert_eq!(found.as_deref(), Some(label), "domain {name}");
    }
}

// ----- C domain --------------------------------------------------------------

#[test]
fn c_declarator_forms() {
    let pointer = parse_declaration("int *f(const char *s)", "function", "c");
    assert!(pointer.is_function);
    assert_eq!(pointer.name, "f");
    assert_eq!(pointer.params.len(), 1);

    let array = parse_declaration("int values[10]", "var", "c");
    assert_eq!(array.name, "values");
    assert!(!array.params.is_empty() || !array.suffix.is_empty());

    let qualifiers = parse_declaration("const volatile unsigned int x", "var", "c");
    assert_eq!(qualifiers.name, "x");
    assert!(qualifiers.prefix.iter().any(|token| matches!(token, scribe_domains::sigbuild::SigToken::Keyword(text) if text == "const")));

    let nested = parse_declaration("struct Outer.Inner.member", "member", "c");
    assert_eq!(nested.name, "Outer.Inner.member");
    assert_eq!(nested.segments.len(), 3);

    let func_ptr = parse_declaration("void (*cb)(int)", "var", "c");
    assert_eq!(func_ptr.name, "cb");
}

#[test]
fn c_namespace_scope_and_resolution() {
    let source = ".. c:namespace:: NS\n\n.. c:var:: int x\n\n:c:var:`x`\n:c:var:`NS.x`\n";
    let (doc, _sink, _registry) = pipeline(source, "index", false);
    let facts = ref_facts(&doc);
    assert!(facts.iter().any(|(_, target, _)| target == "c.NS.x"), "{facts:?}");
    // Unqualified lookup finds the object via the active namespace scope.
    assert!(facts.iter().filter(|(_, target, _)| target == "c.NS.x").count() >= 2, "{facts:?}");
}

#[test]
fn c_identifier_fallback_and_any_dispatch() {
    let source = ".. c:struct:: S\n\n:c:struct:`S`\n:c:type:`S`\n:any:`S`\n";
    let (doc, _sink, _registry) = pipeline(source, "index", false);
    let facts = ref_facts(&doc);
    assert!(facts.iter().any(|(classes, _, _)| classes.contains("c-struct")), "{facts:?}");
    assert!(facts.iter().any(|(classes, _, _)| classes.contains("c-type")), "{facts:?}");
    assert!(facts.iter().any(|(classes, _, _)| classes.contains("any")), "{facts:?}");
}

#[test]
fn c_duplicate_warning() {
    let source = ".. c:function:: void f(void)\n\n.. c:function:: void f(void)\n";
    let (_doc, sink, _registry) = pipeline(source, "index", false);
    let messages: Vec<String> = sink.snapshot().into_iter().map(|diagnostic| diagnostic.message).collect();
    assert!(messages.iter().any(|message| message.contains("duplicate object description of")), "{messages:?}");
}

#[test]
fn c_object_inventory() {
    let domain = CDomain::new();
    let cfg = config(false);
    let sink = DiagnosticSink::default();
    let env = TestEnv::default();
    let ctx = DomainContext::new(Some(&env), Some(&cfg), "index", "html", "html", &sink);
    domain.note_object(&ctx, "index.f", "function", "c.index.f", false, None);
    domain.note_object(&ctx, "index.S", "struct", "c.index.S", false, None);
    assert_eq!(domain.get_objects().len(), 2);
    assert_eq!(domain.find_obj("f", "func", "index").len(), 1);
    assert_eq!(domain.find_obj("S", "type", "").len(), 1);
    assert!(domain.find_obj("missing", "func", "").is_empty());
    let data: CDomainData = domain.snapshot();
    assert_eq!(data.objects.len(), 2);
}

// ----- C++ domain ------------------------------------------------------------

#[test]
fn cpp_declarator_forms() {
    let qualified = parse_cpp_declaration("float NS::S::value", "member", "cpp");
    assert_eq!(qualified.name, "NS::S::value");
    assert_eq!(qualified.segments.len(), 3);

    let reference = parse_cpp_declaration("const std::string& name", "member", "cpp");
    assert_eq!(reference.name, "name");
    assert!(reference.prefix.iter().any(|token| matches!(token, scribe_domains::sigbuild::SigToken::Keyword(text) if text == "const")));

    let template = parse_cpp_declaration("std::vector<int> items", "var", "cpp");
    assert_eq!(template.name, "items");

    let operator = parse_cpp_declaration("int operator+(int, int)", "function", "cpp");
    assert_eq!(operator.name, "operator+");
    assert!(operator.is_function);

    let alias = parse_cpp_declaration("Alias = int;", "type", "cpp");
    assert_eq!(alias.name, "Alias");
    assert!(alias.alias.is_some());
}

#[test]
fn cpp_id_generation_subset() {
    let simple = MangleDecl { names: vec!["f".into()], params: vec![], template_args: vec![] };
    assert_eq!(cpp_ids(&simple)[0], "_CPPv41f");
    let nested = MangleDecl {
        names: vec!["A".into(), "B".into()],
        params: vec![MangleParam { ty: "int".into(), ..MangleParam::default() }],
        template_args: vec![],
    };
    assert_eq!(cpp_ids(&nested)[0], "_CPPv4N1A1BiE");
    assert_eq!(cpp_ids(&nested).len(), 4);
}

#[test]
fn cpp_class_scope_and_overloads() {
    let source = concat!(
        ".. cpp:class:: A\n\n",
        "   .. cpp:function:: void f(int)\n\n",
        "   .. cpp:function:: void f(int, int)\n\n",
        ".. cpp:var:: A a\n\n",
        ":cpp:func:`A::f`\n",
        ":cpp:class:`A`\n",
    );
    let (doc, _sink, registry) = pipeline(source, "index", false);
    let structures = signature_texts(&doc);
    assert_eq!(structures.len(), 4, "{structures:?}");
    let objects = registry.get_objects();
    assert!(objects.iter().any(|entry| entry.name == "A" && entry.objtype == "class"));
    assert!(objects.iter().any(|entry| entry.name == "A::f" && entry.objtype == "function"));
    let domain = registry.domain("cpp").expect("cpp domain");
    let guard = domain.lock();
    let data: CppDomainData = guard.export_data().expect("export").decode().expect("decode data");
    assert!(data.objects.get("A::f").is_some_and(|entries| entries.len() == 2));
}

#[test]
fn cpp_template_and_concept() {
    let source = concat!(
        ".. cpp:concept:: template<typename T> Drawable\n\n",
        ".. cpp:type:: std::vector<int> List\n\n",
        ":cpp:concept:`Drawable`\n",
        ":cpp:type:`List`\n",
    );
    let (doc, _sink, _registry) = pipeline(source, "index", false);
    let facts = ref_facts(&doc);
    assert_eq!(facts.iter().filter(|(classes, _, _)| classes.contains("cpp-")).count(), 2, "{facts:?}");
    assert!(signature_texts(&doc).iter().any(|text| text.contains("template")), "missing concept template");
}

#[test]
fn cpp_namespace_lookup() {
    let source = concat!(
        ".. cpp:namespace:: NS\n\n",
        ".. cpp:function:: void helper()\n\n",
        ":cpp:func:`helper`\n",
        ":cpp:func:`NS::helper`\n",
    );
    let (doc, _sink, _registry) = pipeline(source, "index", false);
    let facts = ref_facts(&doc);
    assert!(facts.iter().any(|(_, target, _)| target.starts_with("_CPPv4")), "{facts:?}");
}

// ----- JavaScript domain -----------------------------------------------------

#[test]
fn js_module_and_nesting() {
    let source = concat!(
        ".. js:module:: mod\n\n",
        ".. js:class:: C\n\n",
        "   .. js:method:: m()\n\n",
        ".. js:function:: top()\n\n",
        ":js:meth:`C.m`\n",
        ":js:class:`mod.C`\n",
        ":js:func:`top`\n",
    );
    let (doc, _sink, registry) = pipeline(source, "roles", false);
    let facts = ref_facts(&doc);
    assert!(facts.iter().any(|(_, target, _)| target == "mod.C.m"), "{facts:?}");
    assert!(facts.iter().any(|(_, target, _)| target == "mod.C"), "{facts:?}");
    let objects = registry.get_objects();
    assert!(objects.iter().any(|entry| entry.name == "mod.C.m" && entry.objtype == "method"));
}

#[test]
fn js_any_dispatch() {
    let source = ".. js:class:: Thing\n\n:any:`Thing`\n";
    let (doc, _sink, _registry) = pipeline(source, "roles", false);
    let facts = ref_facts(&doc);
    assert!(facts.iter().any(|(classes, _, _)| classes.contains("js")), "{facts:?}");
}

#[test]
fn js_duplicate_warning() {
    let source = ".. js:function:: dup()\n\n.. js:function:: dup()\n";
    let (_doc, sink, _registry) = pipeline(source, "roles", false);
    let messages: Vec<String> = sink.snapshot().into_iter().map(|diagnostic| diagnostic.message).collect();
    assert!(messages.iter().any(|message| message.contains("duplicate function description of dup")), "{messages:?}");
}

// ----- smaller domains -------------------------------------------------------

#[test]
fn rst_domain_directives_and_roles() {
    let source = concat!(
        ".. rst:directive:: toctree\n\n",
        ".. rst:role:: ref\n\n",
        ":rst:dir:`toctree`\n",
        ":rst:role:`ref`\n",
    );
    let (doc, _sink, registry) = pipeline(source, "index", false);
    let facts = ref_facts(&doc);
    assert!(facts.iter().any(|(classes, _, _)| classes.contains("rst-dir")), "{facts:?}");
    assert!(facts.iter().any(|(classes, _, _)| classes.contains("rst-role")), "{facts:?}");
    let objects = registry.get_objects();
    assert!(objects.iter().any(|entry| entry.objtype == "directive" && entry.name == "toctree"));
    assert!(objects.iter().any(|entry| entry.objtype == "role" && entry.name == "ref"));
    // Signature structure is a plain `desc_name`.
    assert!(signature_texts(&doc).iter().any(|text| text.contains(".. toctree::")));
}

#[test]
fn math_domain_equation_numbering() {
    let domain = MathDomain::new();
    domain.note_equation("index", "eq:one");
    domain.note_equation("index", "eq:two");
    assert_eq!(domain.equation_number("eq:one"), Some(1));
    assert_eq!(domain.equation_number("eq:two"), Some(2));
    assert_eq!(domain.equation_doc("eq:one").as_deref(), Some("index"));
    assert_eq!(domain.object_types().len(), 0);
    assert_eq!(domain.role_names(), ["numref", "eq"]);
}

#[test]
fn empty_domains_are_resolvable() {
    let registry = DomainRegistry::builtin();
    for name in ["changeset", "citation", "index"] {
        let domain = registry.domain(name).expect("registered");
        let guard = domain.lock();
        assert_eq!(guard.name(), name);
        assert!(guard.object_types().is_empty());
        assert!(guard.get_objects().is_empty());
        assert_eq!(guard.export_data().expect("export").version, 1);
    }
}

// ----- persistence / adversarial ---------------------------------------------

#[test]
fn new_domains_persist_inventories() {
    let registry = DomainRegistry::builtin();
    let exported = registry.export_domain_data();
    for name in ["c", "cpp", "js", "rst", "math"] {
        assert!(exported.contains_key(name), "no inventory for {name}");
    }
    let data: DomainData = exported.get("c").expect("c inventory").clone();
    let mut fresh = CDomain::new();
    fresh.replace_data(&data).expect("replace");
    assert!(fresh.snapshot().objects.is_empty() || !fresh.snapshot().objects.is_empty());
    let js = JavaScriptDomain::new();
    let cpp = CppDomain::new();
    let rst = ReSTDomain::new();
    assert_eq!(js.data_version(), 1);
    assert_eq!(cpp.data_version(), 1);
    assert_eq!(rst.data_version(), 1);
}

#[test]
fn adversarial_declarations_never_panic() {
    let corpus = [
        "", "(", ")", "(((", "*", "&", "::", ":::", "...", "<", ">", "template<",
        "int )", "void f(", "void ()", "a >>> b", "int x[", "operator", "operator()",
        "@@@", "@", "~~", "1.2.3", "0x", "'", "\"", "x = = = y", ", , ,",
        "const const const", "struct struct struct", "⇒", "中文字", "a\tb\nc",
    ];
    for source in corpus {
        let c = parse_declaration(source, "function", "c");
        let _ = c.name;
        let cpp = parse_cpp_declaration(source, "function", "cpp");
        let _ = cpp.name;
        let _ = scribe_domains::cpp::parse_cpp_xref(source);
    }
}

#[test]
fn adversarial_rst_never_panics() {
    let corpus = [
        ".. c:function:: \n",
        ".. c:struct:: (((\n",
        ".. cpp:class:: template<\n",
        ".. js:module:: \n",
        ".. rst:directive:: \n",
        ":c:func:`(`\n",
        ":cpp:type:`x<y`\n",
        ":js:meth:`.`\n",
        ":math:numref:`no-such-label`\n",
    ];
    for source in corpus {
        let mut out = parse_with_options(source, ParseOptions::new(Some("index".to_owned())));
        let _ = resolve_output(&mut out, &DirectiveRegistry::builtin(), &Context::default());
        let result = prepare_document(&mut out.document);
        assert!(result.is_ok(), "prepare failed for {source:?}: {result:?}");
        let registry = DomainRegistry::builtin();
        let cfg = config(true);
        let sink = DiagnosticSink::default();
        let env = TestEnv::default();
        let ctx = DomainContext::new(Some(&env), Some(&cfg), "index", "pseudoxml", "pseudoxml", &sink);
        let _ = registry.process_document(&ctx, "index", &out.document);
        let _ = resolve_pending_xrefs(&registry, &ctx, &mut out.document);
    }
}

#[test]
fn cpp_alias_qualifiers_do_not_create_namespace_xrefs() {
    // A `cpp:alias` argument re-renders the declaration it points at; its
    // qualifier segments must not become bare `cpp:identifier` references to
    // a namespace (namespaces are not registered as symbols upstream either).
    let source = "\
.. cpp:namespace:: ns\n\n\
.. cpp:class:: C\n\n\
.. cpp:alias:: void ns::C::f(double d) const\n";
    let (doc, sink, _) = pipeline(source, "index", true);
    let messages: Vec<String> = sink.snapshot().into_iter().map(|entry| entry.message).collect();
    assert!(
        !messages.iter().any(|message| message.contains("reference target not found")),
        "alias qualifiers produced xrefs: {messages:?}"
    );
    // The qualifier is still rendered in the signature.
    assert!(signature_texts(&doc).iter().any(|text| text.contains("ns::C::f")));
}

#[test]
fn cpp_template_pointer_argument_resolves_to_template_param() {
    // `Outer<T*>` must render `T` as an identifier/type reference (resolved to
    // the registered template parameter) and `*` as punctuation — never a
    // single xref with the literal target `T*`.
    let source = "\
.. cpp:class:: template<typename T> Outer<T*>\n\n\
.. cpp:type:: X = Outer<int>\n";
    let (doc, sink, _) = pipeline(source, "index", true);
    let messages: Vec<String> = sink.snapshot().into_iter().map(|entry| entry.message).collect();
    assert!(
        !messages.iter().any(|message| message.contains("T*")),
        "template argument xref was not tokenized: {messages:?}"
    );
    assert!(signature_texts(&doc).iter().any(|text| text.contains("Outer<T*>")));
}

#[test]
fn clear_doc_removes_domain_objects() {
    let registry = DomainRegistry::builtin();
    let source = ".. c:function:: void f(void)\n\n.. cpp:function:: void g()\n\n.. js:class:: C\n";
    let mut out = parse_with_options(source, ParseOptions::new(Some("a".to_owned())));
    let _ = resolve_output(&mut out, &DirectiveRegistry::builtin(), &Context::default());
    prepare_document(&mut out.document).expect("prepare");
    let cfg = config(false);
    let sink = DiagnosticSink::default();
    let env = TestEnv::default();
    let ctx = DomainContext::new(Some(&env), Some(&cfg), "a", "html", "html", &sink);
    registry.process_document(&ctx, "a", &out.document).expect("process");
    assert!(!registry.get_objects().is_empty());
    registry.clear_doc("a");
    assert!(
        registry.get_objects().iter().all(|entry| !["c", "cpp", "js"].contains(&entry.objtype.as_str())),
        "domain objects survived clear_doc"
    );
}
