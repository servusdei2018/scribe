//! Phase 31 `scribe quickstart` implementation.
//!
//! Generates a Sphinx-compatible documentation skeleton: `conf.py`, the
//! master document, `Makefile`/`make.bat` and `.gitignore`. In `--batch` mode
//! no question is asked and all values come from flags or upstream defaults;
//! otherwise missing values are requested interactively. Existing files are
//! never overwritten unless `--force` is passed.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Component, Path, PathBuf};

use miette::{Diagnostic, Result};
use thiserror::Error;

use crate::options::{QuickstartArgs, QuickstartOptions};

/// Structured diagnostics raised by `scribe quickstart`.
#[derive(Debug, Error, Diagnostic)]
pub enum QuickstartError {
    /// A generated file already exists and `--force` was not supplied.
    #[error("refusing to overwrite `{}`; pass --force to replace it", .path.display())]
    #[diagnostic(
        code(scribe::quickstart::already_exists),
        help("Choose a different target directory or rerun with --force.")
    )]
    AlreadyExists {
        /// File that would be overwritten.
        path: PathBuf,
    },

    /// A source/build directory escaped the project root.
    #[error("unsafe directory `{path}`: {reason}")]
    #[diagnostic(
        code(scribe::quickstart::unsafe_path),
        help("Source and build directories must be relative paths inside the project.")
    )]
    UnsafePath {
        /// Rejected path.
        path: PathBuf,
        /// Why it was rejected.
        reason: String,
    },

    /// Reading an interactive answer failed.
    #[error("cannot read interactive input: {0}")]
    #[diagnostic(
        code(scribe::quickstart::input_failed),
        help("Use --batch to provide every value via command-line flags.")
    )]
    Input(String),

    /// A filesystem operation failed.
    #[error("cannot create `{}`: {message}", .path.display())]
    #[diagnostic(
        code(scribe::quickstart::io_failed),
        help("Check filesystem permissions and that the target path is usable.")
    )]
    Io {
        /// Path of the failed operation.
        path: PathBuf,
        /// OS error message.
        message: String,
    },
}

/// Outcome of a scaffolding pass.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QuickstartReport {
    /// Absolute project root.
    pub root: PathBuf,
    /// Files and directories that were created.
    pub created: Vec<PathBuf>,
    /// Whether existing files were overwritten.
    pub force: bool,
}

/// Execute `scribe quickstart` from parsed arguments.
pub fn run_quickstart(args: QuickstartArgs) -> Result<QuickstartReport> {
    let mut args = args;
    // `--quiet` implies non-interactive operation, matching upstream
    // `sphinx-quickstart -q`: no questions are asked at all.
    if !args.batch && !args.quiet {
        args.project = prompt_optional("Project name", args.project)?;
        args.author = prompt_optional("Author name(s)", args.author)?;
        args.version = prompt_optional("Version", args.version)?;
        args.source_dir = prompt_path("Source directory", args.source_dir)?;
        args.build_dir = prompt_path("Build directory", args.build_dir)?;
        args.master_doc = prompt_optional("Master document", args.master_doc)?;
        args.source_suffix = prompt_optional("Source suffix", args.source_suffix)?;
        args.extensions = prompt_optional("Extensions (comma-separated)", args.extensions)?;
    }

    let options = QuickstartOptions::from_args(args);
    options.validate()?;
    ensure_relative(&options.source_dir)?;
    ensure_relative(&options.build_dir)?;

    let root = normalize_root(&options.path)?;
    let source_root = root.join(&options.source_dir);
    let build_root = root.join(&options.build_dir);

    let master_file = source_root.join(format!(
        "{}{}",
        options.master_doc, options.source_suffix
    ));
    let files: Vec<(PathBuf, String)> = vec![
        (root.join("conf.py"), render_conf_py(&options)),
        (master_file, render_master_doc(&options)),
        (root.join("Makefile"), render_makefile(&options)),
        (root.join("make.bat"), render_make_bat(&options)),
        (root.join(".gitignore"), render_gitignore(&options)),
    ];

    // Pre-flight: never leave a half-generated project when a file already
    // exists and --force was not requested.
    if !options.force {
        for (path, _) in &files {
            if path.try_exists().map_err(|error| QuickstartError::Io {
                path: path.clone(),
                message: error.to_string(),
            })? {
                return Err(miette::Report::new(QuickstartError::AlreadyExists {
                    path: path.clone(),
                }));
            }
        }
    }

    fs::create_dir_all(&source_root).map_err(|error| QuickstartError::Io {
        path: source_root.clone(),
        message: error.to_string(),
    })?;
    fs::create_dir_all(&build_root).map_err(|error| QuickstartError::Io {
        path: build_root.clone(),
        message: error.to_string(),
    })?;
    let mut created = vec![source_root, build_root];

    for (path, content) in &files {
        fs::write(path, content).map_err(|error| QuickstartError::Io {
            path: path.clone(),
            message: error.to_string(),
        })?;
        created.push(path.clone());
    }

    if !options.quiet {
        println!("scribe: created project `{}`", root.display());
        println!("scribe:   source : {}", root.join(&options.source_dir).display());
        println!("scribe:   build  : {}", root.join(&options.build_dir).display());
        for (path, _) in &files {
            println!("scribe:   wrote  {}", path.display());
        }
    }

    Ok(QuickstartReport {
        root,
        created,
        force: options.force,
    })
}

fn prompt_optional(label: &str, current: Option<String>) -> Result<Option<String>> {
    if current.is_some() {
        return Ok(current);
    }
    print!("{label}: ");
    io::stdout()
        .flush()
        .map_err(|error| QuickstartError::Input(error.to_string()))?;
    let mut answer = String::new();
    io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(|error| QuickstartError::Input(error.to_string()))?;
    let trimmed = answer.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_owned()))
    }
}

fn prompt_path(label: &str, current: Option<PathBuf>) -> Result<Option<PathBuf>> {
    let as_string = current.map(|path| path.to_string_lossy().into_owned());
    Ok(prompt_optional(label, as_string)?.map(PathBuf::from))
}

fn normalize_root(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(miette::Report::new(QuickstartError::UnsafePath {
            path: path.to_path_buf(),
            reason: "project path must not be empty".to_owned(),
        }));
    }
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    Ok(std::env::current_dir()
        .map_err(|error| QuickstartError::Io {
            path: PathBuf::from("."),
            message: error.to_string(),
        })?
        .join(path))
}

fn ensure_relative(path: &Path) -> Result<()> {
    let mut stack: Vec<OsString> = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => {
                return Err(miette::Report::new(QuickstartError::UnsafePath {
                    path: path.to_path_buf(),
                    reason: "absolute directories are not allowed".to_owned(),
                }))
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if stack.pop().is_none() {
                    return Err(miette::Report::new(QuickstartError::UnsafePath {
                        path: path.to_path_buf(),
                        reason: "directory escapes the project root".to_owned(),
                    }));
                }
            }
            Component::Normal(part) => stack.push(part.to_os_string()),
        }
    }
    if stack.is_empty() {
        return Err(miette::Report::new(QuickstartError::UnsafePath {
            path: path.to_path_buf(),
            reason: "directory must not be the project root".to_owned(),
        }));
    }
    Ok(())
}

fn python_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn render_conf_py(options: &QuickstartOptions) -> String {
    let mut extensions = options.extensions.clone();
    extensions.sort();
    extensions.dedup();
    let extension_lines = extensions
        .iter()
        .map(|name| format!("    \"{}\",\n", python_string(name)))
        .collect::<String>();
    let minor_version = options
        .version
        .split('.')
        .take(2)
        .collect::<Vec<_>>()
        .join(".");
    format!(
        "# Configuration file for the Sphinx documentation builder.\n\
         #\n\
         # Generated by `scribe quickstart`.\n\
         \n\
         import os\n\
         import sys\n\
         sys.path.insert(0, os.path.abspath(\".\"))\n\
         \n\
         # -- Project information ---------------------------------------------\n\
         \n\
         project = \"{}\"\n\
         author = \"{}\"\n\
         release = \"{}\"\n\
         version = \"{}\"\n\
         \n\
         # -- General configuration -------------------------------------------\n\
         \n\
         extensions = [\n\
         {}]\n\
         \n\
         templates_path = [\"_templates\"]\n\
         exclude_patterns = []\n\
         master_doc = \"{}\"\n\
         source_suffix = \"{}\"\n\
         \n\
         # -- Options for HTML output -----------------------------------------\n\
         \n\
         html_theme = \"alabaster\"\n\
         html_static_path = [\"_static\"]\n",
        python_string(&options.project),
        python_string(&options.author),
        python_string(&options.version),
        python_string(&minor_version),
        extension_lines,
        python_string(&options.master_doc),
        python_string(&options.source_suffix),
    )
}

fn render_master_doc(options: &QuickstartOptions) -> String {
    let title = &options.project;
    let underline = "=".repeat(title.chars().count().max(3));
    format!(
        "{title}\n\
         {underline}\n\
         \n\
         .. toctree::\n\
            :maxdepth: 2\n\
            :caption: Contents:\n\
         \n\
         \n\
         Indices and tables\n\
         ==================\n\
         \n\
         * :ref:`genindex`\n\
         * :ref:`modindex`\n\
         * :ref:`search`\n"
    )
}

fn render_makefile(options: &QuickstartOptions) -> String {
    let sourcedir = options.source_dir.to_string_lossy();
    let builddir = options.build_dir.to_string_lossy();
    format!(
        "# Minimal makefile for Sphinx documentation\n\
         \n\
         SPHINXOPTS    ?=\n\
         SPHINXBUILD   ?= sphinx-build\n\
         SOURCEDIR     = {sourcedir}\n\
         BUILDDIR      = {builddir}\n\
         \n\
         .PHONY: help Makefile\n\
         \n\
         help:\n\
         \t@$(SPHINXBUILD) -M help \"$(SOURCEDIR)\" \"$(BUILDDIR)\" $(SPHINXOPTS) $(O)\n\
         \n\
         %: Makefile\n\
         \t@$(SPHINXBUILD) -M $@ \"$(SOURCEDIR)\" \"$(BUILDDIR)\" $(SPHINXOPTS) $(O)\n"
    )
}

fn render_make_bat(options: &QuickstartOptions) -> String {
    let sourcedir = options.source_dir.to_string_lossy();
    let builddir = options.build_dir.to_string_lossy();
    format!(
        "@ECHO OFF\n\
         \n\
         pushd %~dp0\n\
         \n\
         REM Command file for Sphinx documentation\n\
         \n\
         if \"%SPHINXBUILD%\" == \"\" (\n\
         \tset SPHINXBUILD=sphinx-build\n\
         )\n\
         set SOURCEDIR={sourcedir}\n\
         set BUILDDIR={builddir}\n\
         \n\
         if \"%1\" == \"help\" goto help\n\
         \n\
         %SPHINXBUILD% -M %1 %SOURCEDIR% %BUILDDIR% %SPHINXOPTS% %O%\n\
         goto end\n\
         \n\
         :help\n\
         %SPHINXBUILD% -M help %SOURCEDIR% %BUILDDIR% %SPHINXOPTS% %O%\n\
         \n\
         :end\n\
         popd\n"
    )
}

fn render_gitignore(options: &QuickstartOptions) -> String {
    let builddir = options.build_dir.to_string_lossy();
    let mut lines = BTreeSet::new();
    lines.insert(format!("/{builddir}/"));
    lines.insert("*.pyc".to_owned());
    let mut text = String::new();
    for line in lines {
        text.push_str(&line);
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::{render_conf_py, render_gitignore, render_master_doc};
    use crate::options::QuickstartOptions;
    use std::path::PathBuf;

    fn options() -> QuickstartOptions {
        QuickstartOptions {
            path: PathBuf::from("demo"),
            batch: true,
            quiet: true,
            force: false,
            project: "Demo".to_owned(),
            author: "Ada".to_owned(),
            version: "1.2.3".to_owned(),
            source_dir: PathBuf::from("source"),
            build_dir: PathBuf::from("build"),
            master_doc: "index".to_owned(),
            source_suffix: ".rst".to_owned(),
            extensions: vec!["sphinx.ext.todo".to_owned()],
        }
    }

    #[test]
    fn generated_conf_py_contains_core_values() {
        let conf = render_conf_py(&options());
        assert!(conf.contains("project = \"Demo\""));
        assert!(conf.contains("author = \"Ada\""));
        assert!(conf.contains("release = \"1.2.3\""));
        assert!(conf.contains("version = \"1.2\""));
        assert!(conf.contains("\"sphinx.ext.todo\""));
        assert!(conf.contains("source_suffix = \".rst\""));
    }

    #[test]
    fn generated_master_doc_and_gitignore_are_sane() {
        let doc = render_master_doc(&options());
        assert!(doc.starts_with("Demo\n====\n"));
        assert!(doc.contains(".. toctree::"));
        let ignore = render_gitignore(&options());
        assert!(ignore.contains("/build/"));
    }
}
