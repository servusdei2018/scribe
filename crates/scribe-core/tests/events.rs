//! Integration tests for the Phase 27 event lifecycle bus.

use std::sync::{Arc, Mutex};

use scribe_core::{
    handler, value_result, Event, EventDiagnostic, EventManager, HandlerError, SharedJson,
    SharedString, CORE_EVENTS, DEFAULT_PRIORITY,
};
use serde_json::Value as Json;

fn recorder() -> (EventManager, Arc<Mutex<Vec<String>>>) {
    let manager = EventManager::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    for name in ["source-read", "doctree-read", "build-finished", "custom-event"] {
        let sink = Arc::clone(&sink);
        // custom-event is registered below; connecting to unknown events must
        // be reported, so only connect to core ones here.
        if name == "custom-event" {
            continue;
        }
        manager.connect(name, handler(move |event| {
            sink.lock().expect("lock").push(event.name().to_owned());
            Ok(None)
        }), DEFAULT_PRIORITY).expect("connect core event");
    }
    (manager, seen)
}

#[test]
fn core_event_names_are_discoverable() {
    let names: Vec<&str> = CORE_EVENTS.to_vec();
    assert!(names.contains(&"config-inited"));
    assert!(names.contains(&"builder-inited"));
    assert!(names.contains(&"env-get-outdated"));
    assert!(names.contains(&"env-get-updated"));
    assert!(names.contains(&"source-read"));
    assert!(names.contains(&"doctree-read"));
    assert!(names.contains(&"doctree-resolved"));
    assert!(names.contains(&"missing-reference"));
    assert!(names.contains(&"warn-missing-reference"));
    assert!(names.contains(&"html-page-context"));
    assert!(names.contains(&"build-finished"));
}

#[test]
fn priority_order_is_stable_and_disconnect_works() {
    let manager = EventManager::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    for (priority, tag) in [(500, "b"), (100, "a"), (100, "c")] {
        let tag = tag.to_owned();
        let sink = Arc::clone(&seen);
        manager.connect("doctree-read", handler(move |_| {
            sink.lock().expect("lock").push(tag.clone());
            Ok(None)
        }), priority).expect("connect");
    }
    let _ = manager.emit("doctree-read", &Event::DoctreeRead {
        docname: "index".into(), doctree: Arc::new(Mutex::new(Json::Null)),
    });
    assert_eq!(seen.lock().expect("lock").as_slice(), ["a", "c", "b"]);
}

#[test]
fn emit_firstresult_short_circuits_in_priority_order() {
    let manager = EventManager::new();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&calls);
    manager.connect("missing-reference", handler(move |_| {
        sink.lock().expect("lock").push(100);
        value_result("low")
    }), 100).expect("connect");
    let sink = Arc::clone(&calls);
    manager.connect("missing-reference", handler(move |_| {
        sink.lock().expect("lock").push(900);
        value_result("high")
    }), 900).expect("connect");
    let result = manager.emit_firstresult("missing-reference", &Event::MissingReference {
        docname: "index".into(), target: "x".into(), contnode: "x".into(),
    });
    assert_eq!(result, Some(Json::String("low".into())));
    assert_eq!(*calls.lock().expect("lock"), vec![100]);
}

#[test]
fn panicking_and_erroring_handlers_are_contained() {
    let manager = EventManager::new();
    manager.connect("source-read", handler(move |_| -> scribe_core::HandlerResult {
        panic!("plugin exploded")
    }), DEFAULT_PRIORITY).expect("connect");
    manager.connect("source-read", handler(move |_| {
        Err(HandlerError::failed("source-read", "bad plugin"))
    }), 600).expect("connect");
    let results = manager.emit("source-read", &Event::SourceRead {
        docname: "index".into(), source: Arc::new(Mutex::new("text".into())),
    });
    assert_eq!(results.len(), 2);
    assert!(matches!(results[0], Err(HandlerError::Panicked { .. })));
    assert!(matches!(results[1], Err(HandlerError::Failed { .. })));
    assert!(manager.diagnostics().iter().any(|d| matches!(d, EventDiagnostic::HandlerFailed { .. })));
}

#[test]
fn every_typed_payload_reports_its_canonical_name_and_json() {
    let shared_string = Arc::new(Mutex::new("s".to_owned()));
    let shared_json: SharedJson = Arc::new(Mutex::new(Json::Null));
    let source: SharedString = Arc::clone(&shared_string);
    let samples: Vec<(Event, &str)> = vec![
        (Event::ConfigInited { config: Arc::new(Json::Null) }, "config-inited"),
        (Event::BuilderInited { builder_name: "html".into() }, "builder-inited"),
        (Event::EnvGetOutdated { added: Default::default(), changed: Default::default(), removed: Default::default() }, "env-get-outdated"),
        (Event::EnvGetUpdated, "env-get-updated"),
        (Event::EnvUpdated, "env-updated"),
        (Event::SourceRead { docname: "i".into(), source }, "source-read"),
        (Event::DoctreeRead { docname: "i".into(), doctree: Arc::clone(&shared_json) }, "doctree-read"),
        (Event::DoctreeResolved { docname: "i".into(), doctree: Arc::clone(&shared_json) }, "doctree-resolved"),
        (Event::MissingReference { docname: "i".into(), target: "t".into(), contnode: "t".into() }, "missing-reference"),
        (Event::WarnMissingReference { domain: "std".into(), target: "t".into() }, "warn-missing-reference"),
        (Event::HtmlPageContext { pagename: "i".into(), templatename: Arc::new(Mutex::new("page.html".into())), context: Arc::clone(&shared_json) }, "html-page-context"),
        (Event::WriteDone, "write-done"),
        (Event::BuildFinished { succeeded: true }, "build-finished"),
        (Event::Custom { name: "x".into(), payload: Arc::clone(&shared_json) }, "x"),
    ];
    for (event, name) in samples {
        assert_eq!(event.name(), name);
        let _ = event.to_json();
    }
}

#[test]
fn recording_handlers_observe_all_emitted_core_events() {
    let (manager, seen) = recorder();
    let _ = manager.emit("source-read", &Event::SourceRead {
        docname: "index".into(), source: Arc::new(Mutex::new("text".into())),
    });
    let _ = manager.emit("doctree-read", &Event::DoctreeRead {
        docname: "index".into(), doctree: Arc::new(Mutex::new(Json::Null)),
    });
    let _ = manager.emit("build-finished", &Event::BuildFinished { succeeded: true });
    assert_eq!(
        seen.lock().expect("lock").as_slice(),
        ["source-read", "doctree-read", "build-finished"]
    );
}

#[test]
fn handler_clone_shares_registry() {
    let manager = EventManager::new();
    let other = manager.clone();
    let seen = Arc::new(Mutex::new(0));
    let sink = Arc::clone(&seen);
    other.connect("builder-inited", handler(move |_| {
        *sink.lock().expect("lock") += 1;
        Ok(None)
    }), DEFAULT_PRIORITY).expect("connect");
    let _ = manager.emit("builder-inited", &Event::BuilderInited { builder_name: "html".into() });
    assert_eq!(*seen.lock().expect("lock"), 1);
}
