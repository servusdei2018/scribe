//! Phase 34 HTML5 parity tests for `title_reference`, `classifier` and
//! `option_group`. Sources are parsed by the real RST parser and rendered by
//! the same `HTML5Translator` the HTML builder uses, so these assertions cover
//! the full parser → AST → writer pipeline. Expected fragments are copied from
//! `sphinx-build -b html -D html_theme=basic` on the same source.

use scribe_ast::Document;
use scribe_builders::html::{test_context, HtmlContext, HTML5Translator};
use scribe_config::Config;
use scribe_core::BuildEnvironment;

fn context() -> HtmlContext {
    let mut config = Config::new();
    let _ = config.set_explicit("language", scribe_config::ConfigValue::from("en"));
    let env = BuildEnvironment::new(".");
    test_context("index", &config, &env)
}

fn render(source: &str) -> String {
    let output = scribe_rst::parse(source);
    assert!(!output.has_diagnostics(), "diagnostics: {:?}", output.diagnostics);
    let doc: Document = output.document;
    let ctx = context();
    HTML5Translator::translate(&doc, &ctx)
}

#[test]
fn title_reference_renders_cite() {
    let out = render("Title\n=====\n\nDefault `interpreted text` here.\n");
    assert!(
        out.contains("<p>Default <cite>interpreted text</cite> here.</p>"),
        "{out}"
    );
    assert!(!out.contains("title-reference"), "{out}");
}

#[test]
fn classifier_renders_spans_inside_dt() {
    let out = render(
        "Terms\n=====\n\nterm : classifier one : classifier two\n    Definition body.\n",
    );
    assert!(
        out.contains(
            "<dt>term<span class=\"classifier\">classifier one</span><span class=\"classifier\">classifier two</span></dt>"
        ),
        "{out}"
    );
    assert!(out.contains("<dd><p>Definition body.</p>"), "{out}");
}

#[test]
fn option_group_renders_kbd_and_var() {
    let out = render(
        "Opts\n====\n\n-a, --all <file>  Description of all.\n-b             Other option.\n",
    );
    assert!(
        out.contains(
            "<dt><kbd><span class=\"option\">-a</span>, <span class=\"option\">--all <var>&lt;file&gt;</var></span></kbd></dt>"
        ),
        "{out}"
    );
    assert!(
        out.contains("<dt><kbd><span class=\"option\">-b</span></kbd></dt>"),
        "{out}"
    );
}
