//! Code directives: `code`, `code-block`, `sourcecode`, `literalinclude`, `highlight` (Phase 15).
//!
//! These handlers replace the Phase 06 `LiteralBlock` placeholders registered
//! by `scribe_rst::directives::register_builtins`. They keep the frozen AST
//! intact: the highlighted token stream is serialized into the
//! `__highlight_json` attribute of the emitted `LiteralBlock` node, and the
//! ordinary Docutils attributes (`language`, `force`, `linenos`, `classes`,
//! `ids`, `names`, `source`, `highlight_args`) are populated for writer and
//! pseudo-XML parity.
//!
//! Per-document highlight context (`highlight` directive → language, force and
//! line-number threshold) is stored on the document root under reserved
//! `__highlight_*` attributes. Because `resolve_pending` walks the arena in
//! document order, subsequent code blocks observe the updated setting without
//! any thread-local state. This is the native equivalent of Sphinx's
//! `env.current_document.highlight_language` plus the
//! `HighlightLanguageTransform` post-transform.

use std::collections::BTreeMap;

use miette::SourceSpan;
use scribe_ast::{Document, NodeId, NodeKind};
use scribe_rst::directive_registry::DirectiveContext;
use scribe_rst::option_spec::{normalize, OptionKind, OptionSpec, OptionValue};
use scribe_rst::{Directive, DirectiveError, DirectiveRegistry};

use crate::highlight::{
    highlight_code, parse_line_num_spec, HighlightOptions, HighlightedCode,
};

/// Root attribute holding the language selected by the most recent `highlight`
/// directive (or the config default read lazily by each code block).
const ROOT_LANG: &str = "__highlight_lang";
/// Root attribute holding the `force` flag from the most recent `highlight`.
const ROOT_FORCE: &str = "__highlight_force";
/// Root attribute holding the `linenothreshold` from the most recent `highlight`.
const ROOT_THRESHOLD: &str = "__highlight_linenothreshold";

/// Per-document highlight setting, mirroring Sphinx's `HighlightSetting`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HighlightSetting {
    /// Highlight language (`lang`).
    pub language: String,
    /// Whether strict lexer errors are suppressed (`force`).
    pub force: bool,
    /// Minimum line count that enables automatic line numbers.
    /// `i64::MAX` models `sys.maxsize` (no automatic numbering).
    pub linenothreshold: i64,
}

impl Default for HighlightSetting {
    fn default() -> Self {
        Self {
            language: "default".to_owned(),
            force: false,
            linenothreshold: i64::MAX,
        }
    }
}

/// Describes the directive names installed by [`register_code_directives`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeDirectiveInfo {
    /// `code`, `code-block` and `sourcecode` aliases.
    pub block_names: &'static [&'static str],
    /// File-inclusion directive name.
    pub include_name: &'static str,
    /// Highlight-context directive name.
    pub highlight_name: &'static str,
}

impl CodeDirectiveInfo {
    /// Canonical description of the installed handlers.
    pub const fn new() -> Self {
        Self {
            block_names: &["code", "code-block", "sourcecode"],
            include_name: "literalinclude",
            highlight_name: "highlight",
        }
    }

    /// All registered names.
    pub fn names(&self) -> Vec<&'static str> {
        let mut names = self.block_names.to_vec();
        names.push(self.include_name);
        names.push(self.highlight_name);
        names
    }
}

impl Default for CodeDirectiveInfo {
    fn default() -> Self {
        Self::new()
    }
}

/// Replaces the Phase 06 code placeholders in `registry` with Phase 15 handlers
/// and registers the `highlight` directive.
///
/// Returns the set of installed names for diagnostics/tests. The underlying
/// registry uses interior mutability, so a `&mut` borrow is accepted to match
/// the documented public API while `&DirectiveRegistry` callers can use
/// [`register_highlighting`](crate::register_highlighting) semantics freely.
pub fn register_code_directives(registry: &mut DirectiveRegistry) -> CodeDirectiveInfo {
    let info = CodeDirectiveInfo::new();
    for name in info.block_names {
        registry.register(FnCodeDirective {
            name,
            kind: CodeDirectiveKind::Block,
        });
    }
    registry.register(FnCodeDirective {
        name: info.include_name,
        kind: CodeDirectiveKind::Include,
    });
    registry.register(FnCodeDirective {
        name: info.highlight_name,
        kind: CodeDirectiveKind::Highlight,
    });
    info
}

#[derive(Clone, Copy, Debug)]
enum CodeDirectiveKind {
    Block,
    Include,
    Highlight,
}

struct FnCodeDirective {
    name: &'static str,
    kind: CodeDirectiveKind,
}

impl Directive for FnCodeDirective {
    fn name(&self) -> &'static str {
        self.name
    }

    fn required_arguments(&self) -> usize {
        match self.kind {
            CodeDirectiveKind::Block => 0,
            CodeDirectiveKind::Include | CodeDirectiveKind::Highlight => 1,
        }
    }

    fn optional_arguments(&self) -> usize {
        match self.kind {
            CodeDirectiveKind::Block => 1,
            _ => 0,
        }
    }

    fn final_argument_whitespace(&self) -> bool {
        matches!(self.kind, CodeDirectiveKind::Include)
    }

    fn has_content(&self) -> bool {
        matches!(self.kind, CodeDirectiveKind::Block)
    }

    fn option_spec(&self) -> OptionSpec {
        match self.kind {
            CodeDirectiveKind::Block => block_option_spec(),
            CodeDirectiveKind::Include => include_option_spec(),
            CodeDirectiveKind::Highlight => highlight_option_spec(),
        }
    }

    fn run(&self, ctx: &mut DirectiveContext<'_>) -> Result<Vec<NodeId>, DirectiveError> {
        match self.kind {
            CodeDirectiveKind::Block => run_code_block(ctx, self.name),
            CodeDirectiveKind::Include => run_literal_include(ctx, self.name),
            CodeDirectiveKind::Highlight => run_highlight(ctx),
        }
    }
}

/// Sphinx `optional_int`: empty means auto-dedent (`None`), negatives rejected.
fn optional_int(raw: &str) -> Result<OptionValue, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(OptionValue::Flag);
    }
    let value = trimmed
        .parse::<i64>()
        .map_err(|_| format!("invalid integer literal: \"{raw}\""))?;
    if value < 0 {
        return Err("negative value; must be positive or zero".to_owned());
    }
    Ok(OptionValue::Int(value))
}

fn block_option_spec() -> OptionSpec {
    OptionSpec::empty()
        .with("force", OptionKind::Flag)
        .with("linenos", OptionKind::Flag)
        .with("dedent", OptionKind::Custom("optional_int", optional_int))
        .with("lineno-start", OptionKind::Int)
        .with("emphasize-lines", OptionKind::UnchangedRequired)
        .with("caption", OptionKind::UnchangedRequired)
        .with("class", OptionKind::ClassOption)
        .with("name", OptionKind::Unchanged)
}

fn include_option_spec() -> OptionSpec {
    OptionSpec::empty()
        .with("dedent", OptionKind::Custom("optional_int", optional_int))
        .with("linenos", OptionKind::Flag)
        .with("lineno-start", OptionKind::Int)
        .with("lineno-match", OptionKind::Flag)
        .with("tab-width", OptionKind::Int)
        .with("language", OptionKind::UnchangedRequired)
        .with("force", OptionKind::Flag)
        .with("encoding", OptionKind::Encoding)
        .with("pyobject", OptionKind::UnchangedRequired)
        .with("lines", OptionKind::UnchangedRequired)
        .with("start-after", OptionKind::UnchangedRequired)
        .with("end-before", OptionKind::UnchangedRequired)
        .with("start-at", OptionKind::UnchangedRequired)
        .with("end-at", OptionKind::UnchangedRequired)
        .with("prepend", OptionKind::UnchangedRequired)
        .with("append", OptionKind::UnchangedRequired)
        .with("emphasize-lines", OptionKind::UnchangedRequired)
        .with("caption", OptionKind::Unchanged)
        .with("class", OptionKind::ClassOption)
        .with("name", OptionKind::Unchanged)
        .with("diff", OptionKind::UnchangedRequired)
}

fn highlight_option_spec() -> OptionSpec {
    OptionSpec::empty()
        .with("force", OptionKind::Flag)
        .with("linenothreshold", OptionKind::PositiveInt)
}

/// Reads the current per-document highlight setting from root attributes.
fn current_setting(ctx: &DirectiveContext<'_>) -> HighlightSetting {
    let root = ctx.doc.root();
    let language = root
        .attrs
        .get_str(ROOT_LANG)
        .ok()
        .flatten()
        .map(str::to_owned)
        .or_else(|| {
            ctx.global
                .config
                .as_ref()
                .and_then(|config| config.get_str("highlight_language"))
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "default".to_owned());
    let force = root.attrs.get_bool(ROOT_FORCE).ok().flatten().unwrap_or(false);
    let threshold = root
        .attrs
        .get_int(ROOT_THRESHOLD)
        .ok()
        .flatten()
        .unwrap_or(i64::MAX);
    HighlightSetting {
        language,
        force,
        linenothreshold: threshold,
    }
}

fn store_setting(ctx: &mut DirectiveContext<'_>, setting: &HighlightSetting) {
    let root = ctx.doc.root_mut();
    root.attrs.set_str(ROOT_LANG, &setting.language);
    root.attrs.set_bool(ROOT_FORCE, setting.force);
    root.attrs.set_int(ROOT_THRESHOLD, setting.linenothreshold);
}

fn failed(ctx: &DirectiveContext<'_>, name: &str, detail: impl Into<String>) -> DirectiveError {
    DirectiveError::Failed {
        name: name.to_owned(),
        detail: detail.into(),
        span: SourceSpan::new((ctx.span.start as usize).into(), ctx.span.len() as usize),
    }
}

fn run_highlight(ctx: &mut DirectiveContext<'_>) -> Result<Vec<NodeId>, DirectiveError> {
    let language = ctx.argument(0).unwrap_or("").trim().to_owned();
    let force = ctx.flag("force");
    let linenothreshold = ctx
        .option_int("linenothreshold")
        .unwrap_or(i64::MAX);
    let setting = HighlightSetting {
        language: language.clone(),
        force,
        linenothreshold,
    };
    store_setting(ctx, &setting);

    let id = ctx.add(NodeKind::Highlightlang, ctx.parent)?;
    {
        let node = ctx.doc.get_mut(id)?;
        node.attrs.set_str("lang", &language);
        node.attrs.set_bool("force", force);
        node.attrs.set_int("linenothreshold", linenothreshold);
    }
    Ok(vec![id])
}

/// `textwrap.dedent` equivalent: remove the longest common leading whitespace
/// prefix from every non-blank line.
fn textwrap_dedent(code: &str) -> String {
    let lines: Vec<&str> = code.split('\n').collect();
    let mut margin: Option<usize> = None;
    for line in &lines {
        if line.trim().is_empty() {
            continue;
        }
        let leading = line.chars().take_while(|c| c.is_whitespace()).count();
        margin = Some(margin.map_or(leading, |current| current.min(leading)));
    }
    let margin = margin.unwrap_or(0);
    lines
        .iter()
        .map(|line| {
            if line.trim().is_empty() {
                String::new()
            } else {
                line.chars().skip(margin).collect()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Sphinx `dedent_lines` with an explicit count.
fn dedent_lines(code: &str, count: usize, warnings: &mut Vec<String>) -> String {
    let lines: Vec<&str> = code.split('\n').collect();
    if lines
        .iter()
        .any(|line| !line.chars().take(count).collect::<String>().trim().is_empty())
    {
        warnings.push("non-whitespace stripped by dedent".to_owned());
    }
    lines
        .iter()
        .map(|line| line.chars().skip(count).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn apply_dedent(ctx: &DirectiveContext<'_>, code: &str, warnings: &mut Vec<String>) -> String {
    match ctx.options.get(&normalize("dedent")) {
        Some(OptionValue::Flag) => textwrap_dedent(code),
        Some(OptionValue::Int(value)) => {
            let count = usize::try_from(*value).unwrap_or(usize::MAX);
            dedent_lines(code, count, warnings)
        }
        _ => code.to_owned(),
    }
}

/// Parses `:emphasize-lines:` into 1-based line numbers, matching
/// `CodeBlock.run` / `LiteralIncludeReader` range validation.
fn emphasis_lines(
    ctx: &DirectiveContext<'_>,
    name: &str,
    spec: Option<&str>,
    total: usize,
    warnings: &mut Vec<String>,
) -> Result<Vec<u32>, DirectiveError> {
    let Some(spec) = spec else {
        return Ok(Vec::new());
    };
    let indices = match parse_line_num_spec(spec, total) {
        Ok(indices) => indices,
        Err(message) => return Err(failed(ctx, name, message)),
    };
    if indices.iter().any(|index| *index >= total) {
        warnings.push(format!(
            "line number spec is out of range(1-{total}): '{spec}'"
        ));
    }
    Ok(indices
        .into_iter()
        .filter(|index| *index < total)
        .map(|index| index as u32 + 1)
        .collect())
}

fn effective_linenos(
    requested: bool,
    code: &str,
    setting: &HighlightSetting,
) -> bool {
    if requested {
        return true;
    }
    let newlines = code.as_bytes().iter().filter(|&&byte| byte == b'\n').count() as i64;
    newlines >= setting.linenothreshold.saturating_sub(1)
}

/// Emits the `LiteralBlock` (and optional caption container) and attaches the
/// serialized highlighted payload.
fn emit_block(
    ctx: &mut DirectiveContext<'_>,
    language: &str,
    highlighted: &mut HighlightedCode,
    source: Option<&str>,
    caption: Option<&str>,
) -> Result<Vec<NodeId>, DirectiveError> {
    let custom_classes = ctx.classes();
    let literal = ctx.add_detached(NodeKind::LiteralBlock, ctx.parent, true)?;
    {
        let node = ctx.doc.get_mut(literal)?;
        node.attrs.set_str("xml:space", "preserve");
        node.attrs.set_str("language", language);
        node.attrs.set_bool("force", highlighted.options.force);
        if highlighted.options.linenos {
            node.attrs.set_bool("linenos", true);
        }
        if let Some(source) = source {
            node.attrs.set_str("source", source);
        }
        if !custom_classes.is_empty() {
            node.attrs.set_list("classes", custom_classes.clone());
        }
        if !highlighted.options.emphasize_lines.is_empty() {
            node.attrs.set_list(
                "hl_lines",
                highlighted
                    .options
                    .emphasize_lines
                    .iter()
                    .map(|number| number.to_string())
                    .collect(),
            );
        }
        node.attrs
            .set_int("linenostart", i64::from(highlighted.options.lineno_start));
        highlighted.attach_to_node(node);
    }

    let mut outer = literal;
    if let Some(caption) = caption {
        let container = ctx.add_detached(NodeKind::Container, ctx.parent, true)?;
        {
            let node = ctx.doc.get_mut(container)?;
            node.attrs.set_list("classes", vec!["literal-block-wrapper".to_owned()]);
            node.attrs.set_bool("literal_block", true);
        }
        let caption_node = ctx.add(NodeKind::Caption, container)?;
        ctx.parse_inline_into(caption, caption_node, ctx.line)?;
        ctx.doc.append_child(container, literal)?;
        outer = container;
    }

    // `add_name` in Sphinx targets the caption container when present.
    ctx.set_common(outer)?;
    if outer != literal {
        if let Ok(node) = ctx.doc.get_mut(outer) {
            node.attrs.set_list("classes", vec!["literal-block-wrapper".to_owned()]);
            node.attrs.set_bool("literal_block", true);
        }
    }

    Ok(vec![outer])
}

fn run_code_block(
    ctx: &mut DirectiveContext<'_>,
    name: &str,
) -> Result<Vec<NodeId>, DirectiveError> {
    let setting = current_setting(ctx);
    let language = ctx
        .argument(0)
        .map(str::to_owned)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| setting.language.clone());
    let mut code = ctx.content.trim_end_matches(['\n', '\r']).to_owned();
    let nlines = code.lines().count();

    let spec = ctx.option_str("emphasize-lines");
    let mut warnings = Vec::new();
    let emphasize = emphasis_lines(ctx, name, spec, nlines, &mut warnings)?;
    code = apply_dedent(ctx, &code, &mut warnings);

    let force = ctx.flag("force");
    let lineno_start = ctx.option_int("lineno-start").and_then(|value| u32::try_from(value).ok());
    let linenos = effective_linenos(
        ctx.flag("linenos") || lineno_start.is_some(),
        &code,
        &setting,
    );
    let options = HighlightOptions {
        force,
        linenos,
        lineno_start: lineno_start.unwrap_or(1).max(1),
        emphasize_lines: emphasize,
        lineno_match: false,
    };
    let mut highlighted = highlight_code(&code, &language, &options);
    highlighted.warnings.extend(warnings);

    let caption = ctx.option_str("caption").map(str::to_owned);
    emit_block(
        ctx,
        &language,
        &mut highlighted,
        None,
        caption.as_deref(),
    )
}

/// Reads a file through the injectable resolver and expands tabs.
fn read_source(ctx: &DirectiveContext<'_>, path: &str) -> Result<String, DirectiveError> {
    let text = ctx.global.read_include_encoded(path, ctx.option_str("encoding").unwrap_or("utf-8"))?;
    let tab_width = ctx.option_int("tab-width").unwrap_or(0);
    if tab_width > 0 {
        Ok(expand_tabs(&text, tab_width as usize))
    } else {
        Ok(text)
    }
}

fn expand_tabs(text: &str, width: usize) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let mut column = 0usize;
        for ch in line.chars() {
            if ch == '\t' {
                let spaces = width.saturating_sub(column % width.max(1));
                for _ in 0..spaces {
                    out.push(' ');
                }
                column += spaces;
            } else {
                out.push(ch);
                if ch == '\n' {
                    column = 0;
                } else {
                    column += 1;
                }
            }
        }
    }
    out
}

fn split_include_lines(text: &str) -> Vec<String> {
    let mut lines = text
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned())
        .collect::<Vec<_>>();
    if text.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// Locates Python object tags (`class`/`def`, including dotted methods and
/// decorators) with 1-based start and exclusive end line numbers.
fn find_pyobject_tags(code: &str) -> BTreeMap<String, (usize, usize)> {
    let lines: Vec<&str> = code.lines().collect();
    let mut tags = BTreeMap::new();
    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut pending_decorator: Option<usize> = None;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if trimmed.starts_with('@') {
            if pending_decorator.is_none() {
                pending_decorator = Some(index);
            }
            continue;
        }
        let (keyword, rest) = if let Some(rest) = trimmed.strip_prefix("async def ") {
            ("def", rest)
        } else if let Some(rest) = trimmed.strip_prefix("def ") {
            ("def", rest)
        } else if let Some(rest) = trimmed.strip_prefix("class ") {
            ("class", rest)
        } else {
            continue;
        };
        let name = rest
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .next()
            .unwrap_or("")
            .to_owned();
        if name.is_empty() {
            continue;
        }
        while let Some((parent_indent, _)) = stack.last() {
            if *parent_indent >= indent {
                stack.pop();
            } else {
                break;
            }
        }
        let qualified = if keyword == "def" {
            match stack.last() {
                Some((_, parent)) if !parent.is_empty() => format!("{parent}.{name}"),
                _ => name.clone(),
            }
        } else {
            name.clone()
        };
        let start = pending_decorator.take().unwrap_or(index);
        // Find the end: first subsequent non-blank line with indentation <= this one.
        let mut end = lines.len();
        for (next, candidate) in lines.iter().enumerate().skip(index + 1) {
            if candidate.trim().is_empty() {
                continue;
            }
            let next_indent = candidate.len() - candidate.trim_start().len();
            if next_indent <= indent {
                end = next;
                break;
            }
        }
        tags.insert(qualified, (start + 1, end));
        if keyword == "class" {
            stack.push((indent, name));
        }
    }
    tags
}

/// Returns the canonical `lineno_start` adjustments made by include filters.
fn validate_option_pairs(ctx: &DirectiveContext<'_>, name: &str) -> Result<(), DirectiveError> {
    const PAIRS: &[(&str, &str)] = &[
        ("lineno-match", "lineno-start"),
        ("lineno-match", "append"),
        ("lineno-match", "prepend"),
        ("start-after", "start-at"),
        ("end-before", "end-at"),
        ("diff", "pyobject"),
        ("diff", "lineno-start"),
        ("diff", "lineno-match"),
        ("diff", "lines"),
        ("diff", "start-after"),
        ("diff", "end-before"),
        ("diff", "start-at"),
        ("diff", "end-at"),
    ];
    for (first, second) in PAIRS {
        if ctx.options.contains_key(&normalize(*first)) && ctx.options.contains_key(&normalize(*second)) {
            return Err(failed(
                ctx,
                name,
                format!("Cannot use both \"{first}\" and \"{second}\" options"),
            ));
        }
    }
    Ok(())
}

/// LCS-based unified diff, close to `difflib.unified_diff` for small files.
fn unified_diff(old: &[String], new: &[String], old_name: &str, new_name: &str) -> Vec<String> {
    let n = old.len();
    let m = new.len();
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if old[i] == new[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Kind {
        Equal,
        Delete,
        Insert,
    }
    let mut ops: Vec<(Kind, String)> = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if old[i] == new[j] {
            ops.push((Kind::Equal, old[i].clone()));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            ops.push((Kind::Delete, old[i].clone()));
            i += 1;
        } else {
            ops.push((Kind::Insert, new[j].clone()));
            j += 1;
        }
    }
    while i < n {
        ops.push((Kind::Delete, old[i].clone()));
        i += 1;
    }
    while j < m {
        ops.push((Kind::Insert, new[j].clone()));
        j += 1;
    }

    // Group changed regions with three context lines.
    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, (kind, _))| *kind != Kind::Equal)
        .map(|(index, _)| index)
        .collect();
    let mut out = vec![format!("--- {old_name}"), format!("+++ {new_name}")];
    if changed.is_empty() {
        return out;
    }
    let mut groups: Vec<(usize, usize)> = Vec::new();
    for &index in &changed {
        let start = index.saturating_sub(3);
        let end = (index + 4).min(ops.len());
        if let Some(last) = groups.last_mut() {
            if start <= last.1 {
                last.1 = end;
                continue;
            }
        }
        groups.push((start, end));
    }
    for (start, end) in groups {
        let old_start = ops[..start]
            .iter()
            .filter(|(kind, _)| *kind != Kind::Insert)
            .count()
            + 1;
        let new_start = ops[..start]
            .iter()
            .filter(|(kind, _)| *kind != Kind::Delete)
            .count()
            + 1;
        let old_count = ops[start..end]
            .iter()
            .filter(|(kind, _)| *kind != Kind::Insert)
            .count();
        let new_count = ops[start..end]
            .iter()
            .filter(|(kind, _)| *kind != Kind::Delete)
            .count();
        out.push(format!("@@ -{old_start},{old_count} +{new_start},{new_count} @@"));
        for (kind, text) in &ops[start..end] {
            let prefix = match kind {
                Kind::Equal => ' ',
                Kind::Delete => '-',
                Kind::Insert => '+',
            };
            out.push(format!("{prefix}{text}"));
        }
    }
    out
}

#[allow(clippy::too_many_lines)]
fn run_literal_include(
    ctx: &mut DirectiveContext<'_>,
    name: &str,
) -> Result<Vec<NodeId>, DirectiveError> {
    validate_option_pairs(ctx, name)?;
    let setting = current_setting(ctx);
    let path = ctx.argument(0).unwrap_or("").to_owned();
    let diff_option = ctx.option_str("diff").map(str::to_owned);

    let mut lines = if let Some(old_path) = &diff_option {
        let new_text = read_source(ctx, &path)?;
        let old_text = ctx.global.read_include(old_path)?;
        unified_diff(
            &split_include_lines(&old_text),
            &split_include_lines(&new_text),
            old_path,
            &path,
        )
    } else {
        split_include_lines(&read_source(ctx, &path)?)
    };

    let mut warnings = Vec::new();
    let lineno_match = ctx.flag("lineno-match");
    let mut lineno_start = ctx
        .option_int("lineno-start")
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(1);

    // pyobject filter
    if let Some(pyobject) = ctx.option_str("pyobject") {
        let source = lines.join("\n");
        let tags = find_pyobject_tags(&source);
        let Some(&(start, end)) = tags.get(pyobject) else {
            return Err(failed(
                ctx,
                name,
                format!("Object named '{pyobject}' not found in include file '{path}'"),
            ));
        };
        if lineno_match {
            lineno_start = start as u32;
        }
        lines = lines[start - 1..end.min(lines.len())].to_vec();
    }

    // start-at / start-after
    if let Some(start_marker) = ctx
        .option_str("start-at")
        .or_else(|| ctx.option_str("start-after"))
    {
        let inclusive = ctx.option_str("start-at").is_some();
        let found = lines.iter().position(|line| line.contains(start_marker));
        match found {
            Some(position) => {
                if lineno_match {
                    lineno_start += if inclusive { position as u32 } else { position as u32 + 1 };
                }
                lines = if inclusive {
                    lines[position..].to_vec()
                } else {
                    lines[position + 1..].to_vec()
                };
            }
            None => {
                let kind = if inclusive { "start-at" } else { "start-after" };
                return Err(failed(ctx, name, format!("{kind} pattern not found: {start_marker}")));
            }
        }
    }

    // end-at / end-before
    if let Some(end_marker) = ctx
        .option_str("end-at")
        .or_else(|| ctx.option_str("end-before"))
    {
        let inclusive = ctx.option_str("end-at").is_some();
        let found = lines.iter().position(|line| line.contains(end_marker));
        match found {
            Some(position) => {
                if inclusive || position > 0 {
                    lines = if inclusive {
                        lines[..=position].to_vec()
                    } else {
                        lines[..position].to_vec()
                    };
                }
            }
            None => {
                let kind = if inclusive { "end-at" } else { "end-before" };
                return Err(failed(ctx, name, format!("{kind} pattern not found: {end_marker}")));
            }
        }
    }

    // lines filter
    if let Some(spec) = ctx.option_str("lines") {
        let indices = match parse_line_num_spec(spec, lines.len()) {
            Ok(indices) => indices,
            Err(message) => return Err(failed(ctx, name, message)),
        };
        if indices.iter().any(|index| *index >= lines.len()) {
            warnings.push(format!(
                "line number spec is out of range(1-{}): '{spec}'",
                lines.len()
            ));
        }
        if lineno_match {
            if let Some(first) = indices.first() {
                if indices.iter().enumerate().all(|(offset, index)| first + offset == *index) {
                    lineno_start += *first as u32;
                } else {
                    return Err(failed(
                        ctx,
                        name,
                        "Cannot use \"lineno-match\" with a disjoint set of \"lines\"",
                    ));
                }
            }
        }
        lines = indices
            .into_iter()
            .filter(|index| *index < lines.len())
            .map(|index| lines[index].clone())
            .collect();
        if lines.is_empty() {
            return Err(failed(
                ctx,
                name,
                format!("Line spec '{spec}': no lines pulled from include file '{path}'"),
            ));
        }
    }

    // Preserve the trailing newline across the split/join round-trips so a
    // file of blank lines keeps all of its physical lines (Pygments receives
    // the raw text, including the final newline).
    let code = apply_dedent(ctx, &format!("{}\n", lines.join("\n")), &mut warnings);
    let selected = split_include_lines(&code);
    let mut final_lines = selected;
    if let Some(prepend) = ctx.option_str("prepend") {
        final_lines.insert(0, prepend.to_owned());
    }
    if let Some(append) = ctx.option_str("append") {
        final_lines.push(append.to_owned());
    }
    let code = if final_lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", final_lines.join("\n"))
    };

    let language = if diff_option.is_some() {
        "udiff".to_owned()
    } else {
        ctx.option_str("language")
            .map(str::to_owned)
            .unwrap_or_else(|| setting.language.clone())
    };
    let total = final_lines.len();
    let emphasize = emphasis_lines(ctx, name, ctx.option_str("emphasize-lines"), total, &mut warnings)?;
    let requested_linenos = ctx.flag("linenos")
        || ctx.option_int("lineno-start").is_some()
        || lineno_match;
    let linenos = effective_linenos(requested_linenos, &code, &setting);
    let options = HighlightOptions {
        force: ctx.flag("force"),
        linenos,
        lineno_start: lineno_start.max(1),
        emphasize_lines: emphasize,
        lineno_match,
    };
    let mut highlighted = highlight_code(&code, &language, &options);
    highlighted.warnings.extend(warnings);

    // Upstream wraps in a `literal-block-wrapper` only when `:caption:` is
    // present, using the filename when the option value is empty.
    let caption = ctx.option_str("caption").map(|value| {
        if value.trim().is_empty() {
            path.clone()
        } else {
            value.to_owned()
        }
    });
    emit_block(
        ctx,
        &language,
        &mut highlighted,
        Some(&path),
        caption.as_deref(),
    )
}

/// Returns the source text of a highlighted block from a parsed document.
///
/// Convenience for tests and text writers; the canonical payload accessor is
/// [`HighlightedCode::from_node`].
pub fn highlighted_nodes(document: &Document) -> Vec<(NodeId, HighlightedCode)> {
    document
        .preorder_ids()
        .into_iter()
        .filter_map(|id| {
            let node = document.get(id).ok()?;
            if node.kind != NodeKind::LiteralBlock {
                return None;
            }
            let payload = HighlightedCode::from_node(node)?.ok()?;
            Some((id, payload))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedent_semantics() {
        assert_eq!(textwrap_dedent("  a\n    b\n  c"), "a\n  b\nc");
        let mut warnings = Vec::new();
        assert_eq!(dedent_lines("  ab\n   c", 2, &mut warnings), "ab\n c");
        assert!(warnings.is_empty());
        let mut warnings = Vec::new();
        assert_eq!(dedent_lines("abc\n def", 2, &mut warnings), "c\nef");
        assert_eq!(warnings, vec!["non-whitespace stripped by dedent"]);
    }

    #[test]
    fn pyobject_tags() {
        let source = "class A:\n    def m(self):\n        pass\n\ndef f():\n    pass\n";
        let tags = find_pyobject_tags(source);
        assert_eq!(tags.get("A"), Some(&(1, 4)));
        assert_eq!(tags.get("A.m"), Some(&(2, 4)));
        assert_eq!(tags.get("f"), Some(&(5, 6)));
    }

    #[test]
    fn tabs_and_unified_diff() {
        assert_eq!(expand_tabs("a\tb", 4), "a   b");
        let diff = unified_diff(&["a\n".into(), "b\n".into()], &["a\n".into(), "c\n".into()], "old", "new");
        assert!(diff[0].starts_with("--- old"));
        assert!(diff.iter().any(|line| line.starts_with("-b")));
        assert!(diff.iter().any(|line| line.starts_with("+c")));
    }

    #[test]
    fn option_specs_are_complete() {
        let block = block_option_spec();
        for name in ["force", "linenos", "dedent", "lineno-start", "emphasize-lines", "caption", "class", "name"] {
            assert!(block.contains(name), "missing {name}");
        }
        let include = include_option_spec();
        for name in ["lineno-match", "tab-width", "language", "encoding", "pyobject", "lines", "start-after", "end-before", "start-at", "end-at", "prepend", "append", "diff"] {
            assert!(include.contains(name), "missing {name}");
        }
    }

    #[test]
    fn optional_int_converter() {
        assert_eq!(optional_int(""), Ok(OptionValue::Flag));
        assert_eq!(optional_int(" 4 "), Ok(OptionValue::Int(4)));
        assert!(optional_int("-1").is_err());
    }

    #[test]
    fn error_for_missing_child_is_not_a_panic() {
        // The helper functions used by the directives never index unchecked.
        assert_eq!(split_include_lines("a\nb\n"), vec!["a", "b"]);
        assert!(find_pyobject_tags("").is_empty());
    }
}
