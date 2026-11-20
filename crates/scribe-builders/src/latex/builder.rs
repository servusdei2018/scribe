//! LaTeX builder and optional PDF invocation (Phase 23).
//!
//! [`LatexBuilder::build`] runs a pipeline parallel to the HTML builder:
//!
//! 1. discover sources through `BuildEnvironment::update`,
//! 2. parse and resolve every source, register domain objects and apply the
//!    `Phase::Read` / `Phase::Resolved` transforms so cross-references resolve,
//! 3. render each `latex_documents` entry with [`super::LaTeXTranslator`],
//! 4. write deterministic `.tex` files plus `Makefile`, `make.bat`,
//!    `latexmkrc` and the vendored Sphinx support files,
//! 5. optionally invoke `latexmk` to compile the `.tex` into a PDF.
//!
//! PDF compilation is *best effort*: when `latexmk` is unavailable the builder
//! still emits valid `.tex` files and records a non-fatal warning — it never
//! panics. The `pdf` builder behaves like `latex` but always compiles.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

use scribe_ast::{Document, NodeKind};
use scribe_config::{Config, ConfigValue, Tags};
use scribe_core::{
    apply_transforms, extensions, ActivatedExtensions, BuildEnvironment, Phase, TransformContext,
    UpdateOptions,
};
use scribe_domains::{prepare_document, DomainRegistry};
use scribe_rst::{
    parse_with_options, resolve_output, Context as RstContext, DirectiveRegistry, ParseOptions,
};

use super::escape::LatexEngine;
use super::support::SUPPORT_FILES;
use super::translator::{LatexContext, LaTeXTranslator};
use super::{BuildOptions, BuildResult, LatexError, Result};

use crate::build_common::domain_warning_line;
use crate::register_code_directives;

/// The LaTeX builder.
#[derive(Debug, Default)]
pub struct LatexBuilder;

impl LatexBuilder {
    /// Create a builder.
    #[must_use]
    pub fn new() -> Self { Self }

    /// Run a complete LaTeX build into `options.output_dir`.
    ///
    /// When `options.builder_name == "pdf"` the generated `.tex` files are
    /// compiled with `latexmk`; otherwise PDF generation is controlled by the
    /// `latex_use_pdf` config key (default `false`).
    pub fn build(
        options: &BuildOptions,
        config: &Config,
        env: &mut BuildEnvironment,
    ) -> Result<BuildResult> {
        build_impl(options, config, env)
    }

    /// Compile a single `.tex` file to a PDF using `latexmk`.
    ///
    /// Returns the path of the generated PDF on success. Missing toolchains
    /// are reported as `LatexError::ToolchainUnavailable` so callers can
    /// degrade gracefully.
    pub fn compile_pdf(tex_path: &Path, engine: LatexEngine) -> Result<PathBuf> {
        run_latexmk(tex_path, engine)
    }
}

/// One parsed document awaiting rendering.
struct ParsedDoc {
    document: Document,
}

/// A `latex_documents` entry.
#[derive(Clone, Debug)]
struct LatexDocSpec {
    docname: String,
    filename: String,
    title: String,
    author: String,
    theme: String,
}

fn build_impl(
    options: &BuildOptions,
    config: &Config,
    env: &mut BuildEnvironment,
) -> Result<BuildResult> {
    let source_dir = &options.source_dir;
    let outdir = &options.output_dir;
    if !source_dir.is_dir() {
        return Err(LatexError::MissingSource(source_dir.clone()));
    }
    fs::create_dir_all(outdir).map_err(|e| LatexError::io(outdir, e))?;

    let update_options = UpdateOptions {
        freshenv: options.freshenv,
        write_all: options.force_all,
        cache_dir: Some(options.doctree_dir.clone().unwrap_or_else(|| outdir.join(".doctrees"))),
        persist: true,
        ..UpdateOptions::default()
    };
    let report = env.update(config, source_dir, &update_options)?;
    let mut warnings: Vec<String> = report.warnings.clone();
    warnings.extend(crate::i18n::compile_all(config, source_dir));

    // ---- select documents -------------------------------------------------
    let mut docnames: Vec<String> = if options.filenames.is_empty() {
        env.found_docnames().iter().map(|s| (*s).to_owned()).collect()
    } else {
        let mut selected = std::collections::BTreeSet::new();
        for file in &options.filenames {
            if let Some(docname) = env.path2doc(file) {
                selected.insert(docname.to_owned());
            } else {
                let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
                if env.found_docs.contains_key(stem) {
                    selected.insert(stem.to_owned());
                }
            }
        }
        selected.into_iter().collect()
    };
    docnames.retain(|name| env.found_docs.contains_key(name));
    if docnames.is_empty() {
        docnames = env.found_docnames().iter().map(|s| (*s).to_owned()).collect();
    }

    // ---- parse + resolve --------------------------------------------------
    let extensions = ActivatedExtensions::from_config(config);
    warnings.extend(extensions.warnings.iter().cloned());
    let (extlink_roles, extlink_warnings) = extensions::extlink_roles(config);
    warnings.extend(extlink_warnings);
    let config_arc = Arc::new(config.clone());
    let tags = Tags::with_tags(std::iter::empty::<String>());
    let registry = DomainRegistry::builtin();
    registry.load_from_env(env);
    let inv_cache_dir = options.doctree_dir.clone().unwrap_or_else(|| outdir.join(".doctrees"));
    warnings.extend(crate::build_common::attach_intersphinx(
        &registry,
        config,
        source_dir,
        Some(&inv_cache_dir),
    ));
    let relevant = env.relevant_config();
    let rst_prolog = relevant.rst_prolog.clone();
    let rst_epilog = relevant.rst_epilog.clone();

    let mut parsed: BTreeMap<String, ParsedDoc> = BTreeMap::new();
    for docname in &docnames {
        let Some(path) = env.found_docs.get(docname) else { continue };
        let Ok(raw) = fs::read_to_string(path) else {
            warnings.push(format!("cannot read source for {docname}"));
            continue;
        };
        let started = Instant::now();
        let mut parse_options = ParseOptions::new(Some(docname.clone()));
        parse_options.extlinks = Arc::clone(&extlink_roles);
        if !rst_prolog.is_empty() {
            parse_options.prolog = Some(rst_prolog.clone());
        }
        if !rst_epilog.is_empty() {
            parse_options.epilog = Some(rst_epilog.clone());
        }
        let mut output = parse_with_options(&raw, parse_options);
        let mut rst_ctx = RstContext::new(source_dir, Tags::with_tags(std::iter::empty::<String>()));
        rst_ctx.config = Some(Arc::clone(&config_arc));
        rst_ctx.source_name = Some(docname.clone());
        rst_ctx.extlinks = Arc::clone(&extlink_roles);
        let mut directive_registry = DirectiveRegistry::builtin();
        register_code_directives(&mut directive_registry);
        extensions::register_directives(&mut directive_registry, &extensions);
        let resolve_report = resolve_output(&mut output, &directive_registry, &rst_ctx);
        for issue in &resolve_report.unresolved {
            warnings.push(issue.warning_message(docname));
        }
        let _ = started; // timing intentionally not embedded (determinism)

        output.document.metadata.docname = Some(docname.clone());
        if output.document.metadata.title.is_none() {
            output.document.metadata.title = extract_title(&output.document);
        }
        parsed.insert(docname.clone(), ParsedDoc { document: output.document });
    }

    // ---- domain registration + transforms --------------------------------
    // Expand signatures before registering objects so `py:fullname` /
    // `rst:name` and friends exist when the object table is built.
    for outcome in parsed.values_mut() {
        if let Err(error) = prepare_document(&mut outcome.document) {
            warnings.push(format!("domain signature expansion failed: {error}"));
        }
    }
    {
        let transform_ctx = TransformContext::new(Some(config), Some(&tags), "latex", "latex")
            .with_domains(&registry)
            .with_env(env);
        for (docname, outcome) in &parsed {
            let domain_ctx = transform_ctx.domain_context(docname);
            if let Err(error) = registry.process_document(&domain_ctx, docname, &outcome.document) {
                warnings.push(format!("{docname}: domain registration failed: {error}"));
            }
        }
    }
    let compact = crate::i18n::compact_setting(config);
    for (docname, outcome) in &mut parsed {
        let domain = crate::i18n::docname_to_domain(docname, &compact);
        let translations = crate::i18n::load_catalog(config, source_dir, &domain);
        let mut transform_ctx = TransformContext::new(Some(config), Some(&tags), "latex", "latex")
            .with_domains(&registry)
            .with_env(env);
        if let Some(catalog) = &translations {
            transform_ctx = transform_ctx.with_translations(catalog);
        }
        let read_report = apply_transforms(&mut outcome.document, &transform_ctx, Phase::Read);
        let resolved_report = apply_transforms(&mut outcome.document, &transform_ctx, Phase::Resolved);
        for diagnostic in read_report
            .domain_diagnostics()
            .iter()
            .chain(resolved_report.domain_diagnostics().iter())
        {
            warnings.push(domain_warning_line(diagnostic, docname, env));
        }
        for diagnostic in read_report.diagnostics.iter().chain(resolved_report.diagnostics.iter()) {
            warnings.push(format!("{docname}: {}", diagnostic.message));
        }
    }

    // ---- copy referenced images ------------------------------------------
    let mut images = std::collections::BTreeSet::new();
    for outcome in parsed.values() {
        for id in outcome.document.preorder_ids() {
            if let Ok(node) = outcome.document.get(id) {
                if node.kind == NodeKind::Image {
                    if let Some(uri) = node.get_str("uri").ok().flatten() {
                        if !uri.is_empty() && !uri.contains("://") { images.insert(uri.to_owned()); }
                    }
                }
            }
        }
    }
    for uri in &images {
        let source = source_dir.join(uri);
        let target = outdir.join(uri);
        if source.is_file() {
            if let Some(parent) = target.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Err(error) = fs::copy(&source, &target) {
                warnings.push(format!("could not copy image {uri}: {error}"));
            }
        } else {
            warnings.push(format!("image not found: {uri}"));
        }
    }

    // ---- render each latex_documents entry --------------------------------
    let specs = latex_document_specs(config);
    let engine = LatexEngine::from_name(config.get_str("latex_engine").unwrap_or("pdflatex"));
    let mut written = Vec::new();
    let mut built_docnames = Vec::new();
    let mut generated_tex: Vec<PathBuf> = Vec::new();

    // Render in sorted order for deterministic output.
    let mut specs = specs;
    specs.sort_by(|a, b| a.filename.cmp(&b.filename));
    for spec in &specs {
        let Some(outcome) = parsed.get(&spec.docname) else {
            warnings.push(format!("latex: start document '{}' not found", spec.docname));
            continue;
        };
        let document = &outcome.document;
        let title = if spec.title.is_empty() {
            document.metadata.title.clone().unwrap_or_else(|| config.get_str("project").unwrap_or("").to_owned())
        } else {
            spec.title.clone()
        };
        let ctx = LatexContext::from_config(config, &spec.docname, title, spec.author.clone(), &spec.theme);
        let mut body = LaTeXTranslator::translate_body(document, &ctx);
        // Concatenate the remaining documents behind the start document so a
        // single .tex contains the whole project (mirrors upstream's
        // `collect_toctree` behaviour). Sorted `BTreeMap` iteration keeps the
        // output deterministic.
        if specs.len() == 1 {
            for (docname, other) in &parsed {
                if docname == &spec.docname { continue; }
                body.push('\n');
                body.push_str(&LaTeXTranslator::translate_body(&other.document, &ctx));
            }
        }
        let tex = super::translator::render_document(&ctx, &body);
        let relative = PathBuf::from(&spec.filename);
        let path = outdir.join(&relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| LatexError::io(parent, e))?;
        }
        fs::write(&path, tex).map_err(|e| LatexError::io(&path, e))?;
        written.push(relative.clone());
        generated_tex.push(path);
        built_docnames.push(spec.docname.clone());
    }

    // ---- ancillary files --------------------------------------------------
    write_support_files(outdir)?;
    let makefile = render_makefile(engine);
    fs::write(outdir.join("Makefile"), &makefile).map_err(|e| LatexError::io(&outdir.join("Makefile"), e))?;
    written.push(PathBuf::from("Makefile"));
    let makebat = render_makebat();
    fs::write(outdir.join("make.bat"), &makebat).map_err(|e| LatexError::io(&outdir.join("make.bat"), e))?;
    written.push(PathBuf::from("make.bat"));
    let latexmkrc = render_latexmkrc(engine);
    fs::write(outdir.join("latexmkrc"), &latexmkrc).map_err(|e| LatexError::io(&outdir.join("latexmkrc"), e))?;
    written.push(PathBuf::from("latexmkrc"));

    // ---- optional PDF compilation ----------------------------------------
    let want_pdf = options.builder_name == "pdf" || config.get_bool("latex_use_pdf").unwrap_or(false);
    if want_pdf {
        let mut available = true;
        if Command::new("latexmk").arg("-version").output().is_err() {
            available = false;
        }
        if available {
            for tex in &generated_tex {
                match run_latexmk(tex, engine) {
                    Ok(pdf) => {
                        if let Ok(rel) = pdf.strip_prefix(outdir) {
                            written.push(rel.to_path_buf());
                        }
                    }
                    Err(error) => warnings.push(format!("pdf compilation failed: {error}")),
                }
            }
        } else {
            warnings.push("latexmk not available; emitted .tex without compiling PDF".to_owned());
        }
    }

    Ok(BuildResult { written, built_docnames, warnings })
}

fn extract_title(doc: &Document) -> Option<String> {
    doc.preorder_ids().into_iter().find_map(|id| {
        let node = doc.get(id).ok()?;
        if node.kind == NodeKind::Title { Some(node.astext(doc)) } else { None }
    })
}

fn latex_document_specs(config: &Config) -> Vec<LatexDocSpec> {
    let Some(list) = config.get_list("latex_documents") else {
        return vec![default_spec(config)];
    };
    let mut specs = Vec::new();
    for entry in list {
        let Some(fields) = entry.get_list() else { continue };
        let get = |i: usize| fields.get(i).and_then(ConfigValue::get_str).unwrap_or("").to_owned();
        if get(0).is_empty() { continue; }
        specs.push(LatexDocSpec {
            docname: get(0),
            filename: if get(1).is_empty() { format!("{}.tex", get(0)) } else { get(1) },
            title: get(2),
            author: get(3),
            theme: if get(4).is_empty() { "manual".to_owned() } else { get(4) },
        });
    }
    if specs.is_empty() { vec![default_spec(config)] } else { specs }
}

fn default_spec(config: &Config) -> LatexDocSpec {
    LatexDocSpec {
        docname: config.master_doc().to_owned(),
        filename: "projectnamenotset.tex".to_owned(),
        title: config.get_str("project").unwrap_or("Project name not set").to_owned(),
        author: config.get_str("author").unwrap_or("Author name not set").to_owned(),
        theme: config.get_str("latex_theme").unwrap_or("manual").to_owned(),
    }
}

fn write_support_files(outdir: &Path) -> Result<()> {
    for (name, contents) in SUPPORT_FILES {
        let path = outdir.join(name);
        fs::write(&path, contents).map_err(|e| LatexError::io(&path, e))?;
    }
    Ok(())
}

fn render_makefile(engine: LatexEngine) -> String {
    let all_dvi = if engine.is_unicode() { "" } else { "ALLDVI = $(addsuffix .dvi,$(ALLDOCS))\n" };
    format!(
        "# Makefile for scribe LaTeX output\n\
         ALLDOCS = $(basename $(wildcard *.tex))\n\
         ALLPDF = $(addsuffix .pdf,$(ALLDOCS))\n\
         {all_dvi}\
         LATEXMKOPTS ?=\n\
         PDFLATEX = latexmk -pdf -dvi- -ps-\n\n\
         %.pdf: %.tex\n\
         \t$(PDFLATEX) $(LATEXMKOPTS) '$<'\n\n\
         all: $(ALLPDF)\n\
         all-pdf: $(ALLPDF)\n\n\
         clean:\n\
         \trm -f *.log *.aux *.toc *.idx *.ind *.out *.pdf *.fdb_latexmk *.fls\n\n\
         .PHONY: all all-pdf clean\n"
    )
}

fn render_makebat() -> String {
    "@ECHO OFF\n\
     pushd %~dp0\n\
     latexmk -pdf %1\n\
     popd\n"
        .to_owned()
}

fn render_latexmkrc(engine: LatexEngine) -> String {
    let pdflatex = match engine {
        LatexEngine::PdfLatex => "pdflatex",
        LatexEngine::XeLatex => "xelatex",
        LatexEngine::LuaLatex => "lualatex",
    };
    format!(
        "$pdflatex = '{pdflatex} %O %S';\n\
         $makeindex = 'makeindex -s python.ist %O -o %D %S';\n"
    )
}

fn run_latexmk(tex_path: &Path, engine: LatexEngine) -> Result<PathBuf> {
    let dir = tex_path.parent().unwrap_or_else(|| Path::new("."));
    let file = tex_path.file_name().and_then(|s| s.to_str())
        .ok_or_else(|| LatexError::Invalid("invalid TeX file name".to_owned()))?;
    let flag = match engine {
        LatexEngine::PdfLatex => "-pdf",
        LatexEngine::XeLatex => "-xelatex",
        LatexEngine::LuaLatex => "-lualatex",
    };
    let output = Command::new("latexmk")
        .arg(flag)
        .arg("-interaction=nonstopmode")
        .arg("-halt-on-error")
        .arg(file)
        .current_dir(dir)
        .output()
        .map_err(|e| LatexError::toolchain("latexmk", e))?;
    let pdf = dir.join(Path::new(file).with_extension("pdf"));
    if output.status.success() && pdf.is_file() {
        return Ok(pdf);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = if stderr.trim().is_empty() { stdout } else { stderr };
    Err(LatexError::Compilation {
        path: tex_path.to_path_buf(),
        detail: detail.chars().rev().take(1500).collect::<String>().chars().rev().collect(),
    })
}
