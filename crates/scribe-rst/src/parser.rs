//! Block-level parser: the heart of Phase 04.
//!
//! The parser consumes the [`SourceLine`](crate::lexer::SourceLine) vector
//! produced by [`Lexer`](crate::lexer::Lexer) and builds a
//! [`Document`](scribe_ast::Document). It is a recursive-descent scanner over
//! indentation, *not* a port of Docutils' state machine, but every rule was
//! extracted from `docutils/parsers/rst/states.py` and
//! `docutils/parsers/rst/tableparser.py` (see `ARCHITECTURE.md` §8).
//!
//! Nested constructs are parsed by passing a **base indentation column**:
//! line content is read with [`BlockParser::content_at`], which slices past
//! that column. This keeps every node's span anchored to the original source
//! while still allowing list items, block quotes and table cells to be parsed
//! with the same dispatch loop as the document root.

use std::collections::HashMap;
use std::sync::Arc;

use miette::SourceSpan;
use scribe_ast::{Document, NodeId, NodeKind, SourcePos, Span};

use crate::error::{RstDiagnostic, RstError};
use crate::inline::{materialize_inline, parse_inline, InlineOptions, InlineSource, XrefMeta};
use crate::lexer::{adornment_of, column_width, Lexer, SourceLine};
use crate::names::{make_id, normalize_name, unique_id};
use crate::{InlineContext, ParseOptions};

/// Structured payload of a replacement directive inside a substitution
/// definition (`.. |name| image:: file.png`).
///
/// Additive Phase 04/05 side table consumed by Phase 06; it is not stored as a
/// pseudo-XML attribute so byte-exact differential tests remain unaffected.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SubstitutionDirective {
    /// Directive name (`replace`, `image`, `unicode`, `date`, ...).
    pub name: String,
    /// Text following the directive's `::` marker (after option lines).
    pub arguments: String,
    /// Option lines that followed the directive marker.
    pub options: std::collections::BTreeMap<String, String>,
}

/// Result of a successful parse, including recoverable diagnostics.
#[derive(Debug)]
pub struct ParseOutput {
    /// The best-effort doctree. Always present, even when `diagnostics` is non-empty.
    pub document: Document,
    /// Recoverable diagnostics (malformed indentation, tables, adornment, ...).
    pub diagnostics: Vec<RstDiagnostic>,
    /// Tightness of every emitted bullet/enumerated list (`true` = tight).
    tight_lists: HashMap<u32, bool>,
    /// Contract metadata for role-generated `pending_xref` nodes.
    inline_xrefs: Vec<(NodeId, XrefMeta)>,
    /// Replacement directive payloads for substitution definitions, keyed by
    /// the definition's `NodeId`. Additive side table consumed by Phase 06.
    substitution_directives: Vec<(NodeId, SubstitutionDirective)>,
}

impl ParseOutput {
    /// Returns the document, discarding diagnostics.
    pub fn into_document(self) -> Document {
        self.document
    }

    /// `true` when at least one recoverable diagnostic was produced.
    pub fn has_diagnostics(&self) -> bool {
        !self.diagnostics.is_empty()
    }

    /// Tightness of a bullet or enumerated list, when `id` is one.
    ///
    /// Docutils does not store tight/loose as a doctree attribute (writers
    /// infer it), so Phase 04 exposes it through this side table rather than
    /// adding a non-upstream pseudo-XML attribute.
    pub fn list_is_tight(&self, id: NodeId) -> Option<bool> {
        self.tight_lists.get(&id.0).copied()
    }

    /// All `(list node id, is_tight)` pairs recorded during the parse.
    pub fn tight_lists(&self) -> &HashMap<u32, bool> {
        &self.tight_lists
    }

    /// Contract metadata for every role-generated `pending_xref`, keyed by its
    /// arena id. Consumed by Phases 06/11 to resolve domain cross references.
    pub fn inline_xrefs(&self) -> &[(NodeId, XrefMeta)] {
        &self.inline_xrefs
    }

    /// Mutable view of the inline xref side table, used by the Phase 06
    /// resolve pass to drop entries whose xref node was removed.
    pub fn inline_xrefs_mut(&mut self) -> &mut Vec<(NodeId, XrefMeta)> {
        &mut self.inline_xrefs
    }

    /// Replacement-directive names for substitution definitions, keyed by the
    /// definition's `NodeId` (`replace`, `image`, `unicode`, `date`, ...).
    pub fn substitution_directives(&self) -> &[(NodeId, SubstitutionDirective)] {
        &self.substitution_directives
    }
}

/// A section adornment style. Underlined titles use just the underline
/// character; overlined titles concatenate overline and underline characters.
pub(crate) type AdornmentStyle = String;

/// The block parser state.
pub(crate) struct BlockParser<'a> {
    /// Original (possibly prolog/epilog-augmented) source text.
    pub(crate) source: &'a str,
    /// Scanned logical lines.
    pub(crate) lines: Vec<SourceLine<'a>>,
    /// Current line cursor.
    pub(crate) pos: usize,
    /// Doctree being built.
    pub(crate) doc: Document,
    /// Recoverable diagnostics.
    pub(crate) diagnostics: Vec<RstDiagnostic>,
    /// Parse-time options (source name, tab width, prolog/epilog).
    pub(crate) options: ParseOptions,
    /// Adornment styles in order of first appearance (level = index + 1).
    title_styles: Vec<AdornmentStyle>,
    /// Open sections: `(style, section id)` from outermost to innermost.
    section_stack: Vec<(AdornmentStyle, NodeId)>,
    /// Implicit-id allocation counters.
    used_ids: HashMap<String, usize>,
    /// List tightness side table.
    pub(crate) tight_lists: HashMap<u32, bool>,
    /// Role xref contract side table (forwarded to `ParseOutput`).
    pub(crate) inline_xrefs: Vec<(NodeId, XrefMeta)>,
    /// Custom roles declared by `.. role:: name(base)` in document order.
    /// Maps the local name to its base role; consumed by inline parsing.
    pub(crate) custom_roles: HashMap<String, String>,
    /// Substitution-directive side table (forwarded to `ParseOutput`).
    pub(crate) substitution_directives: Vec<(NodeId, SubstitutionDirective)>,
    /// Exclusive upper bound for the current parse region (`usize::MAX` at root).
    pub(crate) limit: usize,
    /// `(line index, virtual indent)` for a marker line whose content starts
    /// past the marker (list items, fields, options). Lets the generic block
    /// loop treat such a line as if it were indented to its content column.
    pub(crate) indent_override: Option<(usize, usize)>,
    /// The innermost section at document level; subsequent top-level blocks
    /// are attached here (Docutils updates `self.parent` when entering a
    /// section). Nested parses (base_indent > 0) ignore this field.
    section_parent: Option<NodeId>,
}

impl<'a> BlockParser<'a> {
    /// Section title styles seen so far, outermost first. Used to compute the
    /// section-raising count for `only` directives.
    pub(crate) fn title_styles(&self) -> &[AdornmentStyle] {
        &self.title_styles
    }

    /// Depth of the currently open section stack.
    pub(crate) fn section_depth(&self) -> usize {
        self.section_stack.len()
    }

    /// Creates a parser over an already-augmented source string.
    pub(crate) fn with_source(source: &'a str, options: ParseOptions) -> Self {
        let lexer = Lexer::with_tab_width(source, options.tab_width);
        let lines = lexer.scan();
        let doc = Document::new(options.source_name.clone());
        Self {
            source,
            lines,
            pos: 0,
            doc,
            diagnostics: Vec::new(),
            options,
            title_styles: Vec::new(),
            section_stack: Vec::new(),
            used_ids: HashMap::new(),
            tight_lists: HashMap::new(),
            inline_xrefs: Vec::new(),
            custom_roles: HashMap::new(),
            substitution_directives: Vec::new(),
            limit: usize::MAX,
            indent_override: None,
            section_parent: None,
        }
    }

    /// Runs the parse and returns the output (never panics).
    pub(crate) fn run(mut self) -> ParseOutput {
        let root = self.doc.root;
        if let Err(error) = self.parse_blocks(root, 0) {
            self.push_diagnostic(error);
        }
        // Docutils applies its `References` / `Substitutions` transforms as
        // part of publishing; resolve the links our inline parser recorded
        // against the block-level targets and substitution definitions now.
        if let Err(error) = self.resolve_references() {
            self.push_diagnostic(error);
        }
        ParseOutput {
            document: self.doc,
            diagnostics: self.diagnostics,
            tight_lists: self.tight_lists,
            inline_xrefs: self.inline_xrefs,
            substitution_directives: self.substitution_directives,
        }
    }

    // ----- reference / substitution resolution -----------------------------

    /// Resolves named and anonymous hyperlink references to block-level
    /// targets and expands plain substitution references. Both are standard
    /// Docutils transforms that operate on the parse result; doing them here
    /// gives the inline parser the same pseudoxml surface as the oracle.
    fn resolve_references(&mut self) -> Result<(), RstError> {
        use scribe_ast::Node;

        // 1. Index explicit targets.
        let mut named: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
        let mut anonymous: Vec<(Option<String>, Option<String>)> = Vec::new();
        for (_, node) in self.doc.iter() {
            if node.kind != NodeKind::Target {
                continue;
            }
            let refuri = node.get_str("refuri").ok().flatten().map(str::to_owned);
            let refid = node.get_str("refid").ok().flatten().map(str::to_owned);
            if node.attrs.get_bool("anonymous").ok().flatten().unwrap_or(false) {
                anonymous.push((refuri, refid));
            } else if let Ok(Some(names)) = node.get_list("names") {
                for name in names {
                    named.entry(name).or_insert_with(|| (refuri.clone(), refid.clone()));
                }
            }
        }

        // 2. Index substitution definitions (clone their children).
        let mut substitutions: HashMap<String, Vec<Node>> = HashMap::new();
        for (_, node) in self.doc.iter() {
            if node.kind != NodeKind::SubstitutionDefinition {
                continue;
            }
            if let Ok(Some(names)) = node.get_list("names") {
                let children = node
                    .children
                    .iter()
                    .filter_map(|&id| self.doc.get(id).ok().cloned())
                    .collect::<Vec<_>>();
                for name in names {
                    // Inline `|name|` references carry a lower-cased refname
                    // (Docutils `normalize_name`) while definitions preserve
                    // the original case (`whitespace_normalize_name`). Index
                    // both spellings so `|HTML_RAW|` finds `HTML_RAW`.
                    substitutions.insert(crate::names::normalize_name(&name), children.clone());
                    substitutions.insert(name.clone(), children.clone());
                }
            }
        }

        // 3. Apply reference resolution and collect substitution rewrites.
        let mut anon_cursor = 0usize;
        let mut expand: Vec<(NodeId, Vec<Node>)> = Vec::new();
        for id in self.doc.preorder_ids() {
            let Ok(node) = self.doc.get(id) else { continue };
            match node.kind {
                NodeKind::Reference => {
                    let is_anon = node.attrs.get_bool("anonymous").ok().flatten().unwrap_or(false);
                    let refname = node.get_str("refname").ok().flatten().map(str::to_owned);
                    let has_uri = node.get_str("refuri").ok().flatten().is_some();
                    if has_uri {
                        continue;
                    }
                    let resolved = if is_anon {
                        let item = anonymous.get(anon_cursor).cloned();
                        anon_cursor += 1;
                        item
                    } else {
                        refname.as_deref().and_then(|n| named.get(n).cloned())
                    };
                    if let Some((uri, rid)) = resolved {
                        let n = self.doc.get_mut(id)?;
                        if let Some(uri) = uri {
                            n.attrs.set_str("refuri", uri);
                        }
                        if let Some(rid) = rid {
                            n.attrs.set_str("refid", rid);
                        }
                        n.attrs.remove("refname");
                    }
                }
                NodeKind::SubstitutionReference => {
                    let name = node.get_str("refname").ok().flatten().map(str::to_owned);
                    if let Some(name) = name {
                        if let Some(children) = substitutions.get(&name) {
                            expand.push((id, children.clone()));
                        }
                    }
                }
                _ => {}
            }
        }

        // 4. Expand substitutions: drop the reference's own name text, append
        //    clones of the definition content, then unwrap the node in place
        //    (ids never move).
        for (id, children) in expand {
            self.doc.get_mut(id)?.children.clear();
            for child in children {
                self.doc.add_node(child, Some(id))?;
            }
            self.doc.replace_with_children(id)?;
        }
        Ok(())
    }

    // ----- line/cursor helpers ---------------------------------------------

    /// Returns the current line, if any.
    pub(crate) fn current(&self) -> Option<&SourceLine<'a>> {
        self.lines.get(self.pos)
    }

    /// Returns a line by index.
    pub(crate) fn line(&self, index: usize) -> Option<&SourceLine<'a>> {
        self.lines.get(index)
    }

    /// Skips blank lines from the cursor, without crossing the region limit.
    pub(crate) fn skip_blank(&mut self) {
        while self.pos < self.limit && self.lines.get(self.pos).is_some_and(|line| line.blank) {
            self.pos += 1;
        }
    }

    /// Content of line `index` after removing `indent` leading columns.
    pub(crate) fn content_at(&self, index: usize, indent: usize) -> &str {
        match self.lines.get(index) {
            Some(line) => {
                let cut = indent.min(line.text.len());
                &line.text[cut..]
            }
            None => "",
        }
    }

    /// Indentation column of line `index` (0 when out of range).
    ///
    /// Honours the virtual indent of a marker line whose content starts past
    /// the marker (see [`BlockParser::indent_override`]).
    pub(crate) fn indent_at(&self, index: usize) -> usize {
        if let Some((line_index, value)) = self.indent_override {
            if line_index == index {
                return value;
            }
        }
        self.lines.get(index).map_or(0, |line| line.indent_cols)
    }

    /// Span from the start of line `from` to the end of line `to` (exclusive index).
    pub(crate) fn span_of(&self, from: usize, to: usize) -> Span {
        let start = self
            .lines
            .get(from)
            .map_or(0, |line| line.start)
            .min(self.source.len() as u32);
        let end = self
            .lines
            .get(to.saturating_sub(1))
            .map_or(self.source.len() as u32, |line| line.end)
            .min(self.source.len() as u32);
        Span::new(start.min(end), end)
    }

    /// Byte span of line `index`.
    pub(crate) fn line_span(&self, index: usize) -> Span {
        self.lines
            .get(index)
            .map_or(Span::new(0, 0), |line| line.span())
    }

    /// 1-based source line number for line `index`.
    pub(crate) fn line_number(&self, index: usize) -> u32 {
        self.lines.get(index).map_or(1, |line| line.number as u32)
    }

    // ----- node helpers ----------------------------------------------------

    /// Adds an element node with source position and span, returning its id.
    pub(crate) fn add(
        &mut self,
        kind: NodeKind,
        parent: NodeId,
        span: Span,
        line: u32,
    ) -> Result<NodeId, RstError> {
        let source = self.options.source_name.clone();
        let mut node = scribe_ast::Node::new(kind);
        node.span = Some(span);
        node.source = Some(SourcePos::new(source, Some(line), Some(span.start)));
        Ok(self.doc.add_node(node, Some(parent))?)
    }

    /// Adds a `Text` leaf with source position and span.
    pub(crate) fn add_text(
        &mut self,
        text: impl Into<String>,
        parent: NodeId,
        span: Span,
        line: u32,
    ) -> Result<NodeId, RstError> {
        let source = self.options.source_name.clone();
        let mut node = scribe_ast::Node::text(text);
        node.span = Some(span);
        node.source = Some(SourcePos::new(source, Some(line), Some(span.start)));
        Ok(self.doc.add_node(node, Some(parent))?)
    }

    /// Builds a source-anchored inline string from a set of logical lines.
    ///
    /// Each line's content is sliced at `base_indent` bytes, exactly as
    /// [`BlockParser::content_at`] does, and the absolute source offset of the
    /// slice is recorded so inline tokens get exact byte spans.
    pub(crate) fn inline_source(&self, indices: &[usize], base_indent: usize) -> InlineSource {
        let parts = indices
            .iter()
            .filter_map(|&idx| self.lines.get(idx))
            .map(|line| {
                let cut = base_indent.min(line.text.len());
                let start = line.start.saturating_add(cut as u32).min(line.end);
                (line.text[cut..].to_owned(), start)
            })
            .collect::<Vec<_>>();
        InlineSource::from_lines(&parts)
    }

    /// Parses `source` for inline markup and appends the resulting nodes to
    /// `parent`. Recoverable inline warnings become `Note` diagnostics.
    pub(crate) fn add_inline(
        &mut self,
        source: InlineSource,
        parent: NodeId,
        line: u32,
    ) -> Result<Vec<NodeId>, RstError> {
        let context = InlineContext {
            line,
            span: Span::new(
                source.offsets.first().copied().unwrap_or(0),
                source.offsets.last().copied().unwrap_or(0),
            ),
            parent,
            source_name: self.options.source_name.clone(),
        };
        let mut parsed = parse_inline(
            &source,
            &InlineOptions {
                source_name: self.options.source_name.clone(),
                extlinks: Arc::clone(&self.options.extlinks),
                default_role: self.options.default_role.clone(),
                custom_roles: self.custom_roles.clone(),
            },
        );
        // Tab-expanded lines have normalized-text byte offsets that can drift
        // past the end of the original source; clamp every inline span to the
        // source bounds so diagnostics and downstream consumers stay safe.
        clamp_inline_spans(&mut parsed.nodes, self.source.len() as u32);
        for warning in &parsed.warnings {
            let diag = RstDiagnostic::new(
                RstError::Note {
                    message: warning.message.clone(),
                    span: self.miette_span(warning.span),
                },
                self.options.source_name.clone(),
                self.source.to_owned(),
            );
            self.diagnostics.push(diag);
        }
        let (ids, xrefs) = materialize_inline(&mut self.doc, &parsed.nodes, parent, &context)?;
        self.inline_xrefs.extend(xrefs);
        Ok(ids)
    }

    /// Parses a single-line inline string that is a substring of a source line.
    ///
    /// Used for terms, classifiers, field names and line-block lines, where
    /// Phase 04 has already extracted a trimmed string from a marker line.
    pub(crate) fn add_inline_slice(
        &mut self,
        text: &str,
        line_index: usize,
        base_indent: usize,
        parent: NodeId,
    ) -> Result<Vec<NodeId>, RstError> {
        let full = self.inline_source(&[line_index], base_indent);
        let source = match full.text.find(text) {
            Some(start) => full.slice(start, start + text.len()),
            None => {
                // `text` was reassembled from several source lines (e.g. a
                // wrapped substitution definition); anchor it at the line's
                // content start instead of guessing an in-string offset.
                let abs = self.lines.get(line_index).map_or(0, |l| {
                    l.start.saturating_add(base_indent.min(l.text.len()) as u32)
                });
                InlineSource::single(text, abs)
            }
        };
        let line = self.line_number(line_index);
        self.add_inline(source, parent, line)
    }

    /// Sets a vector attribute on a node.
    pub(crate) fn set_list(&mut self, id: NodeId, key: &str, values: Vec<String>) -> Result<(), RstError> {
        self.doc.get_mut(id)?.set_list(key, values);
        Ok(())
    }

    /// Sets a string attribute on a node.
    pub(crate) fn set_str(&mut self, id: NodeId, key: &str, value: impl Into<String>) -> Result<(), RstError> {
        self.doc.get_mut(id)?.set_str(key, value);
        Ok(())
    }

    /// Sets an integer attribute on a node.
    pub(crate) fn set_int(&mut self, id: NodeId, key: &str, value: i64) -> Result<(), RstError> {
        self.doc.get_mut(id)?.attrs.set_int(key, value);
        Ok(())
    }

    /// Sets a boolean attribute on a node.
    pub(crate) fn set_bool(&mut self, id: NodeId, key: &str, value: bool) -> Result<(), RstError> {
        self.doc.get_mut(id)?.attrs.set_bool(key, value);
        Ok(())
    }

    /// Records a diagnostic and wraps its error for the given span.
    pub(crate) fn push_diagnostic(&mut self, error: RstError) {
        let name = self.options.source_name.clone();
        let diag = RstDiagnostic::new(error, name, self.source.to_owned());
        self.diagnostics.push(diag);
    }

    /// Builds a miette span from byte offsets.
    pub(crate) fn miette_span(&self, span: Span) -> SourceSpan {
        SourceSpan::new((span.start as usize).into(), span.len() as usize)
    }

    // ----- main dispatch ---------------------------------------------------

    /// Parses a region of lines at indentation `base_indent` into `parent`.
    ///
    /// Consumes lines until EOF or a non-blank line indented less than
    /// `base_indent`. Returns `Ok(())` on completion; individual construct
    /// failures are recorded as diagnostics and recovery skips one line.
    pub(crate) fn parse_blocks(&mut self, parent: NodeId, base_indent: usize) -> Result<(), RstError> {
        loop {
            self.skip_blank();
            if self.pos >= self.limit {
                return Ok(());
            }
            let Some(index) = self.current().map(|_| self.pos) else {
                return Ok(());
            };
            let indent = self.indent_at(index);
            if indent < base_indent {
                return Ok(());
            }
            if indent > base_indent {
                let container = if base_indent == 0 && parent == self.doc.root {
                    self.section_parent.unwrap_or(parent)
                } else {
                    parent
                };
                if let Err(error) = self.parse_indented(container, base_indent, indent) {
                    self.push_diagnostic(error);
                    self.recover_line();
                }
                continue;
            }

            // At document level, blocks belong to the section most recently
            // entered (Docutils' mutable `self.parent`).
            let target = if base_indent == 0 && parent == self.doc.root {
                self.section_parent.unwrap_or(parent)
            } else {
                parent
            };
            let before = self.pos;
            let result = self.parse_one(target, base_indent);
            if let Err(error) = result {
                self.push_diagnostic(error);
                self.recover_line();
            }
            // Progress guard: a construct that failed to consume its line
            // must not wedge the scanner. This is also our recovery path for
            // constructs whose lookahead was invalidated mid-parse.
            if self.pos <= before {
                self.pos = before + 1;
            }
        }
    }

    /// Advances past the current line (recovery helper).
    fn recover_line(&mut self) {
        if self.pos < self.lines.len() {
            self.pos += 1;
        }
    }

    /// Dispatches one construct at the current cursor.
    fn parse_one(&mut self, parent: NodeId, base_indent: usize) -> Result<(), RstError> {
        let index = self.pos;
        let content = self.content_at(index, base_indent).to_owned();

        // 1. Explicit markup (`.. `).
        if is_explicit_markup(&content) {
            return self.parse_explicit(parent, base_indent);
        }

        // 2. Tables (grid / simple).
        if crate::tables::is_grid_table_top(&content) {
            return self.parse_grid_table(parent, base_indent);
        }
        if crate::tables::is_simple_table_top(&content) {
            return self.parse_simple_table(parent, base_indent);
        }

        // 3. Overline / transition. Docutils allows overline sections whose
        // adornment is only as wide as a short title (e.g. `===\nFoo\n===`),
        // so test for an overline before the >=4 transition threshold.
        if let Some((ch, width)) = adornment_of(&content) {
            if width >= 3 && self.is_overline(index, base_indent) {
                return self.parse_overline_section(parent, base_indent, ch, width);
            }
            if width >= 4 {
                if self.is_transition(index, base_indent) {
                    return self.parse_transition(parent, base_indent);
                }
                // A stray adornment line is bad adornment (recover as paragraph).
                let span = self.line_span(index);
                let mspan = self.miette_span(span);
                self.push_diagnostic(RstError::Adornment {
                    message: format!("unexpected '{ch}' adornment line"),
                    span: mspan,
                });
                return self.parse_paragraph(parent, base_indent);
            }
        }

        // 4. Bullet list.
        if crate::lists::bullet_marker(&content).is_some() {
            return self.parse_bullet_list(parent, base_indent);
        }

        // 5. Enumerated list. Docutils rejects an enumerator whose next line
        //    is neither blank, indented, nor the following enumerator; the
        //    marker then belongs to a section title (`1. Title` / `======`).
        if crate::lists::enumerator_marker(&content).is_some()
            && crate::lists::is_enumerated_list_item(
                &content,
                self.line(index + 1).map(|line| line.text()),
            )
        {
            return self.parse_enumerated_list(parent, base_indent);
        }

        // 6. Field list.
        if crate::lists::field_marker(&content).is_some() {
            return self.parse_field_list(parent, base_indent);
        }

        // 7. Option list.
        if crate::lists::option_marker(&content).is_some() {
            return self.parse_option_list(parent, base_indent);
        }

        // 8. Doctest block.
        if is_doctest(&content) {
            return self.parse_doctest(parent, base_indent);
        }

        // 9. Line block.
        if is_line_block(&content) {
            return self.parse_line_block(parent, base_indent);
        }

        // 10. Plain text: section underline, definition list or paragraph.
        self.parse_text(parent, base_indent)
    }

    /// Classifies a plain-text line: section title, definition list, paragraph.
    fn parse_text(&mut self, parent: NodeId, base_indent: usize) -> Result<(), RstError> {
        let index = self.pos;
        let content = self.content_at(index, base_indent);
        let title_width = column_width(content.trim_end());

        // A following adornment line makes this a section title.
        let next = index + 1;
        if let Some((ch, adorn_width)) = self
            .line(next)
            .and_then(|line| adornment_of(line.content()))
        {
            if self.indent_at(next) == base_indent {
                let acceptable = title_width <= adorn_width || adorn_width >= 4;
                if acceptable {
                    if title_width > adorn_width {
                        let span = self.line_span(next);
                        let mspan = self.miette_span(span);
                        self.push_diagnostic(RstError::Adornment {
                            message: format!(
                                "title underline too short (title is {title_width} columns, underline is {adorn_width})"
                            ),
                            span: mspan,
                        });
                    }
                    return self.parse_underlined_section(parent, base_indent, ch, adorn_width);
                }
                // Docutils treats a 1-3 character adornment under a longer
                // title as ordinary text; still report it as suspicious.
                let span = self.line_span(next);
                let mspan = self.miette_span(span);
                self.push_diagnostic(RstError::Adornment {
                    message: format!(
                        "title underline too short (title is {title_width} columns, underline is {adorn_width})"
                    ),
                    span: mspan,
                });
            }
        }

        // An indented next line makes this a definition list item.
        if self
            .line(next)
            .is_some_and(|line| !line.blank && self.indent_at(next) > base_indent)
        {
            return self.parse_definition_list(parent, base_indent);
        }

        self.parse_paragraph(parent, base_indent)
    }

    // ----- paragraphs, literal blocks --------------------------------------

    /// Parses one paragraph, including a trailing `::` literal-block introducer.
    pub(crate) fn parse_paragraph(&mut self, parent: NodeId, base_indent: usize) -> Result<(), RstError> {
        let start = self.pos;
        let mut lines = Vec::new();
        while let Some(index) = self.current().map(|_| self.pos) {
            let line = &self.lines[index];
            if line.blank || self.indent_at(index) < base_indent {
                break;
            }
            let content = self.content_at(index, base_indent);
            // A new construct marker ends the paragraph (blank lines are not
            // required between a paragraph and a list, per Docutils).
            if !lines.is_empty() && self.starts_construct(index, base_indent) {
                break;
            }
            if adornment_of(content).is_some() && !lines.is_empty() {
                break;
            }
            lines.push((index, content.to_owned()));
            self.pos += 1;
        }
        if lines.is_empty() {
            self.pos = start + 1;
            return Ok(());
        }

        let span = self.span_of(start, self.pos);
        let line_no = self.line_number(start);
        let line_indices = lines.iter().map(|(idx, _)| *idx).collect::<Vec<_>>();
        let mut data = lines.iter().map(|(_, text)| text.as_str()).collect::<Vec<_>>().join("\n");
        data = data.trim_end().to_owned();

        // Detect an unescaped `::` at the end of the paragraph.
        let literal_next = ends_with_literal_marker(&data);
        let display = if literal_next {
            let len = data.len();
            if len == 2 {
                String::new()
            } else if data.as_bytes()[len - 3].is_ascii_whitespace() {
                data[..len - 3].trim_end().to_owned()
            } else {
                data[..len - 1].to_owned()
            }
        } else {
            data.clone()
        };

        if !display.is_empty() {
            let para = self.add(NodeKind::Paragraph, parent, span, line_no)?;
            // Phase 05: build a source-anchored view of the paragraph text and
            // expand all inline markup into real AST nodes. `display` is always
            // a prefix of the joined line text, so truncating the offset table
            // keeps every span exact.
            let mut source = self.inline_source(&line_indices, base_indent);
            let keep = display.len().min(source.text.len());
            source.text.truncate(keep);
            source.offsets.truncate(keep + 1);
            self.add_inline(source, para, line_no)?;
        }

        if literal_next {
            self.parse_literal_body(parent, base_indent, start)?;
        }
        Ok(())
    }

    /// Returns `true` when `index` starts a non-paragraph construct.
    fn starts_construct(&self, index: usize, base_indent: usize) -> bool {
        let content = self.content_at(index, base_indent);
        is_explicit_markup(content)
            || crate::tables::is_grid_table_top(content)
            || crate::tables::is_simple_table_top(content)
            || crate::lists::bullet_marker(content).is_some()
            || crate::lists::enumerator_marker(content).is_some()
            || crate::lists::field_marker(content).is_some()
            || crate::lists::option_marker(content).is_some()
            || is_doctest(content)
            || is_line_block(content)
    }

    /// Parses the indented body following a `::` literal introducer.
    fn parse_literal_body(
        &mut self,
        parent: NodeId,
        base_indent: usize,
        introducer_line: usize,
    ) -> Result<(), RstError> {
        self.skip_blank();
        let Some(first) = self.current().map(|_| self.pos) else {
            let span = self.line_span(introducer_line);
            let mspan = self.miette_span(span);
            self.push_diagnostic(RstError::MissingBody {
                message: "literal block expected after '::' but none found".into(),
                span: mspan,
            });
            return Ok(());
        };
        if self.indent_at(first) <= base_indent {
            let span = self.line_span(introducer_line);
            let mspan = self.miette_span(span);
            self.push_diagnostic(RstError::MissingBody {
                message: "literal block expected after '::' but none found".into(),
                span: mspan,
            });
            return Ok(());
        }

        let body_indent = self.min_indent_in_block(first, base_indent);
        let start = self.pos;
        let mut body_lines = Vec::new();
        while let Some(index) = self.current().map(|_| self.pos) {
            let line = &self.lines[index];
            if line.blank {
                // A blank line belongs to the literal block only if followed
                // by another more-indented line.
                if self.next_nonblank_indented(self.pos + 1, body_indent) {
                    body_lines.push(String::new());
                    self.pos += 1;
                    continue;
                }
                break;
            }
            if self.indent_at(index) < body_indent {
                break;
            }
            body_lines.push(self.content_at(index, body_indent).to_owned());
            self.pos += 1;
        }
        while body_lines.last().is_some_and(|line| line.is_empty()) {
            body_lines.pop();
        }
        let body = body_lines.join("\n");
        let span = self.span_of(start, self.pos);
        let line_no = self.line_number(start);
        let literal = self.add(NodeKind::LiteralBlock, parent, span, line_no)?;
        self.set_str(literal, "xml:space", "preserve")?;
        self.add_text(body, literal, span, line_no)?;
        Ok(())
    }

    /// Computes `(start, end, body_indent)` of the indented block beginning at
    /// `from`, without moving the cursor. Blank lines are included when they
    /// are followed by another more-indented line.
    ///
    /// Leading blank lines are skipped so long as a more-indented line follows
    /// (Docutils `StringList.get_indented(..., strip_top=True)`).  This is what
    /// allows an item whose body starts on a later line to absorb a nested
    /// list as a direct child instead of leaking it into a sibling
    /// `block_quote` at the parent level.
    pub(crate) fn collect_indented_range(
        &self,
        from: usize,
        base_indent: usize,
    ) -> (usize, usize, usize) {
        let mut first = from;
        while first < self.lines.len()
            && self.lines[first].blank
            && self.next_nonblank_indented(first + 1, base_indent + 1)
        {
            first += 1;
        }
        if first >= self.lines.len()
            || self.lines[first].blank
            || self.indent_at(first) <= base_indent
        {
            return (from, from, base_indent + 1);
        }
        let body_indent = self.min_indent_in_block(first, base_indent);
        let mut end = first;
        let mut i = first;
        while i < self.lines.len() {
            let line = &self.lines[i];
            if line.blank {
                if self.next_nonblank_indented(i + 1, body_indent) {
                    end = i + 1;
                    i += 1;
                    continue;
                }
                break;
            }
            if self.indent_at(i) < body_indent {
                break;
            }
            end = i + 1;
            i += 1;
        }
        (from, end, body_indent)
    }

    /// Minimum non-blank indentation in the block starting at `from`.
    fn min_indent_in_block(&self, from: usize, base_indent: usize) -> usize {
        let mut min = usize::MAX;
        for index in from..self.lines.len() {
            let line = &self.lines[index];
            if line.blank {
                continue;
            }
            if self.indent_at(index) <= base_indent {
                break;
            }
            min = min.min(self.indent_at(index));
        }
        if min == usize::MAX {
            base_indent + 1
        } else {
            min
        }
    }

    /// `true` when a blank-run is followed by a line indented past `indent`.
    fn next_nonblank_indented(&self, from: usize, indent: usize) -> bool {
        let mut i = from;
        while self.lines.get(i).is_some_and(|line| line.blank) {
            i += 1;
        }
        self.indent_at(i) >= indent && self.lines.get(i).is_some_and(|line| !line.blank)
    }

    // ----- block quotes ----------------------------------------------------

    /// Parses an indented block as one or more block quotes with attribution.
    fn parse_indented(&mut self, parent: NodeId, base_indent: usize, indent: usize) -> Result<(), RstError> {
        let start = self.pos;
        // Docutils' `get_indented()` fixes the body indentation from the first
        // line; later lines indented less than that end the block.
        let body_indent = indent;
        let mut content_end = self.pos;
        while let Some(index) = self.current().map(|_| self.pos) {
            let line = &self.lines[index];
            if line.blank {
                if self.next_nonblank_indented(index + 1, body_indent) {
                    content_end = index + 1;
                    self.pos += 1;
                    continue;
                }
                break;
            }
            if self.indent_at(index) < body_indent {
                break;
            }
            content_end = index + 1;
            self.pos += 1;
        }

        // A non-blank line that is still indented relative to the container
        // but less than the block body is inconsistent indentation.
        if let Some(next) = self.current().map(|_| self.pos) {
            let next_indent = self.indent_at(next);
            if !self.lines[next].blank && next_indent > base_indent && next_indent < body_indent {
                let span = self.line_span(next);
                let mspan = self.miette_span(span);
                self.push_diagnostic(RstError::Indentation {
                    message: format!(
                        "block body starts at column {body_indent} but a following line is at column {next_indent}"
                    ),
                    span: mspan,
                });
            }
        }

        let (body_end, attribution) = self.split_attribution(start, content_end, body_indent);
        let span = self.span_of(start, content_end);
        let line_no = self.line_number(start);
        let quote = self.add(NodeKind::BlockQuote, parent, span, line_no)?;

        // Parse only the body portion, then add any attribution explicitly.
        let saved_limit = self.limit;
        self.limit = body_end;
        self.pos = start;
        let body_result = self.parse_blocks(quote, body_indent);
        self.limit = saved_limit;
        body_result?;

        if let Some((attr_start, attr_end)) = attribution {
            let aspan = self.span_of(attr_start, attr_end);
            let aline = self.line_number(attr_start);
            let node = self.add(NodeKind::Attribution, quote, aspan, aline)?;
            let indices = (attr_start..attr_end).collect::<Vec<_>>();
            let full = self.inline_source(&indices, body_indent);
            let text = full.text.trim().trim_start_matches('-').trim().to_owned();
            let start_byte = full.text.find(&text).unwrap_or(0);
            let source = full.slice(start_byte, start_byte + text.len());
            self.add_inline(source, node, aline)?;
        }

        // Consume the entire indented region, including the attribution.
        self.pos = self.pos.max(content_end);
        Ok(())
    }

    /// Splits an indented region into body lines and an optional attribution.
    ///
    /// Mirrors `Body.split_attribution`: a line beginning with `--`, `---` or
    /// em-dash after a blank line, with consistent following indentation.
    fn split_attribution(
        &self,
        start: usize,
        end: usize,
        indent: usize,
    ) -> (usize, Option<(usize, usize)>) {
        let mut blank_before = None;
        for i in start..end {
            let text = self.content_at(i, indent).trim_end().to_owned();
            if text.is_empty() {
                if blank_before.is_none() && i > start {
                    blank_before = Some(i);
                }
                continue;
            }
            if let Some(blank) = blank_before {
                if text.starts_with("--") || text.starts_with('\u{2014}') {
                    let attr_indent = self.indent_at(i);
                    for j in (i + 1)..end {
                        let jt = self.content_at(j, indent);
                        if jt.is_empty() {
                            break;
                        }
                        if self.indent_at(j) != attr_indent {
                            return (blank, None);
                        }
                    }
                    return (blank, Some((i, end)));
                }
                let _ = blank;
                return (end, None);
            }
        }
        (end, None)
    }

    // ----- doctest ---------------------------------------------------------

    fn parse_doctest(&mut self, parent: NodeId, base_indent: usize) -> Result<(), RstError> {
        let start = self.pos;
        let mut end = self.pos;
        while let Some(index) = self.current().map(|_| self.pos) {
            if self.lines[index].blank || self.indent_at(index) < base_indent {
                break;
            }
            end = index + 1;
            self.pos += 1;
        }
        let text = (start..end)
            .map(|i| self.content_at(i, base_indent))
            .collect::<Vec<_>>()
            .join("\n");
        let span = self.span_of(start, end);
        let line_no = self.line_number(start);
        let node = self.add(NodeKind::DoctestBlock, parent, span, line_no)?;
        self.set_str(node, "xml:space", "preserve")?;
        self.add_text(text, node, span, line_no)?;
        Ok(())
    }

    // ----- transitions & sections -----------------------------------------

    fn is_transition(&self, index: usize, base_indent: usize) -> bool {
        if self.indent_at(index) != base_indent {
            return false;
        }
        let prev_blank = index == 0 || self.lines.get(index - 1).is_some_and(|l| l.blank);
        let next_blank = self
            .lines
            .get(index + 1)
            .is_none_or(|l| l.blank || self.indent_at(index) < base_indent);
        prev_blank && next_blank
    }

    fn parse_transition(&mut self, parent: NodeId, base_indent: usize) -> Result<(), RstError> {
        let index = self.pos;
        let span = self.line_span(index);
        let line_no = self.line_number(index);
        // A transition between sibling sections is emitted at their common
        // parent level (matching `sphinx-build -b pseudoxml`).
        let container = if base_indent == 0 && self.section_stack.len() > 1 {
            let current = self.section_stack.last().map_or(parent, |(_, id)| *id);
            self.doc
                .parent(current)
                .ok()
                .flatten()
                .unwrap_or(parent)
        } else {
            parent
        };
        self.add(NodeKind::Transition, container, span, line_no)?;
        self.pos += 1;
        Ok(())
    }

    /// `true` when line `index` is an overline: the next line is non-blank text
    /// and the line after is an identical adornment line.
    fn is_overline(&self, index: usize, base_indent: usize) -> bool {
        let Some((over_char, over_width)) = self
            .line(index)
            .and_then(|line| adornment_of(line.content()))
        else {
            return false;
        };
        if over_width < 3 || self.indent_at(index) != base_indent {
            return false;
        }
        let Some(title) = self.line(index + 1) else {
            return false;
        };
        if title.blank || self.content_at(index + 1, base_indent).is_empty() {
            return false;
        }
        let Some((under_char, under_width)) = self
            .line(index + 2)
            .and_then(|line| adornment_of(line.content()))
        else {
            return false;
        };
        over_char == under_char && over_width == under_width && self.indent_at(index + 2) == base_indent
    }

    fn parse_overline_section(
        &mut self,
        parent: NodeId,
        base_indent: usize,
        over_char: char,
        over_width: usize,
    ) -> Result<(), RstError> {
        let over_index = self.pos;
        let title_index = over_index + 1;
        let title = self.content_at(title_index, base_indent).trim_end().to_owned();
        let under_char = self
            .line(over_index + 2)
            .and_then(|line| adornment_of(line.content()))
            .map_or(over_char, |(ch, _)| ch);
        if column_width(&title) > over_width {
            let span = self.line_span(over_index);
            let mspan = self.miette_span(span);
            self.push_diagnostic(RstError::Adornment {
                message: format!("title overline too short (title is {} columns)", column_width(&title)),
                span: mspan,
            });
        }
        let style = format!("{over_char}{under_char}");
        let section = self.create_section(parent, &title, style, title_index, over_index + 3)?;
        if base_indent == 0 {
            self.section_parent = Some(section);
        }
        Ok(())
    }

    fn parse_underlined_section(
        &mut self,
        parent: NodeId,
        base_indent: usize,
        adorn_char: char,
        adorn_width: usize,
    ) -> Result<(), RstError> {
        let title_index = self.pos;
        let under_index = title_index + 1;
        let title = self.content_at(title_index, base_indent).trim_end().to_owned();
        let _ = adorn_width;
        let style = adorn_char.to_string();
        let section = self.create_section(parent, &title, style, title_index, under_index + 1)?;
        if base_indent == 0 {
            self.section_parent = Some(section);
        }
        Ok(())
    }

    /// Creates a `Section`/`Title` pair, updates the hierarchy, and registers ids.
    fn create_section(
        &mut self,
        _parent: NodeId,
        title: &str,
        style: AdornmentStyle,
        title_index: usize,
        end_index: usize,
    ) -> Result<NodeId, RstError> {
        let level = match self.title_styles.iter().position(|s| *s == style) {
            Some(pos) => pos + 1,
            None => {
                self.title_styles.push(style.clone());
                self.title_styles.len()
            }
        };

        let old_level = self.section_stack.len();
        if level > old_level + 1 {
            let span = self.line_span(title_index);
            let mspan = self.miette_span(span);
            self.push_diagnostic(RstError::Adornment {
                message: format!(
                    "inconsistent title style: skipped from section level {old_level} to {level}"
                ),
                span: mspan,
            });
        }
        while self.section_stack.len() >= level {
            self.section_stack.pop();
        }
        let parent = self
            .section_stack
            .last()
            .map_or(self.doc.root, |(_, id)| *id);

        let name = normalize_name(title);
        let base_id = make_id(title);
        let id_value = if base_id.is_empty() {
            format!("section-{}", self.used_ids.len() + 1)
        } else {
            unique_id(&base_id, &mut self.used_ids)
        };
        let duplicate = self
            .doc
            .iter()
            .any(|(_, node)| node.get_list("names").ok().flatten().is_some_and(|names| names.contains(&name)));

        let span = self.span_of(title_index, end_index);
        let line_no = self.line_number(title_index);
        let section = self.add(NodeKind::Section, parent, span, line_no)?;
        if duplicate {
            self.set_list(section, "ids", vec![id_value.clone()])?;
            self.set_list(section, "dupnames", vec![name.clone()])?;
        } else {
            self.set_list(section, "ids", vec![id_value.clone()])?;
            self.set_list(section, "names", vec![name.clone()])?;
        }
        let title_span = self.line_span(title_index);
        let title_node = self.add(NodeKind::Title, section, title_span, line_no)?;
        let source = self.inline_source(&[title_index], 0);
        self.add_inline(source, title_node, line_no)?;

        if duplicate {
            let note = RstError::Note {
                message: format!("duplicate implicit target name: \"{name}\""),
                span: self.miette_span(title_span),
            };
            self.push_diagnostic(note);
        }

        self.section_stack.push((style, section));
        self.pos = end_index;
        Ok(section)
    }
}

/// Recursively clamps inline node spans to `[0, max]`.
fn clamp_inline_spans(nodes: &mut [crate::inline::InlineNode], max: u32) {
    for node in nodes {
        node.span.start = node.span.start.min(max);
        node.span.end = node.span.end.min(max).max(node.span.start);
        clamp_inline_spans(&mut node.children, max);
    }
}

// ----- free predicate helpers ----------------------------------------------

/// Docutils `explicit_markup` pattern: `\.\.( +|$)`.
pub(crate) fn is_explicit_markup(content: &str) -> bool {
    content == ".." || content.starts_with(".. ")
}

/// Docutils `doctest` pattern: `>>>( +|$)`.
pub(crate) fn is_doctest(content: &str) -> bool {
    content == ">>>" || content.starts_with(">>> ")
}

/// Docutils `line_block` pattern: `\|( +|$)`.
pub(crate) fn is_line_block(content: &str) -> bool {
    content == "|" || content.starts_with("| ")
}

/// `true` when `text` ends with an unescaped `::` (even number of preceding backslashes).
fn ends_with_literal_marker(text: &str) -> bool {
    if !text.ends_with("::") {
        return false;
    }
    let prefix = &text[..text.len() - 2];
    let backslashes = prefix.bytes().rev().take_while(|&b| b == b'\\').count();
    backslashes % 2 == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_predicates() {
        assert!(is_explicit_markup(".. note:: x"));
        assert!(is_explicit_markup(".."));
        assert!(!is_explicit_markup("..x"));
        assert!(is_doctest(">>> 1 + 1"));
        assert!(!is_doctest(">>>x"));
        assert!(is_line_block("| a"));
        assert!(!is_line_block("|a"));
    }

    #[test]
    fn literal_marker_escaping() {
        assert!(ends_with_literal_marker("text::"));
        assert!(ends_with_literal_marker("::"));
        assert!(!ends_with_literal_marker(r"text\:"));
        assert!(!ends_with_literal_marker(r"text\::"));
        assert!(ends_with_literal_marker(r"text\\::"));
    }
}
