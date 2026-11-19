//! Python extension interoperability shim (Phase 27).
//!
//! Bridges third-party Python Sphinx extensions into scribe's Rust build. A
//! configured Python worker is launched as a subprocess and spoken to over
//! **line-delimited JSON-RPC 2.0** (the same family of protocols used by the
//! Phase 14/21/22 worker bridges). The worker declares which build events it
//! subscribes to and which directives/roles it provides during `initialize`;
//! the host then wires those into the [`EventManager`] and
//! [`DirectiveRegistry`](scribe_rst::DirectiveRegistry).
//!
//! # Protocol (one JSON object per line)
//!
//! Host → worker:
//! `{"jsonrpc":"2.0","id":N,"method":"initialize"|"on_event"|"directive"|"role"|"shutdown","params":{…}}`
//!
//! Worker → host:
//! `{"jsonrpc":"2.0","id":N,"result":…}` or
//! `{"jsonrpc":"2.0","id":N,"error":{"message":…}}`
//!
//! `initialize` returns `{"events":[...],"directives":[...],"roles":[...]}`.
//! `on_event` receives `{"event":"source-read","payload":{…}}` and may return
//! `{"source":"..."}` to mutate the source in place (upstream's `source-read`
//! contract). `directive` receives `{"name", "arguments", "content",
//! "options"}` and returns `{"text": "...", "classes": [...]}`.
//!
//! The build **never depends on Python being present**: a missing interpreter,
//! a worker that exits, or malformed JSON simply disables the shim and records
//! a warning. No panic can escape.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};

use miette::Diagnostic;
use serde_json::Value as Json;
use thiserror::Error;

use miette::SourceSpan;
use scribe_ast::{NodeId, NodeKind};
use scribe_rst::{Directive, DirectiveContext, DirectiveError, OptionSpec};

use crate::events::{Event, EventManager, HandlerResult, HandlerError, SharedString, DEFAULT_PRIORITY};

/// Failures raised by the Python shim. They degrade the build gracefully.
#[derive(Clone, Debug, Error, Diagnostic, PartialEq, Eq)]
pub enum ShimError {
    /// No usable Python worker is running.
    #[error("python extension shim `{name}` is unavailable: {reason}")]
    #[diagnostic(code(scribe::python_shim::unavailable))]
    Unavailable {
        /// Shim name.
        name: String,
        /// Why it is unavailable.
        reason: String,
    },
    /// The worker could not be spawned.
    #[error("failed to launch python worker for `{name}`: {detail}")]
    #[diagnostic(code(scribe::python_shim::spawn_failed))]
    SpawnFailed {
        /// Shim name.
        name: String,
        /// I/O detail.
        detail: String,
    },
    /// The worker exited or its pipe broke.
    #[error("python worker `{name}` exited unexpectedly")]
    #[diagnostic(code(scribe::python_shim::worker_exited))]
    WorkerExited {
        /// Shim name.
        name: String,
    },
    /// The worker returned malformed or out-of-protocol JSON.
    #[error("python worker `{name}` returned invalid protocol data: {detail}")]
    #[diagnostic(code(scribe::python_shim::protocol))]
    Protocol {
        /// Shim name.
        name: String,
        /// Detail.
        detail: String,
    },
    /// The worker reported a JSON-RPC error.
    #[error("python worker `{name}` error: {message}")]
    #[diagnostic(code(scribe::python_shim::worker_error))]
    WorkerError {
        /// Shim name.
        name: String,
        /// Error message.
        message: String,
    },
}

/// How the Python worker source is supplied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PyScript {
    /// Inline program passed to `python -c`.
    Inline(String),
    /// Path to a Python script passed as the first argument.
    Path(PathBuf),
}

/// Configuration for one Python extension worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShimConfig {
    /// Human-readable shim name (used in diagnostics).
    pub name: String,
    /// Python interpreter (`python3`, absolute path, …).
    pub interpreter: PathBuf,
    /// Worker source.
    pub script: PyScript,
}

impl ShimConfig {
    /// Configuration for an inline `-c` worker.
    #[must_use]
    pub fn inline(name: impl Into<String>, interpreter: impl Into<PathBuf>, script: impl Into<String>) -> Self {
        Self { name: name.into(), interpreter: interpreter.into(), script: PyScript::Inline(script.into()) }
    }

    /// Configuration for a script-path worker.
    #[must_use]
    pub fn path(name: impl Into<String>, interpreter: impl Into<PathBuf>, path: impl Into<PathBuf>) -> Self {
        Self { name: name.into(), interpreter: interpreter.into(), script: PyScript::Path(path.into()) }
    }
}

struct Worker {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

/// Owns the worker and shuts it down when the *last* shim clone is dropped.
/// (A `Drop` impl on [`PythonExtensionShim`] itself would fire on every clone.)
struct WorkerGuard {
    worker: Option<Worker>,
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.as_mut() {
            worker.shutdown();
        }
    }
}

impl Worker {
    fn spawn(config: &ShimConfig) -> Result<Self, ShimError> {
        let mut command = Command::new(&config.interpreter);
        match &config.script {
            PyScript::Inline(script) => {
                command.arg("-c").arg(script);
            }
            PyScript::Path(path) => {
                command.arg(path);
            }
        }
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = command.spawn().map_err(|error| ShimError::SpawnFailed {
            name: config.name.clone(),
            detail: error.to_string(),
        })?;
        let stdin = child.stdin.take().ok_or_else(|| ShimError::SpawnFailed {
            name: config.name.clone(),
            detail: "no stdin pipe".to_owned(),
        })?;
        let stdout = child.stdout.take().ok_or_else(|| ShimError::SpawnFailed {
            name: config.name.clone(),
            detail: "no stdout pipe".to_owned(),
        })?;
        Ok(Self { child, stdin: BufWriter::new(stdin), stdout: BufReader::new(stdout), next_id: 1 })
    }

    fn call(&mut self, name: &str, method: &str, params: &Json) -> Result<Json, ShimError> {
        let id = self.next_id;
        self.next_id += 1;
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let mut line = serde_json::to_string(&request).map_err(|error| ShimError::Protocol {
            name: name.to_owned(), detail: error.to_string(),
        })?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|()| self.stdin.flush())
            .map_err(|_| ShimError::WorkerExited { name: name.to_owned() })?;

        // Skip non-protocol lines (e.g. a worker printing diagnostics to
        // stdout) and responses for other ids, but bound the scan so a chatty
        // worker cannot hang the build.
        for _ in 0..128 {
            let mut response = String::new();
            let read = self
                .stdout
                .read_line(&mut response)
                .map_err(|_| ShimError::WorkerExited { name: name.to_owned() })?;
            if read == 0 {
                return Err(ShimError::WorkerExited { name: name.to_owned() });
            }
            let value: Json = match serde_json::from_str(response.trim()) {
                Ok(value) => value,
                Err(_) => continue,
            };
            if value.get("id").and_then(Json::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = value.get("error") {
                let message = error.get("message").and_then(Json::as_str).unwrap_or("unknown error");
                return Err(ShimError::WorkerError { name: name.to_owned(), message: message.to_owned() });
            }
            return Ok(value.get("result").cloned().unwrap_or(Json::Null));
        }
        Err(ShimError::Protocol {
            name: name.to_owned(),
            detail: "no matching response after 128 lines".to_owned(),
        })
    }

    fn shutdown(&mut self) {
        let id = self.next_id;
        self.next_id += 1;
        let request = serde_json::json!({"jsonrpc":"2.0","id":id,"method":"shutdown","params":{}});
        if let Ok(mut line) = serde_json::to_string(&request) {
            line.push('\n');
            let _ = self.stdin.write_all(line.as_bytes()).and_then(|()| self.stdin.flush());
        }
        let _ = self.child.wait();
    }
}

/// A running Python extension shim. Cloneable; all clones share the worker.
#[derive(Clone)]
pub struct PythonExtensionShim {
    config: Arc<ShimConfig>,
    worker: Arc<Mutex<WorkerGuard>>,
    events: Arc<Mutex<BTreeSet<String>>>,
    directives: Arc<Mutex<BTreeSet<String>>>,
    warnings: Arc<Mutex<Vec<String>>>,
}

impl std::fmt::Debug for PythonExtensionShim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PythonExtensionShim")
            .field("name", &self.config.name)
            .field("available", &self.is_available())
            .field("events", &self.events())
            .field("directives", &self.directives())
            .finish()
    }
}

impl PythonExtensionShim {
    /// Start (or attempt to start) a Python extension worker.
    ///
    /// A missing interpreter or a worker that fails to initialize yields a
    /// disabled shim with a recorded warning — never a panic or a failed build.
    #[must_use]
    pub fn start(config: ShimConfig) -> Self {
        let events = Arc::new(Mutex::new(BTreeSet::new()));
        let directives = Arc::new(Mutex::new(BTreeSet::new()));
        let warnings = Arc::new(Mutex::new(Vec::new()));
        let worker = match Worker::spawn(&config) {
            Ok(worker) => Some(worker),
            Err(error) => {
                if let Ok(mut list) = warnings.lock() {
                    list.push(error.to_string());
                }
                None
            }
        };
        let shim = Self {
            config: Arc::new(config),
            worker: Arc::new(Mutex::new(WorkerGuard { worker })),
            events,
            directives,
            warnings,
        };
        if shim.is_available() {
            if let Err(error) = shim.initialize() {
                shim.warn(&error.to_string());
                if let Ok(mut guard) = shim.worker.lock() {
                    guard.worker = None;
                }
            }
        }
        shim
    }

    /// Whether a live worker is available.
    #[must_use]
    pub fn is_available(&self) -> bool {
        self.worker.lock().map(|guard| guard.worker.is_some()).unwrap_or(false)
    }

    /// Shim name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.config.name
    }

    /// Events the worker declared during initialization.
    #[must_use]
    pub fn events(&self) -> Vec<String> {
        self.events.lock().map(|set| set.iter().cloned().collect()).unwrap_or_default()
    }

    /// Directives the worker declared during initialization.
    #[must_use]
    pub fn directives(&self) -> Vec<String> {
        self.directives.lock().map(|set| set.iter().cloned().collect()).unwrap_or_default()
    }

    /// Non-fatal warnings accumulated by this shim.
    #[must_use]
    pub fn warnings(&self) -> Vec<String> {
        self.warnings.lock().map(|list| list.clone()).unwrap_or_default()
    }

    fn warn(&self, message: &str) {
        if let Ok(mut list) = self.warnings.lock() {
            list.push(message.to_owned());
        }
    }

    fn initialize(&self) -> Result<(), ShimError> {
        let result = self.call("initialize", &serde_json::json!({}))?;
        if let Some(events) = result.get("events").and_then(Json::as_array) {
            if let Ok(mut set) = self.events.lock().map_err(|_| ShimError::Protocol {
                name: self.config.name.clone(), detail: "event lock poisoned".to_owned(),
            }) {
                for event in events.iter().filter_map(Json::as_str) {
                    set.insert(event.to_owned());
                }
            }
        }
        if let Some(directives) = result.get("directives").and_then(Json::as_array) {
            if let Ok(mut set) = self.directives.lock().map_err(|_| ShimError::Protocol {
                name: self.config.name.clone(), detail: "directive lock poisoned".to_owned(),
            }) {
                for directive in directives.iter().filter_map(Json::as_str) {
                    set.insert(directive.to_owned());
                }
            }
        }
        Ok(())
    }

    /// Perform a JSON-RPC call against the worker.
    pub fn call(&self, method: &str, params: &Json) -> Result<Json, ShimError> {
        let mut guard = self.worker.lock().map_err(|_| ShimError::Unavailable {
            name: self.config.name.clone(), reason: "worker lock poisoned".to_owned(),
        })?;
        let worker = guard.worker.as_mut().ok_or_else(|| ShimError::Unavailable {
            name: self.config.name.clone(), reason: "worker not running".to_owned(),
        })?;
        worker.call(&self.config.name, method, params)
    }

    /// Register this shim's event subscriptions on `events`.
    pub fn connect_to_events(&self, events: &EventManager) {
        for event_name in self.events() {
            // Unknown/custom events are ignored gracefully: upstream extensions
            // may subscribe to events scribe has not implemented yet.
            let shim = self.clone();
            let handler = crate::events::handler(move |event: &Event| -> HandlerResult {
                let payload = serde_json::json!({
                    "event": event.name(),
                    "payload": event.to_json(),
                });
                match shim.call("on_event", &payload) {
                    Ok(result) => {
                        if let Event::SourceRead { source, .. } = event {
                            if let Some(text) = result.get("source").and_then(Json::as_str) {
                                apply_source(source, text);
                            }
                        }
                        Ok(match result {
                            Json::Null => None,
                            other => Some(other),
                        })
                    }
                    Err(error) => Err(HandlerError::failed(event.name(), error.to_string())),
                }
            });
            let _ = events.connect(&event_name, handler, DEFAULT_PRIORITY);
        }
    }

    /// Build a [`Directive`] adapter for a declared directive name.
    ///
    /// The name is leaked once so it can satisfy the `Directive::name() ->
    /// &'static str` contract; a build declares a small, bounded set of
    /// third-party directives.
    #[must_use]
    pub fn directive(&self, name: impl Into<String>) -> Arc<dyn Directive> {
        let name: &'static str = Box::leak(name.into().into_boxed_str());
        Arc::new(ShimDirective { shim: self.clone(), name })
    }
}

fn apply_source(source: &SharedString, text: &str) {
    if let Ok(mut guard) = source.lock() {
        *guard = text.to_owned();
    }
}

/// A directive whose `run` is delegated to a Python worker.
struct ShimDirective {
    shim: PythonExtensionShim,
    name: &'static str,
}

impl Directive for ShimDirective {
    fn name(&self) -> &'static str {
        self.name
    }

    fn optional_arguments(&self) -> usize {
        4
    }

    fn final_argument_whitespace(&self) -> bool {
        true
    }

    fn has_content(&self) -> bool {
        true
    }

    fn option_spec(&self) -> OptionSpec {
        OptionSpec::empty()
    }

    fn run(&self, ctx: &mut DirectiveContext<'_>) -> Result<Vec<NodeId>, DirectiveError> {
        let params = serde_json::json!({
            "name": self.name,
            "arguments": ctx.arguments(),
            "content": ctx.content,
            "options": ctx.raw_options,
        });
        let result = self.shim.call("directive", &params).map_err(|error| DirectiveError::Failed {
            name: self.name.to_owned(),
            detail: error.to_string(),
            span: SourceSpan::new((ctx.span.start as usize).into(), ctx.span.len() as usize),
        })?;
        let text = result.get("text").and_then(Json::as_str).unwrap_or("");
        let paragraph = ctx.add(NodeKind::Paragraph, ctx.parent)?;
        ctx.add_text(text, paragraph, ctx.line)?;
        Ok(vec![paragraph])
    }
}

/// A small, self-contained example worker implementing the shim protocol.
///
/// It registers a `source-read` handler that replaces `<<MARKER>>` with a
/// Python-provided string and a `pybadge` directive that emits a text paragraph.
/// Tests use it both inline and as a fixture file.
pub const EXAMPLE_EXTENSION_SCRIPT: &str = r#"
import json, sys

EVENTS = ["source-read", "builder-inited"]
DIRECTIVES = ["pybadge"]


def on_event(params):
    event = params.get("event")
    payload = params.get("payload", {})
    if event == "source-read":
        source = payload.get("source", "")
        return {"source": source.replace("<<MARKER>>", "PYTHON-WAS-HERE")}
    if event == "builder-inited":
        return {"builder": payload.get("builder_name", "")}
    return None


def directive(params):
    name = params.get("name")
    if name == "pybadge":
        args = params.get("arguments", [])
        label = args[0] if args else "badge"
        return {"text": "PYTHON_DIRECTIVE_OUTPUT[" + label + "]", "classes": []}
    return {"text": ""}


for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        request = json.loads(line)
    except Exception:
        continue
    method = request.get("method")
    rid = request.get("id")
    params = request.get("params", {})
    try:
        if method == "initialize":
            result = {"events": EVENTS, "directives": DIRECTIVES, "roles": []}
        elif method == "on_event":
            result = on_event(params)
        elif method == "directive":
            result = directive(params)
        elif method == "role":
            result = {"text": ""}
        elif method == "shutdown":
            result = None
        else:
            result = None
        response = {"jsonrpc": "2.0", "id": rid, "result": result}
    except Exception as exc:
        response = {"jsonrpc": "2.0", "id": rid, "error": {"message": str(exc)}}
    sys.stdout.write(json.dumps(response) + "\n")
    sys.stdout.flush()
    if method == "shutdown":
        break
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn find_python() -> Option<PathBuf> {
        for candidate in ["python3", "python"] {
            let path = PathBuf::from(candidate);
            if Command::new(&path).arg("-c").arg("pass").status().is_ok() {
                return Some(path);
            }
        }
        None
    }

    #[test]
    fn missing_interpreter_degrades_gracefully() {
        let shim = PythonExtensionShim::start(ShimConfig::inline(
            "missing", "/definitely/not/a/python", "pass",
        ));
        assert!(!shim.is_available());
        assert!(!shim.warnings().is_empty());
        assert!(shim.call("on_event", &serde_json::json!({})).is_err());
        // Must not panic even when wired into a bus.
        let events = EventManager::new();
        shim.connect_to_events(&events);
    }

    #[test]
    fn fixture_worker_initializes_and_registers_callbacks() {
        let Some(python) = find_python() else { return };
        let shim = PythonExtensionShim::start(ShimConfig::inline("fixture", python, EXAMPLE_EXTENSION_SCRIPT));
        assert!(shim.is_available(), "warnings: {:?}", shim.warnings());
        assert!(shim.events().iter().any(|event| event == "source-read"));
        assert!(shim.directives().iter().any(|name| name == "pybadge"));
    }
}
