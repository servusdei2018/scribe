//! Differential tests against the pinned Python Sphinx oracle.
//!
//! Two axes are compared: resolved configuration values from a real `Sphinx`
//! application, and `Tags.eval_condition` results/error categories.

use scribe_config::{
    default_python_interpreter, load_config, load_config_with, BridgeStrategy, LoadRequest,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn python() -> PathBuf {
    let candidate = default_python_interpreter();
    assert!(candidate.is_file(), "Python interpreter not found: {}", candidate.display());
    candidate
}

fn temporary_output(label: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "scribe-config-diff-{label}-{}-{id}",
        std::process::id()
    ))
}

const CONFIG_SCRIPT: &str = r#"import json
import os
import sys
from sphinx.application import Sphinx

source = sys.argv[1]
output = sys.argv[2]
app = Sphinx(source, source, output, os.path.join(output, '.doctrees'), 'html',
             status=None, warning=None, freshenv=True)
config = app.config
keys = [
    'project', 'author', 'release', 'version', 'master_doc', 'root_doc',
    'html_theme', 'html_title', 'language', 'extensions', 'exclude_patterns',
    'templates_path', 'source_suffix', 'copyright', 'nitpicky', 'smartquotes',
    'html_static_path', 'intersphinx_mapping', 'autodoc_member_order',
    'autodoc_typehints', 'todo_include_todos', 'latex_documents', 'man_pages',
    'texinfo_documents', 'epub_title', 'htmlhelp_basename',
]


def encode(value):
    if value is None or isinstance(value, (bool, int, float, str)):
        return value
    if isinstance(value, (list, tuple, set, frozenset)):
        return [encode(item) for item in value]
    if isinstance(value, dict):
        return {str(key): encode(item) for key, item in value.items()}
    return str(value)


result = {}
for key in keys:
    try:
        result[key] = encode(getattr(config, key))
    except Exception as exc:
        result[key] = {'oracle_error': type(exc).__name__}
print(json.dumps(result, ensure_ascii=False))
"#;

const TAGS_SCRIPT: &str = r#"import json
import sys
from sphinx.util.tags import Tags

expressions = json.loads(sys.argv[1])
tags = Tags(['foo', 'bar'])
result = {}
for expression in expressions:
    try:
        result[expression] = {'ok': True, 'value': tags.eval_condition(expression)}
    except Exception as exc:
        result[expression] = {'ok': False, 'error': type(exc).__name__}
print(json.dumps(result, ensure_ascii=False))
"#;

#[test]
fn config_values_match_the_oracle() {
    let root = workspace_root();
    let interpreter = python();
    let cases = [
        "test-basic",
        "test-config",
        "test-ext-autodoc",
        "test-root",
        "test-theming",
    ];

    for (index, name) in cases.iter().enumerate() {
        let source = root.join("sphinx/tests/roots").join(name);
        let output = temporary_output(&format!("config-{index}"));
        let _ = std::fs::remove_dir_all(&output);
        std::fs::create_dir_all(&output).expect("create temp output");

        let result = Command::new(&interpreter)
            .arg("-c")
            .arg(CONFIG_SCRIPT)
            .arg(&source)
            .arg(&output)
            .output()
            .unwrap_or_else(|error| panic!("cannot run oracle: {error}"));
        assert!(
            result.status.success(),
            "oracle failed for {name}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let oracle: BTreeMap<String, serde_json::Value> =
            serde_json::from_slice(&result.stdout).expect("oracle JSON");

        let request = LoadRequest {
            conf_dir: source.clone(),
            strategy: BridgeStrategy::Python,
            ..LoadRequest::default()
        };
        let (config, _warnings) = load_config_with(request)
            .unwrap_or_else(|error| panic!("scribe failed to load {name}: {error}"));

        for (key, expected) in &oracle {
            if expected.get("oracle_error").is_some() {
                continue;
            }
            let actual = config.get(key).unwrap_or_else(|| panic!("missing {key} in {name}"));
            let actual_json = actual.to_json();
            assert_eq!(&actual_json, expected, "value mismatch for {key} in {name}");
        }
        let _ = std::fs::remove_dir_all(&output);
    }
}

#[test]
fn tags_match_the_oracle() {
    let expressions = [
        "foo",
        "bar",
        "baz",
        "not foo",
        "not baz",
        "foo and bar",
        "foo and baz",
        "foo or baz",
        "baz or qux",
        "not (foo and bar)",
        "(foo or baz) and bar",
        "foo if bar else baz",
        "foo if baz else bar",
        "foo if bar else baz if foo else qux",
        "foo if bar",
        "foo if baz",
        // Constants parse upstream but `_eval_node` has no Const branch.
        "true",
        "True",
        "false",
        "False",
        "none",
        "None",
        "not None",
        // Malformed inputs.
        "",
        "foo bar",
        "and foo",
        "foo and",
        "(foo",
        "foo)",
        "foo == bar",
        "1",
        "'str'",
        "not",
    ];

    let expressions_json =
        serde_json::to_string(&expressions.as_slice()).expect("serialize expressions");
    let result = Command::new(python())
        .arg("-c")
        .arg(TAGS_SCRIPT)
        .arg(expressions_json)
        .output()
        .expect("run oracle tags evaluator");
    assert!(result.status.success(), "oracle failed: {}", String::from_utf8_lossy(&result.stderr));
    let oracle: BTreeMap<String, serde_json::Value> =
        serde_json::from_slice(&result.stdout).expect("oracle JSON");

    let tags = scribe_config::Tags::with_tags(["foo", "bar"]);
    for expression in expressions {
        let expected = &oracle[expression];
        let expected_ok = expected["ok"].as_bool().expect("ok flag");
        let actual = tags.eval_condition(expression);
        match (expected_ok, actual) {
            (true, Ok(value)) => {
                assert_eq!(expected["value"].as_bool(), Some(value), "mismatch for {expression:?}");
            }
            (false, Err(_)) => {}
            (true, Err(error)) => panic!("oracle accepted {expression:?} but scribe failed: {error}"),
            (false, Ok(value)) => panic!("oracle rejected {expression:?} but scribe returned {value}"),
        }
    }
}

#[test]
fn toml_and_python_paths_produce_identical_scalar_values() {
    let root = workspace_root();
    let source = root.join("sphinx/tests/roots/test-config");
    let (python_config, _) = load_config(&source, None, None).expect("python config");

    let toml_dir = temporary_output("toml");
    let _ = std::fs::remove_dir_all(&toml_dir);
    std::fs::create_dir_all(&toml_dir).expect("create temp dir");
    std::fs::write(
        toml_dir.join("scribe.toml"),
        "project = \"Sphinx <Tests>\"\nrelease = \"0.6alpha1\"\ntemplates_path = [\"_templates\"]\n",
    )
    .expect("write toml");
    let (toml_config, _) = load_config(&toml_dir, None, None).expect("toml config");

    assert_eq!(python_config.get_str("project"), toml_config.get_str("project"));
    assert_eq!(python_config.get_str("release"), toml_config.get_str("release"));
    assert_eq!(
        python_config.get_strings("templates_path"),
        toml_config.get_strings("templates_path")
    );
    let _ = std::fs::remove_dir_all(&toml_dir);
}
