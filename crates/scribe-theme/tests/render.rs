//! Structural render tests for the vendored built-in themes.

use scribe_theme::{RenderContext, ThemeManager};

fn manager() -> ThemeManager {
    ThemeManager::new()
}

#[test]
fn renders_basic_page_without_missing_globals_or_filters() {
    let ctx = RenderContext::new("index")
        .with_theme("basic")
        .add_doc("index");
    let html = manager()
        .render_template("page.html", &ctx)
        .expect("basic/page.html must render");

    assert!(html.contains("<!DOCTYPE html>"), "{html}");
    assert!(html.contains("<head>"), "{html}");
    assert!(html.contains("rel=\"stylesheet\""), "{html}");
    assert!(html.contains("class=\"sphinxsidebar\""), "{html}");
    assert!(html.contains("class=\"document\""), "{html}");
    assert!(html.contains("class=\"related\""), "{html}");
    assert!(html.contains("</body>"), "{html}");
    // The page body is injected through the `body` block of page.html.
    assert!(html.contains("class=\"body\" role=\"main\""), "{html}");
}

#[test]
fn renders_alabaster_page_through_its_basic_parent() {
    let mut ctx = RenderContext::new("index").with_theme("alabaster").add_doc("index");
    ctx.title = "Welcome".to_owned();
    ctx.body = "<p>Hello from the translator.</p>".to_owned();
    ctx.toc = "<ul><li>Topic</li></ul>".to_owned();
    let html = manager()
        .render_template("page.html", &ctx)
        .expect("alabaster must render basic/page.html with its own layout");

    assert!(html.contains("<!DOCTYPE html>"), "{html}");
    assert!(html.contains("<head>"), "{html}");
    assert!(html.contains("rel=\"stylesheet\""), "{html}");
    assert!(html.contains("href=\"_static/custom.css\""), "{html}");
    assert!(html.contains("sphinxsidebar"), "{html}");
    assert!(html.contains("Hello from the translator."), "{html}");
    assert!(html.contains("Powered by"), "{html}");
    assert!(html.contains("alabaster"), "{html}");
    assert!(html.contains("</html>"), "{html}");
}

#[test]
fn renders_classic_and_nature_layouts() {
    for theme in ["classic", "nature"] {
        let ctx = RenderContext::new("index").with_theme(theme).add_doc("index");
        let html = manager()
            .render_template("page.html", &ctx)
            .unwrap_or_else(|error| panic!("{theme} page must render: {error}"));
        assert!(html.contains("<!DOCTYPE html>"), "{theme}: {html}");
        assert!(html.contains("<head>"), "{theme}: {html}");
        assert!(html.contains("rel=\"stylesheet\""), "{theme}: {html}");
    }
}

#[test]
fn renders_css_jinja_templates_without_html_escaping() {
    let theme = manager().get_theme("basic").expect("basic theme");
    let env = manager().environment(&theme).expect("environment");
    let ctx = RenderContext::new("index").with_theme("basic");
    let css = env
        .render("basic.css.jinja", &ctx)
        .expect("basic.css.jinja must render");
    assert!(css.contains("div.body"), "{css}");
    assert!(!css.contains("&lt;"), "CSS must not be HTML escaped: {css}");
}

#[test]
fn missing_template_is_a_typed_error() {
    let ctx = RenderContext::new("index").with_theme("basic");
    let error = manager()
        .render_template("does-not-exist.html", &ctx)
        .expect_err("missing template must fail");
    assert_eq!(error.code(), "scribe::theme::template_not_found");
}
