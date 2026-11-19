//! Phase 31 end-to-end CLI tests for `quickstart`, `check` and `clean`.
//!
//! These drive the real `scribe` binary via `CARGO_BIN_EXE_scribe`, so they
//! exercise clap parsing, diagnostic rendering, exit status and filesystem
//! effects together — not merely the library helpers.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A uniquely named temporary directory that is removed on drop.
struct TempProject {
    root: PathBuf,
}

impl TempProject {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "scribe-cli-{label}-{}-{nanos}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create temp project");
        Self { root }
    }

    fn path(&self) -> &Path {
        &self.root
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::write(path, contents).expect("write fixture file");
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn scribe(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_scribe"))
        .args(args)
        .output()
        .expect("run scribe binary")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn quickstart_batch_scaffolds_a_project() {
    let project = TempProject::new("quickstart");
    let target = project.path().join("demo");
    let target_text = target.to_string_lossy().into_owned();

    let output = scribe(&[
        "quickstart",
        &target_text,
        "--batch",
        "--quiet",
        "-p",
        "Demo Project",
        "-a",
        "Ada Lovelace",
        "-v",
        "1.0",
        "--source-dir",
        "source",
        "--build-dir",
        "build",
        "--suffix",
        ".rst",
        "--extensions",
        "sphinx.ext.todo,sphinx.ext.viewcode",
    ]);
    assert!(output.status.success(), "stdout={} stderr={}", stdout(&output), stderr(&output));

    let conf = fs::read_to_string(target.join("conf.py")).expect("conf.py");
    assert!(conf.contains("project = \"Demo Project\""));
    assert!(conf.contains("author = \"Ada Lovelace\""));
    assert!(conf.contains("release = \"1.0\""));
    assert!(conf.contains("\"sphinx.ext.todo\""));
    assert!(conf.contains("\"sphinx.ext.viewcode\""));

    let index = fs::read_to_string(target.join("source/index.rst")).expect("index.rst");
    assert!(index.starts_with("Demo Project\n"));
    assert!(index.contains(".. toctree::"));
    assert!(target.join("Makefile").is_file());
    assert!(target.join("make.bat").is_file());
    assert!(target.join(".gitignore").is_file());
    assert!(target.join("build").is_dir());

    // A second run must refuse to overwrite without --force.
    let again = scribe(&["quickstart", &target_text, "--batch", "--quiet"]);
    assert!(!again.status.success());
    assert!(stderr(&again).contains("scribe::quickstart::already_exists"));

    // --force replaces the generated files.
    let forced = scribe(&["quickstart", &target_text, "--batch", "--quiet", "--force"]);
    assert!(forced.status.success(), "stderr={}", stderr(&forced));
}

#[test]
fn check_accepts_a_valid_project() {
    let project = TempProject::new("check-ok");
    project.write(
        "conf.py",
        "project = 'Demo'\nauthor = 'Ada'\nmaster_doc = 'index'\n",
    );
    project.write(
        "index.rst",
        "Demo\n====\n\nA tiny valid document.\n\nSection\n-------\n\nBody text.\n",
    );
    let path = project.path().to_string_lossy().into_owned();

    let output = scribe(&["check", &path]);
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        stdout(&output),
        stderr(&output)
    );
    assert!(stdout(&output).contains("1 document(s)"));
    assert!(stdout(&output).contains("0 error(s)"));
}

#[test]
fn check_rejects_a_toctree_cycle() {
    let project = TempProject::new("check-cycle");
    project.write("conf.py", "project = 'Cycle'\nmaster_doc = 'index'\n");
    project.write(
        "index.rst",
        "Index\n=====\n\n.. toctree::\n\n   other\n",
    );
    project.write(
        "other.rst",
        "Other\n=====\n\n.. toctree::\n\n   index\n",
    );
    let path = project.path().to_string_lossy().into_owned();

    let output = scribe(&["check", &path, "--quiet"]);
    assert!(!output.status.success(), "stdout={}", stdout(&output));
    assert!(
        stderr(&output).contains("scribe::toc::circular") || stderr(&output).contains("check failed"),
        "stderr={}",
        stderr(&output)
    );
}

#[test]
fn check_rejects_a_broken_cross_reference() {
    let project = TempProject::new("check-xref");
    project.write("conf.py", "project = 'Xref'\nmaster_doc = 'index'\n");
    project.write(
        "index.rst",
        "Index\n=====\n\nSee :ref:`does-not-exist` for details.\n",
    );
    let path = project.path().to_string_lossy().into_owned();

    let output = scribe(&["check", &path, "--quiet"]);
    assert!(!output.status.success(), "stdout={}", stdout(&output));
    assert!(
        stderr(&output).contains("missing_reference") || stderr(&output).contains("scribe::xref::unresolved"),
        "stderr={}",
        stderr(&output)
    );
}

#[test]
fn check_warns_on_orphan_and_fail_on_warning_escalates() {
    let project = TempProject::new("check-warn");
    project.write("conf.py", "project = 'Orphan'\nmaster_doc = 'index'\n");
    project.write("index.rst", "Index\n=====\n\nRoot document.\n");
    project.write("orphan.rst", "Orphan\n======\n\nNot in a toctree.\n");
    let path = project.path().to_string_lossy().into_owned();

    let relaxed = scribe(&["check", &path, "--quiet"]);
    assert!(relaxed.status.success(), "stderr={}", stderr(&relaxed));
    assert!(stderr(&relaxed).contains("scribe::toc::not_included"));

    let strict = scribe(&["check", &path, "--quiet", "-W"]);
    assert!(!strict.status.success(), "stderr={}", stderr(&strict));
    assert!(stderr(&strict).contains("warnings are errors"));
}

#[test]
fn clean_removes_output_and_cache_idempotently() {
    let project = TempProject::new("clean");
    project.write("index.rst", "Index\n=====\n\nKeep me.\n");
    let build = project.path().join("_build");
    let cache = project.path().join(".scribe-cache");
    fs::create_dir_all(build.join("html")).expect("create build dir");
    fs::write(build.join("html/index.html"), "<html/>").expect("write build file");
    fs::create_dir_all(&cache).expect("create cache dir");
    fs::write(cache.join("env.bin"), b"cache").expect("write cache file");
    let path = project.path().to_string_lossy().into_owned();

    let output = scribe(&["clean", &path]);
    assert!(output.status.success(), "stderr={}", stderr(&output));
    assert!(!build.exists(), "build output was not removed");
    assert!(!cache.exists(), "cache was not removed");
    assert!(project.path().join("index.rst").is_file(), "source file was removed");

    // A second run is a no-op with exit 0.
    let again = scribe(&["clean", &path]);
    assert!(again.status.success(), "stderr={}", stderr(&again));
}

#[test]
fn clean_dry_run_does_not_delete() {
    let project = TempProject::new("clean-dry");
    let build = project.path().join("_build");
    fs::create_dir_all(&build).expect("create build dir");
    let path = project.path().to_string_lossy().into_owned();

    let output = scribe(&["clean", &path, "--dry-run"]);
    assert!(output.status.success(), "stderr={}", stderr(&output));
    assert!(build.is_dir(), "dry run removed the build directory");
    assert!(stdout(&output).contains("would remove"));
}

#[test]
fn clean_refuses_unsafe_paths() {
    let root = scribe(&["clean", "/"]);
    assert!(!root.status.success());
    assert!(stderr(&root).contains("scribe::clean::invalid_root"));

    let project = TempProject::new("clean-unsafe");
    let path = project.path().to_string_lossy().into_owned();
    let escape = scribe(&["clean", &path, "-o", ".."]);
    assert!(!escape.status.success());
    assert!(stderr(&escape).contains("scribe::clean::unsafe_path"));

    let file = project.path().join("plain-file");
    fs::write(&file, "not a directory").expect("write file");
    let file_target = scribe(&["clean", &path, "-o", "plain-file"]);
    assert!(!file_target.status.success());
    assert!(stderr(&file_target).contains("scribe::clean::not_a_directory"));
    assert!(file.is_file());
}

#[test]
fn help_lists_the_three_production_subcommands() {
    let output = scribe(&["--help"]);
    assert!(output.status.success());
    let text = stdout(&output);
    assert!(text.contains("quickstart"));
    assert!(text.contains("clean"));
    assert!(text.contains("check"));
}
