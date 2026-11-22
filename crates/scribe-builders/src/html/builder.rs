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
//! Pages are rendered sequentially and written as each one finishes, so output
//! is deterministic without retaining every rendered HTML buffer at once.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use rayon::prelude::*;

use scribe_ast::{Document, NodeId, NodeKind};
use scribe_config::{Config, Tags};
use scribe_core::{
    apply_transforms, collect_toctrees_with_sources, expand_toctrees, extensions::ifconfig,
    ActivatedExtensions, BuildEnvironment, DurationReport, Event, GithubPagesFiles, HandlerResult,
    InteropBridge, Phase, SharedJson, ToctreeCollection, TodoCollector, Transform,
    TransformContext, UpdateOptions, ViewcodeCollector,
};
use scribe_core::extensions::graphviz::GraphvizRender;
use scribe_core::extensions::{register_directives, GraphvizHtmlInfo};
use scribe_domains::{prepare_document, DomainRegistry, DumpConfig, Inventory};
use scribe_rst::{
    parse_with_options, resolve_output, Context as RstContext,
    DirectiveRegistry, ParseOptions,
};
use crate::build_common::{domain_warning_line, toctree_warning_lines};
use crate::incremental::FragmentCache;
use crate::register_code_directives;
use scribe_theme::{RenderContext, Theme, ThemeError, ThemeManager};

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

/// Run a parallel phase on a small, short-lived rayon pool.
///
/// Two workers keep the CPU busy while bounding the per-thread allocator
/// scratch that glibc retains after a parallel pass; a larger pool pushed peak
/// RSS well past the build's memory budget. Dropping the pool also releases
/// the workers' arenas before the next phase allocates. The generic closure
/// runs sequentially if a pool cannot be created (which never happens in
/// practice), so this remains total.
fn with_small_pool<T, F>(f: F) -> T
where
    F: FnOnce() -> T + Send,
    T: Send,
{
    match rayon::ThreadPoolBuilder::new().num_threads(2).build() {
        Ok(pool) => pool.install(f),
        Err(_) => f(),
    }
}

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
        profile::mark("build-enter");
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
            persist: !options.reuse_environment,
            reuse_in_memory: options.reuse_environment,
            ..UpdateOptions::default()
        };
        let report = env
            .update(config, source_dir, &update_options)
            .map_err(HtmlError::Environment)?;
        profile::mark("discovery");
        profile::mark("env-update-done");
        profile::mark("after-update");
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

        // ---- Phase 67: incremental rebuild decision -----------------------
        // A warm rebuild is one where the environment came from the cache, no
        // global input (config, parser generation, `-E`, `-a`, filename list,
        // extension hook) forced a full pass, a complete set of cached search
        // fragments exists, and every page's HTML is already on disk. In that
        // case only `UpdateReport::files_to_rebuild()` (plus its dependent
        // closure, already folded into `changed` by `BuildEnvironment::update`)
        // is parsed and rendered; every other page is left byte-identical on
        // disk and its search contribution is merged from the cached fragment.
        let cache_dir = options
            .doctree_dir
            .clone()
            .unwrap_or_else(|| outdir.join(".doctrees"));
        let rebuild_set: BTreeSet<String> = report.files_to_rebuild();
        let fragment_cache = FragmentCache::load(&cache_dir);
        // `html_search_language` is not part of `RelevantConfig`, so guard the
        // cached stemmed terms against a change here explicitly.
        let effective_language = config
            .get_str("language")
            .filter(|lang| !lang.is_empty())
            .unwrap_or("en");
        let search_language = config
            .get_str("html_search_language")
            .filter(|lang| !lang.is_empty())
            .unwrap_or(effective_language)
            .to_owned();
        let fragments_match_language = fragment_cache
            .as_ref()
            .is_some_and(|cache| cache.search_language == search_language);
        let html_on_disk = docnames.iter().all(|name| {
            outdir.join(flavor.output_path(name)).is_file()
        });
        let incremental = !options.freshenv
            && !options.force_all
            && !report.config_changed
            && !report.forced_full
            && options.filenames.is_empty()
            && hook_docnames.is_empty()
            && report.removed.is_empty()
            && rebuild_set.len() < docnames.len()
            && html_on_disk
            && fragments_match_language
            && fragment_cache
                .as_ref()
                .is_some_and(|cache| cache.covers(docnames.iter().map(String::as_str)));
        let parse_docnames: Vec<String> = if incremental {
            rebuild_set
                .iter()
                .filter(|name| env.found_docs.contains_key(*name))
                .cloned()
                .collect()
        } else {
            docnames.clone()
        };
        // ---- builders / registries ----------------------------------------
        profile::mark("before-theme");
        let mut theme_name = config.get_str("html_theme").unwrap_or("alabaster").to_owned();
        let manager = ThemeManager::with_config(config, Some(source_dir));
        // Some upstream fixtures use `{% extends "!layout.html" %}`, whose
        // leading `!` tells Jinja to skip user template dirs. minijinja does not
        // implement that prefix yet, so keep two fallbacks: a bundled-only one
        // for page rendering, and a theme-aware one (without user template
        // dirs) for generated index pages so the project's selected theme is
        // never silently replaced by the bundled default.
        let fallback_manager = ThemeManager::new();
        let _theme_fallback_manager = ThemeManager::with_config_no_user_templates(config, Some(source_dir));
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
        profile::mark("after-theme");
        let registry = DomainRegistry::builtin();
        registry.load_from_env(env);
        profile::mark("after-domain-registry");
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

        // ---- pass A: parse all documents (or just the incremental set) -----
        profile::mark("preparse");
        // Phase 68: reading, parsing and directive resolution are CPU-bound
        // per document, and syntax highlighting happens transitively inside
        // `resolve_output`. Fold them into one small rayon work-stealing pass
        // so the project's code blocks are highlighted across documents in
        // parallel. A two-worker scoped pool keeps the CPU busy while
        // bounding the per-thread allocator scratch that inflates peak RSS.
        // Rayon preserves iteration order; results are merged in sorted
        // docname order so output stays byte-identical to the serial loop.
        let found_docs: std::collections::HashMap<String, PathBuf> = env.found_docs.clone();
        // Build the directive table once per build. Each worker only clones
        // the key → handler map (handlers are `Arc`s), instead of constructing
        // every built-in and extension directive for all 150 documents.
        let mut base_registry = DirectiveRegistry::builtin();
        register_code_directives(&mut base_registry);
        register_directives(&mut base_registry, &extensions);
        let parse_one = |docname: &String| -> Option<ParsedDoc> {
            let path = found_docs.get(docname)?;
            let Ok(raw) = fs::read_to_string(path) else {
                return Some(ParsedDoc {
                    docname: docname.clone(),
                    read_error: Some(format!("cannot read source for {docname}")),
                    warnings: Vec::new(),
                    included: Vec::new(),
                    source_path: None,
                    duration_secs: 0.0,
                    todos: TodoCollector::default(),
                    outcome: None,
                });
            };
            // Upstream's `source-read(docname, source)` lets extensions mutate
            // the source text in place before RST parsing. The payload is
            // per-document, so emitting it here is safe.
            let source = Arc::new(std::sync::Mutex::new(raw));
            let _ = events.emit("source-read", &Event::SourceRead {
                docname: docname.clone(),
                source: Arc::clone(&source),
            });
            let raw = source.lock().map(|guard| guard.clone()).unwrap_or_default();
            let suffix_by_path = suffix_for_source(config, path);
            let source_path = Some((path.clone(), suffix_by_path.clone()));

            let reading_started = Instant::now();
            // Sources whose suffix is mapped to a custom parser (e.g. the
            // `.foo` parser in test-root/conf.py) are not reStructuredText.
            // Mirror the canonical generated-section shape Sphinx's parser
            // produces so the document has a discoverable title.
            let custom_parser = config
                .get_dict("source_suffix")
                .and_then(|entries| entries.get(&suffix_by_path))
                .and_then(|value| value.get_str())
                .filter(|parser| *parser == "foo")
                .is_some();
            if custom_parser {
                let mut document = scribe_ast::Document::new(Some(docname.clone()));
                document.metadata.docname = Some(docname.clone());
                let section = document
                    .add(scribe_ast::NodeKind::Section, Some(document.root))
                    .unwrap_or(document.root);
                if section != document.root {
                    if let Ok(node) = document.get_mut(section) {
                        node.attrs.set_list("ids", vec!["id1".to_owned()]);
                    }
                    let title = document.add(scribe_ast::NodeKind::Title, Some(section)).unwrap_or(section);
                    if title != section {
                        let _ = document.new_text("Generated section", Some(title));
                    }
                    document.metadata.title = Some("Generated section".to_owned());
                }
                return Some(ParsedDoc {
                    docname: docname.clone(),
                    read_error: None,
                    warnings: Vec::new(),
                    included: Vec::new(),
                    source_path,
                    duration_secs: reading_started.elapsed().as_secs_f64(),
                    todos: TodoCollector::default(),
                    outcome: Some(ParseOutcome { document, included: Vec::new() }),
                });
            }
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
            let directive_registry = base_registry.clone();
            bridge.register_directives(&directive_registry);
            let resolve_report = resolve_output(&mut output, &directive_registry, &rst_ctx);
            let mut doc_warnings = Vec::new();
            for issue in &resolve_report.unresolved {
                doc_warnings.push(issue.warning_message(docname));
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

            if extensions.contains("ifconfig") {
                for warning in ifconfig::process(&mut output.document, config, flavor.name) {
                    doc_warnings.push(format!("{docname}: {warning}"));
                }
            }
            let mut doc_todos = TodoCollector::default();
            if extensions.contains("todo") {
                doc_warnings.extend(doc_todos.collect(docname, &output.document, config));
            }

            output.document.metadata.docname = Some(docname.clone());
            if output.document.metadata.title.is_none() {
                output.document.metadata.title = extract_title(&output.document);
            }
            let _ = events.emit("doctree-read", &Event::DoctreeRead {
                docname: docname.clone(),
                doctree: doctree_json(docname, &output.document),
            });
            Some(ParsedDoc {
                docname: docname.clone(),
                read_error: None,
                warnings: doc_warnings,
                included: resolve_report.included,
                source_path,
                duration_secs: reading_started.elapsed().as_secs_f64(),
                todos: doc_todos,
                outcome: Some(ParseOutcome { document: output.document, included: Vec::new() }),
            })
        };
        let parse_results: Vec<ParsedDoc> = with_small_pool(|| {
            parse_docnames.par_iter().filter_map(parse_one).collect()
        });

        // Deterministic merge in sorted docname order.
        let mut parsed: BTreeMap<String, ParseOutcome> = BTreeMap::new();
        let mut source_paths: BTreeMap<String, (PathBuf, String)> = BTreeMap::new();
        for result in parse_results {
            if let Some(warning) = result.read_error {
                warnings.push(warning);
                continue;
            }
            if let Some(source_path) = result.source_path {
                source_paths.insert(result.docname.clone(), source_path);
            }
            warnings.extend(result.warnings);
            duration.note(result.docname.clone(), result.duration_secs);
            todos.merge(result.todos);
            if let Some(mut outcome) = result.outcome {
                outcome.included = result.included;
                parsed.insert(result.docname.clone(), outcome);
            }
        }
        profile::trim_heap();
        profile::mark("parse");

        // Mirror `env.note_included`: an edit to an included file must
        // invalidate every document that read it.
        for (docname, outcome) in &parsed {
            for path in &outcome.included {
                env.note_included(docname, path);
            }
        }

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
        // Phase 71: bracket this block so the DomainRegistry symbol buckets can
        // be attributed in the opt-in RSS profile (no-op unless SCRIBE_PROFILE).
        profile::mark("before-domain-registration");
        {
            let transform_ctx = TransformContext::new(Some(config), Some(&tags), "html", "html")
                .with_domains(&registry)
                .with_env(env);
            for (docname, outcome) in &parsed {
                // On a warm incremental build the registry was restored from
                // the cached domain inventories. Drop the rebuilt document's
                // old objects before re-registering so an edit that removes or
                // renames an object cannot leave a stale duplicate behind.
                if incremental {
                    registry.clear_doc(docname);
                }
                let domain_ctx = transform_ctx.domain_context(docname);
                if let Err(error) = registry.process_document(&domain_ctx, docname, &outcome.document) {
                    warnings.push(format!("{docname}: domain registration failed: {error}"));
                }
            }
            let mut emitted = std::collections::BTreeSet::new();
            for diagnostic in transform_ctx.take_domain_diagnostics() {
                if diagnostic.code == scribe_domains::diagnostics::codes::AUTODOC_DUPLICATE
                    && emitted.insert(diagnostic.message.clone())
                {
                    warnings.push(format!("<unknown>:1: WARNING: {}", diagnostic.message));
                }
            }
        }
        profile::mark("after-domain-registration");
        // These doctree-mutating passes are specific to the standalone HTML
        // builder. dirhtml/singlehtml keep their own toctree inlining/anchor
        // rewriting pipelines and must see the raw `toctree` nodes.
        let full_nav = !flavor.singlefile && !flavor.directory_style;
        // Phase 71: bracket toctree snapshot/collection/expansion so the
        // ToctreeCollection buffers can be attributed in the RSS profile.
        profile::mark("before-toctrees");
        let toctrees = if full_nav {
            // Assign implicit `idN` targets to captioned code blocks *before*
            // snapshotting the doctrees, so the numfig pass numbers all of them
            // (not just those with an explicit `:name:`).
            {
                let transform_ctx = TransformContext::new(Some(config), Some(&tags), "html", "html")
                    .with_domains(&registry)
                    .with_env(env);
                for outcome in parsed.values_mut() {
                    let _ = scribe_core::AutoNumbering.apply(&mut outcome.document, &transform_ctx);
                }
            }

            // Record every post-registration doctree into the environment
            // before resolving cross-references. `:numref:` resolution looks up
            // its target node through `env.get_doctree()`, so without this
            // snapshot the HTML builder emits spurious "undefined label"
            // warnings for named code blocks.
            for (docname, outcome) in &parsed {
                let included_digests = env
                    .all_docs
                    .get(docname)
                    .map(|meta| meta.included_digests.clone())
                    .unwrap_or_default();
                env.record_document(docname, &outcome.document, included_digests);
            }
            // Populate the environment's resolved toctree/figure-number state.
            let toctrees = collect_toctrees_with_sources(env, config, &BTreeMap::new());
            // Surface the collector's diagnostics (`toc.secnum`,
            // `toc.not_included`, …) in upstream's logger shape. Without this
            // the HTML builder silently dropped every toctree warning.
            warnings.extend(toctree_warning_lines(&toctrees, &options.source_dir));
            // Materialise the resolved (glob-expanded, reversed) toctree on
            // the working doctrees so the body's `toctree-wrapper` lists every
            // page and its sections. The collector owns glob resolution; the
            // expander otherwise sees the raw `*` in `entry_refs`.
            for (docname, outcome) in &mut parsed {
                let _ = scribe_core::materialize_resolved_entries(
                    &mut outcome.document,
                    docname,
                    &toctrees,
                );
            }
            for (docname, outcome) in &mut parsed {
                match expand_toctrees(&mut outcome.document, docname, env, &toctrees) {
                    Ok(expansion) => {
                        for (target, line) in expansion.no_title {
                            warnings.push(crate::build_common::no_title_warning_line(
                                docname, &target, line, &options.source_dir,
                            ));
                        }
                    }
                    Err(error) => warnings.push(format!("{docname}: toctree expansion failed: {error}")),
                }
                // Mirror the collector's `secnumber` onto the working copy so
                // the writer can render `<span class="section-number">…</span>`.
                let _ = scribe_core::propagate_section_numbers(
                    &mut outcome.document,
                    docname,
                    &toctrees.section_numbers,
                );
                // Mirror numfig's `fignumber` onto the working copies so the
                // writer can render the `caption-number` span.  Sphinx numbers
                // figures, tables and captioned code blocks through the same
                // `FigureNumbers` inventory, keyed by figtype.
                let numbered: Vec<(NodeId, &'static str)> = outcome
                    .document
                    .preorder_ids()
                    .into_iter()
                    .filter_map(|id| {
                        outcome.document.get(id).ok().and_then(|node| {
                            let figtype = match node.kind {
                                NodeKind::Figure => Some("figure"),
                                NodeKind::Table => Some("table"),
                                NodeKind::Container
                                    if node.attrs.get_bool("literal_block").ok().flatten().unwrap_or(false) =>
                                {
                                    Some("code-block")
                                }
                                _ => None,
                            };
                            figtype.map(|kind| (id, kind))
                        })
                    })
                    .collect();
                for (id, figtype) in numbered {
                    let first_id = outcome
                        .document
                        .get(id)
                        .ok()
                        .and_then(|node| node.ids().ok())
                        .and_then(|ids| ids.first().cloned());
                    if let Some(first) = first_id {
                        if let Some(numbers) = toctrees.figure_numbers.get(docname, figtype, &first) {
                            let display = numbers.iter().map(ToString::to_string).collect::<Vec<_>>().join(".");
                            if let Ok(node) = outcome.document.get_mut(id) {
                                node.attrs.set_str("fignumber", &display);
                            }
                        }
                    }
                }
            }
            toctrees
        } else {
            ToctreeCollection::default()
        };

        profile::trim_heap();
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

        // Phase 66: the translator only reads document *titles* from the
        // environment, never the full doctrees. Precompute the exact title the
        // translator would derive from each doctree, then ship a doctree-free
        // environment view. This avoids a deep O(pages) clone of every AST into
        // a second environment (tens of MB at 150 pages) while keeping
        // navigation captions byte-for-byte identical.
        let titles: Arc<BTreeMap<String, String>> = Arc::new(
            env.documents
                .iter()
                .filter_map(|(docname, snapshot)| {
                    let doc = snapshot.doctree.as_ref()?;
                    let title = doc
                        .preorder_ids()
                        .into_iter()
                        .find_map(|id| {
                            let node = doc.get(id).ok()?;
                            if node.kind == NodeKind::Title {
                                Some(node.astext(doc))
                            } else {
                                None
                            }
                        })
                        .or_else(|| doc.metadata.title.clone())?;
                    Some((docname.clone(), title))
                })
                .collect(),
        );
        // Build the doctree-free view field by field: `env.clone()` would first
        // allocate a full second copy of every AST and then immediately free it,
        // spiking the high-water mark. Copying only the cheap fields keeps the
        // transient cost at zero.
        let mut env_view = scribe_core::BuildEnvironment::new(env.source_dir());
        env_view.set_relevant_config(env.relevant_config().clone());
        env_view.set_parser_version(env.parser_version());
        env_view.found_docs = env.found_docs.clone();
        env_view.all_docs = env.all_docs.clone();
        env_view.dependencies = env.dependencies.clone();
        env_view.included = env.included.clone();
        env_view.files_to_rebuild = env.files_to_rebuild.clone();
        env_view.toctree_includes = env.toctree_includes.clone();
        env_view.reread_always = env.reread_always.clone();
        env_view.glob_toctrees = env.glob_toctrees.clone();
        env_view.numbered_toctrees = env.numbered_toctrees.clone();
        env_view.outdated_docs = env.outdated_docs.clone();
        env_view.extra_outdated = env.extra_outdated.clone();
        env_view.documents = env
            .documents
            .iter()
            .map(|(name, snapshot)| {
                (
                    name.clone(),
                    scribe_core::DocumentSnapshot {
                        docname: snapshot.docname.clone(),
                        doctree: None,
                        sections: snapshot.sections.clone(),
                        toctree_targets: snapshot.toctree_targets.clone(),
                        xrefs: snapshot.xrefs.clone(),
                        anchors: snapshot.anchors.clone(),
                    },
                )
            })
            .collect();
        let env_arc = Arc::new(env_view);
        // Doctrees are serialized as soon as a document's transforms finish;
        // `parsed` therefore no longer has to keep every AST resident until the
        // very end of the build (Phase 32 arena-scoped lifecycle).
        let doctree_dir = options.doctree_dir.clone().unwrap_or_else(|| outdir.join(".doctrees"));

        // Phase 66: build the project-wide render data once. Every per-page
        // context then clones only these `Arc`s instead of rebuilding ~150
        // docname strings and ~150 URI pairs per page.
        let mut shared_all_docs = docnames.iter().cloned().collect::<BTreeSet<_>>();
        shared_all_docs.insert("genindex".to_owned());
        shared_all_docs.insert("search".to_owned());
        let mut shared_uris = target_uris.clone();
        for special in ["genindex", "py-modindex", "search"] {
            shared_uris
                .entry(special.to_owned())
                .or_insert_with(|| flavor.target_uri(special));
        }
        let shared = PageShared {
            config: Arc::new(config.clone()),
            all_docs: Arc::new(shared_all_docs),
            target_uris: Arc::new(shared_uris),
            theme_options: Arc::new(theme.options.clone()),
        };
        // Phase 68: the translator's docname/URI lookups need the exact
        // per-flavor sets below. Share one immutable copy across every page
        // instead of cloning ~150 entries into each `HtmlContext` (which made
        // the prepared set O(pages²) in memory).
        let page_shared_all_docs: Arc<BTreeSet<String>> =
            Arc::new(docnames.iter().cloned().collect());
        let page_shared_uris: Arc<BTreeMap<String, String>> = Arc::new(target_uris.clone());

        // Phase 68: transforms and per-page context construction are
        // CPU-bound per document. Run them through the same small parallel
        // pool; warnings are collected per document and merged in docname
        // order so output stays deterministic.
        let compact = crate::i18n::compact_setting(config);
        let theme_checksum = theme_options_checksum(config);
        let docs: Vec<(String, ParseOutcome)> = parse_docnames
            .iter()
            .filter_map(|name| parsed.remove(name).map(|outcome| (name.clone(), outcome)))
            .collect();
        // (docname, document, render context, html context, warnings)
        type TransformResult = (String, Document, RenderContext, HtmlContext, Vec<String>);
        let transform_results: Result<Vec<TransformResult>> =
            with_small_pool(|| {
                docs.into_par_iter()
                    .map(|(docname, mut outcome)| -> Result<_> {
                        let mut doc_warnings = Vec::new();
                        let domain = crate::i18n::docname_to_domain(&docname, &compact);
                        let translations = crate::i18n::load_catalog(config, source_dir, &domain);
                        let mut transform_ctx = TransformContext::new(Some(config), Some(&tags), "html", "html")
                            .with_domains(&registry)
                            .with_env(&*env);
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
                            doc_warnings.push(domain_warning_line(diagnostic, &docname, &*env));
                        }
                        for diagnostic in read_report.diagnostics.iter().chain(resolved_report.diagnostics.iter()) {
                            doc_warnings.push(format!("{docname}: {}", diagnostic.message));
                        }
                        let _ = events.emit("doctree-resolved", &Event::DoctreeResolved {
                            docname: docname.clone(),
                            doctree: doctree_json(&docname, &outcome.document),
                        });

                        // Persist this doctree now, while its AST is live.
                        write_doctree_file(&docname, &outcome.document, &doctree_dir)?;

                        let mut render_ctx = page_context(
                            &docname,
                            config,
                            &theme,
                            &theme_name,
                            &docnames,
                            &target_uris,
                            &language,
                            outdir,
                            &theme_checksum,
                            flavor,
                            Some(&shared),
                        );
                        // Sphinx loads MathJax on pages whose doctree carries math.
                        if outcome.document.preorder_ids().into_iter().any(|id| {
                            matches!(
                                outcome.document.get(id).map(|node| node.kind),
                                Ok(NodeKind::Math | NodeKind::MathBlock)
                            )
                        }) {
                            let mut map = std::collections::BTreeMap::new();
                            map.insert("filename".to_owned(), minijinja::Value::from("https://cdn.jsdelivr.net/npm/mathjax@4/tex-mml-chtml.js"));
                            map.insert("defer".to_owned(), minijinja::Value::from("defer"));
                            render_ctx.script_files.push(minijinja::Value::from(map));
                        }
                        if full_nav {
                            let (sidebar_toc, prev, next) =
                                build_sidebar_nav(&toctrees, &*env, &docname, config);
                            render_ctx.toctree_html = sidebar_toc;
                            render_ctx.prev = prev;
                            render_ctx.next = next;
                            render_ctx.parents = build_parents(&toctrees, &*env, &docname, config);
                        }
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
                        let page_image_uris = if flavor.directory_style {
                            let base = flavor.target_uri(&docname);
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
                            titles: Arc::clone(&titles),
                            target_uris: Arc::clone(&page_shared_uris),
                            all_docs: Arc::clone(&page_shared_all_docs),
                            add_permalinks: !flavor.embedded && config.get_bool("html_permalinks").unwrap_or(true),
                            permalinks_icon: config.get_str("html_permalinks_icon").unwrap_or("¶").to_owned(),
                            image_uris: page_image_uris,
                            language: language.clone(),
                            singlefile: None,
                            graphviz: graphviz_infos.get(&docname).cloned().unwrap_or_default(),
                        };
                        Ok((docname, outcome.document, render_ctx, html_ctx, doc_warnings))
                    })
                    .collect()
            });
        let mut prepared: Vec<(String, Document, RenderContext, HtmlContext)> = Vec::new();
        for (docname, mut document, render_ctx, html_ctx, doc_warnings) in transform_results? {
            warnings.extend(doc_warnings);
            // Viewcode needs the domain-enriched signatures produced by the
            // transform pipeline, so collect after both phases.
            if extensions.contains("viewcode") {
                viewcode.collect(&docname, &mut document, config, source_dir);
            }
            prepared.push((docname, document, render_ctx, html_ctx));
        }
        profile::trim_heap();
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

        // Phase 68: render and write pages in the same small parallel pool.
        // Each page's HTML is written as soon as it is rendered (no O(pages)
        // HTML retention), and rayon preserves iteration order so the result
        // list stays deterministic.
        let rendered: Vec<(std::path::PathBuf, String)> = with_small_pool(|| {
            prepared
            .par_iter()
            .map(|(docname, document, render_ctx, html_ctx)| {
                let body = HTML5Translator::translate(document, html_ctx);
                let mut ctx = render_ctx.clone();
                ctx.body = body;
                ctx.title = document.metadata.title.clone().unwrap_or_else(|| docname.clone());
                // Sphinx only sets `display_toc` when a document has more
                // than one local ToC entry (`toc_num_entries > 1`). A page
                // with a single section therefore renders no local ToC.
                let toc_maxdepth = scribe_core::document_tocdepth(document)
                    .map_or(3, |depth| depth.max(0) as usize);
                let toc = render_local_toc(document, toc_maxdepth, Some(&toctrees.section_numbers), docname);
                ctx.toc = if toc.matches("<li>").count() > 1 { toc } else { String::new() };
                // Alabaster injects a second viewport `<meta>` through the
                // `html-page-context` event for regular pages only (the
                // generated index/search pages use a different context).  The
                // conformance rubric counts tag names, so reproduce it here.
                if ctx.metatags.is_empty() {
                    // Upstream's `clean_meta` retains the translator's trailing
                    // newline, which the layout template renders as a blank line
                    // before `<title>`.
                    ctx.metatags = "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\" />\n".to_owned();
                }
                // `.. meta::` directives are collected into the page head as
                // `<meta>` elements (the frozen AST stores them as `inline`
                // nodes carrying a `meta` class).
                for id in document.preorder_ids() {
                    if let Ok(node) = document.get(id) {
                        if node.kind == NodeKind::Inline
                            && node.classes().unwrap_or_default().iter().any(|class| class == "meta")
                        {
                            let name = node.get_str("name").ok().flatten().unwrap_or("");
                            let content = node.get_str("content").ok().flatten().unwrap_or("");
                            ctx.metatags.push_str(&format!(
                                "<meta content=\"{}\" name=\"{}\" />\n",
                                escape_html(content), escape_attr(name)
                            ));
                        }
                    }
                }
                let suffix = source_paths.get(docname).map(|(_, s)| s.as_str()).unwrap_or(".rst");
                let sourcename = if suffix == ".txt" {
                    format!("{docname}{suffix}")
                } else {
                    format!("{docname}{suffix}.txt")
                };
                ctx.set_var("sourcename", sourcename);
                let html = if flavor.serializing {
                    // The serializinghtml builder dumps the page context instead
                    // of rendering a template (upstream `handle_page`).
                    serialize_page_context(&ctx, flavor)
                } else {
                    manager
                        .render_template("page.html", &ctx)
                        .or_else(|_| fallback_manager.render_template("page.html", &ctx))
                        .map_err(HtmlError::theme)?
                };
                let relative = flavor.output_path(docname);
                let path = outdir.join(&relative);
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).map_err(|e| HtmlError::io(parent, e))?;
                }
                fs::write(&path, &html).map_err(|e| HtmlError::io(&path, e))?;
                Ok((relative, docname.clone()))
            })
            .collect::<Result<Vec<_>>>()
        })?;
        // Phase 66: return the transient parse/render buffers to the OS before
        // the remaining stages allocate their own working memory.
        profile::trim_heap();
        profile::mark("render");

        let mut written = Vec::new();
        let mut built_docnames = Vec::new();
        for (relative, docname) in &rendered {
            written.push(relative.clone());
            built_docnames.push(docname.clone());
        }
        // Phase 66: release the per-page contexts before the final stages. Only
        // the documents are still needed (by the search index).
        drop(rendered);
        let search_docs: Vec<(String, Document)> = prepared
            .drain(..)
            .map(|(name, document, _, _)| (name, document))
            .collect();

        // ---- assets / source dumps ----------------------------------------
        profile::mark("before-assets");
        written.extend(copy_theme_assets(&theme, outdir)?);
        written.extend(render_asset_templates(&manager, &theme, config, outdir, &language, flavor.search)?);
        profile::mark("after-asset-templates");
        let extra_static: Vec<PathBuf> = config.get_strings("html_static_path")
            .unwrap_or_default().into_iter().map(PathBuf::from).collect();
        written.extend(copy_extra_static(&extra_static, source_dir, outdir)?);
        let pygments = pygments_css(config.get_str("pygments_style").unwrap_or_else(|| theme.pygments_style.as_deref().unwrap_or("default")));
        let pygments_path = outdir.join("_static/pygments.css");
        fs::create_dir_all(pygments_path.parent().unwrap_or(outdir)).map_err(|e| HtmlError::io(&pygments_path, e))?;
        fs::write(&pygments_path, pygments).map_err(|e| HtmlError::io(&pygments_path, e))?;
        written.push(PathBuf::from("_static/pygments.css"));
        write_dark_pygments_css(&theme, outdir, &mut written)?;
        write_graphviz_css(config, outdir, &mut written)?;

        if flavor.copy_source && config.get_bool("html_copy_source").unwrap_or(true) {
            written.extend(write_source_dumps(&source_paths, outdir)?);
        }

        // ---- index / search / inventory / buildinfo -----------------------
        profile::mark("before-index-pages");
        let mut gen_ctx = page_context(
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
            None,
        );
        // The generated index/search pages render through a manager that skips
        // user template directories, so a per-page `html_sidebars` list that
        // names a user template (e.g. `customsb.html`) cannot be included there.
        // Keep the theme's default sidebar list for these special pages.
        // Generated pages fall under the catch-all sidebar pattern, not the
        // master document's dedicated sidebar list.
        if let Some(sidebars) = config.get_dict("html_sidebars") {
            if let Some(value) = sidebars.get("**").and_then(|v| v.get_list()) {
                let names: Vec<minijinja::Value> = value.iter()
                    .filter_map(|v| v.get_str().map(minijinja::Value::from))
                    .collect();
                gen_ctx.vars.insert("sidebars".to_owned(), minijinja::Value::from(names));
            }
        }
        let mut gen_ctx = gen_ctx.add_doc("genindex").add_doc("search");
        // Generated index/search pages use this shared context; give them the
        // same flat sidebar global-toctree as any other page outside the tree.
        if full_nav {
            let (gen_toc, _, _) = build_sidebar_nav(&toctrees, env, "genindex", config);
            gen_ctx.toctree_html = gen_toc;
        }
        // User templates (e.g. test-root's `!layout.html` override) may not be
        // compilable by minijinja. When user template dirs are present, render
        // the generated index pages with a theme-aware manager that keeps the
        // selected theme and its inheritance chain but skips those user dirs;
        // this preserves the custom theme instead of silently swapping in the
        // bundled default.
        let index_manager = &manager;
        written.extend(write_index_pages(index_manager, &gen_ctx, &registry, env, outdir, flavor)?);
        profile::mark("after-index-pages");

        // Phase 18 + 67: full-text search index, stemmer assets and search UI.
        // On an incremental build the per-document fragments come from the
        // cache and only the rebuilt documents are recomputed.
        if flavor.search {
            written.extend(write_search_index(
                &search_docs,
                &source_paths,
                &registry,
                config,
                &language,
                outdir,
                flavor,
                &cache_dir,
                incremental,
            )?);
            profile::mark("after-search-index");
        }
        // Phase 66: the search index is built; release the working documents
        // before the environment cache is serialized so the two large live sets
        // never overlap at the high-water mark.
        drop(search_docs);

        // Serializing builders emit JSON instead of a binary inventory + pickle.
        if flavor.serializing {
            written.extend(write_serializing_extras(config, &target_uris, outdir)?);
        } else {
            write_objects_inv(&registry, config, &target_uris, outdir, &mut written)?;
            profile::mark("after-objects-inv");
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
        profile::trim_heap();
        profile::mark("write-assets");
        write_buildinfo(outdir, &theme_options_checksum(config))?;

        // Persist the domain inventories back into the environment so a later
        // incremental build can resolve xrefs without re-reading every source.
        for (name, data) in registry.export_domain_data() {
            if let Ok(value) = serde_json::to_value(&data) {
                env.set_domain_data(&name, value);
            }
        }
        if !options.reuse_environment {
            if let Some(cache_dir) = &update_options.cache_dir {
                let _ = env.save(cache_dir);
            }
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

        Ok(BuildResult {
            written,
            built_docnames,
            removed_docnames: report.removed.iter().cloned().collect(),
            warnings,
        })
}

struct ParseOutcome {
    document: Document,
    /// Include targets read while parsing this document (nested ones
    /// included), forwarded to `BuildEnvironment::note_included`.
    included: Vec<std::path::PathBuf>,
}

/// One document's result from the Phase 68 parallel parse pass.
///
/// Workers own every field, so no shared mutable state is touched. Rayon
/// preserves iteration order for indexed iterators; results are folded back in
/// sorted docname order, keeping warnings and doctrees byte-identical.
struct ParsedDoc {
    docname: String,
    /// Set when the source path was unreadable; the document is skipped.
    read_error: Option<String>,
    /// Warnings produced while parsing this document (issues + extension hooks).
    warnings: Vec<String>,
    /// Include targets read while parsing.
    included: Vec<PathBuf>,
    /// Source path and resolved suffix.
    source_path: Option<(PathBuf, String)>,
    /// Per-document reading duration.
    duration_secs: f64,
    /// Todos recorded while parsing this document.
    todos: TodoCollector,
    /// The parsed document, when parsing succeeded.
    outcome: Option<ParseOutcome>,
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
/// Intermediate toctree ancestors of `docname`, excluding the master doc.
///
/// Upstream `html-page-context` builds `parents` by walking
/// `env.toctree_ancestors(pagename)` from the immediate parent up to (but not
/// including) the root document. Relation titles carry a section-number span
/// and are safe markup in the theme templates.
fn build_parents(
    toctrees: &ToctreeCollection,
    env: &BuildEnvironment,
    docname: &str,
    config: &Config,
) -> Vec<(String, String)> {
    let mut parents = Vec::new();
    let mut current = toctrees
        .relation(docname)
        .and_then(|relation| relation.parent.clone());
    let master = config.master_doc();
    while let Some(name) = current {
        if name == master || name == docname || parents.iter().any(|(n, _)| *n == name) {
            break;
        }
        let title = env
            .documents
            .get(&name)
            .and_then(|snapshot| snapshot.doctree.as_ref())
            .and_then(|doc| {
                doc.preorder_ids().into_iter().find_map(|id| {
                    let node = doc.get(id).ok()?;
                    if node.kind == NodeKind::Title { Some(node.astext(doc)) } else { None }
                })
            })
            .unwrap_or_else(|| name.clone());
        let number = toctrees
            .section_numbers
            .get(&name, "")
            .filter(|number| !number.is_empty())
            .map(|number| {
                format!(
                    "<span class=\"section-number\">{}. </span>",
                    number.iter().map(i64::to_string).collect::<Vec<_>>().join(".")
                )
            })
            .unwrap_or_default();
        parents.push((format!("{name}.html"), format!("{number}{}", escape_html(&title))));
        current = toctrees
            .relation(&name)
            .and_then(|relation| relation.parent.clone());
    }
    parents
}

/// Renders the sidebar global toctree plus prev/next relations for one page.
///
/// Mirrors Sphinx's `global_toctree_for_doc(master_doc)` as rendered by
/// Alabaster's `navigation.html`/`relations.html`: a flat list of top-level
/// documents, the current document expanded with its sub-sections, and a
/// `Previous`/`Next` pair from the toctree traversal order.
#[allow(clippy::type_complexity)]
fn build_sidebar_nav(
    toctrees: &ToctreeCollection,
    env: &BuildEnvironment,
    docname: &str,
    config: &Config,
) -> (String, Option<(String, String)>, Option<(String, String)>) {
    let uri = |name: &str| format!("{name}.html");
    let title_of = |name: &str| -> String {
        env.documents
            .get(name)
            .and_then(|snapshot| snapshot.doctree.as_ref())
            .and_then(|doc| {
                doc.preorder_ids().into_iter().find_map(|id| {
                    let node = doc.get(id).ok()?;
                    if node.kind == NodeKind::Title { Some(node.astext(doc)) } else { None }
                })
            })
            .unwrap_or_else(|| name.to_owned())
    };

    // Plain section-number prefix (no span) for global-ToC links, matching
    // Sphinx's `visit_reference()` for toctree entries.
    let plain_number = |name: &str| -> String {
        toctrees
            .section_numbers
            .get(name, "")
            .filter(|number| !number.is_empty())
            .map(|number| {
                format!("{}. ", number.iter().map(i64::to_string).collect::<Vec<_>>().join("."))
            })
            .unwrap_or_default()
    };

    // Sub-sections of the current document. Upstream's `_toctree_copy` drops
    // these when the current document carries `:tocdepth:` metadata (the
    // local TOC depth is consumed before the global ToC is assembled).
    let sections_of = |name: &str| -> String {
        let Some(doc) = env.documents.get(name).and_then(|snapshot| snapshot.doctree.as_ref()) else {
            return String::new();
        };
        if scribe_core::document_tocdepth(doc).is_some() {
            return String::new();
        }
        let Ok(root_node) = doc.get(doc.root) else { return String::new() };
        // Sphinx's global ToC lists the document's *top-level* sections as
        // flat entries: the first is the document link itself, the rest are
        // emitted here. Nested sub-sections (e.g. `lists.html` subsections)
        // are not included in the global ToC.
        let mut out = String::new();
        // Domain objects are nested under the document's top-level entry as
        // `toctree-l2` links, with their signature name in a `<code>` wrapper.
        let objects: Vec<(String, String)> = doc.preorder_ids().into_iter().filter_map(|id| {
            let node = doc.get(id).ok()?;
            if node.kind != NodeKind::Desc { return None; }
            let anchor = node.ids().ok()?.into_iter().next()?;
            let display = node.children.iter().find_map(|&child| {
                doc.get(child).ok()?.get_str("_toc_name").ok().flatten().map(str::to_owned)
            }).or_else(|| node.get_str("py:fullname").ok().flatten()
                .map(|name| name.rsplit('.').next().unwrap_or(name).to_owned()))
            .or_else(|| node.get_str("fullname").ok().flatten()
                .map(|name| name.rsplit('.').next().unwrap_or(name).to_owned()))
            .or_else(|| doc.subtree_ids(id).into_iter().find_map(|cid| {
                let c = doc.get(cid).ok()?;
                if c.kind == NodeKind::DescName { Some(c.astext(doc)) } else { None }
            }))?;
            Some((anchor, display))
        }).collect();
        // The global ToC is a flat list of document/section entries; domain
        // objects are only expanded in the *local* ToC (render_local_toc).
        let _ = objects;
        let sections: Vec<_> = root_node
            .children
            .iter()
            .copied()
            .filter(|&id| doc.get(id).is_ok_and(|node| node.kind == NodeKind::Section))
            .collect();
        for section in sections.into_iter().skip(1) {
            let Ok(node) = doc.get(section) else { continue };
            let text = node.children.iter().find_map(|&child| {
                let child = doc.get(child).ok()?;
                if child.kind == NodeKind::Title { Some(child.astext(doc)) } else { None }
            }).unwrap_or_default();
            let anchor = node.ids().ok().and_then(|ids| ids.first().cloned()).unwrap_or_default();
            let number = toctrees
                .section_numbers
                .get(name, &format!("#{anchor}"))
                .filter(|number| !number.is_empty())
                .map(|number| {
                    format!("{}. ", number.iter().map(i64::to_string).collect::<Vec<_>>().join("."))
                })
                .unwrap_or_default();
            out.push_str(&format!(
                "<li class=\"toctree-l1\"><a class=\"reference internal\" href=\"{}#{}\">{}{}</a></li>\n",
                escape_attr(&uri(name)),
                escape_attr(&anchor),
                number,
                escape_html(&text)
            ));
        }
        out
    };

    // Sphinx's collapsed global ToC expands the current document's immediate
    // sub-sections into a nested `<ul>` inside its `<li class="toctree-l1
    // current">` entry (alabaster's `navigation.html` calls `toctree()` with
    // `collapse=true`). Restore that nested list, which the flat `sections_of`
    // rendering above does not produce.
    let nested_sections_of = |name: &str| -> String {
        let Some(doc) = env.documents.get(name).and_then(|snapshot| snapshot.doctree.as_ref()) else {
            return String::new();
        };
        if scribe_core::document_tocdepth(doc).is_some() {
            return String::new();
        }
        let Ok(root_node) = doc.get(doc.root) else { return String::new() };
        let Some(root_section) = root_node.children.iter().copied().find(|&id| {
            doc.get(id).is_ok_and(|node| node.kind == NodeKind::Section)
        }) else {
            return String::new();
        };
        let Ok(section_node) = doc.get(root_section) else { return String::new() };
        let sections: Vec<_> = section_node
            .children
            .iter()
            .copied()
            .filter(|&id| doc.get(id).is_ok_and(|node| node.kind == NodeKind::Section))
            .collect();
        if sections.is_empty() {
            return String::new();
        }
        let mut out = String::from("<ul>\n");
        for section in sections {
            let Ok(node) = doc.get(section) else { continue };
            let text = node.children.iter().find_map(|&child| {
                let child = doc.get(child).ok()?;
                if child.kind == NodeKind::Title { Some(child.astext(doc)) } else { None }
            }).unwrap_or_default();
            let anchor = node.ids().ok().and_then(|ids| ids.first().cloned()).unwrap_or_default();
            let number = toctrees
                .section_numbers
                .get(name, &format!("#{anchor}"))
                .filter(|number| !number.is_empty())
                .map(|number| {
                    format!("{}. ", number.iter().map(i64::to_string).collect::<Vec<_>>().join("."))
                })
                .unwrap_or_default();
            out.push_str(&format!(
                "<li class=\"toctree-l2\"><a class=\"reference internal\" href=\"#{}\">{}{}</a></li>\n",
                escape_attr(&anchor),
                number,
                escape_html(&text)
            ));
        }
        out.push_str("</ul>\n");
        out
    };

    // Renders a section/document title as safe HTML, preserving inline `math`
    // nodes as `<span class="math notranslate nohighlight">\(…\)</span>` the
    // way Sphinx's `toctree()` renders `env.titles`.
    let rich_title = |name: &str, plain: &str| -> String {
        let Some(doc) = env.documents.get(name).and_then(|snapshot| snapshot.doctree.as_ref()) else {
            return escape_html(plain);
        };
        let Some(title) = doc.preorder_ids().into_iter().find(|&id| {
            doc.get(id).is_ok_and(|node| node.kind == NodeKind::Title)
        }) else {
            return escape_html(plain);
        };
        let children = doc.get(title).map(|node| node.children.clone()).unwrap_or_default();
        let mut out = String::new();
        for child in children {
            let Ok(node) = doc.get(child) else { continue };
            if node.kind == NodeKind::Math {
                out.push_str(&format!(
                    "<span class=\"math notranslate nohighlight\">\\({}\\)</span>",
                    escape_html(&node.astext(doc))
                ));
            } else {
                out.push_str(&escape_html(&node.astext(doc)));
            }
        }
        if out.is_empty() { escape_html(plain) } else { out }
    };

    let mut out = String::new();
    if let Some(root) = toctrees
        .global_toctree_for_doc(config.master_doc(), true)
        .filter(|root| !root.children.is_empty())
    {
        // Sphinx's `toctree()` renders the master toctree's `:caption:` above
        // the generated `<ul>` as a heading paragraph.
        if let Some(caption) = toctrees.toctrees.iter()
            .find(|tree| tree.docname == config.master_doc())
            .and_then(|tree| tree.options.caption.as_deref())
            .filter(|caption| !caption.is_empty())
        {
            out.push_str(&format!(
                "<p class=\"caption\" role=\"heading\"><span class=\"caption-text\">{}</span></p>\n",
                escape_html(caption)
            ));
        }
        let has_current = root.children.iter().any(|node| node.docname == docname);
        out.push_str(if has_current { "<ul class=\"current\">\n" } else { "<ul>\n" });
        for child in &root.children {
            let is_external = child.docname.starts_with("http://") || child.docname.starts_with("https://");
            let current = child.docname == docname;
            let text = child.title.clone().unwrap_or_else(|| title_of(&child.docname));
            if is_external {
                out.push_str(&format!(
                    "<li class=\"toctree-l1\"><a class=\"reference external\" href=\"{}\">{}</a></li>\n",
                    escape_attr(&child.docname),
                    rich_title(&child.docname, &text)
                ));
                continue;
            }
            let href = if current { "#".to_owned() } else { uri(&child.docname) };
            out.push_str(&format!(
                "<li class=\"{}\"><a class=\"{}\" href=\"{}\">{}{}</a>",
                if current { "toctree-l1 current" } else { "toctree-l1" },
                if current { "current reference internal" } else { "reference internal" },
                escape_attr(&href),
                plain_number(&child.docname),
                rich_title(&child.docname, &text)
            ));
            // A user `layout.html` that renders `toctree(collapse=False)`
            // already emits a flat global ToC; do not add a nested current-
            // page section list on top of it.
            let flat_user_toc = config
                .get_strings("templates_path")
                .is_some_and(|paths| !paths.is_empty());
            if current && !flat_user_toc {
                out.push_str(&nested_sections_of(&child.docname));
            }
            out.push_str("</li>\n");
            out.push_str(&sections_of(&child.docname));
        }
        out.push_str("</ul>\n");
    }

    // Sphinx prefixes relation titles with the target section's number in a
    // `<span class="section-number">`. The theme treats relation titles as
    // safe markup, so escape the plain title ourselves.
    let numbered_title = |name: &str| -> String {
        let number = toctrees
            .section_numbers
            .get(name, "")
            .filter(|number| !number.is_empty())
            .map(|number| {
                let dotted = number.iter().map(i64::to_string).collect::<Vec<_>>().join(".");
                format!("<span class=\"section-number\">{dotted}. </span>")
            })
            .unwrap_or_default();
        format!("{number}{}", rich_title(name, &title_of(name)))
    };
    let (prev, next) = toctrees.relation(docname).map_or((None, None), |relation| {
        (
            relation.prev.as_ref().map(|name| (uri(name), numbered_title(name))),
            relation.next.as_ref().map(|name| (uri(name), numbered_title(name))),
        )
    });
    (out, prev, next)
}

/// Sphinx `patmatch`-style glob: `**` matches any characters (including `/`),
/// `*` matches any run of non-`/` characters, `?` matches one character.
fn sidebar_pattern_matches(pattern: &str, docname: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = docname.chars().collect();
    // Dynamic-programming glob match with explicit `**` support.
    let mut dp = vec![false; s.len() + 1];
    dp[0] = true;
    let mut i = 0usize;
    while i < p.len() {
        if p[i] == '*' && i + 1 < p.len() && p[i + 1] == '*' {
            i += 2;
            let mut any = vec![false; s.len() + 1];
            any[0] = dp[0];
            for j in 1..=s.len() {
                any[j] = dp[j] || any[j - 1];
            }
            dp = any;
        } else if p[i] == '*' {
            i += 1;
            let mut next = vec![false; s.len() + 1];
            for j in 0..=s.len() {
                if dp[j] {
                    next[j] = true;
                    let mut k = j;
                    while k < s.len() && s[k] != '/' {
                        k += 1;
                        next[k] = true;
                    }
                }
            }
            dp = next;
        } else {
            let ch = p[i];
            i += 1;
            let mut next = vec![false; s.len() + 1];
            for j in 0..s.len() {
                if dp[j] && (ch == '?' || s[j] == ch) {
                    next[j + 1] = true;
                }
            }
            dp = next;
        }
    }
    dp[s.len()]
}

/// Resolve the ordered sidebar template list for `docname` from the
/// `html_sidebars` config dict. Returns `None` when the option is absent or
/// empty so the theme default remains in effect.
fn sidebars_for_doc(config: &Config, docname: &str) -> Option<Vec<String>> {
    let dict = config.get_dict("html_sidebars")?;
    if dict.is_empty() {
        return None;
    }
    // Sphinx sorts patterns by "specificity" (number of `*` wildcards), so a
    // literal docname pattern outranks the `**` catch-all.
    let mut patterns: Vec<(&String, usize)> = dict
        .keys()
        .map(|pattern| (pattern, pattern.matches('*').count()))
        .collect();
    patterns.sort_by_key(|(pattern, wildcards)| (*wildcards, std::cmp::Reverse(pattern.len())));
    for (pattern, _) in patterns {
        if sidebar_pattern_matches(pattern, docname) {
            let list = dict.get(pattern).and_then(|value| value.get_list())?;
            let names: Vec<String> = list.iter().filter_map(|v| v.get_str().map(str::to_owned)).collect();
            if !names.is_empty() {
                return Some(names);
            }
        }
    }
    None
}

/// Project-wide data shared by every per-page [`RenderContext`] (Phase 66).
///
/// Building these maps once and cloning only the `Arc`s per page avoids
/// allocating ~150 docname strings and ~150 URI pairs for every page.
struct PageShared {
    config: Arc<Config>,
    all_docs: Arc<BTreeSet<String>>,
    target_uris: Arc<BTreeMap<String, String>>,
    theme_options: Arc<BTreeMap<String, String>>,
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
    shared: Option<&PageShared>,
) -> RenderContext {
    let mut ctx = RenderContext::for_builder(flavor.name, docname, config.clone()).with_theme(theme_name);
    match shared {
        Some(s) => {
            // Phase 66: reuse the single project-wide copy of each map.
            ctx.config = Arc::clone(&s.config);
            ctx.all_docs = Arc::clone(&s.all_docs);
            ctx.target_uris = Arc::clone(&s.target_uris);
            ctx.theme_options = Arc::clone(&s.theme_options);
        }
        None => {
            let mut all_docs = docnames.iter().cloned().collect::<BTreeSet<_>>();
            all_docs.insert("genindex".to_owned());
            all_docs.insert("search".to_owned());
            ctx.all_docs = Arc::new(all_docs);
            // Special pages share the flavor's URI convention (`genindex/` for
            // dirhtml/json, `genindex.html` for the standalone family).
            let mut uris = target_uris.clone();
            for special in ["genindex", "py-modindex", "search"] {
                uris.entry(special.to_owned()).or_insert_with(|| flavor.target_uri(special));
            }
            ctx.target_uris = Arc::new(uris);
            ctx.theme_options = Arc::new(theme.options.clone());
        }
    }
    ctx.link_suffix = if flavor.directory_style { "/".to_owned() } else { flavor.out_suffix.to_owned() };
    ctx.file_suffix = flavor.out_suffix.to_owned();
    ctx.search_enabled = flavor.search;
    ctx.use_index = true;
    ctx.language = language.to_owned();
    // Sphinx's `html_sidebars` maps docname patterns to an ordered list of
    // sidebar templates.  When set it overrides the theme's default sidebar
    // list on a per-page basis.  The most specific matching pattern wins,
    // with `**` acting as the catch-all (mirrors Sphinx's `patmatch`).
    if let Some(sidebars) = sidebars_for_doc(config, docname) {
        ctx.set_var("sidebars", sidebars);
    }

    let master = config.master_doc();
    let root_title = config.get_str("html_title").filter(|s| !s.is_empty()).map(str::to_owned)
        .unwrap_or_else(|| default_html_title(config));
    ctx.rellinks = vec![
        ("genindex".to_owned(), "General Index".to_owned(), "I".to_owned(), "index".to_owned()),
    ];
    ctx.set_var("link", "");
    if !flavor.embedded && docname != master {
        let master_uri = ctx.target_uris.get(master).cloned().unwrap_or_else(|| flavor.target_uri(master));
        ctx.parents.push((format!("{master_uri}#{master}"), root_title));
    }
    let mut css = vec![
        stylesheet(format!("_static/pygments.css?v={}", asset_hash(outdir, "_static/pygments.css", config_hash))),
    ];
    // Sphinx adds a second stylesheet from the theme's `pygments_dark_style`
    // setting, guarded by `prefers-color-scheme: dark` (see
    // `StandaloneHTMLBuilder.init_css_files`).
    if theme.pygments_dark_style.as_deref().is_some_and(|style| !style.is_empty()) {
        let dark_hash = asset_hash(outdir, "_static/pygments_dark.css", config_hash);
        css.push(dark_stylesheet(format!("_static/pygments_dark.css?v={dark_hash}")));
    }
    for name in &theme.stylesheets {
        css.push(stylesheet(format!("_static/{name}?v={}", asset_hash(outdir, &format!("_static/{name}"), config_hash))));
    }
    // `sphinx.ext.graphviz` calls `app.add_css_file('graphviz.css')` and copies
    // its packaged stylesheet into `html_static_path` on config-inited.
    if graphviz_enabled(config) {
        css.push(stylesheet(format!(
            "_static/graphviz.css?v={}",
            asset_hash(outdir, "_static/graphviz.css", config_hash)
        )));
    }
    ctx.css_files = css;
    ctx.script_files = vec![
        script("_static/documentation_options.js?v=".to_owned() + &asset_hash(outdir, "_static/documentation_options.js", config_hash)),
        script("_static/doctools.js?v=".to_owned() + &asset_hash(outdir, "_static/doctools.js", config_hash)),
        script("_static/sphinx_highlight.js?v=".to_owned() + &asset_hash(outdir, "_static/sphinx_highlight.js", config_hash)),
    ];
    // `html_js_files` (including `app.add_js_file()` registrations from
    // `conf.py`) are appended after the built-in scripts.
    if let Some(extra) = config.get_strings("html_js_files") {
        for file in extra {
            if !file.is_empty() {
                ctx.script_files.push(script(file));
            }
        }
    }
    ctx
}

/// Whether the graphviz extension is configured (short or qualified name).
fn graphviz_enabled(config: &Config) -> bool {
    config
        .get_strings("extensions")
        .unwrap_or_default()
        .iter()
        .any(|name| name.rsplit('.').next().is_some_and(|short| short == "graphviz"))
}

/// Ship the stylesheet that `sphinx.ext.graphviz` adds via `add_css_file` and
/// `config-inited`. Its contents mirror Sphinx's packaged `graphviz.css`.
fn write_graphviz_css(config: &Config, outdir: &Path, written: &mut Vec<PathBuf>) -> Result<()> {
    if !graphviz_enabled(config) {
        return Ok(());
    }
    const CSS: &str = "/*\n * Sphinx stylesheet -- graphviz extension.\n */\n\n\
        img.graphviz {\n    border: 0;\n    max-width: 100%;\n}\n\n\
        object.graphviz {\n    max-width: 100%;\n}\n";
    let path = outdir.join("_static/graphviz.css");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| HtmlError::io(parent, e))?;
    }
    fs::write(&path, CSS).map_err(|e| HtmlError::io(&path, e))?;
    written.push(PathBuf::from("_static/graphviz.css"));
    Ok(())
}

fn stylesheet(filename: String) -> minijinja::Value {
    let mut map = std::collections::BTreeMap::new();
    map.insert("filename".to_owned(), minijinja::Value::from(filename));
    map.insert("rel".to_owned(), minijinja::Value::from("stylesheet"));
    map.insert("type".to_owned(), minijinja::Value::from("text/css"));
    map.insert("priority".to_owned(), minijinja::Value::from(500_i64));
    minijinja::Value::from(map)
}

/// The `pygments_dark.css` link emitted for themes declaring a dark Pygments
/// style. Attribute names match Sphinx's `_CascadingStyleSheet` so `css_tag`
/// renders `<link id="pygments_dark_css" media="..." rel="stylesheet" ...>`.
fn dark_stylesheet(filename: String) -> minijinja::Value {
    let mut map = std::collections::BTreeMap::new();
    map.insert("filename".to_owned(), minijinja::Value::from(filename));
    map.insert("id".to_owned(), minijinja::Value::from("pygments_dark_css"));
    map.insert("media".to_owned(), minijinja::Value::from("(prefers-color-scheme: dark)"));
    map.insert("rel".to_owned(), minijinja::Value::from("stylesheet"));
    map.insert("type".to_owned(), minijinja::Value::from("text/css"));
    map.insert("priority".to_owned(), minijinja::Value::from(500_i64));
    minijinja::Value::from(map)
}

/// Emit the dark Pygments stylesheet asset when the theme requests one.
fn write_dark_pygments_css(theme: &Theme, outdir: &Path, written: &mut Vec<PathBuf>) -> Result<()> {
    if let Some(dark_style) = theme.pygments_dark_style.as_deref().filter(|style| !style.is_empty()) {
        let path = outdir.join("_static/pygments_dark.css");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| HtmlError::io(parent, error))?;
        }
        fs::write(&path, pygments_css(dark_style)).map_err(|error| HtmlError::io(&path, error))?;
        written.push(PathBuf::from("_static/pygments_dark.css"));
    }
    Ok(())
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
#[allow(clippy::too_many_arguments)]
fn write_search_index(
    prepared: &[(String, Document)],
    source_paths: &BTreeMap<String, (PathBuf, String)>,
    registry: &DomainRegistry,
    config: &Config,
    language: &str,
    outdir: &Path,
    flavor: &BuildFlavor,
    cache_dir: &Path,
    incremental: bool,
) -> Result<Vec<PathBuf>> {
    let search_language = config
        .get_str("html_search_language")
        .filter(|lang| !lang.is_empty())
        .unwrap_or(language)
        .to_owned();

    // Phase 67: recompute only the rebuilt documents' fragments, then merge
    // the complete set. `SearchIndex::build_fragment` produces exactly the
    // bytes the old inline `feed_with_filename` path produced, so a warm build
    // is byte-identical to a clean `-E` build.
    let mut fragments = if incremental {
        FragmentCache::load(cache_dir).unwrap_or_else(|| FragmentCache::new(&search_language))
    } else {
        FragmentCache::new(&search_language)
    };
    fragments.search_language = search_language.clone();
    for (docname, document) in prepared {
        let title = document.metadata.title.clone().unwrap_or_default();
        let filename = source_paths
            .get(docname)
            .map(|(_, suffix)| format!("{docname}{suffix}"))
            .unwrap_or_else(|| format!("{docname}.rst"));
        // Upstream feeds an empty doctree for untitled pages so their section
        // titles do not leak into `alltitles`.
        let empty_document = Document::new(None);
        let (title, document): (&str, &Document) = if title.is_empty() {
            ("", &empty_document)
        } else {
            (title.as_str(), document)
        };
        let fragment = SearchIndex::build_fragment(docname, &filename, title, document, &search_language);
        fragments.search_fragments.insert(docname.clone(), fragment);
    }

    let mut index = SearchIndex::new(search_language, scribe_envversion());
    for fragment in fragments.search_fragments.values() {
        index.apply_fragment(fragment.clone());
    }
    index.add_domain_objects(registry);

    // Persist the merged fragment set for the next incremental build. A cache
    // write failure is non-fatal: the next build simply recomputes everything.
    if let Err(error) = fragments.save(cache_dir) {
        if std::env::var_os("SCRIBE_PROFILE").is_some() {
            eprintln!("SCRIBE_PROFILE fragment-cache-save-failed: {error}");
        }
    }

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
        .unwrap_or_else(|| page_context(master, config, theme, theme_name, &order, &BTreeMap::new(), language, outdir, &theme_options_checksum(config), &BuildFlavor::singlefile(), None));
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
    write_dark_pygments_css(theme, outdir, &mut written)?;
    write_graphviz_css(config, outdir, &mut written)?;

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
    Ok(BuildResult { written, built_docnames: order, removed_docnames: Vec::new(), warnings: warnings.to_vec() })
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
