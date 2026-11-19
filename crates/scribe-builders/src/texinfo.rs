//! GNU Texinfo writer and builder (Phase 24).
//!
//! Emits a self-contained `.texi` document following `sphinx/writers/texinfo.py`:
//! a `@setfilename` / `@titlepage` header, `@node` + `@chapter` / `@section` /
//! `@subsection` hierarchy with `@menu` navigation, `@example`, `@itemize`,
//! `@table`, `@deftypefn` / `@defun`, cross-references (`@ref`, `@xref`,
//! `@uref`), `@emph` / `@strong`, `@cindex` and a final `@bye`.
//!
//! The generated file is checked with `makeinfo --no-split -o /dev/null`
//! by the validator gate tests.

use std::collections::{BTreeMap, BTreeSet};

use scribe_ast::{Document, Node, NodeId, NodeKind, Visitor};
use scribe_config::Config;
use serde::Deserialize;

use crate::build_common::{prepare, PreparedBuild};
use crate::error::{OutputError, OutputResult};
use crate::html::{BuildOptions, BuildResult};

/// Stable identity of the Texinfo builder.
pub const BUILDER_NAME: &str = "texinfo";

/// One `texinfo_documents` entry.
/// A parsed `texinfo_documents` entry.
#[derive(Clone, Debug, Deserialize)]
pub struct TexinfoSpec {
    /// Document name.
    pub docname: String,
    /// Output file base name.
    pub targetname: String,
    /// Document title.
    pub title: String,
    /// Author.
    #[serde(default)]
    pub author: String,
    /// Info directory entry.
    #[serde(default)]
    pub direntry: String,
    /// One-line description.
    #[serde(default)]
    pub description: String,
    /// Info directory category.
    #[serde(default = "default_category")]
    pub category: String,
    /// Whether to include only toctree documents.
    #[serde(default)]
    pub toctree_only: bool,
}

fn default_category() -> String {
    "Miscellaneous".to_owned()
}

/// Parse `texinfo_documents` from configuration defensively.
#[must_use]
pub fn parse_texinfo_documents(config: &Config) -> Vec<TexinfoSpec> {
    let Some(value) = config.get("texinfo_documents") else { return Vec::new() };
    let json = value.to_json();
    let Some(entries) = json.as_array() else { return Vec::new() };
    let string_field = |array: &[serde_json::Value], index: usize| -> String {
        array.get(index).and_then(serde_json::Value::as_str).unwrap_or("").to_owned()
    };
    let mut specs = Vec::new();
    for entry in entries {
        let Some(array) = entry.as_array() else { continue };
        specs.push(TexinfoSpec {
            docname: string_field(array, 0),
            targetname: string_field(array, 1),
            title: string_field(array, 2),
            author: string_field(array, 3),
            direntry: string_field(array, 4),
            description: string_field(array, 5),
            category: {
                let category = string_field(array, 6);
                if category.is_empty() { default_category() } else { category }
            },
            toctree_only: array.get(7).and_then(serde_json::Value::as_bool).unwrap_or(false),
        });
    }
    specs
}

/// Metadata for the Texinfo header / title page.
#[derive(Clone, Debug, Default)]
pub struct TexinfoHeader {
    /// Output file base name (without `.texi` / `.info`).
    pub filename: String,
    /// Document title.
    pub title: String,
    /// Author.
    pub author: String,
    /// Project / release line.
    pub project_line: String,
    /// Copyright notice.
    pub copyright: String,
    /// Info directory category.
    pub category: String,
    /// Info directory entry.
    pub direntry: String,
    /// One-line description.
    pub description: String,
    /// Date string.
    pub date: String,
    /// Generated-by version string.
    pub generator: String,
}

/// Escape text for an ordinary Texinfo context.
#[must_use]
pub fn escape_texinfo(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '@' => out.push_str("@@"),
            '{' => out.push_str("@{"),
            '}' => out.push_str("@}"),
            _ => out.push(c),
        }
    }
    out
}

/// Escape a string used as an argument of `@node` / menu entries, where commas
/// are structural separators.
fn escape_node_name(text: &str) -> String {
    escape_texinfo(&text.replace([',', '\n'], " "))
}

/// Escape a macro argument, where commas separate positional arguments.
fn escape_arg(text: &str) -> String {
    escape_texinfo(text).replace(',', "@,")
}

/// Information about one section, precomputed for node/menu generation.
#[derive(Clone, Debug)]
struct NodeInfo {
    /// Unique Texinfo node name.
    name: String,
    /// Section nesting level (1 = chapter).
    level: usize,
    /// Next node name (empty for the last section).
    next: String,
    /// Previous node name (empty for the first section).
    prev: String,
    /// Parent node name (or `Top`).
    up: String,
    /// Child section node ids in order.
    children: Vec<NodeId>,
}

/// Precompute node names, navigation pointers and children for every section.
fn plan_sections(documents: &[&Document]) -> BTreeMap<(usize, NodeId), NodeInfo> {
    #[derive(Clone)]
    struct Entry {
        doc_index: usize,
        id: NodeId,
        title: String,
        level: usize,
    }
    let mut entries: Vec<Entry> = Vec::new();
    for (doc_index, document) in documents.iter().enumerate() {
        for id in document.preorder_ids() {
            let Ok(node) = document.get(id) else { continue };
            if node.kind == NodeKind::Section {
                let title = node
                    .children
                    .iter()
                    .find(|&&child| document.get(child).is_ok_and(|n| n.kind == NodeKind::Title))
                    .map(|&child| {
                        let mut text = String::new();
                        collect_text(document, child, &mut text);
                        text
                    })
                    .unwrap_or_else(|| format!("section-{}", entries.len() + 1));
                let level = section_level(document, id, 1);
                entries.push(Entry { doc_index, id, title, level });
            }
        }
    }

    let mut used = BTreeSet::new();
    let mut names: Vec<String> = Vec::new();
    for entry in &entries {
        let base = escape_node_name(&entry.title);
        let base = if base.is_empty() { "section".to_owned() } else { base };
        let mut name = base.clone();
        let mut counter = 2;
        while used.contains(&name) {
            name = format!("{base}-{counter}");
            counter += 1;
        }
        used.insert(name.clone());
        names.push(name);
    }

    // Parent of each entry: the nearest preceding section with a lower level.
    let parents: Vec<Option<usize>> = (0..entries.len())
        .map(|index| {
            (0..index).rev().find(|&before| entries[before].level < entries[index].level)
        })
        .collect();

    let mut infos: BTreeMap<(usize, NodeId), NodeInfo> = BTreeMap::new();
    for (index, entry) in entries.iter().enumerate() {
        let parent = parents[index];
        // Texinfo node `next` / `prev` pointers follow the parent's menu, i.e.
        // they connect siblings rather than the linear pre-order. Using the
        // linear order makes `makeinfo` emit "next pointer ... but ... in menu"
        // warnings.
        let prev = (0..index)
            .rev()
            .find(|&other| parents[other] == parent && entries[other].level == entry.level)
            .map_or_else(String::new, |other| names[other].clone());
        let next = (index + 1..entries.len())
            .find(|&other| parents[other] == parent && entries[other].level == entry.level)
            .map_or_else(String::new, |other| names[other].clone());
        let children: Vec<NodeId> = (index + 1..entries.len())
            .filter(|&other| parents[other] == Some(index))
            .map(|other| entries[other].id)
            .collect();
        infos.insert(
            (entry.doc_index, entry.id),
            NodeInfo {
                name: names[index].clone(),
                level: entry.level,
                next,
                prev,
                up: parent.map_or_else(|| "Top".to_owned(), |p| names[p].clone()),
                children,
            },
        );
    }
    infos
}

fn section_level(document: &Document, id: NodeId, current: usize) -> usize {
    if document.get(id).is_err() {
        return current;
    }
    match document.parent(id).ok().flatten() {
        Some(parent) if document.get(parent).is_ok_and(|n| n.kind == NodeKind::Section) => {
            section_level(document, parent, current + 1)
        }
        _ => current,
    }
}

/// Render one or more doctrees to a complete `.texi` document.
#[must_use]
pub fn render(documents: &[&Document], header: &TexinfoHeader) -> String {
    let infos = plan_sections(documents);
    let mut body = String::new();

    // Top-level menu lists every chapter (level 1) in order.
    let mut chapters: Vec<&NodeInfo> = Vec::new();
    for (doc_index, document) in documents.iter().enumerate() {
        for id in document.preorder_ids() {
            if let Some(info) = infos.get(&(doc_index, id)) {
                if info.level == 1 {
                    chapters.push(info);
                }
            }
        }
    }
    if !chapters.is_empty() {
        body.push_str("\n@menu\n");
        for chapter in &chapters {
            body.push_str(&format!("* {}::\n", chapter.name));
        }
        body.push_str("@end menu\n");
    }

    for (doc_index, document) in documents.iter().enumerate() {
        let mut translator = TexinfoTranslator::new(document, &infos, doc_index);
        let _ = translator.walk(document);
        body.push_str(&translator.finish());
    }

    let mut out = String::new();
    out.push_str("\\input texinfo   @c -*-texinfo-*-\n");
    out.push_str("@c %**start of header\n");
    out.push_str(&format!("@setfilename {}.info\n", header.filename));
    out.push_str("@documentencoding UTF-8\n");
    out.push_str("@ifinfo\n");
    out.push_str(&format!("@*Generated by {}.@*\n", escape_texinfo(&header.generator)));
    out.push_str("@end ifinfo\n");
    out.push_str(&format!("@settitle {}\n", escape_texinfo(&header.title)));
    out.push_str("@defindex ge\n");
    out.push_str("@paragraphindent 0\n");
    out.push_str("@exampleindent 4\n");
    out.push_str("@finalout\n");
    out.push_str(&format!("@dircategory {}\n", escape_texinfo(&header.category)));
    if !header.direntry.is_empty() || !header.description.is_empty() {
        out.push_str("@direntry\n");
        out.push_str(&format!(
            "* {}: ({}.info). {}\n",
            escape_node_name(if header.direntry.is_empty() { &header.filename } else { &header.direntry }),
            header.filename,
            escape_texinfo(&header.description)
        ));
        out.push_str("@end direntry\n");
    }
    out.push_str("@c %**end of header\n\n");
    out.push_str("@copying\n");
    out.push_str("@quotation\n");
    out.push_str(&format!("{}, {}\n\n", escape_texinfo(&header.project_line), escape_texinfo(&header.date)));
    out.push_str(&format!("{}\n\n", escape_texinfo(&header.author)));
    out.push_str(&format!("Copyright @copyright{{}} {}\n", escape_texinfo(&header.copyright)));
    out.push_str("@end quotation\n\n@end copying\n\n");
    out.push_str("@titlepage\n");
    out.push_str(&format!("@title {}\n", escape_texinfo(&header.title)));
    out.push_str("@insertcopying\n@end titlepage\n@contents\n\n");
    out.push_str("@ifnottex\n@node Top\n");
    out.push_str(&format!("@top {}\n", escape_texinfo(&header.title)));
    out.push_str("@insertcopying\n@end ifnottex\n\n");
    out.push_str("@c %**start of body\n");
    out.push_str(&body);
    out.push_str("@c %**end of body\n");
    out.push_str("@bye\n");
    out
}

struct TexinfoTranslator<'a> {
    doc: &'a Document,
    infos: &'a BTreeMap<(usize, NodeId), NodeInfo>,
    doc_index: usize,
    body: String,
    skip_until: Option<NodeId>,
}

impl<'a> TexinfoTranslator<'a> {
    fn new(
        doc: &'a Document,
        infos: &'a BTreeMap<(usize, NodeId), NodeInfo>,
        doc_index: usize,
    ) -> Self {
        Self { doc, infos, doc_index, body: String::new(), skip_until: None }
    }

    fn finish(self) -> String {
        self.body
    }

    fn push(&mut self, text: &str) {
        self.body.push_str(text);
    }

    fn line(&mut self, text: &str) {
        self.body.push_str(text);
        self.body.push('\n');
    }

    fn skip(&mut self, id: NodeId) {
        self.skip_until = Some(id);
    }

    fn text(&self, id: NodeId) -> String {
        let mut out = String::new();
        collect_text(self.doc, id, &mut out);
        out
    }

    /// Text of the `Title` child only (not the whole section subtree).
    fn title_of(&self, id: NodeId) -> String {
        let Ok(node) = self.doc.get(id) else { return String::new() };
        node.children
            .iter()
            .find(|&&child| self.doc.get(child).is_ok_and(|n| n.kind == NodeKind::Title))
            .map(|&child| self.text(child))
            .unwrap_or_default()
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

impl Visitor for TexinfoTranslator<'_> {
    fn visit_node(&mut self, node: &Node, id: NodeId) {
        if self.skip_until.is_some() {
            return;
        }
        match node.kind {
            NodeKind::Section => {
                if let Some(info) = self.infos.get(&(self.doc_index, id)) {
                    self.line(&format!(
                        "\n@node {},{},{},{}\n",
                        info.name, info.next, info.prev, info.up
                    ));
                    let command = match info.level {
                        1 => "@chapter",
                        2 => "@section",
                        3 => "@subsection",
                        _ => "@subsubsection",
                    };
                    let title = self.title_of(id);
                    self.line(&format!("{} {}", command, escape_texinfo(&title)));
                    if !info.children.is_empty() {
                        self.line("@menu");
                        for &child in &info.children {
                            if let Some(child_info) = self.infos.get(&(self.doc_index, child)) {
                                self.line(&format!("* {}::", child_info.name));
                            }
                        }
                        self.line("@end menu");
                    }
                }
            }
            NodeKind::Title => {
                // Section titles are emitted by the Section handler itself.
                self.skip(id);
            }
            NodeKind::Paragraph | NodeKind::CompactParagraph => self.push("\n"),
            NodeKind::LiteralBlock | NodeKind::DoctestBlock => {
                let text = self.text(id);
                self.line("@example");
                self.push(&escape_texinfo(&text));
                self.line("@end example");
                self.skip(id);
            }
            NodeKind::BulletList | NodeKind::Hlist => self.line("@itemize @bullet"),
            NodeKind::EnumeratedList => self.line("@itemize @enumerate"),
            NodeKind::ListItem | NodeKind::Hlistcol => self.line("@item"),
            NodeKind::DefinitionList => self.line("@table @code"),
            NodeKind::FieldList => self.line("@table @asis"),
            NodeKind::DefinitionListItem => {
                let term = node
                    .children
                    .iter()
                    .find(|&&child| self.doc.get(child).is_ok_and(|n| n.kind == NodeKind::Term))
                    .map(|&child| self.text(child))
                    .unwrap_or_default();
                self.line(&format!("@item {}", escape_texinfo(&term)));
            }
            NodeKind::Field => {
                let name = node
                    .children
                    .iter()
                    .find(|&&child| self.doc.get(child).is_ok_and(|n| n.kind == NodeKind::FieldName))
                    .map(|&child| self.text(child))
                    .unwrap_or_default();
                self.line(&format!("@item {}", escape_arg(&name)));
            }
            NodeKind::FieldName => self.skip(id),
            NodeKind::DescSignature => {
                let signature = self.text(id);
                self.line(&format!("@cindex {}", escape_texinfo(&signature)));
                self.line(&format!("@deftypefn {{Function}} {{}} {}", escape_arg(&signature)));
                self.skip(id);
            }
            NodeKind::DescContent => self.push("\n"),
            NodeKind::Emphasis | NodeKind::LiteralEmphasis => self.push("@emph{"),
            NodeKind::Strong | NodeKind::LiteralStrong => self.push("@strong{"),
            NodeKind::Literal => self.push("@code{"),
            NodeKind::Inline => {
                let classes = node.classes().unwrap_or_default();
                if classes.iter().any(|c| c == "xref" || c == "term") {
                    self.push("@emph{");
                }
            }
            NodeKind::Reference | NodeKind::PendingXref => {
                let uri = node.get_str("refuri").ok().flatten().unwrap_or("").to_owned();
                let target = node
                    .get_str("refid")
                    .ok()
                    .flatten()
                    .or_else(|| node.get_str("reftarget").ok().flatten())
                    .unwrap_or("")
                    .to_owned();
                let label = self.text(id);
                if !uri.is_empty() {
                    self.push(&format!("@uref{{{},{}}}", escape_arg(&uri), escape_arg(&label)));
                } else if !target.is_empty() {
                    // Unresolved cross-references use `@xref` ("see"), while
                    // plain internal links use `@ref`; both are Texinfo's
                    // cross-reference primitives.
                    let command = if node.kind == NodeKind::PendingXref { "xref" } else { "ref" };
                    self.push(&format!(
                        "@{}{{{},{}}}",
                        command,
                        escape_node_name(&target),
                        escape_arg(&label)
                    ));
                } else {
                    self.push(&escape_texinfo(&label));
                }
                self.skip(id);
            }
            NodeKind::NumberReference => {
                self.push(&escape_texinfo(node.get_str("title").ok().flatten().unwrap_or("#")));
                self.skip(id);
            }
            NodeKind::Image => {
                if let Some(alt) = node.get_str("alt").ok().flatten() {
                    if !alt.is_empty() {
                        self.push(&escape_texinfo(&format!("[image: {alt}]")));
                    }
                }
                self.skip(id);
            }
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
                self.line("@quotation");
                self.line(&format!("{}:", admonition_label(node.kind)));
            }
            NodeKind::Rubric => {
                self.line(&format!("@heading {}", escape_texinfo(&self.text(id))));
                self.skip(id);
            }
            NodeKind::BlockQuote => self.line("@quotation"),
            NodeKind::Target
            | NodeKind::Comment
            | NodeKind::SystemMessage
            | NodeKind::Index
            | NodeKind::SubstitutionDefinition
            | NodeKind::Toctree
            | NodeKind::TabularColSpec
            | NodeKind::Colspec
            | NodeKind::Label => self.skip(id),
            NodeKind::Text => self.push(&escape_texinfo(node.text_content().unwrap_or(""))),
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
            NodeKind::BulletList | NodeKind::Hlist | NodeKind::EnumeratedList => self.line("@end itemize"),
            NodeKind::DefinitionList | NodeKind::FieldList => self.line("@end table"),
            NodeKind::Desc => self.line("@end deftypefn"),
            NodeKind::Emphasis | NodeKind::LiteralEmphasis | NodeKind::Strong
            | NodeKind::LiteralStrong | NodeKind::Literal => self.push("}"),
            NodeKind::Inline => {
                let classes = node.classes().unwrap_or_default();
                if classes.iter().any(|c| c == "xref" || c == "term") {
                    self.push("}");
                }
            }
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
            | NodeKind::Seealso => self.line("@end quotation"),
            NodeKind::BlockQuote => self.line("@end quotation"),
            NodeKind::Paragraph | NodeKind::CompactParagraph => self.push("\n"),
            _ => {}
        }
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

/// The Texinfo builder.
#[derive(Debug, Default)]
pub struct TexinfoBuilder;

impl TexinfoBuilder {
    /// Create a builder.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Run a complete Texinfo build into `options.output_dir`.
    pub fn build(
        options: &BuildOptions,
        config: &Config,
        env: &mut scribe_core::BuildEnvironment,
    ) -> OutputResult<BuildResult> {
        let PreparedBuild { parsed, mut warnings, .. } = prepare(options, config, env, BUILDER_NAME)?;
        let specs = parse_texinfo_documents(config);
        if specs.is_empty() {
            warnings.push("texinfo: no texinfo_documents config value found; no documents will be written".to_owned());
        }
        let project = config.get_str("project").unwrap_or("Project name not set").to_owned();
        let release = config.get_str("release").unwrap_or("").to_owned();
        let author = config.get_str("author").unwrap_or("Author name not set").to_owned();
        let copyright = config
            .get_str("copyright")
            .or_else(|| config.get_str("project_copyright"))
            .unwrap_or("")
            .to_owned();
        let date = config.get_str("today").filter(|s| !s.is_empty()).map(str::to_owned).unwrap_or_else(|| "Jan 01, 1970".to_owned());
        let generator = format!("scribe {}", env!("CARGO_PKG_VERSION"));

        let mut written = Vec::new();
        let mut built_docnames = Vec::new();
        for spec in &specs {
            let Some(start) = parsed.get(&spec.docname) else {
                warnings.push(format!("texinfo: texinfo_documents references unknown document {}", spec.docname));
                continue;
            };
            let mut documents: Vec<&Document> = vec![start];
            if spec.toctree_only {
                documents.clear();
            } else {
                for (name, document) in &parsed {
                    if name != &spec.docname {
                        documents.push(document);
                    }
                }
            }
            let header = TexinfoHeader {
                filename: spec.targetname.clone(),
                title: spec.title.clone(),
                author: if spec.author.is_empty() { author.clone() } else { spec.author.clone() },
                project_line: format!("{project} {release}"),
                copyright: copyright.clone(),
                category: spec.category.clone(),
                direntry: if spec.direntry.is_empty() { spec.targetname.clone() } else { spec.direntry.clone() },
                description: spec.description.clone(),
                date: date.clone(),
                generator: generator.clone(),
            };
            let content = render(&documents, &header);
            let relative = std::path::PathBuf::from(format!("{}.texi", spec.targetname));
            let path = options.output_dir.join(&relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| OutputError::io(BUILDER_NAME, parent, e))?;
            }
            std::fs::write(&path, content).map_err(|e| OutputError::io(BUILDER_NAME, &path, e))?;
            written.push(relative);
            built_docnames.push(spec.docname.clone());
        }

        Ok(BuildResult { written, built_docnames, warnings })
    }
}
