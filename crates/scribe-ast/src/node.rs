//! Node kinds, typed attributes, source positions and the `Node` record.
//!
//! The layout here mirrors Docutils/Sphinx closely enough that a
//! [`Document`](crate::Document) can be serialized to pseudo-XML with
//! [`Document::to_pseudoxml`](crate::Document::to_pseudoxml) and compare
//! structurally against `sphinx-build -b pseudoxml`.
//!
//! # Text handling
//!
//! Docutils stores inline text as `Text` child nodes. We do the same: a
//! [`NodeKind::Text`] leaf keeps its content in the `"text"` string attribute.
//! Helpers [`Node::text`] / [`Node::set_text`] and [`Document::new_text`](crate::Document::new_text)
//! are provided so parsers never need to touch that attribute directly.

use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};

use serde::{Deserialize, Serialize};

use crate::error::{AstError, AstResult};
use crate::NodeId;

/// Byte-based source location of a node.
///
/// `line` is 1-based (Docutils convention) and `offset` is a 0-based byte
/// offset into `source`. All offsets are byte-based, never character-based, so
/// they can be fed straight into `miette` source spans by later phases.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourcePos {
    /// Document source path or name, when known.
    pub source: Option<String>,
    /// 1-based line number.
    pub line: Option<u32>,
    /// 0-based byte offset.
    pub offset: Option<u32>,
}

impl SourcePos {
    /// Creates a source position, filling the remaining fields with `None`.
    pub fn new(source: Option<String>, line: Option<u32>, offset: Option<u32>) -> Self {
        Self {
            source,
            line,
            offset,
        }
    }
}

/// A byte range `[start, end)` on a source, suitable for `miette` diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    /// Inclusive start byte offset.
    pub start: u32,
    /// Exclusive end byte offset.
    pub end: u32,
}

impl Span {
    /// Creates a span. Callers must ensure `start <= end`.
    pub fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    /// Length of the span in bytes.
    pub fn len(self) -> u32 {
        self.end.saturating_sub(self.start)
    }

    /// Returns `true` when the span covers no bytes.
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
}

/// The value of a typed node attribute.
///
/// The variants deliberately cover both Docutils-style attributes
/// ([`Str`](AttributeValue::Str), [`Bool`](AttributeValue::Bool),
/// [`Int`](AttributeValue::Int), [`List`](AttributeValue::List)) and
/// scribe-internal cross references ([`NodeId`](AttributeValue::NodeId),
/// [`NodeIds`](AttributeValue::NodeIds)). The latter are **never** emitted as
/// pseudo-XML attributes because they are arena bookkeeping, not doctree data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AttributeValue {
    /// A string attribute (the common case).
    Str(String),
    /// A boolean attribute. Pseudo-XML renders this as `"1"`/`"0"`.
    Bool(bool),
    /// A signed integer attribute.
    Int(i64),
    /// A floating point attribute.
    Float(f64),
    /// A list-of-strings attribute (`ids`, `classes`, `names`, `dupnames`, ...).
    List(Vec<String>),
    /// An internal reference to a single node (not serialized to pseudo-XML).
    NodeId(NodeId),
    /// An internal reference to several nodes (not serialized to pseudo-XML).
    NodeIds(Vec<NodeId>),
    /// Python `None`. Pseudo-XML renders this as `"True"` (Docutils behaviour).
    Null,
}

impl AttributeValue {
    /// Short type name used in [`AstError::AttributeType`] messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            AttributeValue::Str(_) => "string",
            AttributeValue::Bool(_) => "bool",
            AttributeValue::Int(_) => "int",
            AttributeValue::Float(_) => "float",
            AttributeValue::List(_) => "list",
            AttributeValue::NodeId(_) => "node id",
            AttributeValue::NodeIds(_) => "node ids",
            AttributeValue::Null => "null",
        }
    }
}

impl From<&str> for AttributeValue {
    fn from(v: &str) -> Self {
        AttributeValue::Str(v.to_owned())
    }
}

impl From<String> for AttributeValue {
    fn from(v: String) -> Self {
        AttributeValue::Str(v)
    }
}

impl From<bool> for AttributeValue {
    fn from(v: bool) -> Self {
        AttributeValue::Bool(v)
    }
}

impl From<i64> for AttributeValue {
    fn from(v: i64) -> Self {
        AttributeValue::Int(v)
    }
}

impl From<Vec<String>> for AttributeValue {
    fn from(v: Vec<String>) -> Self {
        AttributeValue::List(v)
    }
}

impl From<NodeId> for AttributeValue {
    fn from(v: NodeId) -> Self {
        AttributeValue::NodeId(v)
    }
}

/// A sorted, typed map of node attributes.
///
/// Backed by a `BTreeMap` so attribute emission is deterministic (Docutils
/// sorts attributes alphabetically in `attlist()`). `Deref`/`DerefMut` expose
/// the standard map API; typed helpers avoid repetitive downcasting.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Attributes(pub BTreeMap<String, AttributeValue>);

impl Attributes {
    /// Creates an empty attribute map.
    pub fn new() -> Self {
        Self(BTreeMap::new())
    }

    /// Returns the raw value for `key`.
    pub fn get(&self, key: &str) -> Option<&AttributeValue> {
        self.0.get(key)
    }

    /// Inserts a value, returning the previous one if present.
    pub fn insert(&mut self, key: impl Into<String>, value: AttributeValue) -> Option<AttributeValue> {
        self.0.insert(key.into(), value)
    }

    /// Removes and returns the value for `key`.
    pub fn remove(&mut self, key: &str) -> Option<AttributeValue> {
        self.0.remove(key)
    }

    /// Returns `true` when there are no attributes.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Number of attributes.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Iterates over `(name, value)` pairs in sorted key order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &AttributeValue)> {
        self.0.iter()
    }

    /// Reads a string attribute.
    pub fn get_str(&self, key: &str) -> AstResult<Option<&str>> {
        match self.0.get(key) {
            None => Ok(None),
            Some(AttributeValue::Str(s)) => Ok(Some(s.as_str())),
            Some(v) => Err(Self::type_error(key, "string", v)),
        }
    }

    /// Reads a boolean attribute.
    pub fn get_bool(&self, key: &str) -> AstResult<Option<bool>> {
        match self.0.get(key) {
            None => Ok(None),
            Some(AttributeValue::Bool(b)) => Ok(Some(*b)),
            Some(v) => Err(Self::type_error(key, "bool", v)),
        }
    }

    /// Reads an integer attribute.
    pub fn get_int(&self, key: &str) -> AstResult<Option<i64>> {
        match self.0.get(key) {
            None => Ok(None),
            Some(AttributeValue::Int(i)) => Ok(Some(*i)),
            Some(v) => Err(Self::type_error(key, "int", v)),
        }
    }

    /// Reads a list-of-strings attribute, returning a clone.
    pub fn get_list(&self, key: &str) -> AstResult<Option<Vec<String>>> {
        match self.0.get(key) {
            None => Ok(None),
            Some(AttributeValue::List(v)) => Ok(Some(v.clone())),
            Some(v) => Err(Self::type_error(key, "list", v)),
        }
    }

    /// Reads an internal single-node reference.
    pub fn get_node_id(&self, key: &str) -> AstResult<Option<NodeId>> {
        match self.0.get(key) {
            None => Ok(None),
            Some(AttributeValue::NodeId(id)) => Ok(Some(*id)),
            Some(v) => Err(Self::type_error(key, "node id", v)),
        }
    }

    /// Reads an internal multi-node reference, returning a clone.
    pub fn get_node_ids(&self, key: &str) -> AstResult<Option<Vec<NodeId>>> {
        match self.0.get(key) {
            None => Ok(None),
            Some(AttributeValue::NodeIds(v)) => Ok(Some(v.clone())),
            Some(v) => Err(Self::type_error(key, "node ids", v)),
        }
    }

    /// Sets a string attribute.
    pub fn set_str(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.insert(key, AttributeValue::Str(value.into()));
    }

    /// Sets a boolean attribute.
    pub fn set_bool(&mut self, key: impl Into<String>, value: bool) {
        self.insert(key, AttributeValue::Bool(value));
    }

    /// Sets an integer attribute.
    pub fn set_int(&mut self, key: impl Into<String>, value: i64) {
        self.insert(key, AttributeValue::Int(value));
    }

    /// Sets a list-of-strings attribute.
    pub fn set_list(&mut self, key: impl Into<String>, value: Vec<String>) {
        self.insert(key, AttributeValue::List(value));
    }

    /// Sets an internal single-node reference.
    pub fn set_node_id(&mut self, key: impl Into<String>, value: NodeId) {
        self.insert(key, AttributeValue::NodeId(value));
    }

    /// Sets an internal multi-node reference.
    pub fn set_node_ids(&mut self, key: impl Into<String>, value: Vec<NodeId>) {
        self.insert(key, AttributeValue::NodeIds(value));
    }

    fn type_error(key: &str, expected: &'static str, found: &AttributeValue) -> AstError {
        AstError::AttributeType {
            key: key.to_owned(),
            expected,
            found: found.type_name(),
        }
    }
}

impl Deref for Attributes {
    type Target = BTreeMap<String, AttributeValue>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Attributes {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl FromIterator<(String, AttributeValue)> for Attributes {
    fn from_iter<T: IntoIterator<Item = (String, AttributeValue)>>(iter: T) -> Self {
        Self(iter.into_iter().collect())
    }
}

/// Every node kind understood by scribe.
///
/// This enum is the frozen set of node tags. The Docutils variants use the
/// lower-case class name as their pseudo-XML tag (e.g. [`NodeKind::BulletList`]
/// → `bullet_list`); Sphinx `addnodes` variants use their class name too. The
/// only special case is [`NodeKind::Text`], whose Docutils tag is `#text`.
///
/// The `VersionAdded` / `VersionChanged` / `Deprecated` variants are
/// convenience aliases for upstream's single `versionmodified` node; their
/// [`tag_name`](NodeKind::tag_name) is `versionmodified` and they carry a
/// `type` attribute, exactly as Sphinx emits them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NodeKind {
    // ----- standard Docutils nodes -----
    Document,
    Section,
    Title,
    Subtitle,
    Paragraph,
    LiteralBlock,
    BulletList,
    EnumeratedList,
    ListItem,
    DefinitionList,
    DefinitionListItem,
    Term,
    Definition,
    FieldList,
    Field,
    FieldName,
    FieldBody,
    OptionList,
    OptionListItem,
    Option,
    OptionString,
    OptionArgument,
    Description,
    LineBlock,
    Line,
    BlockQuote,
    Attribution,
    DoctestBlock,
    Table,
    Tgroup,
    Colspec,
    Thead,
    Tbody,
    Row,
    Entry,
    Admonition,
    Attention,
    Caution,
    Danger,
    Error,
    Hint,
    Important,
    Note,
    Tip,
    Warning,
    Sidebar,
    Topic,
    Rubric,
    Epigraph,
    Highlights,
    PullQuote,
    Compound,
    Container,
    Transition,
    Target,
    Reference,
    Footnote,
    FootnoteReference,
    Citation,
    CitationReference,
    Label,
    Math,
    MathBlock,
    Raw,
    Image,
    Figure,
    Caption,
    Legend,
    SubstitutionDefinition,
    SubstitutionReference,
    Comment,
    SystemMessage,
    Pending,
    Text,
    Emphasis,
    Strong,
    Literal,
    Inline,
    Problematic,
    Generated,

    // ----- Sphinx addnodes -----
    Toctree,
    PendingXref,
    PendingXrefCondition,
    Index,
    Desc,
    DescSignature,
    DescSignatureLine,
    DescContent,
    DescInline,
    DescName,
    DescAddname,
    DescType,
    DescAnnotation,
    DescReturns,
    DescParameterlist,
    DescParameter,
    DescTypeParameterList,
    DescTypeParameter,
    DescOptional,
    DescSigSpace,
    DescSigName,
    DescSigOperator,
    DescSigPunctuation,
    DescSigKeyword,
    DescSigKeywordType,
    DescSigLiteralNumber,
    DescSigLiteralString,
    DescSigLiteralChar,
    VersionModified,
    VersionAdded,
    VersionChanged,
    Deprecated,
    Seealso,
    Highlightlang,
    Only,
    Glossary,
    Productionlist,
    Production,
    CompactParagraph,
    Centered,
    Acks,
    Hlist,
    Hlistcol,
    TabularColSpec,
    NumberReference,
    DownloadReference,
    LiteralEmphasis,
    LiteralStrong,
    Manpage,

    // ----- Phase 34: upstream Docutils/Sphinx node gaps -----
    // Appended at the end to preserve serde discriminant / serialization
    // compatibility (frozen AST §7: additive, append-only changes only).
    /// Default interpreted-text role (`` `text` ``), upstream `title_reference`.
    TitleReference,
    /// Definition-list classifier (`term : classifier`), upstream `classifier`.
    Classifier,
    /// Group of option strings in an option list (`-a, --all <file>`), upstream
    /// `option_group`.
    OptionGroup,
}

impl NodeKind {
    /// Returns the pseudo-XML / Docutils tag name for this kind.
    pub fn tag_name(self) -> &'static str {
        match self {
            NodeKind::Document => "document",
            NodeKind::Section => "section",
            NodeKind::Title => "title",
            NodeKind::Subtitle => "subtitle",
            NodeKind::Paragraph => "paragraph",
            NodeKind::LiteralBlock => "literal_block",
            NodeKind::BulletList => "bullet_list",
            NodeKind::EnumeratedList => "enumerated_list",
            NodeKind::ListItem => "list_item",
            NodeKind::DefinitionList => "definition_list",
            NodeKind::DefinitionListItem => "definition_list_item",
            NodeKind::Term => "term",
            NodeKind::Definition => "definition",
            NodeKind::FieldList => "field_list",
            NodeKind::Field => "field",
            NodeKind::FieldName => "field_name",
            NodeKind::FieldBody => "field_body",
            NodeKind::OptionList => "option_list",
            NodeKind::OptionListItem => "option_list_item",
            NodeKind::Option => "option",
            NodeKind::OptionString => "option_string",
            NodeKind::OptionArgument => "option_argument",
            NodeKind::Description => "description",
            NodeKind::LineBlock => "line_block",
            NodeKind::Line => "line",
            NodeKind::BlockQuote => "block_quote",
            NodeKind::Attribution => "attribution",
            NodeKind::DoctestBlock => "doctest_block",
            NodeKind::Table => "table",
            NodeKind::Tgroup => "tgroup",
            NodeKind::Colspec => "colspec",
            NodeKind::Thead => "thead",
            NodeKind::Tbody => "tbody",
            NodeKind::Row => "row",
            NodeKind::Entry => "entry",
            NodeKind::Admonition => "admonition",
            NodeKind::Attention => "attention",
            NodeKind::Caution => "caution",
            NodeKind::Danger => "danger",
            NodeKind::Error => "error",
            NodeKind::Hint => "hint",
            NodeKind::Important => "important",
            NodeKind::Note => "note",
            NodeKind::Tip => "tip",
            NodeKind::Warning => "warning",
            NodeKind::Sidebar => "sidebar",
            NodeKind::Topic => "topic",
            NodeKind::Rubric => "rubric",
            NodeKind::Epigraph => "epigraph",
            NodeKind::Highlights => "highlights",
            NodeKind::PullQuote => "pull_quote",
            NodeKind::Compound => "compound",
            NodeKind::Container => "container",
            NodeKind::Transition => "transition",
            NodeKind::Target => "target",
            NodeKind::Reference => "reference",
            NodeKind::Footnote => "footnote",
            NodeKind::FootnoteReference => "footnote_reference",
            NodeKind::Citation => "citation",
            NodeKind::CitationReference => "citation_reference",
            NodeKind::Label => "label",
            NodeKind::Math => "math",
            NodeKind::MathBlock => "math_block",
            NodeKind::Raw => "raw",
            NodeKind::Image => "image",
            NodeKind::Figure => "figure",
            NodeKind::Caption => "caption",
            NodeKind::Legend => "legend",
            NodeKind::SubstitutionDefinition => "substitution_definition",
            NodeKind::SubstitutionReference => "substitution_reference",
            NodeKind::Comment => "comment",
            NodeKind::SystemMessage => "system_message",
            NodeKind::Pending => "pending",
            NodeKind::Text => "#text",
            NodeKind::Emphasis => "emphasis",
            NodeKind::Strong => "strong",
            NodeKind::Literal => "literal",
            NodeKind::Inline => "inline",
            NodeKind::Problematic => "problematic",
            NodeKind::Generated => "generated",

            NodeKind::Toctree => "toctree",
            NodeKind::PendingXref => "pending_xref",
            NodeKind::PendingXrefCondition => "pending_xref_condition",
            NodeKind::Index => "index",
            NodeKind::Desc => "desc",
            NodeKind::DescSignature => "desc_signature",
            NodeKind::DescSignatureLine => "desc_signature_line",
            NodeKind::DescContent => "desc_content",
            NodeKind::DescInline => "desc_inline",
            NodeKind::DescName => "desc_name",
            NodeKind::DescAddname => "desc_addname",
            NodeKind::DescType => "desc_type",
            NodeKind::DescAnnotation => "desc_annotation",
            NodeKind::DescReturns => "desc_returns",
            NodeKind::DescParameterlist => "desc_parameterlist",
            NodeKind::DescParameter => "desc_parameter",
            NodeKind::DescTypeParameterList => "desc_type_parameter_list",
            NodeKind::DescTypeParameter => "desc_type_parameter",
            NodeKind::DescOptional => "desc_optional",
            NodeKind::DescSigSpace => "desc_sig_space",
            NodeKind::DescSigName => "desc_sig_name",
            NodeKind::DescSigOperator => "desc_sig_operator",
            NodeKind::DescSigPunctuation => "desc_sig_punctuation",
            NodeKind::DescSigKeyword => "desc_sig_keyword",
            NodeKind::DescSigKeywordType => "desc_sig_keyword_type",
            NodeKind::DescSigLiteralNumber => "desc_sig_literal_number",
            NodeKind::DescSigLiteralString => "desc_sig_literal_string",
            NodeKind::DescSigLiteralChar => "desc_sig_literal_char",
            NodeKind::VersionModified
            | NodeKind::VersionAdded
            | NodeKind::VersionChanged
            | NodeKind::Deprecated => "versionmodified",
            NodeKind::Seealso => "seealso",
            NodeKind::Highlightlang => "highlightlang",
            NodeKind::Only => "only",
            NodeKind::Glossary => "glossary",
            NodeKind::Productionlist => "productionlist",
            NodeKind::Production => "production",
            NodeKind::CompactParagraph => "compact_paragraph",
            NodeKind::Centered => "centered",
            NodeKind::Acks => "acks",
            NodeKind::Hlist => "hlist",
            NodeKind::Hlistcol => "hlistcol",
            NodeKind::TabularColSpec => "tabular_col_spec",
            NodeKind::NumberReference => "number_reference",
            NodeKind::DownloadReference => "download_reference",
            NodeKind::LiteralEmphasis => "literal_emphasis",
            NodeKind::LiteralStrong => "literal_strong",
            NodeKind::Manpage => "manpage",

            NodeKind::TitleReference => "title_reference",
            NodeKind::Classifier => "classifier",
            NodeKind::OptionGroup => "option_group",
        }
    }

    /// The upstream `type` attribute value for the version-modified aliases.
    pub fn version_type(self) -> Option<&'static str> {
        match self {
            NodeKind::VersionAdded => Some("versionadded"),
            NodeKind::VersionChanged => Some("versionchanged"),
            NodeKind::Deprecated => Some("deprecated"),
            _ => None,
        }
    }

    /// `true` for the standard admonition-family nodes.
    pub fn is_admonition(self) -> bool {
        matches!(
            self,
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
                | NodeKind::VersionModified
                | NodeKind::VersionAdded
                | NodeKind::VersionChanged
                | NodeKind::Deprecated
        )
    }

    /// `true` for node kinds that Docutils classifies as inline elements.
    pub fn is_inline(self) -> bool {
        matches!(
            self,
            NodeKind::Text
                | NodeKind::Emphasis
                | NodeKind::Strong
                | NodeKind::Literal
                | NodeKind::Inline
                | NodeKind::Reference
                | NodeKind::FootnoteReference
                | NodeKind::CitationReference
                | NodeKind::SubstitutionReference
                | NodeKind::Math
                | NodeKind::Problematic
                | NodeKind::Generated
                | NodeKind::PendingXref
                | NodeKind::PendingXrefCondition
                | NodeKind::DescInline
                | NodeKind::TitleReference
        )
    }

    /// `true` for the four aliases that render as `versionmodified`.
    pub fn is_version_modified(self) -> bool {
        self.version_type().is_some() || self == NodeKind::VersionModified
    }
}

/// One node in the arena.
///
/// The struct is deliberately flat: writers pattern-match on `kind` and read
/// node-specific data from the typed [`attrs`](Node::attrs) map. Arena indices
/// are stored in `children` (and, for cross references, in typed attributes),
/// never as boxed pointers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    /// The node's tag/kind.
    pub kind: NodeKind,
    /// Typed attribute map (ids, classes, names, source-specific fields, ...).
    pub attrs: Attributes,
    /// Child node ids in document order.
    pub children: Vec<NodeId>,
    /// Source position, used for diagnostics and `source`/`line` metadata.
    pub source: Option<SourcePos>,
    /// Byte span on the source, used for `miette` diagnostics.
    pub span: Option<Span>,
}

impl Node {
    /// Creates an empty node of `kind`.
    pub fn new(kind: NodeKind) -> Self {
        Self {
            kind,
            attrs: Attributes::new(),
            children: Vec::new(),
            source: None,
            span: None,
        }
    }

    /// Creates a `Text` leaf carrying `text`.
    pub fn text(text: impl Into<String>) -> Self {
        let mut node = Self::new(NodeKind::Text);
        node.set_text(text);
        node
    }

    /// Builder: sets an attribute and returns `self`.
    pub fn with_attr(mut self, key: impl Into<String>, value: AttributeValue) -> Self {
        self.attrs.insert(key, value);
        self
    }

    /// Builder: appends a child id and returns `self`.
    pub fn with_child(mut self, child: NodeId) -> Self {
        self.children.push(child);
        self
    }

    /// Builder: sets the source position and returns `self`.
    pub fn with_source(mut self, source: SourcePos) -> Self {
        self.source = Some(source);
        self
    }

    /// Builder: sets the byte span and returns `self`.
    pub fn with_span(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    /// Returns the text content of a [`NodeKind::Text`] leaf.
    ///
    /// For non-text nodes this returns `None`; use [`Node::astext`] for the
    /// recursive concatenation of a subtree's text.
    pub fn text_content(&self) -> Option<&str> {
        if self.kind != NodeKind::Text {
            return None;
        }
        self.attrs
            .get_str("text")
            .unwrap_or(None)
    }

    /// Sets the text content of a [`NodeKind::Text`] leaf.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.attrs.set_str("text", text);
    }

    /// Concatenates the text of this node and its descendants in document order.
    ///
    /// This is a convenience approximation of Docutils' `astext()`; it does not
    /// reproduce `child_text_separator` rules for every node kind.
    pub fn astext(&self, doc: &crate::Document) -> String {
        let mut out = String::new();
        self.collect_text(doc, &mut out);
        out
    }

    fn collect_text(&self, doc: &crate::Document, out: &mut String) {
        if let Some(text) = self.text_content() {
            out.push_str(text);
        }
        for &child in &self.children {
            if let Ok(node) = doc.get(child) {
                node.collect_text(doc, out);
            }
        }
    }

    /// Reads a string attribute through the typed map.
    pub fn get_str(&self, key: &str) -> AstResult<Option<&str>> {
        self.attrs.get_str(key)
    }

    /// Sets a string attribute through the typed map.
    pub fn set_str(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.attrs.set_str(key, value);
    }

    /// Reads a list-of-strings attribute (e.g. `ids`, `classes`, `names`).
    pub fn get_list(&self, key: &str) -> AstResult<Option<Vec<String>>> {
        self.attrs.get_list(key)
    }

    /// Sets a list-of-strings attribute.
    pub fn set_list(&mut self, key: impl Into<String>, value: Vec<String>) {
        self.attrs.set_list(key, value);
    }

    /// The node's `ids` list, defaulting to empty.
    pub fn ids(&self) -> AstResult<Vec<String>> {
        Ok(self.get_list("ids")?.unwrap_or_default())
    }

    /// The node's `classes` list, defaulting to empty.
    pub fn classes(&self) -> AstResult<Vec<String>> {
        Ok(self.get_list("classes")?.unwrap_or_default())
    }

    /// The node's `names` list, defaulting to empty.
    pub fn names(&self) -> AstResult<Vec<String>> {
        Ok(self.get_list("names")?.unwrap_or_default())
    }

    /// Returns a typed view of this node's `pending_xref` attributes, or
    /// `None` if the node is not a [`NodeKind::PendingXref`].
    pub fn pending_xref(&self) -> Option<PendingXref<'_>> {
        if self.kind == NodeKind::PendingXref {
            Some(PendingXref(&self.attrs))
        } else {
            None
        }
    }

    /// Returns a mutable typed view of this node's `pending_xref` attributes.
    pub fn pending_xref_mut(&mut self) -> Option<PendingXrefMut<'_>> {
        if self.kind == NodeKind::PendingXref {
            Some(PendingXrefMut(&mut self.attrs))
        } else {
            None
        }
    }

    /// Returns the `condition` attribute of a `pending_xref_condition` node.
    pub fn condition(&self) -> AstResult<Option<&str>> {
        self.get_str("condition")
    }
}

/// Frozen typed view of the `pending_xref` attribute set.
///
/// See `ARCHITECTURE.md` §5 — this shape is a frozen cross-phase contract.
/// The backing storage is the node's [`Attributes`] map; values that are arena
/// references use [`AttributeValue::NodeId`] / [`AttributeValue::NodeIds`] and
/// are never serialized into pseudo-XML.
#[derive(Clone, Copy, Debug)]
pub struct PendingXref<'a>(&'a Attributes);

impl<'a> PendingXref<'a> {
    fn strval(&self, key: &str) -> Option<&'a str> {
        // The lifetime is sound because the Attributes outlive this view.
        match self.0.get(key) {
            Some(AttributeValue::Str(s)) => Some(s.as_str()),
            _ => None,
        }
    }

    fn boolval(&self, key: &str) -> Option<bool> {
        match self.0.get(key) {
            Some(AttributeValue::Bool(b)) => Some(*b),
            _ => None,
        }
    }

    /// `refdomain` — the domain that owns the reference (`py`, `std`, ...).
    pub fn refdomain(&self) -> Option<&'a str> {
        self.strval("refdomain")
    }
    /// `reftype` — the object type within the domain (`func`, `class`, ...).
    pub fn reftype(&self) -> Option<&'a str> {
        self.strval("reftype")
    }
    /// `reftarget` — the normalized target name.
    pub fn reftarget(&self) -> Option<&'a str> {
        self.strval("reftarget")
    }
    /// `refexplicit` — whether the role had an explicit title.
    pub fn refexplicit(&self) -> Option<bool> {
        self.boolval("refexplicit")
    }
    /// `refspecific` — whether the target was marked specific via `~`/`.`.
    pub fn refspecific(&self) -> Option<bool> {
        self.boolval("refspecific")
    }
    /// `refwarn` — whether a failed resolution should emit a warning.
    pub fn refwarn(&self) -> Option<bool> {
        self.boolval("refwarn")
    }
    /// `refdoc` — docname in which the reference was encountered.
    pub fn refdoc(&self) -> Option<&'a str> {
        self.strval("refdoc")
    }
    /// `refid` — resolved local id, when resolution succeeded locally.
    pub fn refid(&self) -> Option<&'a str> {
        self.strval("refid")
    }
    /// `refuri` — resolved external URI, when resolution produced one.
    pub fn refuri(&self) -> Option<&'a str> {
        self.strval("refuri")
    }
    /// `reftitle` — resolved title used for the generated reference.
    pub fn reftitle(&self) -> Option<&'a str> {
        self.strval("reftitle")
    }
    /// `refname` — name-based lookup target used by some domains.
    pub fn refname(&self) -> Option<&'a str> {
        self.strval("refname")
    }
    /// `classes` — CSS classes attached to the reference.
    pub fn classes(&self) -> Vec<String> {
        match self.0.get("classes") {
            Some(AttributeValue::List(v)) => v.clone(),
            _ => Vec::new(),
        }
    }
    /// `inner` — id of the explicit inner content node, when present.
    pub fn inner(&self) -> Option<NodeId> {
        match self.0.get("inner") {
            Some(AttributeValue::NodeId(id)) => Some(*id),
            _ => None,
        }
    }
    /// `resolve_conditions` — ids of child `pending_xref_condition` nodes.
    pub fn resolve_conditions(&self) -> Vec<NodeId> {
        match self.0.get("resolve_conditions") {
            Some(AttributeValue::NodeIds(v)) => v.clone(),
            _ => Vec::new(),
        }
    }
}

/// Mutable counterpart of [`PendingXref`].
#[derive(Debug)]
pub struct PendingXrefMut<'a>(&'a mut Attributes);

impl PendingXrefMut<'_> {
    fn set_str(&mut self, key: &str, value: Option<String>) {
        match value {
            Some(v) => {
                self.0.set_str(key, v);
            }
            None => {
                self.0.remove(key);
            }
        }
    }

    fn set_bool(&mut self, key: &str, value: Option<bool>) {
        match value {
            Some(v) => {
                self.0.set_bool(key, v);
            }
            None => {
                self.0.remove(key);
            }
        }
    }

    /// Sets/clears `refdomain`.
    pub fn set_refdomain(&mut self, value: Option<String>) { self.set_str("refdomain", value); }
    /// Sets/clears `reftype`.
    pub fn set_reftype(&mut self, value: Option<String>) { self.set_str("reftype", value); }
    /// Sets/clears `reftarget`.
    pub fn set_reftarget(&mut self, value: Option<String>) { self.set_str("reftarget", value); }
    /// Sets/clears `refexplicit`.
    pub fn set_refexplicit(&mut self, value: Option<bool>) { self.set_bool("refexplicit", value); }
    /// Sets/clears `refspecific`.
    pub fn set_refspecific(&mut self, value: Option<bool>) { self.set_bool("refspecific", value); }
    /// Sets/clears `refwarn`.
    pub fn set_refwarn(&mut self, value: Option<bool>) { self.set_bool("refwarn", value); }
    /// Sets/clears `refdoc`.
    pub fn set_refdoc(&mut self, value: Option<String>) { self.set_str("refdoc", value); }
    /// Sets/clears `refid`.
    pub fn set_refid(&mut self, value: Option<String>) { self.set_str("refid", value); }
    /// Sets/clears `refuri`.
    pub fn set_refuri(&mut self, value: Option<String>) { self.set_str("refuri", value); }
    /// Sets/clears `reftitle`.
    pub fn set_reftitle(&mut self, value: Option<String>) { self.set_str("reftitle", value); }
    /// Sets/clears `refname`.
    pub fn set_refname(&mut self, value: Option<String>) { self.set_str("refname", value); }
    /// Sets/clears `classes`.
    pub fn set_classes(&mut self, value: Vec<String>) { self.0.set_list("classes", value); }
    /// Sets/clears `inner`.
    pub fn set_inner(&mut self, value: Option<NodeId>) {
        match value {
            Some(id) => { self.0.set_node_id("inner", id); }
            None => { self.0.remove("inner"); }
        }
    }
    /// Sets/clears `resolve_conditions`.
    pub fn set_resolve_conditions(&mut self, value: Vec<NodeId>) {
        self.0.set_node_ids("resolve_conditions", value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_names_are_upstream_names() {
        assert_eq!(NodeKind::Paragraph.tag_name(), "paragraph");
        assert_eq!(NodeKind::BulletList.tag_name(), "bullet_list");
        assert_eq!(NodeKind::PendingXref.tag_name(), "pending_xref");
        assert_eq!(NodeKind::Text.tag_name(), "#text");
        assert_eq!(NodeKind::VersionAdded.tag_name(), "versionmodified");
        assert_eq!(NodeKind::VersionAdded.version_type(), Some("versionadded"));
        // Phase 34 node gaps use their exact upstream tag names.
        assert_eq!(NodeKind::TitleReference.tag_name(), "title_reference");
        assert_eq!(NodeKind::Classifier.tag_name(), "classifier");
        assert_eq!(NodeKind::OptionGroup.tag_name(), "option_group");
        assert!(NodeKind::TitleReference.is_inline());
        assert!(!NodeKind::Classifier.is_inline());
        assert!(!NodeKind::OptionGroup.is_inline());
    }

    #[test]
    fn phase34_variants_are_append_only() {
        // serde derives assign discriminants in declaration order, so new
        // variants must sit at the END of the enum (frozen AST §7). The JSON
        // representation is the variant name, so older documents still decode.
        let kinds = [NodeKind::TitleReference, NodeKind::Classifier, NodeKind::OptionGroup];
        for kind in kinds {
            let json = serde_json::to_string(&kind).unwrap_or_default();
            let back: NodeKind = serde_json::from_str(&json).unwrap_or(NodeKind::Text);
            assert_eq!(back, kind, "round-trip failed for {json}");
        }
    }

    #[test]
    fn text_nodes_store_content_in_attribute() {
        let node = Node::text("hello\nworld");
        assert_eq!(node.kind, NodeKind::Text);
        assert_eq!(node.text_content(), Some("hello\nworld"));
        let mut node = node;
        node.set_text("changed");
        assert_eq!(node.text_content(), Some("changed"));
        assert!(Node::new(NodeKind::Paragraph).text_content().is_none());
    }

    #[test]
    fn typed_attribute_helpers_and_errors() {
        let mut attrs = Attributes::new();
        attrs.set_str("id", "a");
        attrs.set_bool("flag", true);
        attrs.set_int("n", 3);
        attrs.set_list("names", vec!["a".into(), "b".into()]);
        assert_eq!(attrs.get_str("id").unwrap(), Some("a"));
        assert_eq!(attrs.get_bool("flag").unwrap(), Some(true));
        assert_eq!(attrs.get_int("n").unwrap(), Some(3));
        assert_eq!(attrs.get_list("names").unwrap().unwrap().len(), 2);
        let err = attrs.get_str("flag").unwrap_err();
        assert!(matches!(err, AstError::AttributeType { .. }));
    }

    #[test]
    fn pending_xref_typed_accessors_round_trip() {
        let mut node = Node::new(NodeKind::PendingXref);
        {
            let mut xr = node.pending_xref_mut().expect("pending xref node");
            xr.set_refdomain(Some("py".into()));
            xr.set_reftype(Some("func".into()));
            xr.set_reftarget(Some("io.StringIO".into()));
            xr.set_refexplicit(Some(false));
            xr.set_refwarn(Some(true));
            xr.set_classes(vec!["xref".into(), "py".into()]);
            xr.set_inner(Some(NodeId(7)));
            xr.set_resolve_conditions(vec![NodeId(8), NodeId(9)]);
        }
        let xr = node.pending_xref().expect("pending xref node");
        assert_eq!(xr.refdomain(), Some("py"));
        assert_eq!(xr.reftype(), Some("func"));
        assert_eq!(xr.reftarget(), Some("io.StringIO"));
        assert_eq!(xr.refexplicit(), Some(false));
        assert_eq!(xr.refwarn(), Some(true));
        assert_eq!(xr.classes(), vec!["xref", "py"]);
        assert_eq!(xr.inner(), Some(NodeId(7)));
        assert_eq!(xr.resolve_conditions(), vec![NodeId(8), NodeId(9)]);
        assert!(Node::new(NodeKind::Paragraph).pending_xref().is_none());
    }

    #[test]
    fn span_length() {
        assert_eq!(Span::new(3, 9).len(), 6);
        assert_eq!(Span::new(9, 3).len(), 0);
    }
}
