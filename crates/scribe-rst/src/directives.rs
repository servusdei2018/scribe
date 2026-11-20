//! Built-in Docutils and Sphinx directives (Phase 06).
//!
//! Every handler's node output and attributes were transcribed from the pinned
//! oracle: `sphinx/sphinx/directives/{admonitions,other,patches,code}.py`,
//! `sphinx/sphinx/domains/{changeset,std}.py`, `sphinx/sphinx/roles.py` and
//! Docutils 0.23's `parsers/rst/directives/{misc,body,parts,tables,images,
//! admonitions}.py`. Post-transform attributes (`candidates`, `force`,
//! `linenos`, `translation_progress`, ...) are intentionally absent — they are
//! owned by later build phases.

use std::collections::BTreeMap;

use scribe_ast::{NodeId, NodeKind};

use crate::directive_registry::{Directive, DirectiveContext, DirectiveError, DirectiveRegistry};
use crate::option_spec::{OptionKind, OptionSpec};

type HandlerResult = Result<Vec<NodeId>, DirectiveError>;
type Handler = fn(&mut DirectiveContext<'_>) -> HandlerResult;

/// A directive implemented by a plain function, with a declarative signature
/// and option spec. Used for the majority of built-ins.
pub struct FnDirective {
    name: &'static str,
    required: usize,
    optional: usize,
    final_ws: bool,
    spec: OptionSpec,
    has_content: bool,
    handler: Handler,
}

impl FnDirective {
    /// Creates a function-backed directive.
    pub fn new(
        name: &'static str,
        required: usize,
        optional: usize,
        final_ws: bool,
        spec: OptionSpec,
        has_content: bool,
        handler: Handler,
    ) -> Self {
        Self {
            name,
            required,
            optional,
            final_ws,
            spec,
            has_content,
            handler,
        }
    }
}

impl Directive for FnDirective {
    fn name(&self) -> &'static str {
        self.name
    }
    fn required_arguments(&self) -> usize {
        self.required
    }
    fn optional_arguments(&self) -> usize {
        self.optional
    }
    fn final_argument_whitespace(&self) -> bool {
        self.final_ws
    }
    fn option_spec(&self) -> OptionSpec {
        self.spec.clone()
    }
    fn has_content(&self) -> bool {
        self.has_content
    }
    fn run(&self, ctx: &mut DirectiveContext<'_>) -> HandlerResult {
        (self.handler)(ctx)
    }
}

/// A generic admonition (`note`, `warning`, `tip`, ... plus `seealso`).
#[derive(Clone, Copy, Debug)]
pub struct AdmonitionDirective {
    name: &'static str,
    kind: NodeKind,
    titled: bool,
}

impl AdmonitionDirective {
    /// Creates an admonition handler.
    pub const fn new(name: &'static str, kind: NodeKind, titled: bool) -> Self {
        Self { name, kind, titled }
    }
    /// The `note` directive.
    pub const fn note() -> Self {
        Self::new("note", NodeKind::Note, false)
    }
}

impl Directive for AdmonitionDirective {
    fn name(&self) -> &'static str {
        self.name
    }
    fn required_arguments(&self) -> usize {
        usize::from(self.titled)
    }
    fn optional_arguments(&self) -> usize {
        // Docutils' `BaseAdmonition` accepts one optional argument; Sphinx's
        // `seealso` uses it for inline text (`.. seealso:: some text`).
        usize::from(!self.titled)
    }
    fn final_argument_whitespace(&self) -> bool {
        true
    }
    fn option_spec(&self) -> OptionSpec {
        // Upstream uses `directives.unchanged` here: a bare `:collapsible:`
        // (empty value) is valid and means "open".  A strict Choice rejected
        // it, producing a spurious warning on otherwise-clean fixtures.
        OptionSpec::common().with("collapsible", OptionKind::Unchanged)
    }
    fn has_content(&self) -> bool {
        true
    }
    fn run(&self, ctx: &mut DirectiveContext<'_>) -> HandlerResult {
        let parent = ctx.parent;
        let line = ctx.line;
        let id = ctx.add(self.kind, parent)?;
        ctx.set_common(id)?;
        if self.titled {
            let title_text = ctx.argument(0).unwrap_or("").to_owned();
            let title = ctx.add(NodeKind::Title, id)?;
            ctx.parse_inline_into(&title_text, title, line)?;
            if ctx.option_list("class").is_none() {
                let class = format!("admonition-{}", crate::names::make_id(&title_text));
                ctx.doc.get_mut(id)?.attrs.set_list("classes", vec![class]);
            }
        } else if let Some(argument) = ctx.argument(0) {
            let argument = argument.to_owned();
            if !argument.is_empty() {
                let paragraph = ctx.add(NodeKind::Paragraph, id)?;
                ctx.parse_inline_into(&argument, paragraph, line)?;
            }
        }
        ctx.parse_nested_into(id)?;
        Ok(vec![id])
    }
}

/// Registers a new directive name that reuses an existing directive's full
/// behaviour but reports the canonical name in its `type` attribute.
#[derive(Clone, Copy, Debug)]
pub struct AliasDirective {
    name: &'static str,
    canonical: &'static str,
}

impl AliasDirective {
    pub const fn new(name: &'static str, canonical: &'static str) -> Self {
        Self { name, canonical }
    }
}

impl Directive for AliasDirective {
    fn name(&self) -> &'static str { self.name }
    fn required_arguments(&self) -> usize { 1 }
    fn optional_arguments(&self) -> usize { 1 }
    fn final_argument_whitespace(&self) -> bool { true }
    fn has_content(&self) -> bool { true }
    fn run(&self, ctx: &mut DirectiveContext<'_>) -> HandlerResult {
        let inner = VersionDirective::from_canonical(self.canonical);
        inner.run(ctx)
    }
}

/// A `versionadded` / `versionchanged` / `deprecated` / `versionremoved`
/// directive, matching Sphinx's `changeset` domain.
#[derive(Clone, Copy, Debug)]
pub struct VersionDirective {
    name: &'static str,
    kind: NodeKind,
    label: &'static str,
    label_class: &'static str,
}

impl VersionDirective {
    /// Builds the descriptor for a canonical Sphinx change directive name.
    pub fn from_canonical(canonical: &'static str) -> Self {
        match canonical {
            "versionadded" => Self {
                name: "versionadded",
                kind: NodeKind::VersionAdded,
                label: "Added in version",
                label_class: "added",
            },
            "versionchanged" => Self {
                name: "versionchanged",
                kind: NodeKind::VersionChanged,
                label: "Changed in version",
                label_class: "changed",
            },
            "versionremoved" => Self {
                name: "versionremoved",
                kind: NodeKind::VersionModified,
                label: "Removed in version",
                label_class: "removed",
            },
            _ => Self {
                name: "deprecated",
                kind: NodeKind::Deprecated,
                label: "Deprecated since version",
                label_class: "deprecated",
            },
        }
    }
}

impl Directive for VersionDirective {
    fn name(&self) -> &'static str {
        self.name
    }
    fn required_arguments(&self) -> usize {
        1
    }
    fn optional_arguments(&self) -> usize {
        1
    }
    fn final_argument_whitespace(&self) -> bool {
        true
    }
    fn has_content(&self) -> bool {
        true
    }
    fn run(&self, ctx: &mut DirectiveContext<'_>) -> HandlerResult {
        let version = ctx.argument(0).unwrap_or("").to_owned();
        let line = ctx.line;
        let parent = ctx.parent;
        let id = ctx.add(self.kind, parent)?;
        {
            let node = ctx.doc.get_mut(id)?;
            node.attrs.set_str("type", self.name);
            node.attrs.set_str("version", &version);
        }
        let label = format!("{} {}: ", self.label, version);
        let plain_label = format!("{} {}.", self.label, version);
        let has_description = ctx.argument(1).is_some();
        if let Some(description) = ctx.argument(1) {
            let description = description.to_owned();
            let para = ctx.add(NodeKind::Paragraph, id)?;
            ctx.parse_inline_into(&description, para, line)?;
            prepend_version_label(ctx, id, para, &label, self.label_class)?;
        }
        let children = ctx.parse_nested_into(id)?;
        let first_is_paragraph = children
            .first()
            .is_some_and(|&first| ctx.doc.get(first).is_ok_and(|node| node.kind == NodeKind::Paragraph));
        if !has_description && first_is_paragraph {
            // Upstream folds the label into the first content paragraph.
            let first = children[0];
            prepend_version_label(ctx, id, first, &label, self.label_class)?;
        } else if !has_description && children.is_empty() {
            let para = ctx.add(NodeKind::Paragraph, id)?;
            prepend_version_label(ctx, id, para, &plain_label, self.label_class)?;
        }
        Ok(vec![id])
    }
}

fn prepend_version_label(
    ctx: &mut DirectiveContext<'_>,
    container: NodeId,
    paragraph: NodeId,
    label: &str,
    label_class: &str,
) -> Result<(), DirectiveError> {
    let inline = ctx.add(NodeKind::Inline, container)?;
    {
        let node = ctx.doc.get_mut(inline)?;
        node.attrs.set_list(
            "classes",
            vec!["versionmodified".to_owned(), label_class.to_owned()],
        );
    }
    ctx.add_text(label, inline, ctx.line)?;
    // `ctx.add` attached the inline to the versionmodified container; detach
    // it before re-parenting into the first paragraph.
    if let Ok(container_node) = ctx.doc.get_mut(container) {
        container_node.children.retain(|&child| child != inline);
    }
    let node = ctx.doc.get_mut(paragraph)?;
    node.children.insert(0, inline);
    node.attrs.set_bool("translatable", false);
    Ok(())
}

// ----- simple body directives ------------------------------------------------

fn add_body_container(ctx: &mut DirectiveContext<'_>, kind: NodeKind) -> HandlerResult {
    let id = ctx.add(kind, ctx.parent)?;
    ctx.set_common(id)?;
    ctx.parse_nested_into(id)?;
    Ok(vec![id])
}

fn rubric(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let text = ctx.argument(0).unwrap_or("").to_owned();
    let line = ctx.line;
    let id = ctx.add(NodeKind::Rubric, ctx.parent)?;
    ctx.set_common(id)?;
    if let Some(level) = ctx.option_str("heading-level") {
        if let Ok(level) = level.parse::<i64>() {
            ctx.doc.get_mut(id)?.attrs.set_int("heading-level", level);
        }
    }
    ctx.parse_inline_into(&text, id, line)?;
    Ok(vec![id])
}

fn topic(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let title = ctx.argument(0).unwrap_or("").to_owned();
    let line = ctx.line;
    let id = ctx.add(NodeKind::Topic, ctx.parent)?;
    ctx.set_common(id)?;
    if !title.is_empty() {
        let title_id = ctx.add(NodeKind::Title, id)?;
        ctx.parse_inline_into(&title, title_id, line)?;
    }
    ctx.parse_nested_into(id)?;
    Ok(vec![id])
}

fn sidebar(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let title = ctx.argument(0).map(str::to_owned);
    let subtitle = ctx.option_str("subtitle").map(str::to_owned);
    let line = ctx.line;
    let id = ctx.add(NodeKind::Sidebar, ctx.parent)?;
    ctx.set_common(id)?;
    if let Some(title) = title {
        let title_id = ctx.add(NodeKind::Title, id)?;
        ctx.parse_inline_into(&title, title_id, line)?;
    }
    if let Some(subtitle) = subtitle {
        let subtitle_id = ctx.add(NodeKind::Subtitle, id)?;
        ctx.parse_inline_into(&subtitle, subtitle_id, line)?;
    }
    ctx.parse_nested_into(id)?;
    Ok(vec![id])
}

fn container(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let id = ctx.add(NodeKind::Container, ctx.parent)?;
    let mut classes = ctx.argument(0).map_or_else(Vec::new, |arg| {
        arg.split_whitespace()
            .map(crate::names::make_id)
            .filter(|name| !name.is_empty())
            .collect::<Vec<_>>()
    });
    classes.extend(ctx.classes());
    if !classes.is_empty() {
        ctx.doc.get_mut(id)?.attrs.set_list("classes", classes);
    }
    if let Some(name) = ctx.option_str("name") {
        let name = crate::names::fully_normalize_name(name);
        let node = ctx.doc.get_mut(id)?;
        node.attrs.set_list("ids", vec![crate::names::make_id(&name)]);
        node.attrs.set_list("names", vec![name]);
    }
    ctx.parse_nested_into(id)?;
    Ok(vec![id])
}

fn centered(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let text = ctx.argument(0).unwrap_or("").to_owned();
    let line = ctx.line;
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let id = ctx.add(NodeKind::Centered, ctx.parent)?;
    ctx.parse_inline_into(&text, id, line)?;
    Ok(vec![id])
}

fn acks(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let children = ctx.parse_nested()?;
    let bullet = children
        .iter()
        .copied()
        .find(|&id| ctx.doc.get(id).is_ok_and(|node| node.kind == NodeKind::BulletList));
    let Some(bullet) = bullet else {
        return Ok(Vec::new());
    };
    let id = ctx.add(NodeKind::Acks, ctx.parent)?;
    let list = ctx.doc.get_mut(bullet)?;
    let children = list.children.clone();
    list.children.clear();
    for child in children {
        ctx.doc.append_child(id, child)?;
    }
    ctx.doc.remove_node(bullet)?;
    Ok(vec![id])
}

fn hlist(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let columns = ctx.option_int("columns").unwrap_or(2).max(1) as usize;
    let children = ctx.parse_nested()?;
    let bullet = children
        .iter()
        .copied()
        .find(|&id| ctx.doc.get(id).is_ok_and(|node| node.kind == NodeKind::BulletList));
    let Some(bullet) = bullet else {
        return Ok(Vec::new());
    };
    let items = ctx.doc.get(bullet)?.children.clone();
    let id = ctx.add(NodeKind::Hlist, ctx.parent)?;
    ctx.doc.get_mut(id)?.attrs.set_str("ncolumns", columns.to_string());
    let per_column = items.len().div_ceil(columns);
    for chunk in items.chunks(per_column.max(1)) {
        let col = ctx.add(NodeKind::Hlistcol, id)?;
        let list = ctx.add(NodeKind::BulletList, col)?;
        for &item in chunk {
            ctx.doc.append_child(list, item)?;
        }
    }
    ctx.doc.remove_node(bullet)?;
    Ok(vec![id])
}

fn only(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let expression = ctx.argument(0).unwrap_or("").to_owned();
    // Docutils retains the `only` wrapper in the doctree during parsing and
    // evaluates it only in `OnlyNodeTransform` (post priority 50). Keeping the
    // wrapper here is what lets the post phase replace a false branch with an
    // exact id-transfer `comment` instead of silently dropping its content.
    let id = ctx.add(NodeKind::Only, ctx.parent)?;
    {
        let node = ctx.doc.get_mut(id)?;
        node.attrs.set_str("expr", &expression);
        if ctx.only_raise > 0 {
            node.attrs
                .set_int(crate::pending::KEY_ONLY_RAISE, ctx.only_raise);
        }
    }
    ctx.parse_nested_into(id)?;
    Ok(vec![id])
}

fn tabularcolumns(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let spec = ctx.argument(0).unwrap_or("").to_owned();
    let id = ctx.add(NodeKind::TabularColSpec, ctx.parent)?;
    ctx.doc.get_mut(id)?.attrs.set_str("spec", spec);
    Ok(vec![id])
}

// ----- standard-domain object directives -----------------------------------
//
// These are lightweight `desc` nodes: the standard domain's `process_doc`
// (Phase 11) reads `objtype`, `ids` and `names` from them and populates its
// inventory. Full signature rendering stays in the domain crates (Phases
// 12-13); the pseudo-XML shape below mirrors Sphinx's `ObjectDescription`.

fn std_object(ctx: &mut DirectiveContext<'_>, objtype: &str) -> HandlerResult {
    let signature = ctx.argument(0).unwrap_or("").trim().to_owned();
    if signature.is_empty() {
        return Ok(Vec::new());
    }
    // Object names are case-sensitive in Sphinx inventories (`envvar:: HOME`),
    // so keep the signature verbatim; only the generated anchor is lowercased.
    // A comma-separated signature (e.g. `-T, --show-traceback`) declares
    // several aliases, each of which must be registered separately.
    let names: Vec<String> = signature
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        // An option's declared name is its flag; any trailing metavariable
        // (`-D setting=value`) is documentation, not part of the lookup key.
        .map(|name| if objtype == "option" { name.split_whitespace().next().unwrap_or(name) } else { name })
        .map(str::to_owned)
        .collect();
    let names = if names.is_empty() { vec![signature.clone()] } else { names };
    let ids: Vec<String> = names
        .iter()
        .map(|name| crate::names::make_id(&format!("{objtype}-{name}")))
        .collect();
    let desc = ctx.add(NodeKind::Desc, ctx.parent)?;
    {
        let node = ctx.doc.get_mut(desc)?;
        node.attrs.set_list("classes", vec![objtype.to_owned()]);
        node.attrs.set_str("objtype", objtype);
        node.attrs.set_list("ids", ids);
        node.attrs.set_list("names", names);
    }
    let signature_id = ctx.add(NodeKind::DescSignature, desc)?;
    {
        let node = ctx.doc.get_mut(signature_id)?;
        node.attrs.set_list("classes", vec![objtype.to_owned()]);
    }
    ctx.parse_inline_into(&signature, signature_id, ctx.line)?;
    let content = ctx.add(NodeKind::DescContent, desc)?;
    ctx.parse_nested_into(content)?;
    Ok(vec![desc])
}

fn std_program(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let program = ctx.argument(0).unwrap_or("").trim().to_owned();
    if program.is_empty() {
        return Ok(Vec::new());
    }
    let name = program.replace(char::is_whitespace, "-");
    let desc = ctx.add(NodeKind::Desc, ctx.parent)?;
    {
        let node = ctx.doc.get_mut(desc)?;
        node.attrs.set_str("objtype", "program");
        node.attrs.set_list("classes", vec!["program".to_owned()]);
        node.attrs.set_list("names", vec![crate::names::fully_normalize_name(&name)]);
    }
    Ok(vec![desc])
}

fn std_envvar(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    std_object(ctx, "envvar")
}

fn std_confval(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    // `.. confval::` accepts several aliases on continuation lines, e.g.
    // `.. confval:: master_doc\n   root_doc`. Docutils keeps those lines in
    // the directive *content*, so consume them here up to the first option
    // (`:type:`) or blank line and register every alias.
    let mut names: Vec<String> = ctx.arguments().join(" ").split_whitespace().map(str::to_owned).collect();
    let mut rest = ctx.content;
    let mut consumed = 0usize;
    for line in ctx.content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with(':') {
            break;
        }
        names.push(trimmed.to_owned());
        consumed += line.len() + 1;
    }
    if consumed > 0 {
        rest = ctx.content.get(consumed.min(ctx.content.len())..).unwrap_or("").trim_start_matches('\n');
    }
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let signature = names.join(" ");
    let ids: Vec<String> = names.iter().map(|name| crate::names::make_id(&format!("confval-{name}"))).collect();
    let desc = ctx.add(NodeKind::Desc, ctx.parent)?;
    if let Ok(node) = ctx.doc.get_mut(desc) {
        node.attrs.set_list("classes", vec!["confval".to_owned()]);
        node.attrs.set_str("objtype", "confval");
        node.attrs.set_list("ids", ids);
        node.attrs.set_list("names", names);
    }
    let signature_id = ctx.add(NodeKind::DescSignature, desc)?;
    if let Ok(node) = ctx.doc.get_mut(signature_id) {
        node.attrs.set_list("classes", vec!["confval".to_owned()]);
    }
    ctx.parse_inline_into(&signature, signature_id, ctx.line)?;
    let content = ctx.add(NodeKind::DescContent, desc)?;
    ctx.parse_block_text(rest, content, ctx.line)?;
    Ok(vec![desc])
}

fn std_option(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let signature = ctx.argument(0).unwrap_or("").trim().to_owned();
    if signature.is_empty() {
        return Ok(Vec::new());
    }
    let result = std_object(ctx, "option")?;
    if let Some(&desc) = result.first() {
        let names: Vec<String> = signature
            .split(',')
            .map(|part| {
                let option = part.trim().split([' ', '=']).next().unwrap_or("").trim().to_owned();
                crate::names::fully_normalize_name(&option)
            })
            .filter(|name| !name.is_empty())
            .collect();
        if let Ok(node) = ctx.doc.get_mut(desc) {
            node.attrs.set_list("allnames", names);
        }
    }
    Ok(result)
}

fn std_label(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let argument = ctx.argument(0).unwrap_or("").trim().to_owned();
    let name = crate::names::fully_normalize_name(&argument);
    if name.is_empty() {
        return Ok(Vec::new());
    }
    let id = ctx.add(NodeKind::Target, ctx.parent)?;
    {
        let node = ctx.doc.get_mut(id)?;
        node.attrs.set_list("ids", vec![crate::names::make_id(&name)]);
        node.attrs.set_list("names", vec![name]);
    }
    Ok(vec![id])
}

/// `.. index::` — registers index entries but emits no visible document body.
fn std_index(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let entries = ctx.arguments().join(" ");
    if entries.trim().is_empty() {
        return Ok(Vec::new());
    }
    let id = ctx.add(NodeKind::Index, ctx.parent)?;
    if let Ok(node) = ctx.doc.get_mut(id) {
        node.attrs.set_str("entries", format!("['single', '{}', '', '', None]", entries.replace('\'', "\\'")));
        node.span = Some(ctx.span);
    }
    Ok(vec![id])
}

/// `.. describe::` — the old spelling of `.. rst:directive::`-style object
/// descriptions. Emit a generic description node so the content still renders.
fn describe(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let signature = ctx.arguments().join(" ");
    let desc = ctx.add(NodeKind::Desc, ctx.parent)?;
    if let Ok(node) = ctx.doc.get_mut(desc) {
        node.attrs.set_str("objtype", "describe");
        node.span = Some(ctx.span);
    }
    let sig = ctx.add(NodeKind::DescSignature, desc)?;
    if !signature.trim().is_empty() {
        ctx.parse_inline_into(signature.trim(), sig, ctx.line)?;
    }
    let content = ctx.add(NodeKind::DescContent, desc)?;
    ctx.parse_nested_into(content)?;
    Ok(vec![desc])
}

/// `.. moduleauthor::` — records module authorship in the index; no body.
fn moduleauthor(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let author = ctx.arguments().join(" ");
    if author.trim().is_empty() {
        return Ok(Vec::new());
    }
    let id = ctx.add(NodeKind::Index, ctx.parent)?;
    if let Ok(node) = ctx.doc.get_mut(id) {
        node.attrs.set_str("entries", format!("['single', '{}', '', '', None]", author.replace('\'', "\\'")));
        node.span = Some(ctx.span);
    }
    Ok(vec![id])
}

/// Placeholder directives contributed by Sphinx extensions that scribe does
/// not render specially (inheritance diagrams, doctest setup/code/output and
/// the `doctest` directive itself). They preserve their parsed content so no
/// documentation is silently lost and, crucially, do not warn as unknown.
fn inert_content(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    if ctx.content.trim().is_empty() {
        return Ok(Vec::new());
    }
    let paragraph = ctx.add(NodeKind::Paragraph, ctx.parent)?;
    ctx.parse_nested_into(paragraph)?;
    Ok(vec![paragraph])
}

/// `.. glossary::` — Sphinx `sphinx.domains.std.Glossary`.
///
/// Emits the exact doctree hierarchy of the oracle: a `glossary` node holding a
/// single `definition_list` with the `glossary` class; each definition-list item
/// carries one or more `term` nodes followed by a single `definition`. Every
/// `term` owns the `term-<slug>` id and a child `index` node whose `entries`
/// attribute is the oracle's one-element list containing the entry's Python
/// list representation (the pseudoxml writer `serial_escape`s the embedded
/// spaces, matching `sphinx.builders.xml.XMLBuilder.write_doc`).
fn glossary(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    /// One raw term line and its offset within the directive content.
    type RawTerm = (String, u32);
    /// A term group and the definition lines shared by all of its terms.
    type RawEntry = (Vec<RawTerm>, Vec<String>);

    let sorted = ctx.flag("sorted");
    let glossary_id = ctx.add(NodeKind::Glossary, ctx.parent)?;
    ctx.doc.get_mut(glossary_id)?.attrs.set_bool("sorted", sorted);

    // Collect entries from the dedented content. Unindented lines are terms,
    // indented lines form the shared definition of the current entry, and blank
    // lines separate entries. This mirrors `Glossary.run`'s state machine.
    let mut entries: Vec<RawEntry> = Vec::new();
    let mut in_definition = true;
    let mut in_comment = false;
    let mut indent_len = 0usize;
    for (index, line) in ctx.content.lines().enumerate() {
        let line_no = index as u32;
        if line.is_empty() {
            if in_definition {
                if let Some((_, definition)) = entries.last_mut() {
                    definition.push(String::new());
                }
            }
            continue;
        }
        let unindented = !line.chars().next().is_some_and(char::is_whitespace);
        if unindented {
            if line.starts_with(".. ") {
                in_comment = true;
                continue;
            }
            in_comment = false;
            if in_definition {
                entries.push((vec![(line.to_owned(), line_no)], Vec::new()));
                in_definition = false;
            } else if let Some((terms, _)) = entries.last_mut() {
                terms.push((line.to_owned(), line_no));
            }
        } else if in_comment {
            // Continuation line of a comment: ignored.
        } else {
            if !in_definition {
                in_definition = true;
                indent_len = line.len() - line.trim_start().len();
            }
            if let Some((_, definition)) = entries.last_mut() {
                definition.push(line.get(indent_len..).unwrap_or("").to_owned());
            }
        }
    }

    let base_line = ctx.line + 1 + ctx.raw_options.len() as u32;
    let dlist = ctx.add(NodeKind::DefinitionList, glossary_id)?;
    ctx.doc.get_mut(dlist)?.attrs.set_list("classes", vec!["glossary".to_owned()]);

    for (terms, definition) in entries {
        let item = ctx.add(NodeKind::DefinitionListItem, dlist)?;
        for (raw_term, line_offset) in terms {
            // Upstream splits on ` +: +` and uses only the first classifier as
            // the index key; the remaining ones are not rendered on the doctree.
            let parts = crate::lists::split_classifiers(&raw_term);
            let inline_term = parts.first().cloned().unwrap_or_default();
            let index_key = parts.get(1).cloned();

            let term_id = ctx.add(NodeKind::Term, item)?;
            let _ = ctx.parse_inline_into(&inline_term, term_id, base_line + line_offset)?;
            let term_text = {
                let doc: &scribe_ast::Document = ctx.doc;
                doc.get(term_id).map(|node| node.astext(doc)).unwrap_or_default()
            };
            let id = format!("term-{}", crate::names::make_id(&term_text));
            ctx.doc.get_mut(term_id)?.attrs.set_list("ids", vec![id.clone()]);

            let index_id = ctx.add(NodeKind::Index, term_id)?;
            let entries_repr = match index_key {
                Some(key) => format!("['single', '{term_text}', '{id}', 'main', '{key}']"),
                None => format!("['single', '{term_text}', '{id}', 'main', None]"),
            };
            ctx.doc.get_mut(index_id)?.attrs.set_list("entries", vec![entries_repr]);
        }

        let definition_id = ctx.add(NodeKind::Definition, item)?;
        let definition_text = definition.join("\n");
        if !definition_text.trim().is_empty() {
            ctx.parse_block_text(&definition_text, definition_id, base_line)?;
        }
    }

    Ok(vec![glossary_id])
}

fn productionlist(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let owned_argument = ctx.argument(0).unwrap_or("").replace("\\\n", "");
    let mut lines: Vec<&str> = owned_argument.lines().collect();

    // Upstream `ProductionList.production_group`: when the first line has no
    // ':' it names the production group and is removed from the rule list.
    let mut group = "";
    if lines.first().is_some_and(|line| !line.contains(':')) {
        group = lines.first().copied().unwrap_or("").trim();
        lines.remove(0);
    }
    let definitions: Vec<(String, String)> = lines
        .iter()
        .filter(|line| line.contains(':'))
        .map(|line| {
            let (name, tokens) = line.split_once(':').unwrap_or((line, ""));
            (name.trim().to_owned(), tokens.trim().to_owned())
        })
        .collect();
    let max_len = definitions
        .iter()
        .map(|(name, _)| name.len())
        .max()
        .unwrap_or(0);

    let id = ctx.add(NodeKind::Productionlist, ctx.parent)?;
    for (name, tokens) in definitions {
        let production = ctx.add(NodeKind::Production, id)?;
        {
            let node = ctx.doc.get_mut(production)?;
            node.attrs.set_str("tokenname", &name);
            node.attrs.set_str("xml:space", "preserve");
        }
        if !name.is_empty() {
            let target = ctx.add(NodeKind::LiteralStrong, production)?;
            let target_id = grammar_token_id(group, &name);
            ctx.doc
                .get_mut(target)?
                .attrs
                .set_list("ids", vec![target_id]);
            ctx.add_text(&name, target, ctx.line)?;
        }
        let pad = max_len.saturating_sub(name.len()) + 5;
        let separator = format!("{:>pad$}", " ::= ");
        ctx.add_text(separator, production, ctx.line)?;
        add_production_tokens(ctx, production, &tokens, group)?;
        ctx.add_text("\n", production, ctx.line)?;
    }
    Ok(vec![id])
}

/// Sphinx's `_make_id` preserves case (unlike docutils' `nodes.make_id`).
fn grammar_token_id(group: &str, name: &str) -> String {
    let raw = if group.is_empty() {
        format!("grammar-token-{name}")
    } else {
        format!("grammar-token-{group}-{name}")
    };
    let mut result = String::new();
    let mut previous_dash = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' {
            result.push(ch);
            previous_dash = false;
        } else if !previous_dash {
            result.push('-');
            previous_dash = true;
        }
    }
    result.trim_matches('-').to_owned()
}

/// Matches upstream's `token_re = r'`((~?[\w-]*:)?\w+)`'` for a single token.
fn is_grammar_token(token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    let (prefix, name) = match token.split_once(':') {
        Some((prefix, name)) => (prefix, name),
        None => ("", token),
    };
    let prefix = prefix.strip_prefix('~').unwrap_or(prefix);
    prefix.chars().all(|ch| ch.is_alphanumeric() || ch == '_' || ch == '-')
        && !name.is_empty()
        && name.chars().all(|ch| ch.is_alphanumeric() || ch == '_')
}

/// Emulates Sphinx's `token_xrefs`: backticked tokens inside a production
/// definition become `pending_xref(reftype="token")` nodes wrapping a
/// `literal` with `classes=["xref"]`.
fn add_production_tokens(
    ctx: &mut DirectiveContext<'_>,
    parent: NodeId,
    tokens: &str,
    group: &str,
) -> Result<(), DirectiveError> {
    let mut pos = 0usize;
    while pos < tokens.len() {
        let Some(relative_start) = tokens[pos..].find('`') else {
            ctx.add_text(&tokens[pos..], parent, ctx.line)?;
            break;
        };
        let start = pos + relative_start;
        if start > pos {
            ctx.add_text(&tokens[pos..start], parent, ctx.line)?;
        }
        let Some(relative_end) = tokens[start + 1..].find('`') else {
            ctx.add_text(&tokens[start..], parent, ctx.line)?;
            break;
        };
        let end = start + 1 + relative_end;
        let token = &tokens[start + 1..end];
        if is_grammar_token(token) {
            add_token_xref(ctx, parent, token, group)?;
        } else {
            ctx.add_text(&tokens[start..=end], parent, ctx.line)?;
        }
        pos = end + 1;
    }
    Ok(())
}

/// Builds one `pending_xref` exactly as upstream's `token_xrefs` does,
/// including the `~domain:token` / `:token` title and target rules.
fn add_token_xref(
    ctx: &mut DirectiveContext<'_>,
    parent: NodeId,
    token: &str,
    group: &str,
) -> Result<(), DirectiveError> {
    let (title, target) = if let Some((prefix, name)) = token.split_once(':') {
        if prefix.starts_with('~') {
            // `~domain:token` links to the qualified name but displays only the
            // token itself.
            (name.to_owned(), token.trim_start_matches('~').to_owned())
        } else if prefix.is_empty() {
            // A leading ':' is a display modifier: show the bare token.
            (name.to_owned(), name.to_owned())
        } else {
            (token.to_owned(), token.to_owned())
        }
    } else if group.is_empty() {
        (token.to_owned(), token.to_owned())
    } else {
        (token.to_owned(), format!("{group}:{token}"))
    };

    let xref = ctx.add(NodeKind::PendingXref, parent)?;
    {
        let node = ctx.doc.get_mut(xref)?;
        node.attrs.set_str("reftype", "token");
        node.attrs.set_str("refdomain", "std");
        node.attrs.set_str("reftarget", &target);
        node.attrs.set_bool("refwarn", false);
    }
    let literal = ctx.add(NodeKind::Literal, xref)?;
    ctx.doc
        .get_mut(literal)?
        .attrs
        .set_list("classes", vec!["xref".to_owned()]);
    ctx.add_text(&title, literal, ctx.line)?;
    Ok(())
}

// ----- document parts --------------------------------------------------------

fn contents(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let title_text = ctx.argument(0).map(str::to_owned).unwrap_or_else(|| "Contents".to_owned());
    let line = ctx.line;
    let mut classes = vec!["contents".to_owned()];
    classes.extend(ctx.classes());
    if ctx.flag("local") {
        classes.push("local".to_owned());
    }
    let id = ctx.add(NodeKind::Topic, ctx.parent)?;
    {
        let node = ctx.doc.get_mut(id)?;
        node.attrs.set_list("classes", classes);
        node.attrs.set_list("names", vec![crate::names::fully_normalize_name(&title_text)]);
    }
    // Upstream omits the title entirely for a local ToC without an explicit
    // argument (docutils `directives.parts.Contents.run`).
    if !(ctx.flag("local") && ctx.argument(0).is_none()) {
        let title = ctx.add(NodeKind::Title, id)?;
        ctx.parse_inline_into(&title_text, title, line)?;
    }
    let details: BTreeMap<String, String> = ctx
        .raw_options
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let pending = ctx.add(NodeKind::Pending, id)?;
    ctx.doc.get_mut(pending)?.attrs.set_str("__directive", "contents");
    ctx.doc
        .get_mut(pending)?
        .attrs
        .set_str("__details", serde_json::to_string(&details).unwrap_or_else(|_| "{}".to_owned()));
    // A `pending` node without a directive name is a transform placeholder and
    // is left for the Phase 10 post-transform pipeline.
    Ok(vec![id])
}

fn sectnum(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let details: BTreeMap<String, String> = ctx
        .raw_options
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let id = ctx.add(NodeKind::Pending, ctx.parent)?;
    let node = ctx.doc.get_mut(id)?;
    node.attrs
        .set_str("__details", serde_json::to_string(&details).unwrap_or_else(|_| "{}".to_owned()));
    // Additive marker so the Phase 09 numbering pass can attach the current
    // section number to this transform placeholder without re-parsing details.
    node.attrs.set_str("__directive", "sectnum");
    Ok(vec![id])
}

// ----- include / raw ---------------------------------------------------------

fn include(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let path = ctx.argument(0).unwrap_or("").to_owned();
    let mut text = ctx.global.read_include(&path)?;
    let start_line = ctx.option_int("start-line").unwrap_or(0).max(0) as usize;
    let end_line = ctx.option_int("end-line");
    if start_line > 0 || end_line.is_some() {
        let lines: Vec<&str> = text.lines().collect();
        let start = start_line.min(lines.len());
        let end = end_line
            .map(|value| (value.max(0) as usize).min(lines.len()))
            .unwrap_or(lines.len());
        text = lines
            .get(start..end.max(start))
            .map_or_else(String::new, |slice| slice.join("\n"));
    }
    if let Some(after) = ctx.option_str("start-after") {
        if let Some(index) = text.find(after) {
            text = text[index + after.len()..].to_owned();
        }
    }
    if let Some(before) = ctx.option_str("end-before") {
        if let Some(index) = text.find(before) {
            text.truncate(index);
        }
    }
    let tab_width = ctx.option_int("tab-width").unwrap_or(8).max(0) as usize;
    if tab_width > 0 {
        text = expand_tabs(&text, tab_width);
    }
    if ctx.flag("literal") || ctx.option_str("code").is_some() {
        let classes = ctx.classes();
        let language = ctx.option_str("code").map(str::to_owned);
        let first_line = ctx.option_int("number-lines").unwrap_or(0).max(0) as u32 + 1;
        let id = ctx.add(NodeKind::LiteralBlock, ctx.parent)?;
        {
            let node = ctx.doc.get_mut(id)?;
            node.attrs.set_str("source", &path);
            node.attrs.set_str("xml:space", "preserve");
            if !classes.is_empty() {
                node.attrs.set_list("classes", classes);
            }
            if let Some(language) = language {
                node.attrs.set_str("language", language);
            }
            node.attrs.set_bool("force", false);
            node.attrs.set_bool("linenos", false);
        }
        ctx.add_text(text, id, first_line)?;
        return Ok(vec![id]);
    }
    let first_line = ctx.option_int("start-line").unwrap_or(0).max(0) as u32 + 1;
    ctx.parse_block_text(&text, ctx.parent, first_line)
}

fn expand_tabs(text: &str, tab_width: usize) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let (body, newline) = line.strip_suffix('\n').map_or((line, ""), |body| (body, "\n"));
        let mut column = 0usize;
        for ch in body.chars() {
            if ch == '\t' {
                let spaces = tab_width.saturating_sub(column % tab_width.max(1)).max(1);
                out.push_str(&" ".repeat(spaces));
                column += spaces;
            } else {
                out.push(ch);
                column += 1;
            }
        }
        out.push_str(newline);
    }
    out
}

fn raw(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let format = ctx
        .argument(0)
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    let mut text = ctx.content.to_owned();
    let mut source = None;
    if text.trim().is_empty() {
        if let Some(path) = ctx.option_str("file") {
            text = ctx.global.read_include(path)?;
            source = Some(path.to_owned());
        }
    }
    let classes = ctx.classes();
    let id = ctx.add(NodeKind::Raw, ctx.parent)?;
    {
        let node = ctx.doc.get_mut(id)?;
        node.attrs.set_str("format", format);
        node.attrs.set_str("xml:space", "preserve");
        if let Some(source) = source {
            node.attrs.set_str("source", source);
        }
        if !classes.is_empty() {
            node.attrs.set_list("classes", classes);
        }
    }
    ctx.add_text(text, id, ctx.line)?;
    Ok(vec![id])
}

// ----- substitution-only / misc directives ----------------------------------

fn replace(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let line = ctx.line;
    let parent = ctx.parent;
    let mut ids = Vec::new();
    if !ctx.content.trim().is_empty() {
        ids.extend(ctx.parse_nested_into(parent)?);
    }
    let argument = ctx.argument(0).map(str::to_owned);
    if let Some(argument) = argument.as_deref() {
        if !argument.trim().is_empty() {
            let paragraph = ctx.add(NodeKind::Paragraph, parent)?;
            ids.push(paragraph);
            ctx.parse_inline_into(argument, paragraph, line)?;
        }
    }
    // `replace` produces inline children; the enclosing substitution
    // definition unwraps paragraphs during resolution.
    Ok(ids)
}

fn unicode(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let argument = ctx.arguments().join(" ");
    let clean = argument.split(".. ").next().unwrap_or("");
    let mut text = String::new();
    for code in clean.split_whitespace() {
        text.push_str(&decode_unicode_code(code));
    }
    let line = ctx.line;
    let id = ctx.add(NodeKind::Text, ctx.parent)?;
    ctx.doc.get_mut(id)?.attrs.set_str("text", text);
    let trim = ctx.flag("trim");
    let ltrim = trim || ctx.flag("ltrim");
    let rtrim = trim || ctx.flag("rtrim");
    if let Some(parent) = ctx.doc.parent(id).ok().flatten() {
        let node = ctx.doc.get_mut(parent)?;
        if ltrim {
            node.attrs.set_int("ltrim", 1);
        }
        if rtrim {
            node.attrs.set_int("rtrim", 1);
        }
    }
    let _ = line;
    Ok(vec![id])
}

fn decode_unicode_code(code: &str) -> String {
    let lower = code.to_ascii_lowercase();
    let hex = if let Some(rest) = lower.strip_prefix("0x") {
        rest
    } else if let Some(rest) = lower.strip_prefix("u+") {
        rest
    } else if let Some(rest) = lower.strip_prefix("\\x") {
        rest
    } else if let Some(rest) = lower.strip_prefix("\\u") {
        rest
    } else if let Some(rest) = lower.strip_prefix('x') {
        rest
    } else if let Some(rest) = lower.strip_prefix('u') {
        rest
    } else if let Some(rest) = lower.strip_prefix("&#x") {
        rest.strip_suffix(';').unwrap_or(rest)
    } else {
        return match code.parse::<u32>() {
            Ok(value) => char::from_u32(value).map_or_else(String::new, |ch| ch.to_string()),
            Err(_) => code.to_owned(),
        };
    };
    match u32::from_str_radix(hex, 16).ok().and_then(char::from_u32) {
        Some(ch) => ch.to_string(),
        None => code.to_owned(),
    }
}

fn date(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    // Reproducible builds: Sphinx/Docutils call `time.strftime`; with no
    // controllable clock in the AST we use the build date supplied by config
    // when present, otherwise the ISO 8601 epoch sentinel. Tests exercise the
    // option-free default shape; builders may override via config.
    let format = if ctx.content.trim().is_empty() {
        "%Y-%m-%d".to_owned()
    } else {
        ctx.content.trim().to_owned()
    };
    let configured = ctx
        .global
        .config
        .as_deref()
        .and_then(|config| config.get_str("today"))
        .map(str::to_owned);
    let rendered = configured.unwrap_or_else(|| crate::names::format_strftime(&format));
    let id = ctx.add(NodeKind::Text, ctx.parent)?;
    ctx.doc.get_mut(id)?.attrs.set_str("text", rendered);
    Ok(vec![id])
}

fn class_directive(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let classes = ctx.classes();
    if !ctx.content.trim().is_empty() {
        let ids = ctx.parse_nested()?;
        for id in &ids {
            let node = ctx.doc.get_mut(*id)?;
            let mut merged = node.attrs.get_list("classes")?.unwrap_or_default();
            merged.extend(classes.iter().cloned());
            node.attrs.set_list("classes", merged);
        }
        Ok(ids)
    } else {
        // With no content, Docutils inserts a `pending` node carrying the
        // ClassAttribute transform. Phase 10 applies it to the next element.
        let id = ctx.add(NodeKind::Pending, ctx.parent)?;
        let details = serde_json::json!({ "class": classes, "directive": "class" });
        ctx.doc
            .get_mut(id)?
            .attrs
            .set_str("__details", details.to_string());
        Ok(vec![id])
    }
}

fn event_directive(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let signature = ctx.argument(0).unwrap_or("").trim().to_owned();
    let desc = ctx.add(NodeKind::Desc, ctx.parent)?;
    if let Ok(node) = ctx.doc.get_mut(desc) {
        node.attrs.set_str("objtype", "event");
        node.attrs.set_str("desctype", "event");
        node.attrs.set_str("domain", "std");
        if !signature.is_empty() {
            let event_name = signature.split_whitespace().next().unwrap_or(&signature).to_owned();
            node.attrs.set_list("names", vec![event_name.clone()]);
            node.attrs.set_list("ids", vec![crate::names::make_id(&format!("event-{event_name}"))]);
        }
    }
    let sig = ctx.add(NodeKind::DescSignature, desc)?;
    if !signature.is_empty() {
        let name = ctx.add(NodeKind::DescName, sig)?;
        ctx.doc.get_mut(name)?.attrs.set_str("text", &signature);
    }
    let content = ctx.add(NodeKind::DescContent, desc)?;
    ctx.parse_nested_into(content)?;
    Ok(vec![desc])
}

fn role_directive(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let inline = ctx.arguments().join(" ");
    let first = if inline.trim().is_empty() {
        ctx.content.lines().next().unwrap_or("").trim()
    } else {
        inline.trim()
    };
    let (name, base) = match first.split_once('(') {
        Some((name, rest)) => (
            name.trim(),
            rest.trim_end().trim_end_matches(')').trim().to_owned(),
        ),
        None => (first, "generic".to_owned()),
    };
    if !name.is_empty() {
        let language = ctx.option_str("language").map(str::to_owned);
        let classes = ctx.classes();
        if let Ok(mut roles) = ctx.global.custom_roles.write() {
            roles.insert(
                name.to_owned(),
                crate::role_registry::CustomRoleInfo { base, language, classes },
            );
        }
    }
    Ok(Vec::new())
}

fn default_role_directive(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let role = ctx.argument(0).map(str::to_owned);
    if let Ok(mut current) = ctx.global.default_role.write() {
        *current = role;
    }
    Ok(Vec::new())
}

fn title_directive(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let title = ctx.argument(0).map(str::to_owned);
    if let Some(title) = title {
        // Docutils' `.. title::` directive stores its argument in the
        // document's attribute dict (no node is emitted). Mirror that on the
        // root node so `pseudoxml` reports `<document title="...">`, and keep
        // `DocumentMetadata::title` as the long title used for HTML `<title>`.
        ctx.doc.metadata.title = Some(title.clone());
        ctx.doc.root_mut().attrs.set_str("title", title);
    }
    Ok(Vec::new())
}

fn meta(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let mut ids = Vec::new();
    for line in ctx.content.lines() {
        let Some(rest) = line.trim().strip_prefix(':') else {
            continue;
        };
        let Some((name, value)) = rest.split_once(':') else {
            continue;
        };
        // Frozen-AST gap: there is no `meta` variant, so the tag is represented
        // as an `inline` carrying a `meta` class (differential tests map it
        // back, exactly as they do for `title_reference`).
        let id = ctx.add(NodeKind::Inline, ctx.parent)?;
        {
            let node = ctx.doc.get_mut(id)?;
            node.attrs.set_list("classes", vec!["meta".to_owned()]);
            node.attrs.set_str("name", name.trim().to_ascii_lowercase());
            node.attrs.set_str("content", value.trim().to_owned());
        }
        ids.push(id);
    }
    Ok(ids)
}

fn math_directive(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let mut latex = ctx.content.trim_end().to_owned();
    if let Some(argument) = ctx.argument(0) {
        if !argument.trim().is_empty() {
            latex = format!("{argument}\n\n{latex}");
        }
    }
    let label = ctx
        .option_str("label")
        .or_else(|| ctx.option_str("name"))
        .unwrap_or("");
    let label = label.to_owned();
    let docname = ctx.global.source_name.clone().unwrap_or_default();
    let no_wrap = ctx.flag("no-wrap") || ctx.flag("nowrap");
    let classes = ctx.classes();
    let mut result = Vec::new();
    if !label.is_empty() {
        let target = ctx.add(NodeKind::Target, ctx.parent)?;
        ctx.doc
            .get_mut(target)?
            .attrs
            .set_str("refid", format!("equation-{label}"));
        result.push(target);
    }
    let id = ctx.add(NodeKind::MathBlock, ctx.parent)?;
    {
        let node = ctx.doc.get_mut(id)?;
        node.attrs.set_str("docname", docname);
        node.attrs.set_str("label", label.clone());
        node.attrs.set_bool("no-wrap", no_wrap);
        node.attrs.set_bool("nowrap", no_wrap);
        node.attrs.set_str("xml:space", "preserve");
        if !label.is_empty() {
            node.attrs.set_list(
                "ids",
                vec![crate::names::make_id(&format!("equation-{label}"))],
            );
        }
        if !classes.is_empty() {
            node.attrs.set_list("classes", classes);
        }
    }
    ctx.add_text(latex, id, ctx.line)?;
    result.push(id);
    Ok(result)
}

// ----- images ----------------------------------------------------------------

fn image(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let uri = ctx.argument(0).unwrap_or("").to_owned();
    let classes = ctx.classes();
    let option_values: Vec<(String, String)> = ["alt", "height", "width", "scale", "align", "loading"]
        .iter()
        .filter_map(|option| ctx.option_str(option).map(|value| ((*option).to_owned(), value.to_owned())))
        .collect();
    let name = ctx.option_str("name").map(crate::names::fully_normalize_name);
    let target = ctx.option_str("target").map(str::to_owned);
    let image_id = ctx.add(NodeKind::Image, ctx.parent)?;
    {
        let node = ctx.doc.get_mut(image_id)?;
        node.attrs.set_str("uri", uri);
        for (option, value) in option_values {
            node.attrs.set_str(option, value);
        }
        if !classes.is_empty() {
            node.attrs.set_list("classes", classes);
        }
        if let Some(name) = name {
            node.attrs.set_list("ids", vec![crate::names::make_id(&name)]);
            node.attrs.set_list("names", vec![name]);
        }
    }
    if let Some(target) = target {
        let reference = ctx.add(NodeKind::Reference, ctx.parent)?;
        if target.ends_with('_') && !target.starts_with("http") {
            ctx.doc
                .get_mut(reference)?
                .attrs
                .set_str("refname", crate::names::fully_normalize_name(target.trim_end_matches('_')));
        } else {
            ctx.doc.get_mut(reference)?.attrs.set_str("refuri", target);
        }
        ctx.doc.append_child(reference, image_id)?;
        // Re-parent the image under the reference (append_child already did it).
        return Ok(vec![reference]);
    }
    Ok(vec![image_id])
}

fn figure(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let image_ids = image(ctx)?;
    let Some(image_id) = image_ids.first().copied() else {
        return Ok(image_ids);
    };
    let figwidth = ctx.option_str("figwidth").map(str::to_owned);
    let mut classes = Vec::new();
    if let Some(figclass) = ctx.option_list("figclass") {
        classes.extend(figclass.iter().cloned());
    }
    let figname = ctx.option_str("figname").map(crate::names::fully_normalize_name);
    if let Some(name) = &figname {
        classes.push(name.clone());
    }
    let align = ctx.option_str("align").map(str::to_owned);
    let figure_id = ctx.add(NodeKind::Figure, ctx.parent)?;
    ctx.doc.append_child(figure_id, image_id)?;
    {
        let node = ctx.doc.get_mut(figure_id)?;
        if let Some(width) = figwidth {
            if width != "image" {
                node.attrs.set_str("width", width);
            }
        }
        if !classes.is_empty() {
            node.attrs.set_list("classes", classes);
        }
        if let Some(align) = align {
            node.attrs.set_str("align", align);
        }
        if let Some(name) = figname {
            node.attrs.set_list("ids", vec![crate::names::make_id(&name)]);
            node.attrs.set_list("names", vec![name]);
        }
    }
    let children = ctx.parse_nested_into(figure_id)?;
    if let Some(&first) = children.first() {
        if ctx.doc.get(first).is_ok_and(|node| node.kind == NodeKind::Paragraph) {
            let node = ctx.doc.get_mut(first)?;
            node.kind = NodeKind::Caption;
        }
    }
    Ok(vec![figure_id])
}

// ----- tables ----------------------------------------------------------------

fn parse_csv(text: &str, delim: char, quote: char, escape: Option<char>, keepspace: bool) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if in_quotes {
            if Some(ch) == escape {
                if let Some(next) = chars.next() {
                    field.push(next);
                }
            } else if ch == quote {
                if chars.peek() == Some(&quote) {
                    chars.next();
                    field.push(quote);
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(ch);
            }
            continue;
        }
        match ch {
            c if c == quote => in_quotes = true,
            c if c == delim => {
                let value = if keepspace { field.clone() } else { field.trim().to_owned() };
                row.push(value);
                field.clear();
            }
            '\n' => {
                let value = if keepspace { field.clone() } else { field.trim().to_owned() };
                row.push(value);
                field.clear();
                if row.iter().any(|cell| !cell.is_empty()) {
                    rows.push(std::mem::take(&mut row));
                } else {
                    row.clear();
                }
            }
            '\r' => {}
            other => field.push(other),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        let value = if keepspace { field } else { field.trim().to_owned() };
        row.push(value);
        rows.push(row);
    }
    rows
}

/// Content of a single cell in a directive-built table.
///
/// `csv-table` cells hold plain inline text (Docutils stores a `StringList`
/// and `nested_parse` turns it into a paragraph). `list-table` cells hold the
/// *parsed body nodes* verbatim: Docutils copies `item.children` straight into
/// the `entry`, so a nested directive (e.g. a nested `list-table`, or a table
/// inside a grid cell) must survive as an AST subtree, not be flattened to
/// text. Keeping the nodes also lets nested `Pending` directives be resolved
/// by the outer fixpoint pass in a subsequent iteration.
#[derive(Clone)]
enum DirectiveCell {
    Text(String),
    Nodes(Vec<NodeId>),
}

fn build_table(
    ctx: &mut DirectiveContext<'_>,
    title: Option<&str>,
    header_rows: &[Vec<DirectiveCell>],
    body_rows: &[Vec<DirectiveCell>],
    widths: &[String],
    stub_columns: usize,
) -> HandlerResult {
    let cols = header_rows
        .first()
        .or_else(|| body_rows.first())
        .map_or(1, Vec::len);
    let line = ctx.line;
    let title = title.map(str::to_owned);
    let mut classes = ctx.classes();
    // Docutils `Body.build_table()` adds a width-style class from the
    // `:widths:` option: `colwidths-auto` for `auto`, `colwidths-given` for an
    // explicit integer list.
    if ctx.option_str("widths") == Some("auto") {
        classes.push("colwidths-auto".to_owned());
    } else if ctx.option_list("widths").is_some() {
        classes.push("colwidths-given".to_owned());
    }
    let width = ctx.option_str("width").map(str::to_owned);
    let align = ctx.option_str("align").map(str::to_owned);
    let table = ctx.add(NodeKind::Table, ctx.parent)?;
    if let Some(title) = title.as_deref() {
        let title_id = ctx.add(NodeKind::Title, table)?;
        ctx.parse_inline_into(title, title_id, line)?;
    }
    {
        let node = ctx.doc.get_mut(table)?;
        if !classes.is_empty() {
            node.attrs.set_list("classes", classes);
        }
        if let Some(width) = width {
            node.attrs.set_str("width", width);
        }
        if let Some(align) = align {
            node.attrs.set_str("align", align);
        }
    }
    let tgroup = ctx.add(NodeKind::Tgroup, table)?;
    ctx.doc.get_mut(tgroup)?.attrs.set_int("cols", cols as i64);
    for index in 0..cols {
        let colspec = ctx.add(NodeKind::Colspec, tgroup)?;
        {
            let node = ctx.doc.get_mut(colspec)?;
            if let Some(width) = widths.get(index) {
                node.attrs.set_str("colwidth", width);
            }
            if index < stub_columns {
                node.attrs.set_bool("stub", true);
            }
        }
    }
    let mut rows = Vec::new();
    for data in header_rows.iter().chain(body_rows.iter()) {
        let row = ctx.add(NodeKind::Row, tgroup)?;
        for cell in data {
            let entry = ctx.add(NodeKind::Entry, row)?;
            match cell {
                DirectiveCell::Text(text) => {
                    let paragraph = ctx.add(NodeKind::Paragraph, entry)?;
                    ctx.parse_inline_into(text, paragraph, line)?;
                }
                DirectiveCell::Nodes(nodes) => {
                    // Move the parsed body nodes into the entry verbatim. This
                    // preserves nested tables and any unresolved `Pending`
                    // directives they contain for the next fixpoint iteration.
                    for &node in nodes {
                        ctx.doc.append_child(entry, node)?;
                    }
                }
            }
        }
        rows.push(row);
    }
    if !header_rows.is_empty() {
        let thead = ctx.add(NodeKind::Thead, tgroup)?;
        for row in rows.iter().take(header_rows.len()) {
            ctx.doc.append_child(thead, *row)?;
        }
    }
    let tbody = ctx.add(NodeKind::Tbody, tgroup)?;
    for row in rows.iter().skip(header_rows.len()) {
        ctx.doc.append_child(tbody, *row)?;
    }
    Ok(vec![table])
}

fn column_widths(ctx: &DirectiveContext<'_>, cols: usize) -> Vec<String> {
    if let Some(widths) = ctx.option_list("widths") {
        return (0..cols)
            .map(|index| widths.get(index).cloned().unwrap_or_else(|| "1".to_owned()))
            .collect();
    }
    if cols == 0 {
        return Vec::new();
    }
    // Docutils `Table.get_column_widths()`: `[100 // n_cols] * n_cols`.
    vec![(100 / cols).to_string(); cols]
}

fn csv_table(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let delim = ctx.option_str("delim").and_then(|s| s.chars().next()).unwrap_or(',');
    let quote = ctx.option_str("quote").and_then(|s| s.chars().next()).unwrap_or('"');
    let escape = ctx.option_str("escape").and_then(|s| s.chars().next());
    let keepspace = ctx.flag("keepspace");
    let mut text = ctx.content.to_owned();
    if text.trim().is_empty() {
        if let Some(path) = ctx.option_str("file") {
            text = ctx.global.read_include(path)?;
        }
    }
    let mut rows = parse_csv(&text, delim, quote, escape, keepspace);
    let header_rows_count = ctx.option_int("header-rows").unwrap_or(0).max(0) as usize;
    let header_option = ctx
        .option_str("header")
        .map(|value| parse_csv(value, delim, quote, escape, keepspace))
        .unwrap_or_default();
    let mut header_rows = header_option;
    header_rows.extend(rows.iter().take(header_rows_count).cloned());
    let body_rows = rows.split_off(header_rows_count);
    let cols = header_rows
        .first()
        .or_else(|| body_rows.first())
        .map_or(1, Vec::len);
    let widths = column_widths(ctx, cols);
    let title = ctx.argument(0).map(str::to_owned);
    let header = header_rows
        .into_iter()
        .map(|row| row.into_iter().map(DirectiveCell::Text).collect())
        .collect::<Vec<_>>();
    let body = body_rows
        .into_iter()
        .map(|row| row.into_iter().map(DirectiveCell::Text).collect())
        .collect::<Vec<_>>();
    let stub = ctx.option_int("stub-columns").unwrap_or(0).max(0) as usize;
    build_table(ctx, title.as_deref(), &header, &body, &widths, stub)
}

fn list_table(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let children = ctx.parse_nested()?;
    let bullet = children
        .iter()
        .copied()
        .find(|&id| ctx.doc.get(id).is_ok_and(|node| node.kind == NodeKind::BulletList));
    let Some(bullet) = bullet else {
        return Err(DirectiveError::Failed {
            name: "list-table".to_owned(),
            detail: "exactly one bullet list expected".to_owned(),
            span: miette::SourceSpan::new((ctx.span.start as usize).into(), ctx.span.len() as usize),
        });
    };
    let row_ids = ctx.doc.get(bullet)?.children.clone();
    let mut table_data: Vec<Vec<DirectiveCell>> = Vec::new();
    for row_id in row_ids {
        let item_ids = ctx.doc.get(row_id)?.children.clone();
        let inner = item_ids
            .iter()
            .copied()
            .find(|&id| ctx.doc.get(id).is_ok_and(|node| node.kind == NodeKind::BulletList));
        let mut cells = Vec::new();
        if let Some(inner) = inner {
            for cell_id in ctx.doc.get(inner)?.children.clone() {
                // A list-table cell's list item holds the cell's body nodes.
                // Docutils copies `item.children` into the entry verbatim, so a
                // nested directive (nested list-table / csv-table / table)
                // must survive as a subtree to be resolved by the outer
                // fixpoint. Plain cells are flattened to inline text as before
                // (preserving the historical handling of inline roles such as
                // `:ref:` inside list-table cells).
                let nodes = ctx
                    .doc
                    .get(cell_id)
                    .map_or_else(|_| Vec::new(), |node| node.children.clone());
                // Docutils copies `item.children` verbatim into the entry. Any
                // reference-bearing inline node would lose its identity if the
                // cell were flattened to plain text (``astext()`` only keeps
                // the display label, so ``:ref:`Title <target>` `` becomes
                // ``Title`` and ``[#]_`` becomes ``#``). Preserve the real AST
                // subtree for nested directives *and* for these reference
                // nodes so the domain and footnote transforms can link them.
                let nested = nodes.iter().any(|&node| {
                    ctx.doc.subtree_ids(node).iter().any(|&id| {
                        ctx.doc.get(id).is_ok_and(|n| {
                            matches!(
                                n.kind,
                                NodeKind::Pending
                                    | NodeKind::PendingXref
                                    | NodeKind::FootnoteReference
                                    | NodeKind::CitationReference
                            )
                        })
                    })
                });
                if nested {
                    cells.push(DirectiveCell::Nodes(nodes));
                } else {
                    let text = ctx
                        .doc
                        .get(cell_id)
                        .map_or_else(|_| String::new(), |node| node.astext(ctx.doc));
                    cells.push(DirectiveCell::Text(text.trim().to_owned()));
                }
            }
        }
        table_data.push(cells);
    }
    let cols = table_data.first().map_or(1, Vec::len);
    let header_count = ctx.option_int("header-rows").unwrap_or(0).max(0) as usize;
    let header_rows = table_data.iter().take(header_count).cloned().collect::<Vec<_>>();
    let body_rows = table_data.iter().skip(header_count).cloned().collect::<Vec<_>>();
    let widths = column_widths(ctx, cols);
    let title = ctx.argument(0).map(str::to_owned);
    let stub = ctx.option_int("stub-columns").unwrap_or(0).max(0) as usize;
    // Build first: `build_table` moves each cell's body nodes out of the
    // temporary bullet list and into the table's entries. Only then is it safe
    // to remove the now-empty bullet list subtree (removing it earlier would
    // mark the cell nodes dead before they can be reparented).
    let result = build_table(ctx, title.as_deref(), &header_rows, &body_rows, &widths, stub);
    ctx.doc.remove_node(bullet)?;
    result
}

// ----- footnotes / citations -------------------------------------------------

fn footnote(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let label = ctx.argument(0).unwrap_or("").to_owned();
    let id = ctx.add(NodeKind::Footnote, ctx.parent)?;
    {
        let node = ctx.doc.get_mut(id)?;
        node.attrs.set_list("classes", vec!["footnote".to_owned()]);
        if !label.is_empty() {
            if label == "#" || label == "*" {
                node.attrs.set_str("auto", if label == "*" { "1" } else { "0" });
            } else if let Ok(number) = label.parse::<i64>() {
                node.attrs.set_int("number", number);
            } else {
                node.attrs.set_str("refname", crate::names::fully_normalize_name(&label));
                node.attrs.set_list("names", vec![crate::names::fully_normalize_name(&label)]);
            }
        }
    }
    ctx.parse_nested_into(id)?;
    Ok(vec![id])
}

fn citation(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let label = ctx.argument(0).unwrap_or("").to_owned();
    let id = ctx.add(NodeKind::Citation, ctx.parent)?;
    {
        let node = ctx.doc.get_mut(id)?;
        node.attrs.set_list("names", vec![crate::names::fully_normalize_name(&label)]);
        node.attrs.set_str("refname", crate::names::fully_normalize_name(&label));
    }
    ctx.parse_nested_into(id)?;
    Ok(vec![id])
}

// ----- code placeholders (full highlighting is Phase 15) --------------------

fn literal_block_placeholder(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let language = ctx.argument(0).map(str::to_owned).unwrap_or_default();
    let content = ctx.content.trim_end().to_owned();
    let mut classes = vec!["code".to_owned()];
    if !language.is_empty() {
        classes.push(language.clone());
    }
    classes.extend(ctx.classes());
    let id = ctx.add(NodeKind::LiteralBlock, ctx.parent)?;
    {
        let node = ctx.doc.get_mut(id)?;
        node.attrs.set_str("xml:space", "preserve");
        node.attrs.set_str("language", language);
        node.attrs.set_bool("force", false);
        node.attrs.set_bool("linenos", false);
        node.attrs.set_list("classes", classes);
    }
    ctx.add_text(content, id, ctx.line)?;
    Ok(vec![id])
}

fn literalinclude(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let path = ctx.argument(0).unwrap_or("").to_owned();
    let text = ctx.global.read_include(&path)?;
    let language = ctx
        .option_str("language")
        .map(str::to_owned)
        .or_else(|| path.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()))
        .unwrap_or_default();
    let classes = ctx.classes();
    let content = text.trim_end().to_owned();
    let id = ctx.add(NodeKind::LiteralBlock, ctx.parent)?;
    {
        let node = ctx.doc.get_mut(id)?;
        node.attrs.set_str("xml:space", "preserve");
        node.attrs.set_str("language", language);
        node.attrs.set_str("source", path);
        node.attrs.set_bool("force", false);
        node.attrs.set_bool("linenos", false);
        if !classes.is_empty() {
            node.attrs.set_list("classes", classes);
        }
    }
    ctx.add_text(content, id, ctx.line)?;
    Ok(vec![id])
}

// ----- registration ----------------------------------------------------------

/// Registers all built-in directives in `registry`.
pub fn register_builtins(registry: &DirectiveRegistry) {
    let common = OptionSpec::common();

    for (name, kind) in [
        ("note", NodeKind::Note),
        ("warning", NodeKind::Warning),
        ("tip", NodeKind::Tip),
        ("important", NodeKind::Important),
        ("caution", NodeKind::Caution),
        ("danger", NodeKind::Danger),
        ("error", NodeKind::Error),
        ("hint", NodeKind::Hint),
        ("attention", NodeKind::Attention),
        ("seealso", NodeKind::Seealso),
    ] {
        registry.register(AdmonitionDirective::new(name, kind, false));
    }
    registry.register(AdmonitionDirective::new("admonition", NodeKind::Admonition, true));

    registry.register(VersionDirective {
        name: "versionadded",
        kind: NodeKind::VersionAdded,
        label: "Added in version",
        label_class: "added",
    });
    registry.register(VersionDirective {
        name: "versionchanged",
        kind: NodeKind::VersionChanged,
        label: "Changed in version",
        label_class: "changed",
    });
    registry.register(VersionDirective {
        name: "deprecated",
        kind: NodeKind::Deprecated,
        label: "Deprecated since version",
        label_class: "deprecated",
    });
    registry.register(VersionDirective {
        name: "versionremoved",
        kind: NodeKind::VersionModified,
        label: "Removed in version",
        label_class: "removed",
    });
    // Sphinx 9 aliases (`:rst:dir:`version-added`` etc.). Keep the historical
    // `type` attribute on the emitted node so the writer picks the same CSS.
    for (alias, canonical) in [
        ("version-added", "versionadded"),
        ("version-changed", "versionchanged"),
        ("version-deprecated", "deprecated"),
        ("version-removed", "versionremoved"),
    ] {
        registry.register(AliasDirective::new(alias, canonical));
    }

    // `add_body_container` cannot be used as a fn pointer with a captured kind;
    // register the individual wrappers instead.
    registry.register(FnDirective::new("compound", 0, 0, false, common.clone(), true, compound_body));
    registry.register(FnDirective::new("glossary", 0, 0, false, common.clone().with("sorted", OptionKind::Flag), true, glossary));
    // Phase 11: standard-domain object directives. `program` carries no id
    // (it only sets the `std:program` context); the others register objects.
    registry.register(FnDirective::new("program", 1, 0, true, OptionSpec::empty(), false, std_program));
    registry.register(FnDirective::new("option", 1, 0, true, common.clone(), true, std_option));
    registry.register(FnDirective::new("cmdoption", 1, 0, true, common.clone(), true, std_option));
    registry.register(FnDirective::new("envvar", 1, 0, true, common.clone(), true, std_envvar));
    registry.register(FnDirective::new("confval", 1, 0, true, common.clone()
        .with("type", OptionKind::UnchangedRequired)
        .with("default", OptionKind::UnchangedRequired)
        .with("no-index", OptionKind::Flag)
        .with("no-index-entry", OptionKind::Flag)
        .with("no-contents-entry", OptionKind::Flag)
        .with("no-typesetting", OptionKind::Flag), true, std_confval));
    registry.register(FnDirective::new("label", 1, 0, true, OptionSpec::empty(), false, std_label));
    // Standard/Sphinx directives encountered in the self-hosted docs.
    registry.register(FnDirective::new("index", 1, 0, true, common.clone(), false, std_index));
    registry.register(FnDirective::new("describe", 1, 0, true, common.clone(), true, describe));
    registry.register(FnDirective::new("moduleauthor", 1, 0, true, OptionSpec::empty(), false, moduleauthor));
    registry.register(FnDirective::new("inheritance-diagram", 1, 0, true, common.clone()
        .with("parts", OptionKind::Unchanged)
        .with("top-classes", OptionKind::Unchanged)
        .with("show-inheritance", OptionKind::Flag), true, inert_content));
    for name in ["testsetup", "testcode", "testoutput", "doctest"] {
        registry.register(FnDirective::new(name, 0, 0, false, OptionSpec::empty(), true, inert_content));
    }

    registry.register(FnDirective::new("epigraph", 0, 1, true, OptionSpec::empty(), true, epigraph_body));
    registry.register(FnDirective::new("highlights", 0, 0, false, OptionSpec::empty(), true, highlights_body));
    registry.register(FnDirective::new("pull-quote", 0, 0, false, OptionSpec::empty(), true, pullquote_body));

    registry.register(FnDirective::new("rubric", 1, 0, true, common.clone().with("heading-level", OptionKind::Choice(vec!["1".into(), "2".into(), "3".into(), "4".into(), "5".into(), "6".into()])), false, rubric));
    registry.register(FnDirective::new("topic", 1, 0, true, common.clone(), true, topic));
    registry.register(FnDirective::new("sidebar", 0, 1, true, common.clone().with("subtitle", OptionKind::UnchangedRequired), true, sidebar));
    registry.register(FnDirective::new("container", 0, 1, true, OptionSpec::empty().with("name", OptionKind::Unchanged), true, container));
    registry.register(FnDirective::new("centered", 1, 0, true, OptionSpec::empty(), false, centered));
    registry.register(FnDirective::new("acks", 0, 0, false, OptionSpec::empty(), true, acks));
    registry.register(FnDirective::new("hlist", 0, 0, false, OptionSpec::empty().with("columns", OptionKind::Int), true, hlist));
    // Phase 09: the Sphinx toctree directive. Glob expansion is deferred to
    // the collector in `scribe-core`, which owns `env.found_docs`.
    registry.register(FnDirective::new("toctree", 0, 0, false, crate::toctree::option_spec(), true, crate::toctree::run));
    registry.register(FnDirective::new("only", 1, 0, true, OptionSpec::empty(), true, only));
    registry.register(FnDirective::new("tabularcolumns", 1, 0, true, OptionSpec::empty(), false, tabularcolumns));
    registry.register(FnDirective::new("productionlist", 1, 0, true, OptionSpec::empty(), false, productionlist));

    registry.register(FnDirective::new("contents", 0, 1, true, OptionSpec::empty()
        .with("depth", OptionKind::NonnegativeInt)
        .with("local", OptionKind::Flag)
        .with("backlinks", OptionKind::Choice(vec!["top".into(), "entry".into(), "none".into()]))
        .with("class", OptionKind::ClassOption), false, contents));
    registry.register(FnDirective::new("sectnum", 0, 0, false, OptionSpec::empty()
        .with("depth", OptionKind::Int)
        .with("start", OptionKind::Int)
        .with("prefix", OptionKind::UnchangedRequired)
        .with("suffix", OptionKind::UnchangedRequired), false, sectnum));

    registry.register(FnDirective::new("include", 1, 0, true, OptionSpec::empty()
        .with("literal", OptionKind::Flag)
        .with("code", OptionKind::Unchanged)
        .with("encoding", OptionKind::Encoding)
        .with("parser", OptionKind::Unchanged)
        .with("tab-width", OptionKind::Int)
        .with("start-line", OptionKind::Int)
        .with("end-line", OptionKind::Int)
        .with("start-after", OptionKind::Unchanged)
        .with("end-before", OptionKind::Unchanged)
        .with("number-lines", OptionKind::ValueOrNoneInt)
        .with("class", OptionKind::ClassOption)
        .with("name", OptionKind::Unchanged), false, include));
    registry.register(FnDirective::new("raw", 1, 0, true, OptionSpec::empty()
        .with("file", OptionKind::Path)
        .with("url", OptionKind::Uri)
        .with("encoding", OptionKind::Encoding)
        .with("class", OptionKind::ClassOption), true, raw));

    registry.register(FnDirective::new("replace", 0, 0, false, OptionSpec::empty(), true, replace));
    registry.register(FnDirective::new("unicode", 1, 0, true, OptionSpec::empty()
        .with("trim", OptionKind::Flag)
        .with("ltrim", OptionKind::Flag)
        .with("rtrim", OptionKind::Flag), false, unicode));
    registry.register(FnDirective::new("date", 0, 0, false, OptionSpec::empty(), true, date));
    registry.register(FnDirective::new("class", 1, 0, true, OptionSpec::empty(), true, class_directive));
    registry.register(FnDirective::new("rst-class", 1, 0, true, OptionSpec::empty(), true, class_directive));
    registry.register(FnDirective::new("cssclass", 1, 0, true, OptionSpec::empty(), true, class_directive));
    // Sphinx's `.. event::` documents an application event.  Emit a plain
    // description node so the directive is recognized (and keeps its body);
    // no domain object is registered because upstream's event directive does
    // not create a cross-reference target either.
    registry.register(FnDirective::new("event", 1, 0, true, OptionSpec::empty(), true, event_directive));
    // Docutils' `role` directive inherits a generic option spec that accepts
    // the full set of options forwarded to the base role (notably `:language:`
    // and `:class:`).  Upstream therefore never warns on these options; using
    // `common()` plus `language` mirrors that contract.
    registry.register(FnDirective::new("role", 0, 1, true, OptionSpec::common().with("language", OptionKind::Unchanged), true, role_directive));
    registry.register(FnDirective::new("default-role", 0, 1, false, OptionSpec::empty(), false, default_role_directive));
    registry.register(FnDirective::new("title", 1, 0, true, OptionSpec::empty(), false, title_directive));
    registry.register(FnDirective::new("meta", 0, 0, false, OptionSpec::empty(), true, meta));
    registry.register(FnDirective::new("math", 0, 1, true, OptionSpec::empty()
        .with("label", OptionKind::Unchanged)
        .with("name", OptionKind::Unchanged)
        .with("class", OptionKind::ClassOption)
        .with("no-wrap", OptionKind::Flag)
        .with("nowrap", OptionKind::Flag), true, math_directive));

    registry.register(FnDirective::new("image", 1, 0, true, OptionSpec::empty()
        .with("alt", OptionKind::Unchanged)
        .with("height", OptionKind::Length)
        .with("width", OptionKind::LengthOrPercent("px".into()))
        .with("scale", OptionKind::Percentage)
        .with("align", OptionKind::Choice(vec!["top".into(), "middle".into(), "bottom".into(), "left".into(), "center".into(), "right".into()]))
        .with("target", OptionKind::UnchangedRequired)
        .with("loading", OptionKind::Choice(vec!["embed".into(), "link".into(), "lazy".into()]))
        .with("class", OptionKind::ClassOption)
        .with("name", OptionKind::Unchanged), false, image));
    registry.register(FnDirective::new("figure", 1, 0, true, OptionSpec::empty()
        .with("alt", OptionKind::Unchanged)
        .with("height", OptionKind::Length)
        .with("width", OptionKind::LengthOrPercent("px".into()))
        .with("scale", OptionKind::Percentage)
        .with("align", OptionKind::Choice(vec!["top".into(), "middle".into(), "bottom".into(), "left".into(), "center".into(), "right".into()]))
        .with("target", OptionKind::UnchangedRequired)
        .with("loading", OptionKind::Choice(vec!["embed".into(), "link".into(), "lazy".into()]))
        .with("class", OptionKind::ClassOption)
        .with("name", OptionKind::Unchanged)
        .with("figwidth", OptionKind::Unchanged)
        .with("figclass", OptionKind::ClassOption)
        .with("figname", OptionKind::Unchanged), true, figure));

    registry.register(FnDirective::new("csv-table", 0, 1, true, OptionSpec::empty()
        .with("header-rows", OptionKind::NonnegativeInt)
        .with("stub-columns", OptionKind::NonnegativeInt)
        .with("header", OptionKind::Unchanged)
        .with("width", OptionKind::LengthOrPercent("px".into()))
        .with("widths", OptionKind::AutoOrPositiveIntList)
        .with("file", OptionKind::Path)
        .with("url", OptionKind::Uri)
        .with("encoding", OptionKind::Encoding)
        .with("class", OptionKind::ClassOption)
        .with("name", OptionKind::Unchanged)
        .with("align", OptionKind::Choice(vec!["top".into(), "middle".into(), "bottom".into(), "left".into(), "center".into(), "right".into()]))
        .with("delim", OptionKind::SingleChar)
        .with("keepspace", OptionKind::Flag)
        .with("quote", OptionKind::SingleChar)
        .with("escape", OptionKind::SingleChar), true, csv_table));
    registry.register(FnDirective::new("list-table", 0, 1, true, OptionSpec::empty()
        .with("header-rows", OptionKind::NonnegativeInt)
        .with("stub-columns", OptionKind::NonnegativeInt)
        .with("width", OptionKind::LengthOrPercent("px".into()))
        .with("widths", OptionKind::AutoOrPositiveIntList)
        .with("class", OptionKind::ClassOption)
        .with("name", OptionKind::Unchanged)
        .with("align", OptionKind::Choice(vec!["top".into(), "middle".into(), "bottom".into(), "left".into(), "center".into(), "right".into()])), true, list_table));

    registry.register(FnDirective::new("footnote", 0, 1, true, OptionSpec::empty(), true, footnote));
    registry.register(FnDirective::new("citation", 1, 0, true, OptionSpec::empty(), true, citation));

    for name in ["code", "code-block", "sourcecode"] {
        registry.register(FnDirective::new(name, 0, 1, false, OptionSpec::empty()
            .with("class", OptionKind::ClassOption)
            .with("name", OptionKind::Unchanged)
            .with("number-lines", OptionKind::ValueOrNoneInt)
            .with("force", OptionKind::Flag), true, literal_block_placeholder));
    }
    registry.register(FnDirective::new("literalinclude", 1, 0, true, OptionSpec::empty()
        .with("language", OptionKind::Unchanged)
        .with("class", OptionKind::ClassOption)
        .with("name", OptionKind::Unchanged), false, literalinclude));

    // Phase 12: Python-domain object directives emit skeleton `desc` nodes.
    crate::python_directives::register(registry);
    crate::domain_directives::register(registry);
}

fn compound_body(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    add_body_container(ctx, NodeKind::Compound)
}

fn epigraph_body(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let id = ctx.add(NodeKind::BlockQuote, ctx.parent)?;
    ctx.doc.get_mut(id)?.attrs.set_list("classes", vec!["epigraph".to_owned()]);
    let argument = ctx.arguments().join(" ");
    if !argument.trim().is_empty() {
        let paragraph = ctx.add(NodeKind::Paragraph, id)?;
        ctx.parse_inline_into(argument.trim(), paragraph, ctx.line)?;
    }
    ctx.parse_nested_into(id)?;
    Ok(vec![id])
}

fn highlights_body(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let id = ctx.add(NodeKind::BlockQuote, ctx.parent)?;
    ctx.doc.get_mut(id)?.attrs.set_list("classes", vec!["highlights".to_owned()]);
    ctx.parse_nested_into(id)?;
    Ok(vec![id])
}

fn pullquote_body(ctx: &mut DirectiveContext<'_>) -> HandlerResult {
    let id = ctx.add(NodeKind::BlockQuote, ctx.parent)?;
    ctx.doc.get_mut(id)?.attrs.set_list("classes", vec!["pull-quote".to_owned()]);
    ctx.parse_nested_into(id)?;
    Ok(vec![id])
}
