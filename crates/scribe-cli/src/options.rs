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

// ---------------------------------------------------------------------------
// Phase 31 process-tooling argument models
// ---------------------------------------------------------------------------

/// Clap-facing arguments for `scribe quickstart`.
///
/// Mirrors the frequently used flags of upstream `sphinx-quickstart`. In
/// `--batch` mode every value must come from a flag (interactive prompts are
/// skipped); without it the command asks for missing values on stdout/stdin.
#[derive(Debug, Args)]
pub struct QuickstartArgs {
    /// Directory in which to create the project.
    #[arg(value_name = "PATH", default_value = ".")]
    pub path: PathBuf,

    /// Non-interactive mode: use flag values and defaults, ask nothing.
    #[arg(long = "batch")]
    pub batch: bool,

    /// Suppress prompts and status messages.
    #[arg(short = 'q', long = "quiet")]
    pub quiet: bool,

    /// Overwrite existing project files.
    #[arg(long = "force")]
    pub force: bool,

    /// Project name.
    #[arg(short = 'p', long = "project", value_name = "NAME")]
    pub project: Option<String>,

    /// Author name(s).
    #[arg(short = 'a', long = "author", value_name = "AUTHOR")]
    pub author: Option<String>,

    /// Project version / release.
    #[arg(short = 'v', long = "release", visible_alias = "version", value_name = "VERSION")]
    pub version: Option<String>,

    /// Source directory (relative to PATH).
    #[arg(long = "source-dir", value_name = "PATH")]
    pub source_dir: Option<PathBuf>,

    /// Build directory (relative to PATH).
    #[arg(long = "build-dir", value_name = "PATH")]
    pub build_dir: Option<PathBuf>,

    /// Master document name (without suffix).
    #[arg(long = "master", value_name = "DOCNAME")]
    pub master_doc: Option<String>,

    /// Source file suffix, e.g. `.rst` or `.md`.
    #[arg(long = "suffix", value_name = "SUFFIX")]
    pub source_suffix: Option<String>,

    /// Comma-separated list of Sphinx extensions.
    #[arg(long = "extensions", value_name = "EXTENSIONS")]
    pub extensions: Option<String>,
}

/// Validated, clap-independent quickstart inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuickstartOptions {
    /// Project root directory.
    pub path: PathBuf,
    /// Non-interactive mode.
    pub batch: bool,
    /// Suppress prompts and status messages.
    pub quiet: bool,
    /// Overwrite existing files.
    pub force: bool,
    /// Project name.
    pub project: String,
    /// Author name(s).
    pub author: String,
    /// Version / release.
    pub version: String,
    /// Source directory relative to the project root.
    pub source_dir: PathBuf,
    /// Build directory relative to the project root.
    pub build_dir: PathBuf,
    /// Master document name without suffix.
    pub master_doc: String,
    /// Source file suffix including the leading dot.
    pub source_suffix: String,
    /// Extension module names.
    pub extensions: Vec<String>,
}

impl QuickstartOptions {
    /// Build options from parsed arguments, filling in upstream defaults for
    /// every value the caller did not supply.
    pub fn from_args(args: QuickstartArgs) -> Self {
        Self {
            path: args.path,
            batch: args.batch,
            quiet: args.quiet,
            force: args.force,
            project: args.project.filter(|value| !value.trim().is_empty()).unwrap_or_else(|| "Project name not set".to_owned()),
            author: args.author.filter(|value| !value.trim().is_empty()).unwrap_or_else(|| "Author name not set".to_owned()),
            version: args.version.filter(|value| !value.trim().is_empty()).unwrap_or_else(|| "0.1".to_owned()),
            source_dir: args.source_dir.unwrap_or_else(|| PathBuf::from("source")),
            build_dir: args.build_dir.unwrap_or_else(|| PathBuf::from("build")),
            master_doc: args.master_doc.filter(|value| !value.trim().is_empty()).unwrap_or_else(|| "index".to_owned()),
            source_suffix: normalize_suffix(
                &args.source_suffix.filter(|value| !value.trim().is_empty()).unwrap_or_else(|| ".rst".to_owned()),
            ),
            extensions: args
                .extensions
                .map(|raw| raw.split(',').map(str::trim).filter(|value| !value.is_empty()).map(str::to_owned).collect())
                .unwrap_or_default(),
        }
    }

    /// Reject values that would generate unusable projects.
    pub fn validate(&self) -> std::result::Result<(), Report> {
        if self.project.trim().is_empty() {
            return Err(Report::new(InvalidQuickstartValue {
                field: "project",
                reason: "the project name must not be empty",
            }));
        }
        if self.master_doc.trim().is_empty() || self.master_doc.contains('/') || self.master_doc.contains('\\') {
            return Err(Report::new(InvalidQuickstartValue {
                field: "master",
                reason: "the master document must be a single path component",
            }));
        }
        if !self.source_suffix.starts_with('.') || self.source_suffix.len() < 2 {
            return Err(Report::new(InvalidQuickstartValue {
                field: "suffix",
                reason: "the source suffix must start with `.` and have at least one character",
            }));
        }
        for path in [&self.source_dir, &self.build_dir] {
            if path.as_os_str().is_empty() {
                return Err(Report::new(InvalidQuickstartValue {
                    field: "directory",
                    reason: "source and build directories must not be empty",
                }));
            }
        }
        if self.source_dir == self.build_dir {
            return Err(Report::new(InvalidQuickstartValue {
                field: "build-dir",
                reason: "the source and build directories must differ",
            }));
        }
        Ok(())
    }
}

fn normalize_suffix(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.starts_with('.') {
        trimmed.to_owned()
    } else {
        format!(".{trimmed}")
    }
}

/// Diagnostic for an invalid quickstart value.
#[derive(Debug, Error, miette::Diagnostic)]
#[error("invalid quickstart value for `{field}`: {reason}")]
#[diagnostic(
    code(scribe::cli::invalid_quickstart_value),
    help("Correct the value and rerun `scribe quickstart --batch`.")
)]
pub struct InvalidQuickstartValue {
    /// Field that failed validation.
    pub field: &'static str,
    /// Explanation of the rejection.
    pub reason: &'static str,
}

/// Clap-facing arguments for `scribe clean`.
#[derive(Debug, Args)]
pub struct CleanArgs {
    /// Project directory to clean.
    #[arg(value_name = "PATH", default_value = ".")]
    pub path: PathBuf,

    /// Print the targets that would be removed without deleting anything.
    #[arg(long = "dry-run")]
    pub dry_run: bool,

    /// Configuration directory (defaults to PATH).
    #[arg(short = 'c', long = "conf-dir", value_name = "PATH")]
    pub conf_dir: Option<PathBuf>,

    /// Additional build/output directory to remove (relative to PATH).
    #[arg(short = 'o', long = "build-dir", value_name = "PATH")]
    pub build_dir: Vec<PathBuf>,
}

/// Clap-facing arguments for `scribe check`.
#[derive(Debug, Args)]
pub struct CheckArgs {
    /// Documentation source directory to inspect.
    #[arg(value_name = "PATH", default_value = ".")]
    pub path: PathBuf,

    /// Configuration directory (defaults to PATH).
    #[arg(short = 'c', long = "conf-dir", value_name = "PATH")]
    pub conf_dir: Option<PathBuf>,

    /// Turn warnings into errors (non-zero exit status).
    #[arg(short = 'W', long = "fail-on-warning")]
    pub warningiserror: bool,

    /// Accepted for compatibility with `scribe build` / `sphinx-build`;
    /// upstream treats it as a no-op that does not change the exit status.
    #[arg(long = "keep-going")]
    pub keep_going: bool,

    /// Report all missing cross-reference targets.
    #[arg(short = 'n', long = "nitpicky")]
    pub nitpicky: bool,

    /// Suppress status output; diagnostics are still printed.
    #[arg(short = 'q', long = "quiet")]
    pub quiet: bool,
}

#[cfg(test)]
mod tests {
    use super::{BuildOptions, ConfigOverride, Jobs, QuickstartOptions, ScribeCliError};
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

    #[test]
    fn quickstart_defaults_match_upstream_shape() {
        let options = QuickstartOptions::from_args(super::QuickstartArgs {
            path: "demo".into(),
            batch: true,
            quiet: true,
            force: false,
            project: None,
            author: None,
            version: None,
            source_dir: None,
            build_dir: None,
            master_doc: None,
            source_suffix: None,
            extensions: None,
        });
        assert_eq!(options.path, std::path::PathBuf::from("demo"));
        assert_eq!(options.project, "Project name not set");
        assert_eq!(options.source_dir, std::path::PathBuf::from("source"));
        assert_eq!(options.build_dir, std::path::PathBuf::from("build"));
        assert_eq!(options.source_suffix, ".rst");
        assert_eq!(options.master_doc, "index");
        assert!(options.extensions.is_empty());
        options.validate().unwrap();
    }

    #[test]
    fn quickstart_normalizes_suffix_and_splits_extensions() {
        let options = QuickstartOptions::from_args(super::QuickstartArgs {
            path: "demo".into(),
            batch: true,
            quiet: true,
            force: false,
            project: Some("Demo".to_owned()),
            author: Some("Ada".to_owned()),
            version: Some("1.0".to_owned()),
            source_dir: None,
            build_dir: None,
            master_doc: None,
            source_suffix: Some("md".to_owned()),
            extensions: Some("sphinx.ext.todo, sphinx.ext.viewcode".to_owned()),
        });
        assert_eq!(options.source_suffix, ".md");
        assert_eq!(options.extensions, vec!["sphinx.ext.todo", "sphinx.ext.viewcode"]);
        assert!(options.validate().is_ok());
    }

    #[test]
    fn quickstart_rejects_invalid_master_and_equal_dirs() {
        let mut options = QuickstartOptions::from_args(super::QuickstartArgs {
            path: "demo".into(),
            batch: true,
            quiet: true,
            force: false,
            project: None,
            author: None,
            version: None,
            source_dir: None,
            build_dir: None,
            master_doc: Some("a/b".to_owned()),
            source_suffix: None,
            extensions: None,
        });
        assert!(options.validate().is_err());
        options.master_doc = "index".to_owned();
        options.build_dir = options.source_dir.clone();
        assert!(options.validate().is_err());
    }
}
