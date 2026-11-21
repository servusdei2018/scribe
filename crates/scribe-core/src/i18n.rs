//! Internationalization primitives shared by the gettext builder and the
//! translation transforms (Phase 26).
//!
//! This module is deliberately free of any PO/MO file-format code — that lives
//! in `scribe-builders::i18n`. It owns the *in-memory* catalog consumed by the
//! `Locale` transform and the message-extraction logic that mirrors upstream
//! `sphinx.util.nodes.extract_messages` / `is_translatable` on the frozen §7
//! AST.
//!
//! Upstream sources inspected (read-only): `sphinx/util/nodes.py`
//! (`is_translatable`, `extract_messages`, `IGNORED_NODES`,
//! `_clean_extracted_message`), `sphinx/transforms/i18n.py` (`Locale`,
//! `TranslationProgressTotaliser`), `sphinx/transforms/__init__.py`
//! (`_calculate_translation_progress`).

use std::collections::BTreeMap;

use scribe_ast::{AttributeValue, Document, Node, NodeId, NodeKind};

// ---------------------------------------------------------------------------
// In-memory translation catalog
// ---------------------------------------------------------------------------

/// A loaded translation catalog: a `msgid → msgstr` map for one gettext
/// *domain* (see `scribe_builders::i18n::docname_to_domain`).
///
/// Empty `msgstr` values are retained (they represent explicitly untranslated
/// messages) but the `Locale` transform ignores them, matching upstream where
/// `if not msgstr` means "not translated".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TranslationCatalog {
    domain: String,
    entries: BTreeMap<String, String>,
}

impl TranslationCatalog {
    /// Creates an empty catalog for `domain`.
    pub fn new(domain: impl Into<String>) -> Self {
        Self { domain: domain.into(), entries: BTreeMap::new() }
    }

    /// Creates a catalog from an iterator of `(msgid, msgstr)` pairs.
    pub fn from_entries<I, K, V>(domain: impl Into<String>, entries: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self {
            domain: domain.into(),
            entries: entries.into_iter().map(|(k, v)| (k.into(), v.into())).collect(),
        }
    }

    /// The gettext domain this catalog was loaded for.
    pub fn domain(&self) -> &str {
        &self.domain
    }

    /// Inserts (or replaces) one translation.
    pub fn insert(&mut self, msgid: impl Into<String>, msgstr: impl Into<String>) {
        self.entries.insert(msgid.into(), msgstr.into());
    }

    /// Looks up a translation by exact `msgid`.
    pub fn get(&self, msgid: &str) -> Option<&str> {
        self.entries.get(msgid).map(String::as_str)
    }

    /// Returns `true` when `msgid` is present, even if its `msgstr` is empty.
    pub fn contains(&self, msgid: &str) -> bool {
        self.entries.contains_key(msgid)
    }

    /// Number of entries (including empty translations).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when no entries are present.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterates entries in deterministic `msgid` order.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

// ---------------------------------------------------------------------------
// Translation progress
// ---------------------------------------------------------------------------

/// The per-document translation progress computed by
/// `TranslationProgressTotaliser`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TranslationProgress {
    /// Number of translatable nodes considered.
    pub total: usize,
    /// Number of those nodes with a non-empty, applied translation.
    pub translated: usize,
}

impl TranslationProgress {
    /// Formats progress the way Sphinx's `_calculate_translation_progress`
    /// does: a two-decimal percentage, or a fixed message when there were no
    /// translatable elements.
    pub fn format(&self) -> String {
        if self.total == 0 {
            return "no translated elements!".to_owned();
        }
        format!("{:.2}%", (self.translated as f64 / self.total as f64) * 100.0)
    }

    /// Attribute keys used to persist progress on the frozen AST's root node.
    pub const TOTAL_KEY: &'static str = "translation_progress_total";
    pub const TRANSLATED_KEY: &'static str = "translation_progress_translated";

    /// Reads progress previously stored on `document`'s root node.
    pub fn from_document(document: &Document) -> Option<Self> {
        let root = document.root();
        let total = int_attr(root, Self::TOTAL_KEY)?;
        let translated = int_attr(root, Self::TRANSLATED_KEY)?;
        Some(Self {
            total: usize::try_from(total).unwrap_or(0),
            translated: usize::try_from(translated).unwrap_or(0),
        })
    }

    /// Stores progress on `document`'s root node.
    pub fn store_on_document(&self, document: &mut Document) {
        let root = document.root_mut();
        root.attrs.set_int(Self::TOTAL_KEY, self.total as i64);
        root.attrs.set_int(Self::TRANSLATED_KEY, self.translated as i64);
    }
}

fn int_attr(node: &Node, key: &str) -> Option<i64> {
    match node.attrs.get(key) {
        Some(AttributeValue::Int(value)) => Some(*value),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Message extraction
// ---------------------------------------------------------------------------

/// One extracted translatable message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtractedMessage {
    /// The message id (cleaned text content).
    pub msgid: String,
    /// Source file of the originating node, when the parser recorded one.
    pub source: String,
    /// 1-based source line, or `-1` when unknown (upstream convention).
    pub line: i64,
    /// The originating node (used by the `Locale` transform).
    pub node: NodeId,
}

/// Returns `true` when `kind` is a text-bearing element that upstream would
/// consider translatable.
///
/// Mirrors `sphinx.util.nodes.is_translatable`: literal/doctest/math blocks,
/// version-modified nodes and (by default) inline nodes are excluded; the
/// explicit `translatable` attribute opts a node in or out.
pub fn is_translatable(node: &Node) -> bool {
    let explicit = match node.attrs.get("translatable") {
        Some(AttributeValue::Bool(value)) => Some(*value),
        Some(AttributeValue::Str(value)) => Some(value.as_ref() == "1" || value.eq_ignore_ascii_case("true")),
        _ => None,
    };
    if explicit == Some(false) {
        return false;
    }
    if explicit == Some(true) {
        return true;
    }

    match node.kind {
        NodeKind::Image => {
            let has_alt = matches!(node.attrs.get("alt"), Some(AttributeValue::Str(alt)) if !alt.is_empty());
            has_alt || node.attrs.contains_key("translatable")
        }
        NodeKind::Inline => false,
        NodeKind::LiteralBlock
        | NodeKind::DoctestBlock
        | NodeKind::MathBlock
        | NodeKind::Raw
        | NodeKind::VersionAdded
        | NodeKind::VersionChanged
        | NodeKind::Deprecated
        | NodeKind::VersionModified
        | NodeKind::Comment
        | NodeKind::Target
        | NodeKind::SystemMessage
        | NodeKind::Pending
        | NodeKind::PendingXref
        | NodeKind::SubstitutionDefinition
        | NodeKind::SubstitutionReference
        | NodeKind::Toctree => false,
        NodeKind::Text
        | NodeKind::Document
        | NodeKind::Section
        | NodeKind::Reference
        | NodeKind::Footnote
        | NodeKind::Citation
        | NodeKind::Figure
        | NodeKind::Table
        // container-like nodes; their text-bearing children are extracted
        // individually and extracting the container too would duplicate text
        | NodeKind::Admonition
        | NodeKind::Note
        | NodeKind::Warning
        | NodeKind::Tip
        | NodeKind::Important
        | NodeKind::Caution
        | NodeKind::Danger
        | NodeKind::Error
        | NodeKind::Hint
        | NodeKind::Attention
        | NodeKind::Seealso
        | NodeKind::Sidebar
        | NodeKind::Topic
        | NodeKind::BlockQuote
        | NodeKind::ListItem
        | NodeKind::DefinitionListItem
        | NodeKind::Field
        | NodeKind::OptionListItem
        | NodeKind::Entry
        | NodeKind::Row
        | NodeKind::Tgroup
        | NodeKind::Thead
        | NodeKind::Tbody
        | NodeKind::Desc
        | NodeKind::DescSignature
        | NodeKind::DescContent
        | NodeKind::Generated
        | NodeKind::Problematic
        | NodeKind::Hlist
        | NodeKind::Glossary
        | NodeKind::Productionlist => false,
        // everything else that carries text in a leaf-ish element is
        // translatable (title, paragraph, rubric, term, caption, ...)
        _ => true,
    }
}

/// Removes trailing line-continuation backslashes and collapses newlines,
/// mirroring `sphinx.util.nodes._clean_extracted_message`.
pub fn clean_extracted_message(text: &str) -> String {
    if !text.contains('\\') {
        return text.replace('\n', " ").trim().to_owned();
    }
    let mut buffer = String::with_capacity(text.len());
    for line in text.split('\n') {
        if let Some(pos) = line.rfind('\\') {
            let prefix = &line[..pos];
            let suffix = &line[pos + 1..];
            if suffix.trim().is_empty() {
                buffer.push_str(prefix.trim_end_matches([' ', '\t']));
            } else {
                buffer.push_str(line);
            }
        } else {
            buffer.push_str(line);
        }
        buffer.push('\n');
    }
    buffer.replace('\n', " ").trim().to_owned()
}

/// Extracts every translatable message from `document` in preorder, together
/// with the source location of its originating node.
///
/// Deterministic: preorder is arena-id order, which is stable for a parsed
/// document, and duplicate messages keep every occurrence (the gettext builder
/// deduplicates them while preserving first-seen order).
pub fn extract_messages(document: &Document) -> Vec<ExtractedMessage> {
    let mut out = Vec::new();
    for id in document.preorder_ids() {
        if id == document.root {
            continue;
        }
        let Ok(node) = document.get(id) else { continue };
        if !is_translatable(node) {
            continue;
        }
        let msgid = if matches!(node.kind, NodeKind::LiteralBlock | NodeKind::DoctestBlock | NodeKind::MathBlock) {
            node.text_content().unwrap_or("").to_owned()
        } else if node.kind == NodeKind::Image {
            node.attrs
                .get("alt")
                .and_then(|v| match v { AttributeValue::Str(s) => Some(s.as_ref()), _ => None })
                .unwrap_or("")
                .to_owned()
        } else {
            clean_extracted_message(&node.astext(document))
        };
        if msgid.is_empty() {
            continue;
        }
        let (source, line) = node_origin(document, id);
        out.push(ExtractedMessage { msgid, source, line, node: id });
    }
    out
}

/// `true` when `id` sits inside a `SubstitutionDefinition` subtree.
pub fn is_in_substitution_definition(document: &Document, id: NodeId) -> bool {
    let mut current = Some(id);
    while let Some(node_id) = current {
        let Ok(node) = document.get(node_id) else { return false };
        if node.kind == NodeKind::SubstitutionDefinition {
            return true;
        }
        current = document.parent(node_id).ok().flatten();
    }
    false
}

/// Finds the nearest ancestor (including `id`) with a recorded source line.
fn node_origin(document: &Document, id: NodeId) -> (String, i64) {
    let mut current = Some(id);
    while let Some(node_id) = current {
        if let Ok(node) = document.get(node_id) {
            if let Some(pos) = &node.source {
                let line = pos.line.map(i64::from).unwrap_or(-1);
                let source = pos
                    .source
                    .clone()
                    .or_else(|| document.source_name.clone())
                    .unwrap_or_default();
                return (source, line);
            }
        }
        current = document.parent(node_id).ok().flatten();
    }
    (document.source_name.clone().unwrap_or_default(), -1)
}

// ---------------------------------------------------------------------------
// Applying translations to the frozen AST
// ---------------------------------------------------------------------------

/// Applies `catalog` to every translatable node of `document`, marking each
/// node with a `translated` boolean attribute and returning translation
/// progress.
///
/// The translated `msgstr` is parsed with the shared RST parser (upstream's
/// `_publish_msgstr` equivalent) and the first paragraph's inline children are
/// transplanted into the original node. This preserves inline markup in
/// translations while keeping the frozen node kind, ids and attributes intact.
pub fn apply_locale(
    document: &mut Document,
    catalog: &TranslationCatalog,
    builder_name: &str,
) -> (bool, TranslationProgress) {
    if builder_name == "gettext" || catalog.is_empty() {
        return (false, TranslationProgress::default());
    }

    let messages = extract_messages(document);
    let mut progress = TranslationProgress { total: messages.len(), translated: 0 };
    let mut changed = false;

    for message in messages {
        let Some(msgstr) = catalog.get(&message.msgid) else {
            mark(document, message.node, false);
            continue;
        };
        if msgstr.trim().is_empty() {
            mark(document, message.node, false);
            continue;
        }
        if msgstr == message.msgid {
            mark(document, message.node, true);
            progress.translated += 1;
            continue;
        }
        if substitute_translation(document, message.node, msgstr) {
            mark(document, message.node, true);
            progress.translated += 1;
            changed = true;
        } else {
            mark(document, message.node, false);
        }
    }

    progress.store_on_document(document);
    (changed, progress)
}

fn mark(document: &mut Document, id: NodeId, translated: bool) {
    if let Ok(node) = document.get_mut(id) {
        node.attrs.set_bool("translated", translated);
    }
}

/// Replaces `id`'s children with the inline children of the first block parsed
/// from `msgstr`. Returns `false` (leaving the document untouched) on malformed
/// or unusable translations.
fn substitute_translation(document: &mut Document, id: NodeId, msgstr: &str) -> bool {
    let parsed = scribe_rst::parse(msgstr);
    let source = &parsed.document;
    let Ok(root_children) = source.children(source.root) else { return false };
    let Some(&first_id) = root_children.first() else { return false };
    let Ok(first) = source.get(first_id) else { return false };

    let child_ids: Vec<NodeId> = if matches!(first.kind, NodeKind::Paragraph | NodeKind::Title) {
        source.children(first_id).map(<[NodeId]>::to_vec).unwrap_or_else(|_| vec![first_id])
    } else {
        vec![first_id]
    };
    if child_ids.is_empty() {
        return false;
    }

    let mut transplanted = Vec::with_capacity(child_ids.len());
    for child in child_ids {
        match transplant(document, source, child) {
            Ok(new_id) => transplanted.push(new_id),
            Err(_) => {
                // Leave the original node intact if any transplant failed.
                return false;
            }
        }
    }
    document.replace_children(id, transplanted).is_ok()
}

/// Deep-copies `src_id` (from `source`) into `destination` with fresh arena ids.
fn transplant(
    destination: &mut Document,
    source: &Document,
    src_id: NodeId,
) -> Result<NodeId, scribe_ast::AstError> {
    let src_node = source.get(src_id)?;
    let mut new_node = src_node.clone();
    let src_children = std::mem::take(&mut new_node.children);
    let dst_id = destination.add_node(new_node, None)?;
    let mut dst_children = Vec::with_capacity(src_children.len());
    for child in src_children {
        dst_children.push(transplant(destination, source, child)?);
    }
    if !dst_children.is_empty() {
        destination.replace_children(dst_id, dst_children)?;
    }
    Ok(dst_id)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_lookup_is_exact_and_deterministic() {
        let mut catalog = TranslationCatalog::new("index");
        catalog.insert("b", "B");
        catalog.insert("a", "");
        assert_eq!(catalog.domain(), "index");
        assert_eq!(catalog.len(), 2);
        assert_eq!(catalog.get("b"), Some("B"));
        assert_eq!(catalog.get("a"), Some(""));
        assert_eq!(catalog.get("missing"), None);
        assert!(catalog.contains("a"));
        let pairs: Vec<_> = catalog.entries().collect();
        assert_eq!(pairs, vec![("a", ""), ("b", "B")]);
    }

    #[test]
    fn clean_extracted_message_handles_continuations() {
        assert_eq!(clean_extracted_message("hello\nworld"), "hello world");
        assert_eq!(clean_extracted_message("  padded \n"), "padded");
        assert_eq!(clean_extracted_message("line\\\nnext"), "line next");
    }

    #[test]
    fn progress_formats_like_upstream() {
        assert_eq!(TranslationProgress { total: 0, translated: 0 }.format(), "no translated elements!");
        assert_eq!(TranslationProgress { total: 16, translated: 11 }.format(), "68.75%");
        assert_eq!(TranslationProgress { total: 1, translated: 1 }.format(), "100.00%");
    }

    #[test]
    fn extraction_and_locale_apply_to_a_parsed_document() {
        let parsed = scribe_rst::parse("Title\n=====\n\nHello world.\n\n.. note:: Notice\n");
        let mut document = parsed.document;
        let messages = extract_messages(&document);
        assert!(messages.iter().any(|m| m.msgid == "Title"));
        assert!(messages.iter().any(|m| m.msgid == "Hello world."));

        let catalog = TranslationCatalog::from_entries(
            "index",
            [("Title", "TITLE"), ("Hello world.", "HELLO WORLD.")],
        );
        let (changed, progress) = apply_locale(&mut document, &catalog, "html");
        assert!(changed);
        assert!(progress.translated >= 2);
        let text = document.to_pseudoxml().unwrap_or_default();
        assert!(text.contains("TITLE"), "{text}");
        assert!(text.contains("HELLO WORLD."), "{text}");
    }

    #[test]
    fn gettext_builder_is_never_translated() {
        let parsed = scribe_rst::parse("Title\n=====\n\nHello.\n");
        let mut document = parsed.document;
        let catalog = TranslationCatalog::from_entries("index", [("Title", "TITLE")]);
        let (changed, progress) = apply_locale(&mut document, &catalog, "gettext");
        assert!(!changed);
        assert_eq!(progress, TranslationProgress::default());
    }

    #[test]
    fn progress_round_trips_through_root_attributes() {
        let mut document = scribe_rst::parse("x\n=\n").document;
        let progress = TranslationProgress { total: 4, translated: 3 };
        progress.store_on_document(&mut document);
        assert_eq!(TranslationProgress::from_document(&document), Some(progress));
    }

    #[test]
    fn malformed_translation_never_panics() {
        let parsed = scribe_rst::parse("Title\n=====\n");
        let mut document = parsed.document;
        let catalog = TranslationCatalog::from_entries(
            "index",
            [("Title", ".. unknown-directive:: oops\n\n   :bad: value\n"), ("", "")],
        );
        let _ = apply_locale(&mut document, &catalog, "html");
    }
}
