//! Unit/integration tests for native autosummary (Phase 22).

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use scribe_ast::{Document, NodeKind};
use scribe_autodoc::autosummary::{
    escape_rst, generate_stubs, mangle_signature, parse_names, StubRenderer,
};
use scribe_config::{Config, ConfigValue, Tags};
use scribe_rst::{parse, resolve_output, Context, DirectiveRegistry};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn fixture() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/asum");
    let target = std::env::temp_dir().join(format!("scribe-asum-test-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed)));
    let _ = fs::remove_dir_all(&target);
    copy_dir(&root, &target);
    target
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn write_index(src: &Path, body: &str) {
    fs::write(src.join("index.rst"), body).unwrap();
}

fn config(extra: &[(&str, ConfigValue)]) -> Config {
    let mut config = Config::new();
    config.set("extensions", ConfigValue::List(vec![ConfigValue::from("sphinx.ext.autosummary")])).unwrap();
    config.set("autosummary_generate", ConfigValue::Bool(true)).unwrap();
    for (key, value) in extra {
        config.set(key, value.clone()).unwrap();
    }
    config
}

fn render(rst: &str, source_dir: &Path, config: &Config, docname: &str) -> Document {
    let mut output = parse(rst);
    let registry = DirectiveRegistry::builtin();
    scribe_autodoc::autosummary::register_directives(&registry);
    let mut ctx = Context::new(source_dir, Tags::new());
    ctx.config = Some(Arc::new(config.clone()));
    ctx.source_name = Some(docname.to_owned());
    resolve_output(&mut output, &registry, &ctx);
    output.document
}

fn table_rows(doc: &Document) -> Vec<String> {
    let mut rows = Vec::new();
    for (_, node) in doc.iter() {
        if node.kind == NodeKind::Row {
            rows.push(node.astext(doc));
        }
    }
    rows
}

#[test]
fn table_has_two_columns_and_pyobj_xrefs() {
    let src = fixture();
    let doc = render(
        ".. autosummary::\n\n   mymod.helper\n   mymod.Widget\n",
        &src,
        &config(&[]),
        "index",
    );
    let table = doc.iter().find(|(_, node)| node.kind == NodeKind::Table).map(|(id, _)| id);
    let table = table.expect("a table must be emitted");
    let children = doc.children(table).unwrap();
    assert!(children.iter().any(|id| doc.get(*id).unwrap().kind == NodeKind::Tgroup));
    let rows = table_rows(&doc);
    assert_eq!(rows.len(), 2);
    assert!(rows[0].contains("mymod.helper"));
    assert!(rows[0].contains("(x[, y])"), "signature should be mangled: {}", rows[0]);
    assert!(rows[0].contains("Help with things."));
    assert!(rows[1].contains("mymod.Widget"));
    let xrefs = doc
        .iter()
        .filter(|(_, node)| node.kind == NodeKind::PendingXref)
        .count();
    assert_eq!(xrefs, 2, "one py:obj xref per first-column entry");
}

#[test]
fn nosignatures_and_list_ordered_variants() {
    let src = fixture();
    let doc = render(
        ".. autosummary::\n   :nosignatures:\n   :list-ordered:\n\n   mymod.helper\n",
        &src,
        &config(&[]),
        "index",
    );
    assert!(doc.iter().all(|(_, node)| node.kind != NodeKind::Table));
    assert!(doc.iter().any(|(_, node)| node.kind == NodeKind::EnumeratedList));
    let text = doc.root().astext(&doc);
    assert!(text.contains("mymod.helper"));
    assert!(!text.contains("(x[, y])"));
}

#[test]
fn stub_files_match_bundled_templates() {
    let src = fixture();
    write_index(&src, ".. autosummary::\n   :toctree: generated\n\n   mymod\n   mymod.Widget\n");
    let report = generate_stubs(&config(&[]), &src, &[]);
    let names: Vec<String> = report.generated.iter().map(|path| path.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert!(names.iter().any(|name| name == "mymod.rst"), "{names:?}");
    assert!(names.iter().any(|name| name == "mymod.Widget.rst"));
    let module = fs::read_to_string(src.join("generated/mymod.rst")).unwrap();
    let module = module.strip_prefix('\u{feff}').unwrap_or(&module);
    assert!(module.starts_with("mymod\n=====\n\n.. automodule:: mymod"), "{module:?}");
    assert!(module.contains(".. rubric:: Functions"));
    assert!(module.contains("   helper"));
    assert!(module.contains(".. rubric:: Classes"));
    assert!(module.contains("   Widget"));
    assert!(module.contains(".. rubric:: Exceptions"));
    assert!(module.contains("   MyError"));
    let widget = fs::read_to_string(src.join("generated/mymod.Widget.rst")).unwrap();
    assert!(widget.contains(".. autoclass:: Widget"));
    assert!(widget.contains(".. automethod:: __init__"));
    assert!(widget.contains("~Widget.run"));
    assert!(widget.contains("~Widget.ready"), "property belongs to Attributes");
}

#[test]
fn template_override_is_honored() {
    let src = fixture();
    let templates = src.join("templates");
    fs::create_dir_all(&templates).unwrap();
    fs::write(
        templates.join("custom.rst"),
        "CUSTOM {{ fullname }} / {{ objtype }} / {{ module }}\n",
    )
    .unwrap();
    let config = config(&[("templates_path", ConfigValue::List(vec![ConfigValue::from("templates")]))]);
    let rst = ".. autosummary::\n   :toctree: generated\n   :template: custom.rst\n\n   mymod.helper\n";
    fs::write(src.join("index.rst"), rst).unwrap();
    let _doc = render(rst, &src, &config, "index");
    let report = generate_stubs(&config, &src, &[]);
    assert!(!report.generated.is_empty());
    let stub = fs::read_to_string(src.join("generated/mymod.helper.rst")).unwrap();
    let stub = stub.strip_prefix('\u{feff}').unwrap_or(&stub);
    assert_eq!(stub.trim_end(), "CUSTOM mymod.helper / function / mymod");
    // Also render the override directly through the renderer API.
    let renderer = StubRenderer::new(vec![templates]);
    let mut vars = std::collections::BTreeMap::new();
    vars.insert("fullname".to_owned(), minijinja::Value::from("mymod.helper"));
    vars.insert("objtype".to_owned(), minijinja::Value::from("function"));
    vars.insert("module".to_owned(), minijinja::Value::from("mymod"));
    let rendered = renderer.render(Some("custom.rst"), "function", &vars).unwrap();
    assert_eq!(rendered, "CUSTOM mymod.helper / function / mymod");
}

#[test]
fn recursive_traversal_generates_package_submodules() {
    let src = fixture();
    let config = config(&[]);
    let rst = ".. autosummary::\n   :toctree: generated\n   :recursive:\n\n   mypkg\n";
    fs::write(src.join("index.rst"), rst).unwrap();
    let report = generate_stubs(&config, &src, &[]);
    let names: Vec<String> = report.generated.iter().map(|path| path.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert!(names.iter().any(|name| name == "mypkg.rst"), "{names:?}");
    assert!(names.iter().any(|name| name == "mypkg.alpha.rst"), "{names:?}");
    assert!(names.iter().any(|name| name == "mypkg.beta.rst"), "{names:?}");
    let module = fs::read_to_string(src.join("generated/mypkg.rst")).unwrap();
    assert!(module.contains(".. rubric:: Modules"), "{module}");
    assert!(module.contains("   alpha"));
}

#[test]
fn filename_map_renames_stubs_and_toctree() {
    let src = fixture();
    let map = ConfigValue::Dict(std::collections::BTreeMap::from([(
        "mymod".to_owned(),
        ConfigValue::from("renamed_mod"),
    )]));
    let config = config(&[("autosummary_filename_map", map)]);
    write_index(&src, ".. autosummary::\n   :toctree: generated\n\n   mymod\n");
    let report = generate_stubs(&config, &src, &[]);
    assert!(report.generated.iter().any(|path| path.ends_with("renamed_mod.rst")));
    assert!(!report.generated.iter().any(|path| path.ends_with("mymod.rst")));
}

#[test]
fn missing_module_is_a_warning_not_a_panic() {
    let src = fixture();
    let doc = render(".. autosummary::\n\n   does.not.exist\n", &src, &config(&[]), "index");
    let text = doc.root().astext(&doc);
    assert!(text.contains("failed to import") || text.contains("stub file not found"), "{text:?}");
}

#[test]
fn malformed_directives_never_panic() {
    let cases = [
        ".. autosummary::\n\n",
        ".. autosummary::\n   :toctree:\n   :signatures: nonsense\n\n   mymod.helper\n",
        ".. autosummary::\n   :toctree: ../..\n\n   \x00 bad name\n",
        ".. autosummary::\n\n   ~\n   ...\n   123\n",
    ];
    for rst in cases {
        let src = fixture();
        let _ = render(rst, &src, &config(&[]), "index");
    }
}

#[test]
fn helpers_follow_upstream_semantics() {
    assert_eq!(escape_rst("a_b/c.d"), "a\\_b\\/c.d");
    assert_eq!(mangle_signature("f(x: int, y=2) -> bool", 50), "(x[, y])");
    assert_eq!(parse_names("~pkg.foo\nbar\n"), vec!["~pkg.foo", "bar"]);
}
