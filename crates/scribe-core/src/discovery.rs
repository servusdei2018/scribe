//! Source discovery mirroring `sphinx.project.Project.discover()` and
//! `sphinx.util.matching.get_matching_files()`.

use crate::{ScribeError, ScribeResult};
use crate::glob::{canon_path, compile_matchers, path_stabilize};
use rayon::prelude::*;
use scribe_cache::ContentDigest;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Patterns upstream always excludes, from `sphinx.project.EXCLUDE_PATHS`.
pub const EXCLUDE_PATHS: [&str; 4] = ["**/_sources", ".#*", "**/.#*", "*.lproj/**"];

/// A source suffix and the parser name it maps to.
pub type SourceSuffixMap = BTreeMap<String, String>;

/// Inputs controlling discovery.
#[derive(Clone, Debug, Default)]
pub struct DiscoveryOptions {
    /// Source directory to walk.
    pub source_dir: PathBuf,
    /// User `include_patterns` (default `**` when empty).
    pub include_patterns: Vec<String>,
    /// User `exclude_patterns`.
    pub exclude_patterns: Vec<String>,
    /// `templates_path` entries (always excluded from source discovery).
    pub templates_path: Vec<String>,
    /// Builder asset paths (`builder.get_asset_paths()`).
    pub asset_paths: Vec<String>,
    /// `source_suffix` mapping.
    pub source_suffix: SourceSuffixMap,
}

impl DiscoveryOptions {
    /// Build discovery inputs directly from a Scribe configuration.
    pub fn from_config(config: &scribe_config::Config, source_dir: impl Into<PathBuf>) -> Self {
        let mut source_suffix = SourceSuffixMap::new();
        if let Some(entries) = config.get_dict("source_suffix") {
            for (suffix, parser) in entries {
                let parser_name = match parser {
                    scribe_config::ConfigValue::Str(name) => name.clone(),
                    scribe_config::ConfigValue::None => "restructuredtext".to_owned(),
                    other => other.display_string(),
                };
                source_suffix.insert(suffix.clone(), parser_name);
            }
        }
        if source_suffix.is_empty() {
            source_suffix.insert(".rst".to_owned(), "restructuredtext".to_owned());
        }
        Self {
            source_dir: source_dir.into(),
            include_patterns: config.get_strings("include_patterns").unwrap_or_default(),
            exclude_patterns: config.get_strings("exclude_patterns").unwrap_or_default(),
            templates_path: config.get_strings("templates_path").unwrap_or_default(),
            asset_paths: Vec::new(),
            source_suffix,
        }
    }
}

/// One discovered source file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredDoc {
    /// Sphinx docname (path relative to the source dir, suffix removed, `/`).
    pub docname: String,
    /// Source path relative to the source dir, `/`-separated.
    pub relative_path: String,
    /// Absolute (or as-supplied) source path.
    pub absolute_path: PathBuf,
    /// Matched source suffix (including the leading dot).
    pub source_suffix: String,
    /// Parser name associated with the suffix.
    pub parser: String,
    /// BLAKE3 digest of the file contents.
    pub digest: ContentDigest,
    /// Last-modified time in integer microseconds, or `0` if unavailable.
    pub mtime_micros: i64,
}

/// Result of a discovery pass.
#[derive(Clone, Debug, Default)]
pub struct DiscoveryResult {
    /// Discovered documents keyed by docname, in deterministic order.
    pub docs: BTreeMap<String, DiscoveredDoc>,
    /// Non-fatal diagnostics (duplicate docnames, unreadable files, ...).
    pub warnings: Vec<String>,
}

impl DiscoveryResult {
    /// Docnames in sorted order.
    pub fn docnames(&self) -> impl Iterator<Item = &str> {
        self.docs.keys().map(String::as_str)
    }
}

/// Discover source files under `options.source_dir`.
///
/// A missing source directory yields an empty result and a warning rather than
/// an error; the incremental engine then behaves like a cold build.
pub fn discover_sources(options: &DiscoveryOptions) -> ScribeResult<DiscoveryResult> {
    let source_dir = &options.source_dir;
    let mut result = DiscoveryResult::default();

    if !source_dir.is_dir() {
        result.warnings.push(format!(
            "source directory `{}` does not exist or is not a directory",
            source_dir.display()
        ));
        return Ok(result);
    }

    let root = std::fs::canonicalize(source_dir).unwrap_or_else(|_| source_dir.clone());

    let include_patterns = if options.include_patterns.is_empty() {
        vec!["**".to_owned()]
    } else {
        options.include_patterns.clone()
    };
    let mut exclude_patterns = Vec::new();
    exclude_patterns.extend(options.exclude_patterns.iter().cloned());
    exclude_patterns.extend(options.templates_path.iter().cloned());
    exclude_patterns.extend(options.asset_paths.iter().cloned());
    exclude_patterns.extend(EXCLUDE_PATHS.iter().map(|pattern| (*pattern).to_owned()));

    let include_matchers = compile_matchers(&include_patterns);
    let exclude_matchers = compile_matchers(&exclude_patterns);
    let matching_include = |path: &str| include_matchers.iter().any(|matcher| matcher.is_match(path));
    let matching_exclude = |path: &str| exclude_matchers.iter().any(|matcher| matcher.is_match(path));

    // Collect all files first so hashing can run in parallel and output order
    // is independent of filesystem iteration order.
    let mut candidates: Vec<String> = Vec::new();
    for entry in walkdir::WalkDir::new(&root).follow_links(true) {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = match entry.path().strip_prefix(&root) {
            Ok(path) => canon_path(&path.to_string_lossy()),
            Err(_) => continue,
        };
        if relative.is_empty() {
            continue;
        }
        if !matching_include(&relative) {
            continue;
        }
        if matching_exclude(&relative) {
            continue;
        }
        // Directory pruning: upstream `get_matching_files` refuses to descend
        // into a directory whose canonical relative path matches an exclusion.
        let mut prefix = String::new();
        let mut excluded_directory = false;
        for component in relative.split('/').take(relative.split('/').count().saturating_sub(1)) {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(component);
            if matching_exclude(&prefix) {
                excluded_directory = true;
                break;
            }
        }
        if excluded_directory {
            continue;
        }
        candidates.push(relative);
    }
    candidates.sort();
    candidates.dedup();

    // Hash contents and collect mtimes in parallel.
    let mut suffixes: Vec<(String, String)> = options
        .source_suffix
        .iter()
        .map(|(suffix, parser)| (suffix.clone(), parser.clone()))
        .collect();
    suffixes.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));

    let hashed: Vec<(String, DiscoveredDoc)> = candidates
        .par_iter()
        .filter_map(|relative| {
            let matched = suffixes.iter().find(|(suffix, _)| relative.ends_with(suffix))?;
            let (suffix, parser) = matched;
            let stem = &relative[..relative.len() - suffix.len()];
            if stem.is_empty() || stem.ends_with('/') {
                return None;
            }
            let docname = path_stabilize(stem);
            let absolute_path = root.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
            let digest = ContentDigest::of_file(&absolute_path).unwrap_or(ContentDigest::ZERO);
            let mtime_micros = std::fs::metadata(&absolute_path)
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_micros() as i64)
                .unwrap_or(0);
            Some((
                docname.clone(),
                DiscoveredDoc {
                    docname,
                    relative_path: relative.clone(),
                    absolute_path,
                    source_suffix: suffix.clone(),
                    parser: parser.clone(),
                    digest,
                    mtime_micros,
                },
            ))
        })
        .collect();

    // Duplicates are resolved deterministically: candidates were sorted, so
    // the first occurrence wins. Upstream emits a warning listing all files.
    let mut all_paths: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (docname, doc) in &hashed {
        all_paths
            .entry(docname.clone())
            .or_default()
            .push(doc.relative_path.clone());
    }
    for (docname, paths) in &all_paths {
        if paths.len() > 1 {
            result.warnings.push(format!(
                "multiple files found for the document `{docname}`: {}; using `{}`",
                paths.join(", "),
                paths.first().map(String::as_str).unwrap_or(docname.as_str())
            ));
        }
    }

    for (docname, doc) in hashed {
        result.docs.entry(docname).or_insert(doc);
    }

    Ok(result)
}

/// Map a docname to its source path using a discovered result.
pub fn doc2path<'a>(result: &'a DiscoveryResult, docname: &str) -> Option<&'a Path> {
    result.docs.get(docname).map(|doc| doc.absolute_path.as_path())
}

/// Map a source path (absolute or relative to `source_dir`) to a docname.
pub fn path2doc<'a>(result: &'a DiscoveryResult, path: &Path) -> Option<&'a str> {
    let relative = canon_path(&path.to_string_lossy());
    if let Some(doc) = result.docs.values().find(|doc| doc.relative_path == relative) {
        return Some(doc.docname.as_str());
    }
    let absolute = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let canonical = canon_path(&absolute.to_string_lossy());
    result
        .docs
        .values()
        .find(|doc| canonical == canon_path(&doc.absolute_path.to_string_lossy()))
        .map(|doc| doc.docname.as_str())
}

/// Convert an I/O failure into a library error with a useful path.
pub fn discovery_error(path: &Path, source: std::io::Error) -> ScribeError {
    ScribeError::Discovery {
        path: path.to_path_buf(),
        message: source.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{discover_sources, DiscoveryOptions};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn workspace() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    #[test]
    fn discovers_test_basic() {
        let root = workspace().join("sphinx/tests/roots/test-basic");
        let options = DiscoveryOptions {
            source_dir: root.clone(),
            source_suffix: BTreeMap::from([(".rst".to_owned(), "restructuredtext".to_owned())]),
            ..Default::default()
        };
        let result = discover_sources(&options).unwrap();
        assert_eq!(result.docs.len(), 1);
        let doc = &result.docs["index"];
        assert_eq!(doc.docname, "index");
        assert_eq!(doc.relative_path, "index.rst");
        let expected = std::fs::canonicalize(root.join("index.rst")).unwrap();
        assert_eq!(doc.absolute_path, expected);
        assert!(!doc.digest.is_zero());
    }

    #[test]
    fn discovers_test_root_with_custom_suffixes_and_excludes() {
        let root = workspace().join("sphinx/tests/roots/test-root");
        let options = DiscoveryOptions::from_config(&scribe_config::Config::new(), &root);
        // Mirror test-root/conf.py: .txt/.foo sources, exclude excluded.* and
        // the `_templates` directory.
        let mut options = options;
        options.source_suffix = BTreeMap::from([
            (".txt".to_owned(), "restructuredtext".to_owned()),
            (".foo".to_owned(), "foo".to_owned()),
        ]);
        options.exclude_patterns = vec!["_build".to_owned(), "**/excluded.*".to_owned()];
        options.templates_path = vec!["_templates".to_owned()];
        let result = discover_sources(&options).unwrap();
        assert!(result.docs.contains_key("index"));
        assert!(result.docs.contains_key("subdir/includes"));
        assert!(result.docs.contains_key("otherext"));
        assert!(!result.docs.contains_key("subdir/excluded"));
        assert!(!result.docs.keys().any(|name| name.starts_with("_templates")));
        assert!(result.docs.keys().all(|name| !name.ends_with(".inc")));
    }

    #[test]
    fn missing_source_dir_is_not_an_error() {
        let options = DiscoveryOptions {
            source_dir: PathBuf::from("/definitely/not/a/real/source/directory"),
            ..Default::default()
        };
        let result = discover_sources(&options).unwrap();
        assert!(result.docs.is_empty());
        assert!(!result.warnings.is_empty());
    }
}
