//! Thin `py:*` directive emitters (Phase 12).
//!
//! Upstream's `PyObject` / `PyModule` build the whole `desc` subtree at parse
//! time. Scribe splits that work: these handlers emit a *skeleton* node (raw
//! signature + directive options in `py:*` attributes) and
//! `scribe_domains::python::prepare_document` expands it into the frozen
//! `desc_*` / `desc_sig_*` tree. This keeps `scribe-rst` independent of
//! `scribe-domains` (ARCHITECTURE.md §15.1) while producing the same doctree.

use scribe_ast::{Node, NodeKind};

use crate::directive_registry::{DirectiveContext, DirectiveError, DirectiveRegistry};
use crate::option_spec::{OptionKind, OptionSpec};

type HandlerResult = Result<Vec<scribe_ast::NodeId>, DirectiveError>;

const FLAG_OPTIONS: &[&str] = &[
    "no-index",
    "noindex",
    "no-index-entry",
    "noindexentry",
    "no-contents-entry",
    "nocontentsentry",
    "no-typesetting",
    "single-line-parameter-list",
    "single-line-type-parameter-list",
    "async",
    "final",
    "abstract",
    "abstractmethod",
    "classmethod",
    "staticmethod",
    "deprecated",
];

const VALUE_OPTIONS: &[&str] = &[
    "module", "canonical", "annotation", "type", "value", "synopsis", "platform",
];

fn object_option_spec() -> OptionSpec {
    let mut spec = OptionSpec::empty().with("class", OptionKind::ClassOption).with("name", OptionKind::Unchanged);
    for flag in FLAG_OPTIONS {
        spec = spec.with(*flag, OptionKind::Flag);
    }
    for value in VALUE_OPTIONS {
        spec = spec.with(*value, OptionKind::Unchanged);
    }
    spec
}

fn bool_option(ctx: &DirectiveContext<'_>, names: &[&str]) -> bool {
    names.iter().any(|name| ctx.flag(name))
}

fn string_option(ctx: &DirectiveContext<'_>, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| ctx.option_str(name)).map(str::to_owned)
}

/// Serializes the directive option set into the JSON string consumed by
/// `PyOptions::from_node`.
fn options_json(ctx: &DirectiveContext<'_>) -> String {
    let config = ctx.global.config.as_deref();
    let get_bool = |key: &str| -> bool {
        config.and_then(|config| config.get_bool(key)).unwrap_or(false)
    };
    // Boolean values must be JSON booleans (not strings) so they deserialize
    // into `PyOptions`' `bool` fields; absent strings become JSON `null`.
    let values = serde_json::json!({
        "module": string_option(ctx, &["module"]),
        "canonical": string_option(ctx, &["canonical"]),
        "annotation": string_option(ctx, &["annotation"]),
        "type": string_option(ctx, &["type"]),
        "value": string_option(ctx, &["value"]),
        "synopsis": string_option(ctx, &["synopsis"]),
        "platform": string_option(ctx, &["platform"]),
        "no_index": bool_option(ctx, &["no-index", "noindex"]),
        "no_index_entry": bool_option(ctx, &["no-index-entry", "noindexentry"]),
        "no_contents_entry": bool_option(ctx, &["no-contents-entry", "nocontentsentry"]),
        "no_typesetting": ctx.flag("no-typesetting"),
        "deprecated": ctx.flag("deprecated"),
        "async_": ctx.flag("async"),
        "final_": ctx.flag("final"),
        "abstract_": ctx.flag("abstract"),
        "abstractmethod": ctx.flag("abstractmethod"),
        "classmethod": ctx.flag("classmethod"),
        "staticmethod": ctx.flag("staticmethod"),
        "single_line_parameter_list": ctx.flag("single-line-parameter-list"),
        "single_line_type_parameter_list": ctx.flag("single-line-type-parameter-list"),
        "use_unqualified_type_names": get_bool("python_use_unqualified_type_names"),
        "add_module_names": config.and_then(|config| config.get_bool("add_module_names")).unwrap_or(true),
    });
    serde_json::to_string(&values).unwrap_or_default()
}

fn emit_index(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let index = ctx.add(NodeKind::Index, ctx.parent)?;
    if let Ok(node) = ctx.doc.get_mut(index) {
        node.attrs.set_str("entries", "[]");
        node.span = Some(ctx.span);
    }
    Ok(vec![index])
}

/// Emits the skeleton for a Python object directive.
fn py_object(ctx: &mut DirectiveContext<'_>, objtype: &'static str) -> HandlerResult {
    let signature = ctx.argument(0).unwrap_or("").trim().to_owned();
    if signature.is_empty() {
        return Ok(Vec::new());
    }
    let options = options_json(ctx);
    let index = emit_index(ctx)?;

    let desc = ctx.add(NodeKind::Desc, ctx.parent)?;
    {
        let node = ctx.doc.get_mut(desc)?;
        if let Some(&index_id) = index.first() {
            node.attrs.set_node_id("py:index_id", index_id);
        }
        node.attrs.set_str("objtype", objtype);
        node.attrs.set_str("py:directive", format!("py:{objtype}"));
        node.attrs.set_str("py:signature", &signature);
        node.attrs.set_str("py:options", options);
        node.attrs.set_str("py:sig_base", (ctx.span.start as u64).to_string());
    }
    // The placeholder signature keeps the upstream child order
    // (`desc_signature` before `desc_content`) after domain expansion.
    let signature_placeholder = ctx.add(NodeKind::DescSignature, desc)?;
    if let Ok(node) = ctx.doc.get_mut(signature_placeholder) {
        node.attrs.set_list("classes", vec!["sig".to_owned(), "sig-object".to_owned(), "py".to_owned()]);
        node.span = Some(ctx.span);
    }
    let content = ctx.add(NodeKind::DescContent, desc)?;
    ctx.parse_nested_into(content)?;

    let mut ids = index;
    ids.push(desc);
    Ok(ids)
}

fn py_module(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let name = ctx.argument(0).unwrap_or("").trim().to_owned();
    if name.is_empty() {
        return Ok(Vec::new());
    }
    // Sphinx's `PyModule.run` also updates `env.current_document.current_module`.
    if let Ok(mut guard) = ctx.global.current_module.write() {
        *guard = Some(name.clone());
    }
    let options = options_json(ctx);
    let no_index = ctx.flag("no-index") || ctx.flag("noindex");
    let mut ids = Vec::new();
    if !no_index {
        ids.extend(emit_index(ctx)?);
        let anchor = crate::names::make_id(&format!("module-{name}"));
        let mut target = Node::new(NodeKind::Target);
        target.attrs.set_str("objtype", "module");
        target.attrs.set_str("py:signature", &name);
        target.attrs.set_str("py:options", options);
        target.attrs.set_str("py:synopsis", ctx.option_str("synopsis").unwrap_or(""));
        target.attrs.set_str("py:platform", ctx.option_str("platform").unwrap_or(""));
        target.attrs.set_bool("py:deprecated", ctx.flag("deprecated"));
        target.attrs.set_bool("ismod", true);
        if !ctx.flag("no-index-entry") && !ctx.flag("noindexentry") {
            target.attrs.set_str("py:index_entry", format!("module; {name}"));
        }
        target.attrs.set_list("ids", vec![anchor]);
        target.span = Some(ctx.span);
        let target_id = ctx.doc.add_node(target, Some(ctx.parent))?;
        ids.push(target_id);
    } else {
        // Even with `no-index`, the module still updates the read context.
        let mut marker = Node::new(NodeKind::Target);
        marker.attrs.set_str("objtype", "currentmodule");
        marker.attrs.set_str("py:signature", &name);
        marker.span = Some(ctx.span);
        let marker_id = ctx.doc.add_node(marker, Some(ctx.parent))?;
        ids.push(marker_id);
    }
    ids.extend(ctx.parse_nested()?);
    Ok(ids)
}

fn py_currentmodule(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let name = ctx.argument(0).unwrap_or("").trim().to_owned();
    if name.is_empty() {
        return Ok(Vec::new());
    }
    if let Ok(mut guard) = ctx.global.current_module.write() {
        *guard = Some(name.clone());
    }
    // Upstream returns no nodes; the marker carries the context change until
    // `prepare_document` removes it.
    let mut marker = Node::new(NodeKind::Target);
    marker.attrs.set_str("objtype", "currentmodule");
    marker.attrs.set_str("py:signature", &name);
    marker.span = Some(ctx.span);
    let id = ctx.doc.add_node(marker, Some(ctx.parent))?;
    Ok(vec![id])
}

/// Registers every `py:*` directive (plus the bare `currentmodule` spelling).
pub fn register(registry: &DirectiveRegistry) {
    let spec = object_option_spec();
    macro_rules! object_directive {
        ($name:literal, $objtype:literal) => {
            registry.register(crate::directives::FnDirective::new(
                concat!("py:", $name),
                1,
                0,
                true,
                spec.clone(),
                true,
                |ctx: &mut DirectiveContext<'_>| py_object(ctx, $objtype),
            ));
            // Sphinx's Python domain registers the unprefixed spellings too:
            // `.. class::`, `.. function::`, `.. data::` and friends are plain
            // aliases of `py:class` / `py:function` / `py:data`.  Without them
            // the docs' class descriptions never become domain objects, so
            // every `:class:` / `:meth:` reference to them is reported missing.
            registry.register(crate::directives::FnDirective::new(
                $name,
                1,
                0,
                true,
                spec.clone(),
                true,
                |ctx: &mut DirectiveContext<'_>| py_object(ctx, $objtype),
            ));
        };
    }
    object_directive!("function", "function");
    object_directive!("data", "data");
    object_directive!("class", "class");
    object_directive!("exception", "exception");
    object_directive!("method", "method");
    object_directive!("classmethod", "classmethod");
    object_directive!("staticmethod", "staticmethod");
    object_directive!("attribute", "attribute");
    object_directive!("property", "property");
    object_directive!("type", "type");
    object_directive!("decorator", "decorator");
    object_directive!("decoratormethod", "decoratormethod");

    registry.register(crate::directives::FnDirective::new(
        "py:module", 1, 0, true, spec.clone(), true, py_module,
    ));
    registry.register(crate::directives::FnDirective::new(
        "module", 1, 0, true, spec.clone(), true, py_module,
    ));
    for name in ["py:currentmodule", "currentmodule"] {
        registry.register(crate::directives::FnDirective::new(
            name, 1, 0, true, OptionSpec::empty(), false, py_currentmodule,
        ));
    }
}
