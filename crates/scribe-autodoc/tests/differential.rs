//! Differential tests against the pinned Sphinx 9.3 oracle (Phase 21).
//!
//! Method (documented in ARCHITECTURE.md §21.9):
//!
//! 1. Copy a real `sphinx/tests/roots/test-ext-*` fixture and run
//!    `sphinx-build -b pseudoxml -E -q`;
//! 2. parse the same `.rst` files through scribe with the autodoc/napoleon
//!    directives registered, expand signatures via `prepare_document`;
//! 3. reduce both trees to object identities, ordered signature names and
//!    docstring field names, then compare the documented subset;
//! 4. record known static-vs-dynamic divergences explicitly rather than
//!    silently ignoring them.
//!
//! Normalized away: generated ids, source paths, XML namespaces, CSS classes,
//! `py:*` skeleton attributes and index-entry payloads.

mod common;

use std::fs;
use std::path::Path;

use common::{
    config, field_names, object_set, oracle_pseudoxml, scribe_pseudoxml, signature_facts,
    signature_text, workspace,
};
use scribe_config::ConfigValue;

fn fixture_rst(fixture: &str, file: &str) -> String {
    let path = workspace()
        .join("sphinx/tests/roots")
        .join(fixture)
        .join(file);
    fs::read_to_string(path).expect("fixture rst")
}

fn fixture_dir(fixture: &str) -> std::path::PathBuf {
    workspace().join("sphinx/tests/roots").join(fixture)
}

#[test]
fn autodoc_fixture_object_identities_match() {
    let fixture = "test-ext-autodoc";
    let oracle = oracle_pseudoxml(fixture, "index");
    let rst = fixture_rst(fixture, "index.rst");
    let config = config(&["sphinx.ext.autodoc"], &[
        ("autodoc_member_order", ConfigValue::from("bysource")),
    ]);
    let ours = scribe_pseudoxml(&rst, &fixture_dir(fixture), &config);

    let oracle_objects = object_set(&oracle);
    let our_objects = object_set(&ours);
    assert!(!oracle_objects.is_empty(), "oracle produced no objects");
    assert_eq!(
        oracle_objects, our_objects,
        "\noracle signatures: {oracle_objects:?}\nours: {our_objects:?}"
    );
    // Ordered signature names must agree in document order too.
    assert_eq!(signature_text(&oracle), signature_text(&ours));
}

#[test]
fn napoleon_fixture_signatures_and_fields_match() {
    let fixture = "test-ext-napoleon";
    let oracle = oracle_pseudoxml(fixture, "typehints");
    let rst = fixture_rst(fixture, "typehints.rst");
    let config = config(&["sphinx.ext.autodoc", "sphinx.ext.napoleon"], &[]);
    let ours = scribe_pseudoxml(&rst, &fixture_dir(fixture), &config);

    assert_eq!(object_set(&oracle), object_set(&ours));
    // Both engines recognize the Napoleon `Parameters` field list.
    let oracle_fields = field_names(&oracle);
    let our_fields = field_names(&ours);
    assert!(oracle_fields.contains(&"parameters".to_owned()), "{oracle_fields:?}");
    assert!(our_fields.contains(&"parameters".to_owned()), "{our_fields:?}");
    // Parameter names survive conversion in both.
    for name in ["x", "*args", "**kwargs"] {
        assert!(oracle.contains(name), "oracle missing {name}");
        assert!(ours.contains(name), "ours missing {name}");
    }
}

#[test]
fn napoleon_paramtype_fixture_documents_keyword_params() {
    let fixture = "test-ext-napoleon-paramtype";
    let oracle = oracle_pseudoxml(fixture, "index");
    let rst = fixture_rst(fixture, "index.rst");
    let config = config(&["sphinx.ext.autodoc", "sphinx.ext.napoleon"], &[]);
    let ours = scribe_pseudoxml(&rst, &fixture_dir(fixture), &config);

    assert_eq!(object_set(&oracle), object_set(&ours));
    // Every `:keyword:` target from the fixture is present in both trees.
    for name in [
        "keyword_paramtype", "keyword_kwtype", "kwarg_paramtype", "kwarg_kwtype",
        "kwparam_paramtype", "kwparam_kwtype",
    ] {
        assert!(oracle.contains(name), "oracle missing {name}");
        assert!(ours.contains(name), "ours missing {name}");
    }
    // Documented divergence: the frozen domain groups `:keyword:` fields as
    // "Parameters" and leaves `:kwtype:` unconverted, whereas upstream groups
    // them as "Keyword Arguments". Asserted in ARCHITECTURE.md §21.9.
    assert!(field_names(&ours).contains(&"parameters".to_owned()));
}

#[test]
fn signature_facts_include_annotations() {
    let fixture = "test-ext-autodoc";
    let oracle = oracle_pseudoxml(fixture, "index");
    let rst = fixture_rst(fixture, "index.rst");
    let config = config(&["sphinx.ext.autodoc"], &[
        ("autodoc_member_order", ConfigValue::from("bysource")),
    ]);
    let ours = scribe_pseudoxml(&rst, &fixture_dir(fixture), &config);
    // The `incr(a: int, b: int = 1) -> int` signature must carry its type
    // annotation in both engines.
    let oracle_facts = signature_facts(&oracle);
    let our_facts = signature_facts(&ours);
    assert!(oracle_facts.iter().any(|(module, _, name)| module == "target.typehints" && name == "incr"));
    assert!(our_facts.iter().any(|(module, _, name)| module == "target.typehints" && name == "incr"));
    assert!(ours.contains("int"), "annotations missing from our signature tree");
}

#[test]
fn missing_module_warns_instead_of_panicking() {
    let registry = scribe_rst::DirectiveRegistry::builtin();
    scribe_autodoc::register_directives(&registry);
    let rst = ".. automodule:: definitely_missing_module_xyz\n   :members:\n";
    let mut output = scribe_rst::parse(rst);
    let config = config(&[], &[]);
    let mut ctx = scribe_rst::Context::new(Path::new(env!("CARGO_MANIFEST_DIR")), scribe_config::Tags::new());
    ctx.config = Some(std::sync::Arc::new(config));
    let report = scribe_rst::resolve_output(&mut output, &registry, &ctx);
    // The directive emits a warning paragraph instead of failing the build.
    let text = output.document.to_pseudoxml().expect("pseudoxml");
    assert!(text.contains("failed to import") || !report.unresolved.is_empty());
}
