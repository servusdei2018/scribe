//! Reusable option model for the `build` subcommand.
//!
//! The clap-facing [`BuildArgs`] struct mirrors the flags of upstream
//! `sphinx/cmd/build.py::get_parser()`. It is converted into the phase-stable
//! [`BuildOptions`] model via [`TryFrom`], which also performs cross-field
//! validation. No other crate needs to know about clap.

use clap::{ArgAction, Args};
use miette::Report;
use std::fmt;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use thiserror::Error;

/// Structured validation errors raised while building [`BuildOptions`].
#[derive(Debug, Error)]
pub enum ScribeCliError {
    /// `-a/--write-all` was combined with explicit filenames.
    #[error("cannot combine --write-all (-a) with explicit filenames")]
    WriteAllWithFilenames,

    /// A `-D`/`-A` override was not in `name=value` form.
    #[error("invalid `name=value` override: `{0}`")]
    InvalidOverride(String),

    /// A `-j/--jobs` value was neither a positive integer nor `auto`.
    #[error("job count must be a positive integer or `auto`, got `{0}`")]
    InvalidJobs(String),

    /// A required positional directory was not supplied.
    #[error("missing required argument `{0}`")]
    MissingArgument(&'static str),
}

/// Diagnostic for cross-field build option errors.
#[derive(Debug, Error, miette::Diagnostic)]
#[error("invalid build options")]
#[diagnostic(
    code(scribe::cli::invalid_build_options),
    help("Remove `-a/--write-all`, or omit the trailing filenames.")
)]
pub struct InvalidBuildOptions;

/// Diagnostic for a missing `SOURCE_DIR` or `OUTPUT_DIR`.
#[derive(Debug, Error, miette::Diagnostic)]
#[error("missing required argument `{name}`")]
#[diagnostic(
    code(scribe::cli::missing_argument),
    help("Run `scribe build --help` to see the expected argument order.")
)]
pub struct MissingBuildArgument {
    /// Name of the missing positional argument.
    pub name: &'static str,
}

/// Number of parallel build workers, mirroring sphinx-build's `-j` parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Jobs {
    /// Use the number of logical CPUs available to the process.
    Auto,
    /// Use an explicit positive count.
    Count(NonZeroUsize),
}

impl Jobs {
    /// Resolve this value to a concrete worker count.
    pub fn get(self) -> usize {
        match self {
            Self::Auto => std::thread::available_parallelism()
                .map(NonZeroUsize::get)
                .unwrap_or(1),
            Self::Count(count) => count.get(),
        }
    }
}

impl fmt::Display for Jobs {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => formatter.write_str("auto"),
            Self::Count(count) => count.fmt(formatter),
        }
    }
}

impl FromStr for Jobs {
    type Err = ScribeCliError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        if raw == "auto" {
            return Ok(Self::Auto);
        }
        raw.parse::<NonZeroUsize>()
            .map(Self::Count)
            .map_err(|_| ScribeCliError::InvalidJobs(raw.to_owned()))
    }
}

/// Output colour policy, corresponding to upstream `--color`/`--no-color`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorChoice {
    /// Auto-detect terminal support.
    #[default]
    Auto,
    /// Always emit ANSI colour.
    Always,
    /// Never emit ANSI colour.
    Never,
}

/// A validated `-D`/`-A` override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigOverride {
    /// Configuration key (everything before the first `=`).
    pub key: String,
    /// Configuration value (everything after the first `=`).
    pub value: String,
}

impl ConfigOverride {
    /// Split a raw `name=value` string, rejecting a missing or empty name.
    pub fn parse(raw: &str) -> Result<Self, ScribeCliError> {
        match raw.split_once('=') {
            Some((key, value)) if !key.is_empty() => Ok(Self {
                key: key.to_owned(),
                value: value.to_owned(),
            }),
            _ => Err(ScribeCliError::InvalidOverride(raw.to_owned())),
        }
    }
}

impl FromStr for ConfigOverride {
    type Err = ScribeCliError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::parse(raw)
    }
}

fn parse_jobs(raw: &str) -> Result<Jobs, ScribeCliError> {
    Jobs::from_str(raw)
}

fn parse_override(raw: &str) -> Result<ConfigOverride, ScribeCliError> {
    ConfigOverride::parse(raw)
}

/// Clap-facing arguments for `scribe build`.
///
/// Long flag names and short aliases are taken verbatim from upstream
/// `sphinx/cmd/build.py::get_parser()`.
#[derive(Debug, Args)]
pub struct BuildArgs {
    /// Path to documentation source files.
    // Optional at the clap layer so `--version` works standalone, like upstream
    // sphinx-build; presence is enforced by `BuildOptions::from_args`.
    #[arg(value_name = "SOURCE_DIR", required_unless_present = "show_version")]
    pub source_dir: Option<PathBuf>,

    /// Path to output directory.
    #[arg(value_name = "OUTPUT_DIR", required_unless_present = "show_version")]
    pub output_dir: Option<PathBuf>,

    /// Specific files to rebuild (ignored with `--write-all`).
    #[arg(value_name = "FILENAMES")]
    pub filenames: Vec<PathBuf>,

    /// Builder to use (default: 'html').
    #[arg(short = 'b', long = "builder", default_value = "html", value_name = "BUILDER")]
    pub builder: String,

    /// Run in parallel with N processes; 'auto' uses the CPU count.
    #[arg(short = 'j', long = "jobs", default_value = "1", value_name = "N", value_parser = parse_jobs)]
    pub jobs: Jobs,

    /// Write all files (default: only write new and changed files).
    #[arg(short = 'a', long = "write-all")]
    pub force_all: bool,

    /// Don't use a saved environment, always read all files.
    #[arg(short = 'E', long = "fresh-env")]
    pub freshenv: bool,

    /// Directory for doctree and environment files.
    #[arg(short = 'd', long = "doctree-dir", value_name = "PATH")]
    pub doctreedir: Option<PathBuf>,

    /// Directory for the configuration file (conf.py).
    #[arg(short = 'c', long = "conf-dir", value_name = "PATH")]
    pub confdir: Option<PathBuf>,

    /// Use no configuration file, only settings from -D options.
    #[arg(short = 'C', long = "isolated")]
    pub noconfig: bool,

    /// Override a setting in the configuration file.
    #[arg(short = 'D', long = "define", value_name = "setting=value", value_parser = parse_override)]
    pub define: Vec<ConfigOverride>,

    /// Pass a value into HTML templates.
    #[arg(short = 'A', long = "html-define", value_name = "name=value", value_parser = parse_override)]
    pub htmldefine: Vec<ConfigOverride>,

    /// Define a tag: include "only" blocks with TAG.
    #[arg(short = 't', long = "tag", value_name = "TAG")]
    pub tags: Vec<String>,

    /// Warn about all missing references.
    #[arg(short = 'n', long = "nitpicky")]
    pub nitpicky: bool,

    /// Increase verbosity (can be repeated).
    #[arg(short = 'v', long = "verbose", action = ArgAction::Count)]
    pub verbosity: u8,

    /// No output on stdout, just warnings on stderr.
    #[arg(short = 'q', long = "quiet")]
    pub quiet: bool,

    /// No output at all, not even warnings.
    #[arg(short = 'Q', long = "silent")]
    pub really_quiet: bool,

    /// Emit coloured output (auto-detected by default).
    #[arg(long = "color", conflicts_with = "no_color")]
    pub color: bool,

    /// Do not emit coloured output.
    #[arg(short = 'N', long = "no-color", conflicts_with = "color")]
    pub no_color: bool,

    /// Write warnings (and errors) to the given file.
    #[arg(short = 'w', long = "warning-file", value_name = "FILE")]
    pub warnfile: Option<PathBuf>,

    /// Turn warnings into errors.
    #[arg(short = 'W', long = "fail-on-warning")]
    pub warningiserror: bool,

    /// Continue building after warnings when `-W` is active.
    #[arg(long = "keep-going")]
    pub keep_going: bool,

    /// Show a full traceback on exception.
    #[arg(short = 'T', long = "show-traceback")]
    pub traceback: bool,

    /// Run the debugger on exception.
    #[arg(short = 'P', long = "pdb")]
    pub pdb: bool,

    /// Raise an exception on warnings.
    #[arg(long = "exception-on-warning")]
    pub exception_on_warning: bool,

    /// Print the emulated sphinx-build version and exit.
    #[arg(long = "version")]
    pub show_version: bool,
}

/// Phase-stable, clap-independent model of a documentation build request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildOptions {
    /// Source directory (`SOURCE_DIR`).
    pub source_dir: PathBuf,
    /// Output directory (`OUTPUT_DIR`).
    pub output_dir: PathBuf,
    /// Optional list of files to rebuild.
    pub filenames: Vec<PathBuf>,
    /// Builder name; defaults to `html`.
    pub builder: String,
    /// Resolved worker count.
    pub jobs: Jobs,
    /// Whether to write every file.
    pub force_all: bool,
    /// Whether to discard the saved environment.
    pub freshenv: bool,
    /// Explicit doctree directory, if supplied.
    pub doctreedir: Option<PathBuf>,
    /// Explicit configuration directory, if supplied.
    pub confdir: Option<PathBuf>,
    /// Whether configuration loading is disabled.
    pub noconfig: bool,
    /// `conf.py` overrides from `-D`.
    pub define: Vec<ConfigOverride>,
    /// HTML template overrides from `-A`.
    pub htmldefine: Vec<ConfigOverride>,
    /// Enabled tags from `-t`.
    pub tags: Vec<String>,
    /// Nitpicky mode.
    pub nitpicky: bool,
    /// Verbosity level from repeated `-v`.
    pub verbosity: u8,
    /// Suppress stdout status output.
    pub quiet: bool,
    /// Suppress all output.
    pub really_quiet: bool,
    /// Resolved colour policy.
    pub color: ColorChoice,
    /// Warning file path.
    pub warnfile: Option<PathBuf>,
    /// Treat warnings as errors.
    pub warningiserror: bool,
    /// Keep going after warnings.
    pub keep_going: bool,
    /// Show tracebacks on exception.
    pub traceback: bool,
    /// Drop into the debugger on exception.
    pub pdb: bool,
    /// Raise an exception on warnings.
    pub exception_on_warning: bool,
}

impl BuildOptions {
    /// Convert clap arguments into validated build options.
    pub fn from_args(args: BuildArgs) -> Result<Self, Report> {
        let source_dir = args
            .source_dir
            .ok_or_else(|| Report::new(MissingBuildArgument { name: "SOURCE_DIR" }))?;
        let output_dir = args
            .output_dir
            .ok_or_else(|| Report::new(MissingBuildArgument { name: "OUTPUT_DIR" }))?;
        let options = Self {
            source_dir,
            output_dir,
            filenames: args.filenames,
            builder: args.builder,
            jobs: args.jobs,
            force_all: args.force_all,
            freshenv: args.freshenv,
            doctreedir: args.doctreedir,
            confdir: args.confdir,
            noconfig: args.noconfig,
            define: args.define,
            htmldefine: args.htmldefine,
            tags: args.tags,
            nitpicky: args.nitpicky,
            verbosity: args.verbosity,
            quiet: args.quiet,
            really_quiet: args.really_quiet,
            color: if args.no_color {
                ColorChoice::Never
            } else if args.color {
                ColorChoice::Always
            } else {
                ColorChoice::Auto
            },
            warnfile: args.warnfile,
            warningiserror: args.warningiserror,
            keep_going: args.keep_going,
            traceback: args.traceback,
            pdb: args.pdb,
            exception_on_warning: args.exception_on_warning,
        };
        options.validate()?;
        Ok(options)
    }

    /// Reject missing directories and combinations upstream also rejects.
    pub fn validate(&self) -> Result<(), Report> {
        if self.source_dir.as_os_str().is_empty() {
            return Err(Report::new(MissingBuildArgument { name: "SOURCE_DIR" }));
        }
        if self.output_dir.as_os_str().is_empty() {
            return Err(Report::new(MissingBuildArgument { name: "OUTPUT_DIR" }));
        }
        if self.force_all && !self.filenames.is_empty() {
            return Err(Report::new(InvalidBuildOptions));
        }
        Ok(())
    }

    /// Effective doctree directory: `--doctree-dir`, else `OUTPUT_DIR/.doctrees`.
    pub fn doctree_dir(&self) -> PathBuf {
        self.doctreedir
            .clone()
            .unwrap_or_else(|| self.output_dir.join(".doctrees"))
    }

    /// Effective configuration directory: `None` under `--isolated`, otherwise
    /// `--conf-dir` or the source directory.
    pub fn conf_dir(&self) -> Option<&Path> {
        if self.noconfig {
            None
        } else {
            Some(self.confdir.as_deref().unwrap_or(self.source_dir.as_path()))
        }
    }

    /// Effective parallel worker count.
    pub fn worker_count(&self) -> usize {
        self.jobs.get()
    }
}

impl TryFrom<BuildArgs> for BuildOptions {
    type Error = Report;

    fn try_from(args: BuildArgs) -> Result<Self, Self::Error> {
        Self::from_args(args)
    }
}

#[cfg(test)]
mod tests {
    use super::{BuildOptions, ConfigOverride, Jobs, ScribeCliError};
    use std::num::NonZeroUsize;

    #[test]
    fn jobs_accept_positive_numbers_and_auto() {
        assert_eq!("auto".parse::<Jobs>().unwrap(), Jobs::Auto);
        let four = NonZeroUsize::new(4).unwrap();
        assert_eq!("4".parse::<Jobs>().unwrap(), Jobs::Count(four));
        assert_eq!(Jobs::Count(four).get(), 4);
        assert!(Jobs::Auto.get() >= 1);
    }

    #[test]
    fn jobs_reject_zero_negative_and_garbage() {
        for raw in ["0", "-1", "abc", ""] {
            assert!(matches!(
                raw.parse::<Jobs>(),
                Err(ScribeCliError::InvalidJobs(_))
            ));
        }
    }

    #[test]
    fn overrides_require_a_name_and_equals_sign() {
        let parsed = ConfigOverride::parse("nitpicky=true").unwrap();
        assert_eq!(parsed.key, "nitpicky");
        assert_eq!(parsed.value, "true");
        assert_eq!(ConfigOverride::parse("key=").unwrap().value, "");
        for raw in ["no-equals", "=value", ""] {
            assert!(matches!(
                ConfigOverride::parse(raw),
                Err(ScribeCliError::InvalidOverride(_))
            ));
        }
    }

    #[test]
    fn resolve_doctree_and_conf_dirs_like_sphinx() {
        let args = super::BuildArgs {
            source_dir: Some("src".into()),
            output_dir: Some("out".into()),
            filenames: Vec::new(),
            builder: "html".to_owned(),
            jobs: Jobs::Auto,
            force_all: false,
            freshenv: false,
            doctreedir: None,
            confdir: None,
            noconfig: false,
            define: Vec::new(),
            htmldefine: Vec::new(),
            tags: Vec::new(),
            nitpicky: false,
            verbosity: 0,
            quiet: false,
            really_quiet: false,
            color: false,
            no_color: false,
            warnfile: None,
            warningiserror: false,
            keep_going: false,
            traceback: false,
            pdb: false,
            exception_on_warning: false,
            show_version: false,
        };
        let options = BuildOptions::try_from(args).unwrap();
        assert_eq!(options.doctree_dir(), std::path::PathBuf::from("out/.doctrees"));
        assert_eq!(options.conf_dir(), Some(std::path::Path::new("src")));
    }
}
