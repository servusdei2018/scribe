//! Public API tests for the Phase 15 highlighting layer.

use scribe_ast::Node;
use scribe_builders::{
    alias_info, available_styles, highlight_code, highlight_many, parse_line_num_spec,
    pygments_css, resolve_language, HighlightOptions,
};

#[test]
fn aliases_cover_the_pygments_spelling_variants() {
    for (input, expected) in [
        ("py", "Python"),
        ("python3", "Python"),
        ("PYTHON", "Python"),
        ("c++", "C++"),
        ("cpp", "C++"),
        ("js", "JavaScript"),
        ("javascript", "JavaScript"),
        ("rb", "Ruby"),
        ("rst", "reStructuredText"),
        ("udiff", "Diff"),
        ("sh", "Shell-Unix-Generic"),
        ("none", "Plain Text"),
    ] {
        assert_eq!(alias_info(input), Some(expected), "alias {input}");
    }
    assert_eq!(alias_info("no-such-language"), None);
    assert_eq!(resolve_language("py").canonical, "python");
    assert!(resolve_language("default").known);
    assert_eq!(resolve_language("default").syntax, "Python");
    assert!(!resolve_language("no-such-language").known);
}

#[test]
fn unknown_language_degrades_to_plain_text_with_warning() {
    let out = highlight_code("x = 1", "not-a-lexer", &HighlightOptions::default());
    assert_eq!(out.plain_text(), "x = 1");
    assert_eq!(out.warnings.len(), 1);
    assert!(out.warnings[0].contains("not-a-lexer"));
    assert!(out.all_classes().iter().all(|class| !class.starts_with("k")));
}

#[test]
fn empty_and_degenerate_inputs_never_panic() {
    assert!(highlight_code("", "python", &HighlightOptions::default()).lines.is_empty());
    assert!(highlight_code("\n", "python", &HighlightOptions::default()).warnings.is_empty());
    assert_eq!(highlight_code("x", "", &HighlightOptions::default()).plain_text(), "x");
    assert!(parse_line_num_spec("10-1", 10).is_err());
    assert!(parse_line_num_spec("x", 10).is_err());
    assert!(parse_line_num_spec("", 10).is_err());
}

#[test]
fn token_classes_are_pygments_compatible() {
    let out = highlight_code(
        "# a comment\nx = 'str'\nn = 42\ndef f(n):\n    return True\n",
        "python",
        &HighlightOptions::default(),
    );
    let classes = out.all_classes();
    for expected in ["c1", "s1", "k", "nf", "kc", "mi"] {
        assert!(classes.iter().any(|class| class == expected), "missing {expected}: {classes:?}");
    }
    assert!(out.pygments_html().contains("<span class=\"c1\">"));
    assert!(out.pygments_html().starts_with("<div class=\"highlight-python notranslate\">"));
}

#[test]
fn line_specs_match_sphinx() {
    assert_eq!(parse_line_num_spec("1,2,4-6", 10).unwrap(), vec![0, 1, 3, 4, 5]);
    assert_eq!(parse_line_num_spec("-10", 20).unwrap(), (0..10).collect::<Vec<_>>());
    assert_eq!(parse_line_num_spec("10-", 10).unwrap(), vec![9]);
    // Open ranges are clamped to `total`, not to the start.
    assert_eq!(parse_line_num_spec("29-", 13).unwrap(), vec![28]);
}

#[test]
fn line_numbers_and_emphasis_are_rendered() {
    let options = HighlightOptions {
        linenos: true,
        lineno_start: 7,
        emphasize_lines: vec![2],
        ..HighlightOptions::default()
    };
    let out = highlight_code("a\nb\nc", "python", &options);
    assert_eq!(out.lines[0].line_number, Some(7));
    assert_eq!(out.lines[2].line_number, Some(9));
    assert_eq!(out.lines[1].line_classes, vec!["hll"]);
    let html = out.pygments_html();
    assert!(html.contains("<span class=\"linenos\">7</span>"));
    assert!(html.contains("<span class=\"hll\">"));
}

#[test]
fn diff_uses_generic_classes() {
    let diff = "--- old\n+++ new\n@@ -1 +1 @@\n-a\n+b\n c\n";
    let out = highlight_code(diff, "udiff", &HighlightOptions::default());
    let classes = out.all_classes();
    for expected in ["gd", "gi", "gu"] {
        assert!(classes.iter().any(|class| class == expected), "missing {expected}");
    }
}

#[test]
fn css_is_deterministic_and_covers_advertised_styles() {
    assert!(!available_styles().is_empty());
    for style in available_styles() {
        let first = pygments_css(style);
        let second = pygments_css(style);
        assert_eq!(first, second);
        assert!(first.contains(".highlight"), "style {style}");
        // The `none` style intentionally emits no token-colour rules.
        if *style != "none" {
            assert!(first.contains(".highlight .k "), "style {style}");
        }
    }
    assert_eq!(pygments_css(""), pygments_css("sphinx"));
    assert_eq!(pygments_css("BOGUS"), pygments_css("sphinx"));
}

#[test]
fn payload_survives_an_ast_attribute_round_trip() {
    let out = highlight_code("def f():\n    pass\n", "python", &HighlightOptions::default());
    let mut node = Node::new(scribe_ast::NodeKind::LiteralBlock);
    out.attach_to_node(&mut node);
    let decoded = scribe_builders::HighlightedCode::from_node(&node)
        .expect("attribute present")
        .expect("valid payload");
    assert_eq!(decoded, out);
    assert_eq!(decoded.pygments_html(), out.pygments_html());
}

#[test]
fn parallel_highlighting_preserves_order() {
    let blocks = vec![
        ("a = 1".to_owned(), "python".to_owned(), HighlightOptions::default()),
        ("def f(): pass".to_owned(), "python".to_owned(), HighlightOptions::default()),
        ("SELECT 1;".to_owned(), "sql".to_owned(), HighlightOptions::default()),
        ("".to_owned(), "text".to_owned(), HighlightOptions::default()),
    ];
    let outputs = highlight_many(&blocks);
    assert_eq!(outputs.len(), blocks.len());
    for (output, block) in outputs.iter().zip(blocks.iter()) {
        assert_eq!(output.plain_text(), block.0);
    }
}
