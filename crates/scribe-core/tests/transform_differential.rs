//! Differential tests for the Phase 10 transform pipeline.
//!
//! Method (documented in ARCHITECTURE.md §14.7):
//!
//! 1. Write a tiny RST fixture plus `conf.py` into a process-unique temp dir.
//! 2. Run the pinned oracle `sphinx-build -b pseudoxml -E -q` and read the
//!    fully transformed doctree.
//! 3. Parse the same source with `scribe-rst`, resolve directives, then apply
//!    our built-in read-phase pipeline and emit pseudo-XML.
//! 4. Normalize the lossy/Sphinx-version-specific attributes listed in
//!    `normalize()` and compare byte-for-byte.
//!
//! Compared subset (three distinct transform areas): smart quotes/dashes/ellipsis
//! education, default substitutions (`version`/`release`/`today`), and footnote
//! auto-numbering with labels/backlinks. Normalized away: the `<document>`
//! attribute list (`source`, `translation_progress`, `xmlns:*`), generated
//! `idN` identifiers, and the `docname` attribute Sphinx adds to references.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use scribe_config::{load_config_with, BridgeStrategy, LoadRequest, Tags};
use scribe_core::{apply_transforms, Phase, TransformContext};
use scribe_rst::{parse_with_options, resolve_output, Context, DirectiveRegistry, ParseOptions};

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
            "scribe-transform-diff-{name}-{}-{sequence}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let source_dir = root.join("src");
        std::fs::create_dir_all(&source_dir).expect("create source dir");
        std::fs::write(source_dir.join("conf.py"), conf).expect("write conf.py");
        std::fs::write(source_dir.join("index.rst"), source).expect("write index.rst");
        let output = root.join("out");
        Self { root, source: source_dir, output }
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

fn oracle_pseudoxml(project: &Project) -> String {
    let binary = workspace().join(".oracle-venv/bin/sphinx-build");
    assert!(binary.is_file(), "oracle binary not found: {}", binary.display());
    let output = Command::new(binary)
        .arg("-b")
        .arg("pseudoxml")
        .arg("-E")
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
    std::fs::read_to_string(project.output.join("index.pseudoxml")).expect("read oracle pseudoxml")
}

fn ours(project: &Project) -> String {
    let request = LoadRequest {
        conf_dir: project.source.clone(),
        strategy: BridgeStrategy::Static,
        ..LoadRequest::default()
    };
    let config = load_config_with(request).expect("load conf.py").0;
    let source = std::fs::read_to_string(project.source.join("index.rst")).expect("read source");
    let mut output = parse_with_options(&source, ParseOptions::new(Some("index".to_owned())));
    let mut context = Context::new(&project.source, Tags::with_tags(["html".to_owned()]));
    context.config = Some(std::sync::Arc::new(config.clone()));
    context.source_name = Some("index".to_owned());
    let report = resolve_output(&mut output, &DirectiveRegistry::builtin(), &context);
    assert!(report.is_clean(), "resolve issues: {:?}", report.unresolved);
    let ctx = TransformContext::new(Some(&config), None, "pseudoxml", "pseudoxml");
    let transform_report = apply_transforms(&mut output.document, &ctx, Phase::Read);
    assert!(!transform_report.has_diagnostics(), "unexpected diagnostics: {:?}", transform_report.diagnostics);
    output.document.to_pseudoxml().expect("emit pseudoxml")
}

/// Normalizes attributes that are outside the compared subset.
fn normalize(pseudoxml: &str) -> String {
    // Generated `idN` values differ only because the two parsers assign ids to
    // notes vs. references in a different order. Rename them by first
    // appearance so cross-links (refid/backrefs) stay structurally comparable.
    let mut mapping: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    let mut next = 1usize;
    let mut out = Vec::new();
    for line in pseudoxml.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("<document") {
            out.push("<document>".to_owned());
            continue;
        }
        // Drop the Sphinx-only `docname` attribute on references/footnotes.
        let mut normalized = remove_attr(line, "docname");
        for key in ["ids", "backrefs", "refid"] {
            normalized = canonicalize_ids(&normalized, key, &mut mapping, &mut next);
        }
        out.push(normalized);
    }
    out.join("\n")
}

fn canonicalize_ids(
    line: &str,
    attribute: &str,
    mapping: &mut std::collections::BTreeMap<String, String>,
    next: &mut usize,
) -> String {
    let needle = format!("{attribute}=\"");
    let mut result = String::new();
    let mut rest = line;
    while let Some(start) = rest.find(&needle) {
        let value_start = start + needle.len();
        let quoted = &rest[value_start..];
        let Some(end) = quoted.find('"') else {
            break;
        };
        let canonical_value = quoted[..end]
            .split(' ')
            .map(|token| {
                if token.len() > 2
                    && token.starts_with("id")
                    && token[2..].chars().all(|ch| ch.is_ascii_digit())
                {
                    mapping
                        .entry(token.to_owned())
                        .or_insert_with(|| {
                            let canonical = format!("id{next}");
                            *next += 1;
                            canonical
                        })
                        .clone()
                } else {
                    token.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        result.push_str(&rest[..value_start]);
        result.push_str(&canonical_value);
        result.push('"');
        rest = &quoted[end + 1..];
    }
    result.push_str(rest);
    result
}

fn remove_attr(line: &str, name: &str) -> String {
    let needle = format!(" {name}=\"");
    let Some(start) = line.find(&needle) else {
        return line.to_owned();
    };
    let value_start = start + needle.len();
    let quoted = &line[value_start..];
    let Some(end) = quoted.find('"') else {
        return line.to_owned();
    };
    let mut result = line[..start].to_owned();
    result.push_str(&quoted[end + 1..]);
    result
}

fn compare(name: &str, source: &str, conf: &str) {
    let project = Project::new(name, source, conf);
    let oracle = normalize(&oracle_pseudoxml(&project));
    let ours = normalize(&ours(&project));
    if std::env::var("SCRIBE_DIFF_DEBUG").is_ok() {
        eprintln!("=== ORACLE ===\n{oracle}\n=== OURS ===\n{ours}");
    }
    assert_eq!(ours, oracle, "pseudoxml mismatch for {name}");
}

const BASE_CONF: &str = "project = 'T'\nmaster_doc = 'index'\n";

#[test]
fn differential_smart_quotes() {
    compare(
        "smartquotes",
        "Title\n=====\n\n\"Hello\" -- world... it's fine.\n",
        BASE_CONF,
    );
}

#[test]
fn differential_default_substitutions() {
    compare(
        "substitutions",
        "Title\n=====\n\n|version| |release| |today|\n",
        "project = 'T'\nmaster_doc = 'index'\nversion = '1.2'\nrelease = '1.2.3'\ntoday = '2026-01-02'\n",
    );
}

#[test]
fn differential_footnotes() {
    compare(
        "footnotes",
        "Title\n=====\n\nA note [1]_ and auto [#]_ and symbol [*]_.\n\n.. [1] Manual.\n.. [#] Auto.\n.. [*] Symbol.\n",
        BASE_CONF,
    );
}

/// Directly compares `OnlyNodeTransform` with upstream
/// `sphinx.util.nodes.process_only_nodes` on the same hand-built tree. The RST
/// parser resolves `only` eagerly, so the post-transform needs an AST that
/// still contains `only` wrappers — built on both sides here.
#[test]
fn differential_only_post_transform() {
    let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "scribe-only-diff-{}-{sequence}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create temp dir");
    let script = root.join("oracle_only.py");
    std::fs::write(
        &script,
        r#"
from docutils import nodes, utils
from sphinx import addnodes
from sphinx.util.nodes import process_only_nodes
from sphinx.util.tags import Tags

doc = utils.new_document('')
section = nodes.section(ids=['title'])
doc += section
title = nodes.title()
title += nodes.Text('Title')
section += title
only_html = addnodes.only(expr='html')
section += only_html
para_kept = nodes.paragraph()
para_kept += nodes.Text('kept')
only_html += para_kept
only_latex = addnodes.only(expr='latex')
section += only_latex
para_hidden = nodes.paragraph()
para_hidden += nodes.Text('hidden')
only_latex += para_hidden
process_only_nodes(doc, Tags(['html']))
print(doc.pformat())
"#,
    )
    .expect("write oracle script");
    let python = workspace().join(".oracle-venv/bin/python");
    let output = Command::new(python)
        .arg(&script)
        .output()
        .expect("run oracle python");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let oracle = String::from_utf8(output.stdout).expect("utf8");
    let oracle = normalize(&oracle).replace(" xml:space=\"preserve\"", "");

    use scribe_ast::{Document, NodeKind};
    use scribe_core::PostTransform;
    let mut doc = Document::new(Some("index.rst".to_owned()));
    let section = doc.add(NodeKind::Section, Some(doc.root)).expect("arena");
    doc.get_mut(section).expect("arena").attrs.set_list("ids", vec!["title".to_owned()]);
    let title = doc.add(NodeKind::Title, Some(section)).expect("arena");
    doc.new_text("Title", Some(title)).expect("arena");
    let only_html = doc.add(NodeKind::Only, Some(section)).expect("arena");
    doc.get_mut(only_html).expect("arena").attrs.set_str("expr", "html");
    let kept = doc.add(NodeKind::Paragraph, Some(only_html)).expect("arena");
    doc.new_text("kept", Some(kept)).expect("arena");
    let only_latex = doc.add(NodeKind::Only, Some(section)).expect("arena");
    doc.get_mut(only_latex).expect("arena").attrs.set_str("expr", "latex");
    let hidden = doc.add(NodeKind::Paragraph, Some(only_latex)).expect("arena");
    doc.new_text("hidden", Some(hidden)).expect("arena");

    let tags = Tags::with_tags(["html".to_owned()]);
    let ctx = TransformContext::new(None, Some(&tags), "html", "html");
    let only = scribe_core::OnlyNodeTransform;
    let result = only.apply(&mut doc, &ctx);
    assert!(result.changed);
    let ours = normalize(&doc.to_pseudoxml().expect("pseudoxml")).replace(" xml:space=\"preserve\"", "");
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(ours.trim_end(), oracle.trim_end(), "only post-transform mismatch");
}
