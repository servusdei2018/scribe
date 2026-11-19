//! Thin `c:*` / `cpp:*` / `js:*` / `rst:*` directive emitters (Phase 13).
//!
//! Mirrors the Phase 12 Python skeleton seam: `scribe-rst` cannot depend on
//! `scribe-domains` (ARCHITECTURE.md §15.1), so each handler emits a lightweight
//! `desc` node carrying the raw signature plus domain options.  The domain's
//! `prepare_document` expands it into the frozen `desc_*` tree.

use scribe_ast::{Node, NodeKind};

use crate::directive_registry::{DirectiveContext, DirectiveError, DirectiveRegistry};
use crate::option_spec::{OptionKind, OptionSpec};

type HandlerResult = Result<Vec<scribe_ast::NodeId>, DirectiveError>;

const FLAG_OPTIONS: &[&str] = &[
    "no-index", "noindex", "no-index-entry", "noindexentry", "no-contents-entry",
    "nocontentsentry", "no-typesetting",
];

fn object_spec() -> OptionSpec {
    let mut spec = OptionSpec::empty()
        .with("class", OptionKind::ClassOption)
        .with("name", OptionKind::Unchanged)
        // `.. rst:directive:option::` documents the option's value type with
        // `:type:`; upstream accepts it on all object descriptions.
        .with("type", OptionKind::Unchanged);
    for flag in FLAG_OPTIONS {
        spec = spec.with(*flag, OptionKind::Flag);
    }
    spec
}

fn options_json(ctx: &DirectiveContext<'_>) -> String {
    let no_index = ctx.flag("no-index") || ctx.flag("noindex");
    let no_index_entry = ctx.flag("no-index-entry") || ctx.flag("noindexentry");
    serde_json::to_string(&serde_json::json!({
        "no_index": no_index,
        "no_index_entry": no_index_entry,
    }))
    .unwrap_or_default()
}

fn emit_index(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let index = ctx.add(NodeKind::Index, ctx.parent)?;
    if let Ok(node) = ctx.doc.get_mut(index) {
        node.attrs.set_str("entries", "[]");
        node.span = Some(ctx.span);
    }
    Ok(vec![index])
}

/// Emits a skeleton object description for one domain directive.
fn object(
    ctx: &mut DirectiveContext<'_>,
    domain: &'static str,
    objtype: &'static str,
) -> HandlerResult {
    let signature = ctx.argument(0).unwrap_or("").trim().to_owned();
    if signature.is_empty() {
        return Ok(Vec::new());
    }
    let options = options_json(ctx);
    let mut ids = emit_index(ctx)?;
    let desc = ctx.add(NodeKind::Desc, ctx.parent)?;
    {
        let node = ctx.doc.get_mut(desc)?;
        if let Some(&index_id) = ids.first() {
            node.attrs.set_node_id(format!("{domain}:index_id"), index_id);
        }
        node.attrs.set_str("domain", domain);
        node.attrs.set_str("objtype", objtype);
        node.attrs.set_str("desctype", objtype);
        node.attrs.set_str(format!("{domain}:signature"), &signature);
        node.attrs.set_str(format!("{domain}:options"), &options);
        node.attrs.set_str(format!("{domain}:sig_base"), (ctx.span.start as u64).to_string());
        node.attrs.set_list("classes", vec![domain.to_owned(), objtype.to_owned()]);
    }
    let placeholder = ctx.add(NodeKind::DescSignature, desc)?;
    if let Ok(node) = ctx.doc.get_mut(placeholder) {
        node.attrs.set_list("classes", vec!["sig".to_owned(), "sig-object".to_owned(), domain.to_owned()]);
        node.span = Some(ctx.span);
    }
    let content = ctx.add(NodeKind::DescContent, desc)?;
    ctx.parse_nested_into(content)?;
    ids.push(desc);
    Ok(ids)
}

/// Emits a namespace / module marker target.
fn marker(
    ctx: &mut DirectiveContext<'_>,
    domain: &'static str,
    kind: &'static str,
) -> HandlerResult {
    let name = ctx.argument(0).unwrap_or("").trim().to_owned();
    let mut target = Node::new(NodeKind::Target);
    target.attrs.set_str("objtype", kind);
    target.attrs.set_str(format!("{domain}:{kind}"), &name);
    target.span = Some(ctx.span);
    let id = ctx.doc.add_node(target, Some(ctx.parent))?;
    let mut ids = vec![id];
    ids.extend(ctx.parse_nested()?);
    Ok(ids)
}

/// Registers every new-domain directive.
pub fn register(registry: &DirectiveRegistry) {
    let spec = object_spec();
    macro_rules! object_directive {
        ($domain:literal, $name:literal, $objtype:literal) => {
            registry.register(crate::directives::FnDirective::new(
                concat!($domain, ":", $name),
                1,
                0,
                true,
                spec.clone(),
                true,
                |ctx: &mut DirectiveContext<'_>| object(ctx, $domain, $objtype),
            ));
        };
    }
    // C domain.
    object_directive!("c", "member", "member");
    object_directive!("c", "var", "var");
    object_directive!("c", "function", "function");
    object_directive!("c", "macro", "macro");
    object_directive!("c", "struct", "struct");
    object_directive!("c", "union", "union");
    object_directive!("c", "enum", "enum");
    object_directive!("c", "enumerator", "enumerator");
    object_directive!("c", "type", "type");
    object_directive!("c", "alias", "alias");
    registry.register(crate::directives::FnDirective::new(
        "c:namespace", 1, 0, true, OptionSpec::empty(), false,
        |ctx: &mut DirectiveContext<'_>| marker(ctx, "c", "namespace"),
    ));
    registry.register(crate::directives::FnDirective::new(
        "c:namespace-push", 1, 0, true, OptionSpec::empty(), false,
        |ctx: &mut DirectiveContext<'_>| marker(ctx, "c", "namespace-push"),
    ));
    registry.register(crate::directives::FnDirective::new(
        "c:namespace-pop", 0, 0, false, OptionSpec::empty(), false,
        |ctx: &mut DirectiveContext<'_>| marker(ctx, "c", "namespace-pop"),
    ));

    // C++ domain.
    object_directive!("cpp", "class", "class");
    object_directive!("cpp", "struct", "struct");
    object_directive!("cpp", "union", "union");
    object_directive!("cpp", "function", "function");
    object_directive!("cpp", "member", "member");
    object_directive!("cpp", "var", "var");
    object_directive!("cpp", "type", "type");
    object_directive!("cpp", "concept", "concept");
    object_directive!("cpp", "enum", "enum");
    object_directive!("cpp", "enum-struct", "enum-struct");
    object_directive!("cpp", "enum-class", "enum-class");
    object_directive!("cpp", "enumerator", "enumerator");
    object_directive!("cpp", "alias", "alias");
    registry.register(crate::directives::FnDirective::new(
        "cpp:namespace", 1, 0, true, OptionSpec::empty(), false,
        |ctx: &mut DirectiveContext<'_>| marker(ctx, "cpp", "namespace"),
    ));
    registry.register(crate::directives::FnDirective::new(
        "cpp:namespace-push", 1, 0, true, OptionSpec::empty(), false,
        |ctx: &mut DirectiveContext<'_>| marker(ctx, "cpp", "namespace-push"),
    ));
    registry.register(crate::directives::FnDirective::new(
        "cpp:namespace-pop", 0, 0, false, OptionSpec::empty(), false,
        |ctx: &mut DirectiveContext<'_>| marker(ctx, "cpp", "namespace-pop"),
    ));

    // JavaScript domain.
    object_directive!("js", "function", "function");
    object_directive!("js", "method", "method");
    object_directive!("js", "class", "class");
    object_directive!("js", "data", "data");
    object_directive!("js", "attribute", "attribute");
    registry.register(crate::directives::FnDirective::new(
        "js:module", 1, 0, true, OptionSpec::empty(), false,
        |ctx: &mut DirectiveContext<'_>| marker(ctx, "js", "module"),
    ));

    // reST domain.
    object_directive!("rst", "directive", "directive");
    object_directive!("rst", "directive:option", "directive:option");
    object_directive!("rst", "role", "role");
}
