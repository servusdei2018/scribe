//! Differential test for incremental rebuild decisions.
//!
//! Method (documented in ARCHITECTURE.md §12.6):
//!
//! 1. Copy a tiny two-document fixture into a process-unique temp directory.
//! 2. Run `sphinx-build -b html -E -q` to establish the oracle's environment.
//! 3. Run `sphinx-build -b html -v` unchanged; the `reading sources...` lines
//!    list the documents Sphinx re-read — assert the set is empty.
//! 4. Run our `BuildEnvironment::update` on an identical temp copy; assert the
//!    same empty rebuild set.
//! 5. Append a line to `other.rst`, re-run both, and compare the sets of
//!    re-read documents after normalizing docnames/paths.
//!
//! `reading sources...` is upstream's authoritative read-set report: it is
//! emitted from `Builder.read()` for exactly `sorted(added | changed)`, i.e.
//! the same union exposed by `UpdateReport::files_to_rebuild()`.

mod common;

use scribe_core::{BuildEnvironment, UpdateOptions};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct OracleProject {
    root: PathBuf,
    source: PathBuf,
    output: PathBuf,
    cache: PathBuf,
}

impl OracleProject {
    fn new() -> Self {
        let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "scribe-diff-it-{}-{sequence}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let source = root.join("src");
        let output = root.join("out");
        let cache = root.join("cache");
        for dir in [&source, &cache] {
            std::fs::create_dir_all(dir).expect("create temp dir");
        }
        let project = Self {
            root,
            source,
            output,
            cache,
        };
        project.write(
            "conf.py",
            "project = 'Differential'\nmaster_doc = 'index'\nextensions = []\nexclude_patterns = []\n",
        );
        project.write(
            "index.rst",
            "Root\n====\n\n.. toctree::\n\n   other\n",
        );
        project.write("other.rst", "Other\n=====\n\nInitial text.\n");
        project
    }

    fn write(&self, relative: &str, content: &str) {
        std::fs::write(self.source.join(relative), content).expect("write fixture file");
    }

    fn append(&self, relative: &str, content: &str) {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(self.source.join(relative))
            .expect("open fixture file");
        file.write_all(content.as_bytes()).expect("append");
    }
}

impl Drop for OracleProject {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn oracle_binary() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.oracle-venv/bin/sphinx-build");
    assert!(path.is_file(), "oracle sphinx-build not found at {}", path.display());
    path
}

fn run_oracle(project: &OracleProject, extra: &[&str]) -> BTreeSet<String> {
    let output = Command::new(oracle_binary())
        .arg("-b")
        .arg("html")
        .args(extra)
        .arg("-v")
        .arg(&project.source)
        .arg(&project.output)
        .output()
        .expect("run oracle sphinx-build");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "oracle build failed:\n{combined}"
    );
    oracle_read_set(&combined)
}

/// Extract upstream's re-read docnames from `reading sources...` status lines.
fn oracle_read_set(output: &str) -> BTreeSet<String> {
    let mut documents = BTreeSet::new();
    for line in output.lines() {
        let Some(rest) = line.strip_prefix("reading sources...") else {
            continue;
        };
        let Some(close) = rest.rfind(']') else {
            continue;
        };
        for name in rest[close + 1..].split(',') {
            let name = name.trim();
            if !name.is_empty() {
                documents.insert(name.to_owned());
            }
        }
    }
    documents
}

fn our_rebuild_set(project: &OracleProject, environment: &mut BuildEnvironment) -> BTreeSet<String> {
    environment
        .update(
            &common::load_conf(&project.source),
            &project.source,
            &UpdateOptions {
                cache_dir: Some(project.cache.clone()),
                persist: true,
                ..UpdateOptions::default()
            },
        )
        .expect("our environment update")
        .files_to_rebuild()
}

#[test]
fn oracle_rebuild_sets_match_our_files_to_rebuild() {
    let project = OracleProject::new();

    // Establish both environments. The oracle's baseline is a full `-E`
    // re-read; our cold cache likewise reports all documents as added.
    let baseline = run_oracle(&project, &["-E"]);
    assert_eq!(
        baseline,
        BTreeSet::from(["index".to_owned(), "other".to_owned()]),
        "`-E` must make the oracle re-read every document"
    );
    let mut environment = BuildEnvironment::new(&project.source);
    let initial = our_rebuild_set(&project, &mut environment);
    assert_eq!(initial, BTreeSet::from(["index".to_owned(), "other".to_owned()]));

    // Unchanged rebuild: both must report no re-reads.
    let oracle_unchanged = run_oracle(&project, &[]);
    let our_unchanged = our_rebuild_set(&project, &mut environment);
    assert_eq!(oracle_unchanged, BTreeSet::new(), "oracle unexpectedly rebuilt docs");
    assert_eq!(our_unchanged, oracle_unchanged);

    // Modify one source and compare the re-read sets.
    project.append("other.rst", "\nA changed paragraph.\n");
    let oracle_changed = run_oracle(&project, &[]);
    assert_eq!(oracle_changed, BTreeSet::from(["other".to_owned()]));
    let our_changed = our_rebuild_set(&project, &mut environment);
    assert_eq!(
        our_changed, oracle_changed,
        "our rebuild set must match the oracle after normalizing docnames"
    );

    // And a second unchanged rebuild must again be empty on both sides.
    let oracle_settled = run_oracle(&project, &[]);
    let our_settled = our_rebuild_set(&project, &mut environment);
    assert_eq!(oracle_settled, BTreeSet::new());
    assert_eq!(our_settled, oracle_settled);
}

#[test]
fn oracle_read_set_parser_handles_multiple_lines_and_progress() {
    let sample = concat!(
        "updating environment: 2 added, 0 changed, 0 removed\n",
        "reading sources... [ 50%] alpha\n",
        "reading sources... [100%] beta\n",
        "writing output... [100%] index\n",
    );
    assert_eq!(
        oracle_read_set(sample),
        BTreeSet::from(["alpha".to_owned(), "beta".to_owned()])
    );
}
