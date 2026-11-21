//! Phase 28 live-reload dev server (`scribe serve` / `scribe watch`).
//!
//! Responsibilities, in one place:
//!
//! * watch the source tree (and optional conf dir) with `notify`, debouncing
//!   rapid events;
//! * map changed paths to docnames and ask Phase 08 `BuildEnvironment::update`
//!   for the authoritative minimal rebuild set (includes / toctree / config
//!   invalidation);
//! * run the deterministic HTML builder for affected changes and broadcast a
//!   reload event only after a successful rebuild;
//! * serve the output directory over HTTP with correct MIME types, directory
//!   indexes, 404s, a health endpoint and an SSE reload stream;
//! * inject a small client script into HTML responses only (`--no-reload`
//!   disables both injection and the stream).
//!
//! Pure helpers live in [`mapping`], [`mime`] and [`inject`]; the transport is
//! in [`http`].

pub mod http;
pub mod inject;
pub mod mapping;
pub mod mime;

use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Args;
use miette::{Diagnostic, IntoDiagnostic, Report, Result};
use notify::Watcher;
use thiserror::Error;
use tokio::sync::{broadcast, mpsc};

use crate::options::{BuildOptions, ColorChoice, ConfigOverride, Jobs};
use crate::{build_config, BuilderFailed};
use scribe_config::Config;
use scribe_core::{BuildEnvironment, InteropBridge};

use http::HttpServer;
use mapping::{classify_path, is_relevant_event, ChangeKind};

/// Arguments for `scribe serve` (also reachable as `scribe watch`).
#[derive(Debug, Clone, Args)]
pub struct ServeArgs {
    /// Path to documentation source files.
    #[arg(value_name = "SOURCE_DIR")]
    pub source_dir: PathBuf,

    /// Path to output directory.
    #[arg(value_name = "OUTPUT_DIR")]
    pub output_dir: PathBuf,

    /// Interface to listen on.
    #[arg(short = 'H', long = "host", default_value = "127.0.0.1", value_name = "HOST")]
    pub host: String,

    /// TCP port to listen on (`0` picks a free ephemeral port).
    #[arg(short = 'p', long = "port", default_value_t = 8000, value_name = "N")]
    pub port: u16,

    /// Builder to use while watching (html family only).
    #[arg(short = 'b', long = "builder", default_value = "html", value_name = "BUILDER")]
    pub builder: String,

    /// Directory containing `conf.py` / `scribe.toml`.
    #[arg(short = 'c', long = "conf-dir", value_name = "PATH")]
    pub confdir: Option<PathBuf>,

    /// Override a configuration setting (`name=value`).
    #[arg(short = 'D', long = "define", value_name = "setting=value", value_parser = |raw: &str| ConfigOverride::parse(raw))]
    pub define: Vec<ConfigOverride>,

    /// Disable live-reload injection and the SSE endpoint.
    #[arg(long = "no-reload")]
    pub no_reload: bool,

    /// Debounce window for filesystem events, in milliseconds.
    #[arg(long = "delay", default_value_t = 150, value_name = "MILLIS")]
    pub delay_ms: u64,

    /// Suppress status lines (errors are still printed).
    #[arg(short = 'q', long = "quiet")]
    pub quiet: bool,
}

/// Diagnostics raised by the dev server. Messages are kept as strings so the
/// CLI boundary does not need to re-export every underlying error type.
#[derive(Debug, Error, Diagnostic)]
pub enum ServeError {
    /// Source directory does not exist or is not a directory.
    #[error("source directory `{}` does not exist", .0.display())]
    #[diagnostic(
        code(scribe::serve::missing_source),
        help("Create the directory or point scribe at an existing documentation source tree.")
    )]
    MissingSource(PathBuf),

    /// The HTTP listener could not be bound (e.g. port already in use).
    #[error("cannot listen on {host}:{port}: {message}")]
    #[diagnostic(
        code(scribe::serve::bind_failed),
        help("Choose a different port with --port, or stop the process currently listening on it.")
    )]
    Bind {
        /// Requested host.
        host: String,
        /// Requested port.
        port: u16,
        /// OS error message.
        message: String,
    },

    /// The filesystem watcher could not be installed.
    #[error("cannot watch the source tree: {message}")]
    #[diagnostic(
        code(scribe::serve::watch_failed),
        help("Check filesystem permissions and that the source path is readable.")
    )]
    Watch {
        /// Watcher error message.
        message: String,
    },

    /// A build failed (initial or incremental).
    #[error("{context} build failed: {message}")]
    #[diagnostic(
        code(scribe::serve::build_failed),
        help("Fix the reported source/configuration problem; the server will retry on the next change.")
    )]
    Build {
        /// `initial` or `incremental`.
        context: &'static str,
        /// Builder error message.
        message: String,
    },

    /// The requested builder cannot serve live HTML.
    #[error("builder `{0}` is not supported by `scribe serve`")]
    #[diagnostic(
        code(scribe::serve::unsupported_builder),
        help("Use --builder html (default), dirhtml or singlehtml.")
    )]
    UnsupportedBuilder(String),

    /// Configuration could not be loaded.
    #[error("failed to load configuration from `{}`: {message}", .path.display())]
    #[diagnostic(
        code(scribe::serve::config_failed),
        help("Point -c at a directory containing conf.py or scribe.toml, or fix the reported error.")
    )]
    Config {
        /// Configuration directory that was searched.
        path: PathBuf,
        /// Configuration error message.
        message: String,
    },
}

/// Entry point called from `crate::execute`.
pub fn run_serve(args: ServeArgs) -> Result<()> {
    if !args.source_dir.is_dir() {
        return Err(Report::new(ServeError::MissingSource(args.source_dir.clone())));
    }
    if !matches!(args.builder.as_str(), "html" | "dirhtml" | "singlehtml") {
        return Err(Report::new(ServeError::UnsupportedBuilder(args.builder.clone())));
    }

    let build_options = to_build_options(&args);
    let config_search_path = args
        .confdir
        .clone()
        .unwrap_or_else(|| args.source_dir.clone());
    let (config, _warnings) = build_config(&build_options)
        .map_err(|error| Report::new(ServeError::Config { path: config_search_path.clone(), message: error.to_string() }))?;

    let suffixes = effective_suffixes(&config);
    let source_root = std::fs::canonicalize(&args.source_dir).unwrap_or_else(|_| args.source_dir.clone());
    let conf_root = std::fs::canonicalize(&config_search_path).ok();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .into_diagnostic()?;
    runtime.block_on(serve_async(args, build_options, config, suffixes, source_root, conf_root))
}

fn to_build_options(args: &ServeArgs) -> BuildOptions {
    BuildOptions {
        source_dir: args.source_dir.clone(),
        output_dir: args.output_dir.clone(),
        filenames: Vec::new(),
        builder: args.builder.clone(),
        jobs: Jobs::Auto,
        force_all: false,
        freshenv: false,
        doctreedir: None,
        confdir: args.confdir.clone(),
        noconfig: false,
        define: args.define.clone(),
        htmldefine: Vec::new(),
        tags: Vec::new(),
        nitpicky: false,
        verbosity: 0,
        quiet: args.quiet,
        really_quiet: false,
        color: ColorChoice::Never,
        warnfile: None,
        warningiserror: false,
        keep_going: false,
        traceback: false,
        pdb: false,
        exception_on_warning: false,
    }
}

fn effective_suffixes(config: &Config) -> Vec<String> {
    let suffixes = config.source_suffixes();
    if suffixes.is_empty() {
        vec![".rst".to_owned(), ".md".to_owned(), ".txt".to_owned()]
    } else {
        suffixes
    }
}

/// Run the deterministic HTML-family builder against a *live* environment.
///
/// Phase 67: the caller keeps one `BuildEnvironment` for the whole server
/// lifetime. The initial build uses `freshenv` and persists the cache; later
/// rebuilds reuse the in-memory environment and the builder's incremental path
/// re-parses / re-renders only `UpdateReport::files_to_rebuild()`, merging the
/// cached per-document search fragments for every other page.
fn perform_build(
    options: &BuildOptions,
    config: &Config,
    builder: &str,
    fresh: bool,
    force: bool,
    env: &mut BuildEnvironment,
) -> Result<scribe_builders::html::BuildResult, Report> {
    let builder_options = scribe_builders::html::BuildOptions {
        source_dir: options.source_dir.clone(),
        output_dir: options.output_dir.clone(),
        builder_name: builder.to_owned(),
        force_all: force && !fresh,
        freshenv: fresh,
        doctree_dir: options.doctreedir.clone(),
        filenames: Vec::new(),
        worker_count: options.worker_count(),
        really_quiet: true,
        bridge: Some(Arc::new(InteropBridge::from_config(config, &options.source_dir))),
        reuse_environment: !fresh,
    };
    let result = match builder {
        "dirhtml" => scribe_builders::DirectoryHTMLBuilder::build(&builder_options, config, env),
        "singlehtml" => scribe_builders::SingleFileHTMLBuilder::build(&builder_options, config, env),
        _ => scribe_builders::html::StandaloneHtmlBuilder::build(&builder_options, config, env),
    };
    result.map_err(|source| Report::new(BuilderFailed { builder: builder.to_owned(), source }))
}

async fn serve_async(
    args: ServeArgs,
    build_options: BuildOptions,
    config: Config,
    suffixes: Vec<String>,
    source_root: PathBuf,
    conf_root: Option<PathBuf>,
) -> Result<()> {
    let reload_enabled = !args.no_reload;
    let quiet = args.quiet;
    let output_dir = build_options.output_dir.clone();

    // ---- initial full build ------------------------------------------------
    // Phase 67: one live environment is kept for the whole server lifetime so
    // subsequent rebuilds skip the environment-cache reload and save.
    let mut env = BuildEnvironment::with_config(&build_options.source_dir, &config);
    let started = Instant::now();
    let initial = perform_build(&build_options, &config, &args.builder, true, false, &mut env)
        .map_err(|error| Report::new(ServeError::Build { context: "initial", message: error.to_string() }))?;
    let initial_docs = initial.built_docnames;
    if !quiet {
        println!(
            "[scribe] initial build: {} page(s) in {:.2?}",
            initial_docs.len(),
            started.elapsed()
        );
    }

    // ---- HTTP server -------------------------------------------------------
    let server = HttpServer::bind(&args.host, args.port, output_dir.clone(), reload_enabled)
        .await
        .map_err(|error| Report::new(ServeError::Bind {
            host: args.host.clone(),
            port: args.port,
            message: error.to_string(),
        }))?;
    let address = server.local_addr();
    let reload_tx = server.reload_sender();

    // ---- filesystem watcher ------------------------------------------------
    // Install the watcher *before* publishing the address: the readiness probe
    // in integration tests (and human users) may otherwise edit a source file
    // in the window between "serving" output and watch registration.
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<PathBuf>();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event {
            // Reading sources during a rebuild emits Access events on Linux;
            // ignoring them prevents a self-sustaining rebuild loop.
            if !is_relevant_event(&event.kind) {
                return;
            }
            for path in &event.paths {
                let _ = event_tx.send(path.clone());
            }
        }
    })
    .map_err(|error| Report::new(ServeError::Watch { message: error.to_string() }))?;

    watcher
        .watch(&source_root, notify::RecursiveMode::Recursive)
        .map_err(|error| Report::new(ServeError::Watch { message: error.to_string() }))?;
    if let Some(conf) = &conf_root {
        if conf.is_dir() && conf != &source_root {
            watcher
                .watch(conf, notify::RecursiveMode::Recursive)
                .map_err(|error| Report::new(ServeError::Watch { message: error.to_string() }))?;
        }
    }
    // Keep the watcher alive for the whole serve loop.
    let _watcher = watcher;

    let (shutdown_tx, shutdown_rx) = broadcast::channel::<()>(4);
    let server_task = tokio::spawn(server.run(shutdown_rx));
    println!("[scribe] serving at http://{address} (Ctrl-C to stop)");
    let _ = std::io::stdout().flush();

    // ---- debounced rebuild loop -------------------------------------------
    let debounce = Duration::from_millis(args.delay_ms);
    let mut pending: HashSet<PathBuf> = HashSet::new();
    let mut deadline: Option<tokio::time::Instant> = None;

    loop {
        let timer = async {
            if let Some(deadline) = deadline {
                tokio::time::sleep_until(deadline).await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::select! {
            biased;
            _ = tokio::signal::ctrl_c() => {
                println!("[scribe] shutting down");
                let _ = shutdown_tx.send(());
                let _ = server_task.await;
                break;
            }
            received = event_rx.recv() => {
                match received {
                    Some(path) => {
                        pending.insert(path);
                        deadline = Some(tokio::time::Instant::now() + debounce);
                    }
                    None => break,
                }
            }
            _ = timer => {
                deadline = None;
                let batch = std::mem::take(&mut pending);
                if batch.is_empty() {
                    continue;
                }
                let mut force = false;
                for path in &batch {
                    match classify_path(path, &source_root, conf_root.as_deref(), &suffixes) {
                        ChangeKind::SourceDocument(_) => {}
                        ChangeKind::Config | ChangeKind::Asset | ChangeKind::Other => force = true,
                    }
                }

                let started = Instant::now();
                match perform_build(&build_options, &config, &args.builder, false, force, &mut env) {
                    Ok(result) => {
                        let rebuild: Vec<String> = result.built_docnames;
                        let removed = result.removed_docnames;
                        for docname in &removed {
                            let path = output_dir.join(format!("{docname}.html"));
                            let _ = tokio::fs::remove_file(path).await;
                        }
                        if rebuild.is_empty() && removed.is_empty() && !force {
                            continue;
                        }
                        // Only emit a reload after a successful rebuild. An
                        // empty page list makes the client reload the current
                        // page (used for config/asset changes).
                        let pages: Vec<String> = if force {
                            Vec::new()
                        } else {
                            rebuild.iter().map(|name| format!("{name}.html")).collect()
                        };
                        let _ = reload_tx.send(pages);
                        if !quiet {
                            let mut names: Vec<&str> = rebuild.iter().map(String::as_str).collect();
                            names.extend(removed.iter().map(String::as_str));
                            let summary = if names.is_empty() {
                                "all".to_owned()
                            } else {
                                names.join(", ")
                            };
                            println!("[scribe] rebuild {summary} in {:.2?}", started.elapsed());
                            let _ = std::io::stdout().flush();
                        }
                    }
                    Err(error) => {
                        eprintln!("[scribe] rebuild failed: {error}");
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{effective_suffixes, to_build_options, ServeArgs, ServeError};
    use crate::options::ConfigOverride;
    use clap::Parser;
    use std::path::PathBuf;

    fn args() -> ServeArgs {
        ServeArgs {
            source_dir: PathBuf::from("src"),
            output_dir: PathBuf::from("out"),
            host: "127.0.0.1".to_owned(),
            port: 8000,
            builder: "html".to_owned(),
            confdir: None,
            define: Vec::new(),
            no_reload: false,
            delay_ms: 150,
            quiet: false,
        }
    }

    #[test]
    fn serve_arguments_have_documented_defaults() {
        let cli = crate::Cli::try_parse_from(["scribe", "serve", "src", "out"]).unwrap();
        let crate::Command::Serve(parsed) = cli.command else {
            panic!("expected serve subcommand");
        };
        assert_eq!(parsed.host, "127.0.0.1");
        assert_eq!(parsed.port, 8000);
        assert_eq!(parsed.builder, "html");
        assert!(!parsed.no_reload);
    }

    #[test]
    fn watch_is_an_alias_for_serve() {
        let cli = crate::Cli::try_parse_from([
            "scribe", "watch", "src", "out", "--host", "0.0.0.0", "--port", "0", "--no-reload",
        ])
        .unwrap();
        let crate::Command::Serve(parsed) = cli.command else {
            panic!("expected serve subcommand for watch alias");
        };
        assert_eq!(parsed.host, "0.0.0.0");
        assert_eq!(parsed.port, 0);
        assert!(parsed.no_reload);
    }

    #[test]
    fn serve_accepts_define_overrides() {
        let cli = crate::Cli::try_parse_from([
            "scribe", "serve", "src", "out", "-D", "html_theme=basic", "-D", "project=X",
        ])
        .unwrap();
        let crate::Command::Serve(parsed) = cli.command else {
            panic!("expected serve subcommand");
        };
        assert_eq!(parsed.define, vec![
            ConfigOverride { key: "html_theme".to_owned(), value: "basic".to_owned() },
            ConfigOverride { key: "project".to_owned(), value: "X".to_owned() },
        ]);
    }

    #[test]
    fn build_options_carry_serve_inputs() {
        let mut parsed = args();
        parsed.define.push(ConfigOverride { key: "html_theme".to_owned(), value: "basic".to_owned() });
        let options = to_build_options(&parsed);
        assert_eq!(options.source_dir, PathBuf::from("src"));
        assert_eq!(options.output_dir, PathBuf::from("out"));
        assert_eq!(options.define.len(), 1);
        assert!(!options.freshenv);
    }

    #[test]
    fn error_messages_are_stable() {
        let message = ServeError::Bind { host: "127.0.0.1".to_owned(), port: 8000, message: "in use".to_owned() }.to_string();
        assert!(message.contains("127.0.0.1:8000"));
        assert!(message.contains("in use"));
        assert!(ServeError::MissingSource(PathBuf::from("nope")).to_string().contains("nope"));
    }

    #[test]
    fn suffixes_follow_the_loaded_configuration() {
        let config = scribe_config::Config::new();
        // A default Config registers `.rst`; the watcher maps exactly the
        // suffixes Sphinx treats as sources.
        assert_eq!(effective_suffixes(&config), vec![".rst"]);
    }
}
