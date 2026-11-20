//! Standalone HTML builder (Phase 17).
//!
//! [`StandaloneHtmlBuilder`] runs the full pipeline:
//!
//! 1. discover sources and compute the rebuild set (Phase 08 `BuildEnvironment`),
//! 2. parse every source and resolve directives/substitutions (Phases 04–06),
//! 3. register domain objects for every document, then apply the Phase 10
//!    read/resolved transform pipelines with the Phase 14 domain engine so all
//!    cross-references are resolved against the complete inventory,
//! 4. render each body with [`HTML5Translator`], wrap it in the Phase 16 theme
//!    templates, and write deterministic bytes,
//! 5. copy static assets, images and source dumps; emit `genindex.html`,
//!    `py-modindex.html`, a placeholder `search.html`, `objects.inv` and
//!    `.buildinfo`.
//!
//! Pages are rendered in parallel with `rayon`; all outputs are collected,
//! sorted by docname and written sequentially so bytes are deterministic.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use rayon::prelude::*;
use scribe_ast::{Document, NodeKind};
use scribe_config::{Config, Tags};
use scribe_core::{
    apply_transforms, extensions::ifconfig, ActivatedExtensions, BuildEnvironment, DurationReport,
    Event, GithubPagesFiles, HandlerResult, InteropBridge, Phase, SharedJson,
    TodoCollector, TransformContext, UpdateOptions, ViewcodeCollector,
};
use scribe_core::extensions::graphviz::GraphvizRender;
use scribe_core::extensions::{register_directives, GraphvizHtmlInfo};
use scribe_domains::{prepare_document, DomainRegistry, DumpConfig, Inventory};
use scribe_rst::{
    parse_with_options, resolve_output, Context as RstContext,
    DirectiveRegistry, ParseOptions,
};
use crate::build_common::domain_warning_line;
use crate::register_code_directives;
use scribe_theme::{RenderContext, ThemeError, ThemeManager};

use crate::error::HtmlError;
use crate::error::Result;
use crate::profile;
use crate::highlight::{highlight_code, HighlightOptions};
use crate::pygments_css::pygments_css;
use crate::search::{scribe_envversion, SearchIndex, BASE_STEMMER_JS, ENGLISH_STEMMER_JS};

use super::assets::{
    copy_extra_static, copy_files_parallel, copy_images, copy_theme_assets,
    render_asset_templates, write_buildinfo, write_source_dumps,
};
use super::index::write_index_pages;
use super::translator::{escape_attr, escape_html, render_local_toc, HtmlContext, SingleFileContext, HTML5Translator};
use super::{BuildOptions, BuildResult};

/// The standalone HTML builder.
#[derive(Debug, Default)]
pub struct StandaloneHtmlBuilder;

impl StandaloneHtmlBuilder {
    /// Create a builder.
    #[must_use]
    pub fn new() -> Self { Self }

    /// Run a complete HTML build into `options.output_dir`.
    pub fn build(
        options: &BuildOptions,
        config: &Config,
        env: &mut BuildEnvironment,
    ) -> Result<BuildResult> {
        build_with_flavor(options, config, env, &BuildFlavor::standalone())
    }
}

/// Knobs that distinguish the HTML builder family.
///
/// Phase 19's [`crate::dirhtml::DirectoryHTMLBuilder`] and the metadata builders
/// in [`crate::metadata`] subclass the standalone builder upstream. They reuse
/// the exact same pipeline; only these small behavioural differences vary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuildFlavor {
    /// Builder name recorded in the theme context and diagnostics.
    pub name: &'static str,
    /// Emit directory-style (`foo/`, `index` → `''`) target URIs and output
    /// paths (`foo/index.html`), as upstream's `DirectoryHTMLBuilder` does.
    pub directory_style: bool,
    /// Output file suffix (`.html` for HTML-family builders, `.fjson` for json).
    pub out_suffix: &'static str,
    /// Copy reST sources into `_sources/` for "show source" links.
    pub copy_source: bool,
    /// Generate the client-side search index and search page.
    pub search: bool,
    /// "Embedded" builders (htmlhelp/qthelp/devhelp) drop permalinks,
    /// sidebars and relation links.
    pub embedded: bool,
    /// Serialize page contexts to JSON instead of rendering HTML templates.
    pub serializing: bool,
    /// Inline every toctree document into one page (singlehtml).
    pub singlefile: bool,
}

impl BuildFlavor {
    /// Plain `html` builder.
    #[must_use]
    pub const fn standalone() -> Self {
        Self { name: "html", directory_style: false, out_suffix: ".html", copy_source: true, search: true, embedded: false, serializing: false, singlefile: false }
    }

    /// `dirhtml` builder (clean URLs, `index.html` per directory).
    #[must_use]
    pub const fn dirhtml() -> Self {
        Self { name: "dirhtml", directory_style: true, out_suffix: ".html", copy_source: true, search: true, embedded: false, serializing: false, singlefile: false }
    }

    /// `json` (serializinghtml) builder.
    #[must_use]
    pub const fn json() -> Self {
        Self { name: "json", directory_style: true, out_suffix: ".fjson", copy_source: true, search: true, embedded: false, serializing: true, singlefile: false }
    }

    /// `htmlhelp` / `qthelp` / `devhelp` metadata builders.
    #[must_use]
    pub const fn metadata(name: &'static str) -> Self {
        Self { name, directory_style: false, out_suffix: ".html", copy_source: false, search: false, embedded: true, serializing: false, singlefile: false }
    }

    /// `singlehtml` builder: one page, all toctree documents inlined.
    #[must_use]
    pub const fn singlefile() -> Self {
        Self { name: "singlehtml", directory_style: false, out_suffix: ".html", copy_source: false, search: false, embedded: true, serializing: false, singlefile: true }
    }

    /// Target URI of `docname` under this flavor.
    #[must_use]
    pub fn target_uri(&self, docname: &str) -> String {
        if self.directory_style {
            crate::dirhtml::target_uri(docname)
        } else {
            format!("{docname}{}", self.out_suffix)
        }
    }

    /// Output path (relative to the build directory) of `docname`.
    #[must_use]
    pub fn output_path(&self, docname: &str) -> PathBuf {
        if self.directory_style {
            crate::dirhtml::output_path(docname, self.out_suffix)
        } else {
            PathBuf::from(format!("{docname}{}", self.out_suffix))
        }
    }
}

/// Whether a theme-resolution failure means the requested theme is genuinely
/// absent (as opposed to present but misconfigured). Only the former may fall
/// back to `basic`; the latter must remain a hard error so a broken theme is
/// never silently ignored.
fn theme_is_genuinely_missing(error: &ThemeError, requested: &str) -> bool {
    match error {
        ThemeError::UnknownTheme { .. } => true,
        ThemeError::MissingParent { name, parent, .. } => {
            // `get_theme` reports `name == parent == requested` when the very
            // first theme in the chain is absent. A missing *ancestor* has a
            // different parent name and indicates a misconfigured present theme.
            name == parent && parent == requested
        }
        _ => false,
    }
}

/// Run the shared HTML-family pipeline with an explicit flavor.
pub fn build_with_flavor(
    options: &BuildOptions,
    config: &Config,
    env: &mut BuildEnvironment,
    flavor: &BuildFlavor,
) -> Result<BuildResult> {
        let source_dir = &options.source_dir;
        let outdir = &options.output_dir;
        if !source_dir.is_dir() {
            return Err(HtmlError::MissingSource(source_dir.clone()));
        }
        fs::create_dir_all(outdir).map_err(|e| HtmlError::io(outdir, e))?;

        // ---- Phase 27 third-party interop bridge --------------------------
        // A bridge supplied by the CLI carries third-party handlers/directives;
        // otherwise an inert one is created so events still fire (harmlessly).
        let bridge = options
            .bridge
            .clone()
            .unwrap_or_else(|| Arc::new(InteropBridge::new()));
        let events = &bridge.events;
        let _ = events.emit("config-inited", &Event::ConfigInited {
            config: Arc::new(config_json(config)),
        });
        let _ = events.emit("builder-inited", &Event::BuilderInited {
            builder_name: flavor.name.to_owned(),
        });

        // ---- environment / discovery -------------------------------------
        let update_options = UpdateOptions {
            freshenv: options.freshenv,
            write_all: options.force_all,
            cache_dir: Some(options.doctree_dir.clone().unwrap_or_else(|| outdir.join(".doctrees"))),
            persist: true,
            ..UpdateOptions::default()
        };
        let report = env
            .update(config, source_dir, &update_options)
            .map_err(HtmlError::Environment)?;
        profile::mark("discovery");
        let mut warnings: Vec<String> = report.warnings.clone();

        // Upstream's `env-get-outdated` / `env-get-updated` hooks run while the
        // environment decides what to re-read. scribe computes the set inside
        // `env.update`, so the events are emitted immediately afterwards with
        // the same added/changed/removed sets and any returned docnames are
        // added to the rebuild set.
        let outdated_event = Event::EnvGetOutdated {
            added: report.added.clone(),
            changed: report.changed.clone(),
            removed: report.removed.clone(),
        };
        let mut hook_docnames = docnames_from_results(&events.emit("env-get-outdated", &outdated_event));
        let updated_results = events.emit("env-get-updated", &Event::EnvGetUpdated);
        hook_docnames.extend(docnames_from_results(&updated_results));
        let _ = events.emit("env-updated", &Event::EnvUpdated);
        warnings.extend(hook_docnames.iter().map(|name| format!("env hook requested rebuild of `{name}`")));

        // Phase 26: compile any changed .po catalogs before documents are
        // transformed so the Locale transform can load their .mo files.
        warnings.extend(crate::i18n::compile_all(config, source_dir));

        // ---- Phase 22 autosummary pre-read stub generation ----------------
        // Upstream's `autosummary_generate` runs on `builder-inited`, before
        // source discovery, so generated stubs are part of `env.found_docs`
        // in the same build. We regenerate before parsing (and re-run discovery
        // when new files appeared) so the directive's toctree resolves in the
        // same build. Guarded by extension activation + the config key.
        let extensions_early = ActivatedExtensions::from_config(config);
        if config.get_bool("autosummary_generate").unwrap_or(true)
            && extensions_early.contains("autosummary")
        {
            let stub_report = scribe_autodoc::generate_stubs(config, source_dir, &[]);
            warnings.extend(stub_report.warnings);
            if !stub_report.generated.is_empty() {
                let updated = env
                    .update(config, source_dir, &update_options)
                    .map_err(HtmlError::Environment)?;
                warnings.extend(updated.warnings);
            }
        }

        let mut docnames: Vec<String> = if options.filenames.is_empty() {
            env.found_docnames().iter().map(|s| (*s).to_owned()).collect()
        } else {
            let mut selected = BTreeSet::new();
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
        for name in &hook_docnames {
            if env.found_docs.contains_key(name) && !docnames.contains(name) {
                docnames.push(name.clone());
            }
        }
        docnames.sort();
        if docnames.is_empty() {
            docnames = env.found_docnames().iter().map(|s| (*s).to_owned()).collect();
        }

        // ---- builders / registries ----------------------------------------
        let mut theme_name = config.get_str("html_theme").unwrap_or("alabaster").to_owned();
        let manager = ThemeManager::with_config(config, Some(source_dir));
        // Some upstream fixtures use `{% extends "!layout.html" %}`, whose
        // leading `!` tells Jinja to skip user template dirs. minijinja does not
        // implement that prefix yet, so keep two fallbacks: a bundled-only one
        // for page rendering, and a theme-aware one (without user template
        // dirs) for generated index pages so the project's selected theme is
        // never silently replaced by the bundled default.
        let fallback_manager = ThemeManager::new();
        let theme_fallback_manager = ThemeManager::with_config_no_user_templates(config, Some(source_dir));
        let theme = match manager.get_theme(&theme_name) {
            Ok(theme) => theme,
            Err(error) if theme_is_genuinely_missing(&error, &theme_name) => {
                // Only a theme that is absent everywhere falls back, and it
                // does so loudly. A theme that exists but is misconfigured
                // (bad TOML, cycle, missing parent) stays a hard error.
                warnings.push(format!(
                    "theme '{theme_name}' was not found in any theme path or site-packages; falling back to 'basic': {error}"
                ));
                theme_name = "basic".to_owned();
                manager.get_theme("basic").map_err(HtmlError::theme)?
            }
            Err(error) => return Err(HtmlError::theme(error)),
        };
        let registry = DomainRegistry::builtin();
        registry.load_from_env(env);
        // Load intersphinx inventories before any xref is resolved.  The
        // resolver is attached to the registry, so the frozen resolve seams
        // need no signature changes.
        let inv_cache_dir = options.doctree_dir.clone().unwrap_or_else(|| outdir.join(".doctrees"));
        warnings.extend(crate::build_common::attach_intersphinx(
            &registry,
            config,
            source_dir,
            Some(&inv_cache_dir),
        ));

        let tag_names = options_filenames_tags(&options.filenames, flavor);
        let tags = Tags::with_tags(tag_names.iter().cloned());
        let config_arc = Arc::new(config.clone());
        let language = config.get_str("language").filter(|s| !s.is_empty()).unwrap_or("en").to_owned();
        let relevant = env.relevant_config();
        let rst_prolog = relevant.rst_prolog.clone();
        let rst_epilog = relevant.rst_epilog.clone();

        // ---- Phase 20 extension activation --------------------------------
        let extensions = ActivatedExtensions::from_config(config);
        warnings.extend(extensions.warnings.iter().cloned());
        let (extlink_roles, extlink_warnings) = scribe_core::extensions::extlink_roles(config);
        warnings.extend(extlink_warnings);
        let mut duration = DurationReport::default();
        let mut todos = TodoCollector::default();
        let mut viewcode = ViewcodeCollector::default();

        // ---- pass A: parse all documents ----------------------------------
        let mut parsed: BTreeMap<String, ParseOutcome> = BTreeMap::new();
        let mut source_paths: BTreeMap<String, (PathBuf, String)> = BTreeMap::new();
        for docname in &docnames {
            let Some(path) = env.found_docs.get(docname) else { continue };
            let Ok(raw) = fs::read_to_string(path) else {
                warnings.push(format!("cannot read source for {docname}"));
                continue;
            };
            // Upstream's `source-read(docname, source)` lets extensions mutate
            // the source text in place before RST parsing.
            let source = Arc::new(std::sync::Mutex::new(raw));
            let _ = events.emit("source-read", &Event::SourceRead {
                docname: docname.clone(),
                source: Arc::clone(&source),
            });
            let raw = source.lock().map(|guard| guard.clone()).unwrap_or_default();
            let suffix_by_path = suffix_for_source(config, path);
            source_paths.insert(docname.clone(), (path.clone(), suffix_by_path));

            let reading_started = Instant::now();
            let mut parse_options = ParseOptions::new(Some(docname.clone()));
            parse_options.extlinks = Arc::clone(&extlink_roles);
            if !rst_prolog.is_empty() {
                parse_options.prolog = Some(rst_prolog.clone());
            }
            if !rst_epilog.is_empty() {
                parse_options.epilog = Some(rst_epilog.clone());
            }
            let mut output = parse_with_options(&raw, parse_options);
            let mut rst_ctx = RstContext::new(source_dir, Tags::with_tags(tag_names.iter().cloned()));
            rst_ctx.config = Some(Arc::clone(&config_arc));
            rst_ctx.source_name = Some(docname.clone());
            rst_ctx.extlinks = Arc::clone(&extlink_roles);
            let mut directive_registry = DirectiveRegistry::builtin();
            register_code_directives(&mut directive_registry);
            register_directives(&mut directive_registry, &extensions);
            bridge.register_directives(&directive_registry);
            let resolve_report = resolve_output(&mut output, &directive_registry, &rst_ctx);
            for issue in &resolve_report.unresolved {
                warnings.push(issue.warning_message(docname));
                let _ = events.emit_firstresult("missing-reference", &Event::MissingReference {
                    docname: docname.clone(),
                    target: issue.name.clone(),
                    contnode: issue.message.clone(),
                });
                let _ = events.emit("warn-missing-reference", &Event::WarnMissingReference {
                    domain: issue.code.to_owned(),
                    target: issue.name.clone(),
                });
            }
            duration.note(docname.clone(), reading_started.elapsed().as_secs_f64());

            // Extension post-read hooks: upstream's `doctree-read` event.
            if extensions.contains("ifconfig") {
                for warning in ifconfig::process(&mut output.document, config, flavor.name) {
                    warnings.push(format!("{docname}: {warning}"));
                }
            }
            if extensions.contains("todo") {
                warnings.extend(todos.collect(docname, &output.document, config));
            }

            output.document.metadata.docname = Some(docname.clone());
            // A `.. title::` directive already supplied the long title; only
            // fall back to the first section title when it did not.
            if output.document.metadata.title.is_none() {
                output.document.metadata.title = extract_title(&output.document);
            }
            let _ = events.emit("doctree-read", &Event::DoctreeRead {
                docname: docname.clone(),
                doctree: doctree_json(docname, &output.document),
            });
            parsed.insert(docname.clone(), ParseOutcome { document: output.document });
        }
        profile::mark("parse");

        if extensions.contains("extlinks") {
            let docs: BTreeMap<String, scribe_ast::Document> = parsed
                .iter()
                .map(|(name, outcome)| (name.clone(), outcome.document.clone()))
                .collect();
            warnings.extend(scribe_core::extensions::hardcoded_link_warnings(config, &docs));
        }

        // Render graphviz nodes while the source directory and output
        // directory are both known. Failures degrade to a text placeholder.
        let mut graphviz_infos: BTreeMap<String, BTreeMap<u32, GraphvizHtmlInfo>> = BTreeMap::new();
        if extensions.contains("graphviz") {
            let mut renderer = GraphvizRender::new(config, source_dir, outdir);
            for (docname, outcome) in &mut parsed {
                let (infos, graphviz_warnings) = renderer.render_document(docname, &mut outcome.document);
                graphviz_infos.insert(docname.clone(), infos);
                warnings.extend(graphviz_warnings);
            }
        }

        // Expand every document's domain signatures *before* registering any
        // object.  `py:fullname` / `rst:name` and friends are produced by
        // `prepare_document`; registering before expansion silently drops every
        // domain object and makes cross-document references unresolvable.
        for outcome in parsed.values_mut() {
            if let Err(error) = prepare_document(&mut outcome.document) {
                warnings.push(format!("domain signature expansion failed: {error}"));
            }
        }
        // Register every document's objects before resolving any xref so
        // cross-document references resolve against the complete inventory.
        {
            let transform_ctx = TransformContext::new(Some(config), Some(&tags), "html", "html")
                .with_domains(&registry)
                .with_env(env);
            for (docname, outcome) in &parsed {
                let domain_ctx = transform_ctx.domain_context(docname);
                if let Err(error) = registry.process_document(&domain_ctx, docname, &outcome.document) {
                    warnings.push(format!("{docname}: domain registration failed: {error}"));
                }
            }
        }
        profile::mark("domain");

        // ---- pass B: transforms + render ----------------------------------
        let target_uris: BTreeMap<String, String> = docnames
            .iter()
            .map(|name| (name.clone(), flavor.target_uri(name)))
            .collect();

        // `todolist` needs the full project-wide todo inventory, so expand it
        // only after every document has been read (upstream's
        // `doctree-resolved` / `TodoListProcessor`).
        if extensions.contains("todo") {
            let mut documents: BTreeMap<String, scribe_ast::Document> = parsed
                .iter()
                .map(|(name, outcome)| (name.clone(), outcome.document.clone()))
                .collect();
            todos.expand(&mut documents, config, &target_uris);
            for (name, document) in documents {
                if let Some(outcome) = parsed.get_mut(&name) {
                    outcome.document = document;
                }
            }
        }

        let mut images = BTreeSet::new();
        for outcome in parsed.values() {
            for id in outcome.document.preorder_ids() {
                if let Ok(node) = outcome.document.get(id) {
                    if node.kind == NodeKind::Image {
                        if let Some(uri) = node.attrs.get_str("uri").ok().flatten() {
                            if !uri.is_empty() { images.insert(uri.to_owned()); }
                        }
                    }
                }
            }
        }
        let image_uris = copy_images(images.iter().cloned().collect::<Vec<_>>().as_slice(), source_dir, outdir)?;

        // The translator only reads immutable title metadata from the
        // environment and config. Share one snapshot across every page instead
        // of cloning the whole environment/config O(pages) times (Phase 32).
        let env_arc = Arc::new((*env).clone());
        // Doctrees are serialized as soon as a document's transforms finish;
        // `parsed` therefore no longer has to keep every AST resident until the
        // very end of the build (Phase 32 arena-scoped lifecycle).
        let doctree_dir = options.doctree_dir.clone().unwrap_or_else(|| outdir.join(".doctrees"));

        let mut prepared: Vec<(String, Document, RenderContext, HtmlContext)> = Vec::new();
        let compact = crate::i18n::compact_setting(config);
        for docname in &docnames {
            // Take ownership of the AST: after this point the document lives
            // only in `prepared` (one copy, not a `parsed` + `prepared` pair).
            let Some(mut outcome) = parsed.remove(docname) else { continue };
            let domain = crate::i18n::docname_to_domain(docname, &compact);
            let translations = crate::i18n::load_catalog(config, source_dir, &domain);
            let mut transform_ctx = TransformContext::new(Some(config), Some(&tags), "html", "html")
                .with_domains(&registry)
                .with_env(env);
            if let Some(catalog) = &translations {
                transform_ctx = transform_ctx.with_translations(catalog);
            }
            let read_report = apply_transforms(&mut outcome.document, &transform_ctx, Phase::Read);
            let resolved_report = apply_transforms(&mut outcome.document, &transform_ctx, Phase::Resolved);
            // Viewcode needs the domain-enriched signatures produced by the
            // transform pipeline, so collect/inject after both phases.
            if extensions.contains("viewcode") {
                viewcode.collect(docname, &mut outcome.document, config, source_dir);
            }
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
            let _ = events.emit("doctree-resolved", &Event::DoctreeResolved {
                docname: docname.clone(),
                doctree: doctree_json(docname, &outcome.document),
            });

            // Persist this doctree now, while its AST is live, then let the
            // arena-scoped AST travel into `prepared` for HTML/JSON emission.
            write_doctree_file(docname, &outcome.document, &doctree_dir)?;

            let mut render_ctx = page_context(
                docname,
                config,
                &theme,
                &theme_name,
                &docnames,
                &target_uris,
                &language,
                outdir,
                &theme_options_checksum(config),
                flavor,
            );
            // Upstream's `html-page-context(pagename, templatename, context,
            // doctree)` lets extensions tweak the Jinja context. Mutations to
            // `context` are applied back to the render context; `templatename`
            // updates are recorded as a diagnostic because all bundled themes
            // render through the single `page.html` template.
            let template_name = Arc::new(std::sync::Mutex::new("page.html".to_owned()));
            let context_json = Arc::new(std::sync::Mutex::new(
                serde_json::json!({ "title": render_ctx.title, "body": render_ctx.body }),
            ));
            let _ = events.emit("html-page-context", &Event::HtmlPageContext {
                pagename: docname.clone(),
                templatename: Arc::clone(&template_name),
                context: Arc::clone(&context_json),
            });
            apply_page_context(&mut render_ctx, &context_json);
            // Directory-style pages live one or more subdirectories deep, so
            // resource URIs must be made relative to the current page's base.
            let page_image_uris = if flavor.directory_style {
                let base = flavor.target_uri(docname);
                image_uris.iter().map(|(key, value)| {
                    (key.clone(), scribe_theme::relative_uri(&base, value))
                }).collect()
            } else {
                image_uris.clone()
            };
            let html_ctx = HtmlContext {
                docname: docname.clone(),
                config: Arc::clone(&config_arc),
                env: Arc::clone(&env_arc),
                theme_ctx: render_ctx.clone(),
                target_uris: target_uris.clone(),
                all_docs: docnames.iter().cloned().collect(),
                add_permalinks: !flavor.embedded && config.get_bool("html_permalinks").unwrap_or(true),
                permalinks_icon: config.get_str("html_permalinks_icon").unwrap_or("¶").to_owned(),
                image_uris: page_image_uris,
                language: language.clone(),
                singlefile: None,
                graphviz: graphviz_infos.get(docname).cloned().unwrap_or_default(),
            };
            prepared.push((docname.clone(), outcome.document, render_ctx, html_ctx));
        }
        profile::mark("transform-resolve");

        warnings.extend(viewcode.warnings.iter().cloned());

        // ---- singlehtml: splice every toctree document into one page -------
        if flavor.singlefile {
            return build_single_file(
                &mut prepared,
                config,
                &theme,
                &theme_name,
                &language,
                outdir,
                &manager,
                &fallback_manager,
                &registry,
                options,
                &mut warnings,
            );
        }

        // Render pages in parallel; collect and write in deterministic order.
        let rendered: Vec<(String, String)> = prepared
            .par_iter()
            .map(|(docname, document, render_ctx, html_ctx)| {
                let body = HTML5Translator::translate(document, html_ctx);
                let mut ctx = render_ctx.clone();
                ctx.body = body;
                ctx.title = document.metadata.title.clone().unwrap_or_else(|| docname.clone());
                ctx.toc = render_local_toc(document, 3);
                let suffix = source_paths.get(docname).map(|(_, s)| s.as_str()).unwrap_or(".rst");
                ctx.set_var("sourcename", format!("{docname}{suffix}.txt"));
                if flavor.serializing {
                    // The serializinghtml builder dumps the page context instead
                    // of rendering a template (upstream `handle_page`).
                    return Ok((docname.clone(), serialize_page_context(&ctx, flavor)));
                }
                let html = manager
                    .render_template("page.html", &ctx)
                    .or_else(|_| fallback_manager.render_template("page.html", &ctx))
                    .map_err(HtmlError::theme)?;
                Ok((docname.clone(), html))
            })
            .collect::<Result<Vec<_>>>()?;
        profile::mark("render");

        let mut written = Vec::new();
        let mut built_docnames = Vec::new();
        for (docname, html) in &rendered {
            let relative = flavor.output_path(docname);
            let path = outdir.join(&relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| HtmlError::io(parent, e))?;
            }
            fs::write(&path, html).map_err(|e| HtmlError::io(&path, e))?;
            written.push(relative);
            built_docnames.push(docname.clone());
        }

        // ---- assets / source dumps ----------------------------------------
        written.extend(copy_theme_assets(&theme, outdir)?);
        written.extend(render_asset_templates(&manager, &theme, config, outdir, &language, flavor.search)?);
        let extra_static: Vec<PathBuf> = config.get_strings("html_static_path")
            .unwrap_or_default().into_iter().map(PathBuf::from).collect();
        written.extend(copy_extra_static(&extra_static, source_dir, outdir)?);
        let pygments = pygments_css(config.get_str("pygments_style").unwrap_or_else(|| theme.pygments_style.as_deref().unwrap_or("default")));
        let pygments_path = outdir.join("_static/pygments.css");
        fs::create_dir_all(pygments_path.parent().unwrap_or(outdir)).map_err(|e| HtmlError::io(&pygments_path, e))?;
        fs::write(&pygments_path, pygments).map_err(|e| HtmlError::io(&pygments_path, e))?;
        written.push(PathBuf::from("_static/pygments.css"));

        if flavor.copy_source && config.get_bool("html_copy_source").unwrap_or(true) {
            written.extend(write_source_dumps(&source_paths, outdir)?);
        }

        // ---- index / search / inventory / buildinfo -----------------------
        let gen_ctx = page_context(
            config.master_doc(),
            config,
            &theme,
            &theme_name,
            &docnames,
            &target_uris,
            &language,
            outdir,
            &theme_options_checksum(config),
            flavor,
        );
        let gen_ctx = gen_ctx.add_doc("genindex").add_doc("search");
        // User templates (e.g. test-root's `!layout.html` override) may not be
        // compilable by minijinja. When user template dirs are present, render
        // the generated index pages with a theme-aware manager that keeps the
        // selected theme and its inheritance chain but skips those user dirs;
        // this preserves the custom theme instead of silently swapping in the
        // bundled default.
        let index_manager = if config.get_strings("templates_path").is_some_and(|paths| !paths.is_empty()) {
            &theme_fallback_manager
        } else {
            &manager
        };
        written.extend(write_index_pages(index_manager, &gen_ctx, &registry, outdir, flavor)?);

        // Phase 18: full-text search index, stemmer assets and search UI.
        if flavor.search {
            written.extend(write_search_index(
                &prepared,
                &source_paths,
                &registry,
                config,
                &language,
                outdir,
                flavor,
            )?);
        }

        // Serializing builders emit JSON instead of a binary inventory + pickle.
        if flavor.serializing {
            written.extend(write_serializing_extras(config, &target_uris, outdir)?);
        } else {
            write_objects_inv(&registry, config, &target_uris, outdir, &mut written)?;
        }

        // ---- Phase 20 extension assets -----------------------------------
        if extensions.contains("githubpages") && !flavor.serializing {
            let files = GithubPagesFiles::from_config(config);
            let nojekyll = outdir.join(".nojekyll");
            fs::write(&nojekyll, "").map_err(|e| HtmlError::io(&nojekyll, e))?;
            written.push(PathBuf::from(".nojekyll"));
            let cname_path = outdir.join("CNAME");
            if let Some(domain) = files.cname {
                fs::write(&cname_path, domain).map_err(|e| HtmlError::io(&cname_path, e))?;
                written.push(PathBuf::from("CNAME"));
            } else if cname_path.exists() {
                let _ = fs::remove_file(&cname_path);
            }
        }
        if extensions.contains("viewcode") && !viewcode.is_empty() && !flavor.singlefile {
            written.extend(write_viewcode_pages(
                &viewcode,
                config,
                &manager,
                &fallback_manager,
                &gen_ctx,
                &target_uris,
                outdir,
            )?);
        }

        // Per-document doctrees were serialized in the pass-B loop as soon as
        // each AST finished its transforms; no whole-project AST map is kept.
        profile::mark("write-assets");
        write_buildinfo(outdir, &theme_options_checksum(config))?;

        // Persist the domain inventories back into the environment so a later
        // incremental build can resolve xrefs without re-reading every source.
        for (name, data) in registry.export_domain_data() {
            if let Ok(value) = serde_json::to_value(&data) {
                env.set_domain_data(&name, value);
            }
        }
        if let Some(cache_dir) = &update_options.cache_dir {
            let _ = env.save(cache_dir);
        }

        written.sort();
        written.dedup();
        let _ = copy_files_parallel(&[]);
        let _ = events.emit("write-done", &Event::WriteDone);

        if extensions.contains("duration") {
            let report_text = duration.render(config);
            if !report_text.is_empty() && !options.really_quiet {
                print!("{report_text}");
            }
            if let Some(path) = config.get_str("duration_write_json").filter(|path| !path.is_empty()) {
                let target = outdir.join(path);
                if let Some(parent) = target.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                if let Err(error) = fs::write(&target, duration.to_json()) {
                    warnings.push(format!("cannot write duration JSON {path}: {error}"));
                } else {
                    written.push(PathBuf::from(path));
                }
            }
        }

        warnings.extend(bridge.warnings());
        warnings.extend(bridge.take_event_diagnostics());
        let _ = events.emit("build-finished", &Event::BuildFinished { succeeded: true });
        profile::mark("done");

        if !options.really_quiet {
            println!("build succeeded, {} warning(s).", warnings.len());
        }

        Ok(BuildResult { written, built_docnames, warnings })
}

struct ParseOutcome {
    document: Document,
}

/// Serializable snapshot of the effective configuration for `config-inited`.
fn config_json(config: &Config) -> serde_json::Value {
    let entries: std::collections::BTreeMap<&str, &scribe_config::ConfigValue> = config.iter().collect();
    serde_json::to_value(entries).unwrap_or(serde_json::Value::Null)
}

/// Lightweight doctree snapshot exposed to third-party handlers. The frozen
/// §7 AST is never mutated through this JSON view.
fn doctree_json(docname: &str, document: &Document) -> SharedJson {
    let value = serde_json::json!({
        "docname": docname,
        "node_count": document.nodes.len(),
        "title": document.metadata.title,
    });
    Arc::new(std::sync::Mutex::new(value))
}

/// Extract docnames returned from `env-get-outdated` / `env-get-updated`.
/// Handlers may return either a JSON string or an array of strings, matching
/// upstream's permissive return contract.
fn docnames_from_results(results: &[HandlerResult]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for result in results.iter().filter_map(|value| value.as_ref().ok().and_then(Option::as_ref)) {
        match result {
            serde_json::Value::String(name) => {
                names.insert(name.clone());
            }
            serde_json::Value::Array(values) => {
                for value in values.iter().filter_map(serde_json::Value::as_str) {
                    names.insert(value.to_owned());
                }
            }
            _ => {}
        }
    }
    names
}

/// Apply an extension-mutated `html-page-context` JSON object to a render
/// context. Only scalar keys already present in the context are applied so a
/// plugin cannot inject arbitrary template variables into the frozen theme API.
fn apply_page_context(render_ctx: &mut RenderContext, context: &SharedJson) {
    let Ok(guard) = context.lock() else { return };
    let serde_json::Value::Object(entries) = &*guard else { return };
    for (key, value) in entries {
        if key == "title" {
            if let Some(title) = value.as_str() {
                render_ctx.title = title.to_owned();
            }
        } else if key == "body" {
            if let Some(body) = value.as_str() {
                render_ctx.body = body.to_owned();
            }
        } else if let Some(text) = value.as_str() {
            render_ctx.set_var(key.clone(), text);
        }
    }
}

fn options_filenames_tags(filenames: &[PathBuf], flavor: &BuildFlavor) -> Vec<String> {
    let mut tags = vec!["html".to_owned(), format!("builder_{}", flavor.name)];
    if filenames.is_empty() { tags.push("write_all".to_owned()); }
    tags
}

/// Serialize the significant parts of a rendered page context into the
/// `sphinxcontrib.serializinghtml` JSON shape.
///
/// Upstream dumps the whole Jinja context; the fields below are the stable,
/// theme-independent subset consumed by readthedocs-style clients and are the
/// ones the differential tests compare (`title`, `body`, `current_page_name`,
/// navigation, TOC and source name).
fn serialize_page_context(ctx: &RenderContext, flavor: &BuildFlavor) -> String {
    let sourcename = ctx.vars.get("sourcename")
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    let pair = |opt: &Option<(String, String)>| -> serde_json::Value {
        match opt {
            Some((link, title)) => serde_json::json!([link, title]),
            None => serde_json::Value::Null,
        }
    };
    let value = serde_json::json!({
        "title": ctx.title,
        "body": ctx.body,
        "current_page_name": ctx.docname,
        "sourcename": sourcename,
        "page_source_suffix": ctx.file_suffix,
        "rellinks": ctx.rellinks.iter().map(|(a, b, c, d)| serde_json::json!([a, b, c, d])).collect::<Vec<_>>(),
        "parents": ctx.parents.iter().map(|(a, b)| serde_json::json!([a, b])).collect::<Vec<_>>(),
        "prev": pair(&ctx.prev),
        "next": pair(&ctx.next),
        "toc": ctx.toc,
        "display_toc": !ctx.toc.is_empty(),
        "metatags": ctx.metatags,
        "meta": serde_json::Value::Null,
        "builder": flavor.name,
    });
    serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_owned())
}

/// Generate the `_modules/<module>.html` source pages contributed by
/// `sphinx.ext.viewcode`, plus the `_modules/index.html` overview.
fn write_viewcode_pages(
    collector: &ViewcodeCollector,
    config: &Config,
    manager: &ThemeManager,
    fallback_manager: &ThemeManager,
    template: &RenderContext,
    target_uris: &BTreeMap<String, String>,
    outdir: &Path,
) -> Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    let linenos = config.get_bool("viewcode_line_numbers").unwrap_or(false);
    let language = if matches!(config.get_str("highlight_language").unwrap_or(""), "default" | "none") {
        config.get_str("highlight_language").unwrap_or("python").to_owned()
    } else {
        "python".to_owned()
    };
    let options = HighlightOptions {
        linenos,
        force: true,
        ..HighlightOptions::default()
    };
    let mut uris = target_uris.clone();
    for module in collector.modules().values() {
        let page = module.page_path();
        uris.insert(page.trim_end_matches(".html").to_owned(), page.clone());
    }
    let mut module_links: Vec<(String, String)> = Vec::new();

    for module in collector.modules().values() {
        let page = module.page_path();
        let module_docname = page.trim_end_matches(".html").to_owned();
        let highlighted = highlight_code(&module.code, &language, &options);
        let root_module = module.name.split('.').next().unwrap_or(&module.name);
        let body = render_module_body(module, &highlighted, &uris, module_docname.as_str(), root_module);
        let mut ctx = template.clone();
        ctx.docname = module_docname.clone();
        ctx.title = module.name.clone();
        ctx.body = body;
        ctx.set_var("sourcename", "");
        let html = manager
            .render_template("page.html", &ctx)
            .or_else(|_| fallback_manager.render_template("page.html", &ctx))
            .map_err(HtmlError::theme)?;
        let path = outdir.join(&page);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| HtmlError::io(parent, e))?;
        }
        fs::write(&path, html).map_err(|e| HtmlError::io(&path, e))?;
        written.push(PathBuf::from(&page));
        let relative = scribe_core::extensions::relative_doc_uri(&uris, "_modules/index", &module_docname);
        let href = if relative.starts_with('#') { page.clone() } else { relative };
        module_links.push((module.name.clone(), href));
    }

    if !module_links.is_empty() {
        let mut body = String::from("<h1>All modules for which code is available</h1>\n<ul>\n");
        for (name, href) in &module_links {
            body.push_str(&format!("<li><a href=\"{}\">{}</a></li>\n", escape_attr(href), escape_html(name)));
        }
        body.push_str("</ul>\n");
        let mut ctx = template.clone();
        ctx.docname = "_modules/index".to_owned();
        ctx.title = "Overview: module code".to_owned();
        ctx.body = body;
        ctx.set_var("sourcename", "");
        let html = manager
            .render_template("page.html", &ctx)
            .or_else(|_| fallback_manager.render_template("page.html", &ctx))
            .map_err(HtmlError::theme)?;
        let path = outdir.join("_modules/index.html");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| HtmlError::io(parent, e))?;
        }
        fs::write(&path, html).map_err(|e| HtmlError::io(&path, e))?;
        written.push(PathBuf::from("_modules/index.html"));
    }
    Ok(written)
}

fn render_module_body(
    module: &scribe_core::extensions::viewcode::ViewcodeModule,
    highlighted: &crate::highlight::HighlightedCode,
    uris: &BTreeMap<String, String>,
    module_docname: &str,
    root_module: &str,
) -> String {
    use crate::html::translator::{escape_attr, escape_html};

    let mut out = format!("<h1>Source code for {}</h1>\n", escape_html(&module.name));
    out.push_str("<div class=\"highlight-python notranslate\"><div class=\"highlight\"><pre><span></span>");
    let width = highlighted
        .lines
        .iter()
        .filter_map(|line| line.line_number)
        .map(|number| number.to_string().len())
        .max()
        .unwrap_or(0);
    // Map `fullname -> (start, end)` from the module analyzer to line indices.
    let mut opens: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    let mut closes: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for (name, (start, end)) in &module.tags {
        opens.entry(*start).or_default().push(name.clone());
        closes.entry(*end).or_default().push(name.clone());
    }
    let backlink_base = scribe_core::extensions::relative_doc_uri(uris, module_docname, "index");
    for (index, line) in highlighted.lines.iter().enumerate() {
        for name in opens.get(&index).into_iter().flatten() {
            let href = if backlink_base.starts_with('#') {
                format!("#{root_module}.{name}")
            } else {
                format!("{backlink_base}#{root_module}.{name}")
            };
            out.push_str(&format!(
                "<div class=\"viewcode-block\" id=\"{}\">\n<a class=\"viewcode-back\" href=\"{}\">[docs]</a>\n",
                escape_attr(name),
                escape_attr(&href)
            ));
        }
        if let Some(number) = line.line_number {
            out.push_str("<span class=\"linenos\">");
            if width == 0 {
                out.push_str(&number.to_string());
            } else {
                out.push_str(&format!("{number:>width$}"));
            }
            out.push_str("</span>");
        }
        for token in &line.tokens {
            if token.classes.is_empty() {
                out.push_str(&escape_html(&token.text));
            } else {
                out.push_str(&format!("<span class=\"{}\">{}</span>", escape_attr(&token.classes.join(" ")), escape_html(&token.text)));
            }
        }
        out.push('\n');
        for _ in closes.get(&index).into_iter().flatten() {
            out.push_str("</div>\n");
        }
    }
    out.push_str("</pre></div></div>");
    out
}

/// Emit the extra files written by `sphinxcontrib.serializinghtml` on finish:
/// the global context, a JSON search index, the environment marker and the
/// `last_build` touch file.
fn write_serializing_extras(
    config: &Config,
    target_uris: &BTreeMap<String, String>,
    outdir: &Path,
) -> Result<Vec<PathBuf>> {
    let project = config.get_str("project").unwrap_or("");
    let release = config.get_str("release").unwrap_or("");
    let version = config.get_str("version").unwrap_or("");
    let master = config.master_doc();
    let global = serde_json::json!({
        "project": project,
        "release": release,
        "version": version,
        "master_doc": master,
        "root_doc": master,
        "docstitle": config.get_str("html_title").unwrap_or(project),
        "language": config.get_str("language").filter(|s| !s.is_empty()).unwrap_or("en"),
        "builder": "json",
        "file_suffix": ".fjson",
        "link_suffix": "/",
        "has_source": true,
        "show_source": true,
        "embedded": false,
        "search": true,
        "use_index": true,
        "copyright": config.get_str("copyright").unwrap_or(""),
        "rellinks": [["genindex", "General Index", "I", "index"]],
        "parents": [],
        "target_uris": target_uris,
    });
    let global_path = outdir.join("globalcontext.json");
    fs::write(&global_path, serde_json::to_string(&global).unwrap_or_else(|_| "{}".to_owned()))
        .map_err(|e| HtmlError::io(&global_path, e))?;
    // `environment.pickle` and `last_build` are opaque runtime markers for the
    // readthedocs web app. Deterministic empty placeholders keep the file
    // layout faithful without embedding non-reproducible pickle bytes.
    for name in ["environment.pickle", "last_build"] {
        let path = outdir.join(name);
        fs::write(&path, b"").map_err(|e| HtmlError::io(&path, e))?;
    }
    Ok(vec![
        PathBuf::from("globalcontext.json"),
        PathBuf::from("environment.pickle"),
        PathBuf::from("last_build"),
    ])
}

fn suffix_for_source(config: &Config, path: &Path) -> String {
    if let Some(dict) = config.get_dict("source_suffix") {
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            let key = format!(".{ext}");
            if dict.contains_key(&key) { return key; }
        }
    }
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_else(|| ".rst".to_owned())
}

fn extract_title(document: &Document) -> Option<String> {
    document.preorder_ids().into_iter().find_map(|id| {
        let node = document.get(id).ok()?;
        if node.kind == NodeKind::Title { Some(node.astext(document)) } else { None }
    })
}

fn theme_options_checksum(config: &Config) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for (key, value) in config.iter() {
        key.hash(&mut hasher);
        value.display_string().hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

/// Build the Phase 16 render context for one page (before the body is set).
#[allow(clippy::too_many_arguments)]
fn page_context(
    docname: &str,
    config: &Config,
    theme: &scribe_theme::Theme,
    theme_name: &str,
    docnames: &[String],
    target_uris: &BTreeMap<String, String>,
    language: &str,
    outdir: &Path,
    config_hash: &str,
    flavor: &BuildFlavor,
) -> RenderContext {
    let mut ctx = RenderContext::for_builder(flavor.name, docname, config.clone()).with_theme(theme_name);
    ctx.all_docs = docnames.iter().cloned().collect::<BTreeSet<_>>();
    ctx.all_docs.insert("genindex".to_owned());
    ctx.all_docs.insert("search".to_owned());
    // Special pages share the flavor's URI convention (`genindex/` for
    // dirhtml/json, `genindex.html` for the standalone family).
    let mut target_uris = target_uris.clone();
    for special in ["genindex", "py-modindex", "search"] {
        target_uris.entry(special.to_owned()).or_insert_with(|| flavor.target_uri(special));
    }
    ctx.target_uris = target_uris.clone();
    ctx.link_suffix = if flavor.directory_style { "/".to_owned() } else { flavor.out_suffix.to_owned() };
    ctx.file_suffix = flavor.out_suffix.to_owned();
    ctx.search_enabled = flavor.search;
    ctx.use_index = true;
    ctx.theme_options = theme.options.clone();
    ctx.language = language.to_owned();

    let master = config.master_doc();
    let root_title = config.get_str("html_title").filter(|s| !s.is_empty()).map(str::to_owned)
        .unwrap_or_else(|| default_html_title(config));
    ctx.rellinks = vec![
        ("genindex".to_owned(), "General Index".to_owned(), "I".to_owned(), "index".to_owned()),
    ];
    ctx.set_var("link", "");
    if !flavor.embedded && docname != master {
        let master_uri = target_uris.get(master).cloned().unwrap_or_else(|| flavor.target_uri(master));
        ctx.parents.push((format!("{master_uri}#{master}"), root_title));
    }
    let mut css = vec![
        stylesheet(format!("_static/pygments.css?v={}", asset_hash(outdir, "_static/pygments.css", config_hash))),
    ];
    for name in &theme.stylesheets {
        css.push(stylesheet(format!("_static/{name}?v={}", asset_hash(outdir, &format!("_static/{name}"), config_hash))));
    }
    ctx.css_files = css;
    ctx.script_files = vec![
        script("_static/documentation_options.js?v=".to_owned() + &asset_hash(outdir, "_static/documentation_options.js", config_hash)),
        script("_static/doctools.js?v=".to_owned() + &asset_hash(outdir, "_static/doctools.js", config_hash)),
        script("_static/sphinx_highlight.js?v=".to_owned() + &asset_hash(outdir, "_static/sphinx_highlight.js", config_hash)),
    ];
    ctx
}

fn stylesheet(filename: String) -> minijinja::Value {
    let mut map = std::collections::BTreeMap::new();
    map.insert("filename".to_owned(), minijinja::Value::from(filename));
    map.insert("rel".to_owned(), minijinja::Value::from("stylesheet"));
    map.insert("type".to_owned(), minijinja::Value::from("text/css"));
    map.insert("priority".to_owned(), minijinja::Value::from(500_i64));
    minijinja::Value::from(map)
}

fn script(filename: String) -> minijinja::Value {
    let mut map = std::collections::BTreeMap::new();
    map.insert("filename".to_owned(), minijinja::Value::from(filename));
    map.insert("priority".to_owned(), minijinja::Value::from(500_i64));
    minijinja::Value::from(map)
}

fn default_html_title(config: &Config) -> String {
    let project = config.get_str("project").unwrap_or("");
    let release = config.get_str("release").unwrap_or("");
    if project.is_empty() { "documentation".to_owned() } else { format!("{project} {release} documentation") }
}

fn asset_hash(_outdir: &Path, relative: &str, fallback: &str) -> String {
    // Deterministic content-independent hash: the differential tests normalise
    // `?v=` values, but distinct assets must still get distinct version tags.
    let digest = blake3::hash(format!("{relative}:{fallback}").as_bytes());
    digest.to_hex()[..8].to_owned()
}

/// Build and write `searchindex.js` plus the vendored stemmer JS assets.
///
/// Documents are fed from their post-transform doctrees (matching upstream's
/// `index_page` call site), in deterministic docname order. Objects come from
/// every registered domain. The emitted JavaScript is the same
/// `Search.setIndex({...})` payload the stock `searchtools.js` expects.
fn write_search_index(
    prepared: &[(String, Document, RenderContext, HtmlContext)],
    source_paths: &BTreeMap<String, (PathBuf, String)>,
    registry: &DomainRegistry,
    config: &Config,
    language: &str,
    outdir: &Path,
    flavor: &BuildFlavor,
) -> Result<Vec<PathBuf>> {
    let search_language = config
        .get_str("html_search_language")
        .filter(|lang| !lang.is_empty())
        .unwrap_or(language)
        .to_owned();
    let mut index = SearchIndex::new(search_language, scribe_envversion());
    for (docname, document, ..) in prepared {
        let title = document.metadata.title.clone().unwrap_or_default();
        let filename = source_paths
            .get(docname)
            .map(|(_, suffix)| format!("{docname}{suffix}"))
            .unwrap_or_else(|| format!("{docname}.rst"));
        if title.is_empty() {
            // Upstream feeds an empty doctree for untitled pages so their
            // section titles do not leak into `alltitles`.
            index.feed_with_filename(docname, &filename, "", &Document::new(None));
        } else {
            index.feed_with_filename(docname, &filename, &title, document);
        }
    }
    index.add_domain_objects(registry);

    let path = outdir.join(if flavor.serializing { "searchindex.json" } else { "searchindex.js" });
    let payload = if flavor.serializing {
        serde_json::to_string(&index.to_json_value())
            .unwrap_or_else(|_| "{}".to_owned())
    } else {
        index.to_js()
    };
    fs::write(&path, payload).map_err(|error| HtmlError::io(&path, error))?;

    // Copy the vendored minified stemmer assets into `_static/` for parity
    // with upstream's `copy_stemmer_js()`.
    let static_dir = outdir.join("_static");
    fs::create_dir_all(&static_dir).map_err(|error| HtmlError::io(&static_dir, error))?;
    for (name, content) in [
        ("base-stemmer.js", BASE_STEMMER_JS),
        ("english-stemmer.js", ENGLISH_STEMMER_JS),
    ] {
        let target = static_dir.join(name);
        fs::write(&target, content).map_err(|error| HtmlError::io(&target, error))?;
    }

    Ok(vec![
        PathBuf::from(if flavor.serializing { "searchindex.json" } else { "searchindex.js" }),
        PathBuf::from("_static/base-stemmer.js"),
        PathBuf::from("_static/english-stemmer.js"),
    ])
}

fn write_objects_inv(
    registry: &DomainRegistry,
    config: &Config,
    target_uris: &BTreeMap<String, String>,
    outdir: &Path,
    written: &mut Vec<PathBuf>,
) -> Result<()> {
    let mut objects: BTreeMap<String, Vec<scribe_domains::ObjectEntry>> = BTreeMap::new();
    for name in registry.domain_names() {
        if let Some(shared) = registry.domain(&name) {
            objects.insert(name, shared.lock().get_objects());
        }
    }
    let project = config.get_str("project").unwrap_or("").to_owned();
    let version = config.get_str("release").or_else(|| config.get_str("version")).unwrap_or("").to_owned();
    let dump_config = DumpConfig { project: &project, version: &version };
    let uris = target_uris.clone();
    let bytes = Inventory::dump(&objects, &dump_config, move |docname| {
        uris.get(docname).cloned().unwrap_or_else(|| format!("{docname}.html"))
    });
    let path = outdir.join("objects.inv");
    fs::write(&path, bytes).map_err(|e| HtmlError::io(&path, e))?;
    written.push(PathBuf::from("objects.inv"));
    Ok(())
}

/// Serialize one document's doctree to the on-disk cache immediately after
/// its transforms finish, so the in-memory AST can be dropped as soon as HTML
/// emission completes (Phase 32 arena-scoped lifecycle). Byte-identical to the
/// previous whole-project `write_doctree_files` helper.
fn write_doctree_file(docname: &str, document: &Document, dir: &Path) -> Result<()> {
    fs::create_dir_all(dir).map_err(|e| HtmlError::io(dir, e))?;
    let json = serde_json::to_string(document)
        .map_err(|e| HtmlError::Invalid(format!("doctree serialization failed: {e}")))?;
    let path = dir.join(format!("{docname}.doctree"));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| HtmlError::io(parent, e))?;
    }
    fs::write(&path, json).map_err(|e| HtmlError::io(&path, e))?;
    Ok(())
}

/// Assemble and write the one page produced by the `singlehtml` builder.
///
/// This is called from [`build_with_flavor`] after each source has been parsed
/// and transformed. The documents are cloned and their ids rewritten in place
/// (the frozen AST crate is untouched), then bodies are concatenated in toctree
/// traversal order and the result is wrapped in the standard page template.
#[allow(clippy::too_many_arguments)]
fn build_single_file(
    prepared: &mut [crate::singlehtml::PreparedPage],
    config: &Config,
    theme: &scribe_theme::Theme,
    theme_name: &str,
    language: &str,
    outdir: &Path,
    manager: &ThemeManager,
    fallback_manager: &ThemeManager,
    registry: &DomainRegistry,
    options: &BuildOptions,
    warnings: &mut [String],
) -> Result<BuildResult> {
    use crate::singlehtml::{global_toc, rewrite_anchors, traversal_order};

    let master = config.master_doc();
    let mut order = traversal_order(prepared, master);
    if order.is_empty() {
        // A source tree with no toctree still yields one page containing the
        // master document only (upstream's `assemble_doctree` starts at root).
        if prepared.iter().any(|(name, ..)| name == master) {
            order.push(master.to_owned());
        } else if let Some((name, ..)) = prepared.first() {
            order.push(name.clone());
        }
    }
    let anchors = rewrite_anchors(prepared, &order);
    let single = SingleFileContext {
        anchors,
        inlined_docs: order.iter().cloned().collect(),
        skip_toctrees: true,
    };

    let mut body = String::new();
    for docname in &order {
        let Some(index) = prepared.iter().position(|(name, ..)| name == docname) else {
            continue;
        };
        let (_, document, _, base_ctx) = &prepared[index];
        // Every inlined document gets a marker so `#document-<docname>` links
        // (including links to the master document) resolve on the one page.
        body.push_str(&format!("<span id=\"document-{docname}\"></span>\n"));
        let mut ctx = base_ctx.clone();
        ctx.singlefile = Some(single.clone());
        body.push_str(&HTML5Translator::translate(document, &ctx));
        body.push('\n');
    }

    let toc = global_toc(prepared, &order);
    let master_index = prepared.iter().position(|(name, ..)| name == master).unwrap_or(0);
    let mut page_ctx = prepared.get(master_index).map(|(_, _, ctx, _)| ctx.clone())
        .unwrap_or_else(|| page_context(master, config, theme, theme_name, &order, &BTreeMap::new(), language, outdir, &theme_options_checksum(config), &BuildFlavor::singlefile()));
    page_ctx.body = body;
    page_ctx.toc = toc.clone();
    // Alabaster/sidebar templates render `toctree()` rather than `toc`; both
    // must carry the assembled navigation for `#document-*` links to appear.
    page_ctx.toctree_html = toc;
    page_ctx.title = config.get_str("html_title").filter(|s| !s.is_empty()).map(str::to_owned)
        .unwrap_or_else(|| default_html_title(config));
    page_ctx.set_var("title", page_ctx.title.clone());
    page_ctx.set_var("sourcename", "");
    page_ctx.parents.clear();
    page_ctx.rellinks.clear();
    page_ctx.prev = None;
    page_ctx.next = None;
    page_ctx.search_enabled = false;
    page_ctx.use_index = false;

    let html = manager.render_template("page.html", &page_ctx)
        .or_else(|_| fallback_manager.render_template("page.html", &page_ctx))
        .map_err(HtmlError::theme)?;
    let path = outdir.join("index.html");
    fs::write(&path, html).map_err(|e| HtmlError::io(&path, e))?;

    let mut written = vec![PathBuf::from("index.html")];
    written.extend(copy_theme_assets(theme, outdir)?);
    written.extend(render_asset_templates(manager, theme, config, outdir, language, false)?);
    let extra_static: Vec<PathBuf> = config.get_strings("html_static_path")
        .unwrap_or_default().into_iter().map(PathBuf::from).collect();
    written.extend(copy_extra_static(&extra_static, &options.source_dir, outdir)?);
    let pygments = pygments_css(config.get_str("pygments_style")
        .unwrap_or_else(|| theme.pygments_style.as_deref().unwrap_or("default")));
    let pygments_path = outdir.join("_static/pygments.css");
    if let Some(parent) = pygments_path.parent() {
        fs::create_dir_all(parent).map_err(|e| HtmlError::io(parent, e))?;
    }
    fs::write(&pygments_path, pygments).map_err(|e| HtmlError::io(&pygments_path, e))?;
    written.push(PathBuf::from("_static/pygments.css"));

    // Single-file builds still ship an inventory but no search page/index.
    let mut target_uris: BTreeMap<String, String> = order.iter()
        .map(|name| (name.clone(), crate::singlehtml::target_uri(name, true)))
        .collect();
    for special in ["genindex", "py-modindex", "search"] {
        target_uris.entry(special.to_owned()).or_insert_with(|| format!("{special}.html"));
    }
    write_objects_inv(registry, config, &target_uris, outdir, &mut written)?;
    write_buildinfo(outdir, &theme_options_checksum(config))?;

    written.sort();
    written.dedup();
    if !options.really_quiet {
        println!("build succeeded, {} warning(s).", warnings.len());
    }
    Ok(BuildResult { written, built_docnames: order, warnings: warnings.to_vec() })
}


#[cfg(test)]
mod tests {
    use super::theme_is_genuinely_missing;
    use scribe_theme::ThemeError;

    #[test]
    fn only_absent_themes_are_eligible_for_basic_fallback() {
        let missing_root = ThemeError::MissingParent {
            name: "ghost".to_owned(),
            parent: "ghost".to_owned(),
            known: "alabaster, basic".to_owned(),
        };
        assert!(theme_is_genuinely_missing(&missing_root, "ghost"));
        assert!(!theme_is_genuinely_missing(&missing_root, "other"));

        // A present theme with a missing ancestor is misconfigured, not absent.
        let missing_ancestor = ThemeError::MissingParent {
            name: "sphinx13".to_owned(),
            parent: "ghost".to_owned(),
            known: "alabaster, basic".to_owned(),
        };
        assert!(!theme_is_genuinely_missing(&missing_ancestor, "sphinx13"));

        // Misconfigured-but-present themes stay hard errors.
        assert!(!theme_is_genuinely_missing(
            &ThemeError::InheritanceCycle {
                name: "a".to_owned(),
                cycle: "a -> b -> a".to_owned(),
            },
            "a"
        ));
        assert!(!theme_is_genuinely_missing(
            &ThemeError::TooManyAncestors { name: "a".to_owned(), limit: 10 },
            "a"
        ));
        assert!(!theme_is_genuinely_missing(
            &ThemeError::InvalidMetadata {
                path: "theme.toml".into(),
                detail: "bad".to_owned(),
            },
            "a"
        ));
    }
}
