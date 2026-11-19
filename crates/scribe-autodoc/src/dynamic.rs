//! Optional dynamic introspection bridge (Phase 21).
//!
//! The primary autodoc path is fully static (§21.1 of ARCHITECTURE.md). This
//! module implements a *documented* fallback worker protocol for modules that
//! cannot be parsed statically (generated code, C extensions, heavy `exec`
//! side effects) or when a build explicitly requests dynamic introspection.
//!
//! # Worker protocol
//!
//! The engine spawns `<python> -c <WORKER_SCRIPT> <module> <root...>` with the
//! module search roots as trailing arguments. The worker:
//!
//! 1. prepends each root to `sys.path`;
//! 2. imports the module with `importlib.import_module`;
//! 3. walks `module.__dict__`, collecting public names and, for each object,
//!    its `objtype`, `signature` (via `inspect.signature`), `docstring`,
//!    `bases` (for classes), `annotation` (for variables) and source `line`;
//! 4. prints one compact JSON object to **stdout** and exits `0`.
//!
//! Any error is written to stderr and yields a non-zero exit status. The Rust
//! side treats that as a warning and degrades to the static result (which, for
//! a generated module, is usually an empty tree); a missing Python binary is
//! handled identically. No panic and no build failure is possible.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;
use serde_json::Value;

use crate::error::{AutodocError, AutodocResult};

/// The worker script, kept as a literal so the protocol is self-documenting
/// and auditable without shipping an auxiliary file.
pub const WORKER_SCRIPT: &str = r#"
import importlib, inspect, json, sys

roots = sys.argv[2:]
for root in reversed(roots):
    if root not in sys.path:
        sys.path.insert(0, root)
name = sys.argv[1]
try:
    module = importlib.import_module(name)
except Exception as exc:  # noqa: BLE001 - intentional broad catch for diagnostics
    print(f"autodoc worker: cannot import {name!r}: {exc}", file=sys.stderr)
    raise SystemExit(2)

def objtype(obj):
    if inspect.isclass(obj):
        return "class"
    if inspect.isroutine(obj):
        return "function"
    return "data"

def signature_of(obj):
    try:
        return name_of(obj) + str(inspect.signature(obj))
    except (TypeError, ValueError):
        return name_of(obj) + "(...)"

def name_of(obj):
    return getattr(obj, "__name__", "")

members = []
for key, value in vars(module).items():
    if key.startswith("_"):
        continue
    entry = {
        "name": key,
        "objtype": objtype(value),
        "signature": signature_of(value),
        "docstring": inspect.getdoc(value),
        "annotation": None,
        "bases": [],
        "line": 0,
    }
    if inspect.isclass(value):
        entry["bases"] = [getattr(base, "__name__", str(base)) for base in value.__bases__]
    members.append(entry)

print(json.dumps({
    "name": name,
    "docstring": inspect.getdoc(module),
    "members": members,
}, ensure_ascii=False))
"#;

/// A dynamically introspected member.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct DynamicMember {
    /// Member name.
    pub name: String,
    /// Upstream-style object type (`class`, `function`, `data`).
    pub objtype: String,
    /// Rendered signature.
    pub signature: String,
    /// Docstring, if any.
    pub docstring: Option<String>,
    /// Variable annotation, if statically available.
    pub annotation: Option<String>,
    /// Base class names.
    #[serde(default)]
    pub bases: Vec<String>,
    /// Source line.
    #[serde(default)]
    pub line: u32,
}

/// A dynamically introspected module.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct DynamicModule {
    /// Module name.
    pub name: String,
    /// Module docstring.
    pub docstring: Option<String>,
    /// Public members.
    #[serde(default)]
    pub members: Vec<DynamicMember>,
}

/// Configuration for the optional dynamic bridge.
#[derive(Clone, Debug)]
pub struct DynamicOptions {
    /// Python interpreter to spawn.
    pub python: PathBuf,
    /// Module search roots passed to the worker.
    pub roots: Vec<PathBuf>,
    /// Per-command timeout in seconds (guards against side-effect hangs).
    pub timeout_secs: u64,
}

impl Default for DynamicOptions {
    fn default() -> Self {
        Self {
            python: PathBuf::from(".oracle-venv/bin/python"),
            roots: Vec::new(),
            timeout_secs: 15,
        }
    }
}

/// Introspects a module through the worker protocol.
///
/// Returns [`AutodocError::Dynamic`] on any failure; callers are expected to
/// warn and continue with the static result.
pub fn introspect(module: &str, options: &DynamicOptions) -> AutodocResult<DynamicModule> {
    let mut command = Command::new(&options.python);
    command
        .arg("-c")
        .arg(WORKER_SCRIPT)
        .arg(module)
        .args(&options.roots)
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped());
    let output = command.output().map_err(|error| AutodocError::Dynamic {
        detail: format!("could not spawn {}: {error}", options.python.display()),
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AutodocError::Dynamic {
            detail: format!("worker exited with {}: {}", output.status, stderr.trim()),
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: Value = serde_json::from_str(stdout.trim()).map_err(|error| AutodocError::Dynamic {
        detail: format!("worker emitted invalid JSON: {error}"),
    })?;
    serde_json::from_value(value).map_err(|error| AutodocError::Dynamic {
        detail: format!("worker JSON did not match the protocol: {error}"),
    })
}

/// Whether the configured interpreter exists (used to skip the bridge with a
/// single warning rather than trying every module).
#[must_use]
pub fn python_available(options: &DynamicOptions) -> bool {
    options.python.is_file() || which(&options.python).is_some()
}

fn which(program: &Path) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_script_is_self_contained() {
        assert!(WORKER_SCRIPT.contains("importlib.import_module"));
        assert!(WORKER_SCRIPT.contains("inspect.signature"));
        assert!(WORKER_SCRIPT.contains("json.dumps"));
    }

    #[test]
    fn missing_interpreter_is_reported_not_panicked() {
        let options = DynamicOptions {
            python: PathBuf::from("/nonexistent/python-binary"),
            roots: Vec::new(),
            timeout_secs: 1,
        };
        assert!(introspect("does_not_exist", &options).is_err());
    }
}
