//! Phase 06 directive registry, option specs and resolve-pass tests.

mod common;

use std::sync::Arc;

use scribe_ast::{NodeId, NodeKind};
use scribe_config::Tags;
use scribe_rst::{
    parse, parse_with_options, resolve_output, resolve_pending, Context, DirectiveRegistry,
    DirectiveError, FsIncludeResolver, IncludeResolver, OptionError, OptionKind, OptionSpec,
    ParseOptions, ResolveReport,
};

fn resolve(source: &str) -> (scribe_rst::ParseOutput, ResolveReport, Context) {
    let mut output = parse(source);
    let registry = DirectiveRegistry::builtin();
    let context = Context {
        source_text: Some(Arc::new(source.to_owned())),
        ..Context::default()
    };
    let report = resolve_output(&mut output, &registry, &context);
    (output, report, context)
}

fn pseudo(source: &str) -> String {
    let (output, report, _) = resolve(source);
    assert!(report.is_clean(), "unexpected issues: {:?}", report.unresolved);
    output.document.to_pseudoxml().expect("pseudo-XML")
}

#[test]
fn builtin_registry_covers_all_core_directives() {
    let registry = DirectiveRegistry::builtin();
    let expected = [
        "admonition", "note", "warning", "tip", "important", "caution", "danger", "error",
        "hint", "attention", "seealso", "versionadded", "versionchanged", "deprecated",
        "rubric", "epigraph", "highlights", "pull-quote", "compound", "container", "topic",
        "sidebar", "contents", "sectnum", "include", "raw", "replace", "unicode", "date",
        "class", "role", "default-role", "title", "meta", "math", "csv-table", "list-table",
        "image", "figure", "only", "tabularcolumns", "centered", "acks", "hlist", "glossary",
        "productionlist", "footnote", "citation", "code", "code-block", "sourcecode",
        "literalinclude",
    ];
    for name in expected {
        assert!(registry.contains(name), "missing directive: {name}");
    }
    assert!(registry.len() >= expected.len());
    // Lookup is case-insensitive and underscore-tolerant.
    assert!(registry.contains("NOTE"));
    assert!(registry.contains("pull_quote"));
}

#[test]
fn title_directive_sets_document_metadata_without_a_node() {
    let (output, report, _) = resolve(
        ".. title:: Set by directive\n\nSection Title\n=============\n\nBody.\n",
    );
    assert!(report.is_clean(), "unexpected issues: {:?}", report.unresolved);
    assert_eq!(output.document.metadata.title.as_deref(), Some("Set by directive"));
    let pseudo = output.document.to_pseudoxml().expect("pseudoxml");
    assert!(pseudo.contains("title=\"Set by directive\""), "{pseudo}");
    assert!(!pseudo.contains("<title>\n            Set by directive"), "{pseudo}");
    // The real section title is still rendered as the document's heading.
    assert!(pseudo.contains("Section Title"), "{pseudo}");
}

#[test]
fn admonitions_produce_upstream_nodes_and_attributes() {
    let out = pseudo(
        ".. note::\n   :class: special\n   :name: my note\n\n   Body with *em*.\n",
    );
    assert!(out.contains("<note classes=\"special\" ids=\"my-note\" names=\"my\\ note\">"), "{out}");
    assert!(out.contains("<emphasis>"), "{out}");

    let out = pseudo(".. admonition:: Custom Title\n\n   Body.\n");
    assert!(out.contains("<admonition classes=\"admonition-custom-title\">"), "{out}");
    assert!(out.contains("<title>"), "{out}");
    assert!(out.contains("Custom Title"), "{out}");

    let out = pseudo(".. seealso::\n\n   More.\n");
    assert!(out.contains("<seealso>"), "{out}");
}

#[test]
fn version_modified_directive_matches_upstream_shape() {
    let out = pseudo(".. versionadded:: 2.0 The feature\n\n   Details.\n");
    assert!(out.contains("<versionmodified type=\"versionadded\" version=\"2.0\">"), "{out}");
    assert!(out.contains("<inline classes=\"versionmodified added\">"), "{out}");
    assert!(out.contains("Added in version 2.0: "), "{out}");
    assert!(out.contains("The feature"), "{out}");
    assert!(out.contains("Details."), "{out}");

    let out = pseudo(".. deprecated:: 1.0\n");
    assert!(out.contains("Deprecated since version 1.0."), "{out}");
}

#[test]
fn body_containers_and_pseudo_sections() {
    let out = pseudo(
        ".. rubric:: A Rubric\n   :heading-level: 2\n\n.. epigraph::\n\n   Quote.\n\n.. compound::\n\n   Compound body.\n\n.. container:: custom\n\n   Container body.\n",
    );
    assert!(out.contains("<rubric heading-level=\"2\">"), "{out}");
    assert!(out.contains("<block_quote classes=\"epigraph\">"), "{out}");
    assert!(out.contains("<compound>"), "{out}");
    assert!(out.contains("<container classes=\"custom\">"), "{out}");

    let out = pseudo(".. topic:: My Topic\n\n   Topic body.\n\n.. sidebar:: Side\n   :subtitle: Sub\n\n   Side body.\n");
    assert!(out.contains("<topic>"), "{out}");
    assert!(out.contains("<title>"), "{out}");
    assert!(out.contains("<sidebar>"), "{out}");
    assert!(out.contains("<subtitle>"), "{out}");
    assert!(out.contains("Sub"), "{out}");
}

#[test]
fn centered_acks_hlist_glossary_productionlist_tabularcolumns() {
    let out = pseudo(
        ".. centered:: **Bold center**\n\n.. tabularcolumns:: |l|c|\n\n.. acks::\n\n   * A\n   * B\n\n.. hlist::\n   :columns: 2\n\n   * one\n   * two\n   * three\n\n.. glossary::\n   :sorted:\n\n   term\n      Definition.\n\n.. productionlist::\n   group\n   name: tokens\n",
    );
    assert!(out.contains("<centered>"), "{out}");
    assert!(out.contains("<strong>"), "{out}");
    assert!(out.contains("<tabular_col_spec spec=\"|l|c|\">"), "{out}");
    assert!(out.contains("<acks>"), "{out}");
    assert!(out.contains("<hlist ncolumns=\"2\">"), "{out}");
    assert_eq!(out.matches("<hlistcol>").count(), 2, "{out}");
    assert!(out.contains("<glossary sorted=\"1\">"), "{out}");
    assert!(out.contains("<productionlist>"), "{out}");
    assert!(out.contains("tokenname=\"name\""), "{out}");
    assert!(out.contains(" ::= "), "{out}");
    assert!(out.contains("tokens"), "{out}");
}

#[test]
fn productionlist_emits_group_qualified_token_xrefs() {
    let (output, report, _) = resolve(".. productionlist:: P1\n   A: `A` | `~P1:B` | plain\n   B: `B`\n");
    assert!(report.is_clean(), "{:?}", report.unresolved);
    let xrefs: Vec<_> = output
        .document
        .preorder_ids()
        .into_iter()
        .filter_map(|id| output.document.get(id).ok())
        .filter(|node| node.kind == NodeKind::PendingXref)
        .collect();
    assert_eq!(xrefs.len(), 3, "expected one pending_xref per backticked token");
    let targets: Vec<_> = xrefs
        .iter()
        .map(|node| node.get_str("reftarget").ok().flatten().unwrap_or(""))
        .collect();
    assert_eq!(targets, ["P1:A", "P1:B", "P1:B"]);
    assert!(xrefs.iter().all(|node| node.get_str("reftype").ok().flatten() == Some("token")));
    assert!(xrefs.iter().all(|node| node.get_str("refdomain").ok().flatten() == Some("std")));
    let pseudo = output.document.to_pseudoxml().expect("pseudo");
    assert!(pseudo.contains("<productionlist>"), "{pseudo}");
    assert_eq!(pseudo.matches("<production ").count(), 2, "{pseudo}");
}

#[test]
fn include_with_line_ranges_and_literal_mode() {
    let root = std::env::temp_dir().join(format!("scribe-include-test-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("create temp dir");
    let file = root.join("inc.txt");
    std::fs::write(&file, "one\ntwo\nthree\nfour\nfive\n").expect("write include file");
    let source = ".. include:: inc.txt\n   :start-line: 1\n   :end-line: 3\n   :literal:\n";
    let mut output = parse_with_options(
        source,
        ParseOptions {
            source_name: Some("index".into()),
            tab_width: 8,
            prolog: None,
            epilog: None,
            default_role: None,
            default_domain: None,
            extlinks: scribe_rst::extlinks::empty_roles(),
        },
    );
    let context = Context::new(&root, Tags::new());
    let report = resolve_output(
        &mut output,
        &DirectiveRegistry::builtin(),
        &context,
    );
    assert!(report.is_clean(), "{:?}", report.unresolved);
    let pseudo = output.document.to_pseudoxml().expect("pseudo");
    assert!(pseudo.contains("<literal_block"), "{pseudo}");
    assert!(pseudo.contains("two"), "{pseudo}");
    assert!(pseudo.contains("three"), "{pseudo}");
    assert!(!pseudo.contains("one"), "{pseudo}");
    assert!(!pseudo.contains("four"), "{pseudo}");

    // start-after / end-before operate on text, not line numbers.
    let source = ".. include:: inc.txt\n   :start-after: two\n   :end-before: five\n   :literal:\n";
    let mut output = parse(source);
    let report = resolve_output(
        &mut output,
        &DirectiveRegistry::builtin(),
        &context,
    );
    assert!(report.is_clean(), "{:?}", report.unresolved);
    let pseudo = output.document.to_pseudoxml().expect("pseudo");
    assert!(pseudo.contains("three"), "{pseudo}");
    assert!(pseudo.contains("four"), "{pseudo}");
    assert!(!pseudo.contains("two"), "{pseudo}");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn include_without_literal_parses_nested_blocks() {
    let root = std::env::temp_dir().join(format!("scribe-include-blocks-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("create temp dir");
    std::fs::write(root.join("inc.rst"), "Included\n========\n\nText.\n").expect("write file");
    let mut output = parse(".. include:: inc.rst\n");
    let report = resolve_output(&mut output, &DirectiveRegistry::builtin(), &Context::new(&root, Tags::new()));
    assert!(report.is_clean(), "{:?}", report.unresolved);
    let pseudo = output.document.to_pseudoxml().expect("pseudo");
    assert!(pseudo.contains("<section ids=\"included\" names=\"included\">"), "{pseudo}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn include_error_is_recoverable_and_reported() {
    let (_, report, _) = resolve(".. include:: definitely-missing-file.rst\n");
    assert_eq!(report.issue_count(), 1);
    assert_eq!(report.unresolved[0].code, "scribe::rst::include_io");
    assert_eq!(report.unresolved[0].line, 1);
    assert!(!report.diagnostics().is_empty());
}

#[test]
fn only_respects_tags() {
    let source = "Before.\n\n.. only:: draft\n\n   Hidden.\n\nAfter.\n";

    let mut output = parse(source);
    let no_tags = Context::default();
    let report = resolve_output(&mut output, &DirectiveRegistry::builtin(), &no_tags);
    assert!(report.is_clean(), "{:?}", report.unresolved);
    let pseudo = output.document.to_pseudoxml().expect("pseudo");
    assert!(!pseudo.contains("Hidden."), "{pseudo}");

    let mut tags = Tags::new();
    tags.add("draft");
    let mut output = parse(source);
    let with_tags = Context::new(std::env::temp_dir(), tags);
    let report = resolve_output(&mut output, &DirectiveRegistry::builtin(), &with_tags);
    assert!(report.is_clean(), "{:?}", report.unresolved);
    let pseudo = output.document.to_pseudoxml().expect("pseudo");
    assert!(pseudo.contains("Hidden."), "{pseudo}");
    assert!(!pseudo.contains("<only"), "{pseudo}");
}

#[test]
fn nested_directives_are_resolved_recursively() {
    let source = ".. note::\n\n   Outer.\n\n   .. warning::\n\n      Inner with ``code``.\n";
    let (output, report, _) = resolve(source);
    assert!(report.is_clean(), "{:?}", report.unresolved);
    assert!(report.resolved >= 2, "{report:?}");
    let pseudo = output.document.to_pseudoxml().expect("pseudo");
    assert!(pseudo.contains("<note>"), "{pseudo}");
    assert!(pseudo.contains("<warning>"), "{pseudo}");
    assert!(pseudo.contains("<literal>"), "{pseudo}");
    assert!(!pseudo.contains("<pending"), "{pseudo}");
}

#[test]
fn unknown_directive_keeps_document_usable_and_reports_span() {
    let source = "Title\n=====\n\n.. made-up-directive:: arg\n\nAfter.\n";
    let mut output = parse(source);
    let context = Context {
        source_text: Some(Arc::new(source.to_owned())),
        source_name: Some("index".into()),
        ..Context::default()
    };
    let report = resolve_pending(&mut output.document, &DirectiveRegistry::builtin(), &context);
    assert_eq!(report.issue_count(), 1);
    let issue = &report.unresolved[0];
    assert_eq!(issue.name, "made-up-directive");
    assert_eq!(issue.line, 4);
    assert_eq!(issue.code, "scribe::rst::unknown_directive");
    assert_eq!(issue.span.offset(), source.find(".. made-up").expect("marker offset"));
    let rendered = format!("{:?}", report.diagnostics()[0]);
    assert!(rendered.contains("unknown directive"), "{rendered}");
    let pseudo = output.document.to_pseudoxml().expect("pseudo");
    assert!(pseudo.contains("<pending"), "{pseudo}");
    assert!(pseudo.contains("After."), "{pseudo}");
}

#[test]
fn invalid_options_are_diagnosed() {
    let (output, report, _) = resolve(".. note::\n   :depth: nope\n\n   Body.\n");
    assert_eq!(report.issue_count(), 1);
    assert_eq!(report.unresolved[0].code, "scribe::rst::unknown_option");
    assert!(output.document.to_pseudoxml().expect("pseudo").contains("<pending"));

    let err = resolve(".. hlist::\n   :columns: -1\n\n   * a\n").1;
    // `int` accepts negative values; the handler clamps to 1.
    assert!(err.is_clean(), "{:?}", err.unresolved);
}

#[test]
fn substitution_definitions_resolve_replace_unicode_and_image() {
    let source = "Use |x| and |copy| and |pic|.\n\n.. |x| replace:: **R**\n\n.. |copy| unicode:: U+00A9\n\n.. |pic| image:: logo.png\n   :width: 20\n";
    let (output, report, _) = resolve(source);
    assert!(report.is_clean(), "{:?}", report.unresolved);
    let pseudo = output.document.to_pseudoxml().expect("pseudo");
    assert!(pseudo.contains("<strong>"), "{pseudo}");
    assert!(pseudo.contains("©"), "{pseudo}");
    assert!(pseudo.contains("<image"), "{pseudo}");
    assert!(pseudo.contains("uri=\"logo.png\""), "{pseudo}");
    assert!(!pseudo.contains("<substitution_reference"), "{pseudo}");
}

#[test]
fn footnote_and_citation_definitions_resolve() {
    let source = "Text [1]_ and [CITE]_.\n\n.. [1] Footnote body.\n\n.. [CITE] Citation body.\n";
    let (output, report, _) = resolve(source);
    assert!(report.is_clean(), "{:?}", report.unresolved);
    assert!(report.resolved >= 2, "{report:?}");
    let pseudo = output.document.to_pseudoxml().expect("pseudo");
    assert!(pseudo.contains("<footnote"), "{pseudo}");
    assert!(pseudo.contains("<citation"), "{pseudo}");
    assert!(!pseudo.contains("directive_name=\"footnote\""), "{pseudo}");
}

#[test]
fn csv_and_list_tables_build_table_nodes() {
    let csv = ".. csv-table:: My CSV\n   :header: \"A\", \"B\"\n   :widths: 10, 20\n\n   1, 2\n   3, 4\n";
    let csv_pseudo = pseudo(csv);
    assert!(csv_pseudo.contains("<table"), "{csv_pseudo}");
    assert!(csv_pseudo.contains("<title>"), "{csv_pseudo}");
    assert!(csv_pseudo.contains("My CSV"), "{csv_pseudo}");
    assert!(csv_pseudo.contains("<tgroup cols=\"2\">"), "{csv_pseudo}");
    assert!(csv_pseudo.contains("colwidth=\"10\""), "{csv_pseudo}");
    assert!(csv_pseudo.contains("<thead>"), "{csv_pseudo}");
    assert!(csv_pseudo.contains("<tbody>"), "{csv_pseudo}");
    assert_eq!(csv_pseudo.matches("<entry>").count(), 6, "{csv_pseudo}");

    let list = ".. list-table:: My LT\n   :header-rows: 1\n\n   * - A\n     - B\n   * - 1\n     - 2\n";
    let list_pseudo = pseudo(list);
    assert!(list_pseudo.contains("<table"), "{list_pseudo}");
    assert!(list_pseudo.contains("<thead>"), "{list_pseudo}");
    assert_eq!(list_pseudo.matches("<entry>").count(), 4, "{list_pseudo}");
}

#[test]
fn image_figure_raw_math_and_code_placeholders() {
    let out = pseudo(
        ".. image:: pic.png\n   :width: 100px\n   :align: center\n   :alt: A pic\n\n.. figure:: fig.png\n   :figwidth: 50%\n\n   Caption.\n\n.. math::\n   :label: eq1\n\n   x^2\n\n.. code-block:: rust\n\n   let x = 1;\n",
    );
    assert!(out.contains("uri=\"pic.png\""), "{out}");
    assert!(out.contains("width=\"100px\""), "{out}");
    assert!(out.contains("<figure"), "{out}");
    assert!(out.contains("<caption>"), "{out}");
    assert!(out.contains("<math_block"), "{out}");
    assert!(out.contains("label=\"eq1\""), "{out}");
    assert!(out.contains("<literal_block"), "{out}");
    assert!(out.contains("language=\"rust\""), "{out}");

    let out = pseudo(".. raw:: html\n\n   <b>hi</b>\n");
    assert!(out.contains("<raw format=\"html\" xml:space=\"preserve\">"), "{out}");
}

#[test]
fn contents_and_sectnum_leave_transform_placeholders() {
    let out = pseudo(".. contents:: Table of Contents\n   :depth: 2\n\n.. sectnum::\n   :start: 3\n");
    assert!(out.contains("<topic"), "{out}");
    assert!(out.contains("classes=\"contents\""), "{out}");
    assert!(out.contains("__details="), "{out}");
}

#[test]
fn option_spec_conversions_and_errors() {
    let spec = OptionSpec::empty()
        .with("flag", OptionKind::Flag)
        .with("count", OptionKind::PositiveInt)
        .with("choice", OptionKind::Choice(vec!["a".into(), "b".into()]))
        .with("path", OptionKind::Path)
        .with("class", OptionKind::ClassOption);
    let raw = std::collections::BTreeMap::from([
        ("flag".to_owned(), String::new()),
        ("count".to_owned(), "3".to_owned()),
        ("choice".to_owned(), "b".to_owned()),
        ("path".to_owned(), "a/b".to_owned()),
        ("class".to_owned(), "one two".to_owned()),
    ]);
    let parsed = spec.convert(&raw, (0usize, 1).into()).expect("valid options");
    assert!(parsed["flag"].is_flag());
    assert_eq!(parsed["count"].as_i64(), Some(3));
    assert_eq!(parsed["class"].as_list().unwrap(), &["one", "two"]);

    let bad = std::collections::BTreeMap::from([("count".to_owned(), "0".to_owned())]);
    assert!(matches!(
        spec.convert(&bad, (0usize, 1).into()),
        Err(OptionError::InvalidValue { .. })
    ));
    let unknown = std::collections::BTreeMap::from([("nope".to_owned(), "x".to_owned())]);
    assert!(matches!(
        spec.convert(&unknown, (0usize, 1).into()),
        Err(OptionError::UnknownOption { .. })
    ));
}

#[test]
fn custom_directive_can_be_registered() {
    use scribe_rst::{Directive, DirectiveContext, DirectiveError};

    struct Hello;
    impl Directive for Hello {
        fn name(&self) -> &'static str {
            "hello"
        }
        fn run(&self, ctx: &mut DirectiveContext<'_>) -> Result<Vec<NodeId>, DirectiveError> {
            let id = ctx.add(NodeKind::Paragraph, ctx.parent)?;
            ctx.add_text("hello from extension", id, ctx.line)?;
            Ok(vec![id])
        }
    }
    let registry = DirectiveRegistry::new();
    registry.register(Hello);
    let mut output = parse(".. hello::\n");
    let report = resolve_pending(&mut output.document, &registry, &Context::default());
    assert!(report.is_clean(), "{:?}", report.unresolved);
    let pseudo = output.document.to_pseudoxml().expect("pseudo");
    assert!(pseudo.contains("hello from extension"), "{pseudo}");
}

#[test]
fn role_and_default_role_directives_update_context_seam() {
    let source = ".. role:: shiny(code)\n\n.. default-role:: literal\n";
    let (_, _, context) = resolve(source);
    assert_eq!(
        context.custom_roles.read().expect("lock").get("shiny").map(String::as_str),
        Some("code")
    );
    assert_eq!(
        context.default_role.read().expect("lock").as_deref(),
        Some("literal")
    );
}

#[test]
fn include_resolver_trait_is_injectable() {
    #[derive(Debug)]
    struct Memory;
    impl IncludeResolver for Memory {
        fn resolve(&self, _: Option<&std::path::Path>, _: Option<&str>, path: &str) -> Result<String, DirectiveError> {
            Ok(format!("virtual: {path}"))
        }
    }
    let err = FsIncludeResolver.resolve(None, None, "/missing");
    assert!(matches!(err, Err(DirectiveError::IncludeIo { .. })));
    let ok = Memory.resolve(None, None, "x.txt").expect("memory resolver");
    assert_eq!(ok, "virtual: x.txt");
}

#[test]
fn prolog_directives_and_roles_resolve() {
    let mut output = parse_with_options(
        "Use |x|.\n",
        ParseOptions {
            source_name: None,
            tab_width: 8,
            prolog: Some(".. |x| replace:: .. note::\n\n   In prolog.".to_owned()),
            epilog: None,
            default_role: None,
            default_domain: None,
            extlinks: scribe_rst::extlinks::empty_roles(),
        },
    );
    let report = resolve_output(&mut output, &DirectiveRegistry::builtin(), &Context::default());
    assert!(report.issue_count() <= 1, "{:?}", report.unresolved);
    let pseudo = output.document.to_pseudoxml().expect("pseudo");
    assert!(pseudo.contains("<substitution_definition"), "{pseudo}");
}
