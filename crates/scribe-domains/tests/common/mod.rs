//! Shared fixtures for the Phase 11 domain tests.

#![allow(dead_code)]

use std::collections::BTreeMap;

use scribe_ast::{Document, Node, NodeId, NodeKind, Span};
use scribe_config::{Config, ConfigValue};
use scribe_domains::{DiagnosticSink, DomainContext, DomainEnv};

/// Minimal in-memory `DomainEnv` for unit tests.
#[derive(Debug, Default)]
pub struct TestEnv {
    /// `docname → (title, doctree)`.
    pub docs: BTreeMap<String, (String, Document)>,
    /// Serialized domain inventories.
    pub domain_data: BTreeMap<String, serde_json::Value>,
}

impl TestEnv {
    /// Adds a document with an empty doctree and the given title.
    pub fn with_doc(mut self, docname: &str, title: &str) -> Self {
        self.docs.insert(
            docname.to_owned(),
            (title.to_owned(), Document::new(Some(format!("{docname}.rst")))),
        );
        self
    }

    /// Adds a document whose doctree is supplied by the caller.
    pub fn with_doctree(mut self, docname: &str, title: &str, document: Document) -> Self {
        self.docs.insert((docname).to_owned(), (title.to_owned(), document));
        self
    }
}

impl DomainEnv for TestEnv {
    fn docnames(&self) -> Vec<String> {
        self.docs.keys().cloned().collect()
    }

    fn has_doc(&self, docname: &str) -> bool {
        self.docs.contains_key(docname)
    }

    fn title(&self, docname: &str) -> Option<String> {
        self.docs.get(docname).map(|(title, _)| title.clone())
    }

    fn doctree(&self, docname: &str) -> Option<&Document> {
        self.docs.get(docname).map(|(_, tree)| tree)
    }

    fn domain_data(&self, domain: &str) -> Option<&serde_json::Value> {
        self.domain_data.get(domain)
    }
}

/// Config with selected settings overridden.
pub fn config_with(entries: &[(&str, ConfigValue)]) -> Config {
    let mut config = Config::new();
    for (key, value) in entries {
        config.set(key, value.clone()).expect("config key known");
    }
    config
}

/// Creates a `DomainContext` with deterministic builder values.
pub fn context<'a>(
    env: &'a TestEnv,
    config: &'a Config,
    sink: &'a DiagnosticSink,
    docname: &'a str,
) -> DomainContext<'a> {
    DomainContext::new(
        Some(env),
        Some(config),
        docname,
        "pseudoxml",
        "pseudoxml",
        sink,
    )
}

/// Appends a `pending_xref` with an inline content node, returning
/// `(xref_id, content_node_id, text_node_id)`.
pub fn add_xref(
    doc: &mut Document,
    domain: &str,
    typ: &str,
    target: &str,
    explicit: bool,
    warn: bool,
    title: &str,
) -> (NodeId, NodeId, NodeId) {
    let mut xref = Node::new(NodeKind::PendingXref);
    if let Some(mut view) = xref.pending_xref_mut() {
        view.set_refdomain(Some(domain.to_owned()));
        view.set_reftype(Some(typ.to_owned()));
        view.set_reftarget(Some(target.to_owned()));
        view.set_refexplicit(Some(explicit));
        view.set_refwarn(Some(warn));
        view.set_refdoc(Some("index".to_owned()));
        view.set_classes(vec!["xref".to_owned(), "std".to_owned(), format!("std-{typ}")]);
    }
    xref.span = Some(Span::new(4, target.len() as u32));
    let root = doc.root;
    let xref_id = doc.add_node(xref, Some(root)).expect("add xref");
    let inner_id = doc.add_node(
        Node::new(NodeKind::Inline).with_attr(
            "classes",
            scribe_ast::AttributeValue::List(vec!["xref".to_owned(), "std".to_owned(), format!("std-{typ}")]),
        ),
        Some(xref_id),
    ).expect("add inner");
    let text_id = doc.new_text(title, Some(inner_id)).expect("add text");
    (xref_id, inner_id, text_id)
}

/// Adds a `target` node with an explicit label to `doc`.
pub fn add_label(doc: &mut Document, name: &str, id: &str) -> NodeId {
    let mut target = Node::new(NodeKind::Target);
    target.attrs.set_list("ids", vec![id.to_owned()]);
    target.attrs.set_list("names", vec![name.to_owned()]);
    doc.add_node(target, Some(doc.root)).expect("add target")
}

/// Adds a paragraph whose text is `text`.
pub fn add_paragraph(doc: &mut Document, parent: NodeId, text: &str) -> NodeId {
    let id = doc.add(NodeKind::Paragraph, Some(parent)).expect("add paragraph");
    doc.new_text(text, Some(id)).expect("add text");
    id
}

/// String values used by nitpick_ignore fixtures.
pub fn str_list(values: &[&str]) -> ConfigValue {
    ConfigValue::List(values.iter().map(|value| ConfigValue::Str((*value).to_owned())).collect())
}
