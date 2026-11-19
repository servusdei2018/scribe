//! Differential tests against the pinned Sphinx oracle.
//!
//! Method (documented in ARCHITECTURE.md §15.7):
//!
//! 1. Write a tiny RST project + `conf.py` into a process-unique temp dir.
//! 2. Run `.oracle-venv/bin/sphinx-build -b pseudoxml -E -n -q` and capture
//!    both the doctree and the exact warning lines.
//! 3. Parse the same source with `scribe-rst`, register objects in the Phase 11
//!    domain registry and resolve all pending xrefs.
//! 4. Reduce both pseudo-XML trees to *resolution facts* (reference tags with
//!    `refid`/`refuri`/`classes`/`reftitle` plus their inner text) and compare.
//!    Generated ids, document attributes and namespace declarations are
//!    deliberately excluded: they belong to the frozen AST's lossy pseudo-XML
//!    surface, not to the resolution contract.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use scribe_ast::NodeKind;
use scribe_config::{load_config_with, BridgeStrategy, LoadRequest};
use scribe_domains::{resolve_pending_xrefs, DiagnosticSink, DomainContext, DomainRegistry};
use scribe_rst::{parse_with_options, resolve_output, Context, DirectiveRegistry, ParseOptions};

mod common;

use common::TestEnv;

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Project {
    root: PathBuf,
    source: PathBuf,
    output: PathBuf,
}

impl Project {
    fn new(name: &str, source: &str, conf: &str) -> Self {
        let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "scribe-domain-diff-{name}-{}-{sequence}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let source_dir = root.join("src");
        std::fs::create_dir_all(&source_dir).expect("create source dir");
        std::fs::write(source_dir.join("conf.py"), conf).expect("write conf.py");
        std::fs::write(source_dir.join("index.rst"), source).expect("write index.rst");
        if name.contains("numref") {
            // Avoid the oracle's unrelated "image file not readable" warning.
            std::fs::write(source_dir.join("example.png"), []).expect("write image");
        }
        Self { root: root.clone(), source: source_dir, output: root.join("out") }
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

fn oracle(project: &Project) -> (String, Vec<String>) {
    let binary = workspace().join(".oracle-venv/bin/sphinx-build");
    assert!(binary.is_file(), "oracle binary not found: {}", binary.display());
    let output = Command::new(binary)
        .arg("-b")
        .arg("pseudoxml")
        .arg("-E")
        .arg("-n")
        .arg("-q")
        .arg(&project.source)
        .arg(&project.output)
        .output()
        .expect("run oracle sphinx-build");
    assert!(
        output.status.success(),
        "oracle build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let pseudoxml = std::fs::read_to_string(project.output.join("index.pseudoxml"))
        .expect("read oracle pseudoxml");
    let warnings = String::from_utf8_lossy(&output.stderr)
        .lines()
        .filter_map(|line| {
            let message = line.split_once(" WARNING: ").map(|(_, message)| message)?;
            Some(message.split(" [").next().unwrap_or(message).to_owned())
        })
        .collect();
    (pseudoxml, warnings)
}

fn ours(project: &Project) -> (String, Vec<String>, usize, usize) {
    let request = LoadRequest {
        conf_dir: project.source.clone(),
        strategy: BridgeStrategy::Static,
        ..LoadRequest::default()
    };
    let mut config = load_config_with(request).expect("load conf.py").0;
    // The oracle differential runs with `-n`; mirror it in our config.
    config.set("nitpicky", scribe_config::ConfigValue::Bool(true)).expect("set nitpicky");
    let source = std::fs::read_to_string(project.source.join("index.rst")).expect("read source");
    let mut output = parse_with_options(&source, ParseOptions::new(Some("index".to_owned())));
    let mut context = Context::new(&project.source, Default::default());
    context.config = Some(std::sync::Arc::new(config.clone()));
    context.source_name = Some("index".to_owned());
    let report = resolve_output(&mut output, &DirectiveRegistry::builtin(), &context);
    assert!(report.is_clean(), "resolve issues: {:?}", report.unresolved);
    output.document.metadata.docname = Some("index".to_owned());

    // Phase 09 owns figure numbering; simulate its metadata for the :numref:
    // fixture so Phase 11 consumes the same input as the oracle build.
    if project.root.to_string_lossy().contains("numref") {
        for id in output.document.preorder_ids() {
            if output.document.get(id).is_ok_and(|node| node.kind == NodeKind::Figure) {
                if let Ok(node) = output.document.get_mut(id) {
                    if node.get_str("fignumber").ok().flatten().is_none() {
                        node.attrs.set_str("fignumber", "1");
                    }
                }
            }
        }
    }

    let sink = DiagnosticSink::default();
    let registry = DomainRegistry::with_standard();
    // `env.doctree` is read for `:numref:`; resolution itself mutates the
    // document in place, so the snapshot is intentionally taken first.
    let env = TestEnv::default().with_doctree("index", "Title", output.document.clone());
    let ctx = DomainContext::new(Some(&env), Some(&config), "index", "pseudoxml", "pseudoxml", &sink);
    registry.process_document(&ctx, "index", &output.document).expect("process document");
    let resolution = resolve_pending_xrefs(&registry, &ctx, &mut output.document).expect("resolve");
    let warnings = ctx
        .diagnostics()
        .snapshot()
        .into_iter()
        .map(|diagnostic| diagnostic.message)
        .collect();
    let pseudoxml = output.document.to_pseudoxml().expect("emit pseudoxml");
    (pseudoxml, warnings, resolution.resolved, resolution.unresolved)
}

/// Generated anchors differ in letter case (`envvar-HOME` upstream vs
/// `envvar-home` from docutils `make_id` in our parser). The anchor itself is
/// internal to the frozen AST, so canonicalize the known generated prefixes.
fn canonicalize_id(value: &str) -> String {
    for prefix in ["envvar-", "confval-", "term-", "grammar-token-", "cmdoption-", "option-", "index-"] {
        if let Some(rest) = value.strip_prefix(prefix) {
            return format!("{prefix}{}", rest.to_lowercase());
        }
    }
    value.to_owned()
}

/// Reduces pseudo-XML to the resolution-relevant surface.
///
/// Kept: reference tags (including `number_reference` / `download_reference`),
/// their `refid` / `refuri` / `reftitle` / `classes`, and the first text line
/// inside them (the display title). Unresolved inline/literal xrefs keep only
/// their classes and text. Everything else is ignored.
fn resolution_facts(pseudoxml: &str) -> Vec<String> {
    let lines: Vec<&str> = pseudoxml.lines().collect();
    let mut facts = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('<') {
            continue;
        }
        let is_reference = trimmed.starts_with("<reference")
            || trimmed.starts_with("<number_reference")
            || trimmed.starts_with("<download_reference");
        let is_fallback = trimmed.starts_with("<inline classes=\"xref")
            || trimmed.starts_with("<literal classes=\"xref");
        if !is_reference && !is_fallback {
            continue;
        }
        let mut fact = String::new();
        fact.push_str(if is_reference { "ref" } else { "fallback" });
        for key in ["refid", "refuri", "reftitle", "title", "classes", "internal"] {
            let needle = format!("{key}=\"");
            if let Some(start) = trimmed.find(&needle) {
                let rest = &trimmed[start + needle.len()..];
                if let Some(end) = rest.find('"') {
                    let value = canonicalize_id(&rest[..end]);
                    let _ = std::fmt::Write::write_fmt(
                        &mut fact,
                        format_args!(" {key}={value}"),
                    );
                }
            }
        }
        // The display title is the first non-tag line below the reference.
        if let Some(text) = lines[index + 1..].iter().find(|next| {
            let next = next.trim();
            !next.is_empty() && !next.starts_with('<')
        }) {
            let _ = std::fmt::Write::write_fmt(&mut fact, format_args!(" text={}", text.trim()));
        }
        facts.push(fact);
    }
    facts
}

fn check(name: &str, source: &str, conf: &str) {
    let project = Project::new(name, source, conf);
    let (oracle_xml, oracle_warnings) = oracle(&project);
    let (our_xml, our_warnings, resolved, unresolved) = ours(&project);
    let oracle_facts = resolution_facts(&oracle_xml);
    let our_facts = resolution_facts(&our_xml);
    assert_eq!(
        our_warnings, oracle_warnings,
        "warning mismatch\noracle pseudoxml:\n{oracle_xml}\nours:\n{our_xml}"
    );
    assert_eq!(
        our_facts, oracle_facts,
        "resolution mismatch (resolved={resolved}, unresolved={unresolved})\noracle facts: {oracle_facts:#?}\nour facts: {our_facts:#?}\noracle pseudoxml:\n{oracle_xml}\nours:\n{our_xml}"
    );
}

#[test]
fn differential_ref_labels_and_docs() {
    let source = "\
Title
=====

.. _named-label:

A section-like label :ref:`named-label` and a bad :ref:`missing-label`.

:doc:`index` and :doc:`missingdoc`.
";
    check("ref-doc", source, "master_doc = 'index'\nextensions = []\n");
}

#[test]
fn differential_terms_and_options() {
    let source = "\
Title
=====

.. program:: git

.. option:: --verbose

   Be verbose.

Use :option:`--verbose` and :option:`--unknown`.

.. glossary::

   boson
      A particle.

A :term:`boson`, :term:`BOSON`, and :term:`unknown`.
";
    check("term-option", source, "master_doc = 'index'\nextensions = []\n");
}

#[test]
fn differential_numref_with_numbering() {
    let source = "\
Title
=====

.. _my-figure:

.. figure:: example.png

   Caption here

See :numref:`my-figure` and :numref:`missing-figure`.
";
    check(
        "numref",
        source,
        "master_doc = 'index'\nextensions = []\nnumfig = True\n",
    );
}

#[test]
fn differential_envvar_confval_token() {
    let source = "\
Title
=====

.. envvar:: HOME

.. confval:: nitpicky

.. productionlist::
   stmt: \"if\" cond \":\" stmt*

Use :envvar:`HOME`, :envvar:`NOPE`, :confval:`nitpicky`, :token:`stmt`.
";
    check("objects", source, "master_doc = 'index'\nextensions = []\n");
}

#[test]
fn differential_oracle_root_fixture() {
    // Reuse the oracle's own `test-domain-py-xref-warning` root: its two
    // diagnostics are the canonical :ref: missing-label messages.
    let binary = workspace().join(".oracle-venv/bin/sphinx-build");
    assert!(binary.is_file());
    let root = workspace().join("sphinx/tests/roots/test-domain-py-xref-warning");
    let out = std::env::temp_dir().join(format!(
        "scribe-domain-root-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&out);
    let result = Command::new(binary)
        .arg("-b")
        .arg("pseudoxml")
        .arg("-E")
        .arg("-n")
        .arg("-q")
        .arg(&root)
        .arg(&out)
        .output()
        .expect("run oracle on fixture root");
    assert!(result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    let messages: Vec<String> = stderr
        .lines()
        .filter_map(|line| {
            let message = line.split_once(" WARNING: ").map(|(_, message)| message)?;
            Some(message.split(" [").next().unwrap_or(message).to_owned())
        })
        .collect();
    assert_eq!(
        messages,
        [
            "undefined label: 'no-label'".to_owned(),
            "Failed to create a cross reference. A title or caption not found: 'existing-label'".to_owned(),
        ]
    );
    let _ = std::fs::remove_dir_all(&out);
}
