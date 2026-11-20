//! Auto directives for the native autodoc engine (Phase 21).
//!
//! These handlers parse Python sources statically, convert docstrings through
//! the napoleon converter, and emit the same *skeleton* `desc` nodes the Phase
//! 12 `py:*` directives produce. `scribe_domains::python::prepare_document`
//! then expands those skeletons into frozen `desc_*` / `desc_sig_*` trees, so
//! signature rendering is delegated to `PythonDomain` exactly as required —
//! no signature parser is duplicated here.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use scribe_ast::{Node, NodeId, NodeKind};
use scribe_config::Config;
use scribe_rst::directive_registry::{Directive, DirectiveContext, DirectiveError, DirectiveRegistry};
use scribe_rst::option_spec::{OptionKind, OptionSpec};

use crate::config::{auto_options, napoleon_options, AutoOptions, ClassDocFrom, MemberOrder};
use crate::discovery::{resolve_module, search_roots};
use crate::model::{ClassInfo, Member, ModuleInfo};
use crate::napoleon::convert_docstring;
use crate::parser::parse_source;

type HandlerResult = Result<Vec<NodeId>, DirectiveError>;

/// Content-hash-keyed cache of parsed modules, shared across directives in one
/// process. The key includes the absolute path and BLAKE3 source hash, so a
/// stale entry can never shadow changed source.
static MODULE_CACHE: OnceLock<Mutex<BTreeMap<String, ModuleInfo>>> = OnceLock::new();

fn cache() -> &'static Mutex<BTreeMap<String, ModuleInfo>> {
    MODULE_CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Loads a module from disk, parsing at most once per `(path, hash)`.
pub(crate) fn load_cached(name: &str, path: &PathBuf) -> Option<ModuleInfo> {
    let source = std::fs::read_to_string(path).ok()?;
    let hash = blake3::hash(source.as_bytes()).to_hex().to_string();
    let key = format!("{}|{hash}", path.display());
    if let Ok(cache) = cache().lock() {
        if let Some(module) = cache.get(&key) {
            return Some(module.clone());
        }
    }
    let module = parse_source(&source, name, path).ok()?;
    if let Ok(mut cache) = cache().lock() {
        // Keep the cache bounded; evict the first key when it exceeds 256 entries.
        if cache.len() >= 256 && !cache.contains_key(&key) {
            if let Some(first) = cache.keys().next().cloned() {
                cache.remove(&first);
            }
        }
        cache.insert(key, module.clone());
    }
    Some(module)
}

/// Option specification shared by every auto directive. Values are declared
/// as `Unchanged` so both flag form (`:members:`) and value form
/// (`:members: a, b`) are accepted.
fn auto_option_spec() -> OptionSpec {
    let mut spec = OptionSpec::empty()
        .with("class", OptionKind::ClassOption)
        .with("name", OptionKind::Unchanged);
    for flag in [
        "no-index", "noindex", "no-index-entry", "noindexentry",
        "show-inheritance", "undoc-members", "ignore-module-all", "no-value",
    ] {
        spec = spec.with(flag, OptionKind::Flag);
    }
    // These accept an optional member list (e.g. `:private-members: _foo, _bar`).
    for value in [
        "members", "exclude-members", "member-order", "annotation", "class-doc-from",
        "private-members", "special-members", "inherited-members",
    ] {
        spec = spec.with(value, OptionKind::Unchanged);
    }
    spec
}

/// Resolves an auto-directive argument to a parsed module and an object path.
///
/// The longest module prefix that resolves to a source file wins; the object
/// path is the remainder. When the object path is empty the module itself is
/// the target.
pub(crate) fn resolve_target(
    argument: &str,
    roots: &[PathBuf],
) -> Option<(ModuleInfo, String)> {
    let segments: Vec<&str> = argument.split('.').filter(|part| !part.is_empty()).collect();
    if segments.is_empty() {
        return None;
    }
    for count in (1..=segments.len()).rev() {
        let module_name = segments[..count].join(".");
        let Some(path) = resolve_module(&module_name, roots) else { continue };
        let Some(module) = load_cached(&module_name, &path) else { continue };
        let object_path = segments[count..].join(".");
        if object_path.is_empty() || module.find(&object_path).is_some() {
            return Some((module, object_path));
        }
        // Follow re-exports: `sphinx.domains.__init__` does
        // `from sphinx.domains._index import Index`, so `.. autoclass:: Index`
        // under `.. module:: sphinx.domains` must resolve in `_index`.
        let (first, rest) = match object_path.split_once('.') {
            Some((first, rest)) => (first, Some(rest)),
            None if !object_path.is_empty() => (object_path.as_str(), None),
            None => continue,
        };
        for import in &module.imports {
            if import.level != 0 || import.module.is_empty() {
                continue;
            }
            let reexport = import
                .names
                .iter()
                .any(|(name, alias)| alias.as_deref().unwrap_or(name) == first);
            if !reexport {
                continue;
            }
            if let Some(target_path) = resolve_module(&import.module, roots) {
                if let Some(target_module) = load_cached(&import.module, &target_path) {
                    let remaining = rest.map_or_else(|| first.to_owned(), |rest| format!("{first}.{rest}"));
                    if target_module.find(&remaining).is_some() {
                        return Some((target_module, remaining));
                    }
                }
            }
        }
    }
    None
}

fn roots_and_config(ctx: &DirectiveContext<'_>) -> (Vec<PathBuf>, Option<Config>) {
    let config = ctx.global.config.as_deref().cloned();
    let roots = search_roots(ctx.global.source_dir.as_deref(), config.as_ref());
    (roots, config)
}

/// Builds a `local name -> fully qualified name` map from a module's imports.
fn import_map(module: &crate::model::ModuleInfo) -> std::collections::BTreeMap<String, String> {
    let mut map = std::collections::BTreeMap::new();
    for import in &module.imports {
        if import.level != 0 || import.module.is_empty() {
            continue;
        }
        for (name, alias) in &import.names {
            let local = alias.as_deref().unwrap_or(name);
            map.insert(local.to_owned(), format!("{}.{}", import.module, name));
        }
    }
    map
}

/// Serializes the options for the `py:*` skeleton seam.
fn options_json(
    module: Option<&str>,
    options: &AutoOptions,
    extra: &ExtraOptions,
    imports: &std::collections::BTreeMap<String, String>,
) -> String {
    let value = serde_json::json!({
        "module": module,
        "canonical": null,
        "annotation": null,
        "type": extra.type_annotation,
        "value": extra.value,
        "synopsis": null,
        "platform": null,
        "no_index": options.no_index,
        "no_index_entry": options.no_index_entry,
        "no_contents_entry": false,
        "no_typesetting": false,
        "deprecated": false,
        "async_": extra.is_async,
        "final_": extra.is_final,
        "abstract_": extra.is_abstract,
        "abstractmethod": false,
        "classmethod": extra.is_classmethod,
        "staticmethod": extra.is_staticmethod,
        "single_line_parameter_list": false,
        "single_line_type_parameter_list": false,
        "use_unqualified_type_names": false,
        "add_module_names": true,
        "imports": imports,
    });
    serde_json::to_string(&value).unwrap_or_default()
}

#[derive(Default)]
struct ExtraOptions {
    type_annotation: Option<String>,
    value: Option<String>,
    is_async: bool,
    is_final: bool,
    is_abstract: bool,
    is_classmethod: bool,
    is_staticmethod: bool,
}

/// Emits an `index` + skeleton `desc` pair, mirroring the Phase 12
/// `py_object` emitter. `content` may append children to `desc_content`.
#[allow(clippy::too_many_arguments)] // mirrors the py:* skeleton emitter's context surface
fn emit_object(
    ctx: &mut DirectiveContext<'_>,
    objtype: &'static str,
    signature: &str,
    module: &str,
    options: &AutoOptions,
    extra: &ExtraOptions,
    imports: &std::collections::BTreeMap<String, String>,
    docstring_rst: Option<&str>,
    content: impl FnOnce(&mut DirectiveContext<'_>, NodeId) -> HandlerResult,
) -> HandlerResult {
    let index = ctx.add(NodeKind::Index, ctx.parent)?;
    if let Ok(node) = ctx.doc.get_mut(index) {
        node.attrs.set_str("entries", "[]");
        node.span = Some(ctx.span);
    }
    let desc = ctx.add(NodeKind::Desc, ctx.parent)?;
    {
        let node = ctx.doc.get_mut(desc)?;
        node.attrs.set_node_id("py:index_id", index);
        node.attrs.set_str("objtype", objtype);
        node.attrs.set_str("py:directive", format!("py:{objtype}"));
        node.attrs.set_str("py:signature", signature);
        node.attrs.set_str("py:options", options_json(Some(module), options, extra, imports));
        node.attrs.set_str("py:sig_base", (ctx.span.start as u64).to_string());
        node.span = Some(ctx.span);
    }
    let placeholder = ctx.add(NodeKind::DescSignature, desc)?;
    if let Ok(node) = ctx.doc.get_mut(placeholder) {
        node.attrs.set_list("classes", vec!["sig".to_owned(), "sig-object".to_owned(), "py".to_owned()]);
        node.span = Some(ctx.span);
    }
    let content_id = ctx.add(NodeKind::DescContent, desc)?;
    if let Some(rst) = docstring_rst {
        if !rst.trim().is_empty() {
            ctx.parse_block_text(rst, content_id, ctx.line)?;
        }
    }
    // Run the content callback with `parent` temporarily retargeted to the
    // new `desc_content`, then restore it so sibling nodes land in the right
    // place after the directive returns.
    let outer_parent = ctx.parent;
    ctx.parent = content_id;
    let result = content(ctx, content_id);
    ctx.parent = outer_parent;
    result?;
    Ok(vec![index, desc])
}

/// Converts a docstring to RST using the build's napoleon configuration.
fn convert(docstring: Option<&str>, config: Option<&Config>) -> Option<String> {
    let docstring = docstring?;
    if docstring.trim().is_empty() {
        return None;
    }
    Some(convert_docstring(docstring, &napoleon_options(config)))
}

/// Selects and orders members according to the directive options.
pub fn select_members<'a>(
    members: &'a [Member],
    options: &AutoOptions,
    all: Option<&[String]>,
) -> Vec<&'a Member> {
    let explicit = options.members.as_ref().filter(|list| !list.is_empty());
    let mut selected: Vec<&Member> = members
        .iter()
        .filter(|member| {
            let name = member.name();
            if options.exclude_members.contains(name) {
                return false;
            }
            if let Some(list) = explicit {
                return list.iter().any(|candidate| candidate == name || candidate == member.qualname());
            }
            let special = name.starts_with("__") && name.ends_with("__");
            let private = name.starts_with('_') && !special;
            let allowed = member.is_public()
                || (options.private_members && private)
                || (options.special_members && special);
            if !allowed {
                return false;
            }
            // Upstream documents data/attributes without a docstring (showing
            // their value or an "alias of ..." sentence) even without
            // `:undoc-members:`; functions/classes still require a docstring.
            let requires_doc = !matches!(member, Member::Attribute(_));
            if !options.undoc_members
                && requires_doc
                && member.docstring().is_none_or(|doc| doc.trim().is_empty())
            {
                return false;
            }
            if let Some(all) = all {
                if !options.ignore_module_all && !all.iter().any(|candidate| candidate == name) {
                    return false;
                }
            }
            true
        })
        .collect();
    match options.member_order {
        MemberOrder::BySource => {}
        MemberOrder::Alphabetical | MemberOrder::Groupwise => {
            selected.sort_by(|left, right| {
                if options.member_order == MemberOrder::Groupwise {
                    let group = |member: &&Member| match member {
                        Member::Class(_) => 0,
                        Member::Attribute(_) => 1,
                        Member::Function(_) => 2,
                    };
                    group(left).cmp(&group(right)).then_with(|| left.name().cmp(right.name()))
                } else {
                    left.name().cmp(right.name())
                }
            });
        }
    }
    selected
}

/// Emits one member (function/class/attribute) as a skeleton desc.
fn emit_member(
    ctx: &mut DirectiveContext<'_>,
    member: &Member,
    module: &str,
    options: &AutoOptions,
    config: Option<&Config>,
    class_name: Option<&str>,
    imports: &std::collections::BTreeMap<String, String>,
) -> HandlerResult {
    let signature = member_signature(member, class_name);
    let extra = member_extra(member);
    let docstring = member_docstring(member, options);
    let rst = convert(docstring, config);
    let objtype = member.objtype();
    emit_object(ctx, objtype, &signature, module, options, &extra, imports, rst.as_deref(), |ctx, content| {
        if let Member::Class(class) = member {
            emit_class_children(ctx, class, module, options, config, imports)?;
        }
        if options.show_inheritance {
            if let Member::Class(class) = member {
                if !class.bases.is_empty() {
                    let text = format!("Bases: {}", class.bases.join(", "));
                    ctx.parse_block_text(&text, content, ctx.line)?;
                }
            }
        }
        Ok(Vec::new())
    })
}

/// Computes the signature string for a member, qualifying methods with their
/// class name so standalone `automethod` directives resolve correctly.
fn member_signature(member: &Member, class_name: Option<&str>) -> String {
    match member {
        Member::Function(info) => {
            let mut signature = info.signature.clone();
            if let Some(class) = class_name {
                if !signature.starts_with(&format!("{class}.")) {
                    let (prefix, rest) = signature.split_once(' ').unwrap_or((&signature, ""));
                    if prefix.starts_with("async") {
                        signature = format!("async {class}.{rest}");
                    } else {
                        signature = format!("{class}.{signature}");
                    }
                }
            }
            signature
        }
        Member::Class(info) => info.name.clone(),
        Member::Attribute(info) => info.name.clone(),
    }
}

fn member_extra(member: &Member) -> ExtraOptions {
    match member {
        Member::Function(info) => ExtraOptions {
            is_async: info.kind == crate::model::FunctionKind::Coroutine,
            is_final: info.is_final,
            is_abstract: info.is_abstract,
            is_classmethod: info.kind == crate::model::FunctionKind::ClassMethod,
            is_staticmethod: info.kind == crate::model::FunctionKind::StaticMethod,
            ..ExtraOptions::default()
        },
        Member::Class(info) => ExtraOptions {
            is_final: info.is_final,
            is_abstract: info.is_abstract,
            ..ExtraOptions::default()
        },
        Member::Attribute(_) => ExtraOptions::default(),
    }
}

fn member_docstring<'a>(member: &'a Member, options: &AutoOptions) -> Option<&'a str> {
    match member {
        Member::Function(info) => info.docstring.as_deref(),
        Member::Class(class) => {
            if options.class_doc_from == ClassDocFrom::Init {
                init_docstring(class)
            } else if options.class_doc_from == ClassDocFrom::Both {
                // Both docstrings are concatenated by the caller via a
                // synthetic string; handled in `emit_class` below.
                class.docstring.as_deref()
            } else {
                class.docstring.as_deref()
            }
        }
        Member::Attribute(info) => info.docstring.as_deref(),
    }
}

fn init_docstring(class: &ClassInfo) -> Option<&str> {
    class.members.iter().find_map(|member| match member {
        Member::Function(info) if info.name == "__init__" => info.docstring.as_deref(),
        _ => None,
    })
}

/// Emits members nested inside a class's `desc_content`.
fn emit_class_children(
    ctx: &mut DirectiveContext<'_>,
    class: &ClassInfo,
    module: &str,
    options: &AutoOptions,
    config: Option<&Config>,
    imports: &std::collections::BTreeMap<String, String>,
) -> HandlerResult {
    let all = None;
    let members = select_members(&class.members, options, all);
    for member in members {
        emit_member(ctx, member, module, options, config, Some(&class.name), imports)?;
    }
    Ok(Vec::new())
}

/// Emits a module docstring at document level, matching upstream `automodule`.
fn emit_module_docstring(
    ctx: &mut DirectiveContext<'_>,
    docstring: Option<&str>,
    config: Option<&Config>,
) -> HandlerResult {
    let Some(rst) = convert(docstring, config) else { return Ok(Vec::new()) };
    if rst.trim().is_empty() {
        return Ok(Vec::new());
    }
    ctx.parse_block_text(&rst, ctx.parent, ctx.line)
}

/// Emits the `py:module` target marker so the domain sets module context and
/// registers a module object (py-modindex).
fn emit_module_target(ctx: &mut DirectiveContext<'_>, name: &str) -> HandlerResult {
    let mut target = Node::new(NodeKind::Target);
    target.attrs.set_str("objtype", "module");
    target.attrs.set_str("py:signature", name);
    target.attrs.set_str("py:options", "{}");
    target.attrs.set_list("ids", vec![format!("module-{}", name)]);
    target.span = Some(ctx.span);
    let id = ctx.doc.add_node(target, Some(ctx.parent))?;
    Ok(vec![id])
}

/// Trait implemented by the auto directives, providing the common name and
/// option specification.
struct AutoDirective {
    name: &'static str,
    objtype: &'static str,
    has_argument: bool,
}

impl AutoDirective {
    const fn new(name: &'static str, objtype: &'static str, has_argument: bool) -> Self {
        Self { name, objtype, has_argument }
    }
}

impl Directive for AutoDirective {
    fn name(&self) -> &'static str {
        self.name
    }

    fn required_arguments(&self) -> usize {
        usize::from(self.has_argument)
    }

    fn final_argument_whitespace(&self) -> bool {
        false
    }

    fn option_spec(&self) -> OptionSpec {
        auto_option_spec()
    }

    fn has_content(&self) -> bool {
        // Upstream's object descriptions (autoclass, autoattribute, ...) accept
        // content such as nested `.. method::` directives.
        true
    }

    fn run(&self, ctx: &mut DirectiveContext<'_>) -> HandlerResult {
        let argument = ctx.argument(0).unwrap_or("").trim();
        let (roots, config) = roots_and_config(ctx);
        let options = auto_options(ctx, config.as_ref());

        if self.objtype == "module" {
            let module = match resolve_module(argument, &roots).and_then(|path| load_cached(argument, &path)) {
                Some(module) => module,
                None => {
                    return ctx.parse_block_text(
                        &format!(".. warning:: autodoc: failed to import module {argument:?}"),
                        ctx.parent,
                        ctx.line,
                    );
                }
            };
            let mut ids = emit_module_target(ctx, &module.name)?;
            ids.extend(emit_module_docstring(ctx, module.docstring.as_deref(), config.as_ref())?);
            let all = if options.ignore_module_all { None } else { module.all.as_deref() };
            let imports = import_map(&module);
            for member in select_members(&module.members, &options, all) {
                ids.extend(emit_member(ctx, member, &module.name, &options, config.as_ref(), None, &imports)?);
            }
            return Ok(ids);
        }

        // Upstream qualifies bare autodoc names with the active `currentmodule`
        // (e.g. `.. currentmodule:: sphinx.application` followed by
        // `.. automethod:: Sphinx.add_js_file`). Try the argument as written
        // first, then the module-qualified spelling.
        let current_module = ctx.global.current_module.read().ok().and_then(|guard| guard.clone());
        let direct = resolve_target(argument, &roots);
        let resolved = direct.or_else(|| {
            current_module
                .as_deref()
                .and_then(|module| resolve_target(&format!("{module}.{argument}"), &roots))
        });
        let (module, object_path) = match resolved {
            Some(target) => target,
            None => {
                return ctx.parse_block_text(
                    &format!(".. warning:: autodoc: failed to import {argument:?}"),
                    ctx.parent,
                    ctx.line,
                );
            }
        };
        let matches = module.find_all(&object_path);
        if matches.is_empty() {
            return Err(DirectiveError::Failed {
                name: self.name.to_owned(),
                detail: format!("{object_path} not found in module {}", module.name),
                span: miette::SourceSpan::new((ctx.span.start as usize).into(), ctx.span.len() as usize),
            });
        };

        let mut ids = Vec::new();
        for member in matches {
        let class_name = match member {
            Member::Function(_) | Member::Attribute(_) => object_path.rsplit_once('.').map(|(class, _)| class.to_owned()),
            Member::Class(_) => None,
        };
        let extra = member_extra(member);
        let docstring: Option<String> = match member {
            Member::Class(class) if options.class_doc_from == ClassDocFrom::Both => {
                // Concatenate init and class docstrings as upstream does.
                let mut combined = String::new();
                if let Some(init) = init_docstring(class) {
                    combined.push_str(init);
                }
                if let Some(class_doc) = class.docstring.as_deref() {
                    if !combined.is_empty() {
                        combined.push_str("\n\n");
                    }
                    combined.push_str(class_doc);
                }
                Some(combined)
            }
            _ => member_docstring(member, &options).map(str::to_owned),
        };
        let rst = convert(docstring.as_deref(), config.as_ref());
        let objtype = if self.objtype == "exception" || self.objtype == "decorator" || self.objtype == "property" || self.objtype == "data" {
            self.objtype
        } else {
            member.objtype()
        };
        let signature = member_signature(member, class_name.as_deref());
        let imports = import_map(&module);
        let directive_content = ctx.content.to_owned();
        ids.extend(emit_object(ctx, objtype, &signature, &module.name, &options, &extra, &imports, rst.as_deref(), |ctx, content_node| {
            if !directive_content.trim().is_empty() {
                ctx.parse_block_text(&directive_content, content_node, ctx.line)?;
            }
            if let Member::Class(class) = member {
                emit_class_children(ctx, class, &module.name, &options, config.as_ref(), &imports)?;
            }
            Ok(Vec::new())
        })?);
        }
        Ok(ids)
    }
}

/// Registers every auto directive plus the napoleon-emitted `.. attribute::` /
/// `.. method::` aliases.
pub fn register_directives(registry: &DirectiveRegistry) {
    let specs: &[(&str, &str)] = &[
        ("automodule", "module"),
        ("autoclass", "class"),
        ("autoexception", "exception"),
        ("autofunction", "function"),
        ("automethod", "method"),
        ("autoproperty", "property"),
        ("autoattribute", "attribute"),
        ("autodata", "data"),
        ("autodecorator", "decorator"),
    ];
    for (name, objtype) in specs {
        let has_argument = true;
        registry.register(AutoDirective::new(name, objtype, has_argument));
    }
    // Napoleon emits `.. attribute::` and `.. method::` for some sections.
    // The canonical unprefixed spellings are registered by
    // `scribe_rst::python_directives::register` (which accepts content and
    // emits a domain object); re-registering them here as `AutoDirective`
    // would reject the documented content and drop the object entirely.
}

