//! Toctree collection, document hierarchy and section numbering (Phase 09).
//!
//! This module is the Scribe counterpart of
//! `sphinx/environment/collectors/toctree.py` and
//! `sphinx/environment/adapters/toctree.py`. It consumes the frozen §7 AST
//! produced by the `scribe-rst` `toctree` directive, expands `:glob:`
//! patterns against the frozen §08 `BuildEnvironment`, populates the
//! environment's toctree seams and derives:
//!
//! * a cycle-free parent → children hierarchy (`ToctreeHierarchy`),
//! * pre-order navigation relations (`NavRelation`, matching upstream
//!   `BuildEnvironment.collect_relations()` / `_traverse_toctree()`),
//! * global section numbers (`SectionNumbers`, matching upstream
//!   `TocTreeCollector.assign_section_numbers()`), and
//! * figure / table / code-block numbers (`FigureNumbers`, matching
//!   `assign_figure_numbers()` when `numfig` is enabled).
//!
//! The frozen `BuildEnvironment` public shape is **not** changed: state is
//! written exclusively through its documented seams (`note_toctree_edge`,
//! `set_toctree_includes`, `note_file_to_rebuild`, `note_glob_toctree`,
//! `note_numbered_toctree`) and its public maps/fields. Because the frozen
//! seam methods sort child lists while upstream preserves source order, the
//! ordered edge list is installed through the public `toctree_includes` field
//! after the seam has recorded the edges; this is documented in §13 of
//! `ARCHITECTURE.md`.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;

use miette::{Diagnostic, LabeledSpan, SourceCode, SourceSpan};
use scribe_ast::{AttributeValue, Document, Node, NodeId, NodeKind, Span};
use thiserror::Error;

use crate::glob::{translate_pattern, GlobSpec};
use crate::BuildEnvironment;

/// Attribute written by the RST directive (see `scribe_rst::toctree`).
const ATTR_ENTRY_REFS: &str = "entry_refs";
/// Parallel explicit titles.
const ATTR_ENTRY_TITLES: &str = "entry_titles";
/// Parallel entry classifiers.
const ATTR_ENTRY_KINDS: &str = "entry_kinds";
/// Resolved child docnames.
const ATTR_ENTRIES: &str = "entries";
/// Upstream's resolved child docname attribute.
const ATTR_INCLUDEFIES: &str = "includefiles";
const KIND_DOC: &str = "doc";
const KIND_EXTERNAL: &str = "external";
const KIND_SELF: &str = "self";

// ---------------------------------------------------------------------------
// Public model
// ---------------------------------------------------------------------------

/// Where a toctree entry points.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryTarget {
    /// An internal document (possibly with an explicit title).
    Doc(String),
    /// An external URL, rendered with `refuri`.
    External(String),
    /// The literal `self` reference (the containing document).
    SelfRef,
}

impl EntryTarget {
    /// The docname for internal entries.
    pub fn docname(&self) -> Option<&str> {
        match self {
            Self::Doc(name) => Some(name),
            _ => None,
        }
    }

    /// The URI for external entries. `self` has URI `""`.
    pub fn uri(&self) -> Option<&str> {
        match self {
            Self::External(uri) => Some(uri),
            Self::SelfRef => Some(""),
            Self::Doc(_) => None,
        }
    }
}

/// One structured toctree entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToctreeEntry {
    /// Explicit title, or `None` to use the target document's title.
    pub title: Option<String>,
    /// The resolved target.
    pub target: EntryTarget,
    /// The raw ref exactly as written in the directive body.
    pub raw_ref: String,
}

impl ToctreeEntry {
    /// Whether this entry is an external URL.
    pub fn is_external(&self) -> bool {
        matches!(self.target, EntryTarget::External(_))
    }

    /// Whether this entry is a `self` reference.
    pub fn is_self(&self) -> bool {
        self.target == EntryTarget::SelfRef
    }

    /// The internal docname, if any.
    pub fn docname(&self) -> Option<&str> {
        self.target.docname()
    }
}

/// Options of one `toctree` directive, mirroring upstream defaults.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToctreeOptions {
    /// `:maxdepth:` (default `-1` = unlimited).
    pub maxdepth: i64,
    /// `:caption:`.
    pub caption: Option<String>,
    /// `:glob:`.
    pub glob: bool,
    /// `:hidden:` (affects rendering, not hierarchy traversal).
    pub hidden: bool,
    /// `:includehidden:` (affects resolved global ToC rendering).
    pub includehidden: bool,
    /// `:numbered:` (`0` = off, `999` for a bare flag).
    pub numbered: i64,
    /// `:titlesonly:`.
    pub titlesonly: bool,
    /// `:reversed:` (entries already reversed by the directive).
    pub reversed: bool,
    /// `:name:` (installed on the wrapper node).
    pub name: Option<String>,
}

impl Default for ToctreeOptions {
    fn default() -> Self {
        Self {
            maxdepth: -1,
            caption: None,
            glob: false,
            hidden: false,
            includehidden: false,
            numbered: 0,
            titlesonly: false,
            reversed: false,
            name: None,
        }
    }
}

/// A parsed and resolved toctree node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedToctree {
    /// Document containing the directive.
    pub docname: String,
    /// Directive options.
    pub options: ToctreeOptions,
    /// All entries (including URLs and `self`).
    pub entries: Vec<ToctreeEntry>,
    /// Resolved internal child docnames, in source order.
    pub includefiles: Vec<String>,
    /// Byte span of the directive, when the source carried one.
    pub span: Option<Span>,
    /// 1-based source line, when the source carried one.
    pub line: Option<u32>,
    /// Arena id of the `NodeKind::Toctree` node.
    pub node_id: NodeId,
}

/// A recoverable toctree diagnostic, rendered with miette.
#[derive(Clone, Debug, Error)]
#[error("{message}")]
pub struct ToctreeWarning {
    /// Stable diagnostic code, e.g. `scribe::toc::circular`.
    pub code: &'static str,
    /// Human-readable message (includes the cycle path for cycle warnings).
    pub message: String,
    /// miette help label.
    pub help: &'static str,
    /// Byte span of the offending directive.
    pub span: SourceSpan,
    /// Source name used for diagnostic rendering.
    pub source_name: String,
    /// 1-based source line of the offending directive, when known.
    ///
    /// Additive span field: located toctree warnings (for example
    /// `toc.secnum`) include `:LINE:` in upstream's log prefix.
    pub line: Option<u32>,
    /// Source text used for diagnostic rendering.
    pub source_text: Arc<String>,
}

impl Diagnostic for ToctreeWarning {
    fn code<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        Some(Box::new(self.code))
    }
    fn help<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        Some(Box::new(self.help))
    }
    fn severity(&self) -> Option<miette::Severity> {
        Some(miette::Severity::Warning)
    }
    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        Some(Box::new(std::iter::once(LabeledSpan::new_with_span(
            Some("toctree entry here".to_owned()),
            self.span,
        ))))
    }
    fn source_code(&self) -> Option<&dyn SourceCode> {
        Some(&self.source_text)
    }
}

impl ToctreeWarning {
    fn new(
        code: &'static str,
        message: impl Into<String>,
        help: &'static str,
        span: Option<Span>,
        source_name: &str,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            help,
            span: span
                .map(|value| SourceSpan::new((value.start as usize).into(), value.end.saturating_sub(value.start) as usize))
                .unwrap_or_else(|| SourceSpan::new(0usize.into(), 0usize)),
            source_name: source_name.to_owned(),
            line: None,
            source_text: Arc::new(String::new()),
        }
    }

    /// Attaches the 1-based source line of the offending toctree directive.
    pub fn with_line(mut self, line: Option<u32>) -> Self {
        self.line = line;
        self
    }
}

/// Section numbers by document and anchor. An empty vector means "unnumbered"
/// (upstream's empty tuple), e.g. entries beyond the numbered depth.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SectionNumbers {
    /// `docname → (section anchor → number components)`.
    pub by_doc: BTreeMap<String, BTreeMap<String, Vec<i64>>>,
}

impl SectionNumbers {
    /// Number of a section anchor, if assigned.
    pub fn get(&self, docname: &str, anchor: &str) -> Option<&[i64]> {
        self.by_doc.get(docname)?.get(anchor).map(Vec::as_slice)
    }

    /// Dotted representation (`1.2.3`), or an empty string when unnumbered.
    pub fn display(&self, docname: &str, anchor: &str) -> String {
        self.get(docname, anchor)
            .map(|numbers| numbers.iter().map(i64::to_string).collect::<Vec<_>>().join("."))
            .unwrap_or_default()
    }
}

/// Figure/table/code-block numbers by document, type and node id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FigureNumbers {
    /// `docname → figtype → node id → number components`.
    pub by_doc: BTreeMap<String, BTreeMap<String, BTreeMap<String, Vec<i64>>>>,
}

impl FigureNumbers {
    /// Number of one enumerable node, if assigned.
    pub fn get(&self, docname: &str, figtype: &str, id: &str) -> Option<&[i64]> {
        self.by_doc.get(docname)?.get(figtype)?.get(id).map(Vec::as_slice)
    }
}

/// Prev/next/parent navigation for one document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NavRelation {
    /// Parent in the master toctree (`None` for the root).
    pub parent: Option<String>,
    /// Previous document in pre-order traversal.
    pub prev: Option<String>,
    /// Next document in pre-order traversal.
    pub next: Option<String>,
}

/// A node in a resolved global toctree subtree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TocTreeNode {
    /// Internal docname (or URL for external entries).
    pub docname: String,
    /// Explicit title, when supplied.
    pub title: Option<String>,
    /// Nested entries.
    pub children: Vec<TocTreeNode>,
}

/// One local section entry (the result of upstream `document_toc()`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionTocEntry {
    /// Section title text.
    pub title: String,
    /// Anchor id (empty for the document's first section).
    pub anchor: String,
    /// Global section number components.
    pub number: Vec<i64>,
    /// Nested sections.
    pub children: Vec<SectionTocEntry>,
}

/// The complete result of a toctree collection pass.
#[derive(Clone, Debug, Default)]
pub struct ToctreeCollection {
    /// Every parsed toctree (in document order, then node order).
    pub toctrees: Vec<ParsedToctree>,
    /// Cycle-free hierarchy rooted at `master_doc`.
    pub hierarchy: ToctreeHierarchy,
    /// Global section numbers.
    pub section_numbers: SectionNumbers,
    /// Global figure numbers.
    pub figure_numbers: FigureNumbers,
    /// Navigation relations keyed by docname.
    pub relations: BTreeMap<String, NavRelation>,
    /// Recoverable diagnostics.
    pub warnings: Vec<ToctreeWarning>,
}

impl ToctreeCollection {
    /// Whether no warnings were produced.
    pub fn is_clean(&self) -> bool {
        self.warnings.is_empty()
    }

    /// Render diagnostics as miette reports.
    pub fn diagnostics(&self) -> Vec<miette::Report> {
        self.warnings.iter().cloned().map(miette::Report::new).collect()
    }

    /// Navigation relation of one document.
    pub fn relation(&self, docname: &str) -> Option<&NavRelation> {
        self.relations.get(docname)
    }

    /// Resolve a global ToC subtree rooted at `docname` (upstream
    /// `global_toctree_for_doc()` equivalent for writers).
    ///
    /// A toctree with `:hidden:` is skipped unless `includehidden` is true,
    /// mirroring upstream `_resolve_toctree()`. Hidden toctrees *do* still
    /// contribute hierarchy edges and prev/next relations, as they do upstream.
    pub fn global_toctree_for_doc(&self, docname: &str, includehidden: bool) -> Option<TocTreeNode> {
        let mut seen = HashSet::new();
        self.global_subtree(docname, includehidden, &mut seen)
    }

    fn global_subtree(
        &self,
        docname: &str,
        includehidden: bool,
        seen: &mut HashSet<String>,
    ) -> Option<TocTreeNode> {
        if !seen.insert(docname.to_owned()) {
            return None;
        }
        let mut children = Vec::new();
        for tree in self.toctrees.iter().filter(|tree| tree.docname == docname) {
            if tree.options.hidden && !includehidden && !tree.options.includehidden {
                continue;
            }
            for entry in &tree.entries {
                let Some(child) = entry.docname() else {
                    // External URLs and `self` are leaf entries.
                    children.push(TocTreeNode {
                        docname: entry.target.uri().unwrap_or_default().to_owned(),
                        title: entry.title.clone(),
                        children: Vec::new(),
                    });
                    continue;
                };
                let nested = self.global_subtree(child, includehidden, seen);
                children.push(TocTreeNode {
                    docname: child.to_owned(),
                    title: entry.title.clone(),
                    children: nested.map(|node| node.children).unwrap_or_default(),
                });
            }
        }
        seen.remove(docname);
        Some(TocTreeNode {
            docname: docname.to_owned(),
            title: None,
            children,
        })
    }
}

// ---------------------------------------------------------------------------
// Hierarchy
// ---------------------------------------------------------------------------

/// The master-document toctree hierarchy.
#[derive(Clone, Debug, Default)]
pub struct ToctreeHierarchy {
    /// `root_doc` / `master_doc` used as the traversal root.
    pub root: String,
    /// Ordered, cycle-free `parent → children` edges.
    pub edges: BTreeMap<String, Vec<String>>,
    /// Parent of each visited document (`None` for the root).
    pub parents: BTreeMap<String, Option<String>>,
    /// Documents in upstream pre-order traversal order.
    pub order: Vec<String>,
}

impl ToctreeHierarchy {
    /// Build a hierarchy from ordered edges.
    pub fn new(root: impl Into<String>, edges: BTreeMap<String, Vec<String>>) -> Self {
        let root = root.into();
        let mut hierarchy = Self {
            root: root.clone(),
            edges,
            parents: BTreeMap::new(),
            order: Vec::new(),
        };
        hierarchy.traverse();
        hierarchy
    }

    fn traverse(&mut self) {
        // Iterative equivalent of upstream `_traverse_toctree()`: pre-order,
        // each document yielded once by the first parent that reaches it.
        #[derive(Clone)]
        struct Frame {
            parent: Option<String>,
            docname: String,
            next_child: usize,
        }
        let mut stack = vec![Frame {
            parent: None,
            docname: self.root.clone(),
            next_child: 0,
        }];
        let mut traversed: HashSet<String> = HashSet::new();
        while let Some(mut frame) = stack.pop() {
            if frame.parent.as_deref() == Some(frame.docname.as_str()) {
                // Self reference: upstream warns and ignores the subtree.
                continue;
            }
            if frame.next_child == 0 {
                if !traversed.insert(frame.docname.clone()) {
                    continue;
                }
                self.parents.insert(frame.docname.clone(), frame.parent.clone());
                self.order.push(frame.docname.clone());
            }
            let children = self.edges.get(&frame.docname).cloned().unwrap_or_default();
            if frame.next_child < children.len() {
                let child = children[frame.next_child].clone();
                frame.next_child += 1;
                stack.push(frame);
                if !traversed.contains(&child) {
                    stack.push(Frame {
                        parent: Some(stack.last().map(|f| f.docname.clone()).unwrap_or_default()),
                        docname: child,
                        next_child: 0,
                    });
                }
            }
        }
    }

    /// Direct children of a document.
    pub fn children(&self, docname: &str) -> &[String] {
        self.edges.get(docname).map(Vec::as_slice).unwrap_or_default()
    }

    /// Parent of a document, if visited.
    pub fn parent(&self, docname: &str) -> Option<&str> {
        self.parents.get(docname).and_then(Option::as_deref)
    }

    /// Whether a document appears in the master traversal.
    pub fn contains(&self, docname: &str) -> bool {
        self.parents.contains_key(docname)
    }

    /// Subtree rooted at `docname` (upstream `global_toctree_for_doc` equivalent).
    pub fn global_toctree_for_doc(&self, docname: &str) -> Option<TocTreeNode> {
        if !self.contains(docname) && docname != self.root {
            return None;
        }
        Some(self.subtree(docname, &mut HashSet::new()))
    }

    fn subtree(&self, docname: &str, seen: &mut HashSet<String>) -> TocTreeNode {
        let mut children = Vec::new();
        if seen.insert(docname.to_owned()) {
            for child in self.children(docname) {
                if !seen.contains(child) {
                    children.push(self.subtree(child, seen));
                }
            }
            seen.remove(docname);
        }
        TocTreeNode {
            docname: docname.to_owned(),
            title: None,
            children,
        }
    }

    /// Documents discovered but not reachable from the root.
    pub fn unreachable<'a, I>(&self, found: I) -> Vec<String>
    where
        I: IntoIterator<Item = &'a str>,
    {
        found
            .into_iter()
            .filter(|name| !self.contains(name) && *name != self.root)
            .map(str::to_owned)
            .collect()
    }

    /// Local section table of contents for a document.
    pub fn document_toc(&self, env: &BuildEnvironment, docname: &str) -> Vec<SectionTocEntry> {
        let Some(document) = env.get_doctree(docname) else {
            return Vec::new();
        };
        let items = toc_tree(document);
        // Upstream `document_toc()` drops the document's own title entry and
        // returns the nested section list (respecting `:tocdepth:`).
        let sections = match items.first() {
            Some(TocItem::Section { children, .. }) => children.clone(),
            _ => items,
        };
        sections.into_iter().filter_map(section_entry).collect()
    }
}

fn section_entry(item: TocItem) -> Option<SectionTocEntry> {
    match item {
        TocItem::Section {
            title,
            anchor,
            children,
            ..
        } => Some(SectionTocEntry {
            title,
            anchor,
            number: Vec::new(),
            children: children.into_iter().filter_map(section_entry).collect(),
        }),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Collector
// ---------------------------------------------------------------------------

/// Toctree collector. `collect()` is idempotent: it recomputes all derived
/// state from the doctrees currently held by the environment.
#[derive(Clone, Debug, Default)]
pub struct ToctreeCollector;

impl ToctreeCollector {
    /// Create a collector.
    pub fn new() -> Self {
        Self
    }

    /// Collect toctrees, populate the environment seams and derive the
    /// hierarchy / numbering / navigation state.
    pub fn collect(&self, env: &mut BuildEnvironment, config: &scribe_config::Config) -> ToctreeCollection {
        collect_toctrees(env, config)
    }

    /// Like [`Self::collect`] but attaches source snippets to diagnostics.
    pub fn collect_with_sources(
        &self,
        env: &mut BuildEnvironment,
        config: &scribe_config::Config,
        sources: &BTreeMap<String, Arc<String>>,
    ) -> ToctreeCollection {
        collect_toctrees_with_sources(env, config, sources)
    }
}

/// Convenience free function equivalent to [`ToctreeCollector::collect`].
/// Collect toctrees without source text for diagnostic snippets.
pub fn collect_toctrees(
    env: &mut BuildEnvironment,
    config: &scribe_config::Config,
) -> ToctreeCollection {
    collect_toctrees_with_sources(env, config, &BTreeMap::new())
}

/// Collect toctrees with the original source text attached to diagnostics.
///
/// `sources` maps docname → source text; spans recorded by the RST parser are
/// byte offsets into that text, so miette can render labeled source snippets.
pub fn collect_toctrees_with_sources(
    env: &mut BuildEnvironment,
    config: &scribe_config::Config,
    sources: &BTreeMap<String, Arc<String>>,
) -> ToctreeCollection {
    let root = config.master_doc().to_owned();
    let found: BTreeSet<String> = env.found_docs.keys().cloned().collect();
    let spec = env.glob_to_spec();
    let suffixes: Vec<String> = config
        .get_dict("source_suffix")
        .map(|dict| dict.keys().cloned().collect())
        .unwrap_or_default();
    let docs: BTreeMap<String, Document> = env
        .documents
        .iter()
        .filter_map(|(name, snapshot)| snapshot.doctree.as_ref().map(|tree| (name.clone(), tree.clone())))
        .collect();

    let mut warnings = Vec::new();
    let mut toctrees = Vec::new();
    let mut writebacks: Vec<(String, NodeId, Vec<String>)> = Vec::new();

    for (docname, document) in &docs {
        for id in document.preorder_ids() {
            let Ok(node) = document.get(id) else {
                continue;
            };
            if node.kind != NodeKind::Toctree {
                continue;
            }
            let parsed = decode_toctree(id, node, docname);
            let resolved = resolve_entries(parsed, &found, &spec, &suffixes, &mut warnings);
            writebacks.push((docname.clone(), id, resolved.includefiles.clone()));
            toctrees.push(resolved);
        }
    }

    // ----- populate the frozen environment seams --------------------------
    let mut edges: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut toctree_targets: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for tree in &toctrees {
        edges.entry(tree.docname.clone()).or_default().extend(tree.includefiles.iter().cloned());
        toctree_targets
            .entry(tree.docname.clone())
            .or_default()
            .extend(tree.includefiles.iter().cloned());
        if tree.options.glob {
            env.note_glob_toctree(&tree.docname);
        }
        if tree.options.numbered != 0 {
            env.note_numbered_toctree(&tree.docname);
        }
        for child in &tree.includefiles {
            env.note_toctree_edge(&tree.docname, child);
            env.note_file_to_rebuild(child, &tree.docname);
        }
    }
    // The frozen seam methods sort child lists; upstream preserves insertion
    // order, which navigation and numbering depend on. Install the ordered
    // list through the public field after recording the edges.
    for (docname, children) in &edges {
        env.set_toctree_includes(docname, children);
        env.toctree_includes.insert(docname.clone(), children.clone());
    }

    // ----- cycle detection / repair ---------------------------------------
    detect_and_break_cycles(&mut edges, &toctrees, &root, &mut warnings);

    // ----- hierarchy & navigation -----------------------------------------
    let hierarchy = ToctreeHierarchy::new(root.clone(), edges);
    let mut relations = BTreeMap::new();
    for (index, docname) in hierarchy.order.iter().enumerate() {
        relations.insert(
            docname.clone(),
            NavRelation {
                parent: hierarchy.parents.get(docname).cloned().flatten(),
                prev: index.checked_sub(1).map(|previous| hierarchy.order[previous].clone()),
                next: hierarchy.order.get(index + 1).cloned(),
            },
        );
    }
    for docname in hierarchy.unreachable(found.iter().map(String::as_str)) {
        if is_orphan(&docs, &docname) {
            continue;
        }
        warnings.push(ToctreeWarning::new(
            "scribe::toc::not_included",
            format!("document isn't included in any toctree: {docname}"),
            "add the document to a toctree or mark it `:orphan:`",
            None,
            &docname,
        ));
    }

    // ----- numbering -------------------------------------------------------
    let section_numbers = assign_section_numbers(&docs, &env.numbered_toctrees, config, &mut warnings);
    let figure_numbers = assign_figure_numbers(&docs, &hierarchy, &section_numbers, config);
    let number_updates = collect_number_updates(&docs, &section_numbers, &figure_numbers);

    // ----- write resolved state back into the stored doctrees -------------
    for (docname, id, includefiles) in &writebacks {
        if let Some(snapshot) = env.documents.get_mut(docname) {
            if let Some(document) = snapshot.doctree.as_mut() {
                if let Ok(node) = document.get_mut(*id) {
                    node.set_list(ATTR_ENTRIES, includefiles.clone());
                    node.set_list(ATTR_INCLUDEFIES, includefiles.clone());
                }
            }
            if let Some(targets) = toctree_targets.get(docname) {
                snapshot.toctree_targets = targets.iter().cloned().collect();
            }
        }
    }
    for (docname, id, key, value) in number_updates {
        if let Some(document) = env
            .documents
            .get_mut(&docname)
            .and_then(|snapshot| snapshot.doctree.as_mut())
        {
            if let Ok(node) = document.get_mut(id) {
                node.attrs.set_str(key, value);
            }
        }
    }

    for warning in &mut warnings {
        if let Some(source) = sources.get(&warning.source_name) {
            warning.source_text = Arc::clone(source);
        }
    }

    ToctreeCollection {
        toctrees,
        hierarchy,
        section_numbers,
        figure_numbers,
        relations,
        warnings,
    }
}

// ---------------------------------------------------------------------------
// Parsing / resolution helpers
// ---------------------------------------------------------------------------

fn list_attr(node: &Node, key: &str) -> Vec<String> {
    match node.attrs.get(key) {
        Some(AttributeValue::List(values)) => values.clone(),
        Some(AttributeValue::Str(raw)) => serde_json::from_str::<Vec<String>>(raw).unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn str_attr(node: &Node, key: &str) -> Option<String> {
    match node.attrs.get(key) {
        Some(AttributeValue::Str(value)) => Some(value.clone()),
        _ => None,
    }
}

fn int_attr(node: &Node, key: &str, default: i64) -> i64 {
    match node.attrs.get(key) {
        Some(AttributeValue::Int(value)) => *value,
        Some(AttributeValue::Str(value)) => value.parse().unwrap_or(default),
        _ => default,
    }
}

fn bool_attr(node: &Node, key: &str) -> bool {
    matches!(node.attrs.get(key), Some(AttributeValue::Bool(true)))
}

fn decode_toctree(id: NodeId, node: &Node, owner: &str) -> ParsedToctree {
    let refs = list_attr(node, ATTR_ENTRY_REFS);
    let titles = list_attr(node, ATTR_ENTRY_TITLES);
    let kinds = list_attr(node, ATTR_ENTRY_KINDS);
    let mut entries = Vec::new();
    if refs.is_empty() {
        // Manually constructed doctrees may carry only `entries`.
        for docname in list_attr(node, ATTR_ENTRIES).into_iter().chain(list_attr(node, ATTR_INCLUDEFIES)) {
            entries.push(ToctreeEntry {
                title: None,
                target: EntryTarget::Doc(docname.clone()),
                raw_ref: docname,
            });
        }
    } else {
        for (index, raw_ref) in refs.iter().enumerate() {
            let title = titles.get(index).filter(|value| !value.is_empty()).cloned();
            let kind = kinds.get(index).map(String::as_str).unwrap_or(KIND_DOC);
            let target = match kind {
                KIND_EXTERNAL => EntryTarget::External(raw_ref.clone()),
                KIND_SELF => EntryTarget::SelfRef,
                _ => EntryTarget::Doc(raw_ref.clone()),
            };
            entries.push(ToctreeEntry {
                title,
                target,
                raw_ref: raw_ref.clone(),
            });
        }
    }
    let options = ToctreeOptions {
        maxdepth: int_attr(node, "maxdepth", -1),
        caption: str_attr(node, "caption"),
        glob: bool_attr(node, "glob"),
        hidden: bool_attr(node, "hidden"),
        includehidden: bool_attr(node, "includehidden"),
        numbered: int_attr(node, "numbered", 0),
        titlesonly: bool_attr(node, "titlesonly"),
        reversed: bool_attr(node, "reversed"),
        name: str_attr(node, "name"),
    };
    let (line, span) = match (&node.source, node.span) {
        (Some(source), span) => (source.line, span),
        (None, span) => (None, span),
    };
    ParsedToctree {
        docname: str_attr(node, "parent").unwrap_or_else(|| owner.to_owned()),
        options,
        entries,
        includefiles: Vec::new(),
        span,
        line,
        node_id: id,
    }
}

/// Upstream `docname_join` port used for manually constructed entries.
fn docname_join(base: &str, name: &str) -> String {
    let mut stack: Vec<&str> = Vec::new();
    let base = base.trim_end_matches('/');
    if let Some((parent, _)) = base.rsplit_once('/') {
        stack.extend(parent.split('/').filter(|part| !part.is_empty() && *part != "."));
    }
    for part in name.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            other => stack.push(other),
        }
    }
    stack.join("/")
}

fn resolve_entries(
    mut tree: ParsedToctree,
    found: &BTreeSet<String>,
    spec: &GlobSpec,
    suffixes: &[String],
    warnings: &mut Vec<ToctreeWarning>,
) -> ParsedToctree {
    let mut includefiles = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut entries = Vec::new();
    for entry in tree.entries.into_iter() {
        match entry.target {
            EntryTarget::External(_) | EntryTarget::SelfRef => {
                entries.push(entry);
                continue;
            }
            EntryTarget::Doc(raw_docname) => {
                if tree.options.glob && is_glob(&raw_docname) {
                    let pattern = docname_join(&tree.docname, &raw_docname);
                    let regex = regex::Regex::new(&translate_pattern(&pattern)).ok();
                    let mut matches: Vec<String> = found
                        .iter()
                        .filter(|name| *name != &tree.docname)
                        .filter(|name| regex.as_ref().is_some_and(|re| re.is_match(name)))
                        .filter(|name| !spec.is_excluded(name))
                        .cloned()
                        .collect();
                    matches.sort();
                    matches.dedup();
                    if matches.is_empty() {
                        warnings.push(ToctreeWarning::new(
                            "scribe::toc::empty_glob",
                            format!("toctree glob pattern '{pattern}' didn't match any documents"),
                            "check the pattern or add matching source documents",
                            tree.span,
                            &tree.docname,
                        ));
                    }
                    for docname in matches {
                        if includefiles.contains(&docname) {
                            continue;
                        }
                        includefiles.push(docname.clone());
                        entries.push(ToctreeEntry {
                            title: None,
                            target: EntryTarget::Doc(docname),
                            raw_ref: pattern.clone(),
                        });
                    }
                    continue;
                }
                let mut docname = raw_docname.clone();
                for suffix in suffixes {
                    if docname.ends_with(suffix) {
                        docname.truncate(docname.len() - suffix.len());
                        break;
                    }
                }
                docname = docname_join(&tree.docname, &docname);
                if !found.contains(&docname) {
                    warnings.push(ToctreeWarning::new(
                        "scribe::toc::missing_document",
                        format!("toctree contains reference to non-existing document '{docname}'"),
                        "create the document or remove the entry from the toctree",
                        tree.span,
                        &tree.docname,
                    ));
                    continue;
                }
                if !seen.insert(docname.clone()) {
                    warnings.push(ToctreeWarning::new(
                        "scribe::toc::duplicate_entry",
                        format!("duplicated entry found in toctree: {docname}"),
                        "each document should appear only once in a toctree",
                        tree.span,
                        &tree.docname,
                    ));
                }
                includefiles.push(docname.clone());
                entries.push(ToctreeEntry {
                    title: entry.title,
                    target: EntryTarget::Doc(docname),
                    raw_ref: entry.raw_ref,
                });
            }
        }
    }
    tree.entries = entries;
    tree.includefiles = includefiles;
    if tree.options.reversed {
        tree.entries.reverse();
        tree.includefiles.reverse();
    }
    tree
}

fn is_glob(value: &str) -> bool {
    value.contains(['*', '?', '['])
}

fn is_orphan(docs: &BTreeMap<String, Document>, docname: &str) -> bool {
    let Some(document) = docs.get(docname) else {
        return false;
    };
    document.preorder_ids().into_iter().any(|id| {
        document.get(id).is_ok_and(|node| {
            node.kind == NodeKind::Field
                && node
                    .children
                    .iter()
                    .any(|child| document.get(*child).is_ok_and(|child| child.astext(document).trim() == "orphan"))
        })
    })
}

// ---------------------------------------------------------------------------
// Cycle detection
// ---------------------------------------------------------------------------

fn detect_and_break_cycles(
    edges: &mut BTreeMap<String, Vec<String>>,
    toctrees: &[ParsedToctree],
    root: &str,
    warnings: &mut Vec<ToctreeWarning>,
) {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Color {
        White,
        Gray,
        Black,
    }

    let span_for = |parent: &str, child: &str| -> Option<Span> {
        toctrees
            .iter()
            .find(|tree| tree.docname == parent && tree.includefiles.iter().any(|name| name == child))
            .and_then(|tree| tree.span)
    };

    for _ in 0..=edges.len() {
        let mut colors: BTreeMap<String, Color> = BTreeMap::new();
        let mut stack: Vec<(String, usize)> = Vec::new();
        let mut cycle: Option<(Vec<String>, String, String)> = None;
        let mut nodes: Vec<String> = edges.keys().cloned().collect();
        // Start at the master document so the edge removed for a cycle is the
        // back edge toward an ancestor, matching upstream `_traverse_toctree`.
        if let Some(position) = nodes.iter().position(|name| name == root) {
            let first = nodes.remove(position);
            nodes.insert(0, first);
        }

        // Iterative DFS so adversarial cyclic graphs cannot overflow the stack.
        for start in &nodes {
            if colors.get(start).copied().unwrap_or(Color::White) != Color::White {
                continue;
            }
            colors.insert(start.clone(), Color::Gray);
            stack.push((start.clone(), 0));
            while let Some((current, index)) = stack.pop() {
                let children = edges.get(&current).cloned().unwrap_or_default();
                if index < children.len() {
                    let child = children[index].clone();
                    stack.push((current.clone(), index + 1));
                    match colors.get(&child).copied().unwrap_or(Color::White) {
                        Color::White => {
                            colors.insert(child.clone(), Color::Gray);
                            stack.push((child, 0));
                        }
                        Color::Gray => {
                            let position = stack.iter().position(|(name, _)| name == &child);
                            let mut path: Vec<String> = stack
                                .iter()
                                .skip(position.unwrap_or(0))
                                .map(|(name, _)| name.clone())
                                .collect();
                            path.push(child.clone());
                            cycle = Some((path, current.clone(), child.clone()));
                            break;
                        }
                        Color::Black => {}
                    }
                } else {
                    colors.insert(current, Color::Black);
                }
            }
            if cycle.is_some() {
                break;
            }
        }

        let Some((path, parent, child)) = cycle else {
            break;
        };
        warnings.push(ToctreeWarning::new(
            "scribe::toc::circular",
            format!("circular toctree reference detected: {}", path.join(" -> ")),
            "the edge closing the cycle was ignored so the hierarchy stays finite",
            span_for(&parent, &child),
            &parent,
        ));
        if let Some(children) = edges.get_mut(&parent) {
            children.retain(|name| name != &child);
        }
    }
}

// ---------------------------------------------------------------------------
// Local ToC tree + numbering
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum TocItem {
    Section {
        title: String,
        anchor: String,
        children: Vec<TocItem>,
    },
    Toctree(ParsedToctree),
    Only(Vec<TocItem>),
}

/// Build the same local ToC tree the upstream collector builds:
/// top-level sections, with toctrees and `only` wrappers nested where they
/// appear in the doctree.
fn toc_tree(document: &Document) -> Vec<TocItem> {
    let mut counter = 0usize;
    let mut items = Vec::new();
    let root = document.root;
    for id in document.children(root).unwrap_or_default().to_vec() {
        collect_toc_children(document, id, &mut counter, &mut items);
    }
    items
}

fn collect_toc_children(
    document: &Document,
    id: NodeId,
    counter: &mut usize,
    output: &mut Vec<TocItem>,
) {
    let Ok(node) = document.get(id) else {
        return;
    };
    match node.kind {
        NodeKind::Section => {
            let title = node
                .children
                .iter()
                .find(|child| document.get(**child).is_ok_and(|child| child.kind == NodeKind::Title))
                .map(|child| document.get(*child).map(|title| title.astext(document)).unwrap_or_default())
                .unwrap_or_default();
            let id_value = node.ids().ok().and_then(|ids| ids.first().cloned()).unwrap_or_default();
            let anchor = if *counter == 0 {
                String::new()
            } else {
                format!("#{id_value}")
            };
            *counter += 1;
            let mut children = Vec::new();
            for child in node.children.clone() {
                if document.get(child).is_ok_and(|child| child.kind == NodeKind::Title) {
                    continue;
                }
                collect_toc_children(document, child, counter, &mut children);
            }
            output.push(TocItem::Section {
                title,
                anchor,
                children,
            });
        }
        NodeKind::Toctree => {
            // A toctree directly under the document root is emitted as-is.
            output.push(TocItem::Toctree(ParsedToctree {
                docname: node
                    .attrs
                    .get("parent")
                    .and_then(|value| match value {
                        AttributeValue::Str(name) => Some(name.clone()),
                        _ => None,
                    })
                    .unwrap_or_default(),
                options: ToctreeOptions {
                    maxdepth: int_attr(node, "maxdepth", -1),
                    numbered: int_attr(node, "numbered", 0),
                    ..ToctreeOptions::default()
                },
                entries: list_attr(node, ATTR_ENTRIES)
                    .into_iter()
                    .map(|docname| ToctreeEntry {
                        title: None,
                        target: EntryTarget::Doc(docname),
                        raw_ref: String::new(),
                    })
                    .collect(),
                includefiles: Vec::new(),
                span: node.span,
                line: node.source.as_ref().and_then(|source| source.line),
                node_id: NodeId::NULL,
            }));
        }
        NodeKind::Only => {
            let mut children = Vec::new();
            for child in node.children.clone() {
                collect_toc_children(document, child, counter, &mut children);
            }
            output.push(TocItem::Only(children));
        }
        _ => {
            // Upstream scans non-section elements for nested toctrees.
            for child in node.children.clone() {
                collect_toc_children(document, child, counter, output);
            }
        }
    }
}

fn assign_section_numbers(
    docs: &BTreeMap<String, Document>,
    numbered: &BTreeSet<String>,
    config: &scribe_config::Config,
    warnings: &mut Vec<ToctreeWarning>,
) -> SectionNumbers {
    let mut result = SectionNumbers::default();
    let mut assigned: BTreeSet<String> = BTreeSet::new();
    let max_stack = docs.len().max(1) + hierarchy_depth_allowance(docs) + 8;

    for docname in numbered {
        assigned.insert(docname.clone());
        let Some(document) = docs.get(docname) else {
            continue;
        };
        for id in document.preorder_ids() {
            let Ok(node) = document.get(id) else {
                continue;
            };
            if node.kind != NodeKind::Toctree {
                continue;
            }
            let depth = int_attr(node, "numbered", 0);
            if depth == 0 {
                continue;
            }
            let tree = ParsedToctree {
                docname: docname.clone(),
                options: ToctreeOptions::default(),
                entries: list_attr(node, ATTR_ENTRIES)
                    .into_iter()
                    .map(|name| ToctreeEntry {
                        title: None,
                        target: EntryTarget::Doc(name),
                        raw_ref: String::new(),
                    })
                    .collect(),
                includefiles: Vec::new(),
                span: node.span,
                line: node.source.as_ref().and_then(|source| source.line),
                node_id: id,
            };
            let mut stack = vec![0i64; max_stack];
            let mut level = 0usize;
            walk_numbered_toctree(
                &tree,
                depth,
                docs,
                &mut assigned,
                &mut stack,
                &mut level,
                warnings,
                &mut result.by_doc,
            );
        }
    }
    let _ = config; // section numbering is independent of numfig settings
    result
}

fn hierarchy_depth_allowance(docs: &BTreeMap<String, Document>) -> usize {
    docs.values()
        .map(|document| {
            document
                .preorder_ids()
                .into_iter()
                .filter(|&id| document.get(id).is_ok_and(|node| node.kind == NodeKind::Section))
                .count()
        })
        .max()
        .unwrap_or(0)
}

#[allow(clippy::too_many_arguments)]
fn walk_numbered_toctree(
    tree: &ParsedToctree,
    depth: i64,
    docs: &BTreeMap<String, Document>,
    assigned: &mut BTreeSet<String>,
    stack: &mut [i64],
    level: &mut usize,
    warnings: &mut Vec<ToctreeWarning>,
    output: &mut BTreeMap<String, BTreeMap<String, Vec<i64>>>,
) {
    if depth == 0 {
        return;
    }
    for entry in &tree.entries {
        let Some(docname) = entry.docname() else {
            continue;
        };
        if assigned.contains(docname) {
            warnings.push(
                ToctreeWarning::new(
                    "scribe::toc::nested_numbered",
                    format!("{docname} is already assigned section numbers (nested numbered toctree?)"),
                    "remove the nested :numbered: toctree or accept the shared numbering",
                    tree.span,
                    &tree.docname,
                )
                .with_line(tree.line),
            );
            continue;
        }
        assigned.insert(docname.to_owned());
        let mut secnums = BTreeMap::new();
        let document = docs.get(docname).cloned().unwrap_or_else(|| Document::new(None));
        walk_toc_items(
            &toc_tree(&document),
            &mut secnums,
            depth,
            stack,
            level,
            docs,
            assigned,
            warnings,
            output,
        );
        output.insert(docname.to_owned(), secnums);
    }
}

#[allow(clippy::too_many_arguments)]
fn walk_toc_items(
    items: &[TocItem],
    secnums: &mut BTreeMap<String, Vec<i64>>,
    depth: i64,
    stack: &mut [i64],
    level: &mut usize,
    docs: &BTreeMap<String, Document>,
    assigned: &mut BTreeSet<String>,
    warnings: &mut Vec<ToctreeWarning>,
    output: &mut BTreeMap<String, BTreeMap<String, Vec<i64>>>,
) {
    for item in items {
        match item {
            TocItem::Section {
                anchor, children, ..
            } => {
                if *level >= stack.len() {
                    continue;
                }
                stack[*level] += 1;
                let number = if depth > 0 {
                    stack[..=*level].to_vec()
                } else {
                    Vec::new()
                };
                secnums.insert(anchor.clone(), number);
                *level += 1;
                if *level < stack.len() {
                    stack[*level] = 0;
                }
                walk_toc_items(
                    children, secnums, depth - 1, stack, level, docs, assigned, warnings, output,
                );
                if *level > 0 {
                    *level -= 1;
                }
            }
            TocItem::Toctree(tree) => {
                walk_numbered_toctree(tree, depth, docs, assigned, stack, level, warnings, output);
            }
            TocItem::Only(children) => {
                walk_toc_items(children, secnums, depth, stack, level, docs, assigned, warnings, output);
            }
        }
    }
}

fn assign_figure_numbers(
    docs: &BTreeMap<String, Document>,
    hierarchy: &ToctreeHierarchy,
    numbers: &SectionNumbers,
    config: &scribe_config::Config,
) -> FigureNumbers {
    let mut result = FigureNumbers::default();
    if !config.get_bool("numfig").unwrap_or(false) {
        return result;
    }
    let depth = config.get_int("numfig_secnum_depth").unwrap_or(1).max(0) as usize;
    let mut counters: BTreeMap<String, BTreeMap<Vec<i64>, i64>> = BTreeMap::new();
    let mut visited = HashSet::new();
    let mut stack = vec![(hierarchy.root.clone(), Vec::<i64>::new())];
    while let Some((docname, inherited)) = stack.pop() {
        if !visited.insert(docname.clone()) {
            continue;
        }
        let Some(document) = docs.get(&docname) else {
            continue;
        };
        let mut section_prefix = inherited;
        for id in document.preorder_ids() {
            let Ok(node) = document.get(id) else {
                continue;
            };
            match node.kind {
                NodeKind::Section => {
                    if let Ok(ids) = node.ids() {
                        if let Some(id_value) = ids.first() {
                            if let Some(number) = numbers.get(&docname, id_value) {
                                if !number.is_empty() {
                                    section_prefix = number.iter().take(depth).copied().collect();
                                }
                            }
                        }
                    }
                }
                NodeKind::Figure | NodeKind::Table | NodeKind::Container => {
                    let figtype = match node.kind {
                        NodeKind::Figure => "figure",
                        NodeKind::Table => "table",
                        _ => {
                            let has_literal = node.children.iter().any(|child| {
                                document.get(*child).is_ok_and(|child| child.kind == NodeKind::LiteralBlock)
                            });
                            if !has_literal {
                                continue;
                            }
                            "code-block"
                        }
                    };
                    let Ok(ids) = node.ids() else {
                        continue;
                    };
                    let Some(id_value) = ids.first() else {
                        continue;
                    };
                    let counter = counters.entry(figtype.to_owned()).or_default();
                    let next = counter.get(&section_prefix).copied().unwrap_or(0) + 1;
                    counter.insert(section_prefix.clone(), next);
                    let mut number = section_prefix.clone();
                    number.push(next);
                    result
                        .by_doc
                        .entry(docname.clone())
                        .or_default()
                        .entry(figtype.to_owned())
                        .or_default()
                        .insert(id_value.clone(), number);
                }
                _ => {}
            }
        }
        for child in hierarchy.children(&docname).iter().rev() {
            stack.push((child.clone(), section_prefix.clone()));
        }
    }
    result
}

fn lookup_secnumber<'a>(numbers: &'a SectionNumbers, docname: &str, id: &str) -> Option<&'a [i64]> {
    numbers
        .get(docname, &format!("#{id}"))
        .or_else(|| numbers.get(docname, id))
}

fn collect_number_updates(
    docs: &BTreeMap<String, Document>,
    numbers: &SectionNumbers,
    figures: &FigureNumbers,
) -> Vec<(String, NodeId, String, String)> {
    let mut updates = Vec::new();
    for (docname, document) in docs {
        for id in document.preorder_ids() {
            let Ok(node) = document.get(id) else {
                continue;
            };
            match node.kind {
                NodeKind::Section => {
                    if let Ok(ids) = node.ids() {
                        for id_value in ids {
                            if let Some(number) = numbers.get(docname, &id_value) {
                                if !number.is_empty() {
                                    let display = number.iter().map(i64::to_string).collect::<Vec<_>>().join(".");
                                    updates.push((docname.clone(), id, "secnumber".to_owned(), display.clone()));
                                    for child in &node.children {
                                        if document.get(*child).is_ok_and(|child| child.kind == NodeKind::Title) {
                                            updates.push((docname.clone(), *child, "secnumber".to_owned(), display.clone()));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                NodeKind::Pending => {
                    // `sectnum` placeholders get the number of the section
                    // they appear in, so Phase 10/writers can render it.
                    if !matches!(
                        node.attrs.get("__directive"),
                        Some(AttributeValue::Str(marker)) if marker == "sectnum"
                    ) {
                        continue;
                    }
                    let Some(section_id) = document.ancestors(id).find(|&ancestor| {
                        document.get(ancestor).is_ok_and(|node| node.kind == NodeKind::Section)
                    }) else {
                        continue;
                    };
                    let Ok(ids) = document.get(section_id).and_then(|section| section.ids()) else {
                        continue;
                    };
                    let Some(id_value) = ids.first() else {
                        continue;
                    };
                    if let Some(number) = lookup_secnumber(numbers, docname, id_value) {
                        if !number.is_empty() {
                            let display = number.iter().map(i64::to_string).collect::<Vec<_>>().join(".");
                            updates.push((docname.clone(), id, "secnumber".to_owned(), display));
                        }
                    }
                }
                NodeKind::Figure | NodeKind::Table | NodeKind::Container => {
                    let figtype = match node.kind {
                        NodeKind::Figure => "figure",
                        NodeKind::Table => "table",
                        _ => "code-block",
                    };
                    if let Ok(ids) = node.ids() {
                        for id_value in ids {
                            if let Some(number) = figures.get(docname, figtype, &id_value) {
                                updates.push((
                                    docname.clone(),
                                    id,
                                    "fignumber".to_owned(),
                                    number.iter().map(i64::to_string).collect::<Vec<_>>().join("."),
                                ));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    updates
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_target_helpers() {
        assert_eq!(EntryTarget::Doc("foo".into()).docname(), Some("foo"));
        assert_eq!(EntryTarget::External("http://x".into()).uri(), Some("http://x"));
        assert_eq!(EntryTarget::SelfRef.uri(), Some(""));
    }

    #[test]
    fn hierarchy_preorder_and_relations_are_cycle_safe() {
        let mut edges = BTreeMap::new();
        edges.insert("index".to_owned(), vec!["a".to_owned(), "b".to_owned()]);
        edges.insert("a".to_owned(), vec!["c".to_owned()]);
        edges.insert("b".to_owned(), vec!["c".to_owned()]);
        let hierarchy = ToctreeHierarchy::new("index", edges);
        assert_eq!(hierarchy.order, vec!["index", "a", "c", "b"]);
        assert_eq!(hierarchy.parent("c"), Some("a"));
        assert_eq!(
            hierarchy.global_toctree_for_doc("a").unwrap().children.iter().map(|n| n.docname.as_str()).collect::<Vec<_>>(),
            vec!["c"]
        );
    }
}
