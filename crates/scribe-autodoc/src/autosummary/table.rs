//! Summary-table generation for `.. autosummary::` (Phase 22).
//!
//! Upstream `Autosummary.get_items()` resolves every listed name through the
//! autodoc documenters and produces `(display_name, signature, summary,
//! real_name)` tuples; `Autosummary.get_table()` lowers them into a two-column
//! `table > tgroup > tbody > row > entry` tree whose first column is a
//! `:py:obj:` cross-reference. The static Phase 21 model supplies the same
//! facts without importing Python: signatures come from `FunctionInfo`, and
//! summaries from the first stanza/sentence of a docstring.

use std::collections::BTreeMap;
use std::path::PathBuf;

use scribe_ast::{NodeId, NodeKind};
use scribe_config::{Config, ConfigValue};
use scribe_rst::directive_registry::{DirectiveContext, DirectiveError};

use crate::directives::load_cached;
use crate::model::{ClassInfo, Member, ModuleInfo};

/// One row of the generated summary table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutosummaryItem {
    /// Display name (with a leading `~` removed).
    pub display_name: String,
    /// Rendered signature (`None` suppresses the signature column part).
    pub signature: Option<String>,
    /// First sentence of the docstring.
    pub summary: String,
    /// Fully-qualified target object name.
    pub real_name: String,
    /// Sphinx object type (`module`, `class`, `function`, `data`, ...).
    pub objtype: String,
    /// Object path relative to its module (used when generating a stub).
    pub stub_objname: String,
}

/// Options of one `.. autosummary::` directive invocation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AutosummaryOptions {
    /// `:toctree:` prefix (when stub pages should be referenced).
    pub toctree: Option<String>,
    /// `:template:` override template name.
    pub template: Option<String>,
    /// `:nosignatures:`.
    pub nosignatures: bool,
    /// `:recursive:` (module traversal for stub pages).
    pub recursive: bool,
    /// `:caption:`.
    pub caption: Option<String>,
    /// Additive `:list-ordered:` (render a numbered list, not a table).
    pub list_ordered: bool,
    /// `:signatures: none|short|long` (takes precedence over `:nosignatures:`).
    pub signatures: Option<String>,
    /// `:class:` values added to the table's class list.
    pub classes: Vec<String>,
}

impl AutosummaryOptions {
    /// Reads the options from a directive context.
    #[must_use]
    pub fn from_context(ctx: &DirectiveContext<'_>) -> Self {
        Self {
            toctree: ctx.raw_options.get("toctree").cloned(),
            template: ctx.raw_options.get("template").cloned(),
            nosignatures: ctx.flag("nosignatures") || ctx.raw_options.contains_key("nosignatures"),
            recursive: ctx.flag("recursive") || ctx.raw_options.contains_key("recursive"),
            caption: ctx.raw_options.get("caption").cloned(),
            list_ordered: ctx.flag("list-ordered") || ctx.raw_options.contains_key("list-ordered"),
            signatures: ctx.raw_options.get("signatures").cloned(),
            classes: ctx.classes(),
        }
    }
}

/// Names listed in an `autosummary` body, transcribed from upstream:
/// `x.strip().split()[0]` for every non-blank line whose first character is
/// `~`, a letter or an underscore.
#[must_use]
pub fn parse_names(content: &str) -> Vec<String> {
    content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let first = *line.as_bytes().first()?;
            if first == b'~' || first.is_ascii_alphabetic() || first == b'_' {
                line.split_whitespace().next().map(str::to_owned)
            } else {
                None
            }
        })
        .collect()
}

/// Resolves an autosummary target permissively.
///
/// Unlike the auto directives, autosummary keeps a name even when the static
/// model cannot find the final object (dynamic instance attributes such as
/// `Foo.value` are common). The longest resolvable module prefix wins.
pub(crate) fn resolve_permissive(argument: &str, roots: &[PathBuf]) -> Option<(ModuleInfo, String)> {
    let segments: Vec<&str> = argument.split('.').filter(|part| !part.is_empty()).collect();
    for count in (1..=segments.len()).rev() {
        let module_name = segments[..count].join(".");
        let Some(path) = crate::discovery::resolve_module(&module_name, roots) else { continue };
        let Some(module) = load_cached(&module_name, &path) else { continue };
        return Some((module, segments[count..].join(".")));
    }
    None
}

/// Builds the item tuples for a list of names.
///
/// Returns the successful items plus one warning per name that could not be
/// resolved (matching upstream's `autosummary: failed to import …` warnings).
#[must_use]
pub fn items_from_names(
    names: &[String],
    roots: &[PathBuf],
    _config: Option<&Config>,
    options: &AutosummaryOptions,
) -> (Vec<AutosummaryItem>, Vec<String>) {
    let mut items = Vec::new();
    let mut warnings = Vec::new();
    for name in names {
        let (display_name, real_name) = match name.strip_prefix('~') {
            Some(short) => (short.rsplit('.').next().unwrap_or(short).to_owned(), short.to_owned()),
            None => (name.clone(), name.clone()),
        };
        let Some((module, object_path)) = resolve_permissive(&real_name, roots) else {
            warnings.push(format!("autosummary: failed to import {real_name}"));
            continue;
        };
        let matched = if object_path.is_empty() {
            None
        } else {
            module.find_all(&object_path).first().copied()
        };
        let (objtype, signature, summary, stub_objname) = if object_path.is_empty() {
            ("module".to_owned(), None, first_summary(module.docstring.as_deref()), String::new())
        } else if let Some(member) = matched {
            let objtype = member_objtype(member, &object_path);
            let signature = signature_for(member, &object_path, options);
            let summary = match member.docstring() {
                Some(doc) => first_summary(Some(doc)),
                None => match attribute_comment_summary_owned(&module, member) {
                    Some(comment) => first_summary(Some(&comment)),
                    None => String::new(),
                },
            };
            (objtype, signature, summary, object_path.clone())
        } else {
            // Dynamic-only member (e.g. `Foo.value` assigned in `__init__`):
            // still emit a row and a base-template stub, as upstream does.
            let objtype = if object_path.contains('.') { "attribute" } else { "data" };
            (objtype.to_owned(), None, String::new(), object_path.clone())
        };
        let signature = apply_signature_option(signature, options);
        items.push(AutosummaryItem {
            display_name,
            signature,
            summary,
            real_name: if object_path.is_empty() { module.name.clone() } else { real_name.clone() },
            objtype,
            stub_objname,
        });
    }
    (items, warnings)
}

fn member_objtype(member: &Member, object_path: &str) -> String {
    match member {
        Member::Class(info) if is_exception(info) => "exception".to_owned(),
        Member::Class(_) => "class".to_owned(),
        Member::Function(info) => info.kind.objtype().to_owned(),
        Member::Attribute(_) => {
            // Module-level variables are `data`; class members are `attribute`.
            if object_path.contains('.') { "attribute".to_owned() } else { "data".to_owned() }
        }
    }
}

pub(crate) fn is_exception(class: &ClassInfo) -> bool {
    class.bases.iter().any(|base| {
        let base = base.rsplit('.').next().unwrap_or(base);
        matches!(base, "Exception" | "BaseException" | "ArithmeticError" | "OSError" | "IOError")
            || base.ends_with("Error")
            || base.ends_with("Exception")
    })
}

fn signature_for(member: &Member, object_path: &str, options: &AutosummaryOptions) -> Option<String> {
    if options.nosignatures || options.signatures.as_deref() == Some("none") {
        return None;
    }
    match member {
        Member::Function(info) => Some(mangle_signature(&info.signature, 50)),
        Member::Class(class) => {
            let init = class.members.iter().find_map(|member| match member {
                Member::Function(info) if info.name == "__init__" => Some(&info.signature),
                _ => None,
            });
            Some(init.map_or_else(|| "()".to_owned(), |sig| mangle_signature(sig, 50)))
        }
        Member::Attribute(_) => {
            let _ = object_path;
            None
        }
    }
}

fn apply_signature_option(sig: Option<String>, options: &AutosummaryOptions) -> Option<String> {
    match options.signatures.as_deref() {
        Some("none") => None,
        Some("short") => sig.map(|sig| if sig == "()" { "()".to_owned() } else { "(…)".to_owned() }),
        _ => sig,
    }
}

/// `sphinx.ext.autosummary.mangle_signature`: compact a signature to its
/// positional/keyword names, dropping annotations, defaults and literals.
#[must_use]
pub fn mangle_signature(signature: &str, max_chars: usize) -> String {
    let mut sig = signature.trim();
    if let Some((name, _)) = sig.split_once(" -> ") {
        sig = name;
    }
    let sig = sig.trim();
    let inner = sig
        .find('(')
        .and_then(|left| sig.rfind(')').map(|right| (left, right)))
        .map_or(sig, |(left, right)| {
            if right > left { &sig[left + 1..right] } else { sig }
        });
    let cleaned = strip_literals(inner);
    // Upstream's `opt_re` search is greedy: it finds the *last* `name=` and
    // drops everything after it (including `*args`/`**kwargs`) before looping
    // on the prefix. Reproduce that by locating the rightmost defaulted part.
    let parts: Vec<&str> = split_top_level(&cleaned).into_iter().map(str::trim).filter(|part| !part.is_empty()).collect();
    let last_default = parts.iter().rposition(|part| split_top_level_once(part, '=').is_some());
    let limit = last_default.map_or(parts.len(), |index| index + 1);
    let mut args: Vec<String> = Vec::new();
    let mut opts: Vec<String> = Vec::new();
    for part in parts.iter().take(limit) {
        let (name, defaulted) = match split_top_level_once(part, '=') {
            Some((name, _)) => (name.trim(), true),
            None => (*part, false),
        };
        let name = split_top_level_once(name, ':').map_or(name, |(name, _)| name.trim()).trim();
        if name.is_empty() {
            continue;
        }
        if defaulted {
            opts.push(name.to_owned());
        } else {
            args.push(name.to_owned());
        }
    }
    let max = max_chars.max(10);
    let mut joined = limited_join(", ", &args, max.saturating_sub(2));
    if !opts.is_empty() {
        if joined.is_empty() {
            joined = format!("[{}]", limited_join(", ", &opts, max.saturating_sub(4)));
        } else if joined.chars().count() + 7 < max {
            joined.push_str(&format!("[, {}]", limited_join(", ", &opts, max - joined.chars().count() - 6)));
        }
    }
    format!("({joined})")
}

fn strip_literals(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(ch) = chars.next() {
        if let Some(active) = quote {
            if ch == active {
                quote = None;
            }
            continue;
        }
        if matches!(ch, '\'' | '"') {
            quote = Some(ch);
            continue;
        }
        if matches!(ch, '<' | '{') {
            let close = if ch == '<' { '>' } else { '}' };
            let mut depth = 1usize;
            for next in chars.by_ref() {
                if next == ch { depth += 1; }
                if next == close {
                    depth -= 1;
                    if depth == 0 { break; }
                }
            }
            continue;
        }
        out.push(ch);
    }
    out
}

fn split_top_level(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (index, ch) in text.char_indices() {
        match ch {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&text[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

fn split_top_level_once(text: &str, needle: char) -> Option<(&str, &str)> {
    let mut depth = 0usize;
    for (index, ch) in text.char_indices() {
        match ch {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' => depth = depth.saturating_sub(1),
            _ if ch == needle && depth == 0 => return Some((&text[..index], &text[index + ch.len_utf8()..])),
            _ => {}
        }
    }
    None
}

/// Upstream `limited_join`: replaces the last fitting item with `...` once the
/// joined string would exceed `max_chars`.
#[must_use]
pub fn limited_join(sep: &str, items: &[String], max_chars: usize) -> String {
    let full = items.join(sep);
    if full.chars().count() < max_chars {
        return full;
    }
    let mut count = 0usize;
    let mut taken = 0usize;
    for item in items {
        count += item.chars().count() + sep.chars().count();
        if count < max_chars.saturating_sub(3) {
            taken += 1;
        } else {
            break;
        }
    }
    let mut kept: Vec<String> = items.iter().take(taken).cloned().collect();
    kept.push("...".to_owned());
    kept.join(sep)
}

const WELL_KNOWN_ABBREVIATIONS: &[&str] = &["et al.", "e.g.", "i.e.", "vs."];

/// First sentence of a docstring's first stanza, mirroring upstream
/// `extract_summary` (including its well-known-abbreviation guard).
#[must_use]
pub fn first_summary(docstring: Option<&str>) -> String {
    let Some(docstring) = docstring else { return String::new() };
    let stanza: Vec<&str> = docstring
        .lines()
        .skip_while(|line| line.trim().is_empty())
        .take_while(|line| !line.trim().is_empty())
        .collect();
    if stanza.is_empty() {
        return String::new();
    }
    let joined = stanza.join(" ");
    let mut sentences: Vec<String> = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = joined.chars().collect();
    let mut index = 0usize;
    while index < chars.len() {
        current.push(chars[index]);
        if chars[index] == '.'
            && chars.get(index + 1).is_some_and(|next| next.is_whitespace())
            && index + 1 < chars.len()
        {
            let candidate = current.trim().to_owned();
            if !WELL_KNOWN_ABBREVIATIONS.iter().any(|abbr| candidate.to_lowercase().ends_with(abbr)) {
                sentences.push(candidate);
                current.clear();
            }
        }
        index += 1;
    }
    if !current.trim().is_empty() {
        sentences.push(current.trim().to_owned());
    }
    let mut summary = sentences.first().cloned().unwrap_or_default();
    if !summary.ends_with('.') && sentences.len() == 1 {
        summary.push('.');
    }
    if summary.ends_with("::") {
        summary.pop();
        summary.push('.');
    }
    summary
}

/// Sphinx attributes documented with `#: comment` rather than a string
/// literal. The static parser only sees string literals, so read the source
/// lines immediately preceding the assignment.
fn attribute_comment_summary_owned(module: &ModuleInfo, member: &Member) -> Option<String> {
    let span = member.span();
    if !span.is_valid() {
        return None;
    }
    let source = std::fs::read_to_string(&module.path).ok()?;
    let start = (span.start as usize).min(source.len());
    let before = &source[..start];
    let mut comments: Vec<String> = Vec::new();
    for line in before.lines().rev() {
        let trimmed = line.trim();
        if let Some(comment) = trimmed.strip_prefix("#:") {
            comments.push(comment.trim().to_owned());
        } else if trimmed.is_empty() {
            continue;
        } else {
            break;
        }
    }
    if comments.is_empty() {
        None
    } else {
        comments.reverse();
        Some(comments.join(" "))
    }
}

/// Emits the summary table (or, with `:list-ordered:`, a numbered list).
pub fn emit_table(
    ctx: &mut DirectiveContext<'_>,
    items: &[AutosummaryItem],
    options: &AutosummaryOptions,
) -> Result<Vec<NodeId>, DirectiveError> {
    if options.list_ordered {
        return emit_list(ctx, items);
    }
    let mut classes = vec!["autosummary".to_owned(), "longtable".to_owned()];
    classes.extend(options.classes.iter().cloned());
    let table = ctx.add(NodeKind::Table, ctx.parent)?;
    {
        let node = ctx.doc.get_mut(table)?;
        node.set_list("classes", classes);
        node.span = Some(ctx.span);
    }
    let group = ctx.add(NodeKind::Tgroup, table)?;
    ctx.doc.get_mut(group)?.set_str("cols", "2");
    for width in ["10", "90"] {
        let colspec = ctx.add(NodeKind::Colspec, group)?;
        ctx.doc.get_mut(colspec)?.set_str("colwidth", width);
    }
    let body = ctx.add(NodeKind::Tbody, group)?;
    for item in items {
        let row = ctx.add(NodeKind::Row, body)?;
        let left = ctx.add(NodeKind::Entry, row)?;
        let left_par = ctx.add(NodeKind::Paragraph, left)?;
        ctx.parse_inline_into(&column_one(item), left_par, ctx.line)?;
        let right = ctx.add(NodeKind::Entry, row)?;
        let right_par = ctx.add(NodeKind::Paragraph, right)?;
        if !item.summary.is_empty() {
            ctx.parse_inline_into(&item.summary, right_par, ctx.line)?;
        }
    }
    Ok(vec![table])
}

fn emit_list(ctx: &mut DirectiveContext<'_>, items: &[AutosummaryItem]) -> Result<Vec<NodeId>, DirectiveError> {
    let list = ctx.add(NodeKind::EnumeratedList, ctx.parent)?;
    {
        let node = ctx.doc.get_mut(list)?;
        node.set_str("enumtype", "arabic");
        node.set_str("start", "1");
        node.set_list("classes", vec!["autosummary".to_owned()]);
    }
    for item in items {
        let list_item = ctx.add(NodeKind::ListItem, list)?;
        let paragraph = ctx.add(NodeKind::Paragraph, list_item)?;
        let mut text = column_one(item);
        if !item.summary.is_empty() {
            text.push_str(" — ");
            text.push_str(&item.summary);
        }
        ctx.parse_inline_into(&text, paragraph, ctx.line)?;
    }
    Ok(vec![list])
}

/// `:py:obj:`{display} <{real}>`\` + escaped signature, matching upstream's
/// `get_table` first-column markup.
fn column_one(item: &AutosummaryItem) -> String {
    let mut text = format!(":py:obj:`{} <{}>`", item.display_name, item.real_name);
    if let Some(sig) = &item.signature {
        text.push(' ');
        text.push_str(sig);
    }
    text
}

/// Serializes the `autosummary_filename_map` config value.
#[must_use]
pub fn filename_map(config: Option<&Config>) -> BTreeMap<String, String> {
    let Some(config) = config else { return BTreeMap::new() };
    let Some(ConfigValue::Dict(dict)) = config.get("autosummary_filename_map") else { return BTreeMap::new() };
    dict.iter()
        .filter_map(|(key, value)| value.get_str().map(|value| (key.clone(), value.to_owned())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_names_follows_upstream_filter() {
        let names = parse_names("~pkg.foo\n   bar  # trailing\n\n  - not a name\n_baz\n");
        assert_eq!(names, vec!["~pkg.foo", "bar", "_baz"]);
    }

    #[test]
    fn mangle_signature_drops_annotations_defaults_and_literals() {
        assert_eq!(mangle_signature("bar(x: Union[int, str], y: int = 1) -> None", 50), "(x[, y])");
        assert_eq!(mangle_signature("f(a, b=2, *args, **kwargs)", 50), "(a[, b])");
        assert_eq!(mangle_signature("f() -> str", 50), "()");
    }

    #[test]
    fn summary_extracts_first_sentence_and_abbreviations() {
        assert_eq!(first_summary(Some("Hello there. More text.")), "Hello there.");
        assert_eq!(first_summary(Some("Use e.g. this. Then that.")), "Use e.g. this.");
        assert_eq!(first_summary(Some("One line")), "One line.");
        assert_eq!(first_summary(None), "");
    }
}
