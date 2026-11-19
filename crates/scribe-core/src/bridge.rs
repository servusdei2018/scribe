//! Third-party extension interop bridge (Phase 27).
//!
//! [`InteropBridge`] is the single object the CLI hands to builders. It owns
//! the [`EventManager`], any live Python extension shims and the Wasm plugin
//! host, so their lifetimes span the entire build. Builders read
//! [`InteropBridge::events`] to emit lifecycle events and call
//! [`InteropBridge::register_directives`] to install third-party directives
//! into the RST registry. When no third-party extension is configured the
//! bridge still exists but carries no handlers, so the build is byte-for-byte
//! identical to the pre-Phase-27 pipeline.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use scribe_config::Config;
use scribe_rst::DirectiveRegistry;

use crate::events::EventManager;
use crate::plugin::WasmPluginHost;
use crate::python_shim::{PythonExtensionShim, ShimConfig};

/// Everything third-party extensions contribute to one build.
pub struct InteropBridge {
    /// Lifecycle event bus.
    pub events: EventManager,
    /// Python extension workers (kept alive for the whole build).
    pub shims: Vec<PythonExtensionShim>,
    /// Wasm plugin host.
    pub plugins: WasmPluginHost,
    /// Directives contributed by Python extensions.
    directives: Vec<Arc<dyn scribe_rst::Directive>>,
    /// Non-fatal bridge diagnostics, surfaced as build warnings.
    warnings: Vec<String>,
}

impl std::fmt::Debug for InteropBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InteropBridge")
            .field("events", &self.events)
            .field("shims", &self.shims)
            .field("plugins", &self.plugins)
            .field("directives", &self.directives.len())
            .field("warnings", &self.warnings)
            .finish()
    }
}

impl PartialEq for InteropBridge {
    fn eq(&self, other: &Self) -> bool {
        self.events == other.events
            && self.directives.len() == other.directives.len()
            && self.warnings == other.warnings
    }
}
impl Eq for InteropBridge {}

impl Default for InteropBridge {
    fn default() -> Self {
        Self {
            events: EventManager::new(),
            shims: Vec::new(),
            plugins: WasmPluginHost::new(),
            directives: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

impl InteropBridge {
    /// Empty bridge with a fresh event bus.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a bridge from configuration.
    ///
    /// * `scribe_python_extensions` — list of Python worker script paths.
    /// * `scribe_python` — interpreter (default `python3`).
    /// * `scribe_wasm_plugins` — list of Wasm plugin manifest/module paths.
    ///
    /// Paths are resolved relative to `source_dir`. Every failure is recorded
    /// as a warning and the offending shim/plugin is skipped; a missing Python
    /// interpreter or an unavailable Wasm runtime never fails the build.
    #[must_use]
    pub fn from_config(config: &Config, source_dir: &Path) -> Self {
        let mut bridge = Self::new();
        let interpreter = PathBuf::from(
            config.get_str("scribe_python").filter(|value| !value.is_empty()).unwrap_or("python3"),
        );
        for script in config.get_strings("scribe_python_extensions").unwrap_or_default() {
            let path = resolve_path(source_dir, &script);
            let name = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or("python-extension");
            let shim = PythonExtensionShim::start(ShimConfig::path(name, &interpreter, &path));
            bridge.warnings.extend(shim.warnings());
            let declared = shim.directives();
            for directive in &declared {
                bridge.directives.push(shim.directive(directive.clone()));
            }
            shim.connect_to_events(&bridge.events);
            bridge.shims.push(shim);
        }
        for plugin in config.get_strings("scribe_wasm_plugins").unwrap_or_default() {
            let path = resolve_path(source_dir, &plugin);
            match std::fs::read(&path) {
                Ok(bytes) => {
                    let name = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or("wasm-plugin");
                    if let Err(error) = bridge.plugins.load_bytes(name, &bytes) {
                        bridge.warnings.push(error.to_string());
                    }
                }
                Err(error) => bridge.warnings.push(format!(
                    "cannot read wasm plugin `{}`: {error}", path.display()
                )),
            }
        }
        bridge.plugins.connect_to_events(&bridge.events);
        bridge
    }

    /// Register third-party directives into `registry`.
    pub fn register_directives(&self, registry: &DirectiveRegistry) {
        for directive in &self.directives {
            registry.register_arc(Arc::clone(directive));
        }
    }

    /// Names of third-party directives contributed by Python extensions.
    #[must_use]
    pub fn directive_names(&self) -> Vec<String> {
        self.directives.iter().map(|directive| directive.name().to_owned()).collect()
    }

    /// Non-fatal bridge diagnostics.
    #[must_use]
    pub fn warnings(&self) -> Vec<String> {
        self.warnings.clone()
    }

    /// Take (and clear) contained event-handler diagnostics.
    #[must_use]
    pub fn take_event_diagnostics(&self) -> Vec<String> {
        self.events.take_diagnostics().into_iter().map(|diagnostic| diagnostic.to_string()).collect()
    }
}

fn resolve_path(source_dir: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        source_dir.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scribe_config::ConfigValue;

    #[test]
    fn empty_config_produces_an_inert_bridge() {
        let bridge = InteropBridge::from_config(&Config::new(), Path::new("/tmp"));
        assert_eq!(bridge.events.listener_count(), 0);
        assert!(bridge.warnings().is_empty());
    }

    #[test]
    fn config_with_bad_paths_degrades_to_warnings() {
        let mut config = Config::new();
        config.set("scribe_python_extensions", ConfigValue::List(vec![
            ConfigValue::from("does-not-exist.py"),
        ])).expect("registered config key");
        config.set("scribe_python", ConfigValue::from("/not/a/python")).expect("registered key");
        config.set("scribe_wasm_plugins", ConfigValue::List(vec![
            ConfigValue::from("missing.wasm"),
        ])).expect("registered key");
        let bridge = InteropBridge::from_config(&config, Path::new("/tmp"));
        assert!(!bridge.warnings().is_empty());
    }
}
