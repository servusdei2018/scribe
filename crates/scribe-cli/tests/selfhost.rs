//! Phase 30 self-hosting smoke test.
//!
//! Builds the real upstream documentation tree (`sphinx/doc`, 155 `.rst`
//! files) end-to-end with the compiled `scribe` binary. The test asserts that
//! the build *terminates within a bounded timeout* and *does not panic*; it
//! deliberately does **not** assert byte parity with the Python oracle, nor
//! that every warning is resolved. Those gaps are tracked honestly in
//! `BENCHMARKS.md` / `STATUS.md` rather than turned into a hard gate.
//!
//! The custom `sphinx13` theme used by `sphinx/doc/conf.py` is resolved from
//! the project-local `html_theme_path` (`sphinx/doc/_themes`), so the full tree
//! now builds with its unmodified `conf.py` — with no `-D html_theme=basic`
//! override. The override build remains a separate regression guard.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const SELFHOST_TIMEOUT: Duration = Duration::from_secs(120);
static COUNTER: AtomicU64 = AtomicU64::new(0);

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .to_path_buf()
}

/// Run a child process with a hard timeout, killing it on expiry so a hung
/// self-host build cannot outlive the harness.
///
/// stdout/stderr go to **files**, not pipes: scribe's config bridge may spawn
/// a short-lived Python child, and an inherited pipe would keep `wait_with_output`
/// blocked on EOF long after scribe itself has exited.
fn run_with_timeout(
    mut command: Command,
    stdout_path: &Path,
    stderr_path: &Path,
) -> std::io::Result<(ExitStatus, String, String)> {
    let stdout_file = std::fs::File::create(stdout_path)?;
    let stderr_file = std::fs::File::create(stderr_path)?;
    command
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file));
    let mut child = command.spawn()?;
    let start = Instant::now();
    loop {
        match child.try_wait()? {
            Some(status) => {
                let stdout = std::fs::read_to_string(stdout_path).unwrap_or_default();
                let stderr = std::fs::read_to_string(stderr_path).unwrap_or_default();
                return Ok((status, stdout, stderr));
            }
            None if start.elapsed() >= SELFHOST_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "self-host build exceeded the bounded timeout",
                ));
            }
            None => thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn count_html(directory: &Path, out: &mut usize) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            count_html(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("html") {
            *out += 1;
        }
    }
}

#[test]
fn selfhost_build_sphinx_doc_terminates_without_panicking() {
    let root = workspace_root();
    let source = root.join("sphinx/doc");
    // The oracle checkout is gitignored; skip gracefully when it is absent.
    if !source.join("conf.py").is_file() {
        eprintln!("sphinx/doc not present; skipping self-host smoke test");
        return;
    }

    let scribe = PathBuf::from(env!("CARGO_BIN_EXE_scribe"));
    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    let output = std::env::temp_dir().join(format!(
        "scribe-selfhost-{}-{sequence}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&output);

    let mut command = Command::new(&scribe);
    command
        .current_dir(&root)
        .arg("build")
        .arg("-b")
        .arg("html")
        .arg("-E")
        .arg("-q")
        .arg("-c")
        .arg(&source)
        .arg("-D")
        .arg("html_theme=basic")
        .arg(&source)
        .arg(&output);
    let stdout_path = output.with_extension("stdout.log");
    let stderr_path = output.with_extension("stderr.log");
    let result = run_with_timeout(command, &stdout_path, &stderr_path);

    // A timeout is a hard failure; the harness kills the child on expiry.
    assert!(
        result.is_ok(),
        "self-host build timed out or failed to spawn: {:?}",
        result.err()
    );
    let (status, stdout, stderr) = result.expect("checked above");
    let combined = format!("{stdout}\n{stderr}");

    // The core contract: no Rust panic anywhere in the output.
    assert!(
        !combined.contains("panicked at"),
        "self-host build panicked:\n{combined}"
    );
    assert!(
        !combined.contains("thread 'main' panicked"),
        "self-host build panicked:\n{combined}"
    );

    // Substantial real output must have been produced (the tree has 155
    // source pages; with the theme override the build writes >150 HTML files).
    let mut html_count = 0usize;
    count_html(&output, &mut html_count);
    assert!(
        html_count >= 100,
        "self-host build produced only {html_count} HTML pages:\n{combined}"
    );

    // With the bundled-theme override the run is expected to succeed. The
    // no-panic + output assertions above remain the primary contract.
    eprintln!(
        "self-host build: exit={:?}, {html_count} HTML pages",
        status.code()
    );
    assert!(status.success(), "self-host build exited non-zero:\n{combined}");

    let _ = std::fs::remove_dir_all(&output);
    let _ = std::fs::remove_file(&stdout_path);
    let _ = std::fs::remove_file(&stderr_path);
}

/// Phase 35 exit gate: build `sphinx/doc` with its **unmodified** `conf.py`
/// and no theme override. The project-local `sphinx13` theme must be resolved
/// from `html_theme_path`, its inheritance chain followed to the bundled
/// `basic` theme, and its templates/assets actually used — not silently
/// replaced by `basic`.
#[test]
fn selfhost_build_uses_unmodified_sphinx13_theme() {
    let root = workspace_root();
    let source = root.join("sphinx/doc");
    if !source.join("conf.py").is_file() {
        eprintln!("sphinx/doc not present; skipping Phase 35 theme self-host test");
        return;
    }

    let scribe = PathBuf::from(env!("CARGO_BIN_EXE_scribe"));
    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    let output = std::env::temp_dir().join(format!(
        "scribe-selfhost-theme-{}-{sequence}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&output);

    // Deliberately NO `-D html_theme=basic`: the unmodified conf.py selects
    // `sphinx13` and points `html_theme_path` at `sphinx/doc/_themes`.
    let mut command = Command::new(&scribe);
    command
        .current_dir(&root)
        .arg("build")
        .arg("-b")
        .arg("html")
        .arg("-E")
        .arg("-q")
        .arg("-c")
        .arg(&source)
        .arg(&source)
        .arg(&output);
    let stdout_path = output.with_extension("stdout.log");
    let stderr_path = output.with_extension("stderr.log");
    let result = run_with_timeout(command, &stdout_path, &stderr_path);

    assert!(
        result.is_ok(),
        "unmodified-theme self-host build timed out or failed to spawn: {:?}",
        result.err()
    );
    let (status, stdout, stderr) = result.expect("checked above");
    let combined = format!("{stdout}\n{stderr}");

    // No Rust panic and no fatal builder error.
    assert!(!combined.contains("panicked at"), "build panicked:\n{combined}");
    assert!(
        !combined.contains("thread 'main' panicked"),
        "build panicked:\n{combined}"
    );
    assert!(
        !combined.contains("Error: scribe::builder::failed"),
        "build reported a fatal error:\n{combined}"
    );

    let mut html_count = 0usize;
    count_html(&output, &mut html_count);
    assert!(
        html_count >= 100,
        "build produced only {html_count} HTML pages:\n{combined}"
    );

    // The generated index/search pages must exist: Phase 35's original
    // failure discarded the custom theme for these pages and exited 1.
    for special in ["genindex.html", "py-modindex.html", "search.html"] {
        assert!(
            output.join(special).is_file(),
            "missing generated page {special}:\n{combined}"
        );
    }

    // The custom theme's templates and assets must actually be present.
    let index = std::fs::read_to_string(output.join("index.html")).expect("index.html readable");
    for marker in ["pageheader", "sphinx-logo.svg", "svg-arrow-right", "<span>Sphinx</span>"] {
        assert!(
            index.contains(marker),
            "index.html missing custom sphinx13 marker {marker:?} — theme was silently replaced:\n{combined}"
        );
    }
    assert!(
        output.join("_static/sphinx13.css").is_file(),
        "custom theme stylesheet sphinx13.css was not copied:\n{combined}"
    );
    assert!(
        output.join("_static/sphinx-logo.svg").is_file(),
        "custom theme logo was not copied:\n{combined}"
    );

    // Search page is rendered through the theme's layout as well.
    let search = std::fs::read_to_string(output.join("search.html")).expect("search.html readable");
    assert!(
        search.contains("pageheader"),
        "search.html did not use the custom sphinx13 layout:\n{combined}"
    );

    eprintln!(
        "Phase 35 self-host (unmodified conf.py): exit={:?}, {html_count} HTML pages, sphinx13 theme used",
        status.code()
    );
    assert!(status.success(), "self-host build exited non-zero:\n{combined}");

    let _ = std::fs::remove_dir_all(&output);
    let _ = std::fs::remove_file(&stdout_path);
    let _ = std::fs::remove_file(&stderr_path);
}
