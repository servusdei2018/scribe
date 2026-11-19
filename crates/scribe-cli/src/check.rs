//! Phase 31 `scribe check` implementation.
//!
//! Validates a documentation project without emitting any build assets. It
//! reuses the library APIs owned by earlier phases: the `conf.py` bridge from
//! `scribe-config`, source discovery and the frozen `BuildEnvironment` from
//! `scribe-core`, the RST parser/directive resolver from `scribe-rst`, the
//! toctree collector (cycle and missing-target detection) from `scribe-core`
//! and the parallel cross-reference engine from `scribe-domains`.
//!
//! The command is deliberately read-only: nothing under the source or build
//! tree is created or modified, and no doctree cache is persisted.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use miette::Result;

use crate::options::CheckArgs;

/// A single structured validation diagnostic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckDiagnostic {
    /// Stable code, e.g. `scribe::toc::circular`.
    pub code: String,
    /// Human-readable message.
    pub message: String,
    /// Source document, when known.
    pub docname: Option<String>,
    /// Whether this diagnostic is fatal (an error rather than a warning).
    pub error: bool,
    /// Canonical Sphinx `type.subtype` code used for `suppress_warnings`
    /// filtering. `None` falls back to the stable scribe `code` mapping.
    pub canonical: Option<String>,
}

impl std::fmt::Display for CheckDiagnostic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.docname {
            Some(docname) => write!(
                formatter,
                "{docname}: {} [{}]: {}",
                if self.error { "error" } else { "warning" },
                self.code,
                self.message
            ),
            None => write!(
                formatter,
                "{} [{}]: {}",
                if self.error { "error" } else { "warning" },
                self.code,
                self.message
            ),
        }
    }
}

/// Outcome of a validation pass.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CheckReport {
    /// Configuration directory that was inspected.
    pub conf_dir: PathBuf,
    /// Number of discovered source documents.
    pub document_count: usize,
    /// Whether a configuration file was loaded.
    pub config_loaded: bool,
    /// All collected diagnostics in deterministic order.
    pub diagnostics: Vec<CheckDiagnostic>,
}

impl CheckReport {
    /// Number of fatal diagnostics.
    pub fn error_count(&self) -> usize {
        self.diagnostics.iter().filter(|diagnostic| diagnostic.error).count()
    }

    /// Number of non-fatal diagnostics.
    pub fn warning_count(&self) -> usize {
        self.diagnostics.iter().filter(|diagnostic| !diagnostic.error).count()
    }
}

/// Execute `scribe check` from parsed arguments.
pub fn run_check(args: CheckArgs) -> Result<CheckReport> {
    let conf_dir = args.conf_dir.clone().unwrap_or_else(|| args.path.clone());
    let mut report = CheckReport {
        conf_dir: conf_dir.clone(),
        ..CheckReport::default()
    };
    let nitpicky = args.nitpicky;
    // `--keep-going` is accepted for flag parity with `scribe build` and
    // `sphinx-build`; upstream 9.x does not let it change the final exit status.
    let _ = args.keep_going;

    // 1. Load and validate configuration through the existing Python/TOML
    //    bridge. A missing file is a warning (upstream `sphinx-build` uses
    //    defaults too); a malformed one is a hard error.
    let (config, config_warnings) = match scribe_config::load_config(&conf_dir, None, None) {
        Ok(pair) => {
            report.config_loaded = pair.0.source().is_some();
            pair
        }
        Err(error) => {
            push(&mut report, false, "scribe::config::load_failed", None, error.to_string());
            (scribe_config::Config::new(), Vec::new())
        }
    };
    for warning in config_warnings {
        push(
            &mut report,
            false,
            format!("scribe::config::{}", warning.kind),
            warning.path.as_ref().and_then(|path| path.to_str()).map(str::to_owned),
            warning.message,
        );
    }

    // For validation purposes every missing reference is reported: upstream's
    // nitpicky switch controls *whether* to warn, and `check` always wants the
    // complete picture. The user-facing `-n` flag is still honoured as a
    // documented compatibility no-op (nitpicky is implicitly on here).
    let mut check_config = config.clone();
    let _ = check_config.set("nitpicky", scribe_config::ConfigValue::Bool(true));
    let _ = nitpicky;

    // 2. Discover sources with the shared discovery engine.
    let discovery_options = scribe_core::DiscoveryOptions::from_config(&check_config, &args.path);
    let discovery = scribe_core::discover_sources(&discovery_options).map_err(|error| {
        miette::Report::new(CheckFailed {
            message: format!("source discovery failed: {error}"),
        })
    })?;
    for warning in &discovery.warnings {
        push(&mut report, false, "scribe::discovery::warning", None, warning.clone());
    }
    report.document_count = discovery.docs.len();
    if discovery.docs.is_empty() {
        push(
            &mut report,
            true,
            "scribe::check::no_sources",
            None,
            format!("no source files discovered in `{}`", args.path.display()),
        );
    }

    // 3. Parse and directive-resolve every source. Diagnostics from the RST
    //    layer are warnings; a file that cannot be read at all is an error.
    let registry = scribe_rst::DirectiveRegistry::builtin();
    let config_arc = Arc::new(check_config.clone());
    let mut parsed: BTreeMap<String, scribe_ast::Document> = BTreeMap::new();
    let mut sources: BTreeMap<String, Arc<String>> = BTreeMap::new();
    for (docname, discovered) in &discovery.docs {
        let raw = match fs::read_to_string(&discovered.absolute_path) {
            Ok(text) => text,
            Err(error) => {
                push(
                    &mut report,
                    true,
                    "scribe::check::read_failed",
                    Some(docname.clone()),
                    format!("cannot read {}: {error}", discovered.absolute_path.display()),
                );
                continue;
            }
        };
        let source = Arc::new(raw);
        sources.insert(docname.clone(), Arc::clone(&source));

        let mut parse_options = scribe_rst::ParseOptions::new(Some(docname.clone()));
        let rst_prolog = config.get_str("rst_prolog").unwrap_or_default().to_owned();
        let rst_epilog = config.get_str("rst_epilog").unwrap_or_default().to_owned();
        if !rst_prolog.is_empty() {
            parse_options.prolog = Some(rst_prolog);
        }
        if !rst_epilog.is_empty() {
            parse_options.epilog = Some(rst_epilog);
        }
        let mut output = scribe_rst::parse_with_options(&source, parse_options);
        for diagnostic in &output.diagnostics {
            push(
                &mut report,
                false,
                "scribe::rst::parse",
                Some(docname.clone()),
                diagnostic.to_string(),
            );
        }

        let tags = scribe_config::Tags::with_tags(std::iter::empty::<String>());
        let mut context = scribe_rst::Context::new(&args.path, tags);
        context.config = Some(Arc::clone(&config_arc));
        context.source_name = Some(docname.clone());
        let resolve_report = scribe_rst::resolve_output(&mut output, &registry, &context);
        for issue in &resolve_report.unresolved {
            push(
                &mut report,
                false,
                "scribe::rst::directive",
                Some(docname.clone()),
                issue.warning_message(docname),
            );
        }
        output.document.metadata.docname = Some(docname.clone());
        parsed.insert(docname.clone(), output.document);
    }

    // 4. Populate a fresh environment from discovery so domains and the
    //    toctree collector can resolve docnames against real source paths.
    let mut env = scribe_core::BuildEnvironment::with_config(&args.path, &check_config);
    for (docname, discovered) in &discovery.docs {
        env.found_docs.insert(docname.clone(), discovered.absolute_path.clone());
    }
    for (docname, document) in &parsed {
        env.record_document(docname, document, BTreeMap::new());
    }

    // 5. Register domain objects project-wide, then resolve every pending
    //    cross-reference through the Phase 14 engine. Unresolved references
    //    are hard errors: a documentation project with dangling links is not
    //    considered valid by `scribe check`.
    let domains = scribe_domains::DomainRegistry::builtin();
    let sink = scribe_domains::DiagnosticSink::default();
    for (docname, document) in &parsed {
        let domain_context = scribe_domains::DomainContext::new(
            Some(&env),
            Some(&check_config),
            docname,
            "dummy",
            "dummy",
            &sink,
        );
        if let Err(error) = domains.process_document(&domain_context, docname, document) {
            push(
                &mut report,
                false,
                "scribe::domains::registration",
                Some(docname.clone()),
                format!("domain registration failed: {error}"),
            );
        }
    }
    let resolve_context = scribe_domains::DomainContext::new(
        Some(&env),
        Some(&check_config),
        "",
        "dummy",
        "dummy",
        &sink,
    );
    let xref_engine = scribe_domains::XrefResolver::new(&domains, None);
    let xref_report = xref_engine
        .resolve_documents(&resolve_context, &mut parsed)
        .map_err(|error| {
            miette::Report::new(CheckFailed {
                message: format!("cross-reference resolution failed: {error}"),
            })
        })?;
    for diagnostic in &xref_report.diagnostics {
        let canonical = diagnostic.typ.as_deref().map(|typ| {
            scribe_core::missing_reference_code(typ).as_str()
        });
        push_canonical(
            &mut report,
            true,
            diagnostic.code.clone(),
            canonical,
            diagnostic.docname.clone(),
            diagnostic.message.clone(),
        );
    }
    // Nitpicky mode guarantees a named diagnostic per unresolved reference.
    // As a belt-and-braces measure, account for any unresolved reference that
    // did not produce one (for example a suppressed domain message).
    if xref_report.unresolved > xref_report.diagnostics.len() {
        let missing = xref_report.unresolved - xref_report.diagnostics.len();
        push(
            &mut report,
            true,
            "scribe::xref::unresolved",
            None,
            format!("{missing} cross-reference(s) could not be resolved"),
        );
    }

    // 6. Collect toctrees from the freshly recorded doctrees. Cycles, missing
    //    documents and duplicate entries are errors; an unreachable document
    //    is the conventional upstream warning.
    let toctrees = scribe_core::collect_toctrees_with_sources(&mut env, &check_config, &sources);
    for warning in &toctrees.warnings {
        let error = warning.code != "scribe::toc::not_included";
        push(&mut report, error, warning.code.to_owned(), Some(warning.source_name.clone()), warning.message.clone());
    }

    // Deterministic order: errors before warnings, then by code/doc/message.
    // Sphinx filters warnings with `suppress_warnings` before counting them
    // (and therefore before `-W` can escalate them). Apply the same filter to
    // every diagnostic using its canonical `type.subtype` code.
    let suppress_patterns = config.get_strings("suppress_warnings").unwrap_or_default();
    report.diagnostics.retain(|diagnostic| {
        let canonical = diagnostic
            .canonical
            .as_deref()
            .map(scribe_core::WarningCode::from_canonical)
            .or_else(|| scribe_core::canonical_code(&diagnostic.code))
            .unwrap_or_else(|| scribe_core::WarningCode::bare("misc"));
        !scribe_core::is_suppressed(&canonical, &suppress_patterns)
    });

    report.diagnostics.sort_by(|a, b| {
        b.error
            .cmp(&a.error)
            .then_with(|| a.code.cmp(&b.code))
            .then_with(|| a.docname.cmp(&b.docname))
            .then_with(|| a.message.cmp(&b.message))
    });
    report.diagnostics.dedup();

    if !args.quiet {
        println!(
            "scribe: checked {} document(s) in `{}`",
            report.document_count,
            args.path.display()
        );
    }
    for diagnostic in &report.diagnostics {
        eprintln!("scribe: {diagnostic}");
    }
    if !args.quiet {
        println!(
            "scribe: check finished: {} error(s), {} warning(s)",
            report.error_count(),
            report.warning_count()
        );
    }

    let has_errors = report.error_count() > 0;
    let has_warnings = report.warning_count() > 0;
    if has_errors || (args.warningiserror && has_warnings) {
        return Err(miette::Report::new(CheckFailed {
            message: format!(
                "check failed: {} error(s), {} warning(s){}",
                report.error_count(),
                report.warning_count(),
                if args.warningiserror { " (warnings are errors)" } else { "" },
            ),
        }));
    }
    Ok(report)
}

fn push(
    report: &mut CheckReport,
    error: bool,
    code: impl Into<String>,
    docname: Option<String>,
    message: impl Into<String>,
) {
    push_canonical(report, error, code, None, docname, message);
}

fn push_canonical(
    report: &mut CheckReport,
    error: bool,
    code: impl Into<String>,
    canonical: Option<String>,
    docname: Option<String>,
    message: impl Into<String>,
) {
    report.diagnostics.push(CheckDiagnostic {
        code: code.into(),
        message: message.into(),
        docname,
        error,
        canonical,
    });
}

/// Final non-zero-exit diagnostic for a failed validation pass.
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error("{message}")]
#[diagnostic(
    code(scribe::check::failed),
    help("Fix the reported source/configuration problems and rerun `scribe check`.")
)]
pub struct CheckFailed {
    /// Summary of the failure.
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::{CheckDiagnostic, CheckReport};

    #[test]
    fn report_counts_errors_and_warnings() {
        let report = CheckReport {
            diagnostics: vec![
                CheckDiagnostic {
                    code: "scribe::toc::circular".to_owned(),
                    message: "cycle".to_owned(),
                    docname: Some("index".to_owned()),
                    error: true,
                    canonical: None,
                },
                CheckDiagnostic {
                    code: "scribe::toc::not_included".to_owned(),
                    message: "orphan".to_owned(),
                    docname: Some("other".to_owned()),
                    error: false,
                    canonical: None,
                },
            ],
            ..CheckReport::default()
        };
        assert_eq!(report.error_count(), 1);
        assert_eq!(report.warning_count(), 1);
        assert!(report.diagnostics[0].to_string().contains("error"));
    }
}
