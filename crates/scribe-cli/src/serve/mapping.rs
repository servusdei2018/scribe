//! Path → docname mapping, watch-event classification and debounce logic for
//! the Phase 28 dev server. All functions here are pure (no I/O, no async) so
//! they are directly unit-testable and shared by the watcher loop.

use notify::EventKind;
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

/// Lexically normalize a path (collapse `.` and `..`) without touching the
/// filesystem. Watch events can reference files that were just removed, so
/// `std::fs::canonicalize` is not always usable.
pub fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Map a watched source path to a Sphinx docname.
///
/// `source_dir` must be the (already normalized) source root and `suffixes`
/// the configured source suffixes (e.g. `".rst"`, `".md"`). Paths outside the
/// source root, inside Sphinx-reserved `_`/dot directories, or without a known
/// suffix do not map to a document.
pub fn path_to_docname(path: &Path, source_dir: &Path, suffixes: &[String]) -> Option<String> {
    let normalized = lexical_normalize(path);
    let relative = normalized.strip_prefix(source_dir).ok()?;
    let rel = relative.to_string_lossy().replace('\\', "/");
    if rel.is_empty() {
        return None;
    }
    for component in rel.split('/') {
        // Sphinx excludes `**/_sources`, `_templates`, `_static`, dotfiles, …
        if component.starts_with('_') || component.starts_with('.') {
            return None;
        }
    }
    for suffix in suffixes {
        if !suffix.is_empty() {
            if let Some(stem) = rel.strip_suffix(suffix) {
                if !stem.is_empty() {
                    return Some(stem.to_owned());
                }
            }
        }
    }
    None
}

/// Whether a watcher event is worth reacting to.
///
/// `notify`'s recommended Linux watcher subscribes to `IN_ACCESS`, so simply
/// *reading* a source file during a rebuild generates `Access(Open(..))`
/// events. Reacting to those would create an infinite rebuild loop. Only
/// create/modify/remove/rename events represent actual content changes.
pub fn is_relevant_event(kind: &EventKind) -> bool {
    !matches!(kind, EventKind::Access(_) | EventKind::Other)
}

/// Why a watched path changed. Drives whether a rebuild is needed even when
/// the incremental engine reports no changed source documents (e.g. editing a
/// stylesheet or the configuration file).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeKind {
    /// A source document changed; carries its docname.
    SourceDocument(String),
    /// `conf.py` / `scribe.toml` changed.
    Config,
    /// A template or static asset changed.
    Asset,
    /// A watched path we do not classify as a source document or config.
    Other,
}

impl ChangeKind {
    /// Whether this change should trigger a rebuild on its own, independent
    /// of the Phase 08 rebuild set.
    pub fn forces_rebuild(&self) -> bool {
        matches!(self, Self::Config | Self::Asset)
    }
}

/// Classify a watched path against the project layout.
pub fn classify_path(
    path: &Path,
    source_dir: &Path,
    conf_dir: Option<&Path>,
    suffixes: &[String],
) -> ChangeKind {
    let file_name = path.file_name().and_then(|value| value.to_str()).unwrap_or_default();
    if file_name == "conf.py" || file_name == "scribe.toml" {
        let in_conf = conf_dir
            .map(|dir| lexical_normalize(path).starts_with(lexical_normalize(dir)))
            .unwrap_or(false);
        if in_conf {
            return ChangeKind::Config;
        }
    }
    if let Some(docname) = path_to_docname(path, source_dir, suffixes) {
        return ChangeKind::SourceDocument(docname);
    }
    let rel = lexical_normalize(path)
        .strip_prefix(source_dir)
        .map(|value| value.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    let first = rel.split('/').next().unwrap_or_default();
    if first == "_templates" || first == "_static" || first.starts_with('.') || first.starts_with('_') {
        return ChangeKind::Asset;
    }
    ChangeKind::Other
}

/// A deterministic, timer-independent debouncer.
///
/// Records changed paths with a monotonically increasing timestamp (millis)
/// and reports a batch once no new event has arrived for `window`. This is the
/// same policy the async loop implements with `tokio::time::sleep_until`, and
/// it exists as a standalone type so debounce behavior can be tested without
/// sleeping in tests.
#[derive(Debug)]
pub struct Debouncer {
    /// Accumulated changed paths.
    pending: HashSet<PathBuf>,
    /// Timestamp of the most recent recorded event.
    last_event: u64,
    /// Whether any event has been recorded since the last flush.
    armed: bool,
    /// Quiet period required before a batch is emitted.
    window: Duration,
}

impl Debouncer {
    /// Create a debouncer with the given quiet window.
    pub fn new(window: Duration) -> Self {
        Self { pending: HashSet::new(), last_event: 0, armed: false, window }
    }

    /// Record a path at logical time `now` (milliseconds).
    pub fn record(&mut self, path: PathBuf, now: u64) {
        self.pending.insert(path);
        self.last_event = now;
        self.armed = true;
    }

    /// Return the accumulated batch when the quiet window has elapsed, or
    /// `None` while events are still arriving. A successful flush disarms the
    /// debouncer.
    pub fn ready(&mut self, now: u64) -> Option<Vec<PathBuf>> {
        if !self.armed || now.saturating_sub(self.last_event) < self.window.as_millis() as u64 {
            return None;
        }
        self.armed = false;
        let mut batch: Vec<PathBuf> = self.pending.drain().collect();
        batch.sort();
        Some(batch)
    }

    /// Number of paths currently accumulated.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::{classify_path, is_relevant_event, lexical_normalize, path_to_docname, ChangeKind, Debouncer};
    use notify::event::{AccessMode, CreateKind, DataChange, ModifyKind};
    use notify::EventKind;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    fn suffixes() -> Vec<String> {
        vec![".rst".to_owned(), ".md".to_owned(), ".txt".to_owned()]
    }

    #[test]
    fn maps_source_paths_to_docnames() {
        let source = Path::new("/docs/src");
        let suffixes = suffixes();
        assert_eq!(
            path_to_docname(Path::new("/docs/src/index.rst"), source, &suffixes).as_deref(),
            Some("index")
        );
        assert_eq!(
            path_to_docname(Path::new("/docs/src/api/intro.md"), source, &suffixes).as_deref(),
            Some("api/intro")
        );
        assert_eq!(
            path_to_docname(Path::new("/docs/src/guide.txt"), source, &suffixes).as_deref(),
            Some("guide")
        );
    }

    #[test]
    fn normalizes_dot_and_dotdot_segments() {
        assert_eq!(
            lexical_normalize(Path::new("/docs/src/./api/../index.rst")),
            PathBuf::from("/docs/src/index.rst")
        );
    }

    #[test]
    fn ignores_reserved_and_unknown_paths() {
        let source = Path::new("/docs/src");
        let suffixes = suffixes();
        assert_eq!(path_to_docname(Path::new("/docs/src/_static/x.rst"), source, &suffixes), None);
        assert_eq!(path_to_docname(Path::new("/docs/src/.hidden.rst"), source, &suffixes), None);
        assert_eq!(path_to_docname(Path::new("/docs/src/data.bin"), source, &suffixes), None);
        assert_eq!(path_to_docname(Path::new("/elsewhere/index.rst"), source, &suffixes), None);
    }

    #[test]
    fn classifies_config_asset_and_source() {
        let source = Path::new("/docs/src");
        let conf = Path::new("/docs/src");
        let suffixes = suffixes();
        assert_eq!(
            classify_path(Path::new("/docs/src/index.rst"), source, Some(conf), &suffixes),
            ChangeKind::SourceDocument("index".to_owned())
        );
        assert_eq!(
            classify_path(Path::new("/docs/src/conf.py"), source, Some(conf), &suffixes),
            ChangeKind::Config
        );
        assert_eq!(
            classify_path(Path::new("/docs/src/_static/theme.css"), source, Some(conf), &suffixes),
            ChangeKind::Asset
        );
        assert_eq!(
            classify_path(Path::new("/docs/src/_templates/page.html"), source, Some(conf), &suffixes),
            ChangeKind::Asset
        );
    }

    #[test]
    fn access_events_are_ignored_to_avoid_rebuild_loops() {
        assert!(!is_relevant_event(&EventKind::Access(notify::event::AccessKind::Open(AccessMode::Read))));
        assert!(is_relevant_event(&EventKind::Modify(ModifyKind::Data(DataChange::Any))));
        assert!(is_relevant_event(&EventKind::Create(CreateKind::File)));
        assert!(is_relevant_event(&EventKind::Remove(notify::event::RemoveKind::File)));
    }

    #[test]
    fn debouncer_coalesces_rapid_events_and_flushes_once() {
        let mut debouncer = Debouncer::new(Duration::from_millis(100));
        debouncer.record(PathBuf::from("a.rst"), 10);
        debouncer.record(PathBuf::from("b.rst"), 20);
        assert!(debouncer.ready(50).is_none());
        debouncer.record(PathBuf::from("a.rst"), 60);
        assert!(debouncer.ready(150).is_none(), "window restarts on new event");
        let batch = debouncer.ready(161).expect("quiet window elapsed");
        assert_eq!(batch, vec![PathBuf::from("a.rst"), PathBuf::from("b.rst")]);
        assert!(debouncer.ready(1000).is_none(), "batch is emitted only once");
    }
}
