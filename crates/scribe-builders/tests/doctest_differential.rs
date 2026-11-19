//! Differential tests: oracle `sphinx-build -b doctest` vs scribe.
//!
//! The two fixture roots under `tests/fixtures/` are faithful, self-contained
//! derivatives of Sphinx's `test-ext-doctest` and `test-ext-doctest-skipif`
//! roots. The only transformation is replacing the cleanup hook that imports
//! `tests.test_extensions` (which requires pytest, absent from the oracle
//! venv) with an inline `record()` helper. All directives, groups, options and
//! expected outputs are preserved, so `output.txt` is compared byte-for-byte
//! after dropping only the timestamp header (which necessarily differs).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use scribe_builders::doctest::DoctestBuilder;
use scribe_config::{Config, ConfigValue};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn fixture(name: &str) -> PathBuf {
    workspace().join("crates/scribe-builders/tests/fixtures").join(name)
}

fn config(name: &str) -> Config {
    let mut config = Config::new();
    config
        .set("extensions", ConfigValue::List(vec![ConfigValue::from("sphinx.ext.doctest")]))
        .unwrap();
    config
        .set(
            "source_suffix",
            ConfigValue::Dict(std::collections::BTreeMap::from([
                (".rst".to_owned(), ConfigValue::from("restructuredtext")),
                (".txt".to_owned(), ConfigValue::from("restructuredtext")),
            ])),
        )
        .unwrap();
    if name == "doctest" {
        config
            .set(
                "doctest_global_setup",
                ConfigValue::from("CALLS = []\ndef record(kind, value):\n    CALLS.append((kind, value))\n    return value\n"),
            )
            .unwrap();
    } else {
        config
            .set(
                "doctest_global_setup",
                ConfigValue::from("LOG = []\ndef record(kind, value):\n    LOG.append((kind, value))\n    return 'recorded ' + str(value)\n"),
            )
            .unwrap();
    }
    config
}

fn temp_output(tag: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "scribe-doctest-diff-{tag}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&path);
    path
}

fn normalized(output: &Path) -> String {
    let text = fs::read_to_string(output.join("output.txt")).unwrap();
    text.split_once('\n').map_or("", |(_, rest)| rest).to_owned()
}

fn run_oracle(src: &Path, out: &Path) -> bool {
    let status = Command::new(workspace().join(".oracle-venv/bin/sphinx-build"))
        .args(["-b", "doctest", "-E", "-q"])
        .arg(src)
        .arg(out)
        .output()
        .expect("spawn oracle");
    status.status.success()
}

#[test]
fn output_txt_matches_oracle_exactly() {
    for name in ["doctest", "doctest-skipif"] {
        let src = fixture(name);
        let oracle_out = temp_output(&format!("{name}-oracle"));
        let scribe_out = temp_output(&format!("{name}-scribe"));
        assert!(run_oracle(&src, &oracle_out), "oracle failed for {name}");

        let cfg = config(name);
        let options = scribe_builders::html::BuildOptions {
            source_dir: src.clone(),
            output_dir: scribe_out.clone(),
            builder_name: "doctest".to_owned(),
            freshenv: true,
            really_quiet: true,
            ..Default::default()
        };
        let mut env = scribe_core::BuildEnvironment::with_config(&src, &cfg);
        let result = DoctestBuilder::build(&options, &cfg, &mut env).unwrap();
        assert!(result.success(), "{name}: scribe had failures:\n{}", result.output);

        let expected = normalized(&oracle_out);
        let actual = normalized(&scribe_out);
        assert_eq!(actual, expected, "{name}: output.txt differs (oracle left, scribe right)");
        let _ = fs::remove_dir_all(oracle_out);
        let _ = fs::remove_dir_all(scribe_out);
    }
}

#[test]
fn real_test_ext_doctest_root_extraction_counts_match() {
    // The upstream root's cleanup imports `tests.test_extensions`, which is
    // unavailable in the pinned venv. Strip only that directive and verify the
    // remaining 22 examples are discovered identically by both engines.
    let original = workspace().join("sphinx/tests/roots/test-ext-doctest");
    let src = temp_output("real-doctest-src");
    fs::create_dir_all(&src).unwrap();
    for entry in fs::read_dir(&original).unwrap().flatten() {
        let name = entry.file_name();
        if name == "conf.py" || name == "doctest.txt" {
            fs::copy(entry.path(), src.join(name)).unwrap();
        }
    }
    let doctest = fs::read_to_string(src.join("doctest.txt")).unwrap();
    let trimmed = doctest
        .split(".. testcleanup:: *")
        .next()
        .unwrap_or(&doctest)
        .to_owned();
    fs::write(src.join("doctest.txt"), trimmed).unwrap();

    let oracle_out = temp_output("real-doctest-oracle");
    let scribe_out = temp_output("real-doctest-scribe");
    assert!(run_oracle(&src, &oracle_out));

    let cfg = config("doctest");
    let options = scribe_builders::html::BuildOptions {
        source_dir: src.clone(),
        output_dir: scribe_out.clone(),
        builder_name: "doctest".to_owned(),
        freshenv: true,
        really_quiet: true,
        ..Default::default()
    };
    let mut env = scribe_core::BuildEnvironment::with_config(&src, &cfg);
    let result = DoctestBuilder::build(&options, &cfg, &mut env).unwrap();
    assert!(result.success(), "{}", result.output);

    let oracle_summary: u32 = fs::read_to_string(oracle_out.join("output.txt"))
        .unwrap()
        .lines()
        .find(|line| line.trim_end().ends_with("tests"))
        .and_then(|line| line.split_whitespace().next())
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    assert_eq!(result.total_tries, oracle_summary, "oracle/scribe test counts differ");
    let _ = fs::remove_dir_all(src);
    let _ = fs::remove_dir_all(oracle_out);
    let _ = fs::remove_dir_all(scribe_out);
}
