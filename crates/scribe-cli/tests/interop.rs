//! End-to-end Phase 27 gate: a third-party Python extension executes inside a
//! real `scribe build` invocation (CLI → config → bridge → HTML output).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn tempdir(label: &str) -> PathBuf {
    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "scribe-cli-phase27-{label}-{}-{sequence}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create temp dir");
    path
}

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
fn scribe_build_runs_third_party_python_extension() {
    let Some(python) = find_python() else { return };
    let root = tempdir("build");
    let source = root.join("src");
    let output = root.join("out");
    std::fs::create_dir_all(&source).expect("create source dir");

    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../scribe-core/tests/fixtures/python_extension.py")
        .canonicalize()
        .expect("fixture script");
    std::fs::copy(&script, source.join("ext.py")).expect("copy extension script");
    std::fs::write(
        source.join("index.rst"),
        "Interop\n=======\n\n<<MARKER>>\n\n.. pybadge:: from-cli\n",
    )
    .expect("write index.rst");
    std::fs::write(
        source.join("scribe.toml"),
        format!(
            "scribe_python = {:?}\nscribe_python_extensions = [\"ext.py\"]\n",
            python.to_string_lossy()
        ),
    )
    .expect("write scribe.toml");

    let status = Command::new(env!("CARGO_BIN_EXE_scribe"))
        .arg("build")
        .arg("-b")
        .arg("html")
        .arg("-E")
        .arg("-q")
        .arg(&source)
        .arg(&output)
        .output()
        .expect("run scribe build");
    assert!(
        status.status.success(),
        "scribe build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&status.stdout),
        String::from_utf8_lossy(&status.stderr)
    );

    let html = std::fs::read_to_string(output.join("index.html")).expect("index.html");
    assert!(html.contains("PYTHON-WAS-HERE"), "source-read handler did not run: {html}");
    assert!(
        html.contains("PYTHON_DIRECTIVE_OUTPUT[from-cli]"),
        "Python directive did not render: {html}"
    );
}

#[test]
fn scribe_build_without_python_still_succeeds() {
    let root = tempdir("no-python");
    let source = root.join("src");
    let output = root.join("out");
    std::fs::create_dir_all(&source).expect("create source dir");
    std::fs::write(source.join("index.rst"), "Plain\n=====\n\ntext\n").expect("write index.rst");
    std::fs::write(
        source.join("scribe.toml"),
        "scribe_python = \"/definitely/not/a/python\"\nscribe_python_extensions = [\"missing.py\"]\n",
    )
    .expect("write scribe.toml");
    let status = Command::new(env!("CARGO_BIN_EXE_scribe"))
        .arg("build")
        .arg("-b")
        .arg("html")
        .arg("-E")
        .arg("-q")
        .arg(&source)
        .arg(&output)
        .output()
        .expect("run scribe build");
    assert!(status.status.success(), "build must degrade gracefully: {}", String::from_utf8_lossy(&status.stderr));
    assert!(output.join("index.html").is_file());
}
