//! Event lifecycle bus (Phase 27).
//!
//! This module is the Rust counterpart of upstream's `sphinx.events.EventManager`
//! (`sphinx/sphinx/events.py`). It delivers the same observable semantics that
//! third-party extensions rely on:
//!
//! * a fixed set of core event names plus user-defined events ([`EventManager::add_event`]);
//! * connection ids with ascending-priority dispatch ([`EventManager::connect`]);
//! * explicit disconnection ([`EventManager::disconnect`]);
//! * [`EventManager::emit`], which calls every handler in `(priority, id)` order
//!   and returns each handler's result;
//! * [`EventManager::emit_firstresult`], which returns the first non-`None`
//!   result (upstream stops calling handlers at that point);
//! * **containment**: a handler that returns an error — or even unwinds — never
//!   aborts the build. The failure is recorded as a structured
//!   [`EventDiagnostic`] and an `Err` is returned in that handler's slot.
//!
//! Upstream passes a mutable *list* to `source-read` / `html-page-context`
//! handlers. The typed payloads here use [`SharedString`] / [`SharedJson`]
//! (interior-mutable `Arc<Mutex<…>>`) so handlers can mutate the source text
//! or page context exactly as upstream does, while the manager itself remains
//! `Send + Sync` and safely shareable across build threads.
//!
//! No frozen contract is touched: this is a brand-new, additive module.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use miette::Diagnostic;
use serde_json::Value as Json;
use thiserror::Error;

/// Shared, mutable string payload (`source-read`, `html-page-context`).
pub type SharedString = Arc<Mutex<String>>;
/// Shared, mutable JSON payload (`html-page-context` context dict, doctrees).
pub type SharedJson = Arc<Mutex<Json>>;
/// Opaque connection id returned by [`EventManager::connect`].
pub type ConnectionId = u64;

/// Result produced by an event handler.
///
/// `Ok(None)` mirrors upstream returning `None` (the common case for
/// notification events); `Ok(Some(value))` is the value consumed by
/// [`EventManager::emit_firstresult`].
pub type HandlerResult = Result<Option<Json>, HandlerError>;

/// Handler function object. Handlers receive an immutable view of the event;
/// payload fields that upstream mutates are interior-mutable.
pub type EventHandler = Arc<dyn Fn(&Event) -> HandlerResult + Send + Sync>;

/// Error returned by a single handler. The bus contains it as a diagnostic.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum HandlerError {
    /// The handler reported a failure with a human-readable message.
    #[error("{name}: {message}")]
    Failed {
        /// Event name the handler was attached to.
        name: String,
        /// Human-readable failure explanation.
        message: String,
    },
    /// The handler panicked; the bus caught the unwind.
    #[error("{name}: handler panicked")]
    Panicked {
        /// Event name the handler was attached to.
        name: String,
    },
}

impl HandlerError {
    /// Construct a `Failed` handler error with an event name and message.
    #[must_use]
    pub fn failed(name: &str, message: impl Into<String>) -> Self {
        Self::Failed { name: name.to_owned(), message: message.into() }
    }
}

/// A structured, miette-renderable diagnostic produced by a contained handler
/// failure (or an invalid bus operation).
#[derive(Clone, Debug, Error, Diagnostic, PartialEq, Eq)]
#[diagnostic(code(scribe::event::handler_failed))]
pub enum EventDiagnostic {
    /// A handler returned an error or panicked.
    #[error("event handler for `{event}` failed: {message}")]
    #[diagnostic(help("third-party extension handlers are contained; the build continues"))]
    HandlerFailed {
        /// Event being emitted.
        event: String,
        /// Contained failure message.
        message: String,
    },
    /// An operation targeted an event that has not been registered.
    #[error("unknown event: {event}")]
    #[diagnostic(help("register it with EventManager::add_event first"))]
    UnknownEvent {
        /// Unknown event name.
        event: String,
    },
    /// A caller attempted to register an event name twice.
    #[error("event already present: {event}")]
    EventAlreadyPresent {
        /// Duplicate event name.
        event: String,
    },
}

/// Typed payloads for the core Sphinx lifecycle events (plus a generic
/// [`Event::Custom`] carrier). Each variant exposes its canonical upstream
/// name through [`Event::name`].
#[derive(Clone, Debug)]
pub enum Event {
    /// `config-inited(config)` — emitted once after configuration is resolved.
    ConfigInited {
        /// Serializable snapshot of the effective configuration.
        config: Arc<Json>,
    },
    /// `builder-inited(app)` — emitted once before any document is read.
    BuilderInited {
        /// Builder name (`html`, `dirhtml`, …).
        builder_name: String,
    },
    /// `env-get-outdated(env, added, changed, removed) -> list[str]`.
    EnvGetOutdated {
        /// Newly discovered docnames.
        added: BTreeSet<String>,
        /// Changed docnames.
        changed: BTreeSet<String>,
        /// Removed docnames.
        removed: BTreeSet<String>,
    },
    /// `env-get-updated(env) -> list[str]`.
    EnvGetUpdated,
    /// `env-updated(env)`.
    EnvUpdated,
    /// `source-read(docname, source)` — `source` is mutated in place.
    SourceRead {
        /// Document being read.
        docname: String,
        /// Mutable source text.
        source: SharedString,
    },
    /// `doctree-read(doctree)` — emitted after a document is parsed.
    DoctreeRead {
        /// Document being read.
        docname: String,
        /// Serializable doctree snapshot.
        doctree: SharedJson,
    },
    /// `doctree-resolved(doctree, docname)` — emitted before writing.
    DoctreeResolved {
        /// Document being resolved.
        docname: String,
        /// Serializable doctree snapshot.
        doctree: SharedJson,
    },
    /// `missing-reference(env, node, contnode) -> reference | None`.
    MissingReference {
        /// Document containing the reference.
        docname: String,
        /// Reference target.
        target: String,
        /// Fallback content node text.
        contnode: String,
    },
    /// `warn-missing-reference(domain, node)`.
    WarnMissingReference {
        /// Owning domain.
        domain: String,
        /// Reference target.
        target: String,
    },
    /// `html-page-context(pagename, templatename, context, doctree)`.
    HtmlPageContext {
        /// Page/doc name.
        pagename: String,
        /// Mutable template name.
        templatename: SharedString,
        /// Mutable Jinja context (JSON object).
        context: SharedJson,
    },
    /// `write-done` — emitted once after all output files are written.
    WriteDone,
    /// `build-finished(exception)` — emitted once at the end of the build.
    BuildFinished {
        /// `false` when the build completed without a fatal error.
        succeeded: bool,
    },
    /// A user-defined event (see [`EventManager::add_event`]).
    Custom {
        /// Registered event name.
        name: String,
        /// Opaque JSON payload.
        payload: SharedJson,
    },
}

impl Event {
    /// Canonical upstream event name for this payload.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::ConfigInited { .. } => "config-inited",
            Self::BuilderInited { .. } => "builder-inited",
            Self::EnvGetOutdated { .. } => "env-get-outdated",
            Self::EnvGetUpdated => "env-get-updated",
            Self::EnvUpdated => "env-updated",
            Self::SourceRead { .. } => "source-read",
            Self::DoctreeRead { .. } => "doctree-read",
            Self::DoctreeResolved { .. } => "doctree-resolved",
            Self::MissingReference { .. } => "missing-reference",
            Self::WarnMissingReference { .. } => "warn-missing-reference",
            Self::HtmlPageContext { .. } => "html-page-context",
            Self::WriteDone => "write-done",
            Self::BuildFinished { .. } => "build-finished",
            Self::Custom { name, .. } => name,
        }
    }

    /// JSON representation of the payload, used by the Python/Wasm bridges.
    #[must_use]
    pub fn to_json(&self) -> Json {
        let stringify_set = |set: &BTreeSet<String>| -> Json {
            Json::Array(set.iter().cloned().map(Json::String).collect())
        };
        match self {
            Self::ConfigInited { config } => serde_json::json!({ "config": (**config).clone() }),
            Self::BuilderInited { builder_name } => serde_json::json!({ "builder_name": builder_name }),
            Self::EnvGetOutdated { added, changed, removed } => serde_json::json!({
                "added": stringify_set(added),
                "changed": stringify_set(changed),
                "removed": stringify_set(removed),
            }),
            Self::EnvGetUpdated | Self::EnvUpdated | Self::WriteDone => serde_json::json!({}),
            Self::SourceRead { docname, source } => serde_json::json!({
                "docname": docname,
                "source": lock_string(source),
            }),
            Self::DoctreeRead { docname, doctree } | Self::DoctreeResolved { docname, doctree } => {
                serde_json::json!({ "docname": docname, "doctree": lock_json(doctree) })
            }
            Self::MissingReference { docname, target, contnode } => serde_json::json!({
                "docname": docname, "target": target, "contnode": contnode,
            }),
            Self::WarnMissingReference { domain, target } => serde_json::json!({
                "domain": domain, "target": target,
            }),
            Self::HtmlPageContext { pagename, templatename, context } => serde_json::json!({
                "pagename": pagename,
                "templatename": lock_string(templatename),
                "context": lock_json(context),
            }),
            Self::BuildFinished { succeeded } => serde_json::json!({ "succeeded": succeeded }),
            Self::Custom { name, payload } => serde_json::json!({
                "name": name, "payload": lock_json(payload),
            }),
        }
    }
}

fn lock_string(value: &SharedString) -> String {
    value.lock().map(|guard| guard.clone()).unwrap_or_default()
}

fn lock_json(value: &SharedJson) -> Json {
    value.lock().map(|guard| guard.clone()).unwrap_or(Json::Null)
}

/// Core event names, transcribed from upstream `core_events`.
pub const CORE_EVENTS: &[&str] = &[
    "config-inited",
    "builder-inited",
    "env-get-outdated",
    "env-before-read-docs",
    "env-purge-doc",
    "source-read",
    "include-read",
    "doctree-read",
    "env-merge-info",
    "env-updated",
    "env-get-updated",
    "env-check-consistency",
    "write-started",
    "doctree-resolved",
    "missing-reference",
    "warn-missing-reference",
    "html-page-context",
    "build-finished",
    "write-done",
];

/// Default handler priority (upstream Sphinx uses 500).
pub const DEFAULT_PRIORITY: i32 = 500;

#[derive(Clone)]
struct Listener {
    id: ConnectionId,
    event: String,
    priority: i32,
    handler: EventHandler,
}

#[derive(Default)]
struct BusState {
    events: BTreeSet<String>,
    listeners: BTreeMap<String, Vec<Listener>>,
    diagnostics: Vec<EventDiagnostic>,
}

/// A cloneable, thread-safe event bus.
///
/// Clones share the same underlying registry and diagnostics log, so a manager
/// built in the CLI can be handed to a builder and both observe the same
/// handler set.
#[derive(Clone, Default)]
pub struct EventManager {
    state: Arc<RwLock<BusState>>,
    next_id: Arc<AtomicU64>,
}

impl fmt::Debug for EventManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.read_state();
        f.debug_struct("EventManager")
            .field("events", &state.events)
            .field("handler_count", &self.listener_count())
            .field("diagnostics", &state.diagnostics.len())
            .finish()
    }
}

impl PartialEq for EventManager {
    fn eq(&self, other: &Self) -> bool {
        self.listener_count() == other.listener_count()
            && self.diagnostic_count() == other.diagnostic_count()
    }
}
impl Eq for EventManager {}

impl EventManager {
    /// Create a manager pre-registered with upstream's core event names.
    #[must_use]
    pub fn new() -> Self {
        let manager = Self::default();
        {
            let mut state = manager.write_state();
            for name in CORE_EVENTS {
                state.events.insert((*name).to_owned());
            }
        }
        manager
    }

    /// Register a custom event. Returns an error when the name already exists,
    /// mirroring upstream's `ExtensionError: Event already present`.
    pub fn add_event(&self, name: impl Into<String>) -> Result<(), EventDiagnostic> {
        let name = name.into();
        let mut state = self.write_state();
        if state.events.contains(&name) {
            let diagnostic = EventDiagnostic::EventAlreadyPresent { event: name };
            state.diagnostics.push(diagnostic.clone());
            return Err(diagnostic);
        }
        state.events.insert(name);
        Ok(())
    }

    /// Register a handler for `name` with an explicit priority.
    ///
    /// Lower numerical priorities run first (upstream semantics). Handler ids
    /// are monotonic and break ties deterministically by registration order.
    pub fn connect(
        &self,
        name: &str,
        handler: EventHandler,
        priority: i32,
    ) -> Result<ConnectionId, EventDiagnostic> {
        let mut state = self.write_state();
        if !state.events.contains(name) {
            let diagnostic = EventDiagnostic::UnknownEvent { event: name.to_owned() };
            state.diagnostics.push(diagnostic.clone());
            return Err(diagnostic);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        state.listeners.entry(name.to_owned()).or_default().push(Listener {
            id,
            event: name.to_owned(),
            priority,
            handler,
        });
        Ok(id)
    }

    /// Convenience wrapper using [`DEFAULT_PRIORITY`].
    pub fn connect_default(
        &self,
        name: &str,
        handler: EventHandler,
    ) -> Result<ConnectionId, EventDiagnostic> {
        self.connect(name, handler, DEFAULT_PRIORITY)
    }

    /// Remove the handler identified by `id`. Returns `true` when a handler
    /// was removed. Upstream's `disconnect` silently ignores unknown ids.
    pub fn disconnect(&self, id: ConnectionId) -> bool {
        let mut state = self.write_state();
        let mut removed = false;
        for listeners in state.listeners.values_mut() {
            let before = listeners.len();
            listeners.retain(|listener| listener.id != id);
            removed |= listeners.len() != before;
        }
        removed
    }

    /// True when at least one handler is connected to `name`.
    #[must_use]
    pub fn has_listeners(&self, name: &str) -> bool {
        self.read_state()
            .listeners
            .get(name)
            .is_some_and(|listeners| !listeners.is_empty())
    }

    /// Total number of connected handlers.
    #[must_use]
    pub fn listener_count(&self) -> usize {
        self.read_state().listeners.values().map(Vec::len).sum()
    }

    /// Number of contained diagnostics recorded so far.
    #[must_use]
    pub fn diagnostic_count(&self) -> usize {
        self.read_state().diagnostics.len()
    }

    /// Drain recorded diagnostics (used by the CLI to surface plugin failures).
    #[must_use]
    pub fn take_diagnostics(&self) -> Vec<EventDiagnostic> {
        std::mem::take(&mut self.write_state().diagnostics)
    }

    /// Peek at recorded diagnostics without draining them.
    #[must_use]
    pub fn diagnostics(&self) -> Vec<EventDiagnostic> {
        self.read_state().diagnostics.clone()
    }

    /// Emit an event, calling handlers in ascending `(priority, id)` order.
    ///
    /// Every handler is invoked; a single failure neither aborts dispatch nor
    /// panics the process. Failed slots contain an `Err(HandlerError)` and a
    /// [`EventDiagnostic`] is recorded.
    #[must_use]
    pub fn emit(&self, name: &str, event: &Event) -> Vec<HandlerResult> {
        let name = event.name_if_placeholder(name);
        let handlers = {
            let state = self.read_state();
            let mut listeners: Vec<Listener> = state
                .listeners
                .get(name)
                .cloned()
                .unwrap_or_default();
            listeners.sort_by(|a, b| a.priority.cmp(&b.priority).then(a.id.cmp(&b.id)));
            listeners
        };
        let mut results = Vec::with_capacity(handlers.len());
        for listener in handlers {
            let result = self.invoke(&listener, event);
            if let Err(error) = &result {
                self.push_diagnostic(EventDiagnostic::HandlerFailed {
                    event: name.to_owned(),
                    message: error.to_string(),
                });
            }
            results.push(result);
        }
        results
    }

    /// Emit an event and return the first non-`None` handler result.
    ///
    /// Handlers are still visited in priority order, but dispatch stops at the
    /// first `Ok(Some(..))`, matching upstream `emit_firstresult`.
    #[must_use]
    pub fn emit_firstresult(&self, name: &str, event: &Event) -> Option<Json> {
        let name = event.name_if_placeholder(name);
        let handlers = {
            let state = self.read_state();
            let mut listeners: Vec<Listener> = state
                .listeners
                .get(name)
                .cloned()
                .unwrap_or_default();
            listeners.sort_by(|a, b| a.priority.cmp(&b.priority).then(a.id.cmp(&b.id)));
            listeners
        };
        for listener in handlers {
            let result = self.invoke(&listener, event);
            match result {
                Ok(Some(value)) => return Some(value),
                Ok(None) => {}
                Err(error) => self.push_diagnostic(EventDiagnostic::HandlerFailed {
                    event: name.to_owned(),
                    message: error.to_string(),
                }),
            }
        }
        None
    }

    fn invoke(&self, listener: &Listener, event: &Event) -> HandlerResult {
        let name = listener.event.clone();
        // Contain panics from third-party handlers so a misbehaving plugin can
        // never abort scribe's build. `AssertUnwindSafe` is justified because
        // the bus state is protected by RwLock and handlers only touch shared
        // payloads through their own mutexes.
        match catch_unwind(AssertUnwindSafe(|| (listener.handler)(event))) {
            Ok(result) => result,
            Err(_) => Err(HandlerError::Panicked { name }),
        }
    }

    fn push_diagnostic(&self, diagnostic: EventDiagnostic) {
        self.write_state().diagnostics.push(diagnostic);
    }

    fn read_state(&self) -> std::sync::RwLockReadGuard<'_, BusState> {
        // The lock is only ever held for short, non-reentrant sections. A
        // poisoned lock (only possible if a panic escaped a critical section,
        // which all call sites avoid) is recovered rather than propagated.
        self.state.read().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write_state(&self) -> std::sync::RwLockWriteGuard<'_, BusState> {
        self.state.write().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Event {
    /// Use the payload's own name for custom events while trusting the caller
    /// for core events (they must agree; `emit` is always called with
    /// `event.name()` by the build pipeline).
    fn name_if_placeholder<'a>(&'a self, requested: &'a str) -> &'a str {
        match self {
            Self::Custom { .. } => self.name(),
            _ => requested,
        }
    }
}

/// Convenience: build a handler closure from a plain function.
#[must_use]
pub fn handler<F>(callback: F) -> EventHandler
where
    F: Fn(&Event) -> HandlerResult + Send + Sync + 'static,
{
    Arc::new(callback)
}

/// Convenience: a handler that always succeeds with `None` (useful for tests
/// that only need to observe that an event fired).
#[must_use]
pub fn recording_handler(seen: Arc<Mutex<Vec<String>>>) -> EventHandler {
    handler(move |event| {
        if let Ok(mut list) = seen.lock() {
            list.push(event.name().to_owned());
        }
        Ok(None)
    })
}

/// Construct a successful `Some(value)` handler result.
pub fn value_result(value: impl serde::Serialize) -> HandlerResult {
    serde_json::to_value(value)
        .map(Some)
        .map_err(|error| HandlerError::failed("emit_firstresult", error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connect_ok(manager: &EventManager, name: &str, priority: i32, seen: Arc<Mutex<Vec<String>>>, tag: &str) -> ConnectionId {
        let tag = tag.to_owned();
        manager
            .connect(name, handler(move |event| {
                if let Ok(mut list) = seen.lock() {
                    list.push(format!("{tag}:{}", event.name()));
                }
                Ok(None)
            }), priority)
            .expect("valid event name")
    }

    #[test]
    fn core_events_are_registered_and_custom_events_can_be_added() {
        let manager = EventManager::new();
        for name in CORE_EVENTS {
            assert!(manager.add_event(*name).is_err(), "{name} must already exist");
        }
        manager.add_event("my-plugin-event").expect("new event");
        assert!(manager.add_event("my-plugin-event").is_err());
        assert_eq!(manager.diagnostic_count(), CORE_EVENTS.len() + 1);
    }

    #[test]
    fn handlers_run_in_priority_then_registration_order() {
        let manager = EventManager::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        connect_ok(&manager, "build-finished", DEFAULT_PRIORITY, Arc::clone(&seen), "b");
        connect_ok(&manager, "build-finished", 100, Arc::clone(&seen), "a");
        connect_ok(&manager, "build-finished", 100, Arc::clone(&seen), "c");
        let _ = manager.emit("build-finished", &Event::BuildFinished { succeeded: true });
        assert_eq!(
            seen.lock().expect("lock").as_slice(),
            ["a:build-finished", "c:build-finished", "b:build-finished"]
        );
    }

    #[test]
    fn disconnect_removes_only_the_requested_handler() {
        let manager = EventManager::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let first = connect_ok(&manager, "builder-inited", 100, Arc::clone(&seen), "first");
        let second = connect_ok(&manager, "builder-inited", 200, Arc::clone(&seen), "second");
        assert!(manager.disconnect(first));
        assert!(!manager.disconnect(first));
        let _ = manager.emit("builder-inited", &Event::BuilderInited { builder_name: "html".into() });
        assert_eq!(seen.lock().expect("lock").as_slice(), ["second:builder-inited"]);
        assert!(manager.disconnect(second));
        assert_eq!(manager.listener_count(), 0);
    }

    #[test]
    fn emit_firstresult_stops_at_first_non_none_in_priority_order() {
        let manager = EventManager::new();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let calls2 = Arc::clone(&calls);
        manager.connect("missing-reference", handler(move |_| {
            calls2.lock().expect("lock").push("low");
            value_result("from-low")
        }), 100).expect("connect");
        let calls3 = Arc::clone(&calls);
        manager.connect("missing-reference", handler(move |_| {
            calls3.lock().expect("lock").push("high");
            value_result("from-high")
        }), 900).expect("connect");
        let result = manager.emit_firstresult("missing-reference", &Event::MissingReference {
            docname: "index".into(), target: "x".into(), contnode: "x".into(),
        });
        assert_eq!(result, Some(Json::String("from-low".into())));
        assert_eq!(calls.lock().expect("lock").as_slice(), ["low"], "dispatch must stop early");
    }

    #[test]
    fn handler_errors_are_contained_as_diagnostics() {
        let manager = EventManager::new();
        manager.connect("source-read", handler(move |_| {
            Err(HandlerError::failed("source-read", "boom"))
        }), DEFAULT_PRIORITY).expect("connect");
        let seen = Arc::new(Mutex::new(false));
        let seen2 = Arc::clone(&seen);
        manager.connect("source-read", handler(move |_| {
            *seen2.lock().expect("lock") = true;
            Ok(None)
        }), 900).expect("connect");
        let results = manager.emit("source-read", &Event::SourceRead {
            docname: "index".into(), source: Arc::new(Mutex::new("text".into())),
        });
        assert_eq!(results.len(), 2);
        assert!(results[0].is_err());
        assert!(results[1].is_ok());
        assert!(*seen.lock().expect("lock"), "later handlers still run");
        assert!(manager.diagnostics()[0].to_string().contains("boom"));
    }

    #[test]
    fn panicking_handler_is_caught_and_diagnosed() {
        let manager = EventManager::new();
        manager.connect("doctree-read", handler(move |_| -> HandlerResult {
            panic!("plugin panic")
        }), DEFAULT_PRIORITY).expect("connect");
        let results = manager.emit("doctree-read", &Event::DoctreeRead {
            docname: "index".into(), doctree: Arc::new(Mutex::new(Json::Null)),
        });
        assert!(matches!(results[0], Err(HandlerError::Panicked { .. })));
        assert_eq!(manager.diagnostic_count(), 1);
    }

    #[test]
    fn source_read_handler_can_mutate_the_shared_source() {
        let manager = EventManager::new();
        manager.connect("source-read", handler(move |event| {
            if let Event::SourceRead { source, .. } = event {
                let mut guard = source.lock().expect("lock");
                *guard = guard.replace("OLD", "NEW");
            }
            Ok(None)
        }), DEFAULT_PRIORITY).expect("connect");
        let source = Arc::new(Mutex::new("before OLD after".to_owned()));
        let _ = manager.emit("source-read", &Event::SourceRead {
            docname: "index".into(), source: Arc::clone(&source),
        });
        assert_eq!(source.lock().expect("lock").as_str(), "before NEW after");
    }

    #[test]
    fn unknown_event_connect_is_reported_not_panicked() {
        let manager = EventManager::new();
        assert!(manager.connect("does-not-exist", handler(|_| Ok(None)), DEFAULT_PRIORITY).is_err());
        assert!(!manager.has_listeners("does-not-exist"));
        assert_eq!(manager.diagnostic_count(), 1);
    }

    #[test]
    fn custom_event_payload_roundtrips_through_json() {
        let manager = EventManager::new();
        manager.add_event("plugin-event").expect("new event");
        let payload = Arc::new(Mutex::new(serde_json::json!({ "answer": 42 })));
        manager.connect("plugin-event", handler(move |event| {
            assert_eq!(event.name(), "plugin-event");
            value_result(event.to_json())
        }), DEFAULT_PRIORITY).expect("connect");
        let result = manager.emit_firstresult("plugin-event", &Event::Custom {
            name: "plugin-event".into(), payload: Arc::clone(&payload),
        });
        assert_eq!(result, Some(serde_json::json!({ "name": "plugin-event", "payload": { "answer": 42 } })));
    }
}
