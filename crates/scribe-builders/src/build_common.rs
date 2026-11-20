//! Shared "read phase" for the Phase 24 output builders.
//!
//! `epub`, `man`, `texinfo` and `text` all need the same preparation before
//! they can render: refresh the build environment, parse and resolve every
//! source, register domain objects, apply the read/resolved transform passes
//! and collect toctree navigation/numbering. This module factors that pipeline
//! out once so the four writers only implement rendering + file emission.
//!
//! It deliberately mirrors the corresponding sections of the Phase 17 HTML
//! builder and Phase 23 LaTeX builder (same registries, same config keys) but
//! keeps the new builders decoupled from those writers' public types.

use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;

use rayon::prelude::*;
use scribe_ast::Document;
use scribe_config::{Config, Tags};
use scribe_core::{
    absolute_source_path, apply_transforms, canonical_code, collect_toctrees_with_sources,
    duplicate_display_code, extensions, format_warning, missing_reference_display_code,
    ActivatedExtensions, BuildEnvironment, Event, InteropBridge, Phase, ToctreeCollection,
    TransformContext, UpdateOptions, WarningCode, WarningLocation,
};
use scribe_domains::{
    fetch_all, prepare_document, diagnostics::DomainDiagnostic, DomainRegistry, FetchOptions,
    IntersphinxMapping, IntersphinxResolver,
};
use scribe_rst::{
    parse_with_options, resolve_output, Context as RstContext, Directive, DirectiveContext,
    DirectiveError, DirectiveRegistry, ParseOptions,
};

use crate::error::{OutputError, OutputResult};
use crate::html::BuildOptions;
use crate::register_code_directives;

/// Everything the renderers need after the shared read phase.
pub struct PreparedBuild {
    /// Selected documents in deterministic (sorted) order.
    pub docnames: Vec<String>,
    /// Parsed, resolved and transformed doctrees keyed by docname.
    pub parsed: BTreeMap<String, Document>,
    /// Toctree hierarchy, section numbering and navigation relations.
    pub toctrees: ToctreeCollection,
    /// Non-fatal diagnostics gathered while preparing.
    pub warnings: Vec<String>,
}

/// Render toctree diagnostics in the shape upstream's logger emits.
///
/// Upstream logs these without a single source line (a toctree warning has no
/// one offending line), as `<source>/<doc>.rst: WARNING: <msg> [<code>]`.
/// Scribe's internal representation carries only a docname, so we reconstruct
/// the path from the build's source directory. This makes the normalized
/// warning lines used by the conformance harness comparable with the oracle.
pub fn toctree_warning_lines(toctrees: &ToctreeCollection, source_dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    // Upstream canonicalizes the source directory before logging, resolving
    // any `..` components. Mirror that so the emitted path is byte-identical
    // to the oracle's after the harness normalizes absolute paths.
    let base = source_dir.canonicalize().unwrap_or_else(|_| source_dir.to_path_buf());
    for warning in &toctrees.warnings {
        if warning.code == "scribe::config::master_doc_fallback" {
            // Upstream logs this from config validation with no source
            // location and no warning code; the message already carries the
            // `WARNING: ` prefix, so emit it verbatim.
            out.push(warning.message.clone());
            continue;
        }
        let (canonical, message, docname) = match warning.code {
            "scribe::toc::not_included" => {
                // Upstream's exact text has no trailing `: <docname>`.
                ("toc.not_included", "document isn't included in any toctree".to_owned(), warning.source_name.as_str())
            }
            "scribe::toc::nested_numbered" | "scribe::toc::secnum" => {
                ("toc.secnum", warning.message.clone(), warning.source_name.as_str())
            }
            "scribe::toc::circular" => ("toc.circular", warning.message.clone(), warning.source_name.as_str()),
            "scribe::toc::empty_glob" => ("toc.empty_glob", warning.message.clone(), warning.source_name.as_str()),
            "scribe::toc::duplicate_entry" => ("toc.duplicate_entry", warning.message.clone(), warning.source_name.as_str()),
            "scribe::toc::missing_document" => ("toc.not_readable", warning.message.clone(), warning.source_name.as_str()),
            "scribe::toc::not_readable" => ("toc.not_readable", warning.message.clone(), warning.source_name.as_str()),
            "scribe::toc::excluded" => ("toc.excluded", warning.message.clone(), warning.source_name.as_str()),
            _ => ("toc", warning.message.clone(), warning.source_name.as_str()),
        };
        let suffix = if docname.ends_with(".md") { "" } else { ".rst" };
        let path = base.join(format!("{docname}{suffix}"));
        // Located toctree warnings (notably `toc.secnum`) carry the 1-based
        // line of the offending directive; family warnings such as
        // `toc.not_included` are logged without one upstream.
        let location = match warning.line {
            Some(line) => format!("{}:{line}", path.display()),
            None => path.display().to_string(),
        };
        out.push(format!("{location}: WARNING: {message} [{canonical}]"));
    }
    out
}

/// Render the upstream `toc.no_title` diagnostic for a toctree entry whose
/// target document carries no title. Upstream keeps the target in
/// `includefiles` (so no `toc.not_included` warning is produced) but emits no
/// link; the warning is located on the toctree directive itself.
pub fn no_title_warning_line(
    source_docname: &str,
    target_docname: &str,
    line: Option<u32>,
    source_dir: &std::path::Path,
) -> String {
    let base = source_dir.canonicalize().unwrap_or_else(|_| source_dir.to_path_buf());
    let suffix = if source_docname.ends_with(".md") { "" } else { ".rst" };
    let path = base.join(format!("{source_docname}{suffix}"));
    let location = match line {
        Some(line) => format!("{}:{line}", path.display()),
        None => path.display().to_string(),
    };
    format!(
        "{location}: WARNING: toctree contains reference to document '{target_docname}' that doesn't have a title: no link will be generated [toc.no_title]"
    )
}

/// Canonical Sphinx warning code for a structured domain diagnostic.
///
/// Upstream logs unresolved xrefs as `ref.<reftype>` (see
/// `sphinx/transforms/post_transforms/__init__.py`), not as the documented
/// `ref.python` suppression family. The other branches mirror the concrete
/// `type`/`subtype` keywords used by upstream's domain loggers.
pub fn diagnostic_code(diagnostic: &DomainDiagnostic) -> WarningCode {
    let message = &diagnostic.message;
    if diagnostic.is_missing_reference() {
        return missing_reference_display_code(diagnostic.typ.as_deref().unwrap_or(""));
    }
    if message.contains("more than one target found for 'any'") {
        return WarningCode::typed("ref", "any");
    }
    if message.contains("more than one target found for cross-reference") {
        return WarningCode::typed("ref", "python");
    }
    if diagnostic.code == scribe_domains::diagnostics::codes::DUPLICATE_OBJECT {
        if message.contains("C++") {
            return duplicate_display_code("cpp");
        }
        if message.contains("C declaration") {
            return duplicate_display_code("c");
        }
        return duplicate_display_code("");
    }
    canonical_code(&diagnostic.code).unwrap_or_else(|| WarningCode::bare("misc"))
}

/// Formats one structured domain diagnostic as an upstream-style warning line.
///
/// The source path is resolved from the build environment (which knows the
/// real source file for `docname`) and canonicalized to the absolute form
/// upstream logs for node locations. The 1-based line comes from the AST node
/// carried on the diagnostic by the span plumbing.
pub fn domain_warning_line(
    diagnostic: &DomainDiagnostic,
    docname: &str,
    env: &BuildEnvironment,
) -> String {
    // Upstream's numfig diagnostics pass no `type`/`subtype` to
    // `logger.warning`, so their log lines carry no `[code]` suffix.
    if diagnostic.code == scribe_domains::diagnostics::codes::NUMFIG {
        return bare_domain_warning_line(diagnostic, docname, env);
    }
    let path = env
        .doc2path(docname)
        .map(absolute_source_path)
        .or_else(|| {
            diagnostic
                .source_path
                .as_deref()
                .map(|path| absolute_source_path(std::path::Path::new(path)))
        });
    let location = path.map(|path| WarningLocation::new(Some(path), diagnostic.line));
    format_warning(&diagnostic.message, &diagnostic_code(diagnostic), location.as_ref())
}

/// True for standard-domain `note_object` duplicate messages, which upstream
/// logs without a `type`/`subtype` (hence no `[code]` suffix). The JS domain's
/// spelling uses "other <type> in", not "other instance in", so it is excluded.
fn is_bare_duplicate_message(message: &str) -> bool {
    message.starts_with("duplicate ")
        && message.contains(" description of ")
        && message.contains(", other instance in ")
}

/// Formats a domain diagnostic that upstream logs with no warning code.
fn bare_domain_warning_line(
    diagnostic: &DomainDiagnostic,
    docname: &str,
    env: &BuildEnvironment,
) -> String {
    let path = env
        .doc2path(docname)
        .map(absolute_source_path)
        .or_else(|| {
            diagnostic
                .source_path
                .as_deref()
                .map(|path| absolute_source_path(std::path::Path::new(path)))
        });
    let location = path.map(|path| WarningLocation::new(Some(path), diagnostic.line));
    match location {
        Some(location) => format!("{}: WARNING: {}", location.render(), diagnostic.message),
        None => format!("WARNING: {}", diagnostic.message),
    }
}

/// Refresh the environment and prepare all documents for rendering.
pub fn prepare(
    options: &BuildOptions,
    config: &Config,
    env: &mut BuildEnvironment,
    builder_name: &'static str,
) -> OutputResult<PreparedBuild> {
    let source_dir = &options.source_dir;
    let outdir = &options.output_dir;
    if !source_dir.is_dir() {
        return Err(OutputError::MissingSource(builder_name, source_dir.clone()));
    }
    fs::create_dir_all(outdir).map_err(|e| OutputError::io(builder_name, outdir, e))?;

    let bridge = options
        .bridge
        .clone()
        .unwrap_or_else(|| Arc::new(InteropBridge::new()));
    let events = &bridge.events;
    let config_entries: std::collections::BTreeMap<&str, &scribe_config::ConfigValue> =
        config.iter().collect();
    let config_json = serde_json::to_value(config_entries).unwrap_or(serde_json::Value::Null);
    let _ = events.emit("config-inited", &Event::ConfigInited { config: Arc::new(config_json) });
    let _ = events.emit("builder-inited", &Event::BuilderInited {
        builder_name: builder_name.to_owned(),
    });

    let update_options = UpdateOptions {
        freshenv: options.freshenv,
        write_all: options.force_all,
        cache_dir: Some(
            options
                .doctree_dir
                .clone()
                .unwrap_or_else(|| outdir.join(".doctrees")),
        ),
        persist: true,
        ..UpdateOptions::default()
    };
    let report = env
        .update(config, source_dir, &update_options)
        .map_err(|e| OutputError::env(builder_name, e))?;
    let mut warnings: Vec<String> = report.warnings.clone();
    let outdated = Event::EnvGetOutdated {
        added: report.added.clone(),
        changed: report.changed.clone(),
        removed: report.removed.clone(),
    };
    let _ = events.emit("env-get-outdated", &outdated);
    let _ = events.emit("env-get-updated", &Event::EnvGetUpdated);
    let _ = events.emit("env-updated", &Event::EnvUpdated);

    // Phase 26: refresh .mo catalogs before the transform pass.
    warnings.extend(crate::i18n::compile_all(config, source_dir));

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

    let ext_activated = ActivatedExtensions::from_config(config);
    warnings.extend(ext_activated.warnings.iter().cloned());
    let (extlink_roles, extlink_warnings) = extensions::extlink_roles(config);
    warnings.extend(extlink_warnings);

    // Upstream's `SphinxProject.read()` passes `rst_prolog` / `rst_epilog` to
    // the reStructuredText parser only (``sphinx/util/rst.py``). Capture the
    // values and the suffix→parser table once so each worker can decide
    // whether a source is reST (augmented) or handled by a custom parser
    // (which never receives the injected text).
    let relevant = env.relevant_config().clone();
    let rst_prolog = relevant.rst_prolog.clone();
    let rst_epilog = relevant.rst_epilog.clone();
    // `RelevantConfig::source_suffix` stores display strings (quoted); use the
    // typed config value to obtain the actual parser names.
    let suffix_parsers: Vec<(String, String)> = config
        .get_dict("source_suffix")
        .map(|entries| {
            entries
                .iter()
                .map(|(suffix, value)| {
                    let parser = match value {
                        scribe_config::ConfigValue::Str(name) => name.clone(),
                        scribe_config::ConfigValue::None => "restructuredtext".to_owned(),
                        other => other.display_string(),
                    };
                    (suffix.clone(), parser)
                })
                .collect()
        })
        .unwrap_or_default();

    let config_arc = Arc::new(config.clone());
    let tags = Tags::with_tags(std::iter::empty::<String>());
    let registry = DomainRegistry::builtin();
    registry.load_from_env(env);
    let inv_cache_dir = options.doctree_dir.clone().unwrap_or_else(|| outdir.join(".doctrees"));
    warnings.extend(attach_intersphinx(
        &registry,
        config,
        source_dir,
        Some(&inv_cache_dir),
    ));

    // Parsing is CPU-bound per document; `rayon` keeps the read phase parallel.
    // Results are collected into a BTreeMap so downstream output is
    // deterministic regardless of completion order.
    let parsed_entries: Vec<(String, Document, Vec<String>)> = docnames
        .par_iter()
        .filter_map(|docname| {
            let path = env.found_docs.get(docname)?;
            let raw = match fs::read_to_string(path) {
                Ok(text) => text,
                Err(error) => {
                    return Some((
                        docname.clone(),
                        Document::new(Some(docname.clone())),
                        vec![format!("cannot read source for {docname}: {error}")],
                    ))
                }
            };
            let source = Arc::new(std::sync::Mutex::new(raw));
            let _ = events.emit("source-read", &Event::SourceRead {
                docname: docname.clone(),
                source: Arc::clone(&source),
            });
            let raw = source.lock().map(|guard| guard.clone()).unwrap_or_default();
            // Find the parser registered for this source's suffix. Non-reST
            // parsers (e.g. a custom parser supplied by an extension) receive
            // the raw source verbatim; upstream never injects rst_prolog or
            // rst_epilog into them and their output is an opaque doctree.
            let parser_name = path
                .extension()
                .and_then(|extension| extension.to_str())
                .map(|extension| format!(".{extension}"))
                .and_then(|suffix| {
                    suffix_parsers
                        .iter()
                        .find(|(registered, _)| registered == &suffix)
                        .map(|(_, parser)| parser.clone())
                })
                .unwrap_or_else(|| "restructuredtext".to_owned());
            if parser_name != "restructuredtext" {
                let mut document = Document::new(Some(docname.clone()));
                document.metadata.docname = Some(docname.clone());
                return Some((docname.clone(), document, Vec::new()));
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

            let mut rst_ctx = RstContext::new(source_dir, Tags::with_tags(std::iter::empty::<String>()));
            rst_ctx.config = Some(Arc::clone(&config_arc));
            rst_ctx.source_name = Some(docname.clone());
            rst_ctx.source_text = Some(Arc::new(raw));
            rst_ctx.extlinks = Arc::clone(&extlink_roles);
            let mut directive_registry = DirectiveRegistry::builtin();
            register_code_directives(&mut directive_registry);
            extensions::register_directives(&mut directive_registry, &ext_activated);
            bridge.register_directives(&directive_registry);
            register_upstream_aliases(&directive_registry, builder_name);
            let resolve_report = resolve_output(&mut output, &directive_registry, &rst_ctx);
            let doc_warnings: Vec<String> = resolve_report
                .unresolved
                .iter()
                .map(|issue| issue.warning_message(docname))
                .collect();

            output.document.metadata.docname = Some(docname.clone());
            if output.document.metadata.title.is_none() {
                output.document.metadata.title = extract_title(&output.document);
            }
            Some((docname.clone(), output.document, doc_warnings))
        })
        .collect();

    let mut parsed = BTreeMap::new();
    for (name, document, doc_warnings) in parsed_entries {
        warnings.extend(doc_warnings);
        parsed.insert(name, document);
    }

    // Expand every document's domain signatures *before* registering objects:
    // `py:fullname` / `rst:name` and friends are produced here, and registering
    // before expansion would drop every domain object.
    for document in parsed.values_mut() {
        if let Err(error) = prepare_document(document) {
            warnings.push(format!("domain signature expansion failed: {error}"));
        }
    }
    // Register domain objects for the whole project before resolving xrefs.
    // Upstream logs `StandardDomain.note_object` duplicates (e.g. two
    // `.. productionlist::` rules defining the same token) with no
    // `type`/`subtype`, so they carry no `[code]` suffix. Surface those here —
    // the registration context's diagnostics would otherwise be dropped.
    let registration_warnings: Vec<String> = {
        let transform_ctx = TransformContext::new(Some(config), Some(&tags), builder_name, builder_name)
            .with_domains(&registry)
            .with_env(env);
        for (docname, document) in &parsed {
            let domain_ctx = transform_ctx.domain_context(docname);
            if let Err(error) = registry.process_document(&domain_ctx, docname, document) {
                warnings.push(format!("{docname}: domain registration failed: {error}"));
            }
        }
        transform_ctx
            .take_domain_diagnostics()
            .into_iter()
            .filter(|diagnostic| {
                diagnostic.code == scribe_domains::diagnostics::codes::DUPLICATE_OBJECT
                    && (is_bare_duplicate_message(&diagnostic.message)
                        || diagnostic.message.starts_with("Duplicate C++ declaration")
                        || diagnostic.message.starts_with("Duplicate C declaration"))
            })
            .map(|diagnostic| {
                let docname = diagnostic.docname.as_deref().unwrap_or("");
                if is_bare_duplicate_message(&diagnostic.message) {
                    bare_domain_warning_line(&diagnostic, docname, env)
                } else {
                    domain_warning_line(&diagnostic, docname, env)
                }
            })
            .collect()
    };
    warnings.extend(registration_warnings);

    // Snapshot every doctree into the environment *before* resolving xrefs.
    // Upstream's `ReferencesResolver` runs after the `TocTreeCollector`, so a
    // `:numref:` can locate its target node (and the section/figure number the
    // collector writes onto it) through `env.get_doctree()`. Recording here
    // makes that state available during the read/resolved transform passes.
    for (docname, document) in &parsed {
        env.record_document(docname, document, BTreeMap::new());
    }
    let sources: BTreeMap<String, Arc<String>> = BTreeMap::new();
    let toctrees = collect_toctrees_with_sources(env, config, &sources);

    let compact = crate::i18n::compact_setting(config);
    for (docname, document) in &mut parsed {
        let domain = crate::i18n::docname_to_domain(docname, &compact);
        let translations = crate::i18n::load_catalog(config, source_dir, &domain);
        let mut transform_ctx = TransformContext::new(Some(config), Some(&tags), builder_name, builder_name)
            .with_domains(&registry)
            .with_env(env);
        if let Some(catalog) = &translations {
            transform_ctx = transform_ctx.with_translations(catalog);
        }
        let read_report = apply_transforms(document, &transform_ctx, Phase::Read);
        let doctree = Arc::new(std::sync::Mutex::new(serde_json::json!({
            "docname": docname,
            "node_count": document.nodes.len(),
        })));
        let _ = events.emit("doctree-read", &Event::DoctreeRead {
            docname: docname.clone(), doctree: Arc::clone(&doctree),
        });
        let resolved_report = apply_transforms(document, &transform_ctx, Phase::Resolved);
        let _ = events.emit("doctree-resolved", &Event::DoctreeResolved {
            docname: docname.clone(), doctree,
        });
        for diagnostic in read_report
            .domain_diagnostics()
            .iter()
            .chain(resolved_report.domain_diagnostics().iter())
        {
            warnings.push(domain_warning_line(diagnostic, docname, env));
        }
        for diagnostic in read_report
            .diagnostics
            .iter()
            .chain(resolved_report.diagnostics.iter())
        {
            warnings.push(format!("{docname}: {}", diagnostic.message));
        }
    }

    warnings.extend(toctree_warning_lines(&toctrees, source_dir));
    warnings.extend(bridge.warnings());
    warnings.extend(bridge.take_event_diagnostics());
    let _ = events.emit("build-finished", &Event::BuildFinished { succeeded: true });

    Ok(PreparedBuild { docnames, parsed, toctrees, warnings })
}

/// Register upstream spelling aliases that the Phase 06 registry does not
/// yet include. This is additive: the canonical handlers are reused through a
/// thin delegating wrapper, so node shapes stay identical.
fn register_upstream_aliases(registry: &DirectiveRegistry, builder_name: &str) {
    let mut aliases = vec![
        ("version-added", "versionadded"),
        ("version-changed", "versionchanged"),
        ("version-deprecated", "deprecated"),
        ("version-removed", "versionremoved"),
        ("module", "py:module"),
    ];
    // Sphinx's Python domain overrides docutils' CSS `class` directive with
    // `py:class`. Only the changes builder needs this to group library
    // changes correctly; other builders keep the Phase 06 behaviour.
    if builder_name == "changes" {
        aliases.push(("class", "py:class"));
    }
    for (alias, canonical) in aliases {
        if let Some(inner) = registry.get(canonical) {
            registry.register(AliasDirective { name: alias, inner });
        }
    }
}

struct AliasDirective {
    name: &'static str,
    inner: Arc<dyn Directive>,
}

impl Directive for AliasDirective {
    fn name(&self) -> &'static str {
        self.name
    }

    fn required_arguments(&self) -> usize {
        self.inner.required_arguments()
    }

    fn optional_arguments(&self) -> usize {
        self.inner.optional_arguments()
    }

    fn final_argument_whitespace(&self) -> bool {
        self.inner.final_argument_whitespace()
    }

    fn option_spec(&self) -> scribe_rst::OptionSpec {
        self.inner.option_spec()
    }

    fn has_content(&self) -> bool {
        self.inner.has_content()
    }

    fn run(&self, ctx: &mut DirectiveContext<'_>) -> Result<Vec<scribe_ast::NodeId>, DirectiveError> {
        self.inner.run(ctx)
    }
}

/// Loads every configured `intersphinx_mapping` inventory and attaches the
/// merged resolver to `registry`, so domain resolution can fall back to
/// external inventories (the upstream `sphinx.ext.intersphinx` seam).
///
/// Cached inventories live under `cache_dir/__intersphinx_cache__`, matching
/// upstream's doctree-local cache location. Supplying local `objects.inv`
/// paths in `intersphinx_mapping` (or pre-seeding that cache) makes the build
/// fully offline. Fetch failures are non-fatal warnings, never errors.
pub fn attach_intersphinx(
    registry: &DomainRegistry,
    config: &Config,
    source_dir: &std::path::Path,
    cache_dir: Option<&std::path::Path>,
) -> Vec<String> {
    let (mapping, mut warnings) = IntersphinxMapping::from_config(config);
    if mapping.is_empty() {
        return warnings;
    }
    let disabled = config.get_strings("intersphinx_disabled_reftypes").unwrap_or_default();
    let timeout = config.get_float("intersphinx_timeout").map(std::time::Duration::from_secs_f64);
    let cache_limit_days = config.get_int("intersphinx_cache_limit").unwrap_or(5);
    let inv_cache_dir = cache_dir.map(|dir| dir.join("__intersphinx_cache__"));
    let options = FetchOptions {
        timeout,
        cache_limit_days,
        cache_dir: inv_cache_dir,
        source_dir: Some(source_dir.to_path_buf()),
        ..FetchOptions::default()
    };
    let outcomes = fetch_all(&mapping, &options);
    for outcome in &outcomes {
        warnings.extend(outcome.warnings.iter().cloned());
    }
    let resolver = IntersphinxResolver::new(outcomes, &disabled);
    registry.set_intersphinx(resolver);
    warnings
}

/// Extract the first section title of a document, if any.
pub fn extract_title(document: &Document) -> Option<String> {
    for id in document.preorder_ids() {
        if let Ok(node) = document.get(id) {
            if node.kind == scribe_ast::NodeKind::Title {
                let title = node.astext(document);
                if !title.is_empty() {
                    return Some(title);
                }
            }
        }
    }
    None
}
