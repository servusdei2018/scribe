//! The `doctest` builder (Phase 22).
//!
//! Runs the read phase exactly like the HTML builder (discovery, RST parse,
//! directive resolution), but instead of rendering pages it extracts every
//! doctest example, groups setup/test/cleanup snippets, executes them through
//! the Python worker and writes `output.txt` with upstream-shaped summaries
//! and diffs. See `ARCHITECTURE.md` §28.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use scribe_config::{
    Config, ConfigDefault, ConfigEntry, ConfigError, ConfigRegistry, ConfigType, ConfigValue,
};
use scribe_core::{ActivatedExtensions, BuildEnvironment, UpdateOptions};
use scribe_rst::{parse_with_options, resolve_output, Context as RstContext, DirectiveRegistry, ParseOptions};

use crate::html::BuildOptions;

use super::directives::register_directives;
use super::error::{DoctestError, Result};
use super::extract::{extract_codes, group_codes};
use super::normalize::flags;
use super::runner::{self, DoctestDocResult, DoctestDocument, DoctestRunResult, RunnerOptions};

/// Result of a complete doctest build.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DoctestResult {
    /// Total examples executed.
    pub total_tries: u32,
    /// Failed test examples.
    pub total_failures: u32,
    /// Failed setup examples.
    pub setup_failures: u32,
    /// Failed cleanup examples.
    pub cleanup_failures: u32,
    /// `(docname, success)` in sorted document order.
    pub docs: Vec<(String, bool)>,
    /// Exact `output.txt` contents.
    pub output: String,
    /// Path of the written report.
    pub output_path: PathBuf,
    /// `true` when no interpreter was available and all tests were skipped.
    pub skipped: bool,
    /// Non-fatal build warnings.
    pub warnings: Vec<String>,
}

impl DoctestResult {
    /// Whether every executed test passed.
    #[must_use]
    pub fn success(&self) -> bool {
        self.total_failures == 0 && self.setup_failures == 0 && self.cleanup_failures == 0
    }

    /// Number of example/test groups discovered.
    #[must_use]
    pub fn group_count(&self) -> usize {
        self.docs.len()
    }
}

/// The `sphinx-build -b doctest` builder.
#[derive(Debug, Default)]
pub struct DoctestBuilder;

impl DoctestBuilder {
    /// Create a builder.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Run the complete doctest build into `options.output_dir`.
    pub fn build(
        options: &BuildOptions,
        config: &Config,
        env: &mut BuildEnvironment,
    ) -> Result<DoctestResult> {
        let source_dir = &options.source_dir;
        let output_dir = &options.output_dir;
        if !source_dir.is_dir() {
            return Err(DoctestError::MissingSource(source_dir.clone()));
        }
        fs::create_dir_all(output_dir).map_err(|error| DoctestError::io(output_dir, error))?;

        let update_options = UpdateOptions {
            freshenv: options.freshenv,
            write_all: options.force_all,
            cache_dir: Some(
                options
                    .doctree_dir
                    .clone()
                    .unwrap_or_else(|| output_dir.join(".doctrees")),
            ),
            persist: true,
            ..UpdateOptions::default()
        };
        let report = env.update(config, source_dir, &update_options).map_err(DoctestError::Environment)?;
        let mut warnings = report.warnings.clone();
        warnings.extend(ActivatedExtensions::from_config(config).warnings.clone());

        let mut docnames: Vec<String> = if options.filenames.is_empty() {
            env.found_docnames().iter().map(|name| (*name).to_owned()).collect()
        } else {
            let mut selected = BTreeSet::new();
            for file in &options.filenames {
                if let Some(docname) = env.path2doc(file) {
                    selected.insert(docname.to_owned());
                } else {
                    let stem = file.file_stem().and_then(|stem| stem.to_str()).unwrap_or_default();
                    if env.found_docs.contains_key(stem) {
                        selected.insert(stem.to_owned());
                    }
                }
            }
            selected.into_iter().collect()
        };
        docnames.retain(|name| env.found_docs.contains_key(name));
        docnames.sort();

        let config_arc = Arc::new(config.clone());
        let relevant = env.relevant_config();
        let rst_prolog = relevant.rst_prolog.clone();
        let rst_epilog = relevant.rst_epilog.clone();
        let mut documents = Vec::new();
        for docname in &docnames {
            let Some(path) = env.found_docs.get(docname) else { continue };
            let raw = match fs::read_to_string(path) {
                Ok(raw) => raw,
                Err(error) => {
                    warnings.push(format!("cannot read source for {docname}: {error}"));
                    continue;
                }
            };
            let mut options = ParseOptions::new(Some(docname.clone()));
            if !rst_prolog.is_empty() {
                options.prolog = Some(rst_prolog.clone());
            }
            if !rst_epilog.is_empty() {
                options.epilog = Some(rst_epilog.clone());
            }
            let mut output = parse_with_options(&raw, options);
            let mut ctx = RstContext::new(source_dir, scribe_config::Tags::new());
            ctx.config = Some(Arc::clone(&config_arc));
            ctx.source_name = Some(docname.clone());
            let registry = DirectiveRegistry::builtin();
            register_directives(&registry);
            let resolve_report = resolve_output(&mut output, &registry, &ctx);
            for issue in &resolve_report.unresolved {
                warnings.push(issue.warning_message(docname));
            }
            let filename = path
                .strip_prefix(source_dir)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            let codes = extract_codes(&output.document, &filename, config);
            let groups = group_codes(codes);
            if !groups.is_empty() {
                documents.push(DoctestDocument { docname: docname.clone(), groups: groups.into_values().collect() });
            }
        }

        let fail_fast = config.get_bool("doctest_fail_fast").unwrap_or(false);
        let show_successes = config.get_bool("doctest_show_successes").unwrap_or(true);
        let flags = config.get_int("doctest_default_flags").unwrap_or(i64::from(flags::DEFAULT)) as u32;
        let runner_options = RunnerOptions {
            python: runner::find_python().unwrap_or_else(|| PathBuf::from("python3")),
            python_paths: config
                .get_strings("doctest_path")
                .unwrap_or_default()
                .into_iter()
                .map(|path| {
                    let candidate = PathBuf::from(&path);
                    if candidate.is_absolute() {
                        path
                    } else {
                        source_dir.join(candidate).to_string_lossy().into_owned()
                    }
                })
                .collect(),
            flags,
            show_successes,
            fail_fast,
            global_setup: config.get_str("doctest_global_setup").unwrap_or_default().to_owned(),
            global_cleanup: config.get_str("doctest_global_cleanup").unwrap_or_default().to_owned(),
            timeout_secs: 60,
        };

        let (run_result, skipped) = if documents.is_empty() {
            (
                DoctestRunResult {
                    output: String::new(),
                    docs: Vec::new(),
                    total_tries: 0,
                    total_failures: 0,
                    setup_failures: 0,
                    cleanup_failures: 0,
                },
                false,
            )
        } else if runner::find_python().is_none() {
            let examples: u32 = documents
                .iter()
                .map(|doc| doc.groups.iter().map(|group| group.tests.len() as u32).sum::<u32>())
                .sum();
            warnings.push(format!(
                "doctest: no Python interpreter available; skipping {examples} example(s)"
            ));
            (
                DoctestRunResult {
                    output: format!("doctest: no Python interpreter available; {examples} example(s) skipped.\n"),
                    docs: documents
                        .iter()
                        .map(|doc| DoctestDocResult {
                            docname: doc.docname.clone(),
                            success: true,
                            tries: 0,
                            failures: 0,
                        })
                        .collect(),
                    total_tries: 0,
                    total_failures: 0,
                    setup_failures: 0,
                    cleanup_failures: 0,
                },
                true,
            )
        } else {
            match runner::run(&documents, &runner_options) {
                Ok(result) => (result, false),
                Err(error) => {
                    warnings.push(format!("doctest: {error}; examples skipped"));
                    (
                        DoctestRunResult {
                            output: format!("doctest: {error}; examples skipped.\n"),
                            docs: documents
                        .iter()
                        .map(|doc| DoctestDocResult {
                            docname: doc.docname.clone(),
                            success: true,
                            tries: 0,
                            failures: 0,
                        })
                        .collect(),
                            total_tries: 0,
                            total_failures: 0,
                            setup_failures: 0,
                            cleanup_failures: 0,
                        },
                        true,
                    )
                }
            }
        };

        let date = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let header = format!(
            "Results of doctest builder run on {date}\n{}\n",
            "=".repeat(34 + date.chars().count())
        );
        let summary = format_summary(&run_result, fail_fast);
        let output = format!("{header}{}{summary}", run_result.output);
        let output_path = output_dir.join("output.txt");
        fs::write(&output_path, &output).map_err(|error| DoctestError::io(&output_path, error))?;

        if !options.really_quiet {
            for doc_result in &run_result.docs {
                let doc = documents.iter().find(|document| document.docname == doc_result.docname);
                let tries = doc.map_or(doc_result.tries, |document| {
                    document.groups.iter().map(|group| group.tests.len()).sum::<usize>() as u32
                });
                let state = if doc_result.success { "PASS" } else { "FAIL" };
                println!("scribe: doctest: {}: {state} ({tries} example(s))", doc_result.docname);
            }
            for warning in &warnings {
                eprintln!("scribe: warning: {warning}");
            }
        }

        Ok(DoctestResult {
            total_tries: run_result.total_tries,
            total_failures: run_result.total_failures,
            setup_failures: run_result.setup_failures,
            cleanup_failures: run_result.cleanup_failures,
            docs: run_result
                .docs
                .into_iter()
                .map(|doc| (doc.docname, doc.success))
                .collect(),
            output,
            output_path,
            skipped,
            warnings,
        })
    }
}

/// Upstream `DocTestBuilder.finish()` executive summary, byte-compatible.
#[must_use]
pub fn format_summary(result: &DoctestRunResult, fail_fast: bool) -> String {
    let plural = |value: u32| if value == 1 { "" } else { "s" };
    let mut header = "Doctest summary".to_owned();
    if fail_fast
        && (result.total_failures > 0 || result.setup_failures > 0 || result.cleanup_failures > 0)
    {
        header.push_str(" (exiting after first failed test)");
    }
    format!(
        "\n{header}\n{underline}\n{:5} test{ts}\n{:5} failure{fs} in tests\n{:5} failure{ss} in setup code\n{:5} failure{cs} in cleanup code\n",
        result.total_tries,
        result.total_failures,
        result.setup_failures,
        result.cleanup_failures,
        underline = "=".repeat(header.chars().count()),
        ts = plural(result.total_tries),
        fs = plural(result.total_failures),
        ss = plural(result.setup_failures),
        cs = plural(result.cleanup_failures),
    )
}

/// Config keys owned by the doctest extension.
pub fn register_config_keys(
    registry: &mut ConfigRegistry,
) -> std::result::Result<(), ConfigError> {
    let entries: &[(&str, ConfigValue, ConfigType, &str)] = &[
        ("doctest_test_doctest_blocks", ConfigValue::from("default"), ConfigType::Str, "env"),
        ("doctest_global_setup", ConfigValue::from(""), ConfigType::Str, ""),
        ("doctest_global_cleanup", ConfigValue::from(""), ConfigType::Str, ""),
        ("doctest_default_flags", ConfigValue::Int(i64::from(flags::DEFAULT)), ConfigType::Int, ""),
        ("doctest_fail_fast", ConfigValue::Bool(false), ConfigType::Bool, ""),
        ("doctest_show_successes", ConfigValue::Bool(true), ConfigType::Bool, ""),
        ("doctest_path", ConfigValue::List(Vec::new()), ConfigType::List, ""),
    ];
    for (name, default, value_type, rebuild) in entries {
        if registry.contains(name) {
            continue;
        }
        registry.register(ConfigEntry {
            name,
            default: ConfigDefault::Value(default.clone()),
            value_type: value_type.clone(),
            rebuild,
            description: "Phase 22 native doctest setting.",
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_matches_upstream_shape() {
        let result = DoctestRunResult {
            output: String::new(),
            docs: Vec::new(),
            total_tries: 5,
            total_failures: 1,
            setup_failures: 0,
            cleanup_failures: 0,
        };
        let summary = format_summary(&result, false);
        assert!(summary.contains("\nDoctest summary\n===============\n"));
        assert!(summary.contains("    5 tests\n    1 failure in tests\n"));
        assert!(summary.contains("    0 failures in setup code\n"));
    }
}
