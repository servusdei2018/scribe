//! The `.. autosummary::` directive handler (Phase 22).

use std::path::PathBuf;

use scribe_ast::{NodeId, NodeKind};
use scribe_rst::directive_registry::{Directive, DirectiveContext, DirectiveError, DirectiveRegistry};
use scribe_rst::docname_join;
use scribe_rst::option_spec::{OptionKind, OptionSpec};

use crate::discovery::search_roots;

use super::table::{
    emit_table, filename_map, items_from_names, parse_names, AutosummaryItem, AutosummaryOptions,
};

/// Handler registered for `sphinx.ext.autosummary`.
#[derive(Debug, Default)]
pub struct AutosummaryDirective;

impl Directive for AutosummaryDirective {
    fn name(&self) -> &'static str {
        "autosummary"
    }

    fn required_arguments(&self) -> usize {
        0
    }

    fn has_content(&self) -> bool {
        true
    }

    fn option_spec(&self) -> OptionSpec {
        OptionSpec::empty()
            .with("caption", OptionKind::UnchangedRequired)
            .with("class", OptionKind::ClassOption)
            .with("toctree", OptionKind::Unchanged)
            .with("nosignatures", OptionKind::Flag)
            .with("recursive", OptionKind::Flag)
            .with("signatures", OptionKind::Unchanged)
            .with("template", OptionKind::Unchanged)
            // Additive option supported by newer Sphinx releases; the pinned
            // oracle does not declare it but the Phase 22 spec requires it.
            .with("list-ordered", OptionKind::Flag)
    }

    fn run(&self, ctx: &mut DirectiveContext<'_>) -> Result<Vec<NodeId>, DirectiveError> {
        let options = AutosummaryOptions::from_context(ctx);
        let names = parse_names(ctx.content);
        let (roots, config) = {
            let config = ctx.global.config.as_deref().cloned();
            let roots = search_roots(ctx.global.source_dir.as_deref(), config.as_ref());
            (roots, config)
        };
        let (items, warnings) = items_from_names(&names, &roots, config.as_ref(), &options);
        let mut ids = Vec::new();
        // Upstream logs a warning but keeps the directive output; scribe has no
        // per-directive logger, so emit a visible paragraph. The resolver only
        // splices the IDs returned by `run`, so they must be part of `ids`.
        for warning in warnings {
            if warning.is_empty() {
                continue;
            }
            if let Ok(paragraph) = ctx.add(NodeKind::Paragraph, ctx.parent) {
                if ctx.add_text(warning, paragraph, ctx.line).is_ok() {
                    ids.push(paragraph);
                }
            }
        }
        ids.extend(emit_table(ctx, &items, &options)?);
        if let Some(prefix) = options.toctree.as_deref() {
            ids.extend(emit_toctree(ctx, &items, prefix, &filename_map(config.as_ref()))?);
        } else if options.caption.is_some() {
            if let Ok(paragraph) = ctx.add(NodeKind::Paragraph, ctx.parent) {
                if ctx
                    .add_text(
                        "A captioned autosummary requires :toctree: option. ignored.",
                        paragraph,
                        ctx.line,
                    )
                    .is_ok()
                {
                    ids.push(paragraph);
                }
            }
        }
        Ok(ids)
    }
}

/// Emits the hidden toctree that upstream wraps in `autosummary_toc`.
///
/// The frozen AST has no dedicated autosummary nodes, so the collector sees a
/// regular `NodeKind::Toctree` (marked hidden so the HTML writer does not
/// render a second navigation block) carrying the same parallel entry lists
/// as the Phase 09 `toctree` directive.
fn emit_toctree(
    ctx: &mut DirectiveContext<'_>,
    items: &[AutosummaryItem],
    prefix: &str,
    filename_map: &std::collections::BTreeMap<String, String>,
) -> Result<Vec<NodeId>, DirectiveError> {
    let docname = ctx.global.source_name.clone().unwrap_or_else(|| "index".to_owned());
    let dirname = docname.rsplit_once('/').map_or("", |(parent, _)| parent);
    let source_dir = ctx.global.source_dir.as_deref().unwrap_or(std::path::Path::new("."));
    let mut docnames = Vec::new();
    let mut ids = Vec::new();
    for item in items {
        let mapped = filename_map.get(&item.real_name).cloned().unwrap_or_else(|| item.real_name.clone());
        let child = docname_join(dirname, &format!("{prefix}/{mapped}"));
        let stub = source_dir.join(format!("{child}.rst"));
        if !stub.is_file() {
            if let Ok(paragraph) = ctx.add(NodeKind::Paragraph, ctx.parent) {
                if ctx
                    .add_text(
                        format!(
                            "autosummary: stub file not found '{child}'. Check your autosummary_generate setting."
                        ),
                        paragraph,
                        ctx.line,
                    )
                    .is_ok()
                {
                    ids.push(paragraph);
                }
            }
            continue;
        }
        docnames.push(child);
    }
    if docnames.is_empty() {
        return Ok(ids);
    }
    let toctree = ctx.add(NodeKind::Toctree, ctx.parent)?;
    {
        let node = ctx.doc.get_mut(toctree)?;
        node.set_list("entries", docnames.clone());
        node.set_list("includefiles", docnames.clone());
        node.set_list("entry_refs", docnames.clone());
        node.set_list("entry_titles", vec![String::new(); docnames.len()]);
        node.set_list("entry_kinds", vec!["doc".to_owned(); docnames.len()]);
        node.set_str("maxdepth", "-1");
        node.set_str("glob", "");
        node.set_str("hidden", "1");
        if let Some(caption) = ctx.raw_options.get("caption") {
            node.set_str("caption", caption.as_str());
        }
        node.span = Some(ctx.span);
    }
    ids.push(toctree);
    Ok(ids)
}

/// Registers the directive into an extension directive registry.
pub fn register_directives(registry: &DirectiveRegistry) {
    registry.register(AutosummaryDirective);
}

/// Config keys owned by the autosummary extension.
pub fn register_config_keys(
    registry: &mut scribe_config::ConfigRegistry,
) -> Result<(), scribe_config::ConfigError> {
    use scribe_config::{ConfigDefault, ConfigType, ConfigValue};
    let entries: &[(&str, ConfigValue, ConfigType, &str)] = &[
        ("autosummary_generate", ConfigValue::Bool(true), ConfigType::Bool, "env"),
        ("autosummary_imported_members", ConfigValue::Bool(false), ConfigType::Bool, "env"),
        ("autosummary_filename_map", ConfigValue::Dict(Default::default()), ConfigType::Dict, "env"),
        ("autosummary_ignore_module_all", ConfigValue::Bool(false), ConfigType::Bool, "env"),
        ("autosummary_context", ConfigValue::Dict(Default::default()), ConfigType::Dict, "env"),
    ];
    for (name, default, value_type, rebuild) in entries {
        if registry.contains(name) {
            continue;
        }
        registry.register(scribe_config::ConfigEntry {
            name,
            default: ConfigDefault::Value(default.clone()),
            value_type: value_type.clone(),
            rebuild,
            description: "Phase 22 native autosummary setting.",
        })?;
    }
    Ok(())
}

/// Source paths searched by stub generation, exposed for tests.
#[must_use]
pub fn stub_roots(source_dir: Option<&std::path::Path>, config: Option<&scribe_config::Config>) -> Vec<PathBuf> {
    search_roots(source_dir, config)
}
