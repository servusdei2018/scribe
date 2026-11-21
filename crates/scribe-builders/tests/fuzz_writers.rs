//! Phase 69 fuzz target for the HTML5, LaTeX and EPUB writers.
//!
//! Escaping helpers consume untrusted text, so arbitrary Unicode must never
//! panic and must emit bounded, deterministic output. The full writers are
//! driven with generated (well-formed) doctrees via the real RST parser so the
//! parser → AST → writer pipeline is exercised end to end without relying on
//! writer-internal invariants.
//!
//! Defaults to 1024 cases per property; `PROPTEST_CASES` raises it for the
//! Phase 69 multi-crate campaign.

use proptest::prelude::*;
use scribe_ast::Document;
use scribe_builders::html::{test_context, HTML5Translator};
use scribe_builders::latex::{LaTeXTranslator, LatexContext, LatexEngine};
use scribe_builders::i18n::{escape_inline, escape_message, unquote_po};
use scribe_builders::{
    escape_latex, escape_xml, render_nav_xhtml, strip_external_refs, wrap_xhtml, EpubMetadata,
    NavPoint,
};
use scribe_config::Config;
use scribe_core::BuildEnvironment;

fn fuzz_cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(1024)
}

fn unicode_text() -> impl Strategy<Value = String> {
    prop::string::string_regex("[\\p{L}\\p{N}\\p{P}\\p{S} \\n\\t\\\\]{0,80}").unwrap()
}

fn html_context() -> scribe_builders::html::HtmlContext {
    let config = Config::new();
    let env = BuildEnvironment::new(".");
    test_context("index", &config, &env)
}

fn latex_context() -> LatexContext {
    LatexContext {
        docname: "index".to_owned(),
        config: Config::new(),
        engine: LatexEngine::PdfLatex,
        title: "Title".to_owned(),
        author: "Author".to_owned(),
        release: String::new(),
        date: String::new(),
        theme: "manual".to_owned(),
        extra_preamble: String::new(),
        use_sphinx_sty: false,
        numfig: false,
        language: "en".to_owned(),
        logo: None,
    }
}

/// Substitute generated text into a handful of well-formed RST skeletons so
/// the writers see realistic node mixes (titles, paragraphs, lists, literals,
/// xrefs) with hostile text content.
fn generated_source(body: &str, skeleton: u32) -> String {
    let escape_newlines = body.replace('\n', " ");
    match skeleton % 6 {
        0 => format!("Title\n=====\n\n{escape_newlines}\n"),
        1 => format!("Paragraph\n=========\n\nFirst {escape_newlines}.\n\nSecond paragraph.\n"),
        2 => format!("List\n====\n\n- {escape_newlines}\n- second item\n"),
        3 => format!("Code\n====\n\n``{escape_newlines}``\n"),
        4 => format!("Refs\n====\n\n:ref:`{escape_newlines} <target>` and :math:`E=mc^2`.\n"),
        _ => format!("Emphasis\n========\n\n*{escape_newlines}* and **strong**.\n"),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(fuzz_cases()))]

    #[test]
    fn escaping_helpers_never_panic_and_are_deterministic(text in unicode_text()) {
        let html = scribe_builders::html::translator::escape_html(&text);
        prop_assert_eq!(&html, &scribe_builders::html::translator::escape_html(&text));
        let attr = scribe_builders::html::translator::escape_attr(&text);
        prop_assert_eq!(attr, scribe_builders::html::translator::escape_attr(&text));
        let xml = escape_xml(&text);
        prop_assert_eq!(&xml, &escape_xml(&text));
        let inline = escape_inline(&text);
        prop_assert_eq!(inline, escape_inline(&text));
        let message = escape_message(&text);
        prop_assert_eq!(message, escape_message(&text));
        let unquoted = unquote_po(&text);
        prop_assert_eq!(unquoted, unquote_po(&text));

        for engine in [LatexEngine::PdfLatex, LatexEngine::XeLatex, LatexEngine::LuaLatex] {
            let latex = escape_latex(&text, engine);
            prop_assert_eq!(&latex, &escape_latex(&text, engine));
            prop_assert!(latex.len() <= text.len() * 16 + 64, "LaTeX escaping is not linearly bounded");
        }
        prop_assert!(html.len() <= text.len() * 8 + 64, "HTML escaping is not linearly bounded");
        prop_assert!(xml.len() <= text.len() * 8 + 64, "XML escaping is not linearly bounded");
    }

    #[test]
    fn html_and_latex_writers_never_panic_on_generated_documents(
        body in unicode_text(),
        skeleton in 0u32..6,
    ) {
        let source = generated_source(&body, skeleton);
        let doc: Document = scribe_rst::parse(&source).document;

        let ctx = html_context();
        let html = HTML5Translator::translate(&doc, &ctx);
        prop_assert_eq!(html, HTML5Translator::translate(&doc, &ctx), "HTML output not deterministic");

        let lctx = latex_context();
        let tex = LaTeXTranslator::translate(&doc, &lctx);
        prop_assert_eq!(&tex, &LaTeXTranslator::translate(&doc, &lctx), "LaTeX output not deterministic");
        prop_assert!(tex.contains("\\end{document}"), "LaTeX document is incomplete");
    }

    #[test]
    fn epub_helpers_never_panic_and_are_deterministic(
        text in unicode_text(),
        text2 in unicode_text(),
    ) {
        let xhtml = wrap_xhtml(&text, &text2, "en");
        prop_assert_eq!(xhtml, wrap_xhtml(&text, &text2, "en"));
        let stripped = strip_external_refs(&text);
        prop_assert_eq!(&stripped, &strip_external_refs(&text));
        prop_assert!(stripped.len() <= text.len(), "strip_external_refs grew the input");

        let nav = vec![
            NavPoint { text: text.clone(), href: text2.clone(), children: vec![] },
            NavPoint {
                text: "parent".to_owned(),
                href: "chapter.xhtml".to_owned(),
                children: vec![NavPoint { text: text.clone(), href: "#anchor".to_owned(), children: vec![] }],
            },
        ];
        let metadata = EpubMetadata::from_config(&Config::new());
        let nav_xhtml = render_nav_xhtml(&nav, &metadata);
        prop_assert_eq!(&nav_xhtml, &render_nav_xhtml(&nav, &metadata));
        prop_assert!(nav_xhtml.contains("<html"), "EPUB nav is not XHTML");
    }
}
