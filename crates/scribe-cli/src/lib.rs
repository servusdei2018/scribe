//! Command-line surface for `scribe`.
//!
//! The `build` subcommand mirrors upstream `sphinx/cmd/build.py::get_parser()`
//! flag-for-flag. Parsed input is normalized into [`BuildOptions`], which is the
//! reusable, clap-independent model consumed by later phases. Phase 01 stops
//! after validation and emits a structured `miette` diagnostic; it never
//! panics on user input.

pub mod options;

pub use options::{
    BuildArgs, BuildOptions, ColorChoice, ConfigOverride, InvalidBuildOptions, Jobs,
    MissingBuildArgument, ScribeCliError,
};

use clap::{Parser, Subcommand};
use miette::{Diagnostic, IntoDiagnostic, Report, Result, WrapErr};
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

    /// Print a quickstart banner (scaffolding lands in a later phase).
    Quickstart {
        /// Target directory for the new project.
        #[arg(value_name = "PATH", default_value = ".")]
        path: std::path::PathBuf,
    },

    /// Remove the local `.scribe-cache` directory, if present.
    Clean {
        /// Directory to clean.
        #[arg(value_name = "PATH", default_value = ".")]
        path: std::path::PathBuf,
    },

    /// Report discovered source files (discovery lands in Phase 08).
    Check {
        /// Directory to inspect.
        #[arg(value_name = "PATH", default_value = ".")]
        path: std::path::PathBuf,
    },
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

/// Diagnostic for subcommands that are still placeholders.
#[derive(Debug, Error, Diagnostic)]
#[error("`scribe {subcommand}` is not implemented yet")]
#[diagnostic(
    code(scribe::not_implemented),
    help("This subcommand is a Phase 01 placeholder with deterministic, non-panicking behaviour.")
)]
pub struct NotImplemented {
    /// Name of the placeholder subcommand.
    pub subcommand: &'static str,
}

/// Parse process arguments and execute the selected subcommand.
pub fn run() -> Result<()> {
    execute(Cli::parse())
}

/// Execute an already-parsed command. Exposed for embedding and tests.
pub fn execute(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Build(args) => run_build(args),
        Command::Quickstart { path } => run_quickstart(&path),
        Command::Clean { path } => run_clean(&path),
        Command::Check { path } => run_check(&path),
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

    Err(Report::new(UnimplementedBuilder {
        builder: options.builder.clone(),
    }))
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

fn run_quickstart(path: &Path) -> Result<()> {
    println!("scribe quickstart (Phase 01 placeholder)");
    println!("  target directory : {}", path.display());
    println!("  source suffix    : .rst");
    println!("  master document  : index");
    println!("Project scaffolding will be generated in a later phase.");
    Ok(())
}

fn run_check(path: &Path) -> Result<()> {
    println!("No source discovered yet at {}.", path.display());
    Ok(())
}

fn run_clean(path: &Path) -> Result<()> {
    let cache = path.join(".scribe-cache");
    let exists = cache
        .try_exists()
        .into_diagnostic()
        .wrap_err_with(|| format!("cannot inspect {}", cache.display()))?;
    if exists {
        std::fs::remove_dir_all(&cache)
            .into_diagnostic()
            .wrap_err_with(|| format!("cannot remove {}", cache.display()))?;
        println!("Removed cache directory {}", cache.display());
    } else {
        println!("No cache directory at {}; nothing to clean.", cache.display());
    }
    Ok(())
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
