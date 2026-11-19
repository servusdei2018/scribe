//! Phase 31 `scribe clean` implementation.
//!
//! Removes generated build output and the doctree cache without ever touching
//! source files. Every candidate path is resolved lexically and checked against
//! the project root before any filesystem mutation happens, so an absolute
//! path, `..` escape, symlink out of the tree, or a plain file is rejected with
//! a structured diagnostic rather than deleted. The operation is idempotent:
//! a missing target is simply reported as "nothing to clean".

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use miette::{Diagnostic, Report, Result};
use thiserror::Error;

use crate::options::CleanArgs;

/// Structured diagnostics raised by `scribe clean`.
#[derive(Debug, Error, Diagnostic)]
pub enum CleanError {
    /// The requested project root does not exist or is not a directory.
    #[error("cannot clean `{}`: {reason}", .path.display())]
    #[diagnostic(
        code(scribe::clean::invalid_root),
        help("Pass an existing project directory to `scribe clean`.")
    )]
    InvalidRoot {
        /// Path that was supplied as the project root.
        path: PathBuf,
        /// Why it cannot be used.
        reason: String,
    },

    /// A candidate target was rejected as unsafe.
    #[error("refusing to clean `{}`: {reason}", .path.display())]
    #[diagnostic(
        code(scribe::clean::unsafe_path),
        help("Choose a generated output directory inside the project root; source files are never removed.")
    )]
    UnsafePath {
        /// Rejected target path.
        path: PathBuf,
        /// Human-readable reason for the refusal.
        reason: String,
    },

    /// A candidate target exists but is not a directory.
    #[error("refusing to clean `{}`: target is a file, not a directory", .path.display())]
    #[diagnostic(
        code(scribe::clean::not_a_directory),
        help("Remove the file manually; `scribe clean` only deletes generated directories.")
    )]
    NotADirectory {
        /// Path of the offending file.
        path: PathBuf,
    },

    /// The filesystem refused to remove a target.
    #[error("cannot remove `{}`: {message}", .path.display())]
    #[diagnostic(
        code(scribe::clean::remove_failed),
        help("Check filesystem permissions and that no other process holds the directory open.")
    )]
    RemoveFailed {
        /// Path that could not be removed.
        path: PathBuf,
        /// OS error message.
        message: String,
    },
}

/// One resolved clean target and whether it existed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CleanTarget {
    /// Absolute path of the target.
    pub path: PathBuf,
    /// Whether the target existed at the time of the scan.
    pub existed: bool,
}

/// Outcome of a clean pass (also used by the integration tests).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CleanReport {
    /// Absolute, canonical project root.
    pub root: PathBuf,
    /// Targets that existed and were removed (or would be removed).
    pub removed: Vec<PathBuf>,
    /// Targets that did not exist (no-op).
    pub absent: Vec<PathBuf>,
    /// Non-fatal configuration warnings.
    pub warnings: Vec<String>,
    /// Whether the pass was a dry run.
    pub dry_run: bool,
}

/// Execute `scribe clean` from parsed arguments.
pub fn run_clean(args: CleanArgs) -> Result<CleanReport> {
    let root = canonical_root(&args.path)?;
    let mut report = CleanReport {
        root: root.clone(),
        dry_run: args.dry_run,
        ..CleanReport::default()
    };

    let mut configured_targets: Vec<PathBuf> = Vec::new();
    let conf_dir = args.conf_dir.clone().unwrap_or_else(|| root.clone());
    match scribe_config::load_config(&conf_dir, None, None) {
        Ok((config, warnings)) => {
            for warning in warnings {
                report.warnings.push(warning.to_string());
            }
            // Upstream conf.py has no output-dir setting, but projects often
            // carry a `build_dir`/`outdir` constant. Honour it when it is a
            // plain string; unknown keys are kept in the config namespace.
            for key in ["build_dir", "outdir", "output_dir"] {
                if let Some(raw) = config.get_str(key) {
                    configured_targets.push(PathBuf::from(raw));
                }
            }
        }
        Err(error) => {
            // A missing or broken config must not block removal of the
            // conventional output directories; it is reported as a warning.
            report.warnings.push(format!("configuration not loaded: {error}"));
        }
    }

    // Explicit `--build-dir` values always win over discovered ones; the
    // conventional `_build` directory is always considered.
    let mut candidates: Vec<PathBuf> = if args.build_dir.is_empty() {
        configured_targets
    } else {
        args.build_dir.clone()
    };
    candidates.push(PathBuf::from("_build"));
    candidates.push(PathBuf::from(".scribe-cache"));

    // Resolve and de-duplicate before any deletion. A single unsafe candidate
    // aborts the whole pass: no partial deletion on a bad path.
    let mut resolved: Vec<PathBuf> = Vec::new();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    for candidate in &candidates {
        let target = resolve_target(&root, candidate)?;
        if let Some(path) = target {
            if seen.insert(path.clone()) {
                resolved.push(path);
            }
        }
    }

    for path in resolved {
        let exists = match path.try_exists() {
            Ok(value) => value,
            Err(error) => {
                return Err(Report::new(CleanError::RemoveFailed {
                    path,
                    message: error.to_string(),
                }))
            }
        };
        if !exists {
            report.absent.push(path);
            continue;
        }
        // Re-check the canonical target immediately before removal to guard
        // against a symlink swapped in between the scan and the mutation.
        let canonical = std::fs::canonicalize(&path).map_err(|error| CleanError::RemoveFailed {
            path: path.clone(),
            message: error.to_string(),
        })?;
        if !canonical.starts_with(&root) || canonical == root {
            return Err(Report::new(CleanError::UnsafePath {
                path,
                reason: "target resolves outside the project root".to_owned(),
            }));
        }
        if !canonical.is_dir() {
            return Err(Report::new(CleanError::NotADirectory { path }));
        }
        if !args.dry_run {
            std::fs::remove_dir_all(&canonical).map_err(|error| CleanError::RemoveFailed {
                path: canonical.clone(),
                message: error.to_string(),
            })?;
        }
        report.removed.push(canonical);
    }

    for path in &report.removed {
        if report.dry_run {
            println!("scribe: would remove {}", path.display());
        } else {
            println!("scribe: removed {}", path.display());
        }
    }
    for path in &report.absent {
        println!("scribe: nothing to clean at {}", path.display());
    }
    for warning in &report.warnings {
        eprintln!("scribe: warning: {warning}");
    }
    if report.removed.is_empty() {
        println!("scribe: nothing to clean; build output is already absent");
    }
    Ok(report)
}

/// Canonicalize the project root, rejecting missing paths and plain files.
fn canonical_root(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(Report::new(CleanError::InvalidRoot {
            path: path.to_path_buf(),
            reason: "the project path must not be empty".to_owned(),
        }));
    }
    let canonical = std::fs::canonicalize(path).map_err(|error| CleanError::InvalidRoot {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;
    if !canonical.is_dir() {
        return Err(Report::new(CleanError::InvalidRoot {
            path: path.to_path_buf(),
            reason: "the project path is not a directory".to_owned(),
        }));
    }
    if !canonical.parent().is_some_and(|parent| parent != canonical) {
        return Err(Report::new(CleanError::InvalidRoot {
            path: path.to_path_buf(),
            reason: "refusing to clean a filesystem root".to_owned(),
        }));
    }
    Ok(canonical)
}

/// Resolve a candidate against the project root using purely lexical rules.
///
/// Returns `Ok(None)` for a target that does not exist (a no-op), and
/// `Ok(Some(path))` for an existing directory that is safe to remove.
fn resolve_target(root: &Path, candidate: &Path) -> Result<Option<PathBuf>> {
    if candidate.as_os_str().is_empty() {
        return Err(Report::new(CleanError::UnsafePath {
            path: candidate.to_path_buf(),
            reason: "empty target path".to_owned(),
        }));
    }

    let mut stack: Vec<std::ffi::OsString> = Vec::new();
    for component in candidate.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => {
                return Err(Report::new(CleanError::UnsafePath {
                    path: candidate.to_path_buf(),
                    reason: "absolute paths are never cleaned".to_owned(),
                }))
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if stack.pop().is_none() {
                    return Err(Report::new(CleanError::UnsafePath {
                        path: candidate.to_path_buf(),
                        reason: "target escapes the project root".to_owned(),
                    }));
                }
            }
            Component::Normal(part) => stack.push(part.to_os_string()),
        }
    }
    if stack.is_empty() {
        return Err(Report::new(CleanError::UnsafePath {
            path: candidate.to_path_buf(),
            reason: "refusing to clean the project root itself".to_owned(),
        }));
    }

    let mut joined = root.to_path_buf();
    for part in stack {
        joined.push(part);
    }

    match joined.try_exists() {
        Ok(false) => return Ok(None),
        Ok(true) => {}
        Err(error) => {
            return Err(Report::new(CleanError::RemoveFailed {
                path: joined,
                message: error.to_string(),
            }))
        }
    }

    let canonical = std::fs::canonicalize(&joined).map_err(|error| CleanError::RemoveFailed {
        path: joined.clone(),
        message: error.to_string(),
    })?;
    if !canonical.starts_with(root) || canonical == root {
        return Err(Report::new(CleanError::UnsafePath {
            path: candidate.to_path_buf(),
            reason: "target resolves outside the project root".to_owned(),
        }));
    }
    if !canonical.is_dir() {
        return Err(Report::new(CleanError::NotADirectory { path: canonical }));
    }
    Ok(Some(canonical))
}

#[cfg(test)]
mod tests {
    use super::{canonical_root, resolve_target};
    use std::path::PathBuf;

    #[test]
    fn clean_rejects_dangerous_candidates() {
        let root = canonical_root(std::path::Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
        for candidate in [
            PathBuf::from("/"),
            PathBuf::from("."),
            PathBuf::from(".."),
            PathBuf::from("a/../../outside"),
            PathBuf::from(""),
        ] {
            assert!(resolve_target(&root, &candidate).is_err(), "accepted {candidate:?}");
        }
    }

    #[test]
    fn clean_accepts_a_child_directory() {
        let root = canonical_root(std::path::Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
        // `_build` is unlikely to exist in the crate directory; a no-op is
        // still a successful resolution.
        let resolved = resolve_target(&root, std::path::Path::new("_build")).unwrap();
        assert!(resolved.is_none() || resolved.is_some_and(|path| path.starts_with(&root)));
    }
}
