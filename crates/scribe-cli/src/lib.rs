//! Command-line surface for `scribe`.
//!
//! The `build` subcommand mirrors upstream `sphinx/cmd/build.py::get_parser()`
//! flag-for-flag. Parsed input is normalized into [`BuildOptions`], which is the
//! reusable, clap-independent model consumed by later phases. Phase 01 stops
//! after validation and emits a structured `miette` diagnostic; it never
//! panics on user input.

pub mod check;
pub mod clean;
pub mod options;
pub mod quickstart;
pub mod serve;

pub use check::{CheckDiagnostic, CheckFailed, CheckReport};
pub use clean::{CleanError, CleanReport, CleanTarget};
pub use options::{
    BuildArgs, BuildOptions, CheckArgs, CleanArgs, ColorChoice, ConfigOverride,
    InvalidBuildOptions, Jobs, MissingBuildArgument, QuickstartArgs, QuickstartOptions,
    ScribeCliError,
};
pub use quickstart::{QuickstartError, QuickstartReport};
pub use serve::ServeArgs;

use clap::{Parser, Subcommand};
use miette::{Diagnostic, Report, Result};
use scribe_config::{load_config_with, Config, ConfigWarning, LoadRequest};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Display version emulated for the pinned upstream oracle (`035a07c3`).
pub const SPHINX_COMPAT_VERSION: &str = "9.3.0+/035a07c32";

/// Top-level `scribe` command.
#[derive(Debug, Parser)]
#[command(
    name = "scribe",
    version,
    about = "A Rust reimplementation of the Sphinx documentation engine",
    long_about = None
)]
pub struct Cli {
    /// Subcommand to execute.
    #[command(subcommand)]
    pub command: Command,
}

/// Available `scribe` subcommands.
#[derive(Debug, Subcommand)]
// BuildArgs is an intentionally flat mirror of sphinx-build's flag set.
#[allow(clippy::large_enum_variant)]
pub enum Command {
    /// Build documentation (mirrors `sphinx-build`).
    Build(BuildArgs),

    /// Scaffold a new documentation project.
    Quickstart(QuickstartArgs),

    /// Safely remove generated build output and the doctree cache.
    Clean(CleanArgs),

    /// Validate configuration, sources, toctrees and cross-references.
    Check(CheckArgs),

    /// Serve documentation with incremental rebuilds and live reload
    /// (alias: `watch`).
    #[command(visible_alias = "watch")]
    Serve(serve::ServeArgs),
}

/// Diagnostic emitted when a builder is requested before builders exist.
#[derive(Debug, Error, Diagnostic)]
#[error("builder `{builder}` is not implemented yet")]
#[diagnostic(
    code(scribe::unimplemented_builder),
    help("Phase 01 delivers the CLI skeleton and oracle harness only; builders arrive in Phases 17-25.")
)]
pub struct UnimplementedBuilder {
    /// Builder requested on the command line.
    pub builder: String,
}

/// Diagnostic emitted when the selected builder fails.
#[derive(Debug, Error, Diagnostic)]
#[error("HTML builder failed for builder `{builder}`")]
#[diagnostic(
    code(scribe::builder::failed),
    help("check the source directory, configuration and reported warnings")
)]
pub struct BuilderFailed {
    /// Builder that failed.
    pub builder: String,
    /// Underlying builder error.
    #[source]
    pub source: scribe_builders::HtmlError,
}

/// Diagnostic emitted when the LaTeX/PDF builder fails.
#[derive(Debug, Error, Diagnostic)]
#[error("LaTeX builder failed for builder `{builder}`")]
#[diagnostic(
    code(scribe::builder::latex_failed),
    help("check the source directory, configuration and reported warnings")
)]
pub struct LatexBuilderFailed {
    /// Builder that failed.
    pub builder: String,
    /// Underlying builder error.
    #[source]
    pub source: scribe_builders::LatexError,
}

/// Diagnostic emitted when one of the Phase 24 output builders fails.
#[derive(Debug, Error, Diagnostic)]
#[error("output builder `{builder}` failed: {source}")]
#[diagnostic(
    code(scribe::builder::output_failed),
    help("check the source directory, configuration and reported warnings")
)]
pub struct OutputBuilderFailed {
    /// Builder that failed.
    pub builder: String,
    /// Underlying builder error.
    #[source]
    pub source: scribe_builders::OutputError,
}

/// Diagnostic emitted when the linkcheck builder reports broken links with
/// `-W` enabled. Upstream sets a non-zero status code unconditionally; scribe
/// gates it on `-W` per the Phase 25 contract (see `ARCHITECTURE.md` §31).
#[derive(Debug, Error, Diagnostic)]
#[error("linkcheck: {broken} broken link(s), {timeouts} timeout(s)")]
#[diagnostic(
    code(scribe::linkcheck::broken_links),
    help("inspect output.txt and output.json in the output directory")
)]
pub struct LinkcheckBrokenLinks {
    /// Number of broken links.
    pub broken: u32,
    /// Number of timed-out links.
    pub timeouts: u32,
    /// Directory containing the detailed report.
    pub output: std::path::PathBuf,
}

/// Diagnostic emitted when `-W` is active and a build produced warnings.
///
/// Upstream counts only warnings that survive `suppress_warnings`, and
/// `--keep-going` is accepted for flag compatibility without changing the
/// final exit status (confirmed against `sphinx-build 9.3.0`).
#[derive(Debug, Error, Diagnostic)]
#[error("build finished with problems, {count} warning(s) (with warnings treated as errors)")]
#[diagnostic(
    code(scribe::build::warnings_as_errors),
    help("fix the reported warnings or extend `suppress_warnings`; --keep-going does not change the exit status")
)]
pub struct WarningsAsErrors {
    /// Number of unsuppressed warnings that escalated to errors.
    pub count: usize,
    /// Whether `--keep-going` was requested (informational).
    pub keep_going: bool,
}

/// Diagnostic emitted when the doctest builder itself fails.
#[derive(Debug, Error, Diagnostic)]
#[error("doctest builder failed: {source}")]
#[diagnostic(
    code(scribe::builder::doctest_failed),
    help("check the source directory, configuration and reported warnings")
)]
pub struct DoctestFailed {
    /// Underlying doctest builder error.
    #[source]
    pub source: scribe_builders::DoctestError,
}

/// Diagnostic emitted when one or more executed doctests fail.
#[derive(Debug, Error, Diagnostic)]
#[error("doctest: {failures} failure(s); see {}", .output.display())]
#[diagnostic(
    code(scribe::doctest::failed),
    help("inspect output.txt for the exact expected/got diff")
)]
pub struct DoctestTestsFailed {
    /// Number of failed examples (tests + setup + cleanup).
    pub failures: u32,
    /// Path of the detailed report.
    pub output: std::path::PathBuf,
}

/// Diagnostic for a configuration file that could not be loaded.
#[derive(Debug, Error, Diagnostic)]
#[error("failed to load configuration from `{}`", .path.display())]
#[diagnostic(
    code(scribe::config::load_failed),
    help("Point -c at a directory containing conf.py or scribe.toml, or pass -C to build without configuration.")
)]
pub struct ConfigLoadFailed {
    /// Configuration directory that was searched.
    pub path: PathBuf,
    /// Underlying library error.
    #[source]
    pub source: scribe_config::ConfigError,
}

/// Parse process arguments and execute the selected subcommand.
pub fn run() -> Result<()> {
    execute(Cli::parse())
}

/// Execute an already-parsed command. Exposed for embedding and tests.
pub fn execute(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Build(args) => run_build(args),
        Command::Quickstart(args) => quickstart::run_quickstart(args).map(|_| ()),
        Command::Clean(args) => clean::run_clean(args).map(|_| ()),
        Command::Check(args) => check::run_check(args).map(|_| ()),
        Command::Serve(args) => serve::run_serve(args),
    }
}

fn run_build(args: BuildArgs) -> Result<()> {
    if args.show_version {
        println!("sphinx-build {SPHINX_COMPAT_VERSION} (scribe compatibility layer)");
        return Ok(());
    }

    // Conversion validates cross-field constraints (e.g. -a + filenames).
    let options = BuildOptions::try_from(args)?;

    // Phase 02 loads configuration (conf.py, scribe.toml and -D/-A/-n
    // overrides) and reports the parsed values. Document parsing and output
    // builders arrive in later phases.
    let search_path = options.conf_dir().map(Path::to_path_buf).unwrap_or_default();
    let (config, warnings) = build_config(&options).map_err(|error| {
        Report::new(ConfigLoadFailed {
            path: search_path,
            source: error,
        })
    })?;

    if !options.really_quiet {
        print_config_summary(&config, &warnings);
    }

    // HTML-family builders share the Phase 17 pipeline (Phase 19 adds the
    // alternate flavors). Unknown builders keep the Phase 01 diagnostic.
    if options.builder == "doctest" {
        return run_doctest_build(&options, &config);
    }

    if options.builder == "gettext" {
        return run_gettext_build(&options, &config);
    }

    if options.builder == "linkcheck" {
        return run_linkcheck_build(&options, &config);
    }

    if matches!(options.builder.as_str(), "xml" | "pseudoxml" | "changes") {
        return run_doc_build(&options, &config);
    }

    if matches!(
        options.builder.as_str(),
        "html" | "dirhtml" | "singlehtml" | "json" | "htmlhelp" | "qthelp" | "devhelp"
    ) {
        return run_html_build(&options, &config);
    }

    if matches!(options.builder.as_str(), "latex" | "pdf") {
        return run_latex_build(&options, &config);
    }

    if matches!(
        options.builder.as_str(),
        "epub" | "epub3" | "man" | "texinfo" | "text"
    ) {
        return run_output_build(&options, &config);
    }

    Err(Report::new(UnimplementedBuilder {
        builder: options.builder.clone(),
    }))
}

/// Print the warnings that survive `suppress_warnings` and apply `-W` exit
/// semantics. Shared by every builder so `build` and `check` behave alike.
fn finish_build_warnings(options: &BuildOptions, config: &Config, warnings: &[String]) -> Result<()> {
    let patterns = config.get_strings("suppress_warnings").unwrap_or_default();
    let visible: Vec<&String> = warnings
        .iter()
        .filter(|warning| {
            !scribe_core::is_suppressed(
                &scribe_core::classify_warning_message(warning),
                &patterns,
            )
        })
        .collect();
    if !options.really_quiet {
        for warning in &visible {
            eprintln!("scribe: warning: {warning}");
        }
    }
    if options.warningiserror && !visible.is_empty() {
        return Err(Report::new(WarningsAsErrors {
            count: visible.len(),
            keep_going: options.keep_going,
        }));
    }
    Ok(())
}

fn run_gettext_build(options: &BuildOptions, config: &Config) -> Result<()> {
    let mut environment = scribe_core::BuildEnvironment::with_config(&options.source_dir, config);
    let builder_options = scribe_builders::html::BuildOptions {
        source_dir: options.source_dir.clone(),
        output_dir: options.output_dir.clone(),
        builder_name: "gettext".to_owned(),
        force_all: options.force_all,
        freshenv: options.freshenv,
        doctree_dir: options.doctreedir.clone(),
        filenames: options.filenames.clone(),
        worker_count: options.worker_count(),
        really_quiet: options.really_quiet,
        bridge: Some(std::sync::Arc::new(
            scribe_core::InteropBridge::from_config(config, &options.source_dir),
        )),
    };
    let result = scribe_builders::GettextBuilder::build(&builder_options, config, &mut environment)
        .map_err(|source| Report::new(OutputBuilderFailed {
            builder: "gettext".to_owned(),
            source,
        }))?;
    if !options.really_quiet {
        println!(
            "scribe: gettext: built {} document(s), wrote {} catalog(s) to {}",
            result.built_docnames.len(),
            result.file_count(),
            options.output_dir.display()
        );
    }
    finish_build_warnings(options, config, &result.warnings)
}

fn run_doctest_build(options: &BuildOptions, config: &Config) -> Result<()> {
    let mut environment = scribe_core::BuildEnvironment::with_config(
        &options.source_dir,
        config,
    );
    let builder_options = scribe_builders::html::BuildOptions {
        source_dir: options.source_dir.clone(),
        output_dir: options.output_dir.clone(),
        builder_name: "doctest".to_owned(),
        force_all: options.force_all,
        freshenv: options.freshenv,
        doctree_dir: options.doctreedir.clone(),
        filenames: options.filenames.clone(),
        worker_count: options.worker_count(),
        really_quiet: options.really_quiet,
        bridge: Some(std::sync::Arc::new(
            scribe_core::InteropBridge::from_config(config, &options.source_dir),
        )),
    };
    let result = scribe_builders::DoctestBuilder::build(&builder_options, config, &mut environment)
        .map_err(|source| Report::new(DoctestFailed { source }))?;
    if !options.really_quiet {
        println!(
            "scribe: doctest: {} test(s), {} failure(s) in tests, {} in setup, {} in cleanup",
            result.total_tries,
            result.total_failures,
            result.setup_failures,
            result.cleanup_failures
        );
        if result.skipped {
            println!("scribe: doctest: execution skipped (no usable Python interpreter)");
        }
    }
    let failures = result.total_failures + result.setup_failures + result.cleanup_failures;
    if failures > 0 {
        return Err(Report::new(DoctestTestsFailed {
            failures,
            output: result.output_path,
        }));
    }
    Ok(())
}

fn run_latex_build(options: &BuildOptions, config: &Config) -> Result<()> {
    let mut environment = scribe_core::BuildEnvironment::with_config(
        &options.source_dir,
        config,
    );
    let builder_options = scribe_builders::latex::BuildOptions {
        source_dir: options.source_dir.clone(),
        output_dir: options.output_dir.clone(),
        builder_name: options.builder.clone(),
        force_all: options.force_all,
        freshenv: options.freshenv,
        doctree_dir: options.doctreedir.clone(),
        filenames: options.filenames.clone(),
        worker_count: options.worker_count(),
        really_quiet: options.really_quiet,
    };
    let result = scribe_builders::LatexBuilder::build(&builder_options, config, &mut environment)
        .map_err(|source| Report::new(LatexBuilderFailed {
            builder: options.builder.clone(),
            source,
        }))?;

    if !options.really_quiet {
        println!(
            "scribe: built {} LaTeX document(s), wrote {} file(s) to {}",
            result.built_docnames.len(),
            result.file_count(),
            options.output_dir.display()
        );
    }
    finish_build_warnings(options, config, &result.warnings)
}

fn run_output_build(options: &BuildOptions, config: &Config) -> Result<()> {
    let mut environment = scribe_core::BuildEnvironment::with_config(&options.source_dir, config);
    let builder_options = scribe_builders::html::BuildOptions {
        source_dir: options.source_dir.clone(),
        output_dir: options.output_dir.clone(),
        builder_name: options.builder.clone(),
        force_all: options.force_all,
        freshenv: options.freshenv,
        doctree_dir: options.doctreedir.clone(),
        filenames: options.filenames.clone(),
        worker_count: options.worker_count(),
        really_quiet: options.really_quiet,
        bridge: Some(std::sync::Arc::new(
            scribe_core::InteropBridge::from_config(config, &options.source_dir),
        )),
    };
    let result = match options.builder.as_str() {
        "epub" | "epub3" => scribe_builders::EpubBuilder::build(&builder_options, config, &mut environment),
        "man" => scribe_builders::ManBuilder::build(&builder_options, config, &mut environment),
        "texinfo" => scribe_builders::TexinfoBuilder::build(&builder_options, config, &mut environment),
        "text" => scribe_builders::TextBuilder::build(&builder_options, config, &mut environment),
        other => Err(scribe_builders::OutputError::config(
            "cli",
            "builder",
            format!("unsupported output builder `{other}`"),
        )),
    }
    .map_err(|source| Report::new(OutputBuilderFailed {
        builder: options.builder.clone(),
        source,
    }))?;

    if !options.really_quiet {
        println!(
            "scribe: built {} output document(s), wrote {} file(s) to {}",
            result.built_docnames.len(),
            result.file_count(),
            options.output_dir.display()
        );
    }
    finish_build_warnings(options, config, &result.warnings)
}

fn run_doc_build(options: &BuildOptions, config: &Config) -> Result<()> {
    let mut environment = scribe_core::BuildEnvironment::with_config(
        &options.source_dir,
        config,
    );
    let builder_options = scribe_builders::html::BuildOptions {
        source_dir: options.source_dir.clone(),
        output_dir: options.output_dir.clone(),
        builder_name: options.builder.clone(),
        force_all: options.force_all,
        freshenv: options.freshenv,
        doctree_dir: options.doctreedir.clone(),
        filenames: options.filenames.clone(),
        worker_count: options.worker_count(),
        really_quiet: options.really_quiet,
        bridge: Some(std::sync::Arc::new(
            scribe_core::InteropBridge::from_config(config, &options.source_dir),
        )),
    };
    let result = match options.builder.as_str() {
        "xml" => scribe_builders::XmlBuilder::build(&builder_options, config, &mut environment),
        "pseudoxml" => scribe_builders::PseudoXmlBuilder::build(&builder_options, config, &mut environment),
        "changes" => scribe_builders::ChangesBuilder::build(&builder_options, config, &mut environment),
        other => Err(scribe_builders::OutputError::config(
            "cli",
            "builder",
            format!("unsupported document builder `{other}`"),
        )),
    }
    .map_err(|source| Report::new(OutputBuilderFailed {
        builder: options.builder.clone(),
        source,
    }))?;

    if !options.really_quiet {
        println!(
            "scribe: built {} document(s), wrote {} file(s) to {}",
            result.built_docnames.len(),
            result.file_count(),
            options.output_dir.display()
        );
    }
    finish_build_warnings(options, config, &result.warnings)
}

fn run_linkcheck_build(options: &BuildOptions, config: &Config) -> Result<()> {
    let mut environment = scribe_core::BuildEnvironment::with_config(
        &options.source_dir,
        config,
    );
    let builder_options = scribe_builders::html::BuildOptions {
        source_dir: options.source_dir.clone(),
        output_dir: options.output_dir.clone(),
        builder_name: "linkcheck".to_owned(),
        force_all: options.force_all,
        freshenv: options.freshenv,
        doctree_dir: options.doctreedir.clone(),
        filenames: options.filenames.clone(),
        worker_count: options.worker_count(),
        really_quiet: options.really_quiet,
        bridge: Some(std::sync::Arc::new(
            scribe_core::InteropBridge::from_config(config, &options.source_dir),
        )),
    };
    let report = scribe_builders::LinkcheckBuilder::build(&builder_options, config, &mut environment)
        .map_err(|source| Report::new(OutputBuilderFailed {
            builder: "linkcheck".to_owned(),
            source,
        }))?;

    if !options.really_quiet {
        println!(
            "scribe: linkcheck: {} link(s) checked, {} broken, {} timeout(s); report in {}",
            report.results.len(),
            report.broken_count(),
            report.timeout_count(),
            options.output_dir.display()
        );
    }
    finish_build_warnings(options, config, &report.warnings)?;
    if options.warningiserror && report.has_broken() {
        return Err(Report::new(LinkcheckBrokenLinks {
            broken: report.broken_count() as u32,
            timeouts: report.timeout_count() as u32,
            output: options.output_dir.clone(),
        }));
    }
    Ok(())
}

fn run_html_build(options: &BuildOptions, config: &Config) -> Result<()> {
    let mut environment = scribe_core::BuildEnvironment::with_config(
        &options.source_dir,
        config,
    );
    let builder_options = scribe_builders::html::BuildOptions {
        source_dir: options.source_dir.clone(),
        output_dir: options.output_dir.clone(),
        builder_name: options.builder.clone(),
        force_all: options.force_all,
        freshenv: options.freshenv,
        doctree_dir: options.doctreedir.clone(),
        filenames: options.filenames.clone(),
        worker_count: options.worker_count(),
        really_quiet: options.really_quiet,
        bridge: Some(std::sync::Arc::new(
            scribe_core::InteropBridge::from_config(config, &options.source_dir),
        )),
    };
    let result = match options.builder.as_str() {
        "dirhtml" => scribe_builders::DirectoryHTMLBuilder::build(&builder_options, config, &mut environment),
        "singlehtml" => scribe_builders::SingleFileHTMLBuilder::build(&builder_options, config, &mut environment),
        "json" => scribe_builders::JsonBuilder::build(&builder_options, config, &mut environment),
        "htmlhelp" => scribe_builders::HtmlHelpBuilder::build(&builder_options, config, &mut environment),
        "qthelp" => scribe_builders::QtHelpBuilder::build(&builder_options, config, &mut environment),
        "devhelp" => scribe_builders::DevHelpBuilder::build(&builder_options, config, &mut environment),
        _ => scribe_builders::html::StandaloneHtmlBuilder::build(&builder_options, config, &mut environment),
    }
    .map_err(|source| Report::new(BuilderFailed {
        builder: options.builder.clone(),
        source,
    }))?;

    if !options.really_quiet {
        println!(
            "scribe: built {} page(s), wrote {} file(s) to {}",
            result.built_docnames.len(),
            result.file_count(),
            options.output_dir.display()
        );
    }
    finish_build_warnings(options, config, &result.warnings)
}

/// Assemble `-D`, `-A`, `-n` and `-t` inputs and load the effective config.
pub fn build_config(
    options: &BuildOptions,
) -> Result<(Config, Vec<ConfigWarning>), scribe_config::ConfigError> {
    let mut overrides: BTreeMap<String, String> = options
        .define
        .iter()
        .map(|override_value| (override_value.key.clone(), override_value.value.clone()))
        .collect();
    for html_define in &options.htmldefine {
        overrides.insert(
            format!("html_context.{}", html_define.key),
            html_define.value.clone(),
        );
    }
    if options.nitpicky {
        overrides.insert("nitpicky".to_owned(), "1".to_owned());
    }

    let request = LoadRequest {
        conf_dir: options.conf_dir().map(Path::to_path_buf).unwrap_or_default(),
        overrides,
        tags: options.tags.clone(),
        ..LoadRequest::default()
    };
    load_config_with(request)
}

fn print_config_summary(config: &Config, warnings: &[ConfigWarning]) {
    match config.source() {
        Some(path) => println!("scribe: loaded configuration from {}", path.display()),
        None => println!("scribe: no configuration file found; using defaults"),
    }
    for key in [
        "project",
        "author",
        "copyright",
        "release",
        "version",
        "master_doc",
        "extensions",
        "source_suffix",
        "html_theme",
        "html_title",
        "language",
        "nitpicky",
    ] {
        if let Some(value) = config.get(key) {
            println!("  {key:<16} = {}", value.display_string());
        }
    }
    for warning in warnings {
        eprintln!("scribe: {warning}");
    }
}

#[cfg(test)]
mod tests {
    use super::{build_config, Cli, Command};
    use clap::{CommandFactory, Parser};
    use std::path::PathBuf;

    #[test]
    fn all_subcommands_are_registered() {
        let command = Cli::command();
        let names: Vec<&str> = command.get_subcommands().map(clap::Command::get_name).collect();
        assert!(names.contains(&"build"));
        assert!(names.contains(&"quickstart"));
        assert!(names.contains(&"clean"));
        assert!(names.contains(&"check"));
        assert!(names.contains(&"serve"));
    }

    #[test]
    fn build_parses_the_sphinx_flag_set() {
        let cli = Cli::try_parse_from([
            "scribe", "build", "src", "out", "index.rst", "-b", "pseudoxml", "-j", "auto",
            "-E", "-d", "doctrees", "-c", "conf", "-C", "-D", "html_theme=basic", "-A",
            "name=value", "-t", "draft", "-n", "-vv", "-q", "-Q", "--color", "-w",
            "warnings.txt", "-W", "--keep-going", "-T", "-P", "--exception-on-warning",
        ])
        .unwrap();
        let Command::Build(args) = cli.command else {
            panic!("expected build subcommand");
        };
        assert_eq!(args.builder, "pseudoxml");
        assert_eq!(args.jobs, super::Jobs::Auto);
        assert_eq!(args.define.len(), 1);
        assert_eq!(args.define[0].key, "html_theme");
        assert_eq!(args.verbosity, 2);
        assert!(args.warningiserror && args.keep_going && args.traceback);
        assert!(args.color);
        assert!(!args.no_color);
        super::BuildOptions::try_from(args).unwrap();
    }

    #[test]
    fn check_parses_w_and_keep_going_flags() {
        let cli = Cli::try_parse_from(["scribe", "check", "docs", "-W", "--keep-going"]).unwrap();
        let Command::Check(args) = cli.command else {
            panic!("expected check subcommand");
        };
        assert!(args.warningiserror && args.keep_going);
        assert_eq!(args.path, PathBuf::from("docs"));
    }

    #[test]
    fn build_rejects_invalid_jobs_and_overrides() {
        assert!(Cli::try_parse_from(["scribe", "build", "src", "out", "-j", "0"]).is_err());
        assert!(
            Cli::try_parse_from(["scribe", "build", "src", "out", "-D", "no-equals"]).is_err()
        );
    }

    #[test]
    fn build_version_needs_no_directories() {
        let cli = Cli::try_parse_from(["scribe", "build", "--version"]).unwrap();
        let Command::Build(args) = cli.command else {
            panic!("expected build subcommand");
        };
        assert!(args.show_version);
        assert!(super::BuildOptions::try_from(args).is_err());
    }

    #[test]
    fn build_loads_real_config_and_applies_cli_overrides() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../sphinx/tests/roots/test-basic");
        let root_text = root.to_string_lossy().into_owned();
        let cli = Cli::try_parse_from([
            "scribe",
            "build",
            root_text.as_str(),
            "out",
            "-c",
            root_text.as_str(),
            "-D",
            "project=CLI Project",
            "-A",
            "answer=42",
            "-n",
            "-t",
            "draft",
        ])
        .unwrap();
        let Command::Build(args) = cli.command else {
            panic!("expected build subcommand");
        };
        let options = super::BuildOptions::try_from(args).unwrap();
        let (config, warnings) = build_config(&options).unwrap();

        assert_eq!(config.get_str("project"), Some("CLI Project"));
        assert_eq!(config.get_str("html_theme"), Some("basic"));
        assert_eq!(config.get_bool("nitpicky"), Some(true));
        assert_eq!(
            config.get_dict("html_context").and_then(|context| context.get("answer")),
            Some(&scribe_config::ConfigValue::Int(42))
        );
        let _ = warnings;
    }

    #[test]
    fn build_rejects_write_all_with_filenames() {
        let cli = Cli::try_parse_from(["scribe", "build", "src", "out", "-a", "index.rst"])
            .unwrap();
        let Command::Build(args) = cli.command else {
            panic!("expected build subcommand");
        };
        assert!(super::BuildOptions::try_from(args).is_err());
    }
}
