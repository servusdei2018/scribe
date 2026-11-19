//! Tests for the native doctest directives and builder (Phase 22).

use std::fs;
use std::path::{Path, PathBuf};

use scribe_builders::doctest::{
    doctest_block_group, extract_codes, group_codes, register_directives, DoctestBuilder,
    DoctestRunResult, RunnerOptions,
};
use scribe_config::{Config, ConfigValue};
use scribe_rst::{parse, resolve_output, Context, DirectiveRegistry};

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn fixture(name: &str) -> PathBuf {
    workspace().join("crates/scribe-builders/tests/fixtures").join(name)
}

fn config() -> Config {
    let mut config = Config::new();
    config
        .set("extensions", ConfigValue::List(vec![ConfigValue::from("sphinx.ext.doctest")]))
        .unwrap();
    config
        .set(
            "doctest_global_setup",
            ConfigValue::from("def record(kind, value):\n    return value\n"),
        )
        .unwrap();
    config
}

fn doc_from_rst(rst: &str, source_dir: &Path) -> scribe_ast::Document {
    let mut output = parse(rst);
    let registry = DirectiveRegistry::builtin();
    register_directives(&registry);
    let mut ctx = Context::new(source_dir, scribe_config::Tags::new());
    ctx.config = Some(std::sync::Arc::new(config()));
    ctx.source_name = Some("index".to_owned());
    resolve_output(&mut output, &registry, &ctx);
    output.document
}

#[test]
fn directives_are_registered_and_emit_test_nodes() {
    let registry = DirectiveRegistry::builtin();
    register_directives(&registry);
    for name in ["testsetup", "testcleanup", "doctest", "testcode", "testoutput"] {
        assert!(registry.contains(name), "missing directive {name}");
    }
    let doc = doc_from_rst(
        ".. doctest::\n\n   >>> 1 + 1\n   2\n\n.. testcode:: g\n\n   print(3)\n\n.. testoutput:: g\n\n   3\n",
        Path::new("."),
    );
    let kinds: Vec<&str> = doc
        .iter()
        .filter_map(|(_, node)| node.attrs.get_str("testnodetype").ok().flatten())
        .collect();
    assert_eq!(kinds, vec!["doctest", "testcode", "testoutput"]);
}

#[test]
fn extraction_captures_free_blocks_groups_options_and_skipif() {
    let rst = "\
>>> 1 + 1
2

.. testcode:: alpha, beta
   :hide:

   print(1)

.. testoutput:: alpha, beta
   :options: +NORMALIZE_WHITESPACE

   1

.. doctest:: group
   :skipif: False
   :pyversion: >= 3.0

   >>> 2 + 2
   4
";
    let doc = doc_from_rst(rst, Path::new("."));
    let codes = extract_codes(&doc, "index.rst", &config());
    assert_eq!(codes.len(), 4, "{codes:?}");
    assert_eq!(codes[0].kind, "doctest");
    assert_eq!(codes[0].groups, vec!["default".to_owned()]);
    assert_eq!(codes[1].group_kind(), ("testcode", 2));
    assert!(codes[1].options.is_empty());
    assert_eq!(codes[2].kind, "testoutput");
    assert_eq!(codes[2].options.get(&4), Some(&true), "NORMALIZE_WHITESPACE");
    assert_eq!(codes[3].skipif.as_deref(), Some("False"));
    assert_eq!(codes[3].pyversion.as_deref(), Some(">= 3.0"));
}

#[test]
fn grouping_handles_setup_star_and_output_pairing() {
    let rst = "\
.. testsetup:: *

   SEED = 5

.. testsetup:: group

   EXTRA = 7

.. doctest:: group

   >>> SEED + EXTRA
   12

.. testcode:: group

   print('a')

.. testoutput:: group

   a
";
    let doc = doc_from_rst(rst, Path::new("."));
    let groups = group_codes(extract_codes(&doc, "index.rst", &config()));
    let group = &groups["group"];
    assert_eq!(group.setup.len(), 2, "`*` setup must be added to every group");
    assert_eq!(group.tests.len(), 2);
    assert_eq!(group.tests[1].len(), 2, "testcode must pair with testoutput");
}

#[test]
fn default_group_follows_config() {
    let mut config = Config::new();
    config.set("doctest_test_doctest_blocks", ConfigValue::from("mygroup")).unwrap();
    assert_eq!(doctest_block_group(&config).as_deref(), Some("mygroup"));
    config.set("doctest_test_doctest_blocks", ConfigValue::from("")).unwrap();
    assert_eq!(doctest_block_group(&config), None);
}

#[test]
fn real_execution_passes_all_self_contained_examples() {
    let out = std::env::temp_dir().join(format!("scribe-doctest-unit-{}", std::process::id()));
    let _ = fs::remove_dir_all(&out);
    let result = DoctestBuilder::build(&builder_options(&fixture("doctest"), &out), &config(), &mut env(&config())).unwrap();
    assert!(result.success(), "unexpected failures: {}", result.output);
    assert!(result.total_tries >= 14, "only {} tests ran", result.total_tries);
    assert_eq!(result.total_failures, 0);
    assert!(result.output_path.is_file());
    assert!(result.output.contains("Doctest summary"));
    let _ = fs::remove_dir_all(out);
}

#[test]
fn real_execution_reports_exact_failure_diff() {
    let src = std::env::temp_dir().join(format!("scribe-doctest-fail-src-{}", std::process::id()));
    let out = std::env::temp_dir().join(format!("scribe-doctest-fail-out-{}", std::process::id()));
    let _ = fs::remove_dir_all(&src);
    let _ = fs::remove_dir_all(&out);
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("index.rst"), "Failure\n=======\n\n>>> 1 + 1\n3\n").unwrap();
    let result = DoctestBuilder::build(&builder_options(&src, &out), &config(), &mut env(&config())).unwrap();
    assert!(!result.success());
    assert_eq!(result.total_tries, 1);
    assert_eq!(result.total_failures, 1);
    assert!(result.output.contains("Failed example:"));
    assert!(result.output.contains("Expected:\n    3\nGot:\n    2"), "{}", result.output);
    assert!(result.docs.iter().any(|(name, ok)| name == "index" && !ok));
    let _ = fs::remove_dir_all(src);
    let _ = fs::remove_dir_all(out);
}

#[test]
fn worker_reports_missing_interpreter_without_panicking() {
    let options = RunnerOptions {
        python: PathBuf::from("/definitely/not/python"),
        ..RunnerOptions::default()
    };
    assert!(scribe_builders::doctest::run_worker(&[], &options).is_err());
}

#[test]
fn fail_fast_and_summary_shape() {
    let result = DoctestRunResult {
        output: String::new(),
        docs: Vec::new(),
        total_tries: 3,
        total_failures: 1,
        setup_failures: 0,
        cleanup_failures: 0,
    };
    let summary = scribe_builders::doctest::format_summary(&result, true);
    assert!(summary.contains("Doctest summary (exiting after first failed test)"));
    assert!(summary.contains("    3 tests\n    1 failure in tests\n"));
}

#[test]
fn malformed_directives_never_panic() {
    let cases = [
        ".. doctest::\n\n",
        ".. doctest::\n   :options: ELLIPSIS, ++SKIP, BOGUS\n\n   >>> 1 + 1\n   2\n",
        ".. testcode:: a, b, c\n\n   print(1)\n",
        ".. testoutput::\n   :skipif: ((((\n\n   x\n",
        ".. testsetup::\n\n   \x00\n",
        ".. doctest::\n   :pyversion: not-a-version\n\n   >>> 1 + 1\n   2\n",
    ];
    for rst in cases {
        let _ = doc_from_rst(rst, Path::new("."));
    }
}

fn builder_options(source: &Path, output: &Path) -> scribe_builders::html::BuildOptions {
    scribe_builders::html::BuildOptions {
        source_dir: source.to_path_buf(),
        output_dir: output.to_path_buf(),
        builder_name: "doctest".to_owned(),
        freshenv: true,
        really_quiet: true,
        ..Default::default()
    }
}

fn env(config: &Config) -> scribe_core::BuildEnvironment {
    scribe_core::BuildEnvironment::with_config(Path::new("."), config)
}

// Helper trait for a compact assertion on grouped codes.
trait GroupKind {
    fn group_kind(&self) -> (&str, usize);
}

impl GroupKind for scribe_builders::doctest::TestCode {
    fn group_kind(&self) -> (&str, usize) {
        (self.kind.as_str(), self.groups.len())
    }
}
