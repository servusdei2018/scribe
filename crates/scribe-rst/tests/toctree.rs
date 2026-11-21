//! Phase 09: `toctree` directive parsing (spans, options and structured
//! entries) at the `scribe-rst` boundary.

use std::sync::Arc;

use scribe_ast::{AttributeValue, NodeKind};
use scribe_config::{Config, Tags};
use scribe_rst::{
    parse_with_options, resolve_output, Context, DirectiveRegistry, ParseOptions,
};

const SOURCE: &str = "\
Root
====

.. toctree::
   :maxdepth: 3
   :caption: Contents
   :numbered:
   :glob:
   :hidden:
   :titlesonly:
   :includehidden:
   :reversed:
   :name: mastertoc

   foo
   A title <bar>
   sub/*
   https://example.org/docs
   self
";

#[test]
fn toctree_directive_emits_structured_entries_with_exact_spans() {
    let mut output = parse_with_options(SOURCE, ParseOptions::new(Some("index".to_owned())));
    let config = Config::new();
    let mut context = Context::new("docs", Tags::new());
    context.config = Some(Arc::new(config));
    context.source_name = Some("index".to_owned());
    let report = resolve_output(&mut output, &DirectiveRegistry::builtin(), &context);
    assert!(report.is_clean(), "{:?}", report.unresolved);

    let doc = &output.document;
    let ids: Vec<_> = doc
        .preorder_ids()
        .into_iter()
        .filter(|&id| doc.get(id).is_ok_and(|node| node.kind == NodeKind::Toctree))
        .collect();
    assert_eq!(ids.len(), 1);
    let node = doc.get(ids[0]).expect("toctree node");

    assert_eq!(node.attrs.get("parent"), Some(&AttributeValue::Str("index".into())));
    assert_eq!(node.attrs.get("maxdepth"), Some(&AttributeValue::Int(3)));
    assert_eq!(node.attrs.get("caption"), Some(&AttributeValue::Str("Contents".into())));
    assert_eq!(node.attrs.get("numbered"), Some(&AttributeValue::Int(999)));
    assert_eq!(node.attrs.get("glob"), Some(&AttributeValue::Bool(true)));
    assert_eq!(node.attrs.get("hidden"), Some(&AttributeValue::Bool(true)));
    assert_eq!(node.attrs.get("titlesonly"), Some(&AttributeValue::Bool(true)));
    assert_eq!(node.attrs.get("includehidden"), Some(&AttributeValue::Bool(true)));
    assert_eq!(node.attrs.get("reversed"), Some(&AttributeValue::Bool(true)));
    assert_eq!(node.attrs.get("name"), Some(&AttributeValue::Str("mastertoc".into())));

    assert_eq!(
        node.attrs.get("entry_refs"),
        Some(&AttributeValue::List(vec![
            "foo".to_owned(),
            "bar".to_owned(),
            "sub/*".to_owned(),
            "https://example.org/docs".to_owned(),
            "self".to_owned(),
        ]))
    );
    assert_eq!(
        node.attrs.get("entry_titles"),
        Some(&AttributeValue::List(vec![
            String::new(),
            "A title".to_owned(),
            String::new(),
            String::new(),
            String::new(),
        ]))
    );
    assert_eq!(
        node.attrs.get("entry_kinds"),
        Some(&AttributeValue::List(vec![
            "doc".to_owned(),
            "doc".to_owned(),
            "glob".to_owned(),
            "external".to_owned(),
            "self".to_owned(),
        ]))
    );
    assert_eq!(
        node.attrs.get("includefiles"),
        Some(&AttributeValue::List(vec![
            "foo".to_owned(),
            "bar".to_owned(),
        ]))
    );

    // The directive body starts at byte 15 (".. toctree::"); the span covers
    // the marker through the last entry line, no more and no less.
    let span = node.span.expect("byte span");
    assert_eq!(span.start as usize, SOURCE.find(".. toctree::").expect("marker"));
    assert!(span.end > span.start && span.end as usize <= SOURCE.len());
    assert_eq!(node.source.as_ref().and_then(|source| source.line), Some(4));

    // External and `self` entries are materialised as references with `refuri`.
    let references: Vec<_> = node
        .children
        .iter()
        .filter_map(|&id| doc.get(id).ok())
        .filter(|child| child.kind == NodeKind::Reference)
        .collect();
    assert_eq!(references.len(), 2);
    assert_eq!(references[0].attrs.get("refuri"), Some(&AttributeValue::Str("https://example.org/docs".into())));
    assert_eq!(references[1].attrs.get("refuri"), Some(&AttributeValue::Str(Arc::from(""))));
}
