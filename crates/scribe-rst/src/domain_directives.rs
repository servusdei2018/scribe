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
    let mut signature = ctx.argument(0).unwrap_or("").trim().to_owned();
    let mut content = ctx.content;
    // reStructuredText folds a trailing `\` into the following indented
    // lines as a single directive argument. Phase 04 leaves those lines in
    // the directive content, so rejoin them here (up to the first blank line,
    // which starts the real description body).
    let mut continuation_lines = 0usize;
    if signature.ends_with('\\') {
        let lines: Vec<&str> = content.lines().collect();
        let mut taken = 0usize;
        let mut joined = String::new();
        for line in &lines {
            if line.trim().is_empty() {
                break;
            }
            joined.push(' ');
            joined.push_str(line.trim());
            taken += line.len() + 1;
            continuation_lines += 1;
        }
        signature.push_str(&joined);
        content = content.get(taken.min(content.len())..).unwrap_or("").trim_start_matches('\n');
    }
    // A directive may declare several overloaded entities by placing extra
    // declaration lines (indented under the first argument) before a blank
    // line. Upstream packs them into one `desc` with several signatures.
    let mut extra_signatures = Vec::new();
    let mut extra_lines = 0usize;
    if content.starts_with([' ', '\t']) {
        for line in content.lines() {
            if line.trim().is_empty() {
                break;
            }
            extra_signatures.push(line.trim().to_owned());
            extra_lines += line.len() + 1;
        }
        content = content.get(extra_lines.min(content.len())..).unwrap_or("").trim_start_matches('\n');
    }
    if signature.is_empty() {
        return Ok(Vec::new());
    }
    let mut all_signatures = vec![signature.clone()];
    all_signatures.extend(extra_signatures.clone());
    let signatures_json = serde_json::to_string(&all_signatures).unwrap_or_default();
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
        node.attrs.set_str(format!("{domain}:signatures"), &signatures_json);
        node.attrs.set_str(format!("{domain}:options"), &options);
        node.attrs.set_str(format!("{domain}:sig_base"), (ctx.span.start as u64).to_string());
        node.attrs.set_list("classes", vec![domain.to_owned(), objtype.to_owned()]);
    }
    let placeholder = ctx.add(NodeKind::DescSignature, desc)?;
    if let Ok(node) = ctx.doc.get_mut(placeholder) {
        node.attrs.set_list("classes", vec!["sig".to_owned(), "sig-object".to_owned(), domain.to_owned()]);
        node.span = Some(ctx.span);
    }
    for _ in 0..extra_signatures.len() {
        let extra = ctx.add(NodeKind::DescSignature, desc)?;
        if let Ok(node) = ctx.doc.get_mut(extra) {
            node.attrs.set_list("classes", vec!["sig".to_owned(), "sig-object".to_owned(), domain.to_owned()]);
            node.span = Some(ctx.span);
        }
    }
    let content_node = ctx.add(NodeKind::DescContent, desc)?;
    let base_line = ctx.line + 1 + ctx.raw_options.len() as u32 + continuation_lines as u32 + extra_lines as u32;
    let _ = ctx.parse_block_text(content, content_node, base_line);
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

/// Emits a `js:module` directive: upstream `JSModule.run()` returns an `index`
/// node followed by an `ismod` target carrying the module anchor, then the
/// parsed content.  `MoveModuleTargets` folds the first module target into its
/// section, so the sibling `index` also carries `js:module` for the domain
/// scope/object passes which run after that transform.
fn js_module(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let name = ctx.argument(0).unwrap_or("").trim().to_owned();
    if name.is_empty() {
        return Ok(Vec::new());
    }
    let mut ids = emit_index(ctx)?;
    if let Some(&index_id) = ids.first() {
        if let Ok(node) = ctx.doc.get_mut(index_id) {
            let entry = format!("['single', '{} (module)', 'module-{}', '', None]", name, name);
            node.attrs.set_str("entries", &entry);
            node.attrs.set_str("js:module", &name);
        }
    }
    let anchor = format!("module-{}", name);
    let mut target = Node::new(NodeKind::Target);
    target.attrs.set_str("objtype", "module");
    target.attrs.set_str("js:module", &name);
    target.attrs.set_bool("ismod", true);
    target.attrs.set_list("ids", vec![anchor]);
    target.span = Some(ctx.span);
    let target_id = ctx.doc.add_node(target, Some(ctx.parent))?;
    ids.push(target_id);
    ids.extend(ctx.parse_nested()?);
    Ok(ids)
}

/// Emits a `default-domain` directive: switches the active default domain for
/// the rest of this source, so unprefixed domain directives and roles resolve
/// against it (mirroring `sphinx.directives.other.DefaultDomain`).
fn default_domain(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let name = ctx.argument(0).unwrap_or("").trim().to_ascii_lowercase();
    if let Ok(mut guard) = ctx.global.default_domain.write() {
        *guard = if name.is_empty() { None } else { Some(name) };
    }
    Ok(Vec::new())
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
    // `.. default-domain::` is a standard directive that selects which domain
    // handles subsequent unprefixed names. It is registered here because the
    // domain directives are the reason it exists.
    registry.register(crate::directives::FnDirective::new(
        "default-domain", 1, 0, false, OptionSpec::empty(), false,
        default_domain,
    ));
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
        |ctx: &mut DirectiveContext<'_>| js_module(ctx),
    ));

    // reST domain.
    object_directive!("rst", "directive", "directive");
    object_directive!("rst", "directive:option", "directive:option");
    object_directive!("rst", "role", "role");
}
