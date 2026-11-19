//! Lowering of `pulldown-cmark` events into the frozen `scribe_ast::Document`.
//!
//! The engine streams the event list produced by `Parser::into_offset_iter()`,
//! which already gives exact byte offsets into the (preprocessed) source. It
//! builds the same node kinds, attributes and nesting as the RST parser for
//! equivalent markup, and records MyST-only payloads (frontmatter, anchors,
//! xref side tables) on [`ParseOutput`](crate::ParseOutput).

use std::collections::HashMap;
use std::ops::Range;

use pulldown_cmark::{
    CodeBlockKind, CowStr, Event, HeadingLevel, Options, Parser, Tag, TagEnd,
};
use scribe_ast::{Document, Node, NodeId, NodeKind, SourcePos, Span};
use scribe_rst::pending::fill_pending;
use scribe_rst::XrefMeta;

use crate::directives::{myst_content_to_rst, parse_directive_body, parse_directive_info};
use crate::error::{MystError, MystResult};
use crate::frontmatter::{parse_yaml, Frontmatter};
use crate::names::{make_id, normalize_name, unique_id};
use crate::preprocessor::preprocess_colon_fences;
use crate::roles::emit_role;
use crate::{Anchor, AnchorKind, ParseOptions};

/// A lowering event: a CommonMark event plus MyST-specific synthetic events.
enum LowerEvent<'a> {
    Pc(Event<'a>, Range<usize>),
    Role {
        name: String,
        payload: String,
        full: Range<usize>,
        payload_range: Range<usize>,
    },
    Anchor {
        label: String,
        range: Range<usize>,
    },
    Frontmatter {
        raw: String,
        range: Range<usize>,
    },
    MathBlock {
        text: String,
        range: Range<usize>,
    },
    Image {
        uri: String,
        title: String,
        alt: String,
        range: Range<usize>,
    },
    Directive {
        name: String,
        arguments: String,
        body: String,
        range: Range<usize>,
    },
}

fn span_of(range: &Range<usize>) -> Span {
    Span::new(
        u32::try_from(range.start).unwrap_or(u32::MAX),
        u32::try_from(range.end).unwrap_or(u32::MAX),
    )
}

/// 1-based source line of a byte offset.
fn line_of(source: &str, offset: usize) -> u32 {
    let clamped = offset.min(source.len());
    source[..clamped].bytes().filter(|&b| b == b'\n').count() as u32 + 1
}

fn source_pos(source: &str, range: &Range<usize>) -> SourcePos {
    SourcePos::new(None, Some(line_of(source, range.start)), None)
}

/// Result assembled by the lowering engine.
pub(crate) struct Lowered {
    pub frontmatter: Option<Frontmatter>,
    pub xrefs: Vec<(NodeId, XrefMeta)>,
    pub anchors: Vec<Anchor>,
    pub diagnostics: Vec<MystError>,
}

/// Lowers `source` into `document`, returning additive side tables.
pub(crate) fn lower(
    source: &str,
    options: &ParseOptions,
    document: &mut Document,
) -> MystResult<Lowered> {
    let preprocessed = preprocess_colon_fences(source);
    let mut parser_options = Options::empty();
    parser_options.insert(Options::ENABLE_TABLES);
    parser_options.insert(Options::ENABLE_FOOTNOTES);
    parser_options.insert(Options::ENABLE_MATH);
    parser_options.insert(Options::ENABLE_DEFINITION_LIST);
    parser_options.insert(Options::ENABLE_YAML_STYLE_METADATA_BLOCKS);
    parser_options.insert(Options::ENABLE_HEADING_ATTRIBUTES);

    let raw_events: Vec<(Event<'_>, Range<usize>)> =
        Parser::new_ext(&preprocessed, parser_options).into_offset_iter().collect();
    let events = expand(&raw_events);

    let mut diagnostics = lexical_diagnostics(source);

    let mut ctx = Engine {
        source,
        source_name: options.source_name.as_deref(),
        doc: document,
        used_ids: HashMap::new(),
        frontmatter: None,
        xrefs: Vec::new(),
        anchors: Vec::new(),
        diagnostics: Vec::new(),
        pending_anchor: None,
    };
    ctx.run(events);
    diagnostics.extend(ctx.diagnostics);
    Ok(Lowered {
        frontmatter: ctx.frontmatter,
        xrefs: ctx.xrefs,
        anchors: ctx.anchors,
        diagnostics,
    })
}

// ----- event expansion ------------------------------------------------------

fn expand<'a>(events: &'a [(Event<'a>, Range<usize>)]) -> Vec<LowerEvent<'a>> {
    let mut out = Vec::with_capacity(events.len());
    let mut i = 0usize;
    while i < events.len() {
        match &events[i].0 {
            // YAML/TOML metadata block → structured frontmatter.
            Event::Start(Tag::MetadataBlock(_)) => {
                let range = events[i].1.clone();
                let mut raw = String::new();
                let mut j = i + 1;
                while j < events.len()
                    && !matches!(events[j].0, Event::End(TagEnd::MetadataBlock(_)))
                {
                    if let Event::Text(text) = &events[j].0 {
                        raw.push_str(text);
                    }
                    j += 1;
                }
                out.push(LowerEvent::Frontmatter { raw, range });
                i = j.saturating_add(1);
            }

            // Fenced code block: if its info string is `{directive}`, emit a
            // synthetic directive event carrying the reconstructed body.
            Event::Start(Tag::CodeBlock(kind)) => {
                let info = match kind {
                    CodeBlockKind::Fenced(info) => Some(info.to_string()),
                    CodeBlockKind::Indented => None,
                };
                let directive = info.as_deref().and_then(parse_directive_info);
                if let Some(parsed) = directive {
                    let range = events[i].1.clone();
                    let mut body = String::new();
                    let mut j = i + 1;
                    while j < events.len() && !matches!(events[j].0, Event::End(TagEnd::CodeBlock))
                    {
                        if let Event::Text(text) = &events[j].0 {
                            body.push_str(text);
                        }
                        j += 1;
                    }
                    out.push(LowerEvent::Directive {
                        name: parsed.name,
                        arguments: parsed.arguments,
                        body,
                        range,
                    });
                    i = j.saturating_add(1);
                } else {
                    out.push(LowerEvent::Pc(events[i].0.clone(), events[i].1.clone()));
                    i += 1;
                }
            }

            // `(label)=` on its own line is a MyST cross-reference target.
            Event::Start(Tag::Paragraph)
                if i + 2 < events.len()
                    && matches!(events[i + 1].0, Event::Text(_))
                    && matches!(events[i + 2].0, Event::End(TagEnd::Paragraph)) =>
            {
                if let Event::Text(text) = &events[i + 1].0 {
                    if let Some(label) = target_label(text) {
                        out.push(LowerEvent::Anchor {
                            label,
                            range: events[i + 1].1.clone(),
                        });
                        i += 3;
                        continue;
                    }
                }
                out.push(LowerEvent::Pc(events[i].0.clone(), events[i].1.clone()));
                i += 1;
            }

            // A display-math expression that is the only content of a
            // paragraph is a block, matching MyST's `$$…$$` MathBlock.
            Event::Start(Tag::Paragraph)
                if i + 2 < events.len()
                    && matches!(events[i + 2].0, Event::End(TagEnd::Paragraph))
                    && matches!(events[i + 1].0, Event::DisplayMath(_)) =>
            {
                if let Event::DisplayMath(text) = &events[i + 1].0 {
                    out.push(LowerEvent::MathBlock {
                        text: text.to_string(),
                        range: events[i + 1].1.clone(),
                    });
                    i += 3;
                    continue;
                }
                out.push(LowerEvent::Pc(events[i].0.clone(), events[i].1.clone()));
                i += 1;
            }

            // Image with its alt text gathered into an attribute.
            Event::Start(Tag::Image { .. }) => {
                let range = events[i].1.clone();
                let mut alt = String::new();
                let (mut uri, mut title) = (String::new(), String::new());
                if let Event::Start(Tag::Image { dest_url, title: t, .. }) = &events[i].0 {
                    uri = dest_url.to_string();
                    title = t.to_string();
                }
                let mut j = i + 1;
                while j < events.len() && !matches!(events[j].0, Event::End(TagEnd::Image)) {
                    if let Event::Text(text) = &events[j].0 {
                        alt.push_str(text);
                    }
                    j += 1;
                }
                out.push(LowerEvent::Image {
                    uri,
                    title,
                    alt,
                    range,
                });
                i = j.saturating_add(1);
            }

            // Inline roles: pulldown-cmark emits `Text("prefix {role}")`
            // followed by `Code("payload")`. Detect the suffix and combine.
            Event::Text(text) => {
                let range = events[i].1.clone();
                if let Some((prefix_len, role)) = role_suffix(text) {
                    if let Some(Event::Code(payload)) = events.get(i + 1).map(|e| &e.0) {
                        let code_range = events[i + 1].1.clone();
                        if prefix_len > 0 {
                            let mut prefix_range = range.clone();
                            prefix_range.end = prefix_range.start + prefix_len;
                            out.push(LowerEvent::Pc(
                                Event::Text(CowStr::from(&text[..prefix_len])),
                                prefix_range,
                            ));
                        }
                        let mut full = range.clone();
                        full.start += prefix_len;
                        full.end = code_range.end;
                        out.push(LowerEvent::Role {
                            name: role,
                            payload: payload.to_string(),
                            full,
                            payload_range: code_range,
                        });
                        i += 2;
                        continue;
                    }
                }
                out.push(LowerEvent::Pc(Event::Text(text.clone()), range));
                i += 1;
            }

            _ => {
                out.push(LowerEvent::Pc(events[i].0.clone(), events[i].1.clone()));
                i += 1;
            }
        }
    }
    out
}

/// Returns `(byte index of '{', role name)` when `text` ends with `{role}`.
fn role_suffix(text: &str) -> Option<(usize, String)> {
    if !text.ends_with('}') {
        return None;
    }
    let open = text.rfind('{')?;
    let role = &text[open + 1..text.len() - 1];
    if role.is_empty()
        || !role
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-' | '+'))
    {
        return None;
    }
    Some((open, role.to_owned()))
}

/// Extracts the label from a `(label)=` target line.
pub(crate) fn target_label(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let inner = trimmed.strip_prefix('(')?.strip_suffix(")=")?;
    let label = inner.trim();
    (!label.is_empty()).then(|| label.to_owned())
}

// ----- lexical diagnostics --------------------------------------------------

/// Detects unclosed directive fences and unterminated inline roles before the
/// event lowering. Both are recoverable: the best-effort document is still
/// produced, but the caller receives a miette diagnostic with an exact span.
///
/// Detection is deliberately conservative and skips lines inside fenced code
/// blocks so that literal ` ```{role}\`text` `` examples are not flagged.
fn lexical_diagnostics(source: &str) -> Vec<MystError> {
    let mut out = Vec::new();
    let mut line_start = 0usize;
    let mut fence: Option<(u8, usize, usize, usize)> = None; // (char, len, span start, span end)

    for line in source.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        let trimmed = body.trim_start();
        let indent = body.len() - trimmed.len();
        let first = trimmed.as_bytes().first().copied().unwrap_or(0);
        let run = if first == b'`' || first == b':' {
            trimmed.bytes().take_while(|&b| b == first).count()
        } else {
            0
        };

        let is_fence_line = run >= 3;
        if let Some((fch, flen, _, _)) = fence {
            if is_fence_line && first == fch && run >= flen && trimmed[run..].trim().is_empty() {
                fence = None; // matching closing fence
            }
            // Inside a fence: do not inspect for roles.
        } else if is_fence_line {
            // Track every fence (directive or plain code) so role-looking text
            // inside code blocks is never flagged as an unclosed role.
            fence = Some((first, run, line_start + indent, line_start + body.len()));
        } else {
            // Outside any fence: look for `{role}` immediately followed by an
            // opening backtick with no closing backtick later on the line.
            let mut i = 0usize;
            while i < body.len() {
                if body.as_bytes()[i] == b'{' {
                    if let Some(close_rel) = body[i + 1..].find('}') {
                        let close = i + 1 + close_rel;
                        let role = &body[i + 1..close];
                        let valid_role = !role.is_empty()
                            && role.chars().all(|c| {
                                c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-' | '+')
                            });
                        let tick = close + 1;
                        if valid_role
                            && tick < body.len()
                            && body.as_bytes()[tick] == b'`'
                            && !body[tick + 1..].contains('`')
                        {
                            out.push(MystError::UnclosedRole {
                                message: role.to_owned(),
                                span: miette::SourceSpan::new(
                                    (line_start + i).into(),
                                    (tick + 1 - i).max(1),
                                ),
                            });
                        }
                    }
                }
                i += 1;
            }
        }
        line_start += line.len();
    }

    if let Some((_, _, start, end)) = fence {
        out.push(MystError::UnclosedDirective {
            message: "fence reached end of file".to_owned(),
            span: miette::SourceSpan::new(start.into(), end.saturating_sub(start).max(1)),
        });
    }
    out
}

// ----- the engine -----------------------------------------------------------

#[derive(Clone, Copy)]
struct SectionFrame {
    level: u32,
    #[allow(dead_code)]
    id: NodeId,
    parents_len: usize,
}

struct TableFrame {
    tgroup: NodeId,
    tbody: Option<NodeId>,
}

struct DefListFrame {
    id: NodeId,
    current_item: Option<NodeId>,
}

struct Engine<'a> {
    source: &'a str,
    source_name: Option<&'a str>,
    doc: &'a mut Document,
    used_ids: HashMap<String, usize>,
    frontmatter: Option<Frontmatter>,
    xrefs: Vec<(NodeId, XrefMeta)>,
    anchors: Vec<Anchor>,
    diagnostics: Vec<MystError>,
    pending_anchor: Option<(String, Range<usize>)>,
}

impl Engine<'_> {
    fn run(&mut self, events: Vec<LowerEvent<'_>>) {
        let mut parents: Vec<NodeId> = vec![self.doc.root];
        let mut sections: Vec<SectionFrame> = Vec::new();
        let mut tables: Vec<TableFrame> = Vec::new();
        let mut def_lists: Vec<DefListFrame> = Vec::new();
        let mut index = 0usize;

        while index < events.len() {
            match &events[index] {
                LowerEvent::Frontmatter { raw, range } => {
                    let (fm, diagnostic) = parse_yaml(raw, span_of(range));
                    self.frontmatter = Some(fm);
                    if let Some(err) = diagnostic {
                        self.diagnostics.push(err);
                    }
                    index += 1;
                }

                LowerEvent::Anchor { label, range } => {
                    self.pending_anchor = Some((label.clone(), range.clone()));
                    index += 1;
                }

                LowerEvent::Directive {
                    name,
                    arguments,
                    body,
                    range,
                } => {
                    let parent = *parents.last().unwrap_or(&self.doc.root);
                    self.apply_anchor_to(parent);
                    let (options, content) = parse_directive_body(body);
                    let content = myst_content_to_rst(&content);
                    let mut node = Node::new(NodeKind::Pending);
                    node.span = Some(span_of(range));
                    node.source = Some(source_pos(self.source, range));
                    fill_pending(
                        &mut node,
                        name,
                        arguments,
                        &options,
                        &content,
                        line_of(self.source, range.start),
                    );
                    let _ = self.doc.add_node(node, Some(parent));
                    index += 1;
                }

                LowerEvent::Role {
                    name,
                    payload,
                    full,
                    payload_range,
                } => {
                    let parent = *parents.last().unwrap_or(&self.doc.root);
                    let line = line_of(self.source, full.start);
                    match emit_role(
                        name,
                        payload,
                        span_of(full),
                        span_of(payload_range),
                        line,
                        self.source_name,
                        self.doc,
                        parent,
                    ) {
                        Ok((ids, Some(meta))) => {
                            if let Some(xref_id) = ids.into_iter().find(|&id| {
                                self.doc.get(id).is_ok_and(|n| n.kind == NodeKind::PendingXref)
                            }) {
                                self.xrefs.push((xref_id, meta));
                            }
                        }
                        Ok(_) => {}
                        Err(err) => self.diagnostics.push(err),
                    }
                    index += 1;
                }

                LowerEvent::MathBlock { text, range } => {
                    let parent = *parents.last().unwrap_or(&self.doc.root);
                    self.apply_anchor_to(parent);
                    self.emit_leaf(NodeKind::MathBlock, None, text, range, parent);
                    index += 1;
                }

                LowerEvent::Image {
                    uri,
                    title,
                    alt,
                    range,
                } => {
                    let parent = *parents.last().unwrap_or(&self.doc.root);
                    self.apply_anchor_to(parent);
                    let mut node = Node::new(NodeKind::Image);
                    node.span = Some(span_of(range));
                    node.source = Some(source_pos(self.source, range));
                    node.attrs.set_str("uri", uri.clone());
                    if !alt.is_empty() {
                        node.attrs.set_str("alt", alt.clone());
                    }
                    if !title.is_empty() {
                        node.attrs.set_str("title", title.clone());
                    }
                    let _ = self.doc.add_node(node, Some(parent));
                    index += 1;
                }

                LowerEvent::Pc(event, range) => match event {
                    Event::Start(tag) => self.start_tag(
                        tag,
                        range,
                        &events,
                        &mut index,
                        &mut parents,
                        &mut sections,
                        &mut tables,
                        &mut def_lists,
                    ),
                    Event::End(tag_end) => {
                        self.end_tag(tag_end, &mut parents, &mut tables, &mut def_lists);
                        index += 1;
                    }
                    Event::Text(text) => {
                        let parent = *parents.last().unwrap_or(&self.doc.root);
                        self.emit_leaf(NodeKind::Text, Some(text), text, range, parent);
                        index += 1;
                    }
                    Event::Code(code) => {
                        let parent = *parents.last().unwrap_or(&self.doc.root);
                        self.emit_leaf(NodeKind::Literal, None, code, range, parent);
                        index += 1;
                    }
                    Event::InlineMath(text) => {
                        let parent = *parents.last().unwrap_or(&self.doc.root);
                        self.emit_leaf(NodeKind::Math, None, text, range, parent);
                        index += 1;
                    }
                    Event::DisplayMath(text) => {
                        let parent = *parents.last().unwrap_or(&self.doc.root);
                        self.emit_leaf(NodeKind::MathBlock, None, text, range, parent);
                        index += 1;
                    }
                    Event::Html(text) | Event::InlineHtml(text) => {
                        let parent = *parents.last().unwrap_or(&self.doc.root);
                        let mut node = Node::new(NodeKind::Raw);
                        node.span = Some(span_of(range));
                        node.source = Some(source_pos(self.source, range));
                        node.attrs.set_str("format", "html");
                        let id = self.add(node, parent);
                        self.emit_leaf(NodeKind::Text, None, text, range, id);
                        index += 1;
                    }
                    Event::FootnoteReference(label) => {
                        let parent = *parents.last().unwrap_or(&self.doc.root);
                        let mut node = Node::new(NodeKind::FootnoteReference);
                        node.span = Some(span_of(range));
                        node.source = Some(source_pos(self.source, range));
                        node.attrs.set_str("refname", normalize_name(label));
                        let id = self.add(node, parent);
                        self.emit_leaf(NodeKind::Text, None, label, range, id);
                        index += 1;
                    }
                    Event::SoftBreak => {
                        self.emit_whitespace(" ", range, &parents);
                        index += 1;
                    }
                    Event::HardBreak => {
                        self.emit_whitespace("\n", range, &parents);
                        index += 1;
                    }
                    Event::Rule => {
                        let parent = *parents.last().unwrap_or(&self.doc.root);
                        self.apply_anchor_to(parent);
                        let mut node = Node::new(NodeKind::Transition);
                        node.span = Some(span_of(range));
                        node.source = Some(source_pos(self.source, range));
                        let _ = self.doc.add_node(node, Some(parent));
                        index += 1;
                    }
                    _ => {
                        index += 1;
                    }
                },
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn start_tag(
        &mut self,
        tag: &Tag<'_>,
        range: &Range<usize>,
        events: &[LowerEvent<'_>],
        index: &mut usize,
        parents: &mut Vec<NodeId>,
        sections: &mut Vec<SectionFrame>,
        tables: &mut Vec<TableFrame>,
        def_lists: &mut Vec<DefListFrame>,
    ) {
        let parent = *parents.last().unwrap_or(&self.doc.root);
        match tag {
            Tag::Heading { level, id, classes, .. } => {
                let heading_level = heading_level(level);
                while let Some(frame) = sections.last().copied() {
                    if frame.level < heading_level {
                        break;
                    }
                    parents.truncate(frame.parents_len);
                    sections.pop();
                }
                let section_parent = *parents.last().unwrap_or(&self.doc.root);

                let title_text = collect_heading_text(events, *index);
                let mut section = Node::new(NodeKind::Section);
                section.span = Some(span_of(range));
                section.source = Some(source_pos(self.source, range));
                let (ids, names) = self.heading_ids_names(id.as_deref(), &title_text);
                section.attrs.set_list("ids", ids);
                section.attrs.set_list("names", names);

                let parents_len = parents.len();
                let section_id = self.add(section, section_parent);
                sections.push(SectionFrame {
                    level: heading_level,
                    id: section_id,
                    parents_len,
                });
                parents.push(section_id);

                // `(label)=` overrides the section id/name and registers an
                // anchor for cross-file references.
                if let Some((label, label_range)) = self.pending_anchor.take() {
                    let normalized = normalize_name(&label);
                    if let Ok(node) = self.doc.get_mut(section_id) {
                        node.attrs.set_list("ids", vec![make_id(&normalized)]);
                        node.attrs.set_list("names", vec![normalized.clone()]);
                    }
                    self.anchors.push(Anchor {
                        label: normalized,
                        node_id: section_id,
                        span: span_of(&label_range),
                        kind: AnchorKind::Heading,
                    });
                }

                let mut title = Node::new(NodeKind::Title);
                title.span = Some(span_of(range));
                title.source = Some(source_pos(self.source, range));
                if !classes.is_empty() {
                    title.attrs.set_list(
                        "classes",
                        classes.iter().map(|c| c.to_string()).collect(),
                    );
                }
                parents.push(self.add(title, section_id));
                *index += 1;
            }

            Tag::Paragraph => {
                self.apply_anchor_to(parent);
                let mut node = Node::new(NodeKind::Paragraph);
                node.span = Some(span_of(range));
                node.source = Some(source_pos(self.source, range));
                parents.push(self.add(node, parent));
                *index += 1;
            }

            Tag::BlockQuote(_) => {
                self.apply_anchor_to(parent);
                let mut node = Node::new(NodeKind::BlockQuote);
                node.span = Some(span_of(range));
                node.source = Some(source_pos(self.source, range));
                parents.push(self.add(node, parent));
                *index += 1;
            }

            Tag::List(start) => {
                self.apply_anchor_to(parent);
                let mut node = match start {
                    Some(n) => {
                        let mut list = Node::new(NodeKind::EnumeratedList);
                        list.attrs.set_str("enumtype", "arabic");
                        list.attrs.set_str("prefix", ".");
                        list.attrs.set_str("suffix", ".");
                        list.attrs.set_int("start", i64::try_from(*n).unwrap_or(1));
                        list
                    }
                    None => {
                        let mut list = Node::new(NodeKind::BulletList);
                        list.attrs.set_str("bullet", "-");
                        list
                    }
                };
                node.span = Some(span_of(range));
                node.source = Some(source_pos(self.source, range));
                parents.push(self.add(node, parent));
                *index += 1;
            }

            Tag::Item => {
                let mut node = Node::new(NodeKind::ListItem);
                node.span = Some(span_of(range));
                node.source = Some(source_pos(self.source, range));
                parents.push(self.add(node, parent));
                *index += 1;
            }

            Tag::Emphasis => {
                parents.push(self.container(NodeKind::Emphasis, range, parent));
                *index += 1;
            }
            Tag::Strong => {
                parents.push(self.container(NodeKind::Strong, range, parent));
                *index += 1;
            }
            Tag::Strikethrough => {
                let mut node = Node::new(NodeKind::Inline);
                node.attrs.set_list("classes", vec!["strike".to_owned()]);
                node.span = Some(span_of(range));
                parents.push(self.add(node, parent));
                *index += 1;
            }

            Tag::Link { dest_url, title, .. } => {
                let mut node = Node::new(NodeKind::Reference);
                node.span = Some(span_of(range));
                node.source = Some(source_pos(self.source, range));
                let dest = dest_url.to_string();
                if let Some(fragment) = dest.strip_prefix('#') {
                    node.attrs.set_str("refid", fragment.to_owned());
                } else if !dest.is_empty() {
                    node.attrs.set_str("refuri", dest);
                }
                if !title.is_empty() {
                    node.attrs.set_str("name", title.to_string());
                }
                parents.push(self.add(node, parent));
                *index += 1;
            }

            Tag::CodeBlock(_) => {
                // Non-directive code blocks pass through from `expand`.
                let mut node = Node::new(NodeKind::LiteralBlock);
                node.span = Some(span_of(range));
                node.source = Some(source_pos(self.source, range));
                node.attrs.set_str("xml:space", "preserve");
                parents.push(self.add(node, parent));
                *index += 1;
            }

            Tag::Table(alignments) => {
                self.apply_anchor_to(parent);
                let mut table = Node::new(NodeKind::Table);
                table.span = Some(span_of(range));
                table.source = Some(source_pos(self.source, range));
                let table_id = self.add(table, parent);
                let mut tgroup = Node::new(NodeKind::Tgroup);
                tgroup.attrs.set_int("cols", alignments.len() as i64);
                let tgroup_id = self.add(tgroup, table_id);
                for _ in alignments {
                    let _ = self.doc.add_node(Node::new(NodeKind::Colspec), Some(tgroup_id));
                }
                tables.push(TableFrame {
                    tgroup: tgroup_id,
                    tbody: None,
                });
                parents.push(tgroup_id);
                *index += 1;
            }

            Tag::TableHead => {
                let tgroup = tables.last().map_or(parent, |f| f.tgroup);
                let mut node = Node::new(NodeKind::Thead);
                node.span = Some(span_of(range));
                parents.push(self.add(node, tgroup));
                *index += 1;
            }

            Tag::TableRow => {
                let tgroup = tables.last().map_or(parent, |f| f.tgroup);
                if let Some(frame) = tables.last_mut() {
                    if frame.tbody.is_none() {
                        frame.tbody = Some(self.add(Node::new(NodeKind::Tbody), tgroup));
                    }
                }
                let row_parent = tables.last().and_then(|f| f.tbody).unwrap_or(tgroup);
                let mut node = Node::new(NodeKind::Row);
                node.span = Some(span_of(range));
                parents.push(self.add(node, row_parent));
                *index += 1;
            }

            Tag::TableCell => {
                let mut node = Node::new(NodeKind::Entry);
                node.span = Some(span_of(range));
                parents.push(self.add(node, parent));
                *index += 1;
            }

            Tag::FootnoteDefinition(label) => {
                let mut node = Node::new(NodeKind::Footnote);
                node.span = Some(span_of(range));
                node.source = Some(source_pos(self.source, range));
                let normalized = normalize_name(label);
                node.attrs.set_list("ids", vec![make_id(&normalized)]);
                node.attrs.set_list("names", vec![normalized]);
                parents.push(self.add(node, parent));
                *index += 1;
            }

            Tag::DefinitionList => {
                let mut node = Node::new(NodeKind::DefinitionList);
                node.span = Some(span_of(range));
                node.source = Some(source_pos(self.source, range));
                let id = self.add(node, parent);
                def_lists.push(DefListFrame {
                    id,
                    current_item: None,
                });
                parents.push(id);
                *index += 1;
            }

            Tag::DefinitionListTitle => {
                if let Some(frame) = def_lists.last_mut() {
                    let item_id = match frame.current_item {
                        Some(id) => id,
                        None => {
                            let id = self.add(Node::new(NodeKind::DefinitionListItem), frame.id);
                            frame.current_item = Some(id);
                            id
                        }
                    };
                    let mut term = Node::new(NodeKind::Term);
                    term.span = Some(span_of(range));
                    parents.push(self.add(term, item_id));
                } else {
                    parents.push(self.container(NodeKind::Term, range, parent));
                }
                *index += 1;
            }

            Tag::DefinitionListDefinition => {
                let def_parent = def_lists
                    .last()
                    .and_then(|f| f.current_item)
                    .unwrap_or(parent);
                let mut node = Node::new(NodeKind::Definition);
                node.span = Some(span_of(range));
                parents.push(self.add(node, def_parent));
                *index += 1;
            }

            Tag::MetadataBlock(_) | Tag::HtmlBlock => {
                // Consumed by `expand` / `Event::Html`; keep the End balanced.
                parents.push(parent);
                *index += 1;
            }

            _ => {
                parents.push(parent);
                *index += 1;
            }
        }
    }

    fn end_tag(
        &mut self,
        tag_end: &TagEnd,
        parents: &mut Vec<NodeId>,
        tables: &mut Vec<TableFrame>,
        def_lists: &mut Vec<DefListFrame>,
    ) {
        match tag_end {
            TagEnd::Heading(_) => {
                // Pop only the `Title`; the `Section` stays open as the parent
                // of following blocks (see `start_tag`).
                if parents.len() > 1 {
                    parents.pop();
                }
            }
            TagEnd::Table => {
                if tables.pop().is_some() {
                    if parents.len() > 1 {
                        parents.pop();
                    }
                    parents.pop();
                    parents.pop();
                }
            }
            TagEnd::DefinitionList => {
                def_lists.pop();
                if parents.len() > 1 {
                    parents.pop();
                }
            }
            _ => {
                if parents.len() > 1 {
                    parents.pop();
                }
            }
        }
    }

    fn emit_leaf(
        &mut self,
        kind: NodeKind,
        text_override: Option<&str>,
        text: &str,
        range: &Range<usize>,
        parent: NodeId,
    ) {
        if kind == NodeKind::Text {
            let mut node = Node::text(text.to_owned());
            node.span = Some(span_of(range));
            node.source = Some(source_pos(self.source, range));
            let _ = self.doc.add_node(node, Some(parent));
            return;
        }
        let mut node = Node::new(kind);
        node.span = Some(span_of(range));
        node.source = Some(source_pos(self.source, range));
        let id = self.add(node, parent);
        let mut text_node = Node::text(text_override.unwrap_or(text).to_owned());
        text_node.span = Some(span_of(range));
        let _ = self.doc.add_node(text_node, Some(id));
    }

    fn emit_whitespace(&mut self, text: &str, range: &Range<usize>, parents: &[NodeId]) {
        let parent = *parents.last().unwrap_or(&self.doc.root);
        let mut node = Node::text(text);
        node.span = Some(span_of(range));
        let _ = self.doc.add_node(node, Some(parent));
    }

    fn heading_ids_names(
        &mut self,
        explicit_id: Option<&str>,
        title: &str,
    ) -> (Vec<String>, Vec<String>) {
        let normalized = normalize_name(title);
        let base = explicit_id
            .map(str::to_owned)
            .unwrap_or_else(|| make_id(&normalized));
        let id = unique_id(&base, &mut self.used_ids);
        (vec![id], vec![normalized])
    }

    fn apply_anchor_to(&mut self, parent: NodeId) {
        let Some((label, range)) = self.pending_anchor.take() else {
            return;
        };
        let normalized = normalize_name(&label);
        let mut node = Node::new(NodeKind::Target);
        node.span = Some(span_of(&range));
        node.source = Some(source_pos(self.source, &range));
        node.attrs.set_list("ids", vec![make_id(&normalized)]);
        node.attrs.set_list("names", vec![normalized.clone()]);
        let id = self.add(node, parent);
        self.anchors.push(Anchor {
            label: normalized,
            node_id: id,
            span: span_of(&range),
            kind: AnchorKind::Target,
        });
    }

    fn add(&mut self, node: Node, parent: NodeId) -> NodeId {
        self.doc.add_node(node, Some(parent)).unwrap_or(parent)
    }

    fn container(&mut self, kind: NodeKind, range: &Range<usize>, parent: NodeId) -> NodeId {
        let mut node = Node::new(kind);
        node.span = Some(span_of(range));
        node.source = Some(source_pos(self.source, range));
        self.add(node, parent)
    }
}

fn heading_level(level: &HeadingLevel) -> u32 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn collect_heading_text(events: &[LowerEvent<'_>], start: usize) -> String {
    let mut text = String::new();
    let mut i = start + 1;
    while i < events.len() {
        match &events[i] {
            LowerEvent::Pc(Event::End(TagEnd::Heading(_)), _) => break,
            LowerEvent::Pc(Event::Text(t), _) | LowerEvent::Pc(Event::Code(t), _) => {
                text.push_str(t);
            }
            LowerEvent::Role { payload, .. } => text.push_str(payload),
            _ => {}
        }
        i += 1;
    }
    text
}
