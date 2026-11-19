//! Plain-text writer and builder (Phase 24).
//!
//! A direct Rust port of Sphinx's `sphinx/writers/text.py` state machine:
//! nested `new_state`/`end_state` frames accumulate wrapped text, lists and
//! admonitions are indented relative to their parent, and section titles are
//! underlined with the glyphs from `text_sectionchars`. Wrapping honours
//! Unicode display width (CJK/fullwidth characters count as two columns) so
//! the non-ASCII fixtures line up the same way the oracle does.
//!
//! Two deliberate extensions over upstream:
//!
//! * `text_width` (default 80) configures the wrap width; upstream hard-codes
//!   `MAXWIDTH = 70`. Setting it to `70` reproduces upstream byte-for-byte.
//! * `unicode_tables` switches the box drawing from Sphinx's ASCII `+---+`
//!   to Unicode `┌─┬┐`, providing the "Unicode box-drawn tables" capability
//!   while keeping Sphinx parity as the default.

use std::collections::BTreeMap;

use scribe_ast::{Document, Node, NodeId, NodeKind, Visitor};
use scribe_config::Config;
use scribe_core::SectionNumbers;

use crate::build_common::{prepare, PreparedBuild};
use crate::error::{OutputError, OutputResult};
use crate::html::{BuildOptions, BuildResult};

/// Stable identity of the plain-text builder.
pub const BUILDER_NAME: &str = "text";
const STDINDENT: usize = 3;

/// Toctree-derived data needed to render a `toctree` node as a bullet list.
#[derive(Clone, Debug, Default)]
pub struct ToctreeContext {
    /// Child docnames for each document.
    pub children: BTreeMap<String, Vec<String>>,
    /// Display title for each document.
    pub titles: BTreeMap<String, String>,
    /// Toctree section number for each document.
    pub numbers: BTreeMap<String, String>,
}

/// Configuration for [`TextTranslator`].
#[derive(Clone, Debug)]
pub struct TextOptions {
    /// Wrap width (upstream `MAXWIDTH`).
    pub width: usize,
    /// Newline sequence.
    pub newline: String,
    /// Underline glyph per section level.
    pub sectionchars: Vec<char>,
    /// Prefix section titles with toctree numbers.
    pub add_secnumbers: bool,
    /// Separator after a section number.
    pub secnumber_suffix: String,
    /// Draw tables with Unicode box characters.
    pub unicode_tables: bool,
    /// Toctree data used to render `toctree` nodes (optional).
    pub toctree: Option<ToctreeContext>,
    /// Docname of the document currently being rendered.
    pub docname: String,
}

impl TextOptions {
    /// Derive options from configuration.
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        let width = match config.get_int("text_width") {
            Some(0) | None => terminal_width().unwrap_or(80),
            Some(n) if n > 0 => n as usize,
            Some(_) => 80,
        };
        let newline = match config.get_str("text_newlines").unwrap_or("unix") {
            "windows" => "\r\n",
            "native" => "\r\n", // deterministic on the Linux build host
            _ => "\n",
        };
        let sectionchars: Vec<char> = config
            .get_str("text_sectionchars")
            .unwrap_or("*=-~\"+`")
            .chars()
            .collect();
        let sectionchars = if sectionchars.is_empty() {
            vec!['*', '=', '-', '~']
        } else {
            sectionchars
        };
        Self {
            width,
            newline: newline.to_owned(),
            sectionchars,
            add_secnumbers: config.get_bool("text_add_secnumbers").unwrap_or(true),
            secnumber_suffix: config
                .get_str("text_secnumber_suffix")
                .unwrap_or(". ")
                .to_owned(),
            unicode_tables: config.get_bool("text_unicode_tables").unwrap_or(false),
            toctree: None,
            docname: String::new(),
        }
    }

    /// Options matching the upstream defaults (`MAXWIDTH = 70`).
    #[must_use]
    pub fn upstream_defaults() -> Self {
        Self {
            width: 70,
            newline: "\n".to_owned(),
            sectionchars: "*=-~\"+`".chars().collect(),
            add_secnumbers: true,
            secnumber_suffix: ". ".to_owned(),
            unicode_tables: false,
            toctree: None,
            docname: String::new(),
        }
    }
}

fn terminal_width() -> Option<usize> {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|width| *width > 0)
}

/// Approximate `docutils.utils.column_width`: East Asian wide/fullwidth
/// characters occupy two columns, combining marks zero, everything else one.
#[must_use]
pub fn column_width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

fn char_width(c: char) -> usize {
    let n = c as u32;
    // Combining / zero-width ranges.
    if (0x0300..=0x036F).contains(&n)
        || (0x1AB0..=0x1AFF).contains(&n)
        || (0x1DC0..=0x1DFF).contains(&n)
        || (0x20D0..=0x20FF).contains(&n)
        || (0xFE20..=0xFE2F).contains(&n)
    {
        return 0;
    }
    // CJK, Hangul, fullwidth forms, emoji.
    if (0x1100..=0x115F).contains(&n)
        || (0x2E80..=0xA4CF).contains(&n)
        || (0xAC00..=0xD7A3).contains(&n)
        || (0xF900..=0xFAFF).contains(&n)
        || (0xFE10..=0xFE19).contains(&n)
        || (0xFE30..=0xFE6F).contains(&n)
        || (0xFF00..=0xFF60).contains(&n)
        || (0xFFE0..=0xFFE6).contains(&n)
        || (0x1F300..=0x1FAFF).contains(&n)
        || (0x20000..=0x3FFFD).contains(&n)
    {
        return 2;
    }
    1
}

/// Greedy word wrap honouring display width; long words are broken.
#[must_use]
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;

    for word in text.split_whitespace() {
        let word_width = column_width(word);
        if current.is_empty() && word_width <= width {
            current.push_str(word);
            current_width = word_width;
            continue;
        }
        if !current.is_empty() && current_width + 1 + word_width <= width {
            current.push(' ');
            current.push_str(word);
            current_width += 1 + word_width;
            continue;
        }
        if !current.is_empty() {
            lines.push(std::mem::take(&mut current));
        }
        // The word did not fit an empty line: break it into chunks.
        let mut rest = word;
        while column_width(rest) > width {
            let (chunk, tail) = split_at_width(rest, width);
            lines.push(chunk.to_owned());
            rest = tail;
        }
        current.push_str(rest);
        current_width = column_width(rest);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn split_at_width(text: &str, width: usize) -> (&str, &str) {
    let mut used = 0usize;
    let mut end = 0usize;
    for (index, c) in text.char_indices() {
        let w = char_width(c);
        if used + w > width {
            break;
        }
        used += w;
        end = index + c.len_utf8();
    }
    if end == 0 {
        // Single character wider than the line: emit it anyway.
        let c = text.chars().next().unwrap_or(' ');
        (&text[..c.len_utf8()], &text[c.len_utf8()..])
    } else {
        (&text[..end], &text[end..])
    }
}

/// One accumulated state item: flowing text or a pre-rendered block.
#[derive(Clone, Debug)]
enum Item {
    /// Wrapped together with adjacent flow items (`itemindent == -1`).
    Flow(String),
    /// A finished block with an additional indent (`itemindent >= 0`).
    Block(i32, Vec<String>),
}

/// Render a frozen doctree to plain text.
#[must_use]
pub fn render(document: &Document, options: &TextOptions) -> String {
    render_with_numbers(document, options, None, "")
}

/// Render a doctree with section numbers supplied by the toctree collector.
#[must_use]
pub fn render_with_numbers(
    document: &Document,
    options: &TextOptions,
    numbers: Option<&SectionNumbers>,
    docname: &str,
) -> String {
    let mut translator = TextTranslator::new(document, options, numbers, docname);
    let _ = translator.walk(document);
    translator.finish()
}

struct TextTranslator<'a> {
    doc: &'a Document,
    options: &'a TextOptions,
    /// Section number lookup for this build.
    numbers: Option<&'a SectionNumbers>,
    docname: &'a str,
    states: Vec<Vec<Item>>,
    stateindent: Vec<usize>,
    list_counter: Vec<i64>,
    sectionlevel: usize,
    lineblocklevel: usize,
    in_production_list: bool,
    title_char: char,
    skip_until: Option<NodeId>,
    /// Label of the footnote/citation currently being closed.
    current_label: String,
    /// Number of flow items in the document root's first state (for title
    /// leading-blank trimming, mirroring upstream's `len(self.states) == 2`).
    first_title_pending: bool,
}

impl<'a> TextTranslator<'a> {
    fn new(
        doc: &'a Document,
        options: &'a TextOptions,
        numbers: Option<&'a SectionNumbers>,
        docname: &'a str,
    ) -> Self {
        Self {
            doc,
            options,
            numbers,
            docname,
            states: vec![Vec::new()],
            stateindent: vec![0],
            list_counter: Vec::new(),
            sectionlevel: 0,
            lineblocklevel: 0,
            in_production_list: false,
            title_char: '*',
            skip_until: None,
            current_label: String::new(),
            first_title_pending: true,
        }
    }

    fn finish(mut self) -> String {
        self.end_state(true, None, None);
        let mut rendered: Vec<String> = Vec::new();
        for item in &self.states[0] {
            let Item::Block(indent, lines) = item else { continue };
            for line in lines {
                if line.is_empty() {
                    rendered.push(String::new());
                } else {
                    rendered.push(format!("{}{}", " ".repeat((*indent).max(0) as usize), line));
                }
            }
        }
        while rendered.last().is_some_and(String::is_empty) {
            rendered.pop();
        }
        if rendered.is_empty() {
            return String::new();
        }
        format!("{}{}", rendered.join(&self.options.newline), self.options.newline)
    }

    fn add_text(&mut self, text: impl Into<String>) {
        if let Some(state) = self.states.last_mut() {
            state.push(Item::Flow(text.into()));
        }
    }

    fn new_state(&mut self, indent: usize) {
        self.states.push(Vec::new());
        self.stateindent.push(indent);
    }

    #[allow(clippy::too_many_arguments)]
    fn end_state(&mut self, do_wrap: bool, end: Option<Vec<String>>, first: Option<String>) {
        let Some(content) = self.states.pop() else { return };
        let maxindent: usize = self.stateindent.iter().sum();
        let indent = self.stateindent.pop().unwrap_or(0);
        let mut result: Vec<Item> = Vec::new();
        let mut toformat = String::new();

        let flush = |toformat: &mut String, result: &mut Vec<Item>, maxindent: usize, indent: usize, do_wrap: bool, end: &Option<Vec<String>>| {
            if toformat.is_empty() {
                return;
            }
            let mut lines = if do_wrap {
                wrap(toformat, self.options.width.saturating_sub(maxindent))
            } else {
                toformat.lines().map(str::to_owned).collect::<Vec<_>>()
            };
            if let Some(suffix) = end {
                lines.extend(suffix.iter().cloned());
            }
            result.push(Item::Block(indent as i32, lines));
            toformat.clear();
        };

        for item in content {
            match item {
                Item::Flow(text) => toformat.push_str(&text),
                Item::Block(extra, lines) => {
                    flush(&mut toformat, &mut result, maxindent, indent, do_wrap, &end);
                    result.push(Item::Block(indent as i32 + extra, lines));
                }
            }
        }
        flush(&mut toformat, &mut result, maxindent, indent, do_wrap, &end);

        if let Some(prefix) = first {
            let mut insertion: Option<Item> = None;
            if let Some(Item::Block(base_indent, lines)) = result.first_mut() {
                let new_indent = *base_indent - indent as i32;
                if lines.len() == 1 && lines[0].is_empty() {
                    insertion = Some(Item::Block(new_indent, vec![prefix]));
                } else if !lines.is_empty() {
                    let first_line = lines.remove(0);
                    insertion = Some(Item::Block(new_indent, vec![format!("{prefix}{first_line}")]));
                }
            }
            if let Some(item) = insertion {
                result.insert(0, item);
            }
        }
        if let Some(state) = self.states.last_mut() {
            state.extend(result);
        }
    }

    fn parent_kind(&self, id: NodeId) -> Option<NodeKind> {
        self.doc.parent(id).ok().flatten().and_then(|p| self.doc.get(p).ok()).map(|n| n.kind)
    }

    fn skip(&mut self, id: NodeId) {
        self.skip_until = Some(id);
    }

    fn is_admonition_family(kind: NodeKind) -> bool {
        matches!(
            kind,
            NodeKind::Admonition
                | NodeKind::Attention
                | NodeKind::Caution
                | NodeKind::Danger
                | NodeKind::Error
                | NodeKind::Hint
                | NodeKind::Important
                | NodeKind::Note
                | NodeKind::Tip
                | NodeKind::Warning
                | NodeKind::Seealso
        )
    }

    fn admonition_label(kind: NodeKind) -> &'static str {
        match kind {
            NodeKind::Attention => "Attention",
            NodeKind::Caution => "Caution",
            NodeKind::Danger => "Danger",
            NodeKind::Error => "Error",
            NodeKind::Hint => "Hint",
            NodeKind::Important => "Important",
            NodeKind::Note => "Note",
            NodeKind::Tip => "Tip",
            NodeKind::Warning => "Warning",
            NodeKind::Seealso => "See also",
            _ => "Note",
        }
    }

    fn section_number(&self, node: &Node) -> String {
        if !self.options.add_secnumbers {
            return String::new();
        }
        let Some(numbers) = self.numbers else { return String::new() };
        let id = node.ids().ok().and_then(|ids| ids.first().cloned()).unwrap_or_default();
        let by_doc = numbers.by_doc.get(self.docname);
        let value = by_doc.and_then(|anchors| {
            anchors
                .get(&format!("#{id}"))
                .or_else(|| anchors.get(""))
        });
        match value {
            Some(parts) if !parts.is_empty() => {
                format!(
                    "{}{}",
                    parts.iter().map(i64::to_string).collect::<Vec<_>>().join("."),
                    self.options.secnumber_suffix
                )
            }
            _ => String::new(),
        }
    }

    /// Emit the toctree as a bullet list of document links. Sphinx's
    /// `resolve_references` replaces toctree nodes with such lists before the
    /// text writer runs; we reproduce that from the toctree collection.
    fn render_toctree(&mut self) {
        let Some(context) = &self.options.toctree else { return };
        let children = context.children.get(&self.options.docname).cloned().unwrap_or_default();
        for child in children {
            let title = context.titles.get(&child).cloned().unwrap_or_else(|| child.clone());
            let number = context.numbers.get(&child).cloned().unwrap_or_default();
            self.new_state(2);
            self.add_text(format!("{number}{title}"));
            self.end_state(true, Some(vec![String::new()]), Some("* ".to_owned()));
        }
    }

    fn table(&self, table_id: NodeId) -> Vec<String> {
        let mut rows: Vec<Vec<String>> = Vec::new();
        let mut header_rows = 0usize;
        // Docutils nests rows under `tgroup → thead/tbody → row` (and may omit
        // the group nodes entirely), so collect rows recursively.
        fn collect_rows(doc: &Document, id: NodeId, rows: &mut Vec<Vec<String>>) {
            let Ok(node) = doc.get(id) else { return };
            if node.kind == NodeKind::Row {
                let mut cells = Vec::new();
                for &entry_id in &node.children {
                    let Ok(entry) = doc.get(entry_id) else { continue };
                    if entry.kind != NodeKind::Entry {
                        continue;
                    }
                    let mut text = String::new();
                    collect_text(doc, entry_id, &mut text);
                    let colspan = entry
                        .attrs
                        .get_int("morecols")
                        .ok()
                        .flatten()
                        .unwrap_or(0)
                        .max(0) as usize
                        + 1;
                    text.push_str(&"  ".repeat(colspan.saturating_sub(1)));
                    cells.push(text);
                }
                rows.push(cells);
                return;
            }
            if matches!(node.kind, NodeKind::Table | NodeKind::Tgroup | NodeKind::Thead | NodeKind::Tbody) {
                for &child in &node.children {
                    collect_rows(doc, child, rows);
                }
            }
        }
        for &child in &self.doc.get(table_id).map(|n| n.children.clone()).unwrap_or_default() {
            if let Ok(group) = self.doc.get(child) {
                if group.kind == NodeKind::Thead {
                    header_rows = self.count_rows(group);
                }
            }
        }
        collect_rows(self.doc, table_id, &mut rows);
        let cols = rows.iter().map(Vec::len).max().unwrap_or(0).max(1);
        let mut widths = vec![0usize; cols];
        for row in &rows {
            for (index, cell) in row.iter().enumerate() {
                if index < cols {
                    widths[index] = widths[index].max(column_width(cell));
                }
            }
        }
        for width in &mut widths {
            *width = (*width).max(1);
        }
        let (h_left, h_mid, h_right, h_bar, v_bar) = if self.options.unicode_tables {
            ('┌', '┬', '┐', '─', '│')
        } else {
            ('+', '+', '+', '-', '|')
        };
        let line = |left: char, mid: char, right: char, fill: char| -> String {
            let mut out = String::new();
            out.push(left);
            for (index, width) in widths.iter().enumerate() {
                if index > 0 {
                    out.push(mid);
                }
                out.extend(std::iter::repeat_n(fill, width + 2));
            }
            out.push(right);
            out
        };
        let mut output = Vec::new();
        for (row_index, row) in rows.iter().enumerate() {
            if row_index == header_rows && header_rows > 0 {
                output.push(if self.options.unicode_tables {
                    line('├', '┼', '┤', '━')
                } else {
                    line('+', '+', '+', '=')
                });
            } else {
                output.push(line(h_left, h_mid, h_right, h_bar));
            }
            let mut text = String::new();
            text.push(v_bar);
            for (index, width) in widths.iter().enumerate() {
                let cell = row.get(index).map(String::as_str).unwrap_or("");
                text.push(' ');
                text.push_str(cell);
                text.push_str(&" ".repeat(width.saturating_sub(column_width(cell)) + 1));
                text.push(v_bar);
            }
            output.push(text);
        }
        output.push(line(h_left, h_mid, h_right, h_bar));
        output
    }

    fn count_rows(&self, group: &Node) -> usize {
        group.children.iter().filter(|&&id| {
            self.doc.get(id).is_ok_and(|node| node.kind == NodeKind::Row)
        }).count()
    }
}

fn collect_text(doc: &Document, id: NodeId, out: &mut String) {
    let Ok(node) = doc.get(id) else { return };
    if node.kind == NodeKind::Text {
        if let Some(text) = node.text_content() {
            out.push_str(text);
        }
        return;
    }
    for &child in &node.children {
        collect_text(doc, child, out);
    }
}

impl Visitor for TextTranslator<'_> {
    fn visit_node(&mut self, node: &Node, id: NodeId) {
        if self.skip_until.is_some() {
            return;
        }
        match node.kind {
            NodeKind::Document => self.new_state(0),
            NodeKind::Section => {
                let char = self.options.sectionchars[self.sectionlevel % self.options.sectionchars.len()];
                self.title_char = char;
                self.sectionlevel += 1;
            }
            NodeKind::Topic | NodeKind::Sidebar => self.new_state(0),
            NodeKind::Rubric => {
                self.new_state(0);
                self.add_text("-[ ");
            }
            NodeKind::Title => {
                if Self::is_admonition_family(self.parent_kind(id).unwrap_or(NodeKind::Document)) {
                    let mut text = String::new();
                    collect_text(self.doc, id, &mut text);
                    self.add_text(format!("{text}: "));
                    self.skip(id);
                } else {
                    self.new_state(0);
                }
            }
            NodeKind::Paragraph => {
                let parent = self.parent_kind(id).unwrap_or(NodeKind::Document);
                if !Self::is_admonition_family(parent) || parent == NodeKind::Seealso {
                    self.new_state(0);
                }
            }
            NodeKind::BulletList => self.list_counter.push(-1),
            NodeKind::EnumeratedList => {
                let start = node.attrs.get_int("start").ok().flatten().unwrap_or(1) - 1;
                self.list_counter.push(start);
            }
            NodeKind::Hlist => self.list_counter.push(-1),
            NodeKind::DefinitionList => self.list_counter.push(-2),
            NodeKind::ListItem | NodeKind::Hlistcol => {
                let counter = self.list_counter.last().copied().unwrap_or(-1);
                match counter {
                    -1 => self.new_state(2),
                    -2 => {}
                    n => {
                        let next = n + 1;
                        if let Some(last) = self.list_counter.last_mut() {
                            *last = next;
                        }
                        self.new_state(next.to_string().len() + 2);
                    }
                }
            }
            NodeKind::Term => self.new_state(0),
            NodeKind::Definition => self.new_state(STDINDENT),
            NodeKind::FieldName => self.new_state(0),
            NodeKind::FieldBody => self.new_state(STDINDENT),
            NodeKind::OptionListItem => self.new_state(0),
            NodeKind::Option => {}
            NodeKind::OptionArgument => {
                self.add_text(node.get_str("delimiter").ok().flatten().unwrap_or("="));
            }
            NodeKind::LineBlock => {
                self.new_state(STDINDENT);
                self.lineblocklevel += 1;
            }
            NodeKind::Admonition => self.new_state(0),
            kind if Self::is_admonition_family(kind) => self.new_state(2),
            NodeKind::VersionModified => self.new_state(0),
            NodeKind::LiteralBlock | NodeKind::DoctestBlock => self.new_state(STDINDENT),
            NodeKind::Figure => self.new_state(STDINDENT),
            NodeKind::Footnote | NodeKind::Citation => {
                let mut label = String::new();
                for &child in &node.children {
                    if self.doc.get(child).is_ok_and(|n| n.kind == NodeKind::Label) {
                        collect_text(self.doc, child, &mut label);
                        break;
                    }
                }
                self.current_label = label.trim().to_owned();
                self.new_state(self.current_label.len() + 3);
            }
            NodeKind::Table => self.new_state(0),
            // `Table::table()` reads cell text straight from the frozen AST,
            // so child visits would only duplicate content into the wrong state.
            NodeKind::Entry => self.skip(id),
            NodeKind::BlockQuote => self.new_state(STDINDENT),
            NodeKind::Attribution => self.add_text("-- "),
            NodeKind::Emphasis | NodeKind::LiteralEmphasis => self.add_text("*"),
            NodeKind::Strong | NodeKind::LiteralStrong => {
                if !self.in_production_list {
                    self.add_text("**");
                }
            }
            NodeKind::Literal => {
                if !self.in_production_list {
                    self.add_text("\"");
                }
            }
            NodeKind::Inline => {
                let classes = node.classes().unwrap_or_default();
                if classes.iter().any(|c| c == "xref" || c == "term") {
                    self.add_text("*");
                }
            }
            NodeKind::Reference => {
                if let Ok(Some(numbers)) = node.get_list("secnumber") {
                    let value = numbers
                        .iter()
                        .filter_map(|value| value.parse::<i64>().ok())
                        .map(|number| number.to_string())
                        .collect::<Vec<_>>()
                        .join(".");
                    if !value.is_empty() {
                        self.add_text(format!("{value}{}", self.options.secnumber_suffix));
                    }
                }
            }
            NodeKind::Problematic => self.add_text(">>"),
            NodeKind::DescSignature => self.new_state(0),
            NodeKind::DescContent => {
                self.new_state(STDINDENT);
                self.add_text(self.options.newline.clone());
            }
            NodeKind::DescParameterlist => self.add_text("("),
            NodeKind::DescTypeParameterList => self.add_text("["),
            NodeKind::DescOptional => self.add_text("["),
            NodeKind::DescReturns => self.add_text(" -> "),
            NodeKind::Productionlist => {
                self.new_state(STDINDENT);
                self.in_production_list = true;
            }
            NodeKind::Raw => {
                let formats = node.get_str("format").ok().flatten().unwrap_or("");
                if formats.split_whitespace().any(|f| f == "text") {
                    let mut text = String::new();
                    collect_text(self.doc, id, &mut text);
                    self.new_state(0);
                    self.add_text(text);
                    self.end_state(false, None, None);
                }
                self.skip(id);
            }
            NodeKind::Image => {
                if let Some(alt) = node.get_str("alt").ok().flatten() {
                    if !alt.is_empty() {
                        self.add_text(format!("[image: {alt}]"));
                    }
                }
                self.add_text("[image]");
                self.skip(id);
            }
            NodeKind::Transition => {
                let indent: usize = self.stateindent.iter().sum();
                self.new_state(0);
                self.add_text("=".repeat(self.options.width.saturating_sub(indent)));
                self.end_state(false, None, None);
                self.skip(id);
            }
            NodeKind::FootnoteReference | NodeKind::CitationReference => {
                let mut text = String::new();
                collect_text(self.doc, id, &mut text);
                self.add_text(format!("[{text}]"));
                self.skip(id);
            }
            NodeKind::NumberReference => {
                self.add_text(node.get_str("title").ok().flatten().unwrap_or("#").to_owned());
                self.skip(id);
            }
            NodeKind::Acks => {
                let mut text = String::new();
                for &child in &node.children {
                    if self.doc.get(child).is_ok_and(|n| n.kind == NodeKind::BulletList) {
                        let mut items = Vec::new();
                        for &li in &self.doc.get(child).map(|n| n.children.clone()).unwrap_or_default() {
                            let mut item = String::new();
                            collect_text(self.doc, li, &mut item);
                            items.push(item);
                        }
                        text.push_str(&items.join(", "));
                        text.push('.');
                    }
                }
                self.new_state(0);
                self.add_text(text);
                self.end_state(true, None, None);
                self.skip(id);
            }
            NodeKind::Toctree => {
                self.render_toctree();
                self.skip(id);
            }
            NodeKind::Target
            | NodeKind::Comment
            | NodeKind::SystemMessage
            | NodeKind::Index
            | NodeKind::SubstitutionDefinition
            | NodeKind::Label
            | NodeKind::TabularColSpec
            | NodeKind::Colspec => self.skip(id),
            NodeKind::Text => {
                if let Some(text) = node.text_content() {
                    self.add_text(text);
                }
            }
            _ => {}
        }
    }

    fn depart_node(&mut self, node: &Node, id: NodeId) {
        if self.skip_until == Some(id) {
            self.skip_until = None;
            return;
        }
        if self.skip_until.is_some() {
            return;
        }
        match node.kind {
            NodeKind::Document => {}
            NodeKind::Section => self.sectionlevel = self.sectionlevel.saturating_sub(1),
            NodeKind::Topic | NodeKind::Sidebar => self.end_state(true, None, None),
            NodeKind::Rubric => {
                self.add_text(" ]-");
                self.end_state(true, None, None);
            }
            NodeKind::Title => {
                let parent = self.parent_kind(id).unwrap_or(NodeKind::Document);
                if Self::is_admonition_family(parent) {
                    return;
                }
                let char = if parent == NodeKind::Section { self.title_char } else { '^' };
                let mut title = String::new();
                if let Some(state) = self.states.pop() {
                    for item in state {
                        if let Item::Flow(text) = item {
                            title.push_str(&text);
                        }
                    }
                }
                self.stateindent.pop();
                let number = self
                    .doc
                    .parent(id)
                    .ok()
                    .flatten()
                    .and_then(|parent| self.doc.get(parent).ok())
                    .map_or_else(String::new, |section| self.section_number(section));
                title = format!("{number}{title}");
                let underline = char.to_string().repeat(column_width(&title));
                let mut lines = vec![String::new(), title, underline, String::new()];
                let is_first = self.states.len() == 2 && self.states.last().is_some_and(Vec::is_empty);
                if is_first && self.first_title_pending {
                    lines.remove(0);
                    self.first_title_pending = false;
                }
                if let Some(state) = self.states.last_mut() {
                    state.push(Item::Block(0, lines));
                }
            }
            NodeKind::Paragraph => {
                let parent = self.parent_kind(id).unwrap_or(NodeKind::Document);
                if !Self::is_admonition_family(parent) || parent == NodeKind::Seealso {
                    self.end_state(true, None, None);
                }
            }
            NodeKind::BulletList | NodeKind::EnumeratedList | NodeKind::Hlist | NodeKind::DefinitionList => {
                self.list_counter.pop();
            }
            NodeKind::ListItem | NodeKind::Hlistcol => {
                let counter = self.list_counter.last().copied().unwrap_or(-1);
                match counter {
                    -1 => self.end_state(true, Some(vec![String::new()]), Some("* ".to_owned())),
                    -2 => {}
                    n => {
                        self.end_state(true, Some(vec![String::new()]), Some(format!("{n}. ")));
                    }
                }
            }
            NodeKind::Term => self.end_state(false, None, None),
            NodeKind::Definition => self.end_state(true, None, None),
            NodeKind::FieldName => self.end_state(false, None, None),
            NodeKind::FieldBody => self.end_state(true, None, None),
            NodeKind::OptionListItem => self.end_state(true, None, None),
            NodeKind::Line => self.add_text("\n"),
            NodeKind::LineBlock => {
                self.lineblocklevel -= 1;
                self.end_state(false, None, None);
                if self.lineblocklevel == 0 {
                    self.add_text("\n");
                }
            }
            NodeKind::Admonition => self.end_state(true, None, None),
            kind if Self::is_admonition_family(kind) => {
                let label = Self::admonition_label(kind);
                let indent: usize = self.stateindent.iter().sum::<usize>() + label.len();
                let single = self.states.last().is_some_and(|state| {
                    state.len() == 1
                        && matches!(&state[0], Item::Block(0, lines) if self.options.width.saturating_sub(indent) >= lines.iter().map(|l| l.chars().count()).sum::<usize>())
                });
                if single {
                    if let Some(last) = self.stateindent.last_mut() {
                        *last += label.len();
                    }
                    self.end_state(true, None, Some(format!("{label}: ")));
                } else {
                    if let Some(state) = self.states.last_mut() {
                        state.insert(0, Item::Block(0, vec![self.options.newline.clone()]));
                    }
                    self.end_state(true, None, Some(format!("{label}:")));
                }
            }
            NodeKind::VersionModified => self.end_state(true, None, None),
            NodeKind::LiteralBlock => self.end_state(false, Some(vec![String::new()]), None),
            NodeKind::DoctestBlock => self.end_state(false, Some(vec![String::new()]), None),
            NodeKind::Figure => self.end_state(true, None, None),
            NodeKind::Footnote | NodeKind::Citation => {
                self.end_state(true, Some(vec![String::new()]), Some(format!("[{}] ", self.current_label)));
            }
            NodeKind::Table => {
                let lines = self.table(id);
                self.end_state(false, None, None);
                // Preserve the box-drawing layout verbatim: adding it as flow
                // text would let the parent re-wrap (and collapse) the lines.
                if let Some(state) = self.states.last_mut() {
                    state.push(Item::Block(0, lines));
                }
            }
            NodeKind::BlockQuote => self.end_state(true, None, None),
            NodeKind::Emphasis | NodeKind::LiteralEmphasis => self.add_text("*"),
            NodeKind::Strong | NodeKind::LiteralStrong => {
                if !self.in_production_list {
                    self.add_text("**");
                }
            }
            NodeKind::Literal => {
                if !self.in_production_list {
                    self.add_text("\"");
                }
            }
            NodeKind::Inline => {
                let classes = node.classes().unwrap_or_default();
                if classes.iter().any(|c| c == "xref" || c == "term") {
                    self.add_text("*");
                }
            }
            NodeKind::Problematic => self.add_text("<<"),
            NodeKind::DescSignature => self.end_state(false, None, None),
            NodeKind::DescContent => self.end_state(true, None, None),
            NodeKind::DescParameterlist => self.add_text(")"),
            NodeKind::DescTypeParameterList => self.add_text("]"),
            NodeKind::DescOptional => self.add_text("]"),
            NodeKind::DescSignatureLine => self.add_text("\n"),
            NodeKind::Productionlist => {
                self.in_production_list = false;
                self.end_state(false, Some(vec![String::new()]), None);
            }
            NodeKind::Raw | NodeKind::Image => {}
            _ => {}
        }
    }
}

/// The plain-text builder.
#[derive(Debug, Default)]
pub struct TextBuilder;

impl TextBuilder {
    /// Create a builder.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Run a complete plain-text build into `options.output_dir`.
    pub fn build(
        options: &BuildOptions,
        config: &Config,
        env: &mut scribe_core::BuildEnvironment,
    ) -> OutputResult<BuildResult> {
        let PreparedBuild { docnames, parsed, toctrees, warnings } =
            prepare(options, config, env, BUILDER_NAME)?;
        let mut text_options = TextOptions::from_config(config);
        let mut toc = ToctreeContext::default();
        for docname in &docnames {
            toc.children.insert(
                docname.clone(),
                toctrees.hierarchy.children(docname).iter().map(|name| (*name).to_owned()).collect(),
            );
            let number = toctrees.section_numbers.display(docname, "");
            if !number.is_empty() {
                toc.numbers.insert(docname.clone(), format!("{number}{}", text_options.secnumber_suffix));
            }
            if let Some(title) = parsed.get(docname).and_then(|doc| doc.metadata.title.clone()) {
                toc.titles.insert(docname.clone(), title);
            }
        }
        text_options.toctree = Some(toc);
        let mut written = Vec::new();
        let mut built_docnames = Vec::new();

        for docname in &docnames {
            let Some(document) = parsed.get(docname) else { continue };
            text_options.docname = docname.clone();
            let body = render_with_numbers(
                document,
                &text_options,
                Some(&toctrees.section_numbers),
                docname,
            );
            let relative = std::path::PathBuf::from(format!("{docname}.txt"));
            let path = options.output_dir.join(&relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| OutputError::io(BUILDER_NAME, parent, e))?;
            }
            std::fs::write(&path, body).map_err(|e| OutputError::io(BUILDER_NAME, &path, e))?;
            written.push(relative);
            built_docnames.push(docname.clone());
        }

        Ok(BuildResult { written, built_docnames, warnings })
    }
}
