//! Phase 20: native built-in extension unit/integration tests.
//!
//! Every test here constructs configuration and doctrees in pure Rust — no
//! Python interpreter is required for the native implementations.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use scribe_config::{Config, ConfigRegistry, ConfigValue};
use scribe_core::extensions::graphviz::GraphvizRender;
use scribe_core::extensions::{ifconfig, viewcode, EXT_GRAPHVIZ, EXT_MARKER};
use scribe_core::{
    register_config_keys, register_directives, ActivatedExtensions, DurationReport,
    GithubPagesFiles, TodoCollector, ViewcodeCollector,
};
use scribe_rst::{parse_with_options, resolve_output, Context, DirectiveRegistry, ParseOptions};

fn roots() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../sphinx/tests/roots")
}

fn config_with(extensions: &[&str]) -> Config {
    let mut config = Config::new();
    config
        .set(
            "extensions",
            ConfigValue::List(extensions.iter().map(|name| ConfigValue::from(*name)).collect()),
        )
        .expect("known key");
    config
}

fn parse(source: &str, config: &Config, source_dir: &std::path::Path) -> scribe_ast::Document {
    let mut output = parse_with_options(source, ParseOptions::new(Some("index".to_owned())));
    let mut registry = DirectiveRegistry::builtin();
    register_directives(&mut registry, &ActivatedExtensions::from_config(config));
    let context = Context::new(source_dir, scribe_config::Tags::new());
    let report = resolve_output(&mut output, &registry, &context);
    assert!(report.issue_count() == 0, "unresolved: {:?}", report.unresolved);
    output.document
}

// ----- activation / config -------------------------------------------------

#[test]
fn activation_activates_native_extensions_and_warns_on_unknown() {
    let config = config_with(&[
        "sphinx.ext.todo",
        "sphinx.ext.ifconfig",
        "sphinx.ext.extlinks",
        "sphinx.ext.viewcode",
        "sphinx.ext.graphviz",
        "sphinx.ext.githubpages",
        "sphinx.ext.duration",
        "sphinx.ext.unknown",
    ]);
    let active = ActivatedExtensions::from_config(&config);
    assert_eq!(active.names(), vec![
        "duration", "extlinks", "githubpages", "graphviz", "ifconfig", "todo", "viewcode",
    ]);
    assert_eq!(active.warnings, vec!["extension sphinx.ext.unknown not found".to_owned()]);
}

#[test]
fn config_registry_declares_all_phase20_keys() {
    let mut registry = ConfigRegistry::builtin();
    register_config_keys(&mut registry).expect("idempotent registration");
    for key in [
        "todo_include_todos", "todo_link_only", "todo_emit_warnings",
        "extlinks", "extlinks_detect_hardcoded_links", "viewcode_line_numbers",
        "graphviz_dot", "graphviz_dot_args", "graphviz_output_format",
        "duration_print_total", "duration_print_slowest", "duration_n_slowest",
    ] {
        assert!(registry.contains(key), "missing {key}");
    }
}

#[test]
fn native_activation_needs_no_python_bridge() {
    // A hand-built Config is enough: activation reads only typed config values.
    let config = config_with(&["sphinx.ext.todo"]);
    let active = ActivatedExtensions::from_config(&config);
    assert!(active.contains("todo"));
    assert!(active.warnings.is_empty());
}

// ----- ifconfig ------------------------------------------------------------

#[test]
fn ifconfig_evaluator_handles_documented_grammar() {
    let mut config = Config::new();
    config.set("project", ConfigValue::from("Scribe")).unwrap();
    config.set("release", ConfigValue::from("1.0")).unwrap();
    config.set("version", ConfigValue::Int(2)).unwrap();

    assert!(ifconfig::eval("project == 'Scribe'", &config, "html").unwrap());
    assert!(!ifconfig::eval("project and not release", &config, "html").unwrap());
    assert!(ifconfig::eval("releaselevel in ('alpha', 'beta', 'rc')", &config, "html").is_err());
    assert!(ifconfig::eval("builder == 'html' and (version >= 2 or project == 'Scribe')", &config, "html").unwrap());
    assert!(ifconfig::eval("1 == 1.0", &config, "html").unwrap());
    assert!(ifconfig::eval("'a' != 'b'", &config, "html").unwrap());
    assert!(!ifconfig::eval("None", &config, "html").unwrap());
    assert!(ifconfig::eval("not False", &config, "html").unwrap());
}

#[test]
fn ifconfig_never_panics_on_malformed_expressions() {
    let config = Config::new();
    for expression in [
        "", "(", ")", "==", "and", "or", "not not not", "'unterminated",
        "project == ", "1 + 2", "@", "(((((((((", "a[0]", "{}", "project ==",
    ] {
        let _ = ifconfig::eval(expression, &config, "html");
    }
}

#[test]
fn ifconfig_directive_keeps_true_branches_and_drops_false() {
    let mut registry = ConfigRegistry::builtin();
    for name in ["confval1", "confval2", "false_config"] {
        registry.register(scribe_config::ConfigEntry {
            name,
            default: scribe_config::ConfigDefault::Value(ConfigValue::Bool(false)),
            value_type: scribe_config::ConfigType::Bool,
            rebuild: "",
            description: "ifconfig test namespace",
        }).unwrap();
    }
    let mut config = Config::with_registry(registry);
    config.set("extensions", ConfigValue::List(vec![ConfigValue::from("sphinx.ext.ifconfig")])).unwrap();
    config.set("confval1", ConfigValue::Bool(true)).unwrap();
    config.set("confval2", ConfigValue::Bool(false)).unwrap();
    let mut doc = parse(
        "Title\n=====\n\n.. ifconfig:: confval1\n\n   kept\n\n.. ifconfig:: confval2\n\n   dropped\n",
        &config,
        std::path::Path::new("."),
    );
    let warnings = ifconfig::process(&mut doc, &config, "html");
    assert!(warnings.is_empty(), "{warnings:?}");
    let pseudo = doc.to_pseudoxml().unwrap();
    assert!(pseudo.contains("kept"));
    assert!(!pseudo.contains("dropped"));
    assert!(!pseudo.contains("ifconfig"));
}

#[test]
fn ifconfig_invalid_expression_lowers_to_system_message() {
    let config = config_with(&["sphinx.ext.ifconfig"]);
    let mut doc = parse(
        "Title\n=====\n\n.. ifconfig:: undefined_name\n\n   body\n",
        &config,
        std::path::Path::new("."),
    );
    let warnings = ifconfig::process(&mut doc, &config, "html");
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("NameError"));
    let pseudo = doc.to_pseudoxml().unwrap();
    assert!(pseudo.contains("system_message"));
}

// ----- extlinks ------------------------------------------------------------

#[test]
fn extlinks_parse_and_expand_with_multiple_replacements() {
    let mut config = Config::new();
    let mut entries = BTreeMap::new();
    entries.insert(
        "issue".to_owned(),
        ConfigValue::List(vec![
            ConfigValue::from("https://example/%s/%s.html"),
            ConfigValue::from("issue %s / %s"),
        ]),
    );
    config.set("extlinks", ConfigValue::Dict(entries)).unwrap();
    let (specs, warnings) = scribe_core::extensions::extlink_roles(&config);
    assert!(warnings.is_empty(), "{warnings:?}");
    let (uri, title) = specs["issue"].resolve("a");
    assert_eq!(uri, "https://example/a/a.html");
    assert_eq!(title, "issue a / a");
}

#[test]
fn extlinks_malformed_entries_are_diagnostics_not_panics() {
    let mut config = Config::new();
    let mut entries = BTreeMap::new();
    entries.insert("bad".to_owned(), ConfigValue::Int(7));
    entries.insert("fmt".to_owned(), ConfigValue::List(vec![
        ConfigValue::from("https://x/%d"),
        ConfigValue::None,
    ]));
    config.set("extlinks", ConfigValue::Dict(entries)).unwrap();
    let (_, warnings) = scribe_core::extensions::extlink_roles(&config);
    assert_eq!(warnings.len(), 2);
    assert!(warnings.iter().any(|warning| warning.contains("pair")));
    assert!(warnings.iter().any(|warning| warning.contains("unsupported")));
}

#[test]
fn extlinks_role_produces_external_reference() {
    let mut config = Config::new();
    config.set("extensions", ConfigValue::List(vec![ConfigValue::from("sphinx.ext.extlinks")])).unwrap();
    let mut entries = BTreeMap::new();
    entries.insert("issue".to_owned(), ConfigValue::List(vec![
        ConfigValue::from("https://example/%s.html"),
        ConfigValue::from("issue %s"),
    ]));
    config.set("extlinks", ConfigValue::Dict(entries)).unwrap();
    let (roles, _) = scribe_core::extensions::extlink_roles(&config);

    let mut output = parse_with_options(
        "Use :issue:`42`.\n",
        ParseOptions { extlinks: roles, ..ParseOptions::default() },
    );
    let context = Context::new(".", scribe_config::Tags::new());
    let _ = resolve_output(&mut output, &DirectiveRegistry::builtin(), &context);
    let pseudo = output.document.to_pseudoxml().unwrap();
    assert!(pseudo.contains("https://example/42.html"), "{pseudo}");
    assert!(pseudo.contains("issue 42"), "{pseudo}");
    assert!(pseudo.contains("extlink-issue"), "{pseudo}");
}

#[test]
fn hardcoded_link_warnings_match_upstream_shapes() {
    let mut config = Config::new();
    config.set("extensions", ConfigValue::List(vec![ConfigValue::from("sphinx.ext.extlinks")])).unwrap();
    let mut entries = BTreeMap::new();
    entries.insert("issue".to_owned(), ConfigValue::List(vec![
        ConfigValue::from("https://x/%s.html"), ConfigValue::from("issue %s"),
    ]));
    config.set("extlinks", ConfigValue::Dict(entries)).unwrap();
    config.set("extlinks_detect_hardcoded_links", ConfigValue::Bool(true)).unwrap();
    let doc = parse("<https://x/7.html>\n", &config, std::path::Path::new("."));
    let mut docs = BTreeMap::new();
    docs.insert("index".to_owned(), doc);
    let warnings = scribe_core::extensions::hardcoded_link_warnings(&config, &docs);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("could be replaced by an extlink"));
    assert!(warnings[0].contains(":issue:`7`"));
}

// ----- todo ----------------------------------------------------------------

#[test]
fn todo_directive_and_todolist_expand_across_documents() {
    let mut config = config_with(&["sphinx.ext.todo"]);
    config.set("todo_include_todos", ConfigValue::Bool(true)).unwrap();
    let context = parse(
        "Title\n=====\n\n.. todolist::\n",
        &config,
        std::path::Path::new("."),
    );
    let source = parse(
        "Todo page\n=========\n\n.. todo:: in source\n",
        &config,
        std::path::Path::new("."),
    );
    let mut todos = TodoCollector::default();
    assert!(todos.collect("index", &context, &config).is_empty());
    assert!(todos.collect("source", &source, &config).is_empty());
    assert_eq!(todos.todos().len(), 1);
    let mut docs = BTreeMap::new();
    docs.insert("index".to_owned(), context);
    docs.insert("source".to_owned(), source);
    let mut target_uris = BTreeMap::new();
    target_uris.insert("index".to_owned(), "index.html".to_owned());
    target_uris.insert("source".to_owned(), "source.html".to_owned());
    todos.expand(&mut docs, &config, &target_uris);
    let pseudo = docs["index"].to_pseudoxml().unwrap();
    assert!(pseudo.contains("admonition-todo"), "{pseudo}");
    assert!(pseudo.contains("todo-source"), "{pseudo}");
    assert!(pseudo.contains("source.html"), "{pseudo}");
    assert!(pseudo.contains("original entry"), "{pseudo}");
}

#[test]
fn todo_emit_warnings_is_config_gated() {
    let mut config = config_with(&["sphinx.ext.todo"]);
    config.set("todo_emit_warnings", ConfigValue::Bool(true)).unwrap();
    let doc = parse(".. todo:: unresolved\n", &config, std::path::Path::new("."));
    let mut todos = TodoCollector::default();
    let warnings = todos.collect("index", &doc, &config);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("TODO entry found"));
}

// ----- viewcode ------------------------------------------------------------

#[test]
fn viewcode_collects_modules_and_injects_source_links() {
    let dir = std::env::temp_dir().join(format!("scribe-viewcode-test-{}", std::process::id()));
    let _ = fs::create_dir_all(&dir);
    fs::write(dir.join("spam.py"), "def func1():\n    return 1\n").unwrap();
    let config = config_with(&["sphinx.ext.viewcode"]);
    let source = "Title\n=====\n\n.. py:function:: func1()\n";
    let mut doc = parse(source, &config, &dir);
    // Run the domain transform pipeline to enrich `desc_signature`.
    let tags = scribe_config::Tags::new();
    let registry = scribe_domains::DomainRegistry::builtin();
    let transform_ctx = scribe_core::TransformContext::new(Some(&config), Some(&tags), "html", "html")
        .with_domains(&registry);
    let _ = scribe_core::apply_transforms(&mut doc, &transform_ctx, scribe_core::Phase::Read);
    let _ = scribe_core::apply_transforms(&mut doc, &transform_ctx, scribe_core::Phase::Resolved);
    let mut collector = ViewcodeCollector::default();
    collector.collect("index", &mut doc, &config, &dir);
    assert!(!collector.is_empty(), "no modules collected");
    assert!(collector.modules().contains_key("spam"));
    let pseudo = doc.to_pseudoxml().unwrap();
    assert!(pseudo.contains("viewcode-link"), "{pseudo}");
    assert!(pseudo.contains("_modules/spam.html#func1"), "{pseudo}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn viewcode_is_robust_when_source_file_is_missing() {
    let dir = std::env::temp_dir().join(format!("scribe-viewcode-missing-{}", std::process::id()));
    let _ = fs::create_dir_all(&dir);
    let config = config_with(&["sphinx.ext.viewcode"]);
    let source = "Title\n=====\n\n.. py:function:: func1()\n";
    let mut doc = parse(source, &config, &dir);
    let tags = scribe_config::Tags::new();
    let registry = scribe_domains::DomainRegistry::builtin();
    let transform_ctx = scribe_core::TransformContext::new(Some(&config), Some(&tags), "html", "html")
        .with_domains(&registry);
    let _ = scribe_core::apply_transforms(&mut doc, &transform_ctx, scribe_core::Phase::Read);
    let _ = scribe_core::apply_transforms(&mut doc, &transform_ctx, scribe_core::Phase::Resolved);
    let mut collector = ViewcodeCollector::default();
    collector.collect("index", &mut doc, &config, &dir);
    assert!(collector.is_empty());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn parse_tags_is_defensive_on_odd_python_source() {
    assert!(viewcode::parse_tags("").is_empty());
    assert!(viewcode::parse_tags("class :\ndef (").is_empty());
    assert_eq!(viewcode::parse_tags("async def thing():\n    pass\n")["thing"].0, 0);
}

// ----- graphviz ------------------------------------------------------------

#[test]
fn graphviz_directives_are_registered_and_produce_typed_nodes() {
    let config = config_with(&["sphinx.ext.graphviz"]);
    let doc = parse(
        "G\n=\n\n.. digraph:: foo\n   :alt: a graph\n\n   a -> b\n",
        &config,
        std::path::Path::new("."),
    );
    let marker_nodes: Vec<_> = doc
        .preorder_ids()
        .into_iter()
        .filter(|&id| doc.get(id).is_ok_and(|node| node.attrs.get_str(EXT_MARKER).ok().flatten() == Some(EXT_GRAPHVIZ)))
        .collect();
    assert_eq!(marker_nodes.len(), 1);
    let node = doc.get(marker_nodes[0]).unwrap();
    assert_eq!(node.attrs.get_str("gv_code").unwrap().unwrap(), "digraph foo {\na -> b\n}\n");
    assert_eq!(node.attrs.get_str("gv_alt").unwrap().unwrap(), "a graph");
}

#[test]
fn graphviz_degrades_gracefully_when_dot_is_missing() {
    let dir = std::env::temp_dir().join(format!("scribe-graphviz-missing-{}", std::process::id()));
    let out = dir.join("out");
    let _ = fs::create_dir_all(&out);
    let mut config = config_with(&["sphinx.ext.graphviz"]);
    config.set("graphviz_dot", ConfigValue::from("scribe-dot-that-does-not-exist")).unwrap();
    let mut doc = parse(
        "G\n=\n\n.. digraph:: foo\n\n   a -> b\n",
        &config,
        &dir,
    );
    let mut renderer = GraphvizRender::new(&config, &dir, &out);
    let (infos, warnings) = renderer.render_document("index", &mut doc);
    assert!(warnings.iter().any(|warning| warning.contains("cannot be run")), "{warnings:?}");
    assert_eq!(infos.len(), 1);
    let info = infos.values().next().unwrap();
    assert!(info.image_uri.is_none());
    assert!(info.code.contains("a -> b"));
    let _ = fs::remove_dir_all(&dir);
}

// ----- githubpages / duration ---------------------------------------------

#[test]
fn githubpages_rules_match_upstream() {
    let mut config = Config::new();
    assert_eq!(GithubPagesFiles::from_config(&config).cname, None);
    config.set("html_baseurl", ConfigValue::from("https://example.com/docs")).unwrap();
    assert_eq!(GithubPagesFiles::from_config(&config).cname.as_deref(), Some("example.com"));
    config.set("html_baseurl", ConfigValue::from("https://x.github.io/docs")).unwrap();
    assert_eq!(GithubPagesFiles::from_config(&config).cname, None);
}

#[test]
fn duration_report_is_deterministic_and_gated() {
    let mut report = DurationReport::default();
    report.note("b", 0.4321);
    report.note("a", 1.5);
    let config = Config::new();
    let text = report.render(&config);
    assert!(text.contains("total reading duration"));
    assert!(text.contains("slowest reading durations"));
    assert!(text.find("1.500s a").unwrap() < text.find("0.432s b").unwrap());
    assert_eq!(report.to_json(), "{\n  \"a\": 1.5,\n  \"b\": 0.432\n}");

    let mut quiet = Config::new();
    quiet.set("duration_print_total", ConfigValue::Bool(false)).unwrap();
    quiet.set("duration_print_slowest", ConfigValue::Bool(false)).unwrap();
    assert!(report.render(&quiet).is_empty());
}

#[test]
fn known_oracle_fixtures_parse_without_panics() {
    let todo = roots().join("test-ext-todo");
    let doc = parse(
        &fs::read_to_string(todo.join("foo.rst")).unwrap(),
        &config_with(&["sphinx.ext.todo"]),
        &todo,
    );
    assert!(doc.to_pseudoxml().is_ok());

    let ifconfig = roots().join("test-ext-ifconfig");
    let mut doc = parse(
        &fs::read_to_string(ifconfig.join("index.rst")).unwrap(),
        &config_with(&["sphinx.ext.ifconfig"]),
        &ifconfig,
    );
    let config = config_with(&["sphinx.ext.ifconfig"]);
    let _ = ifconfig::process(&mut doc, &config, "html");
    assert!(doc.to_pseudoxml().is_ok());

    let graphviz = roots().join("test-ext-graphviz");
    let doc = parse(
        &fs::read_to_string(graphviz.join("index.rst")).unwrap(),
        &config_with(&["sphinx.ext.graphviz"]),
        &graphviz,
    );
    assert!(doc.to_pseudoxml().is_ok());
}

#[test]
fn activation_order_is_deterministic() {
    let config = config_with(&["sphinx.ext.graphviz", "sphinx.ext.todo", "sphinx.ext.graphviz"]);
    let active = ActivatedExtensions::from_config(&config);
    assert_eq!(active.names(), vec!["graphviz", "todo"]);
    assert_eq!(active.names().iter().copied().collect::<BTreeSet<_>>().len(), 2);
}
