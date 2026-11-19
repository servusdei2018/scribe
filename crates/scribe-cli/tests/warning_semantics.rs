//! Phase 34 integration tests for `-W` / `suppress_warnings` exit semantics.
//!
//! The fixture is `sphinx/tests/roots/test-domain-py-xref-warning` (copied to a
//! temporary project); it contains one dangling `:ref:` and one resolved one.
//! Each case is also checked against the pinned `sphinx-build` oracle, so the
//! assertions verify parity rather than an invented scribe behaviour.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").canonicalize().unwrap()
}

/// Build a temporary project from the upstream root fixture and an optional
/// `suppress_warnings` list in `conf.py`.
fn project(suppress: &[&str]) -> PathBuf {
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "scribe-p34-warn-{}-{unique}",
        std::process::id()
    ));
    let src = root.join("src");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&src).unwrap();
    let fixture = workspace_root().join("sphinx/tests/roots/test-domain-py-xref-warning/index.rst");
    fs::copy(&fixture, src.join("index.rst")).unwrap();
    let mut conf = String::from("project = 'p'\nmaster_doc = 'index'\n");
    if !suppress.is_empty() {
        conf.push_str("suppress_warnings = [");
        conf.push_str(
            &suppress.iter().map(|s| format!("'{s}'")).collect::<Vec<_>>().join(", "),
        );
        conf.push_str("]\n");
    }
    fs::write(src.join("conf.py"), conf).unwrap();
    root
}

fn scribe(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_scribe"))
        .args(args)
        .output()
        .expect("spawn scribe")
}

fn sphinx(src: &Path, out: &Path, extra: &[&str]) -> Output {
    let binary = workspace_root().join(".oracle-venv/bin/sphinx-build");
    if !binary.is_file() {
        panic!("oracle missing at {}", binary.display());
    }
    let mut command = Command::new(binary);
    command.arg("-b").arg("pseudoxml").arg("-E").arg("-q");
    for arg in extra {
        command.arg(arg);
    }
    command.arg(src).arg(out).output().expect("spawn sphinx-build")
}

fn assert_sphinx_available() {
    assert!(
        workspace_root().join(".oracle-venv/bin/sphinx-build").is_file(),
        "oracle venv missing"
    );
}

#[test]
fn minus_w_fails_on_unsuppressed_xref_warning() {
    assert_sphinx_available();
    let root = project(&[]);
    let src = root.join("src").to_string_lossy().into_owned();
    let out = root.join("out").to_string_lossy().into_owned();

    let oracle = sphinx(Path::new(&src), Path::new(&out), &["-W"]);
    let actual = scribe(&["build", &src, &out, "-c", &src, "-b", "pseudoxml", "-W"]);
    assert_eq!(
        actual.status.success(),
        oracle.status.success(),
        "oracle stderr:\n{}\nscribe stderr:\n{}",
        String::from_utf8_lossy(&oracle.stderr),
        String::from_utf8_lossy(&actual.stderr),
    );
    assert!(!actual.status.success());
    assert!(String::from_utf8_lossy(&actual.stderr).contains("warning(s)"));
}

#[test]
fn suppress_warnings_exact_type_matches_upstream() {
    assert_sphinx_available();
    let root = project(&["ref"]);
    let src = root.join("src").to_string_lossy().into_owned();
    let out = root.join("out").to_string_lossy().into_owned();

    let oracle = sphinx(Path::new(&src), Path::new(&out), &["-W"]);
    let actual = scribe(&["build", &src, &out, "-c", &src, "-b", "pseudoxml", "-W"]);
    assert_eq!(
        actual.status.success(),
        oracle.status.success(),
        "oracle stderr:\n{}\nscribe stderr:\n{}",
        String::from_utf8_lossy(&oracle.stderr),
        String::from_utf8_lossy(&actual.stderr),
    );
    assert!(actual.status.success(), "{}", String::from_utf8_lossy(&actual.stderr));
}

#[test]
fn suppress_warnings_subtype_and_type_wildcard_match_upstream() {
    assert_sphinx_available();
    // Upstream supports three suppression forms: bare type, `type.*`, and
    // exact `type.subtype`. Each must suppress both warnings in the fixture.
    for pattern in ["ref", "ref.ref", "ref.*"] {
        let root = project(&[pattern]);
        let src = root.join("src").to_string_lossy().into_owned();
        let out = root.join("out").to_string_lossy().into_owned();
        let oracle = sphinx(Path::new(&src), Path::new(&out), &["-W"]);
        let actual = scribe(&["build", &src, &out, "-c", &src, "-b", "pseudoxml", "-W"]);
        assert_eq!(
            actual.status.success(),
            oracle.status.success(),
            "pattern {pattern}; oracle stderr:\n{}\nscribe stderr:\n{}",
            String::from_utf8_lossy(&oracle.stderr),
            String::from_utf8_lossy(&actual.stderr),
        );
        assert!(actual.status.success(), "{}", String::from_utf8_lossy(&actual.stderr));
    }
    // A non-matching subtype or a partial glob must NOT suppress (upstream
    // treats partial globs literally).
    for pattern in ["ref.term", "ref.r*", "*"] {
        let root = project(&[pattern]);
        let src = root.join("src").to_string_lossy().into_owned();
        let out = root.join("out").to_string_lossy().into_owned();
        let oracle = sphinx(Path::new(&src), Path::new(&out), &["-W"]);
        let actual = scribe(&["build", &src, &out, "-c", &src, "-b", "pseudoxml", "-W"]);
        assert_eq!(
            actual.status.success(),
            oracle.status.success(),
            "pattern {pattern}; oracle stderr:\n{}\nscribe stderr:\n{}",
            String::from_utf8_lossy(&oracle.stderr),
            String::from_utf8_lossy(&actual.stderr),
        );
        assert!(!actual.status.success());
    }
}

#[test]
fn keep_going_does_not_change_exit_status() {
    let root = project(&[]);
    let src = root.join("src").to_string_lossy().into_owned();
    let out = root.join("out").to_string_lossy().into_owned();
    let plain = scribe(&["build", &src, &out, "-c", &src, "-b", "pseudoxml", "-W"]);
    let keep_going = scribe(&[
        "build", &src, &(out + "-keep"), "-c", &src, "-b", "pseudoxml", "-W", "--keep-going",
    ]);
    assert_eq!(plain.status.success(), keep_going.status.success());
    assert!(!keep_going.status.success());
}

#[test]
fn check_applies_same_suppression_and_w_semantics() {
    let root_unsuppressed = project(&[]);
    let src_unsuppressed = root_unsuppressed.join("src").to_string_lossy().into_owned();
    let unsuppressed = scribe(&["check", &src_unsuppressed, "-c", &src_unsuppressed, "-W"]);
    assert!(!unsuppressed.status.success());

    let root_suppressed = project(&["ref"]);
    let src_suppressed = root_suppressed.join("src").to_string_lossy().into_owned();
    let suppressed = scribe(&["check", &src_suppressed, "-c", &src_suppressed, "-W"]);
    assert!(
        suppressed.status.success(),
        "{}",
        String::from_utf8_lossy(&suppressed.stderr)
    );
}
