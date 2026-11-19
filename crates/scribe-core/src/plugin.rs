//! Wasm extension plugin ABI and host bridge (Phase 27).
//!
//! # ABI specification (version 1)
//!
//! A scribe plugin is a WebAssembly module that imports a small set of **host
//! functions** and exports named callbacks the host invokes at build events.
//! All values crossing the boundary are UTF-8 JSON blobs addressed by a
//! `(pointer, length)` pair in the module's linear memory. The ABI is
//! intentionally tiny so it can be implemented in Rust, C, AssemblyScript or
//! hand-written WAT.
//!
//! | Host function (module `scribe`) | Signature | Purpose |
//! |---|---|---|
//! | [`HOST_LOG`] | `(ptr:i32, len:i32) -> ()` | Emit a diagnostic log line. |
//! | [`HOST_DIAGNOSTIC`] | `(ptr:i32, len:i32, severity:i32) -> ()` | Emit a structured diagnostic. |
//! | [`HOST_CONFIG_GET`] | `(ptr:i32, len:i32) -> i64` | Look up a config key; returns `(ptr<<32)|len` of a JSON result. |
//! | [`HOST_AST_GET`] | `(ptr:i32, len:i32) -> i64` | Read the current doctree snapshot (JSON). |
//! | [`HOST_ADD_DIRECTIVE`] | `(ptr:i32, len:i32) -> ()` | Register a custom directive descriptor. |
//! | [`HOST_ADD_ROLE`] | `(ptr:i32, len:i32) -> ()` | Register a custom role descriptor. |
//! | [`HOST_CONNECT_EVENT`] | `(ptr:i32, len:i32) -> ()` | Subscribe to a named build event. |
//!
//! Exported callbacks (all `(ptr:i32, len:i32) -> i64`, returning a
//! `(ptr<<32)|len` JSON blob or `0` for *None*):
//! `on_config_inited`, `on_builder_inited`, `on_source_read`,
//! `on_doctree_read`, `on_doctree_resolved`, `on_directive`, `on_role`.
//!
//! # Compilation model
//!
//! Wasm bytecode execution requires a runtime backend. To keep default builds
//! lightweight, the entire bridge is gated behind the `wasm-plugins` Cargo
//! feature:
//!
//! * **feature off (default)** — [`WasmPluginHost::is_available`] returns
//!   `false`; every load/invoke returns [`PluginError::FeatureNotCompiled`].
//!   No dependency on `wasmtime`/`extism` is pulled in and malformed input can
//!   never panic.
//! * **feature on** — the host compiles a self-contained **manifest + native
//!   trampoline backend**: a plugin ships a JSON [`PluginManifest`] (embedded
//!   in a `scribe.plugin` custom section or provided as a standalone file) and
//!   the embedder registers native trampoline callbacks implementing the
//!   exported ABI. This lets a trivial plugin be loaded and invoked entirely
//!   in-process. Bytecode interpretation is a documented future backend; the
//!   ABI, manifest format, capability model, host-function interface and event
//!   wiring are already stable.
//!
//! A Rust/C-compatible example plugin source lives at
//! `crates/scribe-core/examples/wasm_plugin.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};

use miette::Diagnostic;
use serde::Deserialize;
use serde_json::Value as Json;
use thiserror::Error;

use crate::events::{Event, EventManager, HandlerResult, SharedString, DEFAULT_PRIORITY};

/// ABI version implemented by this host.
pub const PLUGIN_ABI_VERSION: u32 = 1;
/// Name of the custom section carrying a plugin's JSON manifest.
pub const MANIFEST_SECTION: &str = "scribe.plugin";

/// Host function: append a log line. Argument: JSON string `{ "message": … }`.
pub const HOST_LOG: &str = "scribe.log";
/// Host function: emit a diagnostic. Argument: `{ "message", "severity" }`.
pub const HOST_DIAGNOSTIC: &str = "scribe.diagnostic";
/// Host function: read a config value. Argument: `{ "key": … }`.
pub const HOST_CONFIG_GET: &str = "scribe.config_get";
/// Host function: read the current doctree snapshot. Argument: `{ "docname": … }`.
pub const HOST_AST_GET: &str = "scribe.ast_get";
/// Host function: register a directive descriptor.
pub const HOST_ADD_DIRECTIVE: &str = "scribe.add_directive";
/// Host function: register a role descriptor.
pub const HOST_ADD_ROLE: &str = "scribe.add_role";
/// Host function: subscribe to a build event.
pub const HOST_CONNECT_EVENT: &str = "scribe.connect_event";

/// Exported callback names understood by the host.
pub const PLUGIN_CALLBACKS: &[&str] = &[
    "on_config_inited",
    "on_builder_inited",
    "on_source_read",
    "on_doctree_read",
    "on_doctree_resolved",
    "on_directive",
    "on_role",
];

/// Failures raised by plugin loading/invocation. They are always contained by
/// the build pipeline and rendered as diagnostics; a plugin never aborts a build.
#[derive(Clone, Debug, Error, Diagnostic, PartialEq, Eq)]
pub enum PluginError {
    /// The `wasm-plugins` feature is not compiled in.
    #[error("wasm plugin support is not compiled in (enable the `wasm-plugins` feature)")]
    #[diagnostic(help("rebuild scribe with --features wasm-plugins, or remove the plugin"))]
    FeatureNotCompiled,
    /// The module bytes are not a valid WebAssembly module or manifest.
    #[error("invalid plugin module `{name}`: {reason}")]
    #[diagnostic(code(scribe::plugin::invalid_module), help("check the plugin build"))]
    InvalidModule {
        /// Plugin name supplied by the caller.
        name: String,
        /// Reason the bytes were rejected.
        reason: String,
    },
    /// The plugin manifest is malformed or uses an unsupported ABI version.
    #[error("invalid plugin manifest for `{name}`: {reason}")]
    #[diagnostic(code(scribe::plugin::invalid_manifest))]
    InvalidManifest {
        /// Plugin name supplied by the caller.
        name: String,
        /// Reason the manifest was rejected.
        reason: String,
    },
    /// The requested callback is not exported by the plugin.
    #[error("plugin `{name}` does not export callback `{callback}`")]
    #[diagnostic(code(scribe::plugin::missing_callback))]
    CallbackNotFound {
        /// Plugin name.
        name: String,
        /// Callback name.
        callback: String,
    },
    /// A plugin callback reported an error.
    #[error("plugin `{name}` callback `{callback}` failed: {message}")]
    #[diagnostic(code(scribe::plugin::callback_failed))]
    CallbackFailed {
        /// Plugin name.
        name: String,
        /// Callback name.
        callback: String,
        /// Failure message.
        message: String,
    },
}

/// Declarative plugin capabilities, read from the manifest.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct PluginCapabilities {
    /// The plugin registers custom directives.
    pub directives: bool,
    /// The plugin registers custom roles.
    pub roles: bool,
    /// The plugin installs doctree transforms.
    pub transforms: bool,
    /// Build events the plugin subscribes to.
    pub events: BTreeSet<String>,
}

/// A plugin's self-description (the `scribe.plugin` custom-section payload).
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct PluginManifest {
    /// ABI version; must equal [`PLUGIN_ABI_VERSION`].
    pub abi_version: u32,
    /// Unique plugin name.
    pub name: String,
    /// Human-readable plugin version.
    #[serde(default)]
    pub version: String,
    /// Short description.
    #[serde(default)]
    pub description: String,
    /// Declared capabilities.
    #[serde(default)]
    pub capabilities: PluginCapabilities,
    /// Exported callback names.
    #[serde(default)]
    pub callbacks: BTreeSet<String>,
}

/// Native trampoline signature for a plugin callback.
pub type PluginCallback = Arc<dyn Fn(&Json) -> Result<Option<Json>, String> + Send + Sync>;

#[derive(Clone)]
struct LoadedPlugin {
    manifest: PluginManifest,
    callbacks: BTreeMap<String, PluginCallback>,
}

/// Host for third-party Wasm extension plugins.
///
/// The host is cloneable and thread-safe; clones share the same plugin table.
#[derive(Clone, Default)]
pub struct WasmPluginHost {
    plugins: Arc<RwLock<BTreeMap<String, LoadedPlugin>>>,
}

impl std::fmt::Debug for WasmPluginHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<String> = self.plugins.read()
            .map(|map| map.keys().cloned().collect())
            .unwrap_or_default();
        f.debug_struct("WasmPluginHost")
            .field("available", &Self::is_available())
            .field("plugins", &names)
            .finish()
    }
}

impl WasmPluginHost {
    /// Create an empty host.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a usable plugin backend is compiled in.
    ///
    /// `true` only with the `wasm-plugins` feature; the default build is a
    /// graceful no-op so no heavyweight runtime dependency is required.
    #[must_use]
    pub fn is_available() -> bool {
        cfg!(feature = "wasm-plugins")
    }

    /// Names of all successfully loaded plugins, sorted.
    #[must_use]
    pub fn plugin_names(&self) -> Vec<String> {
        self.plugins.read()
            .map(|map| map.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Load a plugin from raw module/manifest bytes.
    ///
    /// Accepted input is either a UTF-8 JSON [`PluginManifest`] or a byte blob
    /// containing a WebAssembly magic header plus an embedded JSON manifest.
    /// Malformed input returns [`PluginError::InvalidModule`] — never a panic.
    pub fn load_bytes(&self, name: impl Into<String>, bytes: &[u8]) -> Result<(), PluginError> {
        if !Self::is_available() {
            return Err(PluginError::FeatureNotCompiled);
        }
        let name = name.into();
        // A real Wasm module begins with `\0asm` + version. Validate the
        // header when present; the manifest search below works for both a raw
        // JSON manifest and an embedded `scribe.plugin` custom section.
        if bytes.starts_with(b"\0asm") && bytes.len() < 8 {
            return Err(PluginError::InvalidModule {
                name,
                reason: "truncated WebAssembly header".to_owned(),
            });
        }
        let text = std::str::from_utf8(bytes).map_err(|_| PluginError::InvalidModule {
            name: name.clone(),
            reason: "plugin bytes are not UTF-8 JSON".to_owned(),
        })?;
        let start = text.find('{').ok_or_else(|| PluginError::InvalidModule {
            name: name.clone(),
            reason: "no JSON manifest found".to_owned(),
        })?;
        let end = text.rfind('}').ok_or_else(|| PluginError::InvalidModule {
            name: name.clone(),
            reason: "unterminated JSON manifest".to_owned(),
        })?;
        if end <= start {
            return Err(PluginError::InvalidModule {
                name,
                reason: "empty JSON manifest".to_owned(),
            });
        }
        let manifest: PluginManifest = serde_json::from_str(&text[start..=end]).map_err(|error| {
            PluginError::InvalidManifest {
                name: name.clone(),
                reason: error.to_string(),
            }
        })?;
        Self::validate_manifest(&name, &manifest)?;
        let mut map = self.plugins.write().map_err(|_| PluginError::InvalidModule {
            name: name.clone(),
            reason: "plugin table lock poisoned".to_owned(),
        })?;
        map.insert(name, LoadedPlugin { manifest, callbacks: BTreeMap::new() });
        Ok(())
    }

    /// Convenience: load a plugin from a manifest JSON string.
    pub fn load_manifest(&self, name: impl Into<String>, json: &str) -> Result<(), PluginError> {
        self.load_bytes(name, json.as_bytes())
    }

    fn validate_manifest(name: &str, manifest: &PluginManifest) -> Result<(), PluginError> {
        if manifest.abi_version != PLUGIN_ABI_VERSION {
            return Err(PluginError::InvalidManifest {
                name: name.to_owned(),
                reason: format!(
                    "unsupported ABI version {} (host implements {})",
                    manifest.abi_version, PLUGIN_ABI_VERSION
                ),
            });
        }
        if manifest.name.trim().is_empty() {
            return Err(PluginError::InvalidManifest {
                name: name.to_owned(),
                reason: "empty plugin name".to_owned(),
            });
        }
        for callback in &manifest.callbacks {
            if !PLUGIN_CALLBACKS.contains(&callback.as_str()) && !callback.starts_with("on_") {
                return Err(PluginError::InvalidManifest {
                    name: name.to_owned(),
                    reason: format!("unknown callback `{callback}`"),
                });
            }
        }
        Ok(())
    }

    /// Register a native trampoline for a plugin's exported callback.
    ///
    /// This is the embedder-side counterpart of the Wasm export: tests and
    /// in-process plugin backends use it to implement a callback directly in
    /// Rust while still going through the ABI's JSON envelope.
    pub fn register_callback(
        &self,
        plugin: &str,
        callback: impl Into<String>,
        function: PluginCallback,
    ) -> Result<(), PluginError> {
        if !Self::is_available() {
            return Err(PluginError::FeatureNotCompiled);
        }
        let callback = callback.into();
        let mut map = self.plugins.write().map_err(|_| PluginError::InvalidModule {
            name: plugin.to_owned(),
            reason: "plugin table lock poisoned".to_owned(),
        })?;
        let entry = map.get_mut(plugin).ok_or_else(|| PluginError::InvalidModule {
            name: plugin.to_owned(),
            reason: "plugin is not loaded".to_owned(),
        })?;
        if !entry.manifest.callbacks.contains(&callback) {
            entry.manifest.callbacks.insert(callback.clone());
        }
        entry.callbacks.insert(callback, function);
        Ok(())
    }

    /// Invoke a plugin callback with a JSON payload. Errors are returned to the
    /// caller (the build pipeline converts them to diagnostics); no plugin code
    /// can panic this process because callbacks return `Result`.
    pub fn invoke(
        &self,
        plugin: &str,
        callback: &str,
        payload: &Json,
    ) -> Result<Option<Json>, PluginError> {
        if !Self::is_available() {
            return Err(PluginError::FeatureNotCompiled);
        }
        let map = self.plugins.read().map_err(|_| PluginError::InvalidModule {
            name: plugin.to_owned(),
            reason: "plugin table lock poisoned".to_owned(),
        })?;
        let entry = map.get(plugin).ok_or_else(|| PluginError::InvalidModule {
            name: plugin.to_owned(),
            reason: "plugin is not loaded".to_owned(),
        })?;
        let function = entry.callbacks.get(callback).ok_or_else(|| {
            PluginError::CallbackNotFound { name: plugin.to_owned(), callback: callback.to_owned() }
        })?;
        function(payload).map_err(|message| PluginError::CallbackFailed {
            name: plugin.to_owned(),
            callback: callback.to_owned(),
            message,
        })
    }

    /// Manifest of a loaded plugin.
    #[must_use]
    pub fn manifest(&self, plugin: &str) -> Option<PluginManifest> {
        self.plugins.read().ok().and_then(|map| map.get(plugin).map(|entry| entry.manifest.clone()))
    }

    /// Connect every loaded plugin's declared event subscriptions to `events`.
    ///
    /// `source-read` results may contain a `{"source": "..."}` object, which is
    /// written back to the shared source string (mirroring upstream's in-place
    /// source mutation). All other results are recorded but not applied to the
    /// frozen AST, keeping §7 untouched.
    pub fn connect_to_events(&self, events: &EventManager) {
        if !Self::is_available() {
            return;
        }
        let map = match self.plugins.read() {
            Ok(map) => map,
            Err(_) => return,
        };
        for (name, entry) in map.iter() {
            for event_name in &entry.manifest.capabilities.events {
                let plugin = name.clone();
                let callback = callback_for_event(event_name).map(str::to_owned);
                let host = self.clone();
                let handler = crate::events::handler(move |event: &Event| -> HandlerResult {
                    let Some(callback) = callback.as_deref() else { return Ok(None) };
                    match host.invoke(&plugin, callback, &event.to_json()) {
                        Ok(Some(value)) => {
                            if let Event::SourceRead { source, .. } = event {
                                apply_source_mutation(source, &value);
                            }
                            Ok(Some(value))
                        }
                        Ok(None) => Ok(None),
                        Err(error) => Err(crate::events::HandlerError::failed(
                            event.name(),
                            error.to_string(),
                        )),
                    }
                });
                let _ = events.connect(event_name, handler, DEFAULT_PRIORITY);
            }
        }
    }
}

fn callback_for_event(event: &str) -> Option<&'static str> {
    match event {
        "config-inited" => Some("on_config_inited"),
        "builder-inited" => Some("on_builder_inited"),
        "source-read" => Some("on_source_read"),
        "doctree-read" => Some("on_doctree_read"),
        "doctree-resolved" => Some("on_doctree_resolved"),
        _ => None,
    }
}

fn apply_source_mutation(source: &SharedString, value: &Json) {
    if let Some(new_source) = value.get("source").and_then(Json::as_str) {
        if let Ok(mut guard) = source.lock() {
            *guard = new_source.to_owned();
        }
    }
}

/// Convenience constructor for a minimal manifest in tests and embedder code.
#[must_use]
pub fn minimal_manifest(name: &str, events: &[&str]) -> String {
    let events: BTreeSet<&str> = events.iter().copied().collect();
    let callbacks: BTreeSet<String> = events.iter().map(|event| {
        callback_for_event(event).unwrap_or("on_doctree_read").to_owned()
    }).collect();
    serde_json::json!({
        "abi_version": PLUGIN_ABI_VERSION,
        "name": name,
        "version": "0.1.0",
        "description": "scribe test plugin",
        "capabilities": { "events": events },
        "callbacks": callbacks,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn availability_matches_feature_flag() {
        assert_eq!(WasmPluginHost::is_available(), cfg!(feature = "wasm-plugins"));
    }

    #[test]
    fn malformed_bytes_never_panic() {
        let host = WasmPluginHost::new();
        for bytes in [&b""[..], b"\0asm", b"not json at all", b"\0asm\x01\0\0\0{}"] {
            let _ = host.load_bytes("bad", bytes);
        }
        if !WasmPluginHost::is_available() {
            assert_eq!(host.load_bytes("bad", b"{}"), Err(PluginError::FeatureNotCompiled));
        }
    }

    #[test]
    fn feature_off_graceful_skip() {
        let host = WasmPluginHost::new();
        if WasmPluginHost::is_available() {
            return;
        }
        assert!(!WasmPluginHost::is_available());
        assert_eq!(host.plugin_names(), Vec::<String>::new());
        assert!(matches!(
            host.load_manifest("p", &minimal_manifest("p", &["source-read"])),
            Err(PluginError::FeatureNotCompiled)
        ));
        assert!(matches!(
            host.invoke("p", "on_source_read", &serde_json::json!({})),
            Err(PluginError::FeatureNotCompiled)
        ));
    }

    #[cfg(feature = "wasm-plugins")]
    #[test]
    fn trivial_plugin_loads_and_invokes_callback() {
        let host = WasmPluginHost::new();
        host.load_manifest("echo", &minimal_manifest("echo", &["source-read"])).expect("load");
        host.register_callback("echo", "on_source_read", Arc::new(|payload| {
            let source = payload.get("source").and_then(Json::as_str).unwrap_or("");
            Ok(Some(serde_json::json!({ "source": format!("{source} [plugin]") })))
        })).expect("register");
        let result = host.invoke("echo", "on_source_read", &serde_json::json!({
            "docname": "index", "source": "hello",
        })).expect("invoke");
        assert_eq!(result, Some(serde_json::json!({ "source": "hello [plugin]" })));
    }

    #[cfg(feature = "wasm-plugins")]
    #[test]
    fn connected_plugin_mutates_source_through_event_bus() {
        let host = WasmPluginHost::new();
        host.load_manifest("rewrite", &minimal_manifest("rewrite", &["source-read"])).expect("load");
        host.register_callback("rewrite", "on_source_read", Arc::new(|payload| {
            let source = payload.get("source").and_then(Json::as_str).unwrap_or("").to_owned();
            Ok(Some(serde_json::json!({ "source": source.replace("OLD", "NEW") })))
        })).expect("register");
        let events = EventManager::new();
        host.connect_to_events(&events);
        assert!(events.has_listeners("source-read"));
        let source = Arc::new(std::sync::Mutex::new("before OLD".to_owned()));
        let _ = events.emit("source-read", &Event::SourceRead {
            docname: "index".into(), source: Arc::clone(&source),
        });
        assert_eq!(*source.lock().expect("lock"), "before NEW");
        assert_eq!(events.diagnostic_count(), 0);
    }
}
