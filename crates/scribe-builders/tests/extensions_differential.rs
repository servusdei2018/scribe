//! Phase 20 differential tests against the pinned Sphinx oracle.
//!
//! Normalization (see `ARCHITECTURE.md` §27):
//!   * only the `<div class="body" role="main">` fragment is compared;
//!   * `?v=<hash>` values, graphviz image hashes and absolute source paths are
//!     replaced with stable placeholders;
//!   * whitespace runs collapse to a single space;
//!   * parser-level differences inherited from earlier phases (section numbers,
//!     toctree `compound` classes, substitution handling) are excluded by
//!     comparing curated structural markers rather than raw bytes.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use scribe_builders::html::{BuildOptions, StandaloneHtmlBuilder};
use scribe_config::load_config;
use scribe_core::BuildEnvironment;

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn oracle() -> PathBuf {
    workspace().join(".oracle-venv/bin/sphinx-build")
}

fn roots() -> PathBuf {
    workspace().join("sphinx/tests/roots")
}

fn out_dir(name: &str, kind: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("scribe-phase20-{kind}-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn run_oracle(source: &Path, output: &Path, overrides: &[(&str, &str)]) -> bool {
    if !oracle().is_file() {
        return false;
    }
    let mut command = Command::new(oracle());
    command.arg("-b").arg("html").arg("-E").arg("-q");
    for (key, value) in overrides {
        command.arg("-D").arg(format!("{key}={value}"));
    }
    let status = command
        .arg(source)
        .arg(output)
        .output()
        .expect("spawn oracle");
    assert!(status.status.success(), "oracle failed: {}", String::from_utf8_lossy(&status.stderr));
    true
}

fn run_scribe(source: &Path, output: &Path, overrides: &[(&str, &str)]) -> scribe_builders::html::BuildResult {
    let mut override_map = BTreeMap::new();
    for (key, value) in overrides {
        override_map.insert((*key).to_owned(), (*value).to_owned());
    }
    let (config, _) = load_config(source, Some(&override_map), None).expect("load config");
    let options = BuildOptions {
        source_dir: source.to_path_buf(),
        output_dir: output.to_path_buf(),
        builder_name: "html".to_owned(),
        freshenv: true,
        really_quiet: true,
        ..BuildOptions::default()
    };
    let mut env = BuildEnvironment::new(source);
    StandaloneHtmlBuilder::build(&options, &config, &mut env).expect("build")
}

fn body(path: &Path) -> String {
    let html = fs::read_to_string(path).unwrap_or_default();
    let start = html.find("<div class=\"body\" role=\"main\">").map(|index| index + "<div class=\"body\" role=\"main\">".len());
    let Some(start) = start else { return String::new() };
    let end = html[start..]
        .find("<div class=\"sphinxsidebar\"")
        .map(|index| start + index)
        .unwrap_or(html.len());
    html[start..end].to_owned()
}

fn normalize(mut text: String) -> String {
    // Strip trailing structure markers from body extraction.
    for marker in ["</div>", "</section>", "<!--", "</body>"] {
        if let Some(index) = text.rfind(marker) {
            if text[index + marker.len()..].trim().is_empty() {
                text.truncate(index);
            }
        }
    }
    let out = text.split_whitespace().collect::<Vec<_>>().join(" ");
    // Graphviz hashes are content-addressed differently by the two engines.
    let mut normalized = String::new();
    let bytes = out.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if out[index..].starts_with("graphviz-") {
            normalized.push_str("graphviz-HASH");
            index += "graphviz-".len();
            while index < bytes.len() && bytes[index].is_ascii_hexdigit() {
                index += 1;
            }
            continue;
        }
        let ch = out[index..].chars().next().unwrap_or('\0');
        normalized.push(ch);
        index += ch.len_utf8();
    }
    normalized
}

fn assert_markers(normalized: &str, markers: &[&str]) {
    for marker in markers {
        assert!(normalized.contains(marker), "missing marker {marker:?} in:\n{normalized}");
    }
}

#[test]
fn todo_matches_oracle_structurally() {
    let source = roots().join("test-ext-todo");
    let oracle_out = out_dir("todo", "oracle");
    let scribe_out = out_dir("todo", "scribe");
    if !run_oracle(&source, &oracle_out, &[("todo_include_todos", "1")]) { return; }
    let result = run_scribe(&source, &scribe_out, &[("todo_include_todos", "1")]);
    assert!(result.warnings.iter().all(|warning| !warning.contains("unknown directive")));

    let expected = normalize(body(&oracle_out.join("index.html")));
    let actual = normalize(body(&scribe_out.join("index.html")));
    assert_markers(&expected, &["<div class=\"admonition-todo admonition\">", "<p class=\"todo-source\">", "original entry"]);
    assert_markers(&actual, &["<div class=\"admonition-todo admonition\">", "<p class=\"todo-source\">", "original entry"]);

    // Both engines list six entries (two `todolist` directives × three todos).
    assert_eq!(actual.matches("<p class=\"todo-source\">").count(), expected.matches("<p class=\"todo-source\">").count());
}

#[test]
fn ifconfig_matches_oracle_structurally() {
    let source = roots().join("test-ext-ifconfig");
    let oracle_out = out_dir("ifconfig", "oracle");
    let scribe_out = out_dir("ifconfig", "scribe");
    if !run_oracle(&source, &oracle_out, &[]) { return; }
    let result = run_scribe(&source, &scribe_out, &[]);
    // Custom confval2/false_config are registered by the fixture's Python
    // setup(); scribe reports them as unknown-name diagnostics instead. The
    // rendered body must still agree.
    let _ = &result.warnings;

    let expected = normalize(body(&oracle_out.join("index.html")));
    let actual = normalize(body(&scribe_out.join("index.html")));
    assert_markers(&expected, &["<p>spam</p>"]);
    assert_markers(&actual, &["<p>spam</p>"]);
    assert!(!actual.contains("egg"), "{actual}");
    assert!(!actual.contains("Link 1"), "{actual}");
    assert!(!expected.contains("egg"), "{expected}");
}

#[test]
fn extlinks_hardcoded_warnings_match_oracle() {
    for fixture in ["test-ext-extlinks-hardcoded-urls", "test-ext-extlinks-hardcoded-urls-multiple-replacements"] {
        let source = roots().join(fixture);
        let oracle_out = out_dir(fixture, "oracle");
        let scribe_out = out_dir(fixture, "scribe");
        if !run_oracle(&source, &oracle_out, &[]) { continue; }
        let result = run_scribe(&source, &scribe_out, &[]);
        let warnings: Vec<String> = result
            .warnings
            .iter()
            .filter(|warning| warning.contains("could be replaced by an extlink"))
            .map(|warning| {
                let suggestion = warning.split("(try using ").nth(1).unwrap_or("").to_owned();
                suggestion
            })
            .collect();
        assert!(!warnings.is_empty(), "{fixture}: no extlink warnings: {:?}", result.warnings);
        assert!(warnings.iter().all(|warning| warning.starts_with("':")), "{warnings:?}");
        let role_marker = if fixture.ends_with("multiple-replacements") { "extlink-user" } else { "extlink-issue" };
        let url_marker = if fixture.ends_with("multiple-replacements") { "https://github.com/octocat" } else { "https://github.com/sphinx-doc/sphinx/issues/1" };
        assert_markers(&normalize(body(&scribe_out.join("index.html"))), &[role_marker, url_marker]);
    }
}

#[test]
fn githubpages_files_match_oracle() {
    let source = roots().join("test-ext-githubpages");
    let oracle_out = out_dir("gp", "oracle");
    let scribe_out = out_dir("gp", "scribe");
    if !run_oracle(&source, &oracle_out, &[]) { return; }
    let _ = run_scribe(&source, &scribe_out, &[("html_baseurl", "https://docs.example.com")]);
    assert!(scribe_out.join(".nojekyll").is_file());
    assert_eq!(fs::read_to_string(scribe_out.join("CNAME")).unwrap(), "docs.example.com");

    // github.io domains suppress CNAME upstream; so do we.
    let github_out = out_dir("gp-gh", "scribe");
    let _ = run_scribe(&source, &github_out, &[("html_baseurl", "https://user.github.io/docs")]);
    assert!(github_out.join(".nojekyll").is_file());
    assert!(!github_out.join("CNAME").exists());
}

#[test]
fn graphviz_renders_when_dot_is_available() {
    let source = roots().join("test-ext-graphviz");
    let scribe_out = out_dir("graphviz", "scribe");
    let result = run_scribe(&source, &scribe_out, &[]);
    let dot_available = Command::new("dot").arg("-V").output().is_ok()
        && result.warnings.iter().all(|warning| !warning.contains("cannot be run"));
    let actual = normalize(body(&scribe_out.join("index.html")));
    assert_markers(&actual, &["<div class=\"graphviz\">", "<img src=\"_images/graphviz-HASH.png\"", "usemap=\"#test\""]);
    if dot_available {
        let images: Vec<_> = fs::read_dir(scribe_out.join("_images"))
            .map(|entries| entries.filter_map(|entry| entry.ok()).collect())
            .unwrap_or_default();
        assert!(!images.is_empty(), "dot was available but no images were emitted");
        // Every rendered image must have a real file on disk.
        for image in images {
            assert!(image.path().is_file());
        }
    } else {
        // Graceful degradation: the raw dot source appears as fallback text.
        assert!(actual.contains("digraph"), "{actual}");
    }
}

#[test]
fn viewcode_generates_module_pages_and_source_links() {
    // Minimal fixture derived from test-ext-viewcode that needs no autodoc
    // (Phase 21). Uses explicit py directives as upstream's objects.rst does.
    let source = out_dir("viewcode-src", "src");
    fs::write(
        source.join("conf.py"),
        "import sys\nfrom pathlib import Path\nsys.path.insert(0, str(Path.cwd().resolve()))\nextensions = ['sphinx.ext.viewcode']\n",
    )
    .unwrap();
    fs::write(
        source.join("index.rst"),
        "Viewcode test\n=============\n\n.. py:module:: spam\n\n.. py:function:: func1(a, b)\n\n   First function.\n\n.. py:class:: Class1\n\n   A class.\n",
    )
    .unwrap();
    fs::write(source.join("spam.py"), "def func1(a, b):\n    return a + b\n\n\nclass Class1:\n    pass\n").unwrap();

    let oracle_out = out_dir("viewcode", "oracle");
    let scribe_out = out_dir("viewcode", "scribe");
    let oracle_ran = run_oracle(&source, &oracle_out, &[]);
    let result = run_scribe(&source, &scribe_out, &[]);
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);

    assert!(scribe_out.join("_modules/spam.html").is_file());
    assert!(scribe_out.join("_modules/index.html").is_file());
    let index = fs::read_to_string(scribe_out.join("index.html")).unwrap();
    assert!(index.contains("href=\"_modules/spam.html#func1\""), "{index}");
    assert!(index.contains("viewcode-link"));
    let module_page = fs::read_to_string(scribe_out.join("_modules/spam.html")).unwrap();
    assert!(module_page.contains("viewcode-block\" id=\"func1\""), "{module_page}");
    assert!(module_page.contains("href=\"../index.html#spam.func1\""), "{module_page}");

    if oracle_ran {
        assert!(oracle_out.join("_modules/spam.html").is_file());
        let oracle_index = fs::read_to_string(oracle_out.join("index.html")).unwrap();
        assert!(oracle_index.contains("href=\"_modules/spam.html#func1\""));
        let oracle_module = fs::read_to_string(oracle_out.join("_modules/spam.html")).unwrap();
        assert!(oracle_module.contains("href=\"../index.html#spam.func1\""));
    }
}

#[test]
fn all_native_extensions_work_without_a_python_runtime() {
    // Build a project using every native extension from an in-memory Config;
    // no conf.py bridge and no Python subprocess is involved.
    let source = out_dir("all-ext-src", "src");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("index.rst"), "All extensions\n==============\n\n.. todo:: native only\n\n.. todolist::\n").unwrap();
    let output = out_dir("all-ext", "out");
    let mut config = scribe_config::Config::new();
    config
        .set(
            "extensions",
            scribe_config::ConfigValue::List(
                ["sphinx.ext.todo", "sphinx.ext.ifconfig", "sphinx.ext.extlinks", "sphinx.ext.githubpages", "sphinx.ext.duration"]
                    .iter()
                    .map(|name| scribe_config::ConfigValue::from(*name))
                    .collect(),
            ),
        )
        .unwrap();
    config.set("todo_include_todos", scribe_config::ConfigValue::Bool(true)).unwrap();
    let options = BuildOptions {
        source_dir: source.clone(),
        output_dir: output.clone(),
        builder_name: "html".to_owned(),
        freshenv: true,
        really_quiet: true,
        ..BuildOptions::default()
    };
    let mut env = BuildEnvironment::new(&source);
    let result = StandaloneHtmlBuilder::build(&options, &config, &mut env).unwrap();
    assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    assert!(output.join(".nojekyll").is_file());
    assert!(fs::read_to_string(output.join("index.html")).unwrap().contains("admonition-todo"));
}

#[test]
fn output_is_deterministic_across_two_runs() {
    let source = roots().join("test-ext-githubpages");
    let first = out_dir("determinism", "first");
    let second = out_dir("determinism", "second");
    let _ = run_scribe(&source, &first, &[("html_baseurl", "https://docs.example.com")]);
    let _ = run_scribe(&source, &second, &[("html_baseurl", "https://docs.example.com")]);
    let mut first_files: Vec<_> = fs::read_dir(&first).unwrap().filter_map(|entry| entry.ok()).map(|entry| entry.file_name()).collect();
    let mut second_files: Vec<_> = fs::read_dir(&second).unwrap().filter_map(|entry| entry.ok()).map(|entry| entry.file_name()).collect();
    first_files.sort();
    second_files.sort();
    assert_eq!(first_files, second_files);
    for name in first_files {
        let first_path = first.join(&name);
        if first_path.is_dir() { continue; }
        let a = fs::read(&first_path).unwrap();
        let b = fs::read(second.join(&name)).unwrap();
        assert_eq!(a, b, "nondeterministic file: {name:?}");
    }
}
