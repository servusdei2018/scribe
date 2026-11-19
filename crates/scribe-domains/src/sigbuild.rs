//! Shared node-building primitives for the C-family / JavaScript signature
//! expanders (Phase 13).
//!
//! Signature text is tokenized into [`SigToken`]s by each domain's hand-written
//! declarator parser; [`SigBuilder`] materializes them as the frozen
//! `desc_*` / `desc_sig_*` nodes (ARCHITECTURE.md §7).  Keeping the emitter in
//! one place guarantees that CSS classes and node nesting are identical across
//! the C, C++ and JavaScript domains, matching Sphinx's shared
//! `addnodes.desc_sig_*` helpers.

use scribe_ast::{Document, Node, NodeId, NodeKind, Span};

use crate::error::{DomainError, DomainResult};

/// One atomic piece of a signature, as emitted by a declarator parser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SigToken {
    /// A built-in type keyword (`int`, `char`, `void`, ...) — CSS `kt`.
    KeywordType(String),
    /// A language keyword (`const`, `class`, `struct`, ...) — CSS `k`.
    Keyword(String),
    /// An identifier that is not a reference — CSS `n`.
    Name(String),
    /// Punctuation (`*`, `,`, `(`, ...) — CSS `p`.
    Punctuation(String),
    /// An operator token (`+`, `()`, `::`, ...) — CSS `o`.
    Operator(String),
    /// A single space — CSS `w`.
    Space,
    /// A numeric literal — CSS `m`.
    Number(String),
    /// A string/character literal — CSS `s`.
    Literal(String),
    /// An identifier that should cross-reference an object (type, name, ...).
    /// Emitted as a `pending_xref` wrapping a `desc_sig_name`.
    Ref {
        /// Display text.
        text: String,
        /// Reference type inside the owning domain (`type`, `identifier`, ...).
        reftype: String,
        /// Fully-qualified lookup target (usually the same as `text`).
        target: String,
        /// CSS classes for the pending xref's content node.
        classes: Vec<String>,
    },
}

impl SigToken {
    /// Constructs a whitespace token.
    pub fn space() -> Self {
        Self::Space
    }

    /// Constructs a cross-reference token with the common `xref <domain>` classes.
    pub fn reference(domain: &str, text: impl Into<String>, reftype: &str, css: &str) -> Self {
        let text = text.into();
        Self::Ref {
            target: text.clone(),
            text,
            reftype: reftype.to_owned(),
            classes: vec!["xref".to_owned(), domain.to_owned(), css.to_owned()],
        }
    }
}

/// Materializes [`SigToken`]s into the frozen AST.
pub struct SigBuilder<'a> {
    /// Document being built.
    pub doc: &'a mut Document,
    /// Span assigned to every created node (the directive marker's span).
    pub span: Span,
    /// Owning domain (used for error reporting).
    pub domain: &'static str,
}

impl<'a> SigBuilder<'a> {
    /// Creates a builder.
    pub fn new(doc: &'a mut Document, span: Span, domain: &'static str) -> Self {
        Self { doc, span, domain }
    }

    fn err(&self, error: scribe_ast::AstError) -> DomainError {
        DomainError::InvalidData {
            domain: self.domain.to_owned(),
            detail: error.to_string(),
        }
    }

    /// Adds an empty node under `parent`.
    pub fn add(&mut self, kind: NodeKind, parent: NodeId) -> DomainResult<NodeId> {
        let mut node = Node::new(kind);
        node.span = Some(self.span);
        self.doc.add_node(node, Some(parent)).map_err(|error| self.err(error))
    }

    /// Adds a text leaf under `parent`.
    pub fn text(&mut self, text: impl Into<String>, parent: NodeId) -> DomainResult<NodeId> {
        let mut node = Node::text(text);
        node.span = Some(self.span);
        self.doc.add_node(node, Some(parent)).map_err(|error| self.err(error))
    }

    /// Adds a classified signature leaf (`desc_sig_*`) with a single CSS class.
    pub fn sig(&mut self, kind: NodeKind, text: &str, css: &str, parent: NodeId) -> DomainResult<NodeId> {
        let id = self.add(kind, parent)?;
        self.classes(id, &[css])?;
        if !text.is_empty() {
            self.text(text, id)?;
        }
        Ok(id)
    }

    /// Sets the CSS class list of `id`.
    pub fn classes(&mut self, id: NodeId, classes: &[&str]) -> DomainResult<()> {
        let domain = self.domain;
        let node = self.doc.get_mut(id).map_err(|error| DomainError::InvalidData {
            domain: domain.to_owned(),
            detail: error.to_string(),
        })?;
        node.attrs.set_list("classes", classes.iter().map(|class| (*class).to_owned()).collect());
        Ok(())
    }

    /// Emits one token under `parent`, returning the created node id.
    pub fn emit(&mut self, token: &SigToken, parent: NodeId) -> DomainResult<NodeId> {
        match token {
            SigToken::KeywordType(text) => self.sig(NodeKind::DescSigKeywordType, text, "kt", parent),
            SigToken::Keyword(text) => self.sig(NodeKind::DescSigKeyword, text, "k", parent),
            SigToken::Name(text) => self.sig(NodeKind::DescSigName, text, "n", parent),
            SigToken::Punctuation(text) => self.sig(NodeKind::DescSigPunctuation, text, "p", parent),
            SigToken::Operator(text) => self.sig(NodeKind::DescSigOperator, text, "o", parent),
            SigToken::Space => self.sig(NodeKind::DescSigSpace, " ", "w", parent),
            SigToken::Number(text) => self.sig(NodeKind::DescSigLiteralNumber, text, "m", parent),
            SigToken::Literal(text) => self.sig(NodeKind::DescSigLiteralString, text, "s", parent),
            SigToken::Ref { text, reftype, target, classes } => {
                let xref = self.add(NodeKind::PendingXref, parent)?;
                if let Ok(node) = self.doc.get_mut(xref) {
                    if let Some(mut view) = node.pending_xref_mut() {
                        view.set_refdomain(Some(self.domain.to_owned()));
                        view.set_reftype(Some(reftype.clone()));
                        view.set_reftarget(Some(target.clone()));
                        view.set_refwarn(Some(false));
                    }
                }
                let name = self.sig(NodeKind::DescSigName, text, "n", xref)?;
                if let Ok(node) = self.doc.get_mut(name) {
                    node.attrs.set_list(
                        "classes",
                        std::iter::once("n".to_owned()).chain(classes.iter().cloned()).collect(),
                    );
                }
                Ok(xref)
            }
        }
    }

    /// Emits a sequence of tokens under `parent`.
    pub fn emit_all(&mut self, tokens: &[SigToken], parent: NodeId) -> DomainResult<()> {
        for token in tokens {
            self.emit(token, parent)?;
        }
        Ok(())
    }
}

/// Joins two token slices with a single space token between them.
pub fn with_space(mut first: Vec<SigToken>, second: &[SigToken]) -> Vec<SigToken> {
    if !first.is_empty() && !second.is_empty() {
        first.push(SigToken::Space);
    }
    first.extend(second.iter().cloned());
    first
}

/// Creates a `desc_signature` + `desc_signature_line` skeleton under `desc`,
/// returning `(signature_id, line_id)`.  Sphinx's C/C++ domains always wrap the
/// signature in a single `desc_signature_line`.
pub fn signature_skeleton(builder: &mut SigBuilder<'_>, desc: NodeId) -> DomainResult<(NodeId, NodeId)> {
    let signature = builder.add(NodeKind::DescSignature, desc)?;
    if let Ok(node) = builder.doc.get_mut(signature) {
        node.attrs.set_list("classes", vec!["sig".to_owned(), "sig-object".to_owned(), builder.domain.to_owned()]);
    }
    let line = builder.add(NodeKind::DescSignatureLine, signature)?;
    if let Ok(node) = builder.doc.get_mut(line) {
        node.attrs.set_bool("add_permalink", true);
        node.attrs.set_str("xml:space", "preserve");
    }
    Ok((signature, line))
}

/// CSS class for the name node inside a `desc_name`.
pub const SIG_NAME_CLASS: &str = "sig-name descname";
