//! Integration tests for the Python extension shim and the interop bridge.

use std::path::{Path, PathBuf};
use std::process::Command;

use scribe_core::{
    Event, EventManager, InteropBridge, PythonExtensionShim, ShimConfig, ShimError,
    EXAMPLE_EXTENSION_SCRIPT,
};
use scribe_config::{Config, ConfigValue};

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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/python_extension.py")
}

#[test]
fn inline_worker_registers_and_answers_callbacks() {
    let Some(python) = find_python() else { return };
    let shim = PythonExtensionShim::start(ShimConfig::inline("inline", python, EXAMPLE_EXTENSION_SCRIPT));
    assert!(shim.is_available(), "warnings: {:?}", shim.warnings());
    assert!(shim.events().iter().any(|e| e == "source-read"));
    assert!(shim.directives().iter().any(|d| d == "pybadge"));

    let directive = shim.call("directive", &serde_json::json!({
        "name": "pybadge", "arguments": ["hello"], "content": "", "options": {},
    })).expect("directive callback");
    assert_eq!(directive["text"], "PYTHON_DIRECTIVE_OUTPUT[hello]");

    let event = shim.call("on_event", &serde_json::json!({
        "event": "source-read", "payload": { "source": "x <<MARKER>> y" },
    })).expect("on_event callback");
    assert_eq!(event["source"], "x PYTHON-WAS-HERE y");
}

#[test]
fn script_path_worker_and_source_mutation_through_bus() {
    let Some(python) = find_python() else { return };
    let shim = PythonExtensionShim::start(ShimConfig::path("fileworker", python, fixture_script()));
    assert!(shim.is_available(), "warnings: {:?}", shim.warnings());
    let events = EventManager::new();
    shim.connect_to_events(&events);
    assert!(events.has_listeners("source-read"));

    let source = std::sync::Arc::new(std::sync::Mutex::new("before <<MARKER>> after".to_owned()));
    let _ = events.emit("source-read", &Event::SourceRead {
        docname: "index".into(), source: std::sync::Arc::clone(&source),
    });
    assert_eq!(*source.lock().expect("lock"), "before PYTHON-WAS-HERE after");
    assert_eq!(events.diagnostic_count(), 0, "{:?}", events.diagnostics());
}

#[test]
fn missing_python_degrades_without_panic() {
    let shim = PythonExtensionShim::start(ShimConfig::inline(
        "missing", "/no/such/interpreter", "pass",
    ));
    assert!(!shim.is_available());
    assert!(matches!(shim.call("initialize", &serde_json::json!({})), Err(ShimError::Unavailable { .. })));
    let events = EventManager::new();
    shim.connect_to_events(&events);
    assert!(events.diagnostic_count() == 0);
}

#[test]
fn malformed_worker_is_disabled_gracefully() {
    let Some(python) = find_python() else { return };
    let shim = PythonExtensionShim::start(ShimConfig::inline(
        "malformed", python, "print('not json'); print('still not json')",
    ));
    // The worker initializes to an empty event set (no valid responses), so it
    // is either disabled or inert; in neither case may it panic.
    let _ = shim.is_available();
    let events = EventManager::new();
    shim.connect_to_events(&events);
}

#[test]
fn bridge_from_config_wires_shim_and_directive() {
    let Some(python) = find_python() else { return };
    let mut config = Config::new();
    config.set("scribe_python", ConfigValue::from(python.to_string_lossy().into_owned()))
        .expect("registered config key");
    config.set("scribe_python_extensions", ConfigValue::List(vec![
        ConfigValue::from(fixture_script().to_string_lossy().into_owned()),
    ])).expect("registered config key");
    let bridge = InteropBridge::from_config(&config, Path::new("/tmp"));
    assert_eq!(bridge.shims.len(), 1);
    assert!(bridge.shims[0].is_available());
    assert!(bridge.events.has_listeners("source-read"));
    assert!(bridge.directive_names().iter().any(|name| name == "pybadge"));
}
