//! Module discovery and source loading (Phase 21).
//!
//! Resolves dotted module names to `.py`/`.pyi` files using the project source
//! directory plus a `sys.path`-like list of extra roots (`autodoc_path`), with
//! support for package `__init__.py`, namespace packages and stub files.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::AutodocError;
use crate::model::{ModuleInfo, ModuleSet};
use crate::parser::parse_source;

/// Candidate file extensions, in search order (matching Python's import
/// preferences; `.pyi` stubs are a static-mode extension).
const EXTENSIONS: &[&str] = &["py", "pyi"];

/// Resolves a dotted module name relative to one root directory.
///
/// Both `pkg.mod` → `root/pkg/mod.py` and `pkg.mod` → `root/pkg/mod/__init__.py`
/// (submodule packages) are considered.
#[must_use]
pub fn resolve_in_root(module: &str, root: &Path) -> Option<PathBuf> {
    let relative = module.split('.').collect::<Vec<_>>();
    if relative.is_empty() || relative.iter().any(|part| part.is_empty()) {
        return None;
    }
    // `pkg.mod/__init__.py` (a package whose last segment is a directory).
    let mut package = root.to_path_buf();
    for part in &relative {
        package.push(part);
    }
    for extension in EXTENSIONS {
        let candidate = package.join(format!("__init__.{extension}"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    // `pkg/mod.py` (a plain module file).
    let mut file = root.to_path_buf();
    for (index, part) in relative.iter().enumerate() {
        if index + 1 == relative.len() {
            for extension in EXTENSIONS {
                let candidate = file.join(format!("{part}.{extension}"));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        } else {
            file.push(part);
        }
    }
    None
}

/// Resolves a module name using an ordered list of roots.
#[must_use]
pub fn resolve_module(module: &str, roots: &[PathBuf]) -> Option<PathBuf> {
    roots.iter().find_map(|root| resolve_in_root(module, root))
}

/// Extracts the extra module search roots from `autodoc_path` / project root.
///
/// `autodoc_path` may be a string list or a `;`/` ` separated string. The
/// source directory is always first, matching `sys.path.insert(0, ...)` in
/// Sphinx `conf.py` fixtures.
#[must_use]
pub fn search_roots(source_dir: Option<&Path>, config: Option<&scribe_config::Config>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(dir) = source_dir {
        roots.push(dir.to_path_buf());
        // Sphinx's own documentation lives in `<checkout>/doc` while the
        // package it documents lives in `<checkout>/sphinx`.  Upstream adds
        // the checkout root to `sys.path`, so do the same here; otherwise
        // `.. automethod:: Sphinx.add_js_file` silently emits no object.
        if let Some(parent) = dir.parent() {
            let parent = parent.to_path_buf();
            if !roots.contains(&parent) {
                roots.push(parent);
            }
        }
    }
    if let Some(config) = config {
        if let Some(paths) = config.get_strings("autodoc_path") {
            for path in paths {
                let candidate = PathBuf::from(path);
                let resolved = if candidate.is_absolute() || source_dir.is_none() {
                    candidate
                } else {
                    source_dir.unwrap_or(Path::new(".")).join(candidate)
                };
                if !roots.contains(&resolved) {
                    roots.push(resolved);
                }
            }
        }
    }
    if roots.is_empty() {
        roots.push(PathBuf::from("."));
    }
    roots
}

/// Result of loading a set of modules: the parsed tree and one warning per
/// module that could not be resolved or parsed.
#[derive(Debug)]
pub struct LoadReport {
    /// Successfully parsed modules.
    pub modules: ModuleSet,
    /// Non-fatal warnings, in module order.
    pub warnings: Vec<String>,
}

/// Loads and statically parses the requested modules in parallel.
///
/// `module_names` may contain package roots; when `discover_packages` is true,
/// every `.py` under the package directory is parsed too, building a complete
/// inheritance index for `inherited-members`.
#[must_use]
pub fn load_modules(
    module_names: &[String],
    roots: &[PathBuf],
    discover_packages: bool,
) -> LoadReport {
    let mut names = Vec::new();
    let mut warnings = Vec::new();
    for name in module_names {
        match resolve_module(name, roots) {
            Some(path) => names.push((name.clone(), path)),
            None => warnings.push(format!("autodoc: failed to import module {name:?}; no source found")),
        }
    }

    if discover_packages {
        for (module, path) in names.clone() {
            let dir = if path.file_name().is_some_and(|name| name == "__init__.py" || name == "__init__.pyi") {
                path.parent().map(Path::to_path_buf)
            } else {
                None
            };
            if let Some(dir) = dir {
                for (child_name, child_path) in discover_package(&module, &dir) {
                    if !names.iter().any(|(_, path)| path == &child_path) {
                        names.push((child_name, child_path));
                    }
                }
            }
        }
    }

    // Source hashing is content-based; use a BTreeMap to deduplicate modules
    // discovered through multiple paths while preserving deterministic order.
    let mut unique: BTreeMap<String, PathBuf> = BTreeMap::new();
    for (name, path) in names {
        unique.entry(name).or_insert(path);
    }
    let inputs: Vec<(String, String, PathBuf)> = unique
        .into_iter()
        .map(|(name, path)| {
            let source = std::fs::read_to_string(&path).unwrap_or_default();
            (source, name, path)
        })
        .collect();

    let mut modules = ModuleSet::new();
    for result in crate::parser::parse_sources(&inputs) {
        match result {
            Ok(module) => modules.insert(module),
            Err(error) => warnings.push(format!("{error}")),
        }
    }
    LoadReport { modules, warnings }
}

/// Discovers every Python source file under a package directory and maps it
/// back to a dotted module name.
#[must_use]
pub fn discover_package(package: &str, dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(dir).sort_by_file_name().into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let Some(extension) = path.extension().and_then(|ext| ext.to_str()) else { continue };
        if !EXTENSIONS.contains(&extension) {
            continue;
        }
        let relative = path.strip_prefix(dir).unwrap_or(path);
        let mut parts: Vec<String> = relative
            .components()
            .filter_map(|component| component.as_os_str().to_str().map(str::to_owned))
            .collect();
        let last = parts.len().saturating_sub(1);
        if let Some(name) = parts.get_mut(last) {
            if name == "__init__.py" || name == "__init__.pyi" {
                *name = String::new();
            } else {
                *name = name.trim_end_matches(&format!(".{extension}")).to_owned();
            }
        }
        let suffix = parts.iter().filter(|part| !part.is_empty()).cloned().collect::<Vec<_>>().join(".");
        let full = if suffix.is_empty() {
            package.to_owned()
        } else {
            format!("{package}.{suffix}")
        };
        out.push((full, path.to_path_buf()));
    }
    out
}

/// Parses a single module from an explicit source path. Convenience wrapper
/// used by tests and the dynamic-fallback path.
pub fn load_module_at(source: &str, name: &str, path: &Path) -> Result<ModuleInfo, AutodocError> {
    parse_source(source, name, path)
}
