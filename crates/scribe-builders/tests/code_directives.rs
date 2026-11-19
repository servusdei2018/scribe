//! Directive-level tests for `code-block`, `literalinclude` and `highlight`.

mod common;

use std::collections::BTreeMap;

use scribe_ast::NodeKind;
use scribe_builders::register_code_directives;
use scribe_rst::DirectiveRegistry;

use common::parse_with_files;

#[test]
fn registration_replaces_placeholders_and_adds_highlight() {
    let mut registry = DirectiveRegistry::builtin();
    let info = register_code_directives(&mut registry);
    assert_eq!(info.block_names.len(), 3);
    for name in info.names() {
        assert!(registry.contains(name), "missing directive {name}");
    }
}

#[test]
fn code_block_emits_tokens_and_plain_text() {
    let parsed = common::parse(
        "Title\n=====\n\n.. code-block:: python\n\n   def f():\n       return 1\n",
    );
    assert!(parsed.issues().is_empty(), "{:?}", parsed.issues());
    let blocks = parsed.highlighted();
    assert_eq!(blocks.len(), 1);
    let (id, payload) = &blocks[0];
    assert_eq!(payload.language, "python");
    assert_eq!(payload.plain_text(), "def f():\n    return 1");
    assert_eq!(parsed.document.get(*id).expect("node").kind, NodeKind::LiteralBlock);
    let classes = payload.all_classes();
    assert!(classes.iter().any(|class| class == "k"), "{classes:?}");
    assert!(classes.iter().any(|class| class == "nf"), "{classes:?}");
}

#[test]
fn lininos_start_and_emphasis_ranges() {
    let parsed = common::parse(
        ".. code-block:: python\n   :linenos:\n   :lineno-start: 200\n   :emphasize-lines: 1,3-4\n\n   a\n   b\n   c\n   d\n",
    );
    assert!(parsed.issues().is_empty(), "{:?}", parsed.issues());
    let (_, payload) = &parsed.highlighted()[0];
    assert!(payload.options.linenos);
    assert_eq!(payload.options.lineno_start, 200);
    assert_eq!(payload.options.emphasize_lines, vec![1, 3, 4]);
    assert_eq!(payload.lines[0].line_number, Some(200));
    assert_eq!(payload.lines[1].line_classes, Vec::<String>::new());
    assert_eq!(payload.lines[2].line_classes, vec!["hll"]);
    assert_eq!(payload.lines[3].line_classes, vec!["hll"]);
}

#[test]
fn emphasis_out_of_range_warns_but_keeps_the_block() {
    let parsed = common::parse(
        ".. code-block:: python\n   :emphasize-lines: 1-99\n\n   a\n   b\n",
    );
    let blocks = parsed.highlighted();
    assert_eq!(blocks.len(), 1);
    assert!(blocks[0].1.warnings.iter().any(|warning| warning.contains("out of range(1-2)")));
}

#[test]
fn caption_wraps_in_a_container() {
    let parsed = common::parse(
        ".. code-block:: python\n   :caption: A *nice* caption\n   :name: my-block\n\n   x = 1\n",
    );
    assert!(parsed.issues().is_empty(), "{:?}", parsed.issues());
    let containers: Vec<_> = parsed
        .document
        .preorder_ids()
        .into_iter()
        .filter(|&id| parsed.document.get(id).is_ok_and(|node| node.kind == NodeKind::Container))
        .collect();
    assert_eq!(containers.len(), 1);
    let container = parsed.document.get(containers[0]).expect("container");
    assert!(container.attrs.get_list("classes").ok().flatten().unwrap_or_default().contains(&"literal-block-wrapper".to_owned()));
    assert!(container.children.iter().any(|&child| {
        parsed.document.get(child).is_ok_and(|node| node.kind == NodeKind::Caption)
    }));
    assert!(container.children.iter().any(|&child| {
        parsed.document.get(child).is_ok_and(|node| node.kind == NodeKind::LiteralBlock)
    }));
    assert_eq!(parsed.highlighted().len(), 1);
}

#[test]
fn dedent_auto_and_explicit() {
    let parsed = common::parse(
        ".. code-block::\n   :dedent:\n\n       first\n           second\n",
    );
    assert!(parsed.issues().is_empty(), "{:?}", parsed.issues());
    assert_eq!(parsed.code_texts()[0], "first\n    second");

    let parsed = common::parse(
        ".. code-block::\n   :dedent: 2\n\n     abc\n     def\n",
    );
    assert_eq!(parsed.code_texts()[0], "abc\ndef");
}

#[test]
fn dedent_stripping_non_whitespace_warns() {
    let parsed = common::parse(
        ".. code-block::\n   :dedent: 4\n\n   abc\n   def\n",
    );
    let blocks = parsed.highlighted();
    assert_eq!(blocks.len(), 1);
    assert!(blocks[0].1.warnings.iter().any(|w| w == "non-whitespace stripped by dedent"));
}

#[test]
fn highlight_directive_sets_language_and_threshold() {
    let parsed = common::parse(
        ".. highlight:: ruby\n   :linenothreshold: 3\n\n.. code-block::\n\n   a\n   b\n   c\n",
    );
    assert!(parsed.issues().is_empty(), "{:?}", parsed.issues());
    assert_eq!(parsed.count(NodeKind::Highlightlang), 1);
    let (_, payload) = &parsed.highlighted()[0];
    assert_eq!(payload.language, "ruby");
    // Three lines meet the threshold of 3 even without :linenos:.
    assert!(payload.options.linenos);
}

#[test]
fn unknown_language_warns_and_uses_plain_text() {
    let parsed = common::parse(".. code-block:: no-such-language\n\n   abc\n");
    let blocks = parsed.highlighted();
    assert_eq!(blocks.len(), 1);
    assert!(blocks[0].1.warnings.iter().any(|w| w.contains("no-such-language")));
    assert_eq!(blocks[0].1.plain_text(), "abc");
}

#[test]
fn force_option_is_preserved() {
    let parsed = common::parse(
        ".. code-block:: python\n   :force:\n\n   not python!\n",
    );
    let (_, payload) = &parsed.highlighted()[0];
    assert!(payload.options.force);
    assert!(parsed.document.get(parsed.highlighted()[0].0).expect("node").attrs.get_bool("force").ok().flatten().unwrap_or(false));
}

#[test]
fn literalinclude_reads_files_and_selects_lines() {
    let mut files = BTreeMap::new();
    files.insert("sample.py".to_owned(), "one\ntwo\nthree\nfour\nfive\n".to_owned());
    let parsed = parse_with_files(
        ".. literalinclude:: sample.py\n   :language: python\n   :lines: 2-4\n   :linenos:\n   :lineno-match:\n",
        files,
    );
    assert!(parsed.issues().is_empty(), "{:?}", parsed.issues());
    let (_, payload) = &parsed.highlighted()[0];
    assert_eq!(payload.plain_text(), "two\nthree\nfour");
    assert_eq!(payload.options.lineno_start, 2);
    assert!(payload.options.lineno_match);
    assert!(payload.options.linenos);
}

#[test]
fn literalinclude_without_caption_has_no_wrapper_container() {
    let mut files = BTreeMap::new();
    files.insert("sample.py".to_owned(), "x = 1\n".to_owned());
    let parsed = parse_with_files(
        ".. literalinclude:: sample.py\n   :language: python\n",
        files,
    );
    assert!(parsed.issues().is_empty(), "{:?}", parsed.issues());
    assert_eq!(parsed.count(NodeKind::Container), 0);
    assert_eq!(parsed.highlighted().len(), 1);
}

#[test]
fn literalinclude_start_after_and_end_before() {
    let mut files = BTreeMap::new();
    files.insert(
        "sample.py".to_owned(),
        "header\nstart marker\nkeep me\nend marker\nfooter\n".to_owned(),
    );
    let parsed = parse_with_files(
        ".. literalinclude:: sample.py\n   :start-after: start marker\n   :end-before: end marker\n",
        files,
    );
    assert!(parsed.issues().is_empty(), "{:?}", parsed.issues());
    assert_eq!(parsed.code_texts()[0], "keep me");
}

#[test]
fn literalinclude_prepend_append_and_tab_width() {
    let mut files = BTreeMap::new();
    files.insert("sample.py".to_owned(), "a\tb\n".to_owned());
    let parsed = parse_with_files(
        ".. literalinclude:: sample.py\n   :tab-width: 4\n   :prepend: BEFORE\n   :append: AFTER\n",
        files,
    );
    assert!(parsed.issues().is_empty(), "{:?}", parsed.issues());
    assert_eq!(parsed.code_texts()[0], "BEFORE\na   b\nAFTER");
}

#[test]
fn literalinclude_pyobject_selects_a_block() {
    let mut files = BTreeMap::new();
    files.insert(
        "sample.py".to_owned(),
        "class Foo:\n    def bar(self):\n        return 1\n\ndef other():\n    pass\n".to_owned(),
    );
    let parsed = parse_with_files(
        ".. literalinclude:: sample.py\n   :pyobject: Foo.bar\n   :lineno-match:\n",
        files,
    );
    assert!(parsed.issues().is_empty(), "{:?}", parsed.issues());
    let (_, payload) = &parsed.highlighted()[0];
    assert!(payload.plain_text().contains("def bar"));
    assert_eq!(payload.options.lineno_start, 2);
}

#[test]
fn literalinclude_diff_uses_udiff_classes() {
    let mut files = BTreeMap::new();
    files.insert("new.py".to_owned(), "a\nb\n".to_owned());
    files.insert("old.py".to_owned(), "a\nc\n".to_owned());
    let parsed = parse_with_files(
        ".. literalinclude:: new.py\n   :diff: old.py\n",
        files,
    );
    assert!(parsed.issues().is_empty(), "{:?}", parsed.issues());
    let (_, payload) = &parsed.highlighted()[0];
    assert_eq!(payload.language, "udiff");
    let classes = payload.all_classes();
    for expected in ["gd", "gi"] {
        assert!(classes.iter().any(|class| class == expected), "missing {expected}: {classes:?}");
    }
}

#[test]
fn missing_include_file_is_a_diagnostic_not_a_panic() {
    let parsed = parse_with_files(".. literalinclude:: missing.py\n", BTreeMap::new());
    assert!(!parsed.issues().is_empty());
    assert!(parsed.issues()[0].contains("cannot read included file") || parsed.issues()[0].contains("IncludeIo") || parsed.issues()[0].contains("reading it failed") || parsed.issues()[0].contains("missing.py"));
    assert!(parsed.highlighted().is_empty());
}

#[test]
fn invalid_option_pairs_are_rejected() {
    let parsed = common::parse(
        ".. code-block:: python\n   :linenos:\n   :bogus: 1\n\n   x\n",
    );
    assert!(!parsed.issues().is_empty());
    assert!(parsed.issues()[0].contains("unknown option") || parsed.issues()[0].contains("bogus"));
}

#[test]
fn empty_code_block_is_tolerated() {
    let parsed = common::parse(".. code-block:: python\n\n");
    // An empty body may be reported by the block parser, but nothing panics.
    let _ = parsed.highlighted();
    let _ = parsed.issues();
}

#[test]
fn multiple_blocks_are_all_highlighted() {
    let parsed = common::parse(
        "A\n=\n\n.. code-block:: python\n\n   x = 1\n\n.. code-block:: rust\n\n   fn main() {}\n\n.. code-block:: ruby\n\n   def f; end\n",
    );
    assert!(parsed.issues().is_empty(), "{:?}", parsed.issues());
    let blocks = parsed.highlighted();
    assert_eq!(blocks.len(), 3);
    assert_eq!(blocks[0].1.language, "python");
    assert_eq!(blocks[1].1.language, "rust");
    assert_eq!(blocks[2].1.language, "ruby");
}
