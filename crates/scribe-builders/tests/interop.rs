//! Phase 27 end-to-end interop tests: third-party Python extensions inside a
//! real `scribe build`, plus differential checks that the event bus leaves
//! built-in extension output byte-for-byte unchanged.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use scribe_builders::html::{BuildOptions, StandaloneHtmlBuilder};
use scribe_config::{Config, ConfigValue};
use scribe_core::{BuildEnvironment, InteropBridge};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn tempdir(label: &str) -> PathBuf {
    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "scribe-phase27-{label}-{}-{sequence}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create temp dir");
    path
}

fn find_python() -> Option<PathBuf> {
    for candidate in ["python3", "python"] {
        let path = PathBuf::from(candidate);
        if Command::new(&path).arg("-c").arg("pass").status().is_ok() {
            return Some(path);
        }
    }
    None
}

fn fixture_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../scribe-core/tests/fixtures/python_extension.py")
        .canonicalize()
        .expect("fixture script exists")
}

fn build(source: &Path, config: &Config, bridge: Option<Arc<InteropBridge>>) -> (String, Vec<String>) {
    let out = tempdir("out");
    let options = BuildOptions {
        source_dir: source.to_path_buf(),
        output_dir: out.clone(),
        builder_name: "html".to_owned(),
        force_all: true,
        freshenv: true,
        really_quiet: true,
        bridge,
        ..BuildOptions::default()
    };
    let mut env = BuildEnvironment::with_config(source, config);
    let result = StandaloneHtmlBuilder::build(&options, config, &mut env)
        .expect("scribe html build succeeds");
    let html = std::fs::read_to_string(out.join("index.html")).expect("index.html written");
    (html, result.warnings)
}

fn source_with(label: &str, body: &str) -> PathBuf {
    let root = tempdir(label);
    std::fs::write(root.join("index.rst"), format!("Title\n=====\n\n{body}\n"))
        .expect("write index.rst");
    root
}

#[test]
fn python_extension_directive_and_source_read_appear_in_built_html() {
    let Some(python) = find_python() else { return };
    let source = source_with("python-bridge", "<<MARKER>>\n\n.. pybadge:: phase27\n");
    let mut config = Config::new();
    config.set("scribe_python", ConfigValue::from(python.to_string_lossy().into_owned()))
        .expect("registered key");
    config.set("scribe_python_extensions", ConfigValue::List(vec![
        ConfigValue::from(fixture_script().to_string_lossy().into_owned()),
    ])).expect("registered key");
    let bridge = Arc::new(InteropBridge::from_config(&config, &source));
    let (html, warnings) = build(&source, &config, Some(bridge));

    assert!(html.contains("PYTHON-WAS-HERE"), "source-read handler did not mutate source: {html}");
    assert!(html.contains("PYTHON_DIRECTIVE_OUTPUT[phase27]"), "directive output missing: {html}");
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
}

#[test]
fn without_bridge_the_same_source_has_no_python_markers() {
    let source = source_with("no-bridge", "<<MARKER>>\n");
    let config = Config::new();
    let (html, _warnings) = build(&source, &config, None);
    assert!(html.contains("&lt;&lt;MARKER&gt;&gt;") || html.contains("<<MARKER>>"));
    assert!(!html.contains("PYTHON-WAS-HERE"));
}

#[test]
fn built_in_extension_output_is_identical_with_and_without_event_bus() {
    // The event bus is additive: a build with built-in extensions active but
    // no third-party handlers must be byte-for-byte identical to a build that
    // never constructs a bridge.
    let source = source_with("differential", ".. todo:: A tracked item\n");
    let mut config = Config::new();
    config.set("extensions", ConfigValue::List(vec![ConfigValue::from("sphinx.ext.todo")]))
        .expect("registered key");
    config.set("todo_include_todos", ConfigValue::Bool(true))
        .expect("registered key");

    let (plain, plain_warnings) = build(&source, &config, None);
    let bridge = Arc::new(InteropBridge::from_config(&config, &source));
    let (bridged, bridged_warnings) = build(&source, &config, Some(bridge));

    assert_eq!(plain, bridged, "event bus changed built-in extension output");
    assert_eq!(plain_warnings, bridged_warnings);
    assert!(plain.contains("A tracked item"), "todo extension output missing");
}

#[test]
fn every_required_core_event_fires_in_one_small_build() {
    use std::sync::Mutex;
    use scribe_core::{handler, CORE_EVENTS};

    let source = source_with(
        "all-events",
        // An unknown directive guarantees a `missing-reference` /
        // `warn-missing-reference` emission alongside the always-fired events.
        ".. no-such-directive:: trigger\n",
    );
    let config = Config::new();
    let bridge = Arc::new(InteropBridge::new());
    let seen: Arc<Mutex<std::collections::BTreeSet<String>>> =
        Arc::new(Mutex::new(std::collections::BTreeSet::new()));
    for name in CORE_EVENTS {
        let sink = Arc::clone(&seen);
        bridge.events.connect(name, handler(move |event| {
            sink.lock().expect("lock").insert(event.name().to_owned());
            Ok(None)
        }), scribe_core::DEFAULT_PRIORITY).expect("connect core event");
    }
    let (_html, _warnings) = build(&source, &config, Some(bridge));
    let seen = seen.lock().expect("lock");
    for required in [
        "config-inited", "builder-inited", "env-get-outdated", "env-get-updated",
        "env-updated", "source-read", "doctree-read", "doctree-resolved",
        "missing-reference", "warn-missing-reference", "html-page-context",
        "write-done", "build-finished",
    ] {
        assert!(seen.contains(required), "event `{required}` never fired; saw {seen:?}");
    }
}

#[test]
fn wasm_plugin_feature_off_builds_are_unaffected() {
    let source = source_with("wasm-off", "plain text\n");
    let mut config = Config::new();
    config.set("scribe_wasm_plugins", ConfigValue::List(vec![
        ConfigValue::from("does-not-exist.wasm"),
    ])).expect("registered key");
    let bridge = Arc::new(InteropBridge::from_config(&config, &source));
    let (html, warnings) = build(&source, &config, Some(bridge));
    assert!(html.contains("plain text"));
    assert!(warnings.iter().any(|w| w.contains("wasm") || w.contains("Wasm")));
}
