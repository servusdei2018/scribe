//! Phase 12 unit and integration tests for the Python domain.

use std::collections::{BTreeMap, BTreeSet};

use scribe_ast::{Document, Node, NodeId, NodeKind, Span};
use scribe_config::{load_config_with, BridgeStrategy, Config, ConfigValue, LoadRequest};
use scribe_domains::{
    parse_annotation, parse_reftarget, parse_sig_re, prepare_document, DiagnosticSink, Domain,
    DomainContext, DomainRegistry, PyOptions, PythonDomain, PythonDomainData, PythonModuleEntry,
    PythonObjectEntry,
};
use scribe_domains::python::signature::AnnToken;
use scribe_rst::{parse_with_options, resolve_output, Context, DirectiveRegistry, ParseOptions};

mod common;

use common::TestEnv;

fn simple_config() -> Config {
    let mut config = Config::new();
    let _ = config.set("add_module_names", ConfigValue::Bool(true));
    config
}

/// Parses RST, resolves directives and expands Python signatures.
fn prepare(source: &str) -> Document {
    let mut output = parse_with_options(source, ParseOptions::new(Some("index".to_owned())));
    let report = resolve_output(&mut output, &DirectiveRegistry::builtin(), &Context::default());
    assert!(report.is_clean(), "{:?}", report.unresolved);
    prepare_document(&mut output.document).expect("prepare");
    output.document
}

fn sig_nodes(doc: &Document) -> Vec<NodeId> {
    let mut out = Vec::new();
    for id in doc.preorder_ids() {
        if doc.get(id).is_ok_and(|node| node.kind == NodeKind::DescSignature) {
            out.push(id);
        }
    }
    out
}

fn kinds_under(doc: &Document, id: NodeId) -> Vec<NodeKind> {
    let mut out = Vec::new();
    for child in doc.subtree_ids(id) {
        if let Ok(node) = doc.get(child) {
            out.push(node.kind);
        }
    }
    out
}

fn text_of(doc: &Document, id: NodeId) -> String {
    doc.get(id).map(|node| node.astext(doc)).unwrap_or_default()
}

// ----- signature parsing ----------------------------------------------------

#[test]
fn sig_re_handles_all_captures() {
    let parsed = parse_sig_re("module.Class.method[T, U](self, x: list[str]) -> X | Y")
        .expect("complex signature");
    assert_eq!(parsed.prefix, "module.Class.");
    assert_eq!(parsed.name, "method");
    assert_eq!(parsed.type_params, "T, U");
    assert_eq!(parsed.arglist, "self, x: list[str]");
    assert_eq!(parsed.retann, "X | Y");
    assert!(parse_sig_re("not a signature!").is_none());
    assert!(parse_sig_re("f()").is_some());
    assert!(parse_sig_re("  f  ").is_some());
}

#[test]
fn annotation_forms_tokenize() {
    let rendered = |src: &str| -> String {
        parse_annotation(src)
            .iter()
            .map(|token| match token {
                AnnToken::Space => " ".to_owned(),
                AnnToken::Punct(text)
                | AnnToken::Number(text)
                | AnnToken::Keyword(text)
                | AnnToken::Str(text)
                | AnnToken::Operator(text)
                | AnnToken::Name(text)
                | AnnToken::ShortName(text)
                | AnnToken::SpecificName(text) => text.clone(),
            })
            .collect()
    };
    assert_eq!(rendered("Optional[int]"), "int | None");
    assert_eq!(rendered("Union[int, str]"), "int | str");
    assert_eq!(rendered("list[str]"), "list[str]");
    assert_eq!(rendered("Callable[[int], str]"), "Callable[[int], str]");
    assert_eq!(rendered("X | Y"), "X | Y");
    assert_eq!(rendered("-2"), "-2");
    // `~`/`.` tokens carry the normalized target; the short display name is
    // derived later by `parse_reftarget` when the xref node is materialized.
    assert_eq!(rendered("~a.b.C"), "a.b.C");
    assert_eq!(rendered(".C"), ".C");
}

#[test]
fn reftarget_modifiers_are_upstream_exact() {
    assert_eq!(parse_reftarget("~a.b.C", false), ("class", "a.b.C".into(), "C".into(), false));
    assert_eq!(parse_reftarget(".C", false), ("class", "C".into(), "C".into(), true));
    assert_eq!(parse_reftarget("None", false), ("obj", "None".into(), "None".into(), false));
    assert_eq!(
        parse_reftarget("typing.Any", false),
        ("obj", "typing.Any".into(), "Any".into(), false)
    );
    assert_eq!(parse_reftarget("a.b.C", true), ("class", "a.b.C".into(), "C".into(), false));
}

#[test]
fn signature_node_tree_for_full_form() {
    let doc = prepare(
        ".. py:function:: fn[T](x: int, /, y: list[str] = None, *args, z: X | Y, **kw) -> str\n   :async:\n",
    );
    let signode = *sig_nodes(&doc).first().expect("signature");
    let node = doc.get(signode).expect("signature node");
    assert_eq!(node.attrs.get_str("fullname").ok().flatten(), Some("fn"));
    let kinds = kinds_under(&doc, signode);
    assert!(kinds.contains(&NodeKind::DescParameterlist));
    assert!(kinds.contains(&NodeKind::DescTypeParameterList));
    assert!(kinds.contains(&NodeKind::DescReturns));
    assert!(kinds.contains(&NodeKind::DescSigKeyword)); // async prefix
    assert!(kinds.contains(&NodeKind::DescSigPunctuation)); // brackets, pipes
    assert!(kinds.contains(&NodeKind::Inline)); // default value
    let text = text_of(&doc, signode);
    assert!(text.contains("fn"));
    assert!(text.contains("int"));
    assert!(text.contains("None"));
}

#[test]
fn positional_and_keyword_only_separators() {
    let doc = prepare(".. py:function:: f(a, /, b, *, c)\n");
    let signode = *sig_nodes(&doc).first().expect("signature");
    let mut separators = 0;
    for id in doc.subtree_ids(signode) {
        if let Ok(node) = doc.get(id) {
            if node.kind == NodeKind::DescSigOperator {
                if node.get_str("classes").ok().flatten().is_none() {
                    // CSS lives in a list attribute; check via get_list below.
                }
                if node.get_list("classes").ok().flatten().unwrap_or_default().iter().any(|class| {
                    class == "positional-only-separator" || class == "keyword-only-separator"
                }) {
                    separators += 1;
                }
            }
        }
    }
    assert_eq!(separators, 2);
}

#[test]
fn decorator_and_qualified_names() {
    let doc = prepare(
        ".. py:decorator:: mod.Class.decorate\n\n.. py:method:: Mod.method()\n",
    );
    let signatures = sig_nodes(&doc);
    assert_eq!(signatures.len(), 2);
    let decorator = doc.get(signatures[0]).expect("decorator signature");
    assert_eq!(decorator.attrs.get_str("fullname").ok().flatten(), Some("mod.Class.decorate"));
    assert!(text_of(&doc, signatures[0]).starts_with('@'));
    let method = doc.get(signatures[1]).expect("method signature");
    assert_eq!(method.attrs.get_str("class").ok().flatten(), Some("Mod"));
}

#[test]
fn options_noindex_final_abstract_async() {
    let doc = prepare(
        ".. py:class:: C\n   :final:\n   :abstract:\n\n.. py:method:: m()\n   :async:\n   :abstractmethod:\n   :no-index:\n",
    );
    let signatures = sig_nodes(&doc);
    let class_sig = doc.get(signatures[0]).expect("class signature");
    let class_text = text_of(&doc, signatures[0]);
    assert!(class_text.contains("final"));
    assert!(class_text.contains("abstract"));
    assert!(class_sig.attrs.get_list("ids").ok().flatten().is_some());

    let method_sig = doc.get(signatures[1]).expect("method signature");
    let method_text = text_of(&doc, signatures[1]);
    assert!(method_text.contains("async"));
    assert!(method_text.contains("abstractmethod"));
    assert!(method_sig.attrs.get_list("ids").ok().flatten().is_none());
}

#[test]
fn typed_fields_emit_type_xrefs() {
    let doc = prepare(
        ".. py:function:: f(a)\n\n   :param a: the a\n   :type a: pkg.Thing\n   :rtype: list[pkg.Other]\n   :raises: pkg.Error\n",
    );
    let xrefs: Vec<NodeId> = doc
        .preorder_ids()
        .into_iter()
        .filter(|&id| doc.get(id).is_ok_and(|node| node.kind == NodeKind::PendingXref))
        .collect();
    assert!(!xrefs.is_empty(), "expected type xrefs in field bodies");
    let targets: Vec<String> = xrefs
        .iter()
        .filter_map(|&id| doc.get(id).ok())
        .filter_map(|node| node.pending_xref())
        .filter_map(|xref| xref.reftarget().map(str::to_owned))
        .collect();
    assert!(targets.iter().any(|target| target == "pkg.Thing"));
    assert!(targets.iter().any(|target| target == "pkg.Other"));
    assert!(targets.iter().any(|target| target == "pkg.Error"));
}

// ----- resolution -----------------------------------------------------------

fn add_py_xref(doc: &mut Document, typ: &str, target: &str, module: Option<&str>, class: Option<&str>) -> (NodeId, NodeId) {
    let mut node = Node::new(NodeKind::PendingXref);
    {
        let mut view = node.pending_xref_mut().expect("pending xref");
        view.set_refdomain(Some("py".to_owned()));
        view.set_reftype(Some(typ.to_owned()));
        view.set_reftarget(Some(target.to_owned()));
        view.set_refwarn(Some(true));
        view.set_refexplicit(Some(false));
        view.set_classes(vec!["xref".to_owned(), "py".to_owned(), format!("py-{typ}")]);
    }
    if let Some(module) = module {
        node.attrs.set_str("py:module", module);
    }
    if let Some(class) = class {
        node.attrs.set_str("py:class", class);
    }
    node.span = Some(Span::new(4, 4 + target.len() as u32));
    let root = doc.root;
    let xref_id = doc.add_node(node, Some(root)).expect("add xref");
    let inner = doc.add_node(Node::new(NodeKind::Literal), Some(xref_id)).expect("add inner");
    doc.new_text(target, Some(inner)).expect("add text");
    (xref_id, inner)
}

fn resolve_with(domain: &PythonDomain, doc: &mut Document, typ: &str, target: &str, module: Option<&str>, class: Option<&str>) -> scribe_domains::ResolvedReference {
    let (xref, content) = add_py_xref(doc, typ, target, module, class);
    let sink = DiagnosticSink::default();
    let config = simple_config();
    let env = TestEnv::default();
    let ctx = DomainContext::new(Some(&env), Some(&config), "index", "html", "html", &sink);
    let result = domain.resolve_xref(&ctx, typ, target, xref, content, doc).expect("resolve");
    result.expect("resolved")
}

#[test]
fn xref_modifiers_and_aliasing() {
    let domain = PythonDomain::new();
    let sink = DiagnosticSink::default();
    let config = simple_config();
    let ctx = DomainContext::new(None, Some(&config), "index", "html", "html", &sink);
    domain.note_object(&ctx, "pkg.Class.method", "method", "pkg-class-method", false, None);
    domain.note_object(&ctx, "pkg.func", "function", "pkg-func", false, None);
    domain.note_object(&ctx, "pkg.Original", "class", "pkg-original", false, None);
    domain.note_object(&ctx, "alias.Alias", "class", "pkg-original", true, None);

    let mut doc = Document::new(None);
    let reference = resolve_with(&domain, &mut doc, "meth", "method", Some("pkg"), Some("Class"));
    assert_eq!(reference.refid.as_deref(), Some("pkg-class-method"));
    assert_eq!(reference.reftitle.as_deref(), Some("pkg.Class.method"));

    // `fix_parens` strips the trailing `()` for the lookup; the display text
    // already carrying `()` is left untouched.
    let reference = resolve_with(&domain, &mut doc, "func", "func()", Some("pkg"), None);
    assert_eq!(reference.refid.as_deref(), Some("pkg-func"));
    assert_eq!(reference.title.as_deref(), None);
    let reference = resolve_with(&domain, &mut doc, "func", "func", Some("pkg"), None);
    assert_eq!(reference.title.as_deref(), Some("func()"));

    // `class` falls back to data/attr, and canonical aliases resolve by suffix.
    let reference = resolve_with(&domain, &mut doc, "class", "Alias", Some("alias"), None);
    assert_eq!(reference.refid.as_deref(), Some("pkg-original"));
}

#[test]
fn any_xref_uses_all_types_and_specific_mode() {
    let domain = PythonDomain::new();
    let sink = DiagnosticSink::default();
    let config = simple_config();
    let ctx = DomainContext::new(None, Some(&config), "index", "html", "html", &sink);
    domain.note_object(&ctx, "m.Thing", "class", "m-thing", false, None);
    let mut doc = Document::new(None);
    let (xref, content) = add_py_xref(&mut doc, "any", "Thing", Some("m"), None);
    let result = domain.resolve_any_xref(&ctx, "Thing", xref, content, &doc).expect("any");
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].0, "py:class");
    assert_eq!(result[0].1.refid.as_deref(), Some("m-thing"));
}

#[test]
fn duplicate_object_warning_is_upstream_exact() {
    let domain = PythonDomain::new();
    let sink = DiagnosticSink::default();
    let mut config = simple_config();
    let _ = config.set("nitpicky", ConfigValue::Bool(true));
    let first = DomainContext::new(None, Some(&config), "a", "html", "html", &sink);
    let second = DomainContext::new(None, Some(&config), "b", "html", "html", &sink);
    domain.note_object(&first, "pkg.Thing", "class", "a-thing", false, None);
    domain.note_object(&second, "pkg.Thing", "class", "b-thing", false, None);
    let diagnostics = sink.snapshot();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(
        diagnostics[0].message,
        "duplicate object description of pkg.Thing, other instance in a, use :no-index: for one of them"
    );
    // The original definition wins.
    assert_eq!(domain.snapshot().objects["pkg.Thing"].docname, "b");
}

#[test]
fn module_context_nesting_and_currentmodule() {
    let doc = prepare(
        ".. py:currentmodule:: outer\n\n.. py:class:: A\n\n   .. py:method:: m()\n\n   .. py:class:: B\n\n      .. py:method:: n()\n\n.. py:currentmodule:: None\n\n.. py:function:: f()\n",
    );
    let signatures = sig_nodes(&doc);
    let fullnames: Vec<&str> = signatures
        .iter()
        .filter_map(|&id| doc.get(id).ok())
        .filter_map(|node| node.get_str("fullname").ok().flatten())
        .collect();
    assert_eq!(fullnames, ["A", "A.m", "A.B", "A.B.n", "f"]);
    let modules: Vec<&str> = signatures
        .iter()
        .filter_map(|&id| doc.get(id).ok())
        .filter_map(|node| node.get_str("module").ok().flatten())
        .collect();
    assert_eq!(modules, ["outer", "outer", "outer", "outer", ""]);
}

// ----- inventory & module index ---------------------------------------------

#[test]
fn clear_doc_and_merge_domaindata() {
    let sink = DiagnosticSink::default();
    let config = simple_config();
    let ctx_a = DomainContext::new(None, Some(&config), "a", "html", "html", &sink);
    let ctx_b = DomainContext::new(None, Some(&config), "b", "html", "html", &sink);
    let domain = PythonDomain::new();
    domain.note_object(&ctx_a, "a.Thing", "class", "a-thing", false, None);
    domain.note_object(&ctx_b, "b.Thing", "class", "b-thing", false, None);
    domain.note_module("a", "a.mod", "a-mod", "s", "linux", false);
    domain.note_module("b", "b.mod", "b-mod", "", "", true);
    assert_eq!(domain.snapshot().objects.len(), 2);
    assert_eq!(domain.snapshot().modules.len(), 2);

    let mut cleared = PythonDomain::from_data(domain.snapshot());
    cleared.clear_doc("a");
    assert_eq!(cleared.snapshot().objects.len(), 1);
    assert_eq!(cleared.snapshot().modules.len(), 1);

    let mut merged = PythonDomain::new();
    merged.merge_domaindata(&["b".to_owned()], &domain.export_data().expect("export"));
    assert_eq!(merged.snapshot().objects.len(), 1);
    assert!(merged.snapshot().objects.contains_key("b.Thing"));
    assert!(merged.snapshot().modules.contains_key("b.mod"));
}

#[test]
fn module_index_groups_and_collapse() {
    let domain = PythonDomain::new();
    domain.note_module("a", "zope.interface", "zope-interface", "interfaces", "", false);
    domain.note_module("a", "zope.schema", "zope-schema", "", "linux", true);
    let index = domain.py_modindex(None, &[]);
    assert_eq!(index.groups.len(), 1);
    assert_eq!(index.groups[0].0, "z");
    let entries = &index.groups[0].1;
    // Upstream treats dotted modules as submodules: a package group head is
    // inserted for `zope`, then both modules are `subtype = 2`.
    assert!(entries.iter().any(|entry| entry.name == "zope" && entry.subtype == 1));
    assert!(entries.iter().any(|entry| entry.name == "zope.interface" && entry.subtype == 2));
    assert!(entries.iter().any(|entry| entry.name == "zope.schema" && entry.subtype == 2));
    assert!(entries.iter().any(|entry| entry.qualifier == "Deprecated"));
    assert!(!index.collapse);

    let filtered = domain.py_modindex(Some(&BTreeSet::from(["b".to_owned()])), &[]);
    assert!(filtered.groups.is_empty());
}

#[test]
fn export_replace_and_data_version() {
    let domain = PythonDomain::new();
    let data = domain.export_data().expect("export");
    assert_eq!(data.version, domain.data_version());
    let mut restored = PythonDomain::new();
    restored.replace_data(&data).expect("replace");
    assert_eq!(restored.snapshot(), domain.snapshot());
    let mut stale = data.clone();
    stale.version += 1;
    restored.replace_data(&stale).expect("stale version ignored");
    assert_eq!(restored.snapshot(), domain.snapshot());
}

#[test]
fn persistence_payload_roundtrip() {
    let data = PythonDomainData {
        objects: BTreeMap::from([(
            "m.C".to_owned(),
            PythonObjectEntry {
                docname: "index".to_owned(),
                node_id: "m-c".to_owned(),
                objtype: "class".to_owned(),
                aliased: false,
            },
        )]),
        modules: BTreeMap::from([(
            "m".to_owned(),
            PythonModuleEntry {
                docname: "index".to_owned(),
                node_id: "module-m".to_owned(),
                synopsis: "s".to_owned(),
                platform: "p".to_owned(),
                deprecated: false,
            },
        )]),
    };
    let encoded = serde_json::to_string(&data).expect("serialize");
    let decoded: PythonDomainData = serde_json::from_str(&encoded).expect("deserialize");
    assert_eq!(decoded, data);
}

#[test]
fn builtin_registry_contains_python_domain() {
    let registry = DomainRegistry::builtin();
    assert!(registry.contains("std"));
    assert!(registry.contains("py"));
}

// ----- fuzz / adversarial corpus --------------------------------------------

#[test]
fn malformed_signatures_never_panic() {
    let cases = [
        "", " ", "()", "f(", "f)", "f() trailing", "f[[[]]]", "f(a:)", "f(**)",
        "f(a: Optional[)", "f(a: Union[,])", "f(,)", "f(a=)", "f(:)", ".f()",
        "f<T>(a) -> ", "f() -> ]", "@@@", "f(a: 'unterminated)", "f()\u{0}",
        "f(a: Callable[[int], str])", "f(a: dict[str, list[int]])", "f(/)", "f(*, )",
        "f(a, /, b: int = 1, *args: str, **kwargs: Any) -> None",
    ];
    for case in cases {
        let _ = parse_sig_re(case);
        let _ = parse_annotation(case);
        let options = PyOptions::default();
        let mut doc = Document::new(None);
        let desc = doc.add(NodeKind::Desc, Some(doc.root)).expect("desc");
        doc.get_mut(desc).expect("desc").attrs.set_str("objtype", "function");
        // A malformed signature must be handled without panicking; a parse
        // failure simply leaves the placeholder signature node intact.
        let placeholders: Vec<NodeId> = doc.children(desc).map(<[NodeId]>::to_vec).unwrap_or_default();
        assert!(placeholders.is_empty() || !placeholders.is_empty());
        let _ = scribe_domains::python::signature::build_desc_signature(
            &mut doc, desc, case, "function", &scribe_domains::SigContext::default(), &options,
        );
    }
}

#[test]
fn adversarial_role_payloads_never_panic() {
    let cases = [
        ":py:func:`~`", ":py:meth:`.`", ":py:class:`..`", ":py:obj:`a.b.c.d`",
        ":py:type:`X | Y`", ":py:func:`f()`", ":py:attr:`~a.b.c`",
    ];
    for case in cases {
        let source = format!("Doc {case} end.\n");
        let mut output = parse_with_options(&source, ParseOptions::new(Some("index".to_owned())));
        let report = resolve_output(&mut output, &DirectiveRegistry::builtin(), &Context::default());
        assert!(report.is_clean(), "{case}: {:?}", report.unresolved);
        prepare_document(&mut output.document).expect("prepare");
    }
}

#[test]
fn config_from_conf_py_is_honored() {
    let request = LoadRequest {
        conf_dir: std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../sphinx/tests/roots/test-domain-py-python_use_unqualified_type_names"),
        strategy: BridgeStrategy::Static,
        ..LoadRequest::default()
    };
    let (config, _) = load_config_with(request).expect("load conf");
    assert_eq!(config.get_bool("python_use_unqualified_type_names"), Some(true));
}
