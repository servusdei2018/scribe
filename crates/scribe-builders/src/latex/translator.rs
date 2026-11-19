//! Sphinx-faithful LaTeX translation of a frozen doctree.
//!
//! [`LaTeXTranslator`] implements the frozen [`scribe_ast::Visitor`] trait and
//! turns a [`scribe_ast::Document`] into a complete, compilable LaTeX source
//! file. The emitted body uses the same command/environment vocabulary as
//! upstream Sphinx (`\sphinxVerbatim`, `sphinxadmonition`, `\sphinxincludegraphics`,
//! `\index`, `\hyperref`, `\label`, …) while the default preamble is
//! **self-contained**: it loads only widely available TeX Live packages and
//! defines the handful of Sphinx macros the body relies on. This makes the
//! generated `.tex` compilable with `pdflatex`, `xelatex` and `lualatex` out of
//! the box, which is verified by the Phase 23 PDF compilation test.
//!
//! Design notes
//! ------------
//! * A stack of [`Frame`]s records the closing markup for each visited node;
//!   `depart_node` pops a frame and emits its close string. Every node pushes
//!   exactly one frame so `visit`/`depart` stay balanced.
//! * Nodes that render their whole subtree themselves (tables, images,
//!   literal blocks, figures) set `skip_children`, mirroring upstream's
//!   `raise nodes.SkipNode`.
//! * All plain text goes through [`super::escape::escape_latex`] so TeX's
//!   special characters and problematic Unicode code points are handled.

use std::collections::BTreeSet;

use scribe_ast::{Document, Node, NodeId, NodeKind, Visitor};
use scribe_config::Config;

use super::escape::{escape_id, escape_latex, LatexEngine};

/// Everything the LaTeX translator needs to render one document.
///
/// The struct owns its data so documents can be rendered in parallel by
/// [`super::LatexBuilder`] (it is `Clone + Send`).
#[derive(Clone, Debug)]
pub struct LatexContext {
    /// Sphinx docname of the document being rendered.
    pub docname: String,
    /// Resolved configuration (kept for `latex_elements` customization).
    pub config: Config,
    /// LaTeX engine controlling escaping and font setup.
    pub engine: LatexEngine,
    /// Document title (from `latex_documents` or the doctree).
    pub title: String,
    /// Document author (from `latex_documents`).
    pub author: String,
    /// Release string (from `latex_documents` / `release`).
    pub release: String,
    /// Date string (deterministic: from `today` or empty).
    pub date: String,
    /// Sphinx LaTeX theme: `manual` (report class) or `howto` (article class).
    pub theme: String,
    /// User-supplied extra preamble code (`latex_elements["preamble"]`).
    pub extra_preamble: String,
    /// `true` to load the vendored `sphinx.sty` support files instead of the
    /// self-contained preamble (advanced fidelity mode; requires a full TeX
    /// Live installation).
    pub use_sphinx_sty: bool,
    /// Whether figures/tables/equations are numbered (`numfig`).
    pub numfig: bool,
    /// Document language code.
    pub language: String,
    /// Optional logo image file name (`latex_logo`).
    pub logo: Option<String>,
}

impl LatexContext {
    /// Build a `LatexContext` from a configuration and document metadata.
    #[must_use]
    pub fn from_config(
        config: &Config,
        docname: &str,
        title: String,
        author: String,
        theme: &str,
    ) -> Self {
        let engine = LatexEngine::from_name(config.get_str("latex_engine").unwrap_or("pdflatex"));
        let extra_preamble = config
            .get_dict("latex_elements")
            .and_then(|d| d.get("preamble"))
            .and_then(|v| v.get_str())
            .unwrap_or("")
            .to_owned();
        let use_sphinx_sty = config
            .get_dict("latex_elements")
            .and_then(|d| d.get("use_sphinx_sty"))
            .and_then(|v| v.get_bool())
            .unwrap_or(false);
        Self {
            docname: docname.to_owned(),
            config: config.clone(),
            engine,
            title,
            author,
            release: config.get_str("release").unwrap_or("").to_owned(),
            // Sphinx uses the current date for an empty `latex_documents` date.
            // We use `today` (empty by default) to keep `.tex` deterministic.
            date: config.get_str("today").unwrap_or("").to_owned(),
            theme: theme.to_owned(),
            extra_preamble,
            use_sphinx_sty,
            numfig: config.get_bool("numfig").unwrap_or(false),
            language: config.get_str("language").unwrap_or("en").to_owned(),
            logo: config.get_str("latex_logo").map(str::to_owned),
        }
    }

    /// `true` when the top-level sectioning command is `\chapter`.
    #[must_use]
    pub fn is_manual(&self) -> bool {
        !self.theme.eq_ignore_ascii_case("howto")
    }

    /// LaTeX class corresponding to the configured theme.
    #[must_use]
    pub fn document_class(&self) -> &'static str {
        if self.is_manual() { "report" } else { "article" }
    }
}

/// One open environment/group, remembered between `visit_node` and
/// `depart_node`.
#[derive(Clone, Debug)]
struct Frame {
    close: String,
    kind: NodeKind,
}

impl Frame {
    fn new(close: impl Into<String>, kind: NodeKind) -> Self {
        Self { close: close.into(), kind }
    }
    fn empty(kind: NodeKind) -> Self {
        Self { close: String::new(), kind }
    }
}

/// LaTeX translator over the frozen doctree.
pub struct LaTeXTranslator<'a> {
    doc: &'a Document,
    ctx: &'a LatexContext,
    /// Body markup (without preamble/closing).
    body: String,
    /// Current section nesting depth (0 before the first section).
    section_level: usize,
    /// Stack of closing strings.
    stack: Vec<Frame>,
    /// Non-zero while the subtree of a self-rendering node is skipped.
    skip_depth: usize,
    /// Node whose children (and only whose children) are being skipped.
    skip_owner: Option<NodeId>,
    /// Set by a `visit_kind` handler that renders its own children.
    skip_children: bool,
    /// Label of the section currently being visited (consumed by its title).
    section_label: Option<String>,
    /// Nesting depth inside `literal` inline nodes.
    literal_depth: usize,
    /// Admonition title children whose text is rendered as an environment
    /// argument and must therefore be suppressed during the normal walk.
    suppressed_titles: BTreeSet<NodeId>,
    /// Figures nested inside admonitions cannot float; render them in a
    /// `center` environment instead.
    no_float_figures: BTreeSet<NodeId>,
}

impl<'a> LaTeXTranslator<'a> {
    /// Render `doc` into a complete LaTeX source document.
    #[must_use]
    pub fn translate(doc: &'a Document, ctx: &'a LatexContext) -> String {
        let mut translator = Self::new(doc, ctx);
        // Walking a frozen document cannot fail for a live root; a best-effort
        // body is still returned if an arena lookup unexpectedly fails.
        let _ = translator.walk(doc);
        let body = std::mem::take(&mut translator.body);
        render_document(ctx, &body)
    }

    /// Render only the document body (without preamble/closing).
    ///
    /// Useful for tests and for callers embedding the output in their own
    /// LaTeX template.
    #[must_use]
    pub fn translate_body(doc: &'a Document, ctx: &'a LatexContext) -> String {
        let mut translator = Self::new(doc, ctx);
        let _ = translator.walk(doc);
        translator.body
    }

    fn new(doc: &'a Document, ctx: &'a LatexContext) -> Self {
        Self {
            doc,
            ctx,
            body: String::new(),
            section_level: 0,
            stack: Vec::new(),
            skip_depth: 0,
            skip_owner: None,
            skip_children: false,
            section_label: None,
            literal_depth: 0,
            suppressed_titles: BTreeSet::new(),
            no_float_figures: BTreeSet::new(),
        }
    }

    // ----- low-level emit helpers -----------------------------------------

    fn emit(&mut self, text: &str) { self.body.push_str(text); }
    fn emitln(&mut self, text: &str) { self.body.push_str(text); self.body.push('\n'); }

    fn esc(&self, text: &str) -> String { escape_latex(text, self.ctx.engine) }

    fn parent_kind(&self, id: NodeId) -> Option<NodeKind> {
        self.doc.parent(id).ok().flatten().and_then(|p| self.doc.get(p).ok()).map(|n| n.kind)
    }

    /// Kind of the previous sibling of `id`, when one exists.
    fn prev_sibling_kind(&self, id: NodeId) -> Option<NodeKind> {
        let parent = self.doc.parent(id).ok().flatten()?;
        let node = self.doc.get(parent).ok()?;
        let pos = node.children.iter().position(|&child| child == id)?;
        pos.checked_sub(1)
            .and_then(|previous| node.children.get(previous))
            .and_then(|&child| self.doc.get(child).ok())
            .map(|n| n.kind)
    }

    /// `true` when `id` is nested (at any depth) inside an admonition node.
    fn in_admonition(&self, id: NodeId) -> bool {
        self.doc.ancestors(id).filter_map(|a| self.doc.get(a).ok())
            .any(|n| n.kind.is_admonition())
    }

    fn first_id(&self, node: &Node) -> Option<String> {
        node.ids().ok().map(|ids| ids.first().cloned().unwrap_or_default())
            .filter(|s| !s.is_empty())
    }

    /// Render the children of `node` into a temporary string using a nested
    /// translator that shares the context but not the accumulator. Used by
    /// self-rendering nodes (tables, captions, …).
    fn render_children(&self, node: &Node) -> String {
        let mut nested = Self::new(self.doc, self.ctx);
        nested.section_level = self.section_level;
        for &child in &node.children {
            if let Ok(child_node) = self.doc.get(child) {
                nested.visit_node(child_node, child);
                // Manually walk the child's subtree in the nested translator.
                walk_child(nested_doc_ids(self.doc, child), &mut nested);
                nested.depart_node(child_node, child);
            }
        }
        nested.body
    }

    fn section_command(&self) -> &'static str {
        const HOWTO: &[&str] = &["section", "subsection", "subsubsection", "paragraph", "subparagraph"];
        const MANUAL: &[&str] = &["chapter", "section", "subsection", "subsubsection", "paragraph"];
        let commands = if self.ctx.is_manual() { MANUAL } else { HOWTO };
        let index = self.section_level.saturating_sub(1).min(commands.len() - 1);
        commands[index]
    }

    // ----- per-node handlers ----------------------------------------------

    #[allow(clippy::too_many_lines)]
    fn visit_kind(&mut self, node: &Node, id: NodeId) -> Frame {
        match node.kind {
            NodeKind::Document => Frame::empty(node.kind),

            NodeKind::Section => {
                self.section_level += 1;
                self.section_label = self.first_id(node).map(|s| escape_id(&s));
                Frame::empty(node.kind)
            }

            NodeKind::Title => self.visit_title(node, id),

            NodeKind::Subtitle => {
                self.emit("\\begin{center}\\textbf{");
                Frame::new("}\\end{center}\n", node.kind)
            }

            NodeKind::Paragraph => {
                // `\sphinxAtStartPar` is a no-op macro that lets us mark the
                // start of a paragraph without disturbing list items.
                self.emitln("\\sphinxAtStartPar");
                Frame::new("\n", node.kind)
            }

            NodeKind::CompactParagraph => Frame::empty(node.kind),

            NodeKind::Text => {
                let raw = node.text_content().unwrap_or("");
                let escaped = if self.literal_depth > 0 {
                    // Inline literal: escape and wrap in \texttt.
                    self.esc(raw)
                } else {
                    self.esc(raw)
                };
                self.emit(&escaped);
                Frame::empty(node.kind)
            }

            NodeKind::Emphasis | NodeKind::LiteralEmphasis => {
                self.emit("\\emph{");
                Frame::new("}", node.kind)
            }
            NodeKind::Strong | NodeKind::LiteralStrong => {
                self.emit("\\textbf{");
                Frame::new("}", node.kind)
            }
            NodeKind::Literal => {
                self.literal_depth += 1;
                self.emit("\\texttt{");
                Frame::new("}", node.kind)
            }
            NodeKind::Inline => {
                self.emit("{" );
                Frame::new("}", node.kind)
            }
            // Docutils default role: upstream renders `\sphinxtitleref{...}`.
            NodeKind::TitleReference => {
                self.emit("\\sphinxtitleref{");
                Frame::new("}", node.kind)
            }
            NodeKind::Problematic | NodeKind::Generated => {
                self.emit("\\textbf{");
                Frame::new("}", node.kind)
            }

            NodeKind::Reference | NodeKind::NumberReference | NodeKind::DownloadReference
            | NodeKind::Manpage => self.visit_reference(node),

            NodeKind::PendingXref | NodeKind::PendingXrefCondition => Frame::empty(node.kind),

            NodeKind::LiteralBlock | NodeKind::DoctestBlock => {
                self.render_literal_block(node);
                self.skip_children = true;
                Frame::empty(node.kind)
            }

            NodeKind::BulletList => {
                self.emitln("\\begin{itemize}");
                Frame::new("\\end{itemize}\n", node.kind)
            }
            NodeKind::EnumeratedList => {
                self.emitln("\\begin{enumerate}");
                Frame::new("\\end{enumerate}\n", node.kind)
            }
            NodeKind::ListItem => {
                self.emit("\\item ");
                Frame::new("\n", node.kind)
            }

            NodeKind::DefinitionList | NodeKind::FieldList | NodeKind::OptionList => {
                self.emitln("\\begin{description}");
                Frame::new("\\end{description}\n", node.kind)
            }
            NodeKind::DefinitionListItem | NodeKind::Field | NodeKind::OptionListItem => Frame::empty(node.kind),
            NodeKind::Term | NodeKind::FieldName => {
                let text = node.astext(self.doc);
                self.emit(&format!("\\item[{}] ", self.esc(&text)));
                self.skip_children = true;
                Frame::empty(node.kind)
            }
            // Upstream renders classifiers as `{[}...{]}` inside the \item.
            NodeKind::Classifier => {
                self.emit("{[}");
                Frame::new("{]}", node.kind)
            }
            NodeKind::OptionGroup => {
                self.emit("\\item[");
                Frame::new("] ", node.kind)
            }
            NodeKind::Option => {
                if self.prev_sibling_kind(id) == Some(NodeKind::Option) {
                    self.emit(", ");
                }
                Frame::empty(node.kind)
            }
            NodeKind::OptionString => {
                self.emit(&self.esc(&node.astext(self.doc)));
                self.skip_children = true;
                Frame::empty(node.kind)
            }
            NodeKind::OptionArgument => {
                let delimiter = node.attrs.get_str("delimiter").ok().flatten().unwrap_or(" ");
                self.emit(&self.esc(delimiter));
                Frame::empty(node.kind)
            }
            NodeKind::Definition | NodeKind::FieldBody | NodeKind::Description
            | NodeKind::Line => Frame::empty(node.kind),

            NodeKind::LineBlock => {
                self.emitln("\\begin{flushleft}");
                Frame::new("\\end{flushleft}\n", node.kind)
            }
            NodeKind::BlockQuote => {
                self.emitln("\\begin{quote}");
                Frame::new("\\end{quote}\n", node.kind)
            }
            NodeKind::Attribution => {
                self.emit("\\textit{");
                Frame::new("}\n", node.kind)
            }

            NodeKind::Table => {
                self.emit(&self.render_table(node));
                self.skip_children = true;
                Frame::empty(node.kind)
            }
            NodeKind::Tgroup | NodeKind::Colspec | NodeKind::Thead | NodeKind::Tbody
            | NodeKind::Row | NodeKind::Entry | NodeKind::TabularColSpec => Frame::empty(node.kind),

            NodeKind::Caption => {
                // Captions inside figures/tables are rendered by the owner.
                if self.parent_kind(id) == Some(NodeKind::Figure) {
                    let no_float = self.doc.parent(id).ok().flatten()
                        .is_some_and(|p| self.no_float_figures.contains(&p));
                    if no_float {
                        self.emit("\\textbf{");
                        Frame::new("}\\\\\n", node.kind)
                    } else {
                        self.emit("\\caption{");
                        Frame::new("}\n", node.kind)
                    }
                } else {
                    Frame::empty(node.kind)
                }
            }
            NodeKind::Legend => Frame::empty(node.kind),

            NodeKind::Image => {
                self.render_image(node);
                self.skip_children = true;
                Frame::empty(node.kind)
            }
            NodeKind::Figure => {
                if self.in_admonition(id) {
                    self.no_float_figures.insert(id);
                    self.emitln("\\begin{center}");
                    Frame::new("\\end{center}\n", node.kind)
                } else {
                    self.emitln("\\begin{figure}[htbp]");
                    self.emitln("\\centering");
                    Frame::new("\\end{figure}\n", node.kind)
                }
            }

            NodeKind::Admonition | NodeKind::Attention | NodeKind::Caution | NodeKind::Danger
            | NodeKind::Error | NodeKind::Hint | NodeKind::Important | NodeKind::Note
            | NodeKind::Tip | NodeKind::Warning | NodeKind::Seealso => self.visit_admonition(node, id),

            NodeKind::VersionModified | NodeKind::VersionAdded | NodeKind::VersionChanged
            | NodeKind::Deprecated => self.visit_versionmodified(node),

            NodeKind::Topic | NodeKind::Sidebar | NodeKind::Rubric | NodeKind::Compound
            | NodeKind::Container | NodeKind::Epigraph | NodeKind::Highlights
            | NodeKind::PullQuote => Frame::empty(node.kind),

            NodeKind::Transition => {
                self.emitln("\\bigskip\\hrule\\bigskip");
                Frame::empty(node.kind)
            }

            NodeKind::Target => {
                if let Some(label) = self.first_id(node) {
                    self.emitln(&format!("\\phantomsection\\label{{{}}}", escape_id(&label)));
                }
                Frame::empty(node.kind)
            }

            NodeKind::Footnote | NodeKind::Citation => {
                let label = self.first_id(node).unwrap_or_else(|| node.astext(self.doc));
                self.emit(&format!("\\footnote[{}]{{", escape_id(&label)));
                Frame::new("}", node.kind)
            }
            NodeKind::FootnoteReference | NodeKind::CitationReference => {
                let label = node.attrs.get_str("refid").ok().flatten()
                    .or_else(|| node.attrs.get_str("ids").ok().flatten())
                    .map(str::to_owned)
                    .unwrap_or_else(|| node.astext(self.doc));
                self.emit(&format!("\\ref{{{}}}", escape_id(&label)));
                self.skip_children = true;
                Frame::empty(node.kind)
            }
            NodeKind::Label => Frame::empty(node.kind),

            NodeKind::Math => {
                let raw = node.attrs.get_str("text").ok().flatten()
                    .map(str::to_owned)
                    .unwrap_or_else(|| node.astext(self.doc));
                let raw = raw.trim();
                self.emit(&format!("${raw}$"));
                self.skip_children = true;
                Frame::empty(node.kind)
            }
            NodeKind::MathBlock => {
                let raw = node.attrs.get_str("text").ok().flatten()
                    .map(str::to_owned)
                    .unwrap_or_else(|| node.astext(self.doc));
                let raw = raw.trim().to_owned();
                let numbered = self.ctx.numfig
                    && node.attrs.get_bool("numbered").ok().flatten().unwrap_or(false);
                if numbered {
                    self.emit(&format!("\\begin{{equation}}{raw}\\end{{equation}}\n"));
                } else {
                    self.emit(&format!("\\[{raw}\\]\n"));
                }
                self.skip_children = true;
                Frame::empty(node.kind)
            }

            NodeKind::Raw => {
                let format = node.attrs.get_str("format").ok().flatten().unwrap_or("");
                if matches!(format, "tex" | "latex") {
                    self.emit(&node.astext(self.doc));
                }
                self.skip_children = true;
                Frame::empty(node.kind)
            }

            NodeKind::Comment | NodeKind::SystemMessage | NodeKind::SubstitutionDefinition
            | NodeKind::SubstitutionReference | NodeKind::Pending => {
                self.skip_children = true;
                Frame::empty(node.kind)
            }

            NodeKind::Toctree => {
                // Upstream inserts the master toctree via `\sphinxtableofcontents`
                // in the preamble; an inline toctree degrades to a local contents
                // marker in the body.
                self.emitln("\\sphinxtableofcontents");
                self.skip_children = true;
                Frame::empty(node.kind)
            }

            NodeKind::Index => {
                let entries = node.attrs.get_list("entries").ok().flatten().unwrap_or_default();
                for entry in entries {
                    self.emit(&format!("\\index{{{}}}", escape_id(&entry)));
                }
                self.skip_children = true;
                Frame::empty(node.kind)
            }

            NodeKind::Glossary => {
                self.emitln("\\begin{description}");
                Frame::new("\\end{description}\n", node.kind)
            }
            NodeKind::Productionlist => {
                self.emitln("\\begin{verbatim}");
                Frame::new("\\end{verbatim}\n", node.kind)
            }
            NodeKind::Production => {
                self.emitln(&format!("{}:", self.esc(&node.astext(self.doc))));
                self.skip_children = true;
                Frame::empty(node.kind)
            }

            NodeKind::Only | NodeKind::Hlist | NodeKind::Hlistcol | NodeKind::Centered
            | NodeKind::Acks | NodeKind::Highlightlang => Frame::empty(node.kind),

            // Domain object signatures render as monospaced text with the
            // individual inline nodes concatenated.
            NodeKind::Desc => {
                self.emitln("\\begin{description}");
                Frame::new("\\end{description}\n", node.kind)
            }
            NodeKind::DescSignature | NodeKind::DescSignatureLine => {
                self.emit("\\item[");
                Frame::new("]\n", node.kind)
            }
            NodeKind::DescContent => Frame::empty(node.kind),
            NodeKind::DescInline | NodeKind::DescName | NodeKind::DescAddname
            | NodeKind::DescType | NodeKind::DescAnnotation | NodeKind::DescReturns
            | NodeKind::DescSigName | NodeKind::DescSigKeywordType => {
                self.emit("\\texttt{");
                Frame::new("}", node.kind)
            }
            NodeKind::DescParameterlist | NodeKind::DescTypeParameterList => {
                self.emit("(");
                Frame::new(")", node.kind)
            }
            NodeKind::DescParameter | NodeKind::DescTypeParameter | NodeKind::DescOptional => Frame::empty(node.kind),
            NodeKind::DescSigSpace => { self.emit(" "); Frame::empty(node.kind) }
            NodeKind::DescSigOperator | NodeKind::DescSigPunctuation | NodeKind::DescSigKeyword
            | NodeKind::DescSigLiteralNumber | NodeKind::DescSigLiteralString
            | NodeKind::DescSigLiteralChar => Frame::empty(node.kind),
        }
    }

    fn visit_title(&mut self, node: &Node, _id: NodeId) -> Frame {
        match self.parent_kind(_id) {
            Some(NodeKind::Section) => {
                let command = self.section_command();
                self.emit(&format!("\\{command}{{"));
                let label = self.section_label.take().unwrap_or_default();
                let close = if label.is_empty() {
                    "}\n".to_owned()
                } else {
                    format!("}}\\phantomsection\\label{{{label}}}\n")
                };
                Frame::new(close, node.kind)
            }
            Some(kind) if kind.is_admonition() => {
                // Admonition titles become the second argument of the
                // `sphinxadmonition` environment opened by the parent.
                Frame::empty(node.kind)
            }
            Some(NodeKind::Table) | Some(NodeKind::Figure) => Frame::empty(node.kind),
            _ => {
                self.emit("\\textbf{");
                Frame::new("}\n", node.kind)
            }
        }
    }

    fn visit_reference(&mut self, node: &Node) -> Frame {
        if let Some(uri) = node.attrs.get_str("refuri").ok().flatten() {
            if let Some(anchor) = uri.strip_prefix('#') {
                let anchor = escape_id(anchor);
                self.emit(&format!("\\hyperref[{anchor}]{{"));
            } else {
                let escaped_uri = uri.replace('%', "\\%").replace('#', "\\#");
                self.emit(&format!("\\href{{{escaped_uri}}}{{"));
            }
            return Frame::new("}", NodeKind::Reference);
        }
        if let Some(refid) = node.attrs.get_str("refid").ok().flatten() {
            self.emit(&format!("\\hyperref[{}]{{", escape_id(refid)));
            return Frame::new("}", NodeKind::Reference);
        }
        if let Some(name) = node.attrs.get_str("refname").ok().flatten() {
            self.emit(&format!("\\hyperref[{}]{{", escape_id(name)));
            return Frame::new("}", NodeKind::Reference);
        }
        Frame::empty(NodeKind::Reference)
    }

    fn visit_admonition(&mut self, node: &Node, id: NodeId) -> Frame {
        let (kind, title) = match node.kind {
            NodeKind::Admonition => {
                let title = node.children.iter()
                    .filter_map(|&c| self.doc.get(c).ok())
                    .find(|n| n.kind == NodeKind::Title)
                    .map(|n| n.astext(self.doc))
                    .unwrap_or_else(|| "Admonition".to_owned());
                ("admonition", title)
            }
            other => {
                let name = match other {
                    NodeKind::Attention => "attention", NodeKind::Caution => "caution",
                    NodeKind::Danger => "danger", NodeKind::Error => "error",
                    NodeKind::Hint => "hint", NodeKind::Important => "important",
                    NodeKind::Note => "note", NodeKind::Tip => "tip",
                    NodeKind::Warning => "warning", NodeKind::Seealso => "seealso",
                    _ => "note",
                };
                let pretty = match other {
                    NodeKind::Seealso => "See also".to_owned(),
                    _ => {
                        let mut s = name.to_owned();
                        if let Some(first) = s.get_mut(0..1) { first.make_ascii_uppercase(); }
                        s
                    }
                };
                (name, pretty)
            }
        };
        // The title child is rendered by the parent environment argument, so
        // skip it by marking it for suppression.
        self.emit(&format!(
            "\\begin{{sphinxadmonition}}{{{}}}{{{}}}",
            escape_id(kind),
            self.esc(&title)
        ));
        // Record which child is the title so `visit_kind` can skip its text.
        let title_id = node.children.iter().copied()
            .find(|&c| self.doc.get(c).is_ok_and(|n| n.kind == NodeKind::Title));
        if let Some(tid) = title_id {
            self.suppressed_titles.insert(tid);
            let _ = id;
        }
        Frame::new("\\end{sphinxadmonition}\n", node.kind)
    }

    fn visit_versionmodified(&mut self, node: &Node) -> Frame {
        let kind = node.kind.version_type().unwrap_or("versionmodified");
        let pretty = match kind {
            "versionadded" => "New in version",
            "versionchanged" => "Changed in version",
            "deprecated" => "Deprecated since version",
            _ => "Version",
        };
        let version = node.attrs.get_str("version").ok().flatten().unwrap_or("");
        self.emit(&format!("\\emph{{{pretty} {version}:}} "));
        Frame::empty(node.kind)
    }

    fn render_literal_block(&mut self, node: &Node) {
        let text = crate::highlight::HighlightedCode::from_node(node)
            .and_then(|r| r.ok())
            .map(|h| h.plain_text())
            .unwrap_or_else(|| node.astext(self.doc));
        self.emitln("\\begin{sphinxVerbatim}");
        for line in text.split('\n') {
            // Verbatim mode prints characters literally; transliterate
            // non-ASCII for pdflatex so the engine never sees an unknown slot.
            self.emitln(&verbatim_text(line, self.ctx.engine));
        }
        self.emitln("\\end{sphinxVerbatim}");
    }

    fn render_image(&mut self, node: &Node) {
        let uri = node.attrs.get_str("uri").ok().flatten().unwrap_or("").to_owned();
        if uri.is_empty() { return; }
        let width = node.attrs.get_str("width").ok().flatten().unwrap_or("");
        let mut opts = String::new();
        if !width.is_empty() {
            opts.push_str(&format!("width={},", latex_dimension(width)));
        }
        let align = node.attrs.get_str("align").ok().flatten().unwrap_or("");
        if !align.is_empty() {
            opts.push_str(&format!("align={align},"));
        }
        self.emitln(&format!("\\sphinxincludegraphics[{}]{{{}}}", opts, uri));
    }

    fn render_table(&self, node: &Node) -> String {
        let mut out = String::new();
        let tgroup = node.children.iter()
            .filter_map(|&c| self.doc.get(c).ok())
            .find(|n| n.kind == NodeKind::Tgroup);
        let Some(tgroup) = tgroup else { return out };
        let cols: Vec<&Node> = tgroup.children.iter()
            .filter_map(|&c| self.doc.get(c).ok())
            .filter(|n| n.kind == NodeKind::Colspec)
            .collect();
        let ncols = cols.len().max(1);
        let is_longtable = node.attrs.get_str("longtable").ok().flatten().is_some()
            || node.classes().ok().map(|c| c.iter().any(|c| c == "longtable")).unwrap_or(false);
        let env = if is_longtable { "longtable" } else { "tabularx" };
        let spec = if is_longtable {
            format!("|{}|", "c|".repeat(ncols))
        } else {
            format!("|{}|", "X|".repeat(ncols))
        };
        let caption = node.children.iter()
            .filter_map(|&c| self.doc.get(c).ok())
            .find(|n| n.kind == NodeKind::Title)
            .map(|n| self.render_children(n))
            .filter(|c| !c.is_empty());
        if is_longtable {
            out.push_str(&format!("\\begin{{{env}}}{{{spec}}}\n", env = env, spec = spec));
            if let Some(caption) = &caption {
                out.push_str(&format!("\\caption{{{caption}}}\\\\\n"));
            }
        } else {
            if let Some(caption) = &caption {
                out.push_str(&format!("\\begin{{center}}\\textbf{{{caption}}}\\\\\n"));
            }
            out.push_str(&format!("\\begin{{{env}}}{{\\textwidth}}{{{spec}}}\n", env = env, spec = spec));
        }
        out.push_str("\\hline\n");
        for &sid in &tgroup.children {
            let Ok(section) = self.doc.get(sid) else { continue };
            if !matches!(section.kind, NodeKind::Thead | NodeKind::Tbody) { continue; }
            for &rid in &section.children {
                let Ok(row) = self.doc.get(rid) else { continue };
                if row.kind != NodeKind::Row { continue; }
                let mut cells = Vec::new();
                for &eid in &row.children {
                    let Ok(entry) = self.doc.get(eid) else { continue };
                    if entry.kind != NodeKind::Entry { continue; }
                    cells.push(self.render_children(entry));
                }
                out.push_str(&cells.join(" & "));
                out.push_str(" \\\\\n\\hline\n");
            }
        }
        out.push_str(&format!("\\end{{{env}}}\n", env = env));
        if !is_longtable && node.children.iter().filter_map(|&c| self.doc.get(c).ok())
            .any(|n| n.kind == NodeKind::Title) {
            out.push_str("\\end{center}\n");
        }
        out
    }
}

impl Visitor for LaTeXTranslator<'_> {
    fn visit_node(&mut self, node: &Node, id: NodeId) {
        if self.skip_depth > 0 {
            self.skip_depth += 1;
            return;
        }
        if self.suppressed_titles.contains(&id) {
            self.stack.push(Frame::empty(node.kind));
            self.skip_owner = Some(id);
            self.skip_depth = 1;
            return;
        }
        let frame = self.visit_kind(node, id);
        self.stack.push(frame);
        if self.skip_children {
            self.skip_children = false;
            self.skip_owner = Some(id);
            self.skip_depth = 1;
        }
    }

    fn depart_node(&mut self, _node: &Node, id: NodeId) {
        if self.skip_owner == Some(id) {
            self.skip_owner = None;
            self.skip_depth = 0;
        } else if self.skip_depth > 0 {
            self.skip_depth -= 1;
            return;
        }
        if let Some(frame) = self.stack.pop() {
            if frame.kind == NodeKind::Section {
                self.section_level = self.section_level.saturating_sub(1);
            }
            if frame.kind == NodeKind::Literal {
                self.literal_depth = self.literal_depth.saturating_sub(1);
            }
            self.emit(&frame.close);
        }
    }
}

// The `suppressed_titles` set is kept on the translator via an extension below.
// (Declared as a separate impl block for clarity.)

/// Walk `ids` in depth-first order using `visitor`, without touching `visitor`'s
/// own frame stack invariants (used only by the self-rendering helpers).
fn walk_child(ids: Vec<NodeId>, visitor: &mut LaTeXTranslator<'_>) {
    for id in ids {
        if let Ok(node) = visitor.doc.get(id) {
            visitor.visit_node(node, id);
            walk_child(nested_doc_ids(visitor.doc, id), visitor);
            visitor.depart_node(node, id);
        }
    }
}

/// Direct children of `id` (used by the nested-render helpers).
fn nested_doc_ids(doc: &Document, id: NodeId) -> Vec<NodeId> {
    doc.get(id).map(|n| n.children.clone()).unwrap_or_default()
}

/// Convert an image width/height value from Sphinx/SVG CSS pixels into a
/// LaTeX dimension. Bare numbers and `px` suffixes are interpreted as CSS
/// pixels (`1px = 0.75bp`, matching upstream's `\sphinxpxdimen`); values with
/// an explicit TeX unit are passed through.
fn latex_dimension(value: &str) -> String {
    let value = value.trim();
    // Sphinx CSS-length units that TeX does not understand, converted to
    // big points with the same ratios graphicx uses for `px`.
    let css_units: &[(&str, f64)] = &[
        ("px", 0.75), ("ch", 5.0), ("em", 10.0), ("ex", 5.0),
        ("vw", 6.5), ("vh", 6.5), ("vmin", 6.5), ("vmax", 6.5),
        ("q", 0.71), ("Q", 0.71), ("mm", 2.84), ("cm", 28.45),
        ("in", 72.0), ("pt", 1.0),
    ];
    for (suffix, factor) in css_units {
        if let Some(num) = value.strip_suffix(suffix) {
            if let Ok(amount) = num.trim().parse::<f64>() {
                return format!("{}bp", amount * factor);
            }
        }
    }
    if value.chars().all(|c| c.is_ascii_digit() || c == '.') && !value.is_empty() {
        if let Ok(pixels) = value.parse::<f64>() {
            return format!("{}bp", pixels * 0.75);
        }
    }
    // Relative widths (e.g. `50%`) become a fraction of `\linewidth`.
    if let Some(percent) = value.strip_suffix('%') {
        if let Ok(p) = percent.trim().parse::<f64>() {
            return format!("{}\\linewidth", p / 100.0);
        }
    }
    value.to_owned()
}

/// Prepare verbatim text: keep ASCII as-is, transliterate non-ASCII for
/// `pdflatex` so every character has a LaTeX slot.
fn verbatim_text(text: &str, engine: LatexEngine) -> String {
    if engine.is_unicode() {
        return text.to_owned();
    }
    text.chars().map(|c| if c.is_ascii() { c } else { '?' }).collect()
}

/// Render the complete `.tex` document (preamble + body + closing).
#[must_use]
pub fn render_document(ctx: &LatexContext, body: &str) -> String {
    let mut out = String::new();
    out.push_str(&render_preamble(ctx));
    out.push_str("\\begin{document}\n");
    out.push_str("\\maketitle\n");
    out.push_str("\\tableofcontents\n");
    out.push_str("\\pagestyle{plain}\n");
    out.push_str(body);
    out.push_str("\n\\printindex\n");
    out.push_str("\\end{document}\n");
    out
}

/// Render the preamble for a generated document.
///
/// By default a self-contained preamble is emitted (see module docs). When
/// `ctx.use_sphinx_sty` is set, the vendored Sphinx support files are loaded
/// instead; that mode needs a full TeX Live installation.
#[must_use]
pub fn render_preamble(ctx: &LatexContext) -> String {
    let mut out = String::new();
    out.push_str("%% Generated by scribe (LaTeX writer).\n");
    if ctx.use_sphinx_sty {
        return render_sphinx_preamble(ctx, &mut out);
    }
    let class = ctx.document_class();
    let options = "letterpaper,11pt";
    out.push_str(&format!("\\documentclass[{options}]{{{class}}}\n"));
    out.push_str("\\usepackage[T1]{fontenc}\n");
    out.push_str("\\usepackage[utf8]{inputenc}\n");
    out.push_str("\\usepackage{amsmath,amssymb,amstext}\n");
    out.push_str("\\usepackage{graphicx}\n");
    out.push_str("\\usepackage{geometry}\n");
    out.push_str("\\usepackage{longtable}\n");
    out.push_str("\\usepackage{tabularx}\n");
    out.push_str("\\usepackage{multirow}\n");
    out.push_str("\\usepackage{booktabs}\n");
    out.push_str("\\usepackage{fancyvrb}\n");
    out.push_str("\\usepackage{framed}\n");
    out.push_str("\\usepackage{textcomp}\n");
    if ctx.engine.is_unicode() {
        out.push_str("\\usepackage{fontspec}\n");
    } else {
        out.push_str("\\usepackage{lmodern}\n");
    }
    out.push_str("\\usepackage{hyperref}\n");
    out.push_str("\\usepackage{makeidx}\n");
    out.push_str("\\makeindex\n");
    out.push_str("\\def\\sphinxAtStartPar{}\n");
    out.push_str("\\providecommand{\\sphinxtableofcontents}{}\n");
    out.push_str("\\providecommand{\\sphinxincludegraphics}[2][]{\\includegraphics[#1]{#2}}\n");
    out.push_str("\\DefineVerbatimEnvironment{sphinxVerbatim}{Verbatim}{fontsize=\\small,frame=lines}\n");
    out.push_str(
        "\\newenvironment{sphinxadmonition}[2]{%\n\
         \\begin{framed}\\textbf{#2:}\\\\\n\
         }{\\end{framed}}\n",
    );
    out.push('\n');
    out.push_str(&format!("\\title{{{}}}\n", ctx.title));
    out.push_str(&format!("\\date{{{}}}\n", ctx.date));
    out.push_str(&format!("\\author{{{}}}\n", ctx.author));
    if !ctx.release.is_empty() {
        out.push_str(&format!("\\newcommand{{\\release}}{{{}}}\n", ctx.release));
    } else {
        out.push_str("\\newcommand{\\release}{}\n");
    }
    if !ctx.extra_preamble.is_empty() {
        out.push_str(&ctx.extra_preamble);
        out.push('\n');
    }
    out
}

/// Sphinx-fidelity preamble that loads the vendored support files copied next
/// to the generated `.tex`. This is intentionally minimal; it is only enabled
/// through `latex_elements["use_sphinx_sty"]`.
fn render_sphinx_preamble(ctx: &LatexContext, out: &mut String) -> String {
    let class = if ctx.is_manual() { "sphinxmanual" } else { "sphinxhowto" };
    let docclass = ctx.document_class();
    out.push_str(&format!("\\def\\sphinxdocclass{{{docclass}}}\n"));
    out.push_str(&format!("\\documentclass[letterpaper,10pt]{{{class}}}\n"));
    out.push_str("\\usepackage[T1]{fontenc}\n");
    out.push_str("\\usepackage[utf8]{inputenc}\n");
    out.push_str("\\usepackage{amsmath,amssymb,amstext}\n");
    out.push_str("\\usepackage{sphinx}\n");
    out.push_str("\\usepackage{hyperref}\n");
    out.push_str("\\makeindex\n");
    out.push_str("\\def\\sphinxAtStartPar{}\n");
    out.push_str(&format!("\\title{{{}}}\n", ctx.title));
    out.push_str(&format!("\\date{{{}}}\n", ctx.date));
    out.push_str(&format!("\\author{{{}}}\n", ctx.author));
    out.push_str("\\newcommand{\\sphinxlogo}{\\vbox{}}\n");
    out.push_str("\\newcommand{\\release}{}\n");
    if !ctx.extra_preamble.is_empty() {
        out.push_str(&ctx.extra_preamble);
        out.push('\n');
    }
    out.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> LatexContext {
        LatexContext {
            docname: "index".into(),
            config: Config::new(),
            engine: LatexEngine::PdfLatex,
            title: "Test Doc".into(),
            author: "Author".into(),
            release: String::new(),
            date: String::new(),
            theme: "manual".into(),
            extra_preamble: String::new(),
            use_sphinx_sty: false,
            numfig: false,
            language: "en".into(),
            logo: None,
        }
    }

    #[test]
    fn empty_document_produces_complete_tex() {
        let doc = Document::new(None);
        let tex = LaTeXTranslator::translate(&doc, &ctx());
        assert!(tex.starts_with("%% Generated by scribe"));
        assert!(tex.contains("\\documentclass"));
        assert!(tex.contains("\\begin{document}"));
        assert!(tex.contains("\\end{document}"));
        assert!(tex.contains("\\makeindex"));
    }

    #[test]
    fn section_and_title_emit_chapter_and_label() {
        let mut doc = Document::new(None);
        let section = doc.add(NodeKind::Section, Some(doc.root)).unwrap();
        doc.get_mut(section).unwrap().set_list("ids", vec!["intro".into()]);
        let title = doc.add(NodeKind::Title, Some(section)).unwrap();
        doc.new_text("Introduction", Some(title)).unwrap();
        let tex = LaTeXTranslator::translate(&doc, &ctx());
        assert!(tex.contains("\\chapter{"), "{tex}");
        assert!(tex.contains("Introduction"), "{tex}");
        assert!(tex.contains("\\label{intro}"), "{tex}");
    }

    #[test]
    fn inline_markup_maps_to_latex_commands() {
        let mut doc = Document::new(None);
        let p = doc.add(NodeKind::Paragraph, Some(doc.root)).unwrap();
        let em = doc.add(NodeKind::Emphasis, Some(p)).unwrap();
        doc.new_text("yes", Some(em)).unwrap();
        let strong = doc.add(NodeKind::Strong, Some(p)).unwrap();
        doc.new_text("no", Some(strong)).unwrap();
        let lit = doc.add(NodeKind::Literal, Some(p)).unwrap();
        doc.new_text("code", Some(lit)).unwrap();
        let body = LaTeXTranslator::translate_body(&doc, &ctx());
        assert!(body.contains("\\emph{yes}"), "{body}");
        assert!(body.contains("\\textbf{no}"), "{body}");
        assert!(body.contains("\\texttt{code}"), "{body}");
    }

    #[test]
    fn lists_and_admonitions_use_sphinx_environments() {
        let mut doc = Document::new(None);
        let ul = doc.add(NodeKind::BulletList, Some(doc.root)).unwrap();
        let li = doc.add(NodeKind::ListItem, Some(ul)).unwrap();
        doc.new_text("item", Some(li)).unwrap();
        let note = doc.add(NodeKind::Note, Some(doc.root)).unwrap();
        doc.new_text("body", Some(note)).unwrap();
        let body = LaTeXTranslator::translate_body(&doc, &ctx());
        assert!(body.contains("\\begin{itemize}"), "{body}");
        assert!(body.contains("\\item"), "{body}");
        assert!(body.contains("\\begin{sphinxadmonition}"), "{body}");
    }

    #[test]
    fn math_and_index_are_rendered() {
        let mut doc = Document::new(None);
        let math = doc.add(NodeKind::Math, Some(doc.root)).unwrap();
        doc.get_mut(math).unwrap().set_str("text", "x^2");
        let mb = doc.add(NodeKind::MathBlock, Some(doc.root)).unwrap();
        doc.get_mut(mb).unwrap().set_str("text", "E=mc^2");
        let index = doc.add(NodeKind::Index, Some(doc.root)).unwrap();
        doc.get_mut(index).unwrap().set_list("entries", vec!["Einstein".into()]);
        let body = LaTeXTranslator::translate_body(&doc, &ctx());
        assert!(body.contains("$x^2$"), "{body}");
        assert!(body.contains("\\[E=mc^2\\]"), "{body}");
        assert!(body.contains("\\index{Einstein}"), "{body}");
    }

    #[test]
    fn howto_uses_section_not_chapter() {
        let mut c = ctx();
        c.theme = "howto".into();
        let mut doc = Document::new(None);
        let section = doc.add(NodeKind::Section, Some(doc.root)).unwrap();
        let title = doc.add(NodeKind::Title, Some(section)).unwrap();
        doc.new_text("Title", Some(title)).unwrap();
        let body = LaTeXTranslator::translate_body(&doc, &c);
        assert!(body.contains("\\section{"), "{body}");
        assert!(!body.contains("\\chapter{"), "{body}");
    }

    #[test]
    fn deterministic_output() {
        let mut doc = Document::new(None);
        let p = doc.add(NodeKind::Paragraph, Some(doc.root)).unwrap();
        doc.new_text("hello", Some(p)).unwrap();
        let a = LaTeXTranslator::translate(&doc, &ctx());
        let b = LaTeXTranslator::translate(&doc, &ctx());
        assert_eq!(a, b);
    }
}
