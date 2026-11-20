//! Unit tests for the Phase 17 HTML5 translator (one test per node family).
//!
//! Every test constructs a tiny frozen doctree through the public AST API,
//! renders it with [`HTML5Translator::translate`] and asserts the exact tags,
//! CSS classes, attributes and nesting Sphinx's `html5.py` writer produces.

use scribe_ast::{Document, Node, NodeId, NodeKind};
use scribe_builders::highlight::{highlight_code, HighlightOptions};
use scribe_builders::html::{test_context, HtmlContext, HTML5Translator};
use scribe_config::Config;
use scribe_core::BuildEnvironment;

fn ctx() -> HtmlContext {
    let mut config = Config::new();
    let _ = config.set_explicit("language", scribe_config::ConfigValue::from("en"));
    let env = BuildEnvironment::new(".");
    test_context("index", &config, &env)
}

fn render(build: impl FnOnce(&mut Document)) -> String {
    let mut doc = Document::new(Some("index".to_owned()));
    doc.metadata.docname = Some("index".to_owned());
    build(&mut doc);
    let context = ctx();
    HTML5Translator::translate(&doc, &context)
}

fn text(doc: &mut Document, parent: NodeId, value: &str) -> NodeId {
    doc.new_text(value, Some(parent)).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn empty_document_does_not_panic() {
    let out = render(|_| {});
    assert!(out.is_empty() || out.trim().is_empty());
}

#[test]
fn section_title_and_headerlink() {
    let out = render(|doc| {
        let section = doc.add(NodeKind::Section, Some(doc.root)).unwrap();
        doc.get_mut(section).unwrap().set_list("ids", vec!["intro".into()]);
        let title = doc.add(NodeKind::Title, Some(section)).unwrap();
        text(doc, title, "Introduction");
        let para = doc.add(NodeKind::Paragraph, Some(section)).unwrap();
        text(doc, para, "Body text.");
    });
    assert!(out.contains("<section id=\"intro\">"), "{out}");
    assert!(out.contains("<h1>Introduction<a class=\"headerlink\" href=\"#intro\" title=\"Link to this heading\">¶</a></h1>"), "{out}");
    assert!(out.contains("<p>Body text.</p>"));
    assert!(out.contains("</section>"));
}

#[test]
fn nested_sections_use_h2() {
    let out = render(|doc| {
        let outer = doc.add(NodeKind::Section, Some(doc.root)).unwrap();
        doc.get_mut(outer).unwrap().set_list("ids", vec!["outer".into()]);
        let inner = doc.add(NodeKind::Section, Some(outer)).unwrap();
        doc.get_mut(inner).unwrap().set_list("ids", vec!["inner".into()]);
        let title = doc.add(NodeKind::Title, Some(inner)).unwrap();
        text(doc, title, "Nested");
    });
    assert!(out.contains("<h2>Nested"), "{out}");
}

#[test]
fn inline_emphasis_strong_literal_and_classes() {
    let out = render(|doc| {
        let para = doc.add(NodeKind::Paragraph, Some(doc.root)).unwrap();
        let em = doc.add(NodeKind::Emphasis, Some(para)).unwrap();
        text(doc, em, "em");
        let strong = doc.add(NodeKind::Strong, Some(para)).unwrap();
        text(doc, strong, "strong");
        let lit = doc.add(NodeKind::Literal, Some(para)).unwrap();
        text(doc, lit, "code");
        let inline = doc.add(NodeKind::Inline, Some(para)).unwrap();
        doc.get_mut(inline).unwrap().set_list("classes", vec!["guilabel".into()]);
        text(doc, inline, "GUI");
    });
    assert!(out.contains("<em>em</em>"), "{out}");
    assert!(out.contains("<strong>strong</strong>"), "{out}");
    assert!(out.contains("<code class=\"docutils literal notranslate\"><span class=\"pre\">code</span></code>"), "{out}");
    assert!(out.contains("<span class=\"guilabel\">GUI</span>"), "{out}");
}

#[test]
fn references_internal_and_external() {
    let out = render(|doc| {
        let para = doc.add(NodeKind::Paragraph, Some(doc.root)).unwrap();
        let ext = Node::new(NodeKind::Reference)
            .with_attr("refuri", "https://example.com".into())
            .with_attr("internal", false.into());
        let ext_id = doc.add_node(ext, Some(para)).unwrap();
        text(doc, ext_id, "external");
        let internal = Node::new(NodeKind::Reference)
            .with_attr("refid", "target".into())
            .with_attr("internal", true.into());
        let internal_id = doc.add_node(internal, Some(para)).unwrap();
        text(doc, internal_id, "internal");
    });
    assert!(out.contains("<a class=\"reference external\" href=\"https://example.com\">external</a>"), "{out}");
    assert!(out.contains("<a class=\"reference internal\" href=\"#target\">internal</a>"), "{out}");
}

#[test]
fn highlighted_literal_block_uses_phase_15_payload() {
    let out = render(|doc| {
        let block = doc.add(NodeKind::LiteralBlock, Some(doc.root)).unwrap();
        let highlighted = highlight_code("def f():\n    pass\n", "python", &HighlightOptions::default());
        highlighted.attach_to_node(doc.get_mut(block).unwrap());
    });
    assert!(out.contains("<div class=\"highlight-python notranslate\">"), "{out}");
    assert!(out.contains("<div class=\"highlight\"><pre><span></span>"), "{out}");
    assert!(out.contains("</pre></div>"), "{out}");
}

#[test]
fn lists_get_simple_class() {
    let out = render(|doc| {
        let list = doc.add(NodeKind::BulletList, Some(doc.root)).unwrap();
        let item = doc.add(NodeKind::ListItem, Some(list)).unwrap();
        let para = doc.add(NodeKind::Paragraph, Some(item)).unwrap();
        text(doc, para, "one");
        let ordered = doc.add(NodeKind::EnumeratedList, Some(doc.root)).unwrap();
        doc.get_mut(ordered).unwrap().attrs.set_int("start", 3);
        let item2 = doc.add(NodeKind::ListItem, Some(ordered)).unwrap();
        let para2 = doc.add(NodeKind::Paragraph, Some(item2)).unwrap();
        text(doc, para2, "three");
    });
    assert!(out.contains("<ul class=\"simple\">"), "{out}");
    assert!(out.contains("<ol class=\"simple\" start=\"3\">"), "{out}");
    assert!(out.matches("<li>").count() == 2, "{out}");
}

#[test]
fn definition_and_field_lists() {
    let out = render(|doc| {
        let dl = doc.add(NodeKind::DefinitionList, Some(doc.root)).unwrap();
        let item = doc.add(NodeKind::DefinitionListItem, Some(dl)).unwrap();
        let term = doc.add(NodeKind::Term, Some(item)).unwrap();
        text(doc, term, "term");
        let def = doc.add(NodeKind::Definition, Some(item)).unwrap();
        let para = doc.add(NodeKind::Paragraph, Some(def)).unwrap();
        text(doc, para, "definition");

        let fields = doc.add(NodeKind::FieldList, Some(doc.root)).unwrap();
        let field = doc.add(NodeKind::Field, Some(fields)).unwrap();
        let name = doc.add(NodeKind::FieldName, Some(field)).unwrap();
        text(doc, name, "author");
        let body = doc.add(NodeKind::FieldBody, Some(field)).unwrap();
        let p = doc.add(NodeKind::Paragraph, Some(body)).unwrap();
        text(doc, p, "me");
    });
    assert!(out.contains("<dl>"), "{out}");
    assert!(out.contains("<dt>term</dt>"), "{out}");
    assert!(out.contains("<dd>"), "{out}");
    assert!(out.contains("<dt>author</dt>"), "{out}");
}

#[test]
fn specific_admonitions_and_seealso() {
    let out = render(|doc| {
        for kind in [NodeKind::Note, NodeKind::Warning, NodeKind::Seealso] {
            let node = doc.add(kind, Some(doc.root)).unwrap();
            let para = doc.add(NodeKind::Paragraph, Some(node)).unwrap();
            text(doc, para, "body");
        }
    });
    assert!(out.contains("<div class=\"admonition note\">\n<p class=\"admonition-title\">Note</p>"), "{out}");
    assert!(out.contains("<div class=\"admonition warning\">\n<p class=\"admonition-title\">Warning</p>"), "{out}");
    assert!(out.contains("<div class=\"admonition seealso\">\n<p class=\"admonition-title\">See also</p>"), "{out}");
}

#[test]
fn generic_admonition_uses_title_child() {
    let out = render(|doc| {
        let node = doc.add(NodeKind::Admonition, Some(doc.root)).unwrap();
        doc.get_mut(node).unwrap().set_list("classes", vec!["admonition-custom".into()]);
        let title = doc.add(NodeKind::Title, Some(node)).unwrap();
        text(doc, title, "Custom");
        let para = doc.add(NodeKind::Paragraph, Some(node)).unwrap();
        text(doc, para, "body");
    });
    assert!(out.contains("<div class=\"admonition-custom admonition\">"), "{out}");
    assert!(out.contains("<p class=\"admonition-title\">Custom</p>"), "{out}");
}

#[test]
fn versionmodified_is_inline() {
    let out = render(|doc| {
        let para = doc.add(NodeKind::Paragraph, Some(doc.root)).unwrap();
        let modified = doc.add(NodeKind::VersionModified, Some(para)).unwrap();
        doc.get_mut(modified).unwrap().set_str("type", "added");
        doc.get_mut(modified).unwrap().set_str("version", "1.0");
        let inline = doc.add(NodeKind::Inline, Some(modified)).unwrap();
        doc.get_mut(inline).unwrap().set_list("classes", vec!["versionmodified".into(), "added".into()]);
        text(doc, inline, "New in version 1.0: ");
    });
    assert!(out.contains("<span class=\"versionmodified added\">New in version 1.0: </span>"), "{out}");
    assert!(!out.contains("<div class=\"admonition"), "{out}");
}

#[test]
fn image_and_figure() {
    let out = render(|doc| {
        let image = doc.add(NodeKind::Image, Some(doc.root)).unwrap();
        doc.get_mut(image).unwrap().set_str("uri", "pic.png");
        doc.get_mut(image).unwrap().set_str("align", "right");
        let figure = doc.add(NodeKind::Figure, Some(doc.root)).unwrap();
        let img2 = doc.add(NodeKind::Image, Some(figure)).unwrap();
        doc.get_mut(img2).unwrap().set_str("uri", "fig.png");
        let caption = doc.add(NodeKind::Caption, Some(figure)).unwrap();
        text(doc, caption, "A caption");
    });
    assert!(out.contains("<img src=\"../pic.png\" alt=\"\" class=\"align-right\" />"), "{out}");
    assert!(out.contains("<figure>"), "{out}");
    assert!(out.contains("<figcaption>"), "{out}");
    assert!(out.contains("A caption"), "{out}");
}

#[test]
fn footnotes_and_citations() {
    let out = render(|doc| {
        let footnote = doc.add(NodeKind::Footnote, Some(doc.root)).unwrap();
        doc.get_mut(footnote).unwrap().set_list("ids", vec!["fn1".into()]);
        let label = doc.add(NodeKind::Label, Some(footnote)).unwrap();
        text(doc, label, "1");
        let para = doc.add(NodeKind::Paragraph, Some(footnote)).unwrap();
        text(doc, para, "note body");
        let reference = doc.add(NodeKind::FootnoteReference, Some(doc.root)).unwrap();
        doc.get_mut(reference).unwrap().set_str("refid", "fn1");
        text(doc, reference, "1");
    });
    assert!(out.contains("class=\"footnote-list brackets\""), "{out}");
    assert!(out.contains("id=\"fn1\" class=\"footnote brackets\" role=\"doc-footnote\""), "{out}");
    assert!(
        out.contains("<a class=\"footnote-reference brackets\" role=\"doc-noteref\" href=\"#fn1\"><span class=\"fn-bracket\">[</span>1<span class=\"fn-bracket\">]</span></a>"),
        "{out}"
    );
}

#[test]
fn math_inline_and_block() {
    let out = render(|doc| {
        let para = doc.add(NodeKind::Paragraph, Some(doc.root)).unwrap();
        let math = doc.add(NodeKind::Math, Some(para)).unwrap();
        text(doc, math, "x^2");
        let block = doc.add(NodeKind::MathBlock, Some(doc.root)).unwrap();
        text(doc, block, "E = mc^2");
    });
    assert!(out.contains("\\(x^2\\)"), "{out}");
    assert!(out.contains("\\["), "{out}");
    assert!(out.contains("E = mc^2"), "{out}");
}

#[test]
fn raw_html_passes_through_but_other_formats_do_not() {
    let out = render(|doc| {
        let raw = Node::new(NodeKind::Raw)
            .with_attr("format", "html".into());
        let id = doc.add_node(raw, Some(doc.root)).unwrap();
        text(doc, id, "<b>bold</b>");
        let latex = Node::new(NodeKind::Raw)
            .with_attr("format", "latex".into());
        let id2 = doc.add_node(latex, Some(doc.root)).unwrap();
        text(doc, id2, "\\LaTeX");
    });
    assert!(out.contains("<b>bold</b>"), "{out}");
    assert!(!out.contains("\\LaTeX"), "{out}");
}

#[test]
fn toctree_renders_nested_entries() {
    let out = render(|doc| {
        let node = Node::new(NodeKind::Toctree)
            .with_attr("entries", "index other".into())
            .with_attr("entry_kinds", "doc external".into())
            .with_attr("entry_refs", "index https://example.com".into())
            .with_attr("entry_titles", "Home External".into())
            .with_attr("caption", "Contents".into());
        let _ = doc.add_node(node, Some(doc.root)).unwrap();
    });
    assert!(out.contains("<p class=\"caption\"><span class=\"caption-text\">Contents</span></p>"), "{out}");
    assert!(out.contains("<a class=\"reference internal\" href=\"#\">Home</a>"), "{out}");
    assert!(out.contains("<a class=\"reference external\" href=\"https://example.com\">External</a>"), "{out}");
}

#[test]
fn desc_signature_uses_sig_object_classes() {
    let out = render(|doc| {
        let desc = doc.add(NodeKind::Desc, Some(doc.root)).unwrap();
        doc.get_mut(desc).unwrap().set_list("classes", vec!["py".into(), "function".into()]);
        let sig = doc.add(NodeKind::DescSignature, Some(desc)).unwrap();
        let name = doc.add(NodeKind::DescName, Some(sig)).unwrap();
        text(doc, name, "my_func");
        let content = doc.add(NodeKind::DescContent, Some(desc)).unwrap();
        let para = doc.add(NodeKind::Paragraph, Some(content)).unwrap();
        text(doc, para, "docs");
    });
    assert!(out.contains("sig sig-object"), "{out}");
    assert!(out.contains("<dt class=\"sig sig-object\">"), "{out}");
    assert!(out.contains("my_func"), "{out}");
    assert!(out.contains("<dd>"), "{out}");
}

#[test]
fn target_comment_and_system_message() {
    let out = render(|doc| {
        let target = Node::new(NodeKind::Target).with_attr("refid", "anchor".into());
        let _ = doc.add_node(target, Some(doc.root)).unwrap();
        let comment = doc.add(NodeKind::Comment, Some(doc.root)).unwrap();
        text(doc, comment, "hidden");
        let system = doc.add(NodeKind::SystemMessage, Some(doc.root)).unwrap();
        let para = doc.add(NodeKind::Paragraph, Some(system)).unwrap();
        text(doc, para, "warning");
    });
    assert!(out.contains("<span class=\"target\" id=\"anchor\"></span>"), "{out}");
    assert!(!out.contains("hidden"), "{out}");
    assert!(out.contains("<div class=\"system-message\">"), "{out}");
}

#[test]
fn unresolved_xref_degrades_to_content() {
    let out = render(|doc| {
        let para = doc.add(NodeKind::Paragraph, Some(doc.root)).unwrap();
        let xref = doc.add(NodeKind::PendingXref, Some(para)).unwrap();
        text(doc, xref, "some target");
    });
    assert!(out.contains("some target"), "{out}");
    assert!(!out.contains("pending_xref"), "{out}");
}
