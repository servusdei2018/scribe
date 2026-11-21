//! Behavior tests for Sphinx's Jinja2 globals and filters.

use std::collections::BTreeMap;

use minijinja::Value;
use scribe_theme::{RenderContext, TemplateEnvironment, ThemeManager};

fn render(source: &str, ctx: &RenderContext) -> String {
    let theme = ThemeManager::new().get_theme("basic").expect("basic");
    let env = TemplateEnvironment::with_search_paths(&theme, Vec::new()).expect("env");
    env.render_string("expr.html", source, ctx)
        .unwrap_or_else(|error| panic!("render failed for `{source}`: {error}"))
}

fn render_text(source: &str, ctx: &RenderContext) -> String {
    let theme = ThemeManager::new().get_theme("basic").expect("basic");
    let env = TemplateEnvironment::with_search_paths(&theme, Vec::new()).expect("env");
    env.render_string("expr.txt", source, ctx)
        .unwrap_or_else(|error| panic!("render failed for `{source}`: {error}"))
}

fn context() -> RenderContext {
    let mut ctx = RenderContext::new("index")
        .with_theme("basic")
        .add_doc("index")
        .add_doc("other");
    ctx.title = "A <b>Title</b>".to_owned();
    ctx
}

#[test]
fn pathto_matches_sphinx_relative_uri() {
    let ctx = context();
    assert_eq!(render("{{ pathto('other') }}", &ctx), "other.html");
    assert_eq!(render("{{ pathto('index') }}", &ctx), "#");
    assert_eq!(render("{{ pathto('_static/x.css', 1) }}", &ctx), "_static/x.css");
    assert_eq!(
        render("{{ pathto('https://cdn/x.css', resource=True) }}", &ctx),
        "https://cdn/x.css"
    );

    let mut nested = RenderContext::new("guide/index").with_theme("basic").add_doc("other");
    nested.link_suffix = ".html".to_owned();
    assert_eq!(render("{{ pathto('other') }}", &nested), "../other.html");
    assert_eq!(render("{{ pathto('guide/index') }}", &nested), "#");

    let mut sharp = context();
    sharp.allow_sharp_as_current_path = false;
    assert_eq!(render("{{ pathto('index') }}", &sharp), "index.html");
}

#[test]
fn hasdoc_knows_generated_pages() {
    let ctx = context();
    assert_eq!(render("{{ hasdoc('index') }}", &ctx), "True");
    assert_eq!(render("{{ hasdoc('missing') }}", &ctx), "False");
    assert_eq!(render("{{ hasdoc('search') }}", &ctx), "True");
    assert_eq!(render("{{ hasdoc('genindex') }}", &ctx), "True");
    let mut no_generated = context();
    no_generated.search_enabled = false;
    no_generated.use_index = false;
    assert_eq!(render("{{ hasdoc('search') }}", &no_generated), "False");
    assert_eq!(render("{{ hasdoc('genindex') }}", &no_generated), "False");
}

#[test]
fn css_tag_and_js_tag_emit_html() {
    let mut ctx = context();
    ctx.set_var("css", RenderContext::css_file("_static/site.css"));
    ctx.set_var("js", RenderContext::js_file("_static/app.js"));
    let css = render("{{ css_tag(css) }}", &ctx);
    assert!(css.starts_with("<link "), "{css}");
    assert!(css.contains("href=\"_static/site.css\""), "{css}");
    let js = render("{{ js_tag(js) }}", &ctx);
    assert_eq!(js, "<script src=\"_static/app.js\"></script>");
    assert_eq!(
        render("{{ js_tag('_static/app.js') }}", &ctx),
        "<script src=\"_static/app.js\"></script>"
    );

    ctx.set_var("inline", Value::from(BTreeMap::from([
        ("body".to_owned(), Value::from("var x = 1;")),
    ])));
    assert_eq!(
        render("{{ js_tag(inline) }}", &ctx),
        "<script>var x = 1;</script>"
    );
}

#[test]
fn tobool_toint_todim_filters() {
    let ctx = context();
    assert_eq!(render("{{ 'YES'|tobool }}", &ctx), "True");
    assert_eq!(render("{{ 'off'|tobool }}", &ctx), "False");
    assert_eq!(render("{{ 'yes' is string }}", &ctx), "True");
    assert_eq!(render("{{ '42'|toint + 1 }}", &ctx), "43");
    assert_eq!(render("{{ 'nope'|toint }}", &ctx), "0");
    assert_eq!(render("{{ 0|todim }} / {{ 12|todim }}", &ctx), "0 / 12px");
    assert_eq!(render("{{ '940px'|todim }}", &ctx), "940px");
    assert_eq!(render_text("{{ none|todim }}", &ctx), "initial");
}

#[test]
fn slice_index_splits_index_entries() {
    let mut ctx = context();
    let entries = vec![
        Value::from(vec![Value::from("a"), Value::from(1)]),
        Value::from(vec![Value::from("b"), Value::from(2)]),
        Value::from(vec![Value::from("c"), Value::from(3)]),
    ];
    ctx.set_var("entries", Value::from(entries));
    assert_eq!(render("{{ entries|slice_index(2)|length }}", &ctx), "2");
    assert_eq!(render("{{ (entries|slice_index(2))[0]|length }}", &ctx), "1");
    assert_eq!(render("{{ (entries|slice_index(2))[1]|length }}", &ctx), "2");
}

#[test]
fn accesskey_emits_each_key_once() {
    let ctx = context();
    assert_eq!(
        render("{{ accesskey('a') }}|{{ accesskey('a') }}|{{ accesskey('B') }}", &ctx),
        "accesskey=\"a\"||accesskey=\"B\""
    );
    // Each render starts with a fresh access-key set.
    assert_eq!(render("{{ accesskey('a') }}", &ctx), "accesskey=\"a\"");
}

#[test]
fn idgen_object_has_current_and_next() {
    let ctx = context();
    let rendered = render(
        "{% set g = idgen() %}{{ g.current() }}{{ g.next() }}{{ g.current() }}{{ g.next() }}",
        &ctx,
    );
    assert_eq!(rendered, "0112");
}

#[test]
fn get_theme_option_and_translation() {
    let mut options = BTreeMap::new();
    options.insert("sidebarwidth".to_owned(), "260".to_owned());
    let mut ctx = context().with_theme_options(options);
    std::sync::Arc::make_mut(&mut ctx.translations).insert("Hello".to_owned(), "Bonjour".to_owned());

    assert_eq!(render("{{ get_theme_option('sidebarwidth') }}", &ctx), "260");
    assert_eq!(render("{{ get_theme_option('missing', 'fallback') }}", &ctx), "fallback");
    assert_eq!(render("{{ _('Hello') }}", &ctx), "Bonjour");
    assert_eq!(render("{{ gettext('Goodbye') }}", &ctx), "Goodbye");
}

#[test]
fn no_autoescape_and_common_filters() {
    let ctx = context();
    // Sphinx builds its Jinja environment without autoescape: plain values and
    // literal MarkupSafe entities pass through verbatim; `|e` escapes explicitly.
    assert_eq!(render("{{ '<b>x</b>' }}", &ctx), "<b>x</b>");
    assert_eq!(render("{{ '&#8212;' }}", &ctx), "&#8212;");
    assert_eq!(render("{{ '<b>'|safe }}", &ctx), "<b>");
    assert_eq!(render("{{ '<b>x</b>'|striptags }}", &ctx), "x");
    assert_eq!(render("{{ '<b>x</b>'|e }}", &ctx), "&lt;b&gt;x&lt;/b&gt;");
    assert_eq!(render_text("{{ '<b>x</b>' }}", &ctx), "<b>x</b>");
    assert_eq!(render_text("{{ 'a b'|urlencode }}", &ctx), "a%20b");
    assert_eq!(render_text("{{ {'b': 1, 'a': 2}|tojson }}", &ctx), "{\"a\":2,\"b\":1}");
}

#[test]
fn sidebar_relbar_and_toctree_globals_return_safe_markup() {
    let mut ctx = context();
    ctx.sidebar_html = "<div id='side'>S</div>".to_owned();
    ctx.relbar_html = "<div id='rel'>R</div>".to_owned();
    ctx.toctree_html = "<ul><li>T</li></ul>".to_owned();
    assert_eq!(render("{{ sidebar() }}", &ctx), "<div id='side'>S</div>");
    assert_eq!(render("{{ relbar() }}", &ctx), "<div id='rel'>R</div>");
    assert_eq!(
        render("{{ toctree(includehidden=true) }}", &ctx),
        "<ul><li>T</li></ul>"
    );
}
