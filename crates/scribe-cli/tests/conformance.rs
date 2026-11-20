//! Phase 29 automated upstream-conformance harness.
//!
//! For a documented, representative subset of `sphinx/tests/roots`, builds the
//! fixture with both the pinned Python Sphinx oracle and the compiled `scribe`
//! binary, then computes a **semantic parity score** from a deterministic
//! rubric (doctree/DOM structure + normalized warning lines).  The measured
//! scores are emitted to a deterministic `CONFORMANCE.md`, and a committed
//! per-fixture baseline acts as a *regression guard*: a fixture that used to
//! pass may not silently drop to a worse status.
//!
//! The test does **not** fail merely because parity is below an aspirational
//! threshold (parity is a tracked metric, not a gate).  It fails only if the
//! harness cannot run, the report is non-deterministic, the baseline is
//! missing/incomplete, or a fixture regressed.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use rayon::prelude::*;

const ORACLE_RELATIVE: &str = ".oracle-venv/bin/sphinx-build";
const FIXTURES_RELATIVE: &str = "sphinx/tests/roots";
const BASELINE_RELATIVE: &str = "crates/scribe-cli/tests/fixtures/conformance_baseline.txt";
const REPORT_NAME: &str = "CONFORMANCE.md";
const PER_BUILD_TIMEOUT: Duration = Duration::from_secs(90);

/// A fixture selected for the campaign and the builder used to exercise it.
#[derive(Clone, Copy)]
struct Fixture {
    name: &'static str,
    builder: &'static str,
    category: &'static str,
    /// Set only for fixtures whose upstream build needs a feature the local
    /// environment cannot provide (network, external binaries, import-heavy
    /// third-party extensions). Recorded verbatim in the report as a SKIP.
    skip: Option<&'static str>,
}

// Selection criteria (mirrored into the generated report):
//  1. Deterministic, offline builds with no third-party services.
//  2. Cover every parser/directive family: markup & inline, lists/tables,
//     footnotes/citations, admonitions, code/highlighting, include/raw/only,
//     toctree, glossary/productionlist, themes.
//  3. Cover every domain we implement (py, c, cpp, js) plus xref warnings.
//  4. Use `pseudoxml` for doctree-structure parity and `html` for DOM-marker
//     parity on a representative handful.
//  5. Keep the total build count modest so the harness runs in normal CI.
const FIXTURES: &[Fixture] = &[
    // --- RST parser / inline / lists / tables -------------------------------
    Fixture { name: "test-basic", builder: "pseudoxml", category: "parser", skip: None },
    Fixture { name: "test-footnotes", builder: "pseudoxml", category: "parser", skip: None },
    Fixture { name: "test-smartquotes", builder: "pseudoxml", category: "parser", skip: None },
    Fixture { name: "test-markup-rubric", builder: "pseudoxml", category: "parser", skip: None },
    Fixture { name: "test-markup-citation", builder: "pseudoxml", category: "parser", skip: None },
    Fixture { name: "test-nested-enumerated-list", builder: "pseudoxml", category: "parser", skip: None },
    Fixture { name: "test-nested-tables", builder: "pseudoxml", category: "parser", skip: None },
    Fixture { name: "test-maxlistdepth", builder: "pseudoxml", category: "parser", skip: None },
    Fixture { name: "test-prolog", builder: "pseudoxml", category: "parser", skip: None },
    Fixture { name: "test-default_role", builder: "pseudoxml", category: "parser", skip: None },
    Fixture { name: "test-directive-csv-table", builder: "pseudoxml", category: "directive", skip: Some("root has no index.rst; upstream supplies one via its testroot copy step") },
    Fixture { name: "test-directive-include", builder: "pseudoxml", category: "directive", skip: Some("include paths assume Sphinx's temp testroot layout, not a bare roots/ directory") },
    Fixture { name: "test-directive-only", builder: "pseudoxml", category: "directive", skip: None },
    Fixture { name: "test-directives-raw", builder: "pseudoxml", category: "directive", skip: None },
    Fixture { name: "test-directives-admonition-collapse", builder: "pseudoxml", category: "directive", skip: None },
    Fixture { name: "test-glossary", builder: "pseudoxml", category: "directive", skip: None },
    Fixture { name: "test-productionlist", builder: "pseudoxml", category: "directive", skip: None },
    Fixture { name: "test-numfig", builder: "pseudoxml", category: "directive", skip: None },
    // --- code / highlighting (HTML DOM markers) -----------------------------
    Fixture { name: "test-directive-code", builder: "html", category: "directive", skip: None },
    Fixture { name: "test-highlight_options", builder: "html", category: "directive", skip: None },
    Fixture { name: "test-reST-code-block", builder: "html", category: "parser", skip: None },
    Fixture { name: "test-reST-code-role", builder: "html", category: "parser", skip: None },
    // --- domains ------------------------------------------------------------
    Fixture { name: "test-domain-py", builder: "pseudoxml", category: "domain", skip: None },
    Fixture { name: "test-domain-py-xref-warning", builder: "pseudoxml", category: "domain", skip: None },
    Fixture { name: "test-domain-py-xref-type-alias", builder: "pseudoxml", category: "domain", skip: None },
    Fixture { name: "test-domain-py-python_use_unqualified_type_names", builder: "pseudoxml", category: "domain", skip: None },
    Fixture { name: "test-domain-c", builder: "pseudoxml", category: "domain", skip: None },
    Fixture { name: "test-domain-cpp", builder: "pseudoxml", category: "domain", skip: None },
    Fixture { name: "test-domain-js", builder: "pseudoxml", category: "domain", skip: None },
    // --- toctree / builders / themes ---------------------------------------
    Fixture { name: "test-toctree", builder: "pseudoxml", category: "builder", skip: None },
    Fixture { name: "test-toctree-glob", builder: "pseudoxml", category: "builder", skip: None },
    Fixture { name: "test-toctree-maxdepth", builder: "html", category: "builder", skip: None },
    Fixture { name: "test-theming", builder: "html", category: "builder", skip: None },
    Fixture { name: "test-changes", builder: "pseudoxml", category: "builder", skip: None },
    Fixture { name: "test-basic", builder: "html", category: "builder", skip: None },
    Fixture { name: "test-root", builder: "html", category: "builder", skip: None },
    // --- explicitly skipped (unavailable features) --------------------------
    Fixture { name: "test-linkcheck", builder: "html", category: "builder", skip: Some("requires outbound network access for URL checks") },
    Fixture { name: "test-ext-graphviz", builder: "html", category: "directive", skip: Some("requires the external `dot`/graphviz binary") },
    Fixture { name: "test-ext-autodoc", builder: "pseudoxml", category: "domain", skip: Some("import-heavy third-party Python extension fixture") },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Status {
    Fail,
    Partial,
    Pass,
    Skip,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Partial => "partial",
            Status::Fail => "fail",
            Status::Skip => "skip",
        }
    }
}

#[derive(Clone, Debug)]
struct FixtureResult {
    name: &'static str,
    builder: &'static str,
    category: &'static str,
    status: Status,
    score: f64,
    structure: f64,
    warnings: f64,
    oracle_exit: i32,
    scribe_exit: i32,
    oracle_tags: usize,
    scribe_tags: usize,
    divergences: Vec<String>,
    skip_reason: Option<String>,
}

#[derive(Clone, Debug)]
struct BuildCapture {
    exit: i32,
    timed_out: bool,
    stderr: String,
    tags: BTreeMap<String, usize>,
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn require_oracle(root: &Path) -> PathBuf {
    let oracle = root.join(ORACLE_RELATIVE);
    assert!(oracle.is_file(), "Python Sphinx oracle not found at {}", oracle.display());
    oracle
}

/// Run a child process with a hard timeout, killing it on expiry so no stray
/// `sphinx-build`/`scribe` process can outlive the harness.
fn run_with_timeout(mut command: Command) -> std::io::Result<Output> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let start = Instant::now();
    loop {
        match child.try_wait()? {
            Some(_) => return child.wait_with_output(),
            None if start.elapsed() >= PER_BUILD_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "build exceeded the per-build timeout",
                ));
            }
            None => thread::sleep(Duration::from_millis(25)),
        }
    }
}

fn collect_files(directory: &Path, extension: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, extension, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some(extension) {
            out.push(path);
        }
    }
}

/// Extract a multiset of element tag names from generated pseudoxml/HTML.
fn extract_tags(text: &str) -> BTreeMap<String, usize> {
    let bytes = text.as_bytes();
    let mut tags = BTreeMap::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        if j >= bytes.len() {
            break;
        }
        if matches!(bytes[j], b'!' | b'?' | b'/') {
            i = j;
            continue;
        }
        let start = j;
        while j < bytes.len()
            && (bytes[j].is_ascii_alphanumeric()
                || bytes[j] == b'_'
                || bytes[j] == b'-'
                || bytes[j] == b':')
        {
            j += 1;
        }
        if j > start {
            // Tag names are ASCII by construction, so this slice is safe.
            let tag = &text[start..j];
            *tags.entry(tag.to_owned()).or_insert(0) += 1;
        }
        i = j;
    }
    tags
}

fn build_capture(binary: &Path, builder: &str, source: &Path, out: &Path) -> BuildCapture {
    let _ = fs::remove_dir_all(out);
    if let Err(error) = fs::create_dir_all(out) {
        return BuildCapture {
            exit: -1,
            timed_out: false,
            stderr: format!("cannot create output dir: {error}"),
            tags: BTreeMap::new(),
        };
    }
    let mut command = Command::new(binary);
    // Python hash randomization changes Sphinx's iteration order for sets such
    // as `env.numbered_toctrees`, which in turn moves the source location of
    // `toc.secnum` diagnostics between runs. Pin the seed so the oracle's
    // output (and therefore the tracked metric) is reproducible.
    command.env("PYTHONHASHSEED", "0");
    command
        .arg("build")
        .arg("-b")
        .arg(builder)
        .arg("-E")
        .arg("-q")
        .arg("-n")
        .arg(source)
        .arg(out);
    let output = match run_with_timeout(command) {
        Ok(output) => output,
        Err(error) => {
            return BuildCapture {
                exit: -1,
                timed_out: error.kind() == std::io::ErrorKind::TimedOut,
                stderr: format!("build failed to run: {error}"),
                tags: BTreeMap::new(),
            }
        }
    };
    let mut files = Vec::new();
    let extension = if builder == "html" { "html" } else { "pseudoxml" };
    collect_files(out, extension, &mut files);
    files.sort();
    let mut tags = BTreeMap::new();
    for file in files {
        if let Ok(text) = fs::read_to_string(&file) {
            for (tag, count) in extract_tags(&text) {
                *tags.entry(tag).or_insert(0) += count;
            }
        }
    }
    BuildCapture {
        exit: output.status.code().unwrap_or(-1),
        timed_out: false,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        tags,
    }
}

/// Normalize warning lines so absolute paths and scribe's diagnostic prefix do
/// not masquerade as divergences.
fn normalized_warnings(stderr: &str, root: &Path, out: &Path) -> BTreeSet<String> {
    let root_text = root.to_string_lossy();
    let out_text = out.to_string_lossy();
    let mut lines = BTreeSet::new();
    for raw in stderr.lines() {
        let mut line = raw.trim().to_owned();
        if line.is_empty() {
            continue;
        }
        line = line.replace(out_text.as_ref(), "<OUT>");
        line = line.replace(root_text.as_ref(), "<ROOT>");
        if let Some(rest) = line.strip_prefix("scribe: warning: ") {
            line = rest.to_owned();
        }
        lines.insert(line);
    }
    lines
}

fn multiset_jaccard(a: &BTreeMap<String, usize>, b: &BTreeMap<String, usize>) -> f64 {
    let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    if keys.is_empty() {
        return 1.0;
    }
    let mut intersection = 0u64;
    let mut union = 0u64;
    for key in keys {
        let ca = a.get(key).copied().unwrap_or(0) as u64;
        let cb = b.get(key).copied().unwrap_or(0) as u64;
        intersection += ca.min(cb);
        union += ca.max(cb);
    }
    if union == 0 { 1.0 } else { intersection as f64 / union as f64 }
}

fn set_jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let intersection = a.intersection(b).count() as u64;
    let union = a.union(b).count() as u64;
    if union == 0 { 1.0 } else { intersection as f64 / union as f64 }
}

fn categorize_tag(tag: &str) -> &'static str {
    if tag.starts_with("desc_")
        || tag.starts_with("py:")
        || tag.starts_with("cpp:")
        || matches!(tag, "py" | "cpp" | "c" | "js" | "std" | "pending_xref" | "reference" | "literal_emphasis")
    {
        "domain"
    } else if matches!(
        tag,
        "admonition" | "note" | "warning" | "tip" | "important" | "caution" | "danger"
            | "error" | "hint" | "attention" | "seealso" | "figure" | "table" | "tgroup"
            | "toctree" | "only" | "glossary" | "productionlist" | "production" | "topic"
            | "sidebar" | "container" | "rubric" | "versionmodified" | "index"
            | "compact_paragraph" | "hlist" | "centered" | "acks"
    ) {
        "directive"
    } else if matches!(
        tag,
        "literal_block" | "highlight" | "paragraph" | "section" | "title" | "bullet_list"
            | "enumerated_list" | "definition_list" | "field_list" | "option_list"
            | "emphasis" | "strong" | "literal" | "footnote" | "citation" | "inline"
            | "problematic" | "raw" | "subtitle" | "transition" | "block_quote"
    ) {
        "parser"
    } else {
        "html classes"
    }
}

fn divergences(
    oracle: &BTreeMap<String, usize>,
    scribe: &BTreeMap<String, usize>,
    warnings_differ: bool,
) -> Vec<String> {
    let mut categories = BTreeSet::new();
    let keys: BTreeSet<&String> = oracle.keys().chain(scribe.keys()).collect();
    for key in keys {
        if oracle.get(key) != scribe.get(key) {
            categories.insert(categorize_tag(key).to_owned());
        }
    }
    if warnings_differ {
        categories.insert("warning codes".to_owned());
    }
    categories.into_iter().collect()
}

fn evaluate(fixture: &Fixture, oracle: &Path, scribe: &Path, root: &Path, slot: usize) -> FixtureResult {
    if let Some(reason) = fixture.skip {
        return FixtureResult {
            name: fixture.name,
            builder: fixture.builder,
            category: fixture.category,
            status: Status::Skip,
            score: 0.0,
            structure: 0.0,
            warnings: 0.0,
            oracle_exit: 0,
            scribe_exit: 0,
            oracle_tags: 0,
            scribe_tags: 0,
            divergences: Vec::new(),
            skip_reason: Some(reason.to_owned()),
        };
    }

    let source = root.join(FIXTURES_RELATIVE).join(fixture.name);
    let tag = format!("{}-{}", std::process::id(), slot);
    let oracle_out = std::env::temp_dir().join(format!("scribe-conf-oracle-{tag}"));
    let scribe_out = std::env::temp_dir().join(format!("scribe-conf-scribe-{tag}"));

    let oracle_capture = build_capture(oracle, fixture.builder, &source, &oracle_out);
    let scribe_capture = build_capture(scribe, fixture.builder, &source, &scribe_out);

    if oracle_capture.exit != 0 || oracle_capture.timed_out {
        // The fixture itself could not be built by the reference engine:
        // record it as a skip with the captured reason rather than counting a
        // false divergence against scribe.
        return FixtureResult {
            name: fixture.name,
            builder: fixture.builder,
            category: fixture.category,
            status: Status::Skip,
            score: 0.0,
            structure: 0.0,
            warnings: 0.0,
            oracle_exit: oracle_capture.exit,
            scribe_exit: scribe_capture.exit,
            oracle_tags: oracle_capture.tags.len(),
            scribe_tags: scribe_capture.tags.len(),
            divergences: Vec::new(),
            skip_reason: Some(format!(
                "oracle build failed (exit {}, timed_out={}): {}",
                oracle_capture.exit,
                oracle_capture.timed_out,
                oracle_capture.stderr.lines().take(2).collect::<Vec<_>>().join(" | ")
            )),
        };
    }

    let structure = multiset_jaccard(&oracle_capture.tags, &scribe_capture.tags);
    let oracle_warnings = normalized_warnings(&oracle_capture.stderr, root, &oracle_out);
    let scribe_warnings = normalized_warnings(&scribe_capture.stderr, root, &scribe_out);
    let warnings = set_jaccard(&oracle_warnings, &scribe_warnings);
    // A failed or timed-out scribe build cannot have matching structure.
    let structure = if scribe_capture.exit == 0 && !scribe_capture.timed_out { structure } else { 0.0 };
    let score = 0.85 * structure + 0.15 * warnings;
    let status = if score >= 0.90 {
        Status::Pass
    } else if score >= 0.50 {
        Status::Partial
    } else {
        Status::Fail
    };
    let diverges = divergences(
        &oracle_capture.tags,
        &scribe_capture.tags,
        oracle_warnings != scribe_warnings,
    );

    // Clean up this slot's output directories.
    let _ = fs::remove_dir_all(&oracle_out);
    let _ = fs::remove_dir_all(&scribe_out);

    FixtureResult {
        name: fixture.name,
        builder: fixture.builder,
        category: fixture.category,
        status,
        score,
        structure,
        warnings,
        oracle_exit: oracle_capture.exit,
        scribe_exit: scribe_capture.exit,
        oracle_tags: oracle_capture.tags.len(),
        scribe_tags: scribe_capture.tags.len(),
        divergences: diverges,
        skip_reason: None,
    }
}

fn run_campaign(root: &Path, oracle: &Path, scribe: &Path) -> Vec<FixtureResult> {
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(2, 8);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .expect("cannot build bounded rayon pool");
    pool.install(|| {
        FIXTURES
            .par_iter()
            .enumerate()
            .map(|(slot, fixture)| evaluate(fixture, oracle, scribe, root, slot))
            .collect::<Vec<_>>()
    })
}

fn aggregate(results: &[FixtureResult]) -> (f64, usize, usize, usize, usize) {
    let mut sum = 0.0;
    let mut count = 0usize;
    let mut pass = 0;
    let mut partial = 0;
    let mut fail = 0;
    let mut skip = 0;
    for result in results {
        match result.status {
            Status::Skip => skip += 1,
            Status::Pass => pass += 1,
            Status::Partial => partial += 1,
            Status::Fail => fail += 1,
        }
        if result.status != Status::Skip {
            sum += result.score;
            count += 1;
        }
    }
    let percent = if count == 0 { 0.0 } else { sum / count as f64 * 100.0 };
    (percent, pass, partial, fail, skip)
}

fn format_report(results: &[FixtureResult], oracle_version: &str) -> String {
    // The formatter sorts internally, so callers may pass results in any
    // order and still obtain byte-identical output (the determinism check).
    let mut sorted = results.to_vec();
    sorted.sort_by(|a, b| (a.name, a.builder).cmp(&(b.name, b.builder)));
    let (percent, pass, partial, fail, skip) = aggregate(&sorted);

    let mut out = String::new();
    out.push_str("# scribe conformance report\n\n");
    out.push_str("> Generated by `cargo test -p scribe-cli --test conformance`.\n");
    out.push_str("> This is a **tracked metric**, not a pass/fail gate. The per-fixture\n");
    out.push_str("> baseline in `crates/scribe-cli/tests/fixtures/conformance_baseline.txt`\n");
    out.push_str("> is the regression guard.\n\n");
    out.push_str(&format!("Oracle: `{oracle_version}`\n\n"));
    out.push_str("## Selection criteria\n\n");
    out.push_str(
        "1. Deterministic, offline builds with no third-party services.  \n\
         2. Cover every parser/directive family: markup & inline, lists/tables,  \n   \
         footnotes/citations, admonitions, code/highlighting, include/raw/only,  \n   \
         toctree, glossary/productionlist, themes.  \n\
         3. Cover every implemented domain (py, c, cpp, js) plus xref warnings.  \n\
         4. `pseudoxml` compares doctree structure; `html` compares DOM markers.  \n\
         5. Keep the build count modest enough for regular CI runs.\n\n",
    );
    out.push_str("## Rubric\n\n");
    out.push_str(
        "- **Structure score** = multiset Jaccard overlap of element tag names\n  \
         (`pseudoxml` doctree tags or HTML tag names) between oracle and scribe.\n\
         - **Warning score** = Jaccard overlap of normalized warning lines\n  \
         (absolute paths and the `scribe: warning:` prefix removed; sorted).\n\
         - **Fixture score** = `0.85 * structure + 0.15 * warnings`.\n\
         - **Status**: `pass` >= 0.90, `partial` >= 0.50, else `fail`; fixtures\n  \
         needing unavailable features are `skip` with a recorded reason.\n\
         - **Aggregate parity** = mean fixture score over non-skipped fixtures.\n\n",
    );
    out.push_str(&format!(
        "## Aggregate semantic parity: {percent:.1}% over {} applicable fixtures \
         ({} pass, {} partial, {} fail, {} skip)\n\n",
        if pass + partial + fail == 0 { 0 } else { pass + partial + fail },
        pass,
        partial,
        fail,
        skip
    ));
    out.push_str("## Per-fixture results\n\n");
    out.push_str("| Fixture | Builder | Category | Status | Score | Struct | Warn | Tags (o/s) | Exit (o/s) | Divergences |\n");
    out.push_str("|---|---|---|---|---|---|---|---|---|\n");
    for result in &sorted {
        let divergences = if result.divergences.is_empty() {
            "—".to_owned()
        } else {
            result.divergences.join(", ")
        };
        out.push_str(&format!(
            "| {} | {} | {} | {} | {:.3} | {:.3} | {:.3} | {}/{} | {}/{} | {} |\n",
            result.name,
            result.builder,
            result.category,
            result.status.label(),
            result.score,
            result.structure,
            result.warnings,
            result.oracle_tags,
            result.scribe_tags,
            result.oracle_exit,
            result.scribe_exit,
            divergences
        ));
    }
    out.push('\n');

    if skip > 0 {
        out.push_str("## Skipped fixtures\n\n");
        for result in sorted.iter().filter(|r| r.status == Status::Skip) {
            out.push_str(&format!(
                "- `{}` ({}): {}\n",
                result.name,
                result.builder,
                result.skip_reason.as_deref().unwrap_or("unspecified")
            ));
        }
        out.push('\n');
    }

    out.push_str("## Divergences by category\n\n");
    let mut by_category: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for result in &sorted {
        for category in &result.divergences {
            by_category
                .entry(category.clone())
                .or_default()
                .push(format!("{} ({})", result.name, result.builder));
        }
    }
    if by_category.is_empty() {
        out.push_str("- none\n");
    } else {
        for (category, fixtures) in &by_category {
            out.push_str(&format!("- **{category}**: {}\n", fixtures.join(", ")));
        }
    }
    out.push('\n');
    out
}

fn baseline_lines(results: &[FixtureResult]) -> String {
    let mut sorted = results.to_vec();
    sorted.sort_by(|a, b| (a.name, a.builder).cmp(&(b.name, b.builder)));
    let mut out = String::new();
    for result in &sorted {
        out.push_str(&format!(
            "{}\t{}\t{:.6}\t{}\n",
            result.name,
            result.builder,
            result.score,
            result.status.label()
        ));
    }
    out
}

fn load_baseline(root: &Path, results: &[FixtureResult]) -> BTreeMap<(String, String), (f64, Status)> {
    let path = root.join(BASELINE_RELATIVE);
    let text = fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "conformance baseline missing at {} ({error}).\n\
             The harness generated the following baseline; commit it to \
             `{}`:\n{}",
            path.display(),
            BASELINE_RELATIVE,
            baseline_lines(results)
        )
    });
    let mut map = BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        assert_eq!(
            fields.len(),
            4,
            "malformed baseline line {} in {}: {line:?}",
            index + 1,
            path.display()
        );
        let score: f64 = fields[2].parse().unwrap_or_else(|_| panic!("bad baseline score: {}", fields[2]));
        let status = match fields[3] {
            "pass" => Status::Pass,
            "partial" => Status::Partial,
            "fail" => Status::Fail,
            "skip" => Status::Skip,
            other => panic!("bad baseline status: {other}"),
        };
        map.insert((fields[0].to_owned(), fields[1].to_owned()), (score, status));
    }
    assert!(!map.is_empty(), "empty conformance baseline at {}", path.display());
    map
}

fn oracle_version(oracle: &Path) -> String {
    let output = Command::new(oracle)
        .arg("--version")
        .output()
        .expect("sphinx-build --version must run");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[test]
fn conformance_report_is_deterministic_and_regression_free() {
    let root = workspace_root();
    let oracle = require_oracle(&root);
    let scribe = PathBuf::from(env!("CARGO_BIN_EXE_scribe"));
    assert!(scribe.is_file(), "compiled scribe binary not found at {}", scribe.display());

    let results = run_campaign(&root, &oracle, &scribe);
    assert_eq!(results.len(), FIXTURES.len(), "not every fixture was evaluated");
    assert!(
        results.iter().any(|r| r.status != Status::Skip),
        "the harness skipped every fixture"
    );

    // 1. The report must be generated and deterministic: reformatting a
    //    reversed result set must yield byte-identical output.
    let report = format_report(&results, &oracle_version(&oracle));
    let mut reversed = results.clone();
    reversed.reverse();
    assert_eq!(
        report,
        format_report(&reversed, &oracle_version(&oracle)),
        "the conformance report is not deterministic"
    );
    assert!(report.contains("Aggregate semantic parity:"), "report missing aggregate");
    assert!(report.contains("| Fixture | Builder |"), "report missing result table");
    assert!(report.contains("## Divergences by category"), "report missing divergences");

    let report_path = root.join(REPORT_NAME);
    fs::write(&report_path, &report).unwrap_or_else(|error| {
        panic!("cannot write conformance report at {}: {error}", report_path.display())
    });
    assert_eq!(
        fs::read_to_string(&report_path).expect("re-read report"),
        report,
        "written report differs from in-memory report"
    );

    // 2. Regression guard: no fixture that passed (or partially passed) in the
    //    committed baseline may fall to a worse status, and scores may not drop
    //    by more than the documented tolerance (0.15).
    let baseline = load_baseline(&root, &results);
    for result in &results {
        let key = (result.name.to_owned(), result.builder.to_owned());
        let (baseline_score, baseline_status) = baseline
            .get(&key)
            .unwrap_or_else(|| panic!("fixture {:?} is missing from the conformance baseline", key));
        let current_rank = if result.status == Status::Skip { *baseline_status } else { result.status };
        assert!(
            current_rank >= *baseline_status,
            "conformance regression for {:?}: baseline {:?} -> current {:?}",
            key,
            baseline_status,
            result.status
        );
        if result.status != Status::Skip {
            assert!(
                result.score + 0.15 >= *baseline_score,
                "score regression for {:?}: baseline {:.3} -> current {:.3}",
                key,
                baseline_score,
                result.score
            );
        }
    }
    assert_eq!(
        baseline.len(),
        results.len(),
        "baseline has {} rows but the campaign evaluated {} fixtures",
        baseline.len(),
        results.len()
    );
}

#[test]
fn harness_selection_covers_all_categories_and_builders() {
    // A cheap structural self-check that does not invoke the oracle.
    let categories: BTreeSet<&str> = FIXTURES.iter().map(|f| f.category).collect();
    for expected in ["parser", "directive", "domain", "builder"] {
        assert!(categories.contains(expected), "selection missing category {expected}");
    }
    let builders: BTreeSet<&str> = FIXTURES.iter().map(|f| f.builder).collect();
    assert!(builders.contains("pseudoxml") && builders.contains("html"));
    assert!(FIXTURES.iter().any(|f| f.skip.is_some()), "selection must record at least one skip");
    assert!(FIXTURES.len() >= 30 && FIXTURES.len() <= 60, "selection size out of documented range");
}
