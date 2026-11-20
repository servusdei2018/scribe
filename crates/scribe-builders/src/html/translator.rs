//! Sphinx-faithful HTML5 translation of a frozen doctree.
//!
//! [`HTML5Translator`] implements the frozen [`Visitor`] trait and emits the
//! tags, CSS classes and attribute nesting produced by upstream's
//! `sphinx/writers/html5.py` for every node kind in the frozen AST. The output
//! is the *body* fragment inserted into Phase 16's `page.html` template.
//!
//! Design notes
//! ------------
//! * A stack of [`Frame`]s records the closing markup for each visited node;
//!   `depart_node` pops a frame and emits its close string.
//! * Nodes that render their whole subtree themselves (highlighted literal
//!   blocks, tables, toctrees, images, …) set `skip_depth` so child visits are
//!   invisible — exactly like upstream's `raise nodes.SkipNode`.
//! * `section_level` tracks the current `<hN>` depth.
//! * All internal links go through [`HtmlContext::resolve_refuri`] so `pathto`
//!   semantics (current page → `#`, other page → relative URI) are preserved.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use scribe_ast::{Document, Node, NodeId, NodeKind, Visitor};
use scribe_config::Config;
use scribe_core::BuildEnvironment;
use scribe_theme::RenderContext;

use crate::highlight::HighlightedCode;

/// Intra-page anchor rewriting state for the single-file builder.
///
/// When all documents are inlined into one page, section ids from different
/// documents may collide. [`crate::singlehtml::SingleFileHTMLBuilder`] rewrites
/// them to globally unique ids before translation and records the mapping here
/// so `refid`/`refuri` links are redirected to the new anchors.
#[derive(Clone, Debug, Default)]
pub struct SingleFileContext {
    /// Original anchor → rewritten, globally unique anchor.
    pub anchors: BTreeMap<String, String>,
    /// Docnames that are inlined into the single page.
    pub inlined_docs: BTreeSet<String>,
    /// Suppress inline toctrees (their content is spliced into the page and
    /// navigation is provided by the assembled global TOC).
    pub skip_toctrees: bool,
}

/// Everything the translator needs to resolve links/resources for one page.
///
/// It owns its data so pages can be rendered in parallel by
/// [`super::StandaloneHtmlBuilder`].
#[derive(Clone, Debug)]
pub struct HtmlContext {
    /// Sphinx docname of the page being rendered.
    pub docname: String,
    /// Resolved configuration. Shared across pages so rendering a large tree
    /// does not clone the whole registry once per document (Phase 32).
    pub config: Arc<Config>,
    /// Build environment, shared across pages. The translator only reads
    /// document titles from it, so an immutable `Arc` is sufficient and avoids
    /// an O(pages²) retention of the environment (Phase 32).
    pub env: Arc<BuildEnvironment>,
    /// Phase 16 theme render context (navigation/relations already filled).
    pub theme_ctx: RenderContext,
    /// `docname -> target URI` mapping (e.g. `index -> index.html`).
    pub target_uris: BTreeMap<String, String>,
    /// All known docnames.
    pub all_docs: BTreeSet<String>,
    /// Whether to emit `headerlink` permalinks on headings.
    pub add_permalinks: bool,
    /// Text/icon used for permalinks (upstream default `¶`).
    pub permalinks_icon: String,
    /// `image uri -> copied resource URI` mapping.
    pub image_uris: BTreeMap<String, String>,
    /// Document language.
    pub language: String,
    /// Set only by the single-file builder; triggers intra-page link rewriting.
    pub singlefile: Option<SingleFileContext>,
    /// Per-node graphviz metadata produced by `sphinx.ext.graphviz`, keyed by
    /// the frozen arena node id.
    pub graphviz: BTreeMap<u32, scribe_core::extensions::GraphvizHtmlInfo>,
}

impl HtmlContext {
    /// Relative URI from the current page to `other_uri`, matching
    /// `sphinx.util.osutil.relative_uri` / theme `pathto`.
    #[must_use]
    pub fn relative_uri(&self, other_uri: &str) -> String {
        if other_uri.starts_with('#') {
            return "#".to_owned();
        }
        if other_uri.contains("://") || other_uri.starts_with("mailto:") || other_uri.starts_with("data:") {
            return other_uri.to_owned();
        }
        let base = self.target_uris.get(&self.docname).cloned().unwrap_or_else(|| format!("{}.html", self.docname));
        let uri = scribe_theme::relative_uri(&base, other_uri);
        if uri.is_empty() { "#".to_owned() } else { uri }
    }

    /// Target URI of a docname, falling back to `docname.html`.
    #[must_use]
    pub fn target_uri(&self, docname: &str) -> String {
        self.target_uris.get(docname).cloned().unwrap_or_else(|| format!("{docname}.html"))
    }

    /// Remap a bare in-page anchor through the single-file rewriting table.
    #[must_use]
    pub fn resolve_anchor(&self, anchor: &str) -> String {
        if let Some(single) = &self.singlefile {
            single.anchors.get(anchor).map_or_else(|| anchor.to_owned(), String::to_owned)
        } else {
            anchor.to_owned()
        }
    }

    /// Resolve a `refuri` that may be a docname (with optional `#anchor`) into
    /// a page-relative link.
    #[must_use]
    pub fn resolve_refuri(&self, raw: &str) -> String {
        if raw.contains("://") || raw.starts_with("mailto:") || raw.starts_with('#') || raw.starts_with("data:") {
            return raw.to_owned();
        }
        let (path, anchor) = match raw.split_once('#') {
            Some((p, a)) => (p, Some(a)),
            None => (raw, None),
        };
        let docname = path.strip_suffix(".html").unwrap_or(path);
        if let Some(single) = &self.singlefile {
            if single.inlined_docs.contains(docname) || self.all_docs.contains(docname) {
                // Every inlined document lives on the same page. A bare doc
                // link becomes the `document-<docname>` marker; an anchor is
                // already globally unique and becomes a plain in-page link
                // (matching upstream's double-anchor collapse).
                return match anchor {
                    Some(fragment) => format!("#{}", single.anchors.get(fragment).map_or(fragment, String::as_str)),
                    None => format!("#document-{docname}"),
                };
            }
            // Special pages (genindex, search, ...) are not inlined.
            return match anchor {
                Some(fragment) => format!("{path}#{fragment}"),
                None => path.to_owned(),
            };
        }
        let mut uri = if self.all_docs.contains(docname) || self.target_uris.contains_key(docname) {
            self.relative_uri(&self.target_uri(docname))
        } else if path.ends_with(".html") {
            self.relative_uri(path)
        } else {
            self.target_uri(docname)
        };
        if let Some(anchor) = anchor {
            uri.push('#');
            uri.push_str(anchor);
        }
        uri
    }
}

/// One open element, remembered between `visit_node` and `depart_node`.
#[derive(Clone, Debug)]
struct Frame {
    close: String,
    /// Extra markup emitted *before* `close` (e.g. a heading permalink).
    extra: Option<String>,
    kind: NodeKind,
}

impl Frame {
    fn new(close: impl Into<String>, kind: NodeKind) -> Self {
        Self { close: close.into(), extra: None, kind }
    }
    fn with_extra(mut self, extra: String) -> Self {
        self.extra = Some(extra);
        self
    }
}

/// HTML5 translator over the frozen doctree.
pub struct HTML5Translator<'a> {
    doc: &'a Document,
    ctx: &'a HtmlContext,
    out: String,
    section_level: usize,
    stack: Vec<Frame>,
    /// Non-zero while the subtree of a self-rendering node is being skipped.
    skip_depth: usize,
    /// Set by `visit_kind` handlers that render their own children: applied in
    /// `visit_node` *after* the node's frame is pushed so depart stays balanced.
    skip_children: bool,
    /// Node whose children (and only whose children) are being skipped.
    skip_owner: Option<NodeId>,
    /// Nesting depth inside literal nodes (for `<span class="pre">`).
    literal_depth: usize,
}

impl<'a> HTML5Translator<'a> {
    /// Render `doc` into a body fragment.
    #[must_use]
    pub fn translate(doc: &'a Document, ctx: &'a HtmlContext) -> String {
        let mut translator = Self::new(doc, ctx);
        let _ = translator.walk(doc);
        translator.out
    }

    fn new(doc: &'a Document, ctx: &'a HtmlContext) -> Self {
        Self {
            doc,
            ctx,
            out: String::new(),
            section_level: 0,
            stack: Vec::new(),
            skip_depth: 0,
            skip_children: false,
            skip_owner: None,
            literal_depth: 0,
        }
    }

    // ----- low-level emit helpers ------------------------------------------

    fn emit(&mut self, text: &str) { self.out.push_str(text); }
    fn emitln(&mut self, text: &str) { self.out.push_str(text); self.out.push('\n'); }

    fn parent_kind(&self, id: NodeId) -> Option<NodeKind> {
        self.doc.parent(id).ok().flatten().and_then(|p| self.doc.get(p).ok()).map(|n| n.kind)
    }
    fn parent_node(&self, id: NodeId) -> Option<&Node> {
        self.doc.parent(id).ok().flatten().and_then(|p| self.doc.get(p).ok())
    }

    /// Next sibling of `id` in document order, when one exists.
    fn next_sibling(&self, id: NodeId) -> Option<&Node> {
        let parent = self.parent_node(id)?;
        let pos = parent.children.iter().position(|&child| child == id)?;
        parent.children.get(pos + 1).and_then(|&child| self.doc.get(child).ok())
    }

    /// Previous sibling of `id` in document order, when one exists.
    fn prev_sibling(&self, id: NodeId) -> Option<&Node> {
        let parent = self.parent_node(id)?;
        let pos = parent.children.iter().position(|&child| child == id)?;
        if pos == 0 { None } else { parent.children.get(pos - 1).and_then(|&child| self.doc.get(child).ok()) }
    }

    fn class_attr(&self, node: &Node, extra: &[&str]) -> String {
        let mut classes = node.classes().unwrap_or_default();
        for c in extra {
            if !classes.iter().any(|existing| existing == c) {
                classes.push((*c).to_owned());
            }
        }
        classes.join(" ")
    }

    /// Emit an opening tag with the node's `id`/`class` plus extra attributes.
    fn starttag(&mut self, node: &Node, tag: &str, extra_classes: &[&str], extra_attrs: &[(&str, &str)]) {
        let open = self.open_tag(node, tag, extra_classes, extra_attrs);
        self.emit(&open);
    }

    fn open_tag(&self, node: &Node, tag: &str, extra_classes: &[&str], extra_attrs: &[(&str, &str)]) -> String {
        let mut out = String::new();
        out.push('<');
        out.push_str(tag);
        let ids = node.ids().unwrap_or_default();
        // Docutils' `starttag` uses only the first id as the element's `id`
        // (extra ids become sibling `<span>` targets). A space-joined value
        // would be invalid HTML5 and break headerlink/anchor resolution.
        if let Some(first) = ids.iter().map(|id| id.trim()).find(|id| !id.is_empty()) {
            out.push_str(" id=\"");
            out.push_str(&escape_attr(first));
            out.push('"');
        }
        let classes = self.class_attr(node, extra_classes);
        if !classes.is_empty() {
            out.push_str(" class=\"");
            out.push_str(&escape_attr(&classes));
            out.push('"');
        }
        for (key, value) in extra_attrs {
            out.push(' ');
            out.push_str(key);
            out.push_str("=\"");
            out.push_str(&escape_attr(value));
            out.push('"');
        }
        out.push('>');
        out
    }

    fn is_simple_list(&self, id: NodeId) -> bool {
        let Ok(list) = self.doc.get(id) else { return false };
        if list.children.is_empty() { return false; }
        for &item in &list.children {
            let Ok(node) = self.doc.get(item) else { return false };
            if node.kind != NodeKind::ListItem { return false; }
            let mut paragraphs = 0usize;
            for &child in &node.children {
                let Ok(c) = self.doc.get(child) else { return false };
                match c.kind {
                    NodeKind::Paragraph => { paragraphs += 1; if paragraphs > 1 { return false; } }
                    NodeKind::BulletList | NodeKind::EnumeratedList => {
                        if !self.is_simple_list(child) { return false; }
                    }
                    _ => return false,
                }
            }
        }
        true
    }

    /// Render child ids with a fresh sub-translator using a proper visit/depart walk.
    fn render_children(&self, children: &[NodeId]) -> String {
        let mut translator = Self::new(self.doc, self.ctx);
        for &child in children {
            translator.walk_node(child);
        }
        translator.out
    }

    fn walk_node(&mut self, id: NodeId) {
        if !self.doc.is_live(id) { return; }
        let Ok(node) = self.doc.get(id) else { return };
        self.visit_node(node, id);
        for &child in &node.children {
            self.walk_node(child);
        }
        if let Ok(node) = self.doc.get(id) {
            self.depart_node(node, id);
        }
    }

    // ----- self-rendering composite nodes ----------------------------------

    fn render_toctree(&self, node: &Node) -> String {
        let entries = node_list(node, "entries");
        if entries.is_empty() { return String::new(); }
        let kinds = node_list(node, "entry_kinds");
        let refs = node_list(node, "entry_refs");
        let titles = node_list(node, "entry_titles");

        let mut out = String::new();
        if let Some(caption) = node.attrs.get_str("caption").ok().flatten().filter(|s| !s.is_empty()) {
            out.push_str("<p class=\"caption\"><span class=\"caption-text\">");
            out.push_str(&escape_html(caption));
            out.push_str("</span></p>\n");
        }
        out.push_str("<ul>\n");
        for (index, entry) in entries.iter().enumerate() {
            let kind = kinds.get(index).map(String::as_str).unwrap_or("doc");
            let reference = refs.get(index).map(String::as_str).unwrap_or(entry.as_str());
            let title = titles.get(index).filter(|s| !s.is_empty()).cloned()
                .unwrap_or_else(|| self.doc_title(entry).unwrap_or_else(|| entry.clone()));
            let (href, class_list) = if kind == "external" {
                (reference.to_owned(), "reference external".to_owned())
            } else {
                (self.ctx.resolve_refuri(&format!("{entry}.html")), "reference internal".to_owned())
            };
            let current = kind != "external" && entry == &self.ctx.docname;
            out.push_str("<li>");
            if current { out.push_str("<span class=\"toctree-anchor\">"); }
            out.push_str("<a class=\"");
            out.push_str(&class_list);
            out.push_str("\" href=\"");
            out.push_str(&escape_attr(&href));
            out.push_str("\">");
            out.push_str(&escape_html(&title));
            out.push_str("</a>");
            if current { out.push_str("</span>"); }
            out.push_str("</li>\n");
        }
        out.push_str("</ul>\n");
        out
    }

    fn doc_title(&self, docname: &str) -> Option<String> {
        self.ctx.env.documents.get(docname)
            .and_then(|snapshot| snapshot.doctree.as_ref())
            .and_then(|doc| {
                // Upstream toctree captions use `env.titles` (the *short*
                // title: first section heading), while HTML `<title>` uses
                // `env.longtitles` (the `.. title::` override). Prefer the
                // section heading here so an explicit title does not leak
                // into navigation links.
                doc.preorder_ids().into_iter().find_map(|id| {
                    let node = doc.get(id).ok()?;
                    if node.kind == NodeKind::Title { Some(node.astext(doc)) } else { None }
                }).or_else(|| doc.metadata.title.clone())
            })
            .or_else(|| self.ctx.env.all_docs.get(docname).and_then(|meta| meta.title.clone()))
    }

    fn render_literal_block(&self, node: &Node) -> Option<String> {
        let payload = HighlightedCode::from_node(node)?.ok()?;
        Some(payload.pygments_html())
    }

    fn render_table(&self, node: &Node) -> String {
        let mut out = self.open_tag(node, "table", &["docutils", "align-default"], &[]);
        out.push('\n');
        // Table caption (a `title` child).
        for &child in &node.children {
            if let Ok(title) = self.doc.get(child) {
                if title.kind == NodeKind::Title && !title.children.is_empty() {
                    out.push_str("<caption>");
                    out.push_str(&self.render_children(&[child]));
                    out.push_str("</caption>\n");
                }
            }
        }
        for &child in &node.children {
            if let Ok(tgroup) = self.doc.get(child) {
                if tgroup.kind == NodeKind::Tgroup { out.push_str(&self.render_tgroup(tgroup)); }
            }
        }
        out.push_str("</table>\n");
        out
    }

    fn render_tgroup(&self, tgroup: &Node) -> String {
        let mut out = String::new();
        let cols: Vec<&Node> = tgroup.children.iter()
            .filter_map(|&id| self.doc.get(id).ok())
            .filter(|n| n.kind == NodeKind::Colspec)
            .collect();
        if !cols.is_empty() {
            out.push_str("<colgroup>\n");
            for col in cols {
                let width = node_str(col, "colwidth");
                let style = if width.is_empty() { String::new() } else { format!(" style=\"width: {}\"", escape_attr(&width)) };
                out.push_str(&format!("<col{style} />\n"));
            }
            out.push_str("</colgroup>\n");
        }
        let mut row_index = 0usize;
        for &child in &tgroup.children {
            let Ok(section) = self.doc.get(child) else { continue };
            let (tag, is_body) = match section.kind {
                NodeKind::Thead => ("thead", false),
                NodeKind::Tbody => ("tbody", true),
                _ => continue,
            };
            out.push_str(&format!("<{tag}>\n"));
            for &rid in &section.children {
                let Ok(row) = self.doc.get(rid) else { continue };
                if row.kind != NodeKind::Row { continue; }
                let parity = if is_body && row_index % 2 == 1 { "row-even" } else { "row-odd" };
                if is_body { row_index += 1; }
                out.push_str(&format!("<tr class=\"{parity}\">"));
                for &eid in &row.children {
                    let Ok(entry) = self.doc.get(eid) else { continue };
                    if entry.kind != NodeKind::Entry { continue; }
                    let mut attrs = String::new();
                    if let Some(morerows) = entry.attrs.get_int("morerows").ok().flatten().filter(|n| *n > 0) {
                        attrs.push_str(&format!(" rowspan=\"{morerows}\""));
                    }
                    if let Some(morecols) = entry.attrs.get_int("morecols").ok().flatten().filter(|n| *n > 0) {
                        attrs.push_str(&format!(" colspan=\"{}\"", morecols + 1));
                    }
                    let classes = entry.classes().unwrap_or_default().join(" ");
                    if !classes.is_empty() {
                        attrs.push_str(&format!(" class=\"{}\"", escape_attr(&classes)));
                    }
                    out.push_str("<td");
                    out.push_str(&attrs);
                    out.push('>');
                    out.push_str(&self.render_children(&entry.children));
                    out.push_str("</td>");
                }
                out.push_str("</tr>\n");
            }
            out.push_str(&format!("</{tag}>\n"));
        }
        out
    }

    // ----- per-node visit handlers -----------------------------------------

    /// Emit the markup described by a graphviz extension node.
    fn render_graphviz(&mut self, node: &Node, id: NodeId) {
        let Some(info) = self.ctx.graphviz.get(&id.index()) else {
            self.emit(&escape_html(node.text_content().unwrap_or_default()));
            return;
        };
        let Some(uri) = &info.image_uri else {
            // `dot` unavailable: upstream appends the escaped code verbatim.
            self.emit(&escape_html(info.code.trim()));
            return;
        };
        let src = self.ctx.relative_uri(uri);
        let mut classes: Vec<String> = vec!["graphviz".to_owned()];
        for class in node.classes().unwrap_or_default() {
            if !classes.contains(&class) {
                classes.push(class);
            }
        }
        let class_attr = classes.join(" ");
        let alt = if info.alt.is_empty() {
            escape_attr(info.code.trim())
        } else {
            escape_attr(&info.alt)
        };
        let mut open_align = false;
        if let Some(align) = &info.align {
            self.emitln(&format!("<div align=\"{}\" class=\"align-{}\">", escape_attr(align), escape_attr(align)));
            open_align = true;
        }
        if info.format == "svg" {
            self.emitln("<div class=\"graphviz\">");
            self.emitln(&format!("<object data=\"{}\" type=\"image/svg+xml\" class=\"{}\">", escape_attr(&src), escape_attr(&class_attr)));
            self.emitln(&format!("<p class=\"warning\">{}</p>", alt));
            self.emitln("</object></div>");
        } else {
            self.emitln("<div class=\"graphviz\">");
            let usemap = info.usemap.as_ref().map(|id| format!(" usemap=\"#{}\"", escape_attr(id))).unwrap_or_default();
            self.emitln(&format!("<img src=\"{}\" alt=\"{}\"{} class=\"{}\" />", escape_attr(&src), alt, usemap, escape_attr(&class_attr)));
            self.emitln("</div>");
            if let Some(map) = &info.usemap_html {
                self.emitln(map);
            }
        }
        if open_align {
            self.emitln("</div>");
        }
    }

    fn admonition_title(&self, node: &Node) -> Option<String> {
        let first = node.children.first().copied()?;
        let child = self.doc.get(first).ok()?;
        if child.kind == NodeKind::Title { Some(child.astext(self.doc)) } else { None }
    }

    fn visit_title(&mut self, node: &Node, id: NodeId) -> Option<Frame> {
        match self.parent_kind(id) {
            Some(NodeKind::Admonition)
            | Some(NodeKind::Note) | Some(NodeKind::Warning) | Some(NodeKind::Tip)
            | Some(NodeKind::Hint) | Some(NodeKind::Important) | Some(NodeKind::Attention)
            | Some(NodeKind::Caution) | Some(NodeKind::Danger) | Some(NodeKind::Error)
            | Some(NodeKind::Seealso)
            | Some(NodeKind::VersionAdded) | Some(NodeKind::VersionChanged)
            | Some(NodeKind::Deprecated) | Some(NodeKind::VersionModified) => {
                let open = self.open_tag(node, "p", &["admonition-title"], &[]);
                self.emit(&open);
                Some(Frame::new("</p>\n", NodeKind::Title))
            }
            Some(NodeKind::Topic) | Some(NodeKind::Sidebar) => {
                let class = if self.parent_kind(id) == Some(NodeKind::Topic) { "topic-title" } else { "sidebar-title" };
                let open = self.open_tag(node, "p", &[class], &[]);
                self.emit(&open);
                Some(Frame::new("</p>\n", NodeKind::Title))
            }
            Some(NodeKind::Table) => {
                self.emit("<caption><span class=\"caption-text\">");
                Some(Frame::new("</span></caption>\n", NodeKind::Title))
            }
            Some(NodeKind::Figure) | Some(NodeKind::Caption) => Some(Frame::new("", NodeKind::Title)),
            _ => {
                let level = self.section_level.clamp(1, 6);
                let tag = format!("h{level}");
                let open = self.open_tag(node, &tag, &[], &[]);
                self.emit(&open);
                let mut frame = Frame::new(format!("</{tag}>\n"), NodeKind::Title);
                if self.ctx.add_permalinks {
                    if let Some(parent) = self.parent_node(id) {
                        let ids = parent.ids().unwrap_or_default();
                        let anchor = ids.first().map(|id| id.trim().to_owned()).filter(|id| !id.is_empty());
                        if let Some(anchor) = anchor {
                            frame = frame.with_extra(format!(
                                "<a class=\"headerlink\" href=\"#{}\" title=\"Link to this heading\">{}</a>",
                                escape_attr(&anchor), escape_html(&self.ctx.permalinks_icon)
                            ));
                        }
                    }
                }
                Some(frame)
            }
        }
    }

    fn visit_reference(&mut self, node: &Node) -> Option<Frame> {
        let mut class_list: Vec<String> = Vec::new();
        let internal = node.attrs.get_bool("internal").ok().flatten().unwrap_or(false);
        let refuri = node.attrs.get_str("refuri").ok().flatten();
        let refid = node.attrs.get_str("refid").ok().flatten();
        if internal {
            class_list.push("reference".to_owned());
            class_list.push("internal".to_owned());
        } else if refuri.is_some() {
            class_list.push("reference".to_owned());
            class_list.push("external".to_owned());
        } else {
            class_list.push("reference".to_owned());
        }
        for c in node.classes().unwrap_or_default() {
            if !class_list.contains(&c) { class_list.push(c); }
        }
        let mut out = format!("<a class=\"{}\"", escape_attr(&class_list.join(" ")));
        let href = if let Some(uri) = refuri {
            let resolved = self.ctx.resolve_refuri(uri);
            if let Some(fragment) = resolved.strip_prefix('#') {
                format!("#{}", self.ctx.resolve_anchor(fragment))
            } else {
                resolved
            }
        } else if let Some(id) = refid {
            format!("#{}", self.ctx.resolve_anchor(id))
        } else {
            String::new()
        };
        out.push_str(" href=\"");
        out.push_str(&escape_attr(&href));
        out.push('"');
        if let Some(title) = node.attrs.get_str("reftitle").ok().flatten().filter(|s| !s.is_empty()) {
            out.push_str(" title=\"");
            out.push_str(&escape_attr(title));
            out.push('"');
        }
        out.push('>');
        self.emit(&out);
        Some(Frame::new("</a>", NodeKind::Reference))
    }

    fn visit_image(&mut self, node: &Node) {
        let uri = node_str(node, "uri");
        let src = self.ctx.image_uris.get(&uri).cloned().unwrap_or_else(|| format!("../{uri}"));
        let mut classes: Vec<String> = vec!["aligned".to_owned()];
        if let Some(align) = node.attrs.get_str("align").ok().flatten().filter(|s| !s.is_empty()) {
            classes = vec![format!("align-{align}")];
        }
        let mut out = format!("<img src=\"{}\" alt=\"{}\" class=\"{}\"",
            escape_attr(&src),
            escape_attr(node.attrs.get_str("alt").ok().flatten().unwrap_or("")),
            escape_attr(&classes.join(" ")));
        for key in ["width", "height"] {
            if let Some(value) = node.attrs.get_str(key).ok().flatten().filter(|s| !s.is_empty()) {
                out.push_str(" style=\"");
                out.push_str(key);
                out.push_str(": ");
                out.push_str(&escape_attr(value));
                out.push_str(";\"");
            }
        }
        out.push_str(" />");
        self.emitln(&out);
    }

    fn visit_target(&mut self, node: &Node) {
        let refid = node_str(node, "refid");
        let ids = node.ids().unwrap_or_default();
        let anchor = ids.first().cloned().unwrap_or_else(|| refid.clone());
        if !anchor.is_empty() {
            self.emit(&format!("<span class=\"target\" id=\"{}\"></span>", escape_attr(&anchor)));
        }
    }

    fn visit_footnote(&mut self, node: &Node, id: NodeId) -> Option<Frame> {
        let class = if node.kind == NodeKind::Citation { "citation" } else { "footnote" };
        let prev_is_footnote = self
            .prev_sibling(id)
            .is_some_and(|prev| matches!(prev.kind, NodeKind::Footnote | NodeKind::Citation));
        let next_is_footnote = self
            .next_sibling(id)
            .is_some_and(|next| matches!(next.kind, NodeKind::Footnote | NodeKind::Citation));
        let mut out = String::new();
        if !prev_is_footnote {
            out.push_str("<aside class=\"footnote-list brackets\">\n");
        }
        out.push_str(&self.open_tag(node, "aside", &[class, "brackets"], &[("role", "doc-footnote")]));
        out.push('\n');
        self.emit(&out);
        let close = if next_is_footnote { "</aside>\n".to_owned() } else { "</aside>\n</aside>\n".to_owned() };
        Some(Frame::new(close, NodeKind::Footnote))
    }

    fn visit_footnote_reference(&mut self, node: &Node) -> Option<Frame> {
        let refid = node_str(node, "refid");
        let refname = node_str(node, "refname");
        let anchor = if refid.is_empty() { refname } else { refid };
        let mut out = String::from("<a class=\"footnote-reference brackets\"");
        if let Some(backrefs) = node.attrs.get_list("backrefs").ok().flatten() {
            if let Some(first) = backrefs.first() {
                out.push_str(&format!(" id=\"{}\"", escape_attr(first)));
            }
        }
        out.push_str(&format!(" role=\"doc-noteref\" href=\"#{}\">", escape_attr(&anchor)));
        out.push_str("<span class=\"fn-bracket\">[</span>");
        self.emit(&out);
        Some(Frame::new("<span class=\"fn-bracket\">]</span></a>", node.kind))
    }
}

fn node_str(node: &Node, key: &str) -> String {
    node.attrs.get_str(key).ok().flatten().unwrap_or("").to_owned()
}

/// Read a string-list attribute, accepting both the typed `List` form written
/// by the toctree collector and the JSON-string form used by the RST directive.
fn node_list(node: &Node, key: &str) -> Vec<String> {
    if let Some(values) = node.attrs.get_list(key).ok().flatten() {
        return values.clone();
    }
    let raw = node_str(node, key);
    if raw.is_empty() {
        Vec::new()
    } else if let Ok(values) = serde_json::from_str::<Vec<String>>(&raw) {
        values
    } else {
        split_escaped(&raw)
    }
}

/// Split a Sphinx list attribute while honouring backslash-escaped spaces.
fn split_escaped(raw: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(&next) = chars.peek() {
                if next == ' ' || next == '\\' {
                    current.push(next);
                    chars.next();
                    continue;
                }
            }
            current.push(ch);
        } else if ch.is_whitespace() {
            if !current.is_empty() {
                result.push(std::mem::take(&mut current));
            }
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}

/// Concatenate the text of a node's direct `Text` children (used by container
/// nodes such as `math` and `raw` whose payload lives in children, not attrs).
fn node_text(doc: &Document, node: &Node) -> String {
    node.children.iter()
        .filter_map(|&id| doc.get(id).ok())
        .filter_map(|child| child.text_content())
        .collect()
}

/// Render the local table of contents (sidebar `localtoc.html`) from the
/// sections of `doc`. The first top-level section links to `#` (the page top),
/// matching Sphinx's document-title behaviour.
#[must_use]
pub fn render_local_toc(doc: &Document, maxdepth: usize) -> String {
    fn sections_of(doc: &Document, parent: NodeId) -> Vec<NodeId> {
        doc.get(parent)
            .map(|node| node.children.iter().copied()
                .filter(|&id| doc.get(id).is_ok_and(|n| n.kind == NodeKind::Section))
                .collect())
            .unwrap_or_default()
    }
    fn title_of(doc: &Document, section: NodeId) -> Option<String> {
        doc.get(section).ok()?.children.iter().find_map(|&id| {
            let node = doc.get(id).ok()?;
            if node.kind == NodeKind::Title { Some(node.astext(doc)) } else { None }
        })
    }
    fn anchor_of(doc: &Document, section: NodeId) -> Option<String> {
        doc.get(section).ok()?.ids().ok()?.into_iter().next()
    }
    fn walk(doc: &Document, parent: NodeId, depth: usize, maxdepth: usize, first: bool, out: &mut String) {
        let sections = sections_of(doc, parent);
        if sections.is_empty() || depth > maxdepth { return; }
        out.push_str("<ul>\n");
        for section in sections {
            let title = title_of(doc, section).unwrap_or_default();
            let href = if first { "#".to_owned() } else {
                anchor_of(doc, section).map(|a| format!("#{a}")).unwrap_or_else(|| "#".to_owned())
            };
            out.push_str(&format!("<li><a class=\"reference internal\" href=\"{}\">{}</a>", escape_attr(&href), escape_html(&title)));
            if depth < maxdepth {
                let mut nested = String::new();
                walk(doc, section, depth + 1, maxdepth, false, &mut nested);
                if !nested.is_empty() { out.push('\n'); out.push_str(&nested); }
            }
            out.push_str("</li>\n");
        }
        out.push_str("</ul>\n");
    }
    let mut out = String::new();
    walk(doc, doc.root, 1, maxdepth, true, &mut out);
    out
}

/// Escape text for use in HTML content.
#[must_use]
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Escape text for use in a double-quoted HTML attribute.
#[must_use]
pub fn escape_attr(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

fn admonition_name(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Attention => "attention",
        NodeKind::Caution => "caution",
        NodeKind::Danger => "danger",
        NodeKind::Error => "error",
        NodeKind::Hint => "hint",
        NodeKind::Important => "important",
        NodeKind::Note => "note",
        NodeKind::Tip => "tip",
        NodeKind::Warning => "warning",
        NodeKind::Seealso => "seealso",
        _ => "note",
    }
}

fn titlecase(name: &str) -> String {
    if name == "seealso" { return "See also".to_owned(); }
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

impl Visitor for HTML5Translator<'_> {
    fn visit_node(&mut self, node: &Node, id: NodeId) {
        if self.skip_depth > 0 { self.skip_depth += 1; return; }
        // Every node pushes exactly one frame so visit/depart stay balanced,
        // including leaf nodes that emit no markup at all.
        let frame = self.visit_kind(node, id).unwrap_or_else(|| Frame::new("", node.kind));
        self.stack.push(frame);
        if self.skip_children {
            self.skip_children = false;
            self.skip_owner = Some(id);
            self.skip_depth = 1;
        }
    }

    fn depart_node(&mut self, _node: &Node, id: NodeId) {
        if self.skip_owner == Some(id) {
            // The owner's frame is still on the stack; skip_depth only ever
            // applied to its children, so clear it and pop normally.
            self.skip_owner = None;
            self.skip_depth = 0;
        } else if self.skip_depth > 0 {
            self.skip_depth -= 1;
            return;
        }
        if let Some(frame) = self.stack.pop() {
            if frame.kind == NodeKind::Section { self.section_level = self.section_level.saturating_sub(1); }
            if matches!(frame.kind, NodeKind::Literal | NodeKind::LiteralEmphasis | NodeKind::LiteralStrong) {
                self.literal_depth = self.literal_depth.saturating_sub(1);
            }
            if let Some(extra) = frame.extra { self.emit(&extra); }
            self.emit(&frame.close);
        }
    }
}

impl HTML5Translator<'_> {
    #[allow(clippy::too_many_lines)]
    fn visit_kind(&mut self, node: &Node, id: NodeId) -> Option<Frame> {
        match node.kind {
            NodeKind::Document => None,

            NodeKind::Section => {
                self.section_level += 1;
                let open = self.open_tag(node, "section", &[], &[]);
                self.emitln(&open);
                Some(Frame::new("</section>\n", NodeKind::Section))
            }

            NodeKind::Title => self.visit_title(node, id),

            NodeKind::Subtitle => {
                let class = if self.parent_kind(id) == Some(NodeKind::Sidebar) { "sidebar-subtitle" } else { "subtitle" };
                self.starttag(node, "p", &[class], &[]);
                Some(Frame::new("</p>\n", NodeKind::Subtitle))
            }

            NodeKind::Paragraph => {
                self.starttag(node, "p", &[], &[]);
                let compact = matches!(self.parent_kind(id), Some(NodeKind::ListItem) | Some(NodeKind::Entry));
                Some(Frame::new(if compact { "</p>" } else { "</p>\n" }, NodeKind::Paragraph))
            }

            NodeKind::CompactParagraph => None,

            NodeKind::Text => {
                let text = node.text_content().unwrap_or("");
                if self.literal_depth > 0 {
                    // Docutils' `protect_literal_text` wraps each word run in
                    // its own `<span class="pre">`, leaving whitespace bare.
                    let mut start = 0usize;
                    let mut in_whitespace = text.chars().next().is_some_and(char::is_whitespace);
                    for (index, ch) in text.char_indices() {
                        let whitespace = ch.is_whitespace();
                        if whitespace != in_whitespace {
                            let run = &text[start..index];
                            if !in_whitespace {
                                self.emit(&format!("<span class=\"pre\">{}</span>", escape_html(run)));
                            } else {
                                self.emit(&escape_html(run));
                            }
                            start = index;
                            in_whitespace = whitespace;
                        }
                    }
                    let run = &text[start..];
                    if !in_whitespace && !run.is_empty() {
                        self.emit(&format!("<span class=\"pre\">{}</span>", escape_html(run)));
                    } else {
                        self.emit(&escape_html(run));
                    }
                } else {
                    self.emit(&escape_html(text));
                }
                None
            }

            NodeKind::Emphasis => {
                self.starttag(node, "em", &[], &[]);
                Some(Frame::new("</em>", NodeKind::Emphasis))
            }

            NodeKind::LiteralEmphasis => {
                self.starttag(node, "em", &["docutils", "literal", "notranslate"], &[]);
                Some(Frame::new("</em>", NodeKind::LiteralEmphasis))
            }

            NodeKind::Strong => {
                self.starttag(node, "strong", &[], &[]);
                Some(Frame::new("</strong>", NodeKind::Strong))
            }

            NodeKind::LiteralStrong => {
                self.starttag(node, "strong", &["docutils", "literal", "notranslate"], &[]);
                Some(Frame::new("</strong>", NodeKind::LiteralStrong))
            }

            NodeKind::Literal => {
                let language = node.attrs.get_str("language").ok().flatten().map(str::to_owned);
                let is_code = node.classes().unwrap_or_default().iter().any(|class| class == "code");
                if let (Some(lang), true) = (language.as_deref(), is_code) {
                    // Sphinx `HTML5Translator.visit_literal`: a `:code:` role
                    // carrying a language is highlighted inline with the bare
                    // Pygments span markup (no `<pre>` / `<span class="pre">`).
                    let source = node.astext(self.doc);
                    let highlighted = crate::highlight::highlight_code(
                        &source,
                        lang,
                        &crate::highlight::HighlightOptions::default(),
                    );
                    let mut classes = node.classes().unwrap_or_default();
                    classes.extend([
                        "docutils".to_owned(),
                        "literal".to_owned(),
                        "highlight".to_owned(),
                        format!("highlight-{}", lang.replace('+', "p").to_ascii_lowercase()),
                    ]);
                    let mut tag = String::from("<code");
                    let ids = node.ids().unwrap_or_default();
                    if let Some(first) = ids.iter().map(|id| id.trim()).find(|id| !id.is_empty()) {
                        tag.push_str(&format!(" id=\"{}\"", escape_attr(first)));
                    }
                    tag.push_str(" class=\"");
                    tag.push_str(&escape_attr(&classes.join(" ")));
                    tag.push_str("\">");
                    self.emit(&tag);
                    self.emit(&highlighted.inline_html());
                    self.skip_children = true;
                    Some(Frame::new("</code>", NodeKind::Literal))
                } else {
                    self.literal_depth += 1;
                    self.starttag(node, "code", &["docutils", "literal", "notranslate"], &[]);
                    Some(Frame::new("</code>", NodeKind::Literal))
                }
            }

            NodeKind::Inline => {
                if node.classes().unwrap_or_default().is_empty() { None } else {
                    self.starttag(node, "span", &[], &[]);
                    Some(Frame::new("</span>", NodeKind::Inline))
                }
            }

            // Docutils' default (title-reference) role renders as `<cite>`.
            NodeKind::TitleReference => {
                self.starttag(node, "cite", &[], &[]);
                Some(Frame::new("</cite>", NodeKind::TitleReference))
            }

            // Sphinx keeps the `<dt>` open across a run of classifiers and
            // closes it from the last one (mirrors `writers/html5.py`).
            NodeKind::Classifier => {
                self.starttag(node, "span", &["classifier"], &[]);
                let another = self.next_sibling(id).is_some_and(|next| next.kind == NodeKind::Classifier);
                Some(Frame::new(if another { "</span>" } else { "</span></dt>" }, NodeKind::Classifier))
            }

            NodeKind::Problematic => {
                self.starttag(node, "span", &["problematic"], &[]);
                Some(Frame::new("</span>", NodeKind::Problematic))
            }

            NodeKind::Generated => {
                let marker = node.attrs.get_str(scribe_core::extensions::EXT_MARKER).ok().flatten();
                if marker == Some(scribe_core::extensions::EXT_GRAPHVIZ) {
                    self.render_graphviz(node, id);
                    self.skip_children = true;
                }
                None
            }

            NodeKind::Reference | NodeKind::NumberReference | NodeKind::DownloadReference | NodeKind::Manpage => self.visit_reference(node),

            NodeKind::PendingXref | NodeKind::PendingXrefCondition => None,

            NodeKind::LiteralBlock => {
                if let Some(html) = self.render_literal_block(node) {
                    self.emitln(&html);
                    self.skip_children = true;
                    None
                } else if node.children.iter().all(|&child| {
                    self.doc.get(child).is_ok_and(|n| matches!(n.kind, NodeKind::Text | NodeKind::Inline))
                }) {
                    // Plain `::` literal block: emit a Pygments-style shell even
                    // when no highlighter payload was attached, matching Sphinx.
                    let lang = node.attrs.get_str("language").ok().flatten().unwrap_or("none");
                    let mut html = format!("<div class=\"highlight-{lang} notranslate\"");
                    let ids = node.ids().unwrap_or_default();
                    if let Some(id) = ids.first() {
                        html.push_str(&format!(" id=\"{}\"", escape_attr(id.trim())));
                    }
                    html.push_str("><div class=\"highlight\"><pre><span></span>");
                    for &child in &node.children {
                        if let Ok(n) = self.doc.get(child) {
                            html.push_str(&escape_html(n.text_content().unwrap_or("")));
                        }
                    }
                    html.push_str("</pre></div>\n</div>");
                    self.emitln(&html);
                    self.skip_children = true;
                    None
                } else {
                    self.emit("<pre class=\"literal-block\">");
                    Some(Frame::new("</pre>\n", NodeKind::LiteralBlock))
                }
            }

            NodeKind::DoctestBlock => {
                self.emit("<pre class=\"doctest-block\">");
                Some(Frame::new("</pre>\n", NodeKind::DoctestBlock))
            }

            NodeKind::BulletList => {
                let classes = if self.is_simple_list(id) { vec!["simple"] } else { vec![] };
                let open = self.open_tag(node, "ul", &classes, &[]);
                self.emitln(&open);
                Some(Frame::new("</ul>\n", NodeKind::BulletList))
            }

            NodeKind::EnumeratedList => {
                let mut attrs: Vec<(&str, String)> = Vec::new();
                let start = node.attrs.get_int("start").ok().flatten().unwrap_or(1);
                if start != 1 { attrs.push(("start", start.to_string())); }
                let refs: Vec<(&str, &str)> = attrs.iter().map(|(k, v)| (*k, v.as_str())).collect();
                let classes = if self.is_simple_list(id) { vec!["simple"] } else { vec![] };
                let open = self.open_tag(node, "ol", &classes, &refs);
                self.emitln(&open);
                Some(Frame::new("</ol>\n", NodeKind::EnumeratedList))
            }

            NodeKind::ListItem => {
                self.emit("<li>");
                Some(Frame::new("</li>\n", NodeKind::ListItem))
            }

            NodeKind::DefinitionList => {
                let open = self.open_tag(node, "dl", &[], &[]);
                self.emitln(&open);
                Some(Frame::new("</dl>\n", NodeKind::DefinitionList))
            }

            NodeKind::DefinitionListItem => None,

            NodeKind::Term => {
                self.starttag(node, "dt", &[], &[]);
                // When classifiers follow, `depart_classifier` closes the <dt>.
                let has_classifier = self.next_sibling(id).is_some_and(|next| next.kind == NodeKind::Classifier);
                Some(Frame::new(if has_classifier { "" } else { "</dt>\n" }, NodeKind::Term))
            }

            NodeKind::Definition => {
                self.starttag(node, "dd", &[], &[]);
                Some(Frame::new("</dd>\n", NodeKind::Definition))
            }

            NodeKind::FieldList => {
                let classes = if self.is_simple_list(id) { vec!["simple"] } else { vec![] };
                let open = self.open_tag(node, "dl", &classes, &[]);
                self.emitln(&open);
                Some(Frame::new("</dl>\n", NodeKind::FieldList))
            }

            NodeKind::Field => None,

            NodeKind::FieldName => {
                self.starttag(node, "dt", &[], &[]);
                Some(Frame::new("</dt>\n", NodeKind::FieldName))
            }

            NodeKind::FieldBody => {
                self.starttag(node, "dd", &[], &[]);
                Some(Frame::new("</dd>\n", NodeKind::FieldBody))
            }

            NodeKind::OptionList => {
                let open = self.open_tag(node, "dl", &["option-list"], &[]);
                self.emitln(&open);
                Some(Frame::new("</dl>\n", NodeKind::OptionList))
            }

            NodeKind::OptionListItem => None,

            // Docutils 0.23 opens `<dt><kbd>` at `option_group`, not `option`.
            NodeKind::OptionGroup => {
                self.emit("<dt><kbd>");
                Some(Frame::new("</kbd></dt>\n", NodeKind::OptionGroup))
            }

            // Each `option` becomes a `<span class="option">`; consecutive
            // options are comma-separated (docutils' `depart_option`).
            NodeKind::Option => {
                self.starttag(node, "span", &["option"], &[]);
                let separator = if self.next_sibling(id).is_some_and(|next| next.kind == NodeKind::Option) {
                    ", "
                } else {
                    ""
                };
                Some(Frame::new(format!("</span>{separator}"), NodeKind::Option))
            }

            // `option_string` is transparent; its Text child carries the flag.
            NodeKind::OptionString => None,

            // `option_argument` emits its delimiter followed by `<var>...`.
            NodeKind::OptionArgument => {
                let delimiter = node.attrs.get_str("delimiter").ok().flatten().unwrap_or(" ");
                self.emit(&escape_html(delimiter));
                self.starttag(node, "var", &[], &[]);
                Some(Frame::new("</var>", NodeKind::OptionArgument))
            }

            NodeKind::Description => {
                self.starttag(node, "dd", &[], &[]);
                Some(Frame::new("</dd>\n", NodeKind::Description))
            }

            NodeKind::LineBlock => {
                let open = self.open_tag(node, "div", &["line-block"], &[]);
                self.emitln(&open);
                Some(Frame::new("</div>\n", NodeKind::LineBlock))
            }

            NodeKind::Line => {
                self.starttag(node, "div", &["line"], &[]);
                Some(Frame::new("</div>\n", NodeKind::Line))
            }

            NodeKind::BlockQuote => {
                let open = self.open_tag(node, "blockquote", &[], &[]);
                self.emitln(&open);
                Some(Frame::new("</blockquote>\n", NodeKind::BlockQuote))
            }

            NodeKind::Attribution => {
                self.starttag(node, "p", &["attribution"], &[]);
                Some(Frame::new("</p>\n", NodeKind::Attribution))
            }

            NodeKind::Epigraph | NodeKind::Highlights | NodeKind::PullQuote => {
                let class = match node.kind {
                    NodeKind::Epigraph => "epigraph",
                    NodeKind::Highlights => "highlights",
                    _ => "pull-quote",
                };
                let mut open = self.open_tag(node, "blockquote", &[class], &[]);
                open.push_str("\n<div>");
                self.emitln(&open);
                Some(Frame::new("</div></blockquote>\n", node.kind))
            }

            NodeKind::Compound | NodeKind::Container => {
                if self.ctx.singlefile.as_ref().is_some_and(|single| single.skip_toctrees)
                    && node.classes().unwrap_or_default().iter().any(|class| class == "toctree-wrapper")
                {
                    self.skip_children = true;
                    return None;
                }
                let open = self.open_tag(node, "div", &[], &[]);
                self.emitln(&open);
                Some(Frame::new("</div>\n", node.kind))
            }

            NodeKind::Topic => {
                let open = self.open_tag(node, "aside", &["topic"], &[]);
                self.emitln(&open);
                Some(Frame::new("</aside>\n", NodeKind::Topic))
            }

            NodeKind::Sidebar => {
                let open = self.open_tag(node, "aside", &["sidebar"], &[]);
                self.emitln(&open);
                Some(Frame::new("</aside>\n", NodeKind::Sidebar))
            }

            NodeKind::Rubric => {
                self.starttag(node, "p", &["rubric"], &[]);
                Some(Frame::new("</p>\n", NodeKind::Rubric))
            }

            NodeKind::Transition => {
                self.emitln("<hr class=\"docutils\" />");
                None
            }

            NodeKind::Admonition => {
                let mut open = self.open_tag(node, "div", &["admonition"], &[]);
                open.push('\n');
                self.emit(&open);
                Some(Frame::new("</div>\n", NodeKind::Admonition))
            }

            NodeKind::Attention | NodeKind::Caution | NodeKind::Danger | NodeKind::Error
            | NodeKind::Hint | NodeKind::Important | NodeKind::Note | NodeKind::Tip
            | NodeKind::Warning | NodeKind::Seealso => {
                let name = admonition_name(node.kind);
                let title = self.admonition_title(node).unwrap_or_else(|| titlecase(name));
                let mut open = self.open_tag(node, "div", &["admonition", name], &[]);
                open.push('\n');
                open.push_str(&format!("<p class=\"admonition-title\">{}</p>\n", escape_html(&title)));
                self.emit(&open);
                Some(Frame::new("</div>\n", node.kind))
            }

            // Sphinx emits `versionmodified` inline (`<span class="versionmodified …">`),
            // which the parser already materialises on the child `inline` node.
            NodeKind::VersionModified | NodeKind::VersionAdded | NodeKind::VersionChanged | NodeKind::Deprecated => None,

            NodeKind::Table => {
                let html = self.render_table(node);
                self.emitln(&html);
                self.skip_children = true;
                None
            }

            NodeKind::Tgroup | NodeKind::Colspec | NodeKind::Thead | NodeKind::Tbody
            | NodeKind::Row | NodeKind::Entry => None,

            NodeKind::Caption => {
                let parent = self.parent_node(id).filter(|parent| {
                    parent.kind == NodeKind::Container
                        && parent.attrs.get_bool("literal_block").ok().flatten().unwrap_or(false)
                });
                let caption_meta = parent.map(|parent| {
                    (
                        parent.ids().ok().and_then(|ids| ids.first().cloned()).unwrap_or_default(),
                        parent.attrs.get_str("fignumber").ok().flatten().map(str::to_owned),
                    )
                });
                if let Some((anchor, number)) = caption_meta {
                    // Sphinx `visit_caption`/`depart_caption` for a
                    // `literal-block-wrapper` container: caption number,
                    // text span and an optional headerlink permalink.
                    self.emit("<div class=\"code-block-caption\">");
                    if let Some(number) = number.as_deref() {
                        let prefix = self
                            .ctx
                            .config
                            .get_dict("numfig_format")
                            .and_then(|map| map.get("code-block"))
                            .and_then(|value| value.get_str().map(str::to_owned))
                            .unwrap_or_else(|| "Listing %s".to_owned());
                        self.emit(&format!(
                            "<span class=\"caption-number\">{} </span>",
                            escape_html(&prefix.replacen("%s", number, 1))
                        ));
                    }
                    self.emit("<span class=\"caption-text\">");
                    let mut close = String::from("</span>");
                    if self.ctx.add_permalinks && !anchor.is_empty() {
                        close.push_str(&format!(
                            "<a class=\"headerlink\" href=\"#{}\" title=\"Link to this code\">{}</a>",
                            escape_attr(&anchor),
                            escape_html(&self.ctx.permalinks_icon)
                        ));
                    }
                    close.push_str("</div>\n");
                    Some(Frame::new(close, NodeKind::Caption))
                } else {
                    self.starttag(node, "figcaption", &[], &[]);
                    Some(Frame::new("</figcaption>\n", NodeKind::Caption))
                }
            }

            NodeKind::Legend => {
                self.starttag(node, "div", &["legend"], &[]);
                Some(Frame::new("</div>\n", NodeKind::Legend))
            }

            NodeKind::Image => { self.visit_image(node); None }

            NodeKind::Figure => {
                let open = self.open_tag(node, "figure", &[], &[]);
                self.emitln(&open);
                Some(Frame::new("</figure>\n", NodeKind::Figure))
            }

            NodeKind::Target => { self.visit_target(node); None }

            NodeKind::Footnote | NodeKind::Citation => self.visit_footnote(node, id),

            NodeKind::FootnoteReference | NodeKind::CitationReference => self.visit_footnote_reference(node),

            NodeKind::Label => {
                let backref = self
                    .parent_node(id)
                    .and_then(|parent| parent.attrs.get_list("backrefs").ok().flatten())
                    .and_then(|refs| refs.first().cloned());
                let mut out = String::from("<span class=\"label\"><span class=\"fn-bracket\">[</span>");
                if let Some(backref) = backref {
                    out.push_str(&format!("<a role=\"doc-backlink\" href=\"#{}\">", escape_attr(&backref)));
                    self.emit(&out);
                    Some(Frame::new(
                        "</a><span class=\"fn-bracket\">]</span></span>\n",
                        NodeKind::Label,
                    ))
                } else {
                    out.push_str("<span class=\"fn-bracket\">]</span></span>\n");
                    self.emit(&out);
                    self.skip_children = true;
                    Some(Frame::new("", NodeKind::Label))
                }
            }

            NodeKind::Math => {
                self.emit(&format!("\\({}\\)", escape_html(&node_text(self.doc, node))));
                None
            }

            NodeKind::MathBlock => {
                self.emitln("\\[");
                self.emitln(&escape_html(&node_text(self.doc, node)));
                self.emitln("\\]");
                None
            }

            NodeKind::Raw => {
                let format = node_str(node, "format");
                if format.split('+').any(|f| matches!(f, "html" | "html5" | "html4")) {
                    self.emit(&node_text(self.doc, node));
                }
                self.skip_children = true;
                None
            }

            NodeKind::Comment => { self.skip_children = true; None }

            NodeKind::SystemMessage => {
                let open = self.open_tag(node, "div", &["system-message"], &[]);
                self.emitln(&open);
                Some(Frame::new("</div>\n", NodeKind::SystemMessage))
            }

            NodeKind::SubstitutionDefinition | NodeKind::SubstitutionReference => None,

            NodeKind::Toctree => {
                if self.ctx.singlefile.as_ref().is_some_and(|single| single.skip_toctrees) {
                    // Upstream's `inline_all_toctrees` replaces toctree nodes
                    // with the inlined documents' bodies.
                    self.skip_children = true;
                    return None;
                }
                let html = self.render_toctree(node);
                self.emit(&html);
                self.skip_children = true;
                None
            }

            NodeKind::Index => None,

            NodeKind::Glossary => {
                let open = self.open_tag(node, "dl", &["glossary"], &[]);
                self.emitln(&open);
                Some(Frame::new("</dl>\n", NodeKind::Glossary))
            }

            NodeKind::Productionlist => {
                let open = self.open_tag(node, "div", &["productionlist"], &[]);
                self.emitln(&open);
                Some(Frame::new("</div>\n", NodeKind::Productionlist))
            }

            NodeKind::Production => {
                self.starttag(node, "pre", &[], &[]);
                Some(Frame::new("</pre>\n", NodeKind::Production))
            }

            NodeKind::Only | NodeKind::Hlist | NodeKind::Hlistcol | NodeKind::Centered
            | NodeKind::Acks | NodeKind::TabularColSpec | NodeKind::Highlightlang | NodeKind::Pending => None,

            NodeKind::Desc => {
                let open = self.open_tag(node, "dl", &["sig", "sig-object"], &[]);
                self.emitln(&open);
                Some(Frame::new("</dl>\n", NodeKind::Desc))
            }

            NodeKind::DescSignature => {
                let open = self.open_tag(node, "dt", &["sig", "sig-object"], &[]);
                self.emit(&open);
                Some(Frame::new("</dt>\n", NodeKind::DescSignature))
            }

            NodeKind::DescSignatureLine => {
                self.emit("<span class=\"sig sig-object\">");
                Some(Frame::new("</span>", NodeKind::DescSignatureLine))
            }

            NodeKind::DescContent => {
                let open = self.open_tag(node, "dd", &[], &[]);
                self.emitln(&open);
                Some(Frame::new("</dd>\n", NodeKind::DescContent))
            }

            NodeKind::DescInline => {
                self.starttag(node, "span", &[], &[]);
                Some(Frame::new("</span>", NodeKind::DescInline))
            }

            NodeKind::DescName | NodeKind::DescAddname | NodeKind::DescType | NodeKind::DescAnnotation
            | NodeKind::DescReturns | NodeKind::DescSigName | NodeKind::DescSigKeywordType => {
                self.starttag(node, "span", &[], &[]);
                Some(Frame::new("</span>", node.kind))
            }

            NodeKind::DescParameterlist | NodeKind::DescTypeParameterList => {
                self.emit("<span class=\"sig-param\">(");
                Some(Frame::new(")</span>", node.kind))
            }

            NodeKind::DescParameter | NodeKind::DescTypeParameter | NodeKind::DescOptional => {
                self.starttag(node, "span", &["sig-param"], &[]);
                Some(Frame::new("</span>", node.kind))
            }

            NodeKind::DescSigSpace => { self.emit(" "); None }

            NodeKind::DescSigOperator => {
                self.starttag(node, "span", &["o"], &[]);
                Some(Frame::new("</span>", NodeKind::DescSigOperator))
            }

            NodeKind::DescSigPunctuation => {
                self.starttag(node, "span", &["p"], &[]);
                Some(Frame::new("</span>", NodeKind::DescSigPunctuation))
            }

            NodeKind::DescSigKeyword => {
                self.starttag(node, "span", &["k"], &[]);
                Some(Frame::new("</span>", NodeKind::DescSigKeyword))
            }

            NodeKind::DescSigLiteralNumber | NodeKind::DescSigLiteralString | NodeKind::DescSigLiteralChar => {
                let class = match node.kind {
                    NodeKind::DescSigLiteralNumber => "m",
                    _ => "s",
                };
                self.starttag(node, "span", &[class], &[]);
                Some(Frame::new("</span>", node.kind))
            }
        }
    }
}
