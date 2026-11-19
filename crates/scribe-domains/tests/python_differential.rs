//! Differential tests for the Python domain against the pinned Sphinx oracle.
//!
//! Method (documented in ARCHITECTURE.md §16):
//!
//! 1. Copy a real `sphinx/tests/roots/test-domain-py*` fixture to a temp dir.
//! 2. Run `.oracle-venv/bin/sphinx-build -b pseudoxml -E -n -q` and collect the
//!    doctrees plus exact `WARNING:` messages.
//! 3. Parse the same `.rst` files with `scribe-rst`, expand Python signatures,
//!    register objects in `DomainRegistry::builtin()`, resolve pending xrefs.
//! 4. Reduce both trees to (a) `desc_signature` structure and (b) reference
//!    resolution facts, then compare; warnings are compared verbatim.
//!
//! Normalized away (internal/frozen-AST surface): `py:*` skeleton attributes,
//! `_toc_*` hints, `xml:space`, generated ids (canonicalized to their object
//! key), `multi_line*` flags, document attributes and namespace declarations.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use scribe_config::{load_config_with, BridgeStrategy, LoadRequest};
use scribe_domains::{prepare_document, resolve_pending_xrefs, DiagnosticSink, DomainContext, DomainRegistry};
use scribe_rst::{parse_with_options, resolve_output, Context, DirectiveRegistry, ParseOptions};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

struct Tree {
    tag: String,
    attrs: BTreeMap<String, String>,
    text: String,
    children: Vec<Tree>,
}

fn parse_attrs(line: &str) -> (String, BTreeMap<String, String>) {
    let trimmed = line.trim_start();
    let body = trimmed.strip_prefix('<').unwrap_or(trimmed).trim_end_matches('>');
    let mut it = body.splitn(2, char::is_whitespace);
    let tag = it.next().unwrap_or("").to_owned();
    let mut attrs = BTreeMap::new();
    if let Some(rest) = it.next() {
        // Attribute values are double-quoted and may contain escaped spaces.
        let bytes = rest.as_bytes();
        let mut i = 0usize;
        while i < bytes.len() {
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            let start = i;
            while i < bytes.len() && bytes[i] != b'=' && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= bytes.len() || bytes[i] != b'=' {
                break;
            }
            let key = rest[start..i].to_owned();
            i += 1;
            if i >= bytes.len() || bytes[i] != b'"' {
                continue;
            }
            i += 1;
            let value_start = i;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if bytes[i] == b'"' {
                    break;
                }
                i += 1;
            }
            let value = rest[value_start..i.min(rest.len())].replace("\\ ", " ");
            attrs.insert(key, value);
            i += 1;
        }
    }
    (tag, attrs)
}

fn parse_pseudoxml(xml: &str) -> Vec<Tree> {
    let mut roots: Vec<Tree> = Vec::new();
    // (indent, index path into the tree) stack; we rebuild using recursive insertion.
    let mut stack: Vec<(usize, Vec<usize>)> = Vec::new();
    for line in xml.lines() {
        if line.trim().is_empty() {
            continue;
        }
        // Text nodes may themselves begin with a space, so round the leading
        // whitespace down to the 4-column indentation grid.
        let raw_indent = line.len() - line.trim_start_matches(' ').len();
        let indent = (raw_indent / 4) * 4;
        let trimmed = line.trim_start();
        if trimmed.starts_with('<') {
            let (tag, attrs) = parse_attrs(trimmed);
            while stack.last().is_some_and(|(level, _)| *level >= indent) {
                stack.pop();
            }
            let node = Tree { tag, attrs, text: String::new(), children: Vec::new() };
            if let Some((_, path)) = stack.last() {
                insert_at(&mut roots, path, node);
            } else {
                roots.push(node);
            }
            let mut path = match stack.last() {
                Some((_, path)) => path.clone(),
                None => vec![roots.len() - 1],
            };
            if stack.last().is_some() {
                let parent_len = get_at(&roots, &path).map(|node| node.children.len()).unwrap_or(0);
                path.push(parent_len.saturating_sub(1));
            }
            stack.push((indent, path));
        } else {
            // A text line belongs to the deepest element whose indentation is
            // strictly smaller than the text's; pop deeper (sibling) elements.
            while stack.last().is_some_and(|(level, _)| *level >= indent) {
                stack.pop();
            }
            if let Some((_, path)) = stack.last() {
                if let Some(node) = get_mut_at(&mut roots, path) {
                    if !node.text.is_empty() {
                        node.text.push(' ');
                    }
                    node.text.push_str(trimmed);
                }
            }
        }
    }
    roots
}

fn get_at<'a>(nodes: &'a [Tree], path: &[usize]) -> Option<&'a Tree> {
    let mut current = nodes.get(*path.first()?)?;
    for index in &path[1..] {
        current = current.children.get(*index)?;
    }
    Some(current)
}

fn get_mut_at<'a>(nodes: &'a mut [Tree], path: &[usize]) -> Option<&'a mut Tree> {
    let mut current = nodes.get_mut(*path.first()?)?;
    for index in &path[1..] {
        current = current.children.get_mut(*index)?;
    }
    Some(current)
}

fn insert_at(nodes: &mut Vec<Tree>, path: &[usize], node: Tree) {
    if let Some(parent) = get_mut_at(nodes, path) {
        parent.children.push(node);
    } else {
        nodes.push(node);
    }
}

fn collect<'a>(nodes: &'a [Tree], tag: &str, out: &mut Vec<&'a Tree>) {
    for node in nodes {
        if node.tag == tag {
            out.push(node);
        }
        collect(&node.children, tag, out);
    }
}

fn canonical_id(value: &str) -> String {
    let lower = value.to_lowercase();
    // Upstream's pseudoxml builder returns bare docnames; the frozen
    // `DomainContext` synthesizes a `.pseudoxml` suffix for unknown formats.
    lower.replace(".pseudoxml", "")
}

fn normalize_attrs(node: &Tree) -> Vec<String> {
    const KEPT: &[&str] = &[
        "refid", "refuri", "reftitle", "title", "classes", "fullname", "class", "module",
    ];
    let mut out = Vec::new();
    for (key, value) in &node.attrs {
        if key.starts_with("py:") || key.starts_with("_toc_") || key == "xml:space"
            || key.starts_with("multi_line") || key == "entries" || key == "source"
        {
            continue;
        }
        if !KEPT.contains(&key.as_str()) {
            continue;
        }
        let value = if key == "refid" || key == "refuri" || key == "ids" {
            canonical_id(value)
        } else if key == "module" && value == "True" {
            // Docutils renders a `None` module attribute as the string "True";
            // the frozen AST uses the empty string.
            String::new()
        } else {
            value.clone()
        };
        out.push(format!("{key}={value}"));
    }
    out.sort();
    out
}

fn normalize_node(node: &Tree) -> String {
    let attrs = normalize_attrs(node);
    let text = node.text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut children: Vec<String> = node.children.iter().map(normalize_node).collect();
    children.retain(|child| !child.is_empty());
    let mut parts = vec![node.tag.clone()];
    parts.extend(attrs);
    if !text.is_empty() {
        parts.push(format!("text={text}"));
    }
    parts.extend(children);
    parts.join("|")
}

/// Canonical `desc_signature` structures, in document order.
fn signature_facts(xml: &str) -> Vec<String> {
    let tree = parse_pseudoxml(xml);
    let mut signatures = Vec::new();
    collect(&tree, "desc_signature", &mut signatures);
    signatures.iter().map(|node| normalize_node(node)).collect()
}

fn all_text(node: &Tree) -> String {
    let mut out = node.text.clone();
    for child in &node.children {
        out.push(' ');
        out.push_str(&all_text(child));
    }
    out
}

/// Reference resolution facts (same reduction as Phase 11 §15.7).
fn resolution_facts(xml: &str) -> Vec<String> {
    let tree = parse_pseudoxml(xml);
    let mut refs = Vec::new();
    fn walk(node: &Tree, refs: &mut Vec<String>, in_ref: bool) {
        let is_ref = matches!(node.tag.as_str(), "reference" | "number_reference" | "download_reference")
            || (matches!(node.tag.as_str(), "inline" | "literal")
                && node.attrs.get("classes").is_some_and(|classes| classes.contains("xref")));
        if is_ref {
            let mut fact = if node.tag.starts_with("reference") || node.tag.ends_with("reference") {
                "ref".to_owned()
            } else {
                "fallback".to_owned()
            };
            for key in ["refid", "refuri", "reftitle", "title", "classes"] {
                if let Some(value) = node.attrs.get(key) {
                    let value = if key == "refid" || key == "refuri" { canonical_id(value) } else { value.clone() };
                    fact.push_str(&format!(" {key}={value}"));
                }
            }
            let text = all_text(node).split_whitespace().collect::<Vec<_>>().join(" ");
            if !text.is_empty() {
                fact.push_str(&format!(" text={text}"));
            }
            refs.push(fact);
        }
        for child in &node.children {
            walk(child, refs, is_ref);
        }
        let _ = in_ref;
    }
    for root in &tree {
        walk(root, &mut refs, false);
    }
    refs
}

fn warning_lines(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .filter_map(|line| {
            let message = line.split_once(" WARNING: ").map(|(_, message)| message)?;
            Some(message.split(" [").next().unwrap_or(message).to_owned())
        })
        .collect()
}

fn oracle(root: &Path) -> (BTreeMap<String, String>, Vec<String>) {
    let binary = workspace().join(".oracle-venv/bin/sphinx-build");
    assert!(binary.is_file(), "oracle not found");
    let out = std::env::temp_dir().join(format!(
        "scribe-py-oracle-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&out);
    let result = Command::new(binary)
        .arg("-b").arg("pseudoxml").arg("-E").arg("-n").arg("-q")
        .arg(root).arg(&out).output().expect("run oracle");
    assert!(result.status.success(), "oracle failed: {}", String::from_utf8_lossy(&result.stderr));
    let mut docs = BTreeMap::new();
    if let Ok(entries) = std::fs::read_dir(&out) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("pseudoxml") {
                let name = path.file_stem().and_then(|name| name.to_str()).unwrap_or("").to_owned();
                if let Ok(xml) = std::fs::read_to_string(&path) {
                    docs.insert(name, xml);
                }
            }
        }
    }
    let warnings = warning_lines(&String::from_utf8_lossy(&result.stderr));
    let _ = std::fs::remove_dir_all(&out);
    (docs, warnings)
}

fn ours(root: &Path, docs: &[&str]) -> (BTreeMap<String, String>, Vec<String>) {
    let request = LoadRequest {
        conf_dir: root.to_path_buf(),
        strategy: BridgeStrategy::Static,
        ..LoadRequest::default()
    };
    let mut config = load_config_with(request).expect("load conf").0;
    config.set("nitpicky", scribe_config::ConfigValue::Bool(true)).expect("nitpicky");
    let mut result = BTreeMap::new();
    let mut all_warnings = Vec::new();
    let sink = DiagnosticSink::default();
    let registry = DomainRegistry::builtin();
    for name in docs {
        let source = std::fs::read_to_string(root.join(format!("{name}.rst"))).expect("read source");
        let mut output = parse_with_options(&source, ParseOptions::new(Some((*name).to_owned())));
        let mut context = Context::new(root, Default::default());
        context.config = Some(std::sync::Arc::new(config.clone()));
        context.source_name = Some((*name).to_owned());
        let report = resolve_output(&mut output, &DirectiveRegistry::builtin(), &context);
        assert!(report.is_clean(), "{name} resolve issues: {:?}", report.unresolved);
        output.document.metadata.docname = Some((*name).to_owned());
        prepare_document(&mut output.document).expect("prepare");
        let mut env = TestEnv::default();
        for other in docs {
            // Real fixture titles, used by `:doc:` implicit captions.
            let title = match (*other, *name) {
                ("index", "module") => "test-domain-py",
                ("roles", _) => "roles",
                ("module", _) => "module",
                ("module_option", _) => "module_option",
                ("abbr", _) => "abbrev",
                ("canonical", _) => "caninical",
                ("type_alias", _) => "Type Alias",
                _ => other,
            };
            env = env.with_doc(other, title);
        }
        env = env.with_doctree(name, name, output.document.clone());
        let ctx = DomainContext::new(Some(&env), Some(&config), name, "pseudoxml", "pseudoxml", &sink);
        registry.process_document(&ctx, name, &output.document).expect("process");
        let resolution = resolve_pending_xrefs(&registry, &ctx, &mut output.document).expect("resolve");
        let _ = resolution;
        let xml = output.document.to_pseudoxml().expect("pseudoxml");
        result.insert((*name).to_owned(), xml);
    }
    all_warnings.extend(sink.snapshot().into_iter().map(|diagnostic| diagnostic.message));
    (result, all_warnings)
}

/// Tiny in-memory `DomainEnv` (mirrors Phase 11's test adapter).
#[derive(Debug, Default)]
struct TestEnv {
    docs: BTreeMap<String, (String, scribe_ast::Document)>,
}

impl TestEnv {
    fn with_doctree(mut self, docname: &str, title: &str, document: scribe_ast::Document) -> Self {
        self.docs.insert(docname.to_owned(), (title.to_owned(), document));
        self
    }

    fn with_doc(mut self, docname: &str, title: &str) -> Self {
        self.docs.entry(docname.to_owned()).or_insert_with(|| {
            (title.to_owned(), scribe_ast::Document::new(Some(format!("{docname}.rst"))))
        });
        self
    }
}

impl scribe_domains::DomainEnv for TestEnv {
    fn docnames(&self) -> Vec<String> { self.docs.keys().cloned().collect() }
    fn has_doc(&self, docname: &str) -> bool { self.docs.contains_key(docname) }
    fn title(&self, docname: &str) -> Option<String> { self.docs.get(docname).map(|(title, _)| title.clone()) }
    fn doctree(&self, docname: &str) -> Option<&scribe_ast::Document> { self.docs.get(docname).map(|(_, tree)| tree) }
}

fn check_fixture(fixture: &str, docs: &[&str], compare: &[&str]) {
    let root = workspace().join("sphinx/tests/roots").join(fixture);
    let (oracle_docs, oracle_warnings) = oracle(&root);
    let (our_docs, our_warnings) = ours(&root, docs);
    assert_eq!(our_warnings, oracle_warnings, "warning mismatch for {fixture}\nours={our_warnings:#?}\noracle={oracle_warnings:#?}");
    for name in compare {
        let oracle_xml = oracle_docs.get(*name).unwrap_or_else(|| panic!("missing oracle pseudoxml for {name}"));
        let our_xml = our_docs.get(*name).unwrap_or_else(|| panic!("missing our pseudoxml for {name}"));
        assert_eq!(
            signature_facts(our_xml),
            signature_facts(oracle_xml),
            "signature mismatch for {fixture}/{name}\nours={}\n{}",
            signature_facts(our_xml).join("\n"),
            signature_facts(oracle_xml).join("\n")
        );
        assert_eq!(
            resolution_facts(our_xml),
            resolution_facts(oracle_xml),
            "resolution mismatch for {fixture}/{name}\nours={:#?}\noracle={:#?}",
            resolution_facts(our_xml),
            resolution_facts(oracle_xml)
        );
    }
}

#[test]
fn differential_test_domain_py() {
    check_fixture(
        "test-domain-py",
        &["index", "roles", "module", "module_option", "abbr", "canonical", "type_alias"],
        // `index.rst` is a toctree-only page (Phase 09 owns those references).
        &["roles", "module", "module_option", "abbr", "canonical", "type_alias"],
    );
}

#[test]
fn differential_python_use_unqualified_type_names() {
    check_fixture("test-domain-py-python_use_unqualified_type_names", &["index"], &["index"]);
}

#[test]
fn differential_py_xref_type_alias() {
    check_fixture("test-domain-py-xref-type-alias", &["index"], &["index"]);
}

#[test]
fn differential_py_xref_warning_root() {
    // The oracle's own `test-domain-py-xref-warning` root is the canonical
    // missing-`:ref:` warning fixture (Phase 11 §15.7, re-checked here).
    let root = workspace().join("sphinx/tests/roots/test-domain-py-xref-warning");
    let (_, oracle_warnings) = oracle(&root);
    let (_, our_warnings) = ours(&root, &["index"]);
    assert_eq!(our_warnings, oracle_warnings);
    assert_eq!(
        our_warnings,
        [
            "undefined label: 'no-label'".to_owned(),
            "Failed to create a cross reference. A title or caption not found: 'existing-label'".to_owned(),
        ]
    );
}
