//! Pseudo-XML snapshot and differential tests against the Sphinx oracle.
//!
//! Two independent oracles are used:
//!
//! 1. `tests/fixtures/gen_pseudoxml.py` builds the exact same tree as
//!    `Document::fixture_document()` and prints `document.pformat()` — the
//!    final code path of `sphinx-build -b pseudoxml`.
//! 2. A real `sphinx-build -b pseudoxml` run on a tiny project; its output is
//!    parsed back into our AST (see `common::parse_pseudoxml`) and re-emitted,
//!    asserting byte-for-byte equality.

mod common;

use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use common::{oracle_python, oracle_sphinx_build, parse_pseudoxml, workspace_root};
use scribe_ast::Document;

/// Exact output captured from:
///
/// ```text
/// cd /home/credo/code/scribe
/// .oracle-venv/bin/python crates/scribe-ast/tests/fixtures/gen_pseudoxml.py
/// ```
const ORACLE_FIXTURE_PSEUDOXML: &str = r#"<document source="test.rst">
    <section ids="a-section" names="a\ section">
        <title>
            A Section
        <paragraph>
            See 
            <pending_xref classes="xref py func" refdoc="index" refdomain="py" refexplicit="0" refid="" refname="StringIO" refspecific="0" reftarget="io.StringIO" reftitle="" reftype="func" refuri="" refwarn="1">
                <literal>
                    StringIO
             done.
        <paragraph>
            This is 
            <emphasis>
                emphasis
             and 
            <literal>
                literal
             and 
            <strong>
                strong
            .
        <bullet_list bullet="-">
            <list_item>
                <paragraph>
                    first item
            <list_item>
                <paragraph>
                    second item
        <note>
            <paragraph>
                This is a note admonition.
"#;

#[test]
fn snapshot_fixture_matches_oracle() {
    let doc = Document::fixture_document();
    assert_eq!(doc.to_pseudoxml().expect("emit"), ORACLE_FIXTURE_PSEUDOXML);
}

#[test]
fn differential_against_python_pformat_script() {
    let python = oracle_python();
    if !python.exists() {
        eprintln!("skipping: oracle python not found at {}", python.display());
        return;
    }
    let script = workspace_root()
        .join("crates")
        .join("scribe-ast")
        .join("tests")
        .join("fixtures")
        .join("gen_pseudoxml.py");
    let output = Command::new(python)
        .arg(&script)
        .output()
        .expect("run oracle script");
    assert!(
        output.status.success(),
        "oracle script failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = String::from_utf8(output.stdout).expect("utf-8 oracle output");
    let actual = Document::fixture_document()
        .to_pseudoxml()
        .expect("emit");
    assert_eq!(actual, expected, "Rust emitter diverged from Docutils pformat");
}

#[test]
fn differential_against_real_sphinx_build() {
    let sphinx_build = oracle_sphinx_build();
    if !sphinx_build.exists() {
        eprintln!("skipping: sphinx-build not found at {}", sphinx_build.display());
        return;
    }

    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let base = std::env::temp_dir().join(format!("scribe-ast-pseudoxml-{unique}"));
    let src = base.join("src");
    let out = base.join("out");
    fs::create_dir_all(&src).expect("create source dir");

    // A tiny extension injects a pending_xref on `doctree-resolved`, i.e. after
    // Sphinx's reference-resolution transforms, so it survives into the
    // pseudo-XML writer exactly like a domain-produced cross reference.
    fs::write(
        src.join("conf.py"),
        concat!(
            "project = 'Scribe AST'\n",
            "extensions = []\n",
            "master_doc = 'index'\n",
            "\n",
            "from docutils import nodes\n",
            "from sphinx import addnodes\n",
            "\n",
            "def inject_xref(app, doctree, docname):\n",
            "    para = nodes.paragraph()\n",
            "    para += nodes.Text('See ')\n",
            "    xr = addnodes.pending_xref(\n",
            "        '', refdomain='py', reftype='func', reftarget='io.StringIO',\n",
            "        refexplicit=False, refwarn=True, refdoc=docname,\n",
            "    )\n",
            "    xr += nodes.literal(text='StringIO')\n",
            "    para += xr\n",
            "    doctree += para\n",
            "\n",
            "def setup(app):\n",
            "    app.connect('doctree-resolved', inject_xref)\n",
        ),
    )
    .expect("write conf.py");
    fs::write(
        src.join("index.rst"),
        concat!(
            "AST Fixture\n",
            "===========\n",
            "\n",
            "This is a paragraph with *emphasis* and ``literal`` text.\n",
            "\n",
            "A bullet list:\n",
            "\n",
            "- first item\n",
            "- second item\n",
            "\n",
            ".. note::\n",
            "\n",
            "   This is a note admonition.\n",
        ),
    )
    .expect("write index.rst");

    let status = Command::new(&sphinx_build)
        .args(["-b", "pseudoxml", "-E", "-q"])
        .arg(&src)
        .arg(&out)
        .output()
        .expect("run sphinx-build");
    assert!(
        status.status.success(),
        "sphinx-build failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );

    let oracle = fs::read_to_string(out.join("index.pseudoxml")).expect("read oracle output");
    assert!(
        oracle.contains("<pending_xref"),
        "fixture must exercise a real, unresolved pending_xref from the oracle"
    );

    // Oracle pseudo-XML -> our AST -> our pseudo-XML must be an exact fixpoint.
    let doc = parse_pseudoxml(&oracle);
    let roundtrip = doc.to_pseudoxml().expect("emit");

    if roundtrip != oracle {
        for (i, (a, b)) in oracle.lines().zip(roundtrip.lines()).enumerate() {
            if a != b {
                panic!(
                    "pseudo-XML mismatch at line {}:\n  oracle: {a:?}\n  scribe: {b:?}",
                    i + 1
                );
            }
        }
        panic!(
            "pseudo-XML line-count mismatch: oracle {} lines, scribe {} lines",
            oracle.lines().count(),
            roundtrip.lines().count()
        );
    }

    fs::remove_dir_all(&base).ok();
}
