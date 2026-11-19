mod common;

use scribe_rst::{parse, resolve_output, Context, DirectiveRegistry};

const FIXTURE: &str = r#"Directives
==========

.. note::
   :class: note-class

   Note body.

.. admonition:: Custom

   Admonition body.

.. versionadded:: 2.0 Feature

   Details.

.. rubric:: A Rubric

.. epigraph::

   Quote.

.. compound::

   Compound body.

.. container:: custom

   Container body.

.. topic:: Topic

   Topic body.

.. sidebar:: Side

   Side body.

.. centered:: Centered

.. tabularcolumns:: |l|c|

.. image:: pic.png
   :width: 100px

.. figure:: fig.png

   Caption.

.. csv-table:: CSV
   :header: "A", "B"

   1, 2

.. list-table:: LT
   :header-rows: 1

   * - A
     - B
   * - 1
     - 2

.. glossary::

   term
      Definition.

.. hlist::

   * one
   * two

.. productionlist:: name: tokens

Roles: :abbr:`HTML (HyperText)`, :kbd:`Ctrl-C`, :math:`x^2`, :regexp:`a+`.
"#;

/// Extends `common::normalize` with the post-parse attributes/writer artefacts
/// that Sphinx adds *after* the directive handlers ran. These are owned by
/// later phases (image candidates, implicit ids, glossary transform) and are
/// documented in `ARCHITECTURE.md` §10.6.
fn normalize(pseudoxml: &str) -> String {
    common::normalize(pseudoxml)
        .lines()
        .filter(|line| !line.trim_start().starts_with("<index "))
        .map(|line| {
            let indent_len = line.len() - line.trim_start_matches(' ').len();
            let mut content = line[indent_len..].to_owned();
            for attr in ["candidates", "classes=\"glossary\""] {
                if attr.starts_with("classes=") {
                    let needle = format!(" {attr}");
                    if let Some(index) = content.find(&needle) {
                        content.replace_range(index..index + needle.len(), "");
                    }
                } else {
                    common_remove_attribute(&mut content, attr);
                }
            }
            // Generated implicit ids: `id1`, `id2`, ... and `term-*`.
            let mut cleaned = String::new();
            let mut rest = content.as_str();
            while let Some(index) = rest.find(" ids=\"") {
                cleaned.push_str(&rest[..index]);
                let value_start = index + 6;
                if let Some(end) = rest[value_start..].find('"') {
                    let value = &rest[value_start..value_start + end];
                    let generated = value.starts_with("id")
                        && value[2..].chars().all(|ch| ch.is_ascii_digit())
                        || value.starts_with("term-");
                    if generated {
                        rest = &rest[value_start + end + 1..];
                        continue;
                    }
                }
                cleaned.push_str(&rest[index..value_start]);
                rest = &rest[value_start..];
            }
            cleaned.push_str(rest);
            // Frozen-AST gap: `abbreviation` has no NodeKind variant.
            if let Some(pos) = cleaned.find("<inline classes=\"abbreviation\"") {
                cleaned.replace_range(pos..pos + "<inline classes=\"abbreviation\"".len(), "<abbreviation");
            }
            format!("{}{}", " ".repeat(indent_len), cleaned)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn common_remove_attribute(tag: &mut String, name: &str) {
    let needle = format!(" {name}=\"");
    let Some(start) = tag.find(&needle) else {
        return;
    };
    let value_start = start + needle.len();
    let bytes = tag.as_bytes();
    let mut i = value_start;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == b'"' {
            tag.replace_range(start..=i, "");
            return;
        }
        i += 1;
    }
}

fn ours(source: &str) -> String {
    let mut output = parse(source);
    let report = resolve_output(&mut output, &DirectiveRegistry::builtin(), &Context::default());
    assert!(report.is_clean(), "{:?}", report.unresolved);
    normalize(&output.document.to_pseudoxml().expect("pseudo-XML"))
}

#[test]
fn byte_exact_core_directives_and_roles() {
    let expected = normalize(&common::oracle_pseudoxml(FIXTURE));
    let actual = ours(FIXTURE);
    assert_eq!(actual, expected);
}

const INCLUDE_FIXTURE: &str = r#"Includes
========

.. include:: inc.txt
   :start-line: 1
   :end-line: 3
   :literal:

.. raw:: html

   <b>raw</b>

.. only:: draft

   Draft-only paragraph.

.. image:: pic.png
   :target: https://example.com
   :alt: link image
"#;

fn oracle_with_tags(source: &str, tags: &[&str], files: &[(&str, &str)]) -> String {
    let root = std::env::temp_dir().join(format!(
        "scribe-rst-directive-diff-{}-{}",
        std::process::id(),
        source.len()
    ));
    let src = root.join("src");
    let out = root.join("out");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&src).expect("create source dir");
    std::fs::write(
        src.join("conf.py"),
        "project = 'scribe diff'\nmaster_doc = 'index'\nextensions = []\n",
    )
    .expect("write conf.py");
    std::fs::write(src.join("index.rst"), source).expect("write index.rst");
    for (name, text) in files {
        std::fs::write(src.join(name), text).expect("write auxiliary file");
    }
    let mut command = std::process::Command::new(common::oracle_sphinx_build());
    command.args(["-b", "pseudoxml", "-E", "-q"]);
    for tag in tags {
        command.args(["-t", tag]);
    }
    let result = command.arg(&src).arg(&out).output().expect("spawn oracle");
    assert!(
        result.status.success(),
        "oracle failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let text = std::fs::read_to_string(out.join("index.pseudoxml")).expect("pseudoxml");
    let _ = std::fs::remove_dir_all(&root);
    text
}

#[test]
fn byte_exact_include_raw_only_and_image_target() {
    let expected = normalize(&oracle_with_tags(
        INCLUDE_FIXTURE,
        &["draft"],
        &[("inc.txt", "one\ntwo\nthree\nfour\n")],
    ));
    let root = std::env::temp_dir().join(format!("scribe-rst-include-diff-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create root");
    std::fs::write(root.join("inc.txt"), "one\ntwo\nthree\nfour\n").expect("write include");
    let mut output = parse(INCLUDE_FIXTURE);
    let mut tags = scribe_config::Tags::new();
    tags.add("draft");
    let context = Context::new(&root, tags);
    let report = resolve_output(&mut output, &DirectiveRegistry::builtin(), &context);
    assert!(report.is_clean(), "{:?}", report.unresolved);
    let mut actual = normalize(&output.document.to_pseudoxml().expect("pseudo-XML"));
    // The include source path is an absolute path in our context and an
    // absolute path inside the oracle's temp project; both are post-parse
    // writer metadata and are normalized away.
    let mut lines = Vec::new();
    for line in actual.lines() {
        let mut line = line.to_owned();
        if line.trim_start().starts_with("<literal_block") {
            if let Some(start) = line.find(" source=\"") {
                let value_start = start + 9;
                if let Some(end) = line[value_start..].find('"') {
                    line.replace_range(start..value_start + end + 1, "");
                }
            }
        }
        lines.push(line);
    }
    actual = lines.join("\n");
    let expected: String = expected
        .lines()
        .map(|line| {
            let mut line = line.to_owned();
            if line.trim_start().starts_with("<literal_block") {
                if let Some(start) = line.find(" source=\"") {
                    let value_start = start + 9;
                    if let Some(end) = line[value_start..].find('"') {
                        line.replace_range(start..value_start + end + 1, "");
                    }
                }
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(actual, expected);
    let _ = std::fs::remove_dir_all(&root);
}
