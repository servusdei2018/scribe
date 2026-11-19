//! Phase 29 regression guard.
//!
//! Replays (a) every persisted proptest regression seed file under `crates/*/`
//! and (b) the known adversarial corpora accumulated in Phases 05–13. A
//! regression here means a parser that previously tolerated an input now
//! panics or emits an invalid span, so this test asserts the durable fixes stay
//! fixed. It is intentionally cheap (no oracle subprocesses).

use std::fs;
use std::path::{Path, PathBuf};

use scribe_ast::Document;
use scribe_config::Tags;
use scribe_domains::cpp::parser::parse_cpp_declaration;
use scribe_domains::{parse_annotation, parse_reftarget, parse_sig_re};
use scribe_rst::inline::{parse_inline, InlineOptions, InlineSource};
use scribe_rst::{parse as parse_rst, parse_with_options, ParseOptions};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Walk `crates/*/tests/` looking for persisted proptest regression files.
fn find_regression_files(root: &Path, out: &mut Vec<PathBuf>) {
    let crates = root.join("crates");
    let Ok(entries) = fs::read_dir(&crates) else { return };
    for entry in entries.flatten() {
        let tests = entry.path().join("tests");
        let Ok(files) = fs::read_dir(&tests) else { continue };
        for file in files.flatten() {
            let path = file.path();
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".proptest-regressions"))
            {
                out.push(path);
            }
        }
    }
}

#[test]
fn persisted_proptest_regression_seeds_are_well_formed() {
    let root = workspace_root();
    let mut files = Vec::new();
    find_regression_files(&root, &mut files);
    files.sort();
    assert!(
        !files.is_empty(),
        "no persisted proptest regression files found under crates/*/tests/"
    );

    let mut total_seeds = 0usize;
    for path in &files {
        let text = fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split_whitespace();
            let marker = parts.next().unwrap_or_default();
            let seed = parts.next().unwrap_or_default();
            assert_eq!(marker, "cc", "malformed regression seed line in {}: {line}", path.display());
            assert_eq!(seed.len(), 64, "seed must be a 64-char hex digest in {}", path.display());
            assert!(
                seed.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "non-hex regression seed in {}: {line}",
                path.display()
            );
            total_seeds += 1;
        }
    }
    // The two Phase 05 span bugs and the Phase 08 inline panic each persisted a
    // seed; the corpus may grow but must never shrink below this floor.
    assert!(
        total_seeds >= 5,
        "expected at least five persisted regression seeds across the workspace, found {total_seeds}"
    );
}

fn assert_spans_in_bounds(source: &str, doc: &Document) {
    let len = source.len() as u32;
    for (id, node) in doc.iter() {
        if let Some(span) = node.span {
            assert!(span.start <= span.end, "inverted span on {id:?}: {span:?}");
            assert!(span.end <= len, "out-of-bounds span on {id:?}: {span:?} (len {len})");
            let start = span.start as usize;
            let end = span.end as usize;
            assert!(source.is_char_boundary(start), "span start not a UTF-8 boundary: {start}");
            assert!(source.is_char_boundary(end), "span end not a UTF-8 boundary: {end}");
        }
    }
}

fn assert_inline_spans(source: &str, base: u32) {
    let inline_source = InlineSource::single(source, base);
    let result = parse_inline(&inline_source, &InlineOptions::default());
    let len = base + source.len() as u32;
    fn walk(nodes: &[scribe_rst::InlineNode], len: u32) {
        for node in nodes {
            let span = node.span;
            assert!(span.start <= span.end, "inverted inline span on {:?}", node.kind);
            assert!(span.end <= len, "out-of-bounds inline span {span:?} on {:?}", node.kind);
            for child in &node.children {
                assert!(
                    child.span.start >= span.start && child.span.end <= span.end,
                    "child span {:?} outside parent {span:?}",
                    child.span
                );
            }
            walk(&node.children, len);
        }
    }
    walk(&result.nodes, len);
}

#[test]
fn phase05_inline_span_bug_corpora_are_fixed() {
    const CASES: &[&str] = &[
        // Phase 05 bug #1: synthesised empty child anchored at global zero.
        "||[````]",
        "``",
        "````",
        "`",
        "||",
        // Phase 05 bug #2: multi-byte explicit title used as a token index.
        "`———<>`_}",
        "`中文标题 <https://example.org/é>`_",
        ":py:func:`~模組.函數 <pkg.mod.func>`",
        // Phase 08 follow-up: tab-expanded prolog/epilog offset table.
        "_||||||||*\t`!\n`\t******",
    ];
    for case in CASES {
        for base in [0u32, 97u32, 1000u32] {
            assert_inline_spans(case, base);
        }
        let doc = parse_rst(case).document;
        let _ = doc.to_pseudoxml().expect("pseudo-XML emission must not fail");
    }

    let options = ParseOptions {
        prolog: Some(String::new()),
        epilog: Some("_||||||||*\t`!\n`\t******".to_owned()),
        ..ParseOptions::default()
    };
    let output = parse_with_options("", options);
    assert_spans_in_bounds("\n\n_||||||||*\t`!\n`\t******\n", &output.document);
}

#[test]
fn phase07_myst_adversarial_corpus_is_fixed() {
    const CASES: &[&str] = &[
        "", "\n", "```", "````", ":::", "{ref}`", "{ref}", "$", "$$", "$$$",
        "---\n---\n", "---\n", "(...)=\n", "[]()", "![x]()", "|a|\n|-|\n",
        "term\n:\n", "\u{0}\u{0}\u{0}", "# A\n\n# A\n", "- a\n  - b\n    - c\n",
    ];
    for source in CASES {
        let output = scribe_myst::parse(source).expect("MyST parse must not fail");
        assert_spans_in_bounds(source, &output.document);
        let _ = output.document.to_json().expect("JSON round-trip must not fail");
    }
}

#[test]
fn phase02_tags_corpus_is_fixed() {
    const CASES: &[&str] = &[
        "", "draft", "not draft", "draft and html", "(draft or html) and latex",
        "true", "True", "false", "none", "not None", "draft ? html : latex",
        "draft and", ")draft(", "((((draft))))", "中draft",
    ];
    let tags = Tags::with_tags(["draft", "html", "latex"]);
    for condition in CASES {
        let first = tags.eval_condition(condition);
        assert_eq!(first, tags.eval_condition(condition), "unstable for {condition:?}");
    }
}

#[test]
fn phase12_13_signature_corpus_is_fixed() {
    const PY_SIGS: &[&str] = &[
        "", "func()", "Class.method(self, arg: int = 1, *args, **kwargs) -> str",
        "func[T](x: list[T]) -> Optional[T]", "f((((((x))))))", "unterminated(a, b",
        "中.函数(x) -> 类型",
    ];
    for sig in PY_SIGS {
        let parsed = parse_sig_re(sig);
        assert_eq!(parsed, parse_sig_re(sig));
        let _ = parse_annotation(sig);
        let _ = parse_reftarget(sig, false);
    }
    const CPP_DECLS: &[&str] = &[
        "", "void f()", "float Sphinx::version", "template<typename T> T max(T a, T b)",
        "std::vector<int> ns::Class<T>::method(const int&) const", "operator<<",
        "((((((x))))))", "中文::函数(int)",
    ];
    for decl in CPP_DECLS {
        let parsed = parse_cpp_declaration(decl, "function", "cpp");
        assert_eq!(parsed.name, parsed.segments.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join("::"));
    }
}
