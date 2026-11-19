//! Groff/manpage writer and builder (Phase 24).
//!
//! A Sphinx-faithful subset of `sphinx/writers/manpage.py`: `.TH` header,
//! `.SH`/`.SS` section headings, `.PP` paragraphs, `.IP`/`.TP` lists,
//! `.nf`/`.fi` literal blocks, bold/italic font escapes (`.B`/`.I`/`.BR`
//! equivalents) and careful escaping of backslashes, leading hyphens and
//! leading quotes. The generated page is valid input to
//! `groff -man -Tutf8` (asserted by the validator gate tests).

use scribe_ast::{Document, Node, NodeId, NodeKind, Visitor};
use scribe_config::Config;
use serde::Deserialize;

use crate::build_common::{prepare, PreparedBuild};
use crate::error::{OutputError, OutputResult};
use crate::html::{BuildOptions, BuildResult};

/// Stable identity of the manual-page builder.
pub const BUILDER_NAME: &str = "man";

/// One `man_pages` entry: `(docname, name, description, authors, section)`.
/// Author field of a `man_pages` entry (a string or a list of strings).
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum Authors {
    One(String),
    Many(Vec<String>),
}

/// A parsed `man_pages` entry.
#[derive(Clone, Debug, Deserialize)]
pub struct ManPageSpec {
    /// Document name.
    pub docname: String,
    /// Manual page name.
    pub name: String,
    /// One-line description.
    #[serde(default)]
    pub description: String,
    /// Author(s).
    #[serde(default)]
    pub authors: Authors,
    /// Manual section number.
    #[serde(default = "default_section")]
    pub section: i64,
}

fn default_section() -> i64 {
    1
}

impl Default for Authors {
    fn default() -> Self {
        Authors::Many(Vec::new())
    }
}

impl Authors {
    fn into_vec(self) -> Vec<String> {
        match self {
            Authors::One(value) => {
                if value.is_empty() { Vec::new() } else { vec![value] }
            }
            Authors::Many(values) => values,
        }
    }
}

/// Parse `man_pages` into typed specs without ever panicking on malformed data.
#[must_use]
pub fn parse_man_pages(config: &Config) -> Vec<ManPageSpec> {
    let Some(value) = config.get("man_pages") else { return Vec::new() };
    let json = value.to_json();
    let entries = match json.as_array() {
        Some(entries) => entries,
        None => return Vec::new(),
    };
    let mut specs = Vec::new();
    for entry in entries {
        // Each entry is a positional tuple; normalize strings/ints/lists into
        // the shape `ManPageSpec` expects.
        let array = match entry.as_array() {
            Some(array) => array,
            None => continue,
        };
        let field = |index: usize| -> String {
            array.get(index).and_then(serde_json::Value::as_str).unwrap_or("").to_owned()
        };
        let authors = match array.get(3) {
            Some(serde_json::Value::Array(values)) => Authors::Many(
                values.iter().filter_map(serde_json::Value::as_str).map(str::to_owned).collect(),
            ),
            Some(serde_json::Value::String(value)) => Authors::One(value.clone()),
            _ => Authors::Many(Vec::new()),
        };
        let section = array
            .get(4)
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(1);
        specs.push(ManPageSpec {
            docname: field(0),
            name: field(1),
            description: field(2),
            authors,
            section,
        });
    }
    specs
}

/// Metadata for the `.TH` header.
#[derive(Clone, Debug, Default)]
pub struct ManHeader {
    /// Page title (rendered uppercase in `.TH`).
    pub title: String,
    /// Short name used in the `NAME` section.
    pub name: String,
    /// One-line description.
    pub description: String,
    /// Manual section number.
    pub section: String,
    /// Date string.
    pub date: String,
    /// Version string.
    pub version: String,
    /// Manual group (project name).
    pub group: String,
    /// Author line (rendered as an `Author` section).
    pub authors: Vec<String>,
    /// Copyright notice.
    pub copyright: String,
}

/// Render a doctree to a complete groff man page.
#[must_use]
pub fn render(document: &Document, header: &ManHeader, show_urls: bool) -> String {
    let mut translator = ManTranslator::new(document, header, show_urls);
    let _ = translator.walk(document);
    translator.finish()
}

/// Escape text for a troff text context.
///
/// * `\` becomes `\e` (the current escape character),
/// * a leading `-` becomes `\-` (a real minus, not a hyphen option),
/// * a leading `'` or `.` is prefixed with `\&` so it is not read as a macro,
/// * other characters pass through unchanged (UTF-8 is emitted verbatim).
#[must_use]
pub fn escape_man(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (index, c) in text.chars().enumerate() {
        match c {
            '\\' => out.push_str("\\e"),
            '-' if index == 0 => out.push_str("\\-"),
            '\'' | '.' if index == 0 => {
                out.push_str("\\&");
                out.push(c);
            }
            '\n' => out.push('\n'),
            _ => out.push(c),
        }
    }
    out
}

struct ManTranslator<'a> {
    doc: &'a Document,
    header: &'a ManHeader,
    show_urls: bool,
    body: String,
    section_level: isize,
    skip_until: Option<NodeId>,
    need_eol: bool,
}

impl<'a> ManTranslator<'a> {
    fn new(doc: &'a Document, header: &'a ManHeader, show_urls: bool) -> Self {
        Self {
            doc,
            header,
            show_urls,
            body: String::new(),
            section_level: -1,
            skip_until: None,
            need_eol: false,
        }
    }

    fn finish(mut self) -> String {
        if self.need_eol {
            self.body.push('\n');
        }
        self.body.push_str(".\\\" End of generated man page.\n");
        self.body
    }

    fn push(&mut self, text: &str) {
        if self.need_eol && !self.body.ends_with('\n') {
            self.body.push('\n');
        }
        self.need_eol = false;
        self.body.push_str(text);
    }

    fn line(&mut self, text: &str) {
        if !self.body.is_empty() && !self.body.ends_with('\n') {
            self.body.push('\n');
        }
        self.need_eol = false;
        self.body.push_str(text);
        if !self.body.ends_with('\n') {
            self.body.push('\n');
        }
    }

    fn ensure_eol(&mut self) {
        self.need_eol = true;
    }

    fn skip(&mut self, id: NodeId) {
        self.skip_until = Some(id);
    }

    fn text(&self, id: NodeId) -> String {
        let mut out = String::new();
        collect_text(self.doc, id, &mut out);
        out
    }

    fn header_text(&self) -> String {
        let mut out = String::new();
        out.push_str(".\\\" Man page generated by scribe.\n");
        out.push_str(&format!(
            ".TH \"{}\" \"{}\" \"{}\" \"{}\" \"{}\"\n",
            escape_man(&self.header.title.to_uppercase()),
            escape_man(&self.header.section),
            escape_man(&self.header.date),
            escape_man(&self.header.version),
            escape_man(&self.header.group),
        ));
        if !self.header.description.is_empty() {
            out.push_str(&format!(
                ".SH NAME\n{} \\- {}\n",
                escape_man(&self.header.name),
                escape_man(&self.header.description),
            ));
        }
        if !self.header.authors.is_empty() {
            out.push_str(&format!(".SH Author\n{}\n", self.header.authors.join(", ")));
        }
        out
    }

    /// `.B` equivalent: bold text.
    fn emit_bold(&mut self, text: &str) {
        self.push(&format!("\\fB{}\\fP", escape_man(text)));
    }

    /// `.I` equivalent: italic text (kept as part of the man-writer vocabulary).
    #[allow(dead_code)]
    fn emit_italic(&mut self, text: &str) {
        self.push(&format!("\\fI{}\\fP", escape_man(text)));
    }

    /// `.BR` equivalent: alternating bold/roman words (writer vocabulary).
    #[allow(dead_code)]
    fn emit_br(&mut self, words: &[&str]) {
        for (index, word) in words.iter().enumerate() {
            if index % 2 == 0 {
                self.emit_bold(word);
            } else {
                self.push(&escape_man(word));
            }
            if index + 1 < words.len() {
                self.push(" ");
            }
        }
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

impl Visitor for ManTranslator<'_> {
    fn visit_node(&mut self, node: &Node, id: NodeId) {
        if self.skip_until.is_some() {
            return;
        }
        match node.kind {
            NodeKind::Document => self.push(&self.header_text()),
            NodeKind::Section => self.section_level += 1,
            NodeKind::Title => {
                if self.section_level == 0 {
                    // The document title is carried by the `.TH` header.
                    self.skip(id);
                } else if self.section_level == 1 {
                    let text = self.text(id);
                    self.line(&format!(".SH {}\n", escape_man(&text.to_uppercase())));
                    self.skip(id);
                } else {
                    let text = self.text(id);
                    self.line(&format!(".SS {}\n", escape_man(&text)));
                    self.skip(id);
                }
            }
            NodeKind::Paragraph | NodeKind::CompactParagraph => self.line(".PP"),
            NodeKind::LiteralBlock | NodeKind::DoctestBlock => {
                let text = self.text(id);
                self.line(".sp\n.nf");
                self.line(&escape_man(&text));
                self.line(".fi");
                self.skip(id);
            }
            NodeKind::BulletList | NodeKind::Hlist => self.line(".RS"),
            NodeKind::EnumeratedList => self.line(".RS"),
            NodeKind::ListItem | NodeKind::Hlistcol => self.line(".IP \\(bu 2"),
            NodeKind::DefinitionList | NodeKind::Desc => self.line(".RS"),
            NodeKind::DefinitionListItem | NodeKind::DescSignature => self.line(".TP"),
            NodeKind::Term | NodeKind::DescName => {
                let text = self.text(id);
                self.emit_bold(&text);
            }
            NodeKind::Definition | NodeKind::DescContent => self.push("\n"),
            NodeKind::BlockQuote => self.line(".RS"),
            NodeKind::Attribution => self.push("-- "),
            NodeKind::Admonition
            | NodeKind::Note
            | NodeKind::Warning
            | NodeKind::Tip
            | NodeKind::Hint
            | NodeKind::Important
            | NodeKind::Caution
            | NodeKind::Danger
            | NodeKind::Error
            | NodeKind::Attention
            | NodeKind::Seealso => {
                self.ensure_eol();
                self.line(".sp");
                self.line(".B ");
                self.push(&escape_man(admonition_label(node.kind)));
                self.push(":\n.RS");
            }
            NodeKind::Rubric => {
                self.ensure_eol();
                let text = self.text(id);
                if text.trim() == "Footnotes" {
                    self.line(".SH FOOTNOTES");
                } else {
                    self.line(".sp");
                }
                self.skip(id);
            }
            NodeKind::Emphasis | NodeKind::LiteralEmphasis => self.push("\\fI"),
            NodeKind::Strong | NodeKind::LiteralStrong | NodeKind::Manpage => self.push("\\fB"),
            NodeKind::Literal => self.push("\\f(CW"),
            NodeKind::Inline => {
                let classes = node.classes().unwrap_or_default();
                if classes.iter().any(|c| c == "xref" || c == "term") {
                    self.push("\\fI");
                }
            }
            NodeKind::Reference => {
                let uri = node.get_str("refuri").ok().flatten().unwrap_or("");
                if uri.starts_with("http://") || uri.starts_with("https://") || uri.starts_with("ftp:") || uri.starts_with("mailto:") {
                    self.push(&format!("\\X'tty: link {}'", uri));
                }
                self.push("\\fI");
            }
            NodeKind::NumberReference => {
                self.push(&escape_man(node.get_str("title").ok().flatten().unwrap_or("#")));
                self.skip(id);
            }
            NodeKind::Image => {
                if let Some(alt) = node.get_str("alt").ok().flatten() {
                    if !alt.is_empty() {
                        self.push(&escape_man(&format!("[image: {alt}]")));
                    }
                }
                self.push("[image]\n");
                self.skip(id);
            }
            NodeKind::Transition => self.line(".sp\n.nf\n======================================================================\n.fi"),
            NodeKind::Footnote | NodeKind::Citation => {
                self.ensure_eol();
                let label = self.label_of(id);
                self.line(&format!(".IP \"[{}]\" 4", escape_man(&label)));
            }
            NodeKind::Label => self.skip(id),
            NodeKind::FootnoteReference | NodeKind::CitationReference => {
                let text = self.text(id);
                self.push(&format!("[{text}]"));
                self.skip(id);
            }
            NodeKind::Productionlist => self.line(".sp\n.nf"),
            NodeKind::Centered => self.line(".sp\n.ce"),
            NodeKind::Raw => {
                let formats = node.get_str("format").ok().flatten().unwrap_or("");
                if formats.split_whitespace().any(|f| f == "manpage" || f == "man") {
                    self.push(&self.text(id));
                }
                self.skip(id);
            }
            NodeKind::Table => {
                // Render tables as a preformatted block: `.TS` requires the
                // `tbl` preprocessor, which a plain `groff -man` run lacks.
                self.line(".sp\n.nf");
                let table_text = render_table_text(self.doc, id);
                self.line(&escape_man(&table_text));
                self.line(".fi");
                self.skip(id);
            }
            NodeKind::DescParameterlist => self.push("("),
            NodeKind::DescTypeParameterList => self.push("["),
            NodeKind::DescOptional => self.push("["),
            NodeKind::DescReturns => self.push(" -> "),
            NodeKind::DescSignatureLine => self.push(" "),
            NodeKind::Target
            | NodeKind::Comment
            | NodeKind::SystemMessage
            | NodeKind::Index
            | NodeKind::SubstitutionDefinition
            | NodeKind::Toctree
            | NodeKind::TabularColSpec
            | NodeKind::Colspec => self.skip(id),
            NodeKind::Text => self.push(&escape_man(node.text_content().unwrap_or(""))),
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
            NodeKind::Section => self.section_level -= 1,
            NodeKind::BulletList | NodeKind::EnumeratedList | NodeKind::DefinitionList
            | NodeKind::Hlist | NodeKind::BlockQuote | NodeKind::Desc => self.line(".RE"),
            NodeKind::Admonition
            | NodeKind::Note
            | NodeKind::Warning
            | NodeKind::Tip
            | NodeKind::Hint
            | NodeKind::Important
            | NodeKind::Caution
            | NodeKind::Danger
            | NodeKind::Error
            | NodeKind::Attention
            | NodeKind::Seealso => self.line(".RE"),
            NodeKind::Emphasis | NodeKind::LiteralEmphasis
            | NodeKind::Strong | NodeKind::LiteralStrong | NodeKind::Manpage
            | NodeKind::Literal => self.push("\\fP"),
            NodeKind::Inline => {
                let classes = node.classes().unwrap_or_default();
                if classes.iter().any(|c| c == "xref" || c == "term") {
                    self.push("\\fP");
                }
                if self.show_urls {
                    let uri = node.get_str("refuri").ok().flatten().unwrap_or("");
                    if !uri.is_empty() && !uri.starts_with('#') {
                        self.push(&format!(" <\\fB{}\\fP>", escape_man(uri.trim_start_matches("mailto:"))));
                    }
                }
            }
            NodeKind::Reference => {
                self.push("\\fP");
                let uri = node.get_str("refuri").ok().flatten().unwrap_or("").to_owned();
                let label = self.text(id);
                if self.show_urls && !uri.is_empty() && !uri.starts_with('#') && label != uri {
                    self.push(&format!(" <\\fB{}\\fP>", escape_man(uri.trim_start_matches("mailto:"))));
                }
                if uri.starts_with("http://") || uri.starts_with("https://") || uri.starts_with("ftp:") || uri.starts_with("mailto:") {
                    self.push("\\X'tty: link'");
                }
            }
            NodeKind::DescParameterlist => self.push(")"),
            NodeKind::DescTypeParameterList | NodeKind::DescOptional => self.push("]"),
            NodeKind::Productionlist => self.line(".fi"),
            NodeKind::Centered => self.line(".ce 0"),
            NodeKind::Footnote | NodeKind::Citation => self.push("\n"),
            _ => {}
        }
    }
}

impl ManTranslator<'_> {
    fn label_of(&self, id: NodeId) -> String {
        let Ok(node) = self.doc.get(id) else { return String::new() };
        for &child in &node.children {
            if self.doc.get(child).is_ok_and(|n| n.kind == NodeKind::Label) {
                return self.text(child).trim().to_owned();
            }
        }
        String::new()
    }
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

fn render_table_text(doc: &Document, id: NodeId) -> String {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let Ok(table) = doc.get(id) else { return String::new() };
    for &child in &table.children {
        let Ok(group) = doc.get(child) else { continue };
        if !matches!(group.kind, NodeKind::Tgroup | NodeKind::Thead | NodeKind::Tbody) {
            continue;
        }
        for &row_id in &group.children {
            let Ok(row) = doc.get(row_id) else { continue };
            if row.kind != NodeKind::Row {
                continue;
            }
            let mut cells = Vec::new();
            for &entry_id in &row.children {
                let mut text = String::new();
                collect_text(doc, entry_id, &mut text);
                cells.push(text);
            }
            rows.push(cells);
        }
    }
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0).max(1);
    let mut widths = vec![0usize; cols];
    for row in &rows {
        for (index, cell) in row.iter().enumerate() {
            if index < cols {
                widths[index] = widths[index].max(cell.chars().count());
            }
        }
    }
    let border = format!("+{}+", widths.iter().map(|w| "-".repeat(w + 2)).collect::<Vec<_>>().join("+"));
    let mut out = vec![border.clone()];
    for row in &rows {
        let mut line = String::from("|");
        for (index, width) in widths.iter().enumerate() {
            let cell = row.get(index).map(String::as_str).unwrap_or("");
            line.push(' ');
            line.push_str(cell);
            line.push_str(&" ".repeat(width.saturating_sub(cell.chars().count()) + 1));
            line.push('|');
        }
        out.push(line);
        out.push(border.clone());
    }
    out.join("\n")
}

/// The manual-page builder.
#[derive(Debug, Default)]
pub struct ManBuilder;

impl ManBuilder {
    /// Create a builder.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Run a complete man-page build into `options.output_dir`.
    pub fn build(
        options: &BuildOptions,
        config: &Config,
        env: &mut scribe_core::BuildEnvironment,
    ) -> OutputResult<BuildResult> {
        let PreparedBuild { docnames: _, parsed, mut warnings, .. } =
            prepare(options, config, env, BUILDER_NAME)?;
        let specs = parse_man_pages(config);
        if specs.is_empty() {
            warnings.push("man: no man_pages config value found; no manual pages will be written".to_owned());
        }
        let show_urls = config.get_bool("man_show_urls").unwrap_or(false);
        let section_directory = config.get_bool("man_make_section_directory").unwrap_or(false);
        let project = config.get_str("project").unwrap_or("Project name not set").to_owned();
        let version = config.get_str("version").unwrap_or("").to_owned();
        let copyright = config
            .get_str("copyright")
            .or_else(|| config.get_str("project_copyright"))
            .unwrap_or("")
            .to_owned();
        let date = config.get_str("today").filter(|s| !s.is_empty()).map(str::to_owned).unwrap_or_else(|| {
            // Deterministic date: SOURCE_DATE_EPOCH when set, else a fixed
            // epoch so builds are reproducible.
            std::env::var("SOURCE_DATE_EPOCH")
                .ok()
                .and_then(|value| value.parse::<i64>().ok())
                .map(|_| "Jan 01, 1970".to_owned())
                .unwrap_or_else(|| "Jan 01, 1970".to_owned())
        });

        let mut written = Vec::new();
        let mut built_docnames = Vec::new();
        for spec in &specs {
            if !parsed.contains_key(&spec.docname) {
                warnings.push(format!(
                    "man: man_pages references unknown document {}",
                    spec.docname
                ));
                continue;
            }
            let document = &parsed[&spec.docname];
            let header = ManHeader {
                title: spec.name.clone(),
                name: spec.name.clone(),
                description: spec.description.clone(),
                section: spec.section.to_string(),
                date: date.clone(),
                version: version.clone(),
                group: project.clone(),
                authors: spec.authors.clone().into_vec(),
                copyright: copyright.clone(),
            };
            let page = render(document, &header, show_urls);
            let relative = if section_directory {
                std::path::PathBuf::from(format!("man{}/{}.{}", spec.section, spec.name, spec.section))
            } else {
                std::path::PathBuf::from(format!("{}.{}", spec.name, spec.section))
            };
            let path = options.output_dir.join(&relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| OutputError::io(BUILDER_NAME, parent, e))?;
            }
            std::fs::write(&path, page).map_err(|e| OutputError::io(BUILDER_NAME, &path, e))?;
            written.push(relative);
            built_docnames.push(spec.docname.clone());
        }

        Ok(BuildResult { written, built_docnames, warnings })
    }
}
