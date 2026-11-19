//! Differential tests for the Phase 13 C / C++ / JavaScript domains against
//! the pinned Sphinx oracle (ARCHITECTURE.md §17).
//!
//! Method:
//! 1. Copy a real `sphinx/tests/roots/test-domain-*` fixture to a temp dir and
//!    run `.oracle-venv/bin/sphinx-build -b pseudoxml -E -n -q`.
//! 2. Parse the same `.rst` files with `scribe-rst`, expand every domain
//!    skeleton, register objects in `DomainRegistry::builtin()` and resolve all
//!    pending xrefs (nitpicky enabled).
//! 3. Reduce both trees to (a) per-signature ordered leaf structure and
//!    (b) reference resolution facts; compare exact warning messages.
//!
//! Normalized away (internal / frozen-AST surface): all attributes except the
//! ones listed below, generated ids (canonicalized by their object role),
//! `xml:space`, `is_multiline` / `multi_line*` flags, `_toc_*` hints, document
//! attributes and namespace declarations.  Signature xrefs are compared by
//! their inner text; C++ Itanium mangled ids are canonicalized (documented
//! divergence in ARCHITECTURE.md §17.6).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use scribe_config::{Config, ConfigValue};
use scribe_domains::{prepare_document, resolve_pending_xrefs, DiagnosticSink, DomainContext, DomainRegistry};
use scribe_rst::{parse_with_options, resolve_output, Context, DirectiveRegistry, ParseOptions};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("workspace root")
}

fn oracle_pseudoxml(fixture: &str) -> BTreeMap<String, String> {
    let root = workspace();
    let src = root.join("sphinx/tests/roots").join(fixture);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let out = std::env::temp_dir().join(format!("scribe-phase13-{fixture}-{n}"));
    let _ = std::fs::remove_dir_all(&out);
    let status = Command::new(root.join(".oracle-venv/bin/sphinx-build"))
        .args(["-b", "pseudoxml", "-E", "-n", "-q"])
        .arg(&src).arg(&out)
        .output()
        .expect("run oracle sphinx-build");
    assert!(status.status.success(), "oracle build failed: {}", String::from_utf8_lossy(&status.stderr));
    let mut result = BTreeMap::new();
    for entry in std::fs::read_dir(&out).expect("read oracle output") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.extension().is_some_and(|extension| extension == "pseudoxml") {
            let name = path.file_stem().expect("stem").to_string_lossy().to_string();
            if let Ok(text) = std::fs::read_to_string(&path) {
                result.insert(name, text);
            }
        }
    }
    let _ = std::fs::remove_dir_all(&out);
    result
}

// ----- tiny pseudo-XML tree --------------------------------------------------

#[derive(Clone, Debug, Default)]
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
        let bytes = rest.as_bytes();
        let mut i = 0usize;
        while i < bytes.len() {
            while i < bytes.len() && bytes[i].is_ascii_whitespace() { i += 1; }
            let start = i;
            while i < bytes.len() && bytes[i] != b'=' && !bytes[i].is_ascii_whitespace() { i += 1; }
            if i >= bytes.len() || bytes[i] != b'=' { break; }
            let key = rest[start..i].to_owned();
            i += 1;
            if i >= bytes.len() || bytes[i] != b'"' { continue; }
            i += 1;
            let value_start = i;
            while i < bytes.len() {
                if bytes[i] == b'\\' { i = (i + 2).min(bytes.len()); continue; }
                if bytes[i] == b'"' { break; }
                i += 1;
            }
            attrs.insert(key, rest[value_start..i.min(rest.len())].replace("\\ ", " "));
            i += 1;
        }
    }
    (tag, attrs)
}

fn parse_pseudoxml(xml: &str) -> Vec<Tree> {
    let mut roots: Vec<Tree> = Vec::new();
    let mut stack: Vec<(usize, Vec<usize>)> = Vec::new();
    for line in xml.lines() {
        if line.trim().is_empty() { continue; }
        let raw_indent = line.len() - line.trim_start_matches(' ').len();
        let indent = (raw_indent / 4) * 4;
        let trimmed = line.trim_start();
        if trimmed.starts_with('<') {
            let (tag, attrs) = parse_attrs(trimmed);
            while stack.last().is_some_and(|(level, _)| *level >= indent) { stack.pop(); }
            let node = Tree { tag, attrs, text: String::new(), children: Vec::new() };
            if let Some((_, path)) = stack.last() { insert_at(&mut roots, path, node); } else { roots.push(node); }
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
            while stack.last().is_some_and(|(level, _)| *level >= indent) { stack.pop(); }
            if let Some((_, path)) = stack.last() {
                if let Some(node) = get_mut_at(&mut roots, path) {
                    if !node.text.is_empty() { node.text.push(' '); }
                    node.text.push_str(trimmed);
                }
            }
        }
    }
    roots
}

fn get_at<'a>(nodes: &'a [Tree], path: &[usize]) -> Option<&'a Tree> {
    let mut current = nodes.get(*path.first()?)?;
    for index in &path[1..] { current = current.children.get(*index)?; }
    Some(current)
}
fn get_mut_at<'a>(nodes: &'a mut [Tree], path: &[usize]) -> Option<&'a mut Tree> {
    let mut current = nodes.get_mut(*path.first()?)?;
    for index in &path[1..] { current = current.children.get_mut(*index)?; }
    Some(current)
}
fn insert_at(nodes: &mut Vec<Tree>, path: &[usize], node: Tree) {
    if let Some(parent) = get_mut_at(nodes, path) { parent.children.push(node); } else { nodes.push(node); }
}

fn collect<'a>(nodes: &'a [Tree], tag: &str, out: &mut Vec<&'a Tree>) {
    for node in nodes {
        if node.tag == tag { out.push(node); }
        collect(&node.children, tag, out);
    }
}

/// Normalized leaf structure of one `desc_signature` from the oracle XML.
fn oracle_signature_structure(sig: &Tree) -> String {
    let mut parts = Vec::new();
    fn walk(node: &Tree, parts: &mut Vec<String>) {
        let tag = if matches!(node.tag.as_str(), "reference" | "pending_xref") {
            // Signature xrefs reduce to their inner node.
            String::new()
        } else {
            node.tag.clone()
        };
        if !tag.is_empty() {
            let text = node.text.trim();
            if text.is_empty() { parts.push(tag); } else { parts.push(format!("{tag}:{text}")); }
        } else {
            let text = node.text.trim();
            if !text.is_empty() {
                if let Some(last) = parts.last_mut() {
                    if last.starts_with("desc_sig_name:") {
                        // keep as-is
                    }
                }
            }
        }
        for child in &node.children { walk(child, parts); }
    }
    for child in &sig.children { walk(child, &mut parts); }
    parts.join("|")
}

/// Normalized leaf structure of one signature from our AST.
fn our_signature_structure(doc: &scribe_ast::Document, id: scribe_ast::NodeId) -> String {
    use scribe_ast::{NodeId, NodeKind};
    fn walk(doc: &scribe_ast::Document, id: NodeId, parts: &mut Vec<String>) {
        let Ok(node) = doc.get(id) else { return };
        if matches!(node.kind, NodeKind::PendingXref | NodeKind::Reference | NodeKind::Text
            | NodeKind::Inline | NodeKind::Literal) {
            for &child in &node.children { walk(doc, child, parts); }
            return;
        }
        let is_leaf = matches!(
            node.kind,
            NodeKind::DescSigSpace | NodeKind::DescSigName | NodeKind::DescSigKeyword
                | NodeKind::DescSigKeywordType | NodeKind::DescSigPunctuation
                | NodeKind::DescSigOperator | NodeKind::DescSigLiteralNumber
                | NodeKind::DescSigLiteralString | NodeKind::DescSigLiteralChar
        );
        let tag = node.kind.tag_name().to_owned();
        if is_leaf {
            let text = node.astext(doc);
            let text = text.trim();
            if text.is_empty() { parts.push(tag); } else { parts.push(format!("{tag}:{text}")); }
        } else {
            parts.push(tag);
        }
        for &child in &node.children { walk(doc, child, parts); }
    }
    let mut parts = Vec::new();
    for &child in &doc.get(id).map(|node| node.children.clone()).unwrap_or_default() {
        walk(doc, child, &mut parts);
    }
    parts.join("|")
}

/// Resolution facts from oracle XML: `(classes, refid-kind, first-text)`.
fn oracle_refs(nodes: &[Tree]) -> Vec<String> {
    let mut refs = Vec::new();
    let mut found = Vec::new();
    collect(nodes, "reference", &mut found);
    for node in found {
        let classes = node.attrs.get("classes").cloned().unwrap_or_default();
        if !classes.contains("xref") { continue; }
        let refid = node.attrs.get("refid").cloned().unwrap_or_default();
        let refuri = node.attrs.get("refuri").cloned().unwrap_or_default();
        let text = node.children.first().map(|child| child.text.trim().to_owned()).unwrap_or_default();
        refs.push(format!("{classes}|{}|{text}", canonical_ref(&refid, &refuri)));
    }
    refs.sort();
    refs
}

fn canonical_ref(refid: &str, refuri: &str) -> String {
    if !refuri.is_empty() { return format!("uri:{refuri}"); }
    if refid.starts_with("_CPP") { return "cpp:<mangled>".to_owned(); }
    refid.to_owned()
}

/// Resolution facts from our AST.
fn our_refs(doc: &scribe_ast::Document) -> Vec<String> {
    use scribe_ast::NodeKind;
    let mut refs = Vec::new();
    for id in doc.preorder_ids() {
        let Ok(node) = doc.get(id) else { continue };
        if !matches!(node.kind, NodeKind::Reference | NodeKind::NumberReference) { continue; }
        let classes = node.get_list("classes").ok().flatten().unwrap_or_default().join(" ");
        if !classes.contains("xref") { continue; }
        let refid = node.get_str("refid").ok().flatten().unwrap_or_default().to_owned();
        let refuri = node.get_str("refuri").ok().flatten().unwrap_or_default().to_owned();
        let text = node.children.first()
            .and_then(|child| doc.get(*child).ok())
            .map(|child| child.astext(doc).trim().to_owned())
            .unwrap_or_default();
        refs.push(format!("{classes}|{}|{text}", canonical_ref(&refid, &refuri)));
    }
    refs.sort();
    refs
}

// ----- our pipeline ----------------------------------------------------------

fn build_our_doc(source: &str, fixture: &str) -> scribe_ast::Document {
    let mut config = Config::new();
    let _ = config.set("nitpicky", ConfigValue::Bool(true));
    let _ = config.set("add_module_names", ConfigValue::Bool(true));
    let docname = source_path_docname(fixture);
    let mut out = parse_with_options(source, ParseOptions::new(Some(docname.to_owned())));
    let report = resolve_output(&mut out, &DirectiveRegistry::builtin(), &Context::default());
    assert!(report.is_clean(), "{fixture}: {:?}", report.unresolved);
    prepare_document(&mut out.document).expect("prepare");
    let registry = DomainRegistry::builtin();
    let sink = DiagnosticSink::default();
    let env = DomainEnvStub;
    let ctx = DomainContext::new(Some(&env), Some(&config), docname, "pseudoxml", "pseudoxml", &sink);
    registry.process_document(&ctx, docname, &out.document).expect("process");
    resolve_pending_xrefs(&registry, &ctx, &mut out.document).expect("resolve");
    out.document
}

fn source_path_docname(fixture: &str) -> &'static str {
    match fixture {
        "test-domain-c" => "index",
        "test-domain-cpp" => "index",
        "test-domain-js" => "roles",
        other => panic!("unknown fixture {other}"),
    }
}

struct DomainEnvStub;
impl scribe_domains::DomainEnv for DomainEnvStub {
    fn has_doc(&self, docname: &str) -> bool {
        matches!(docname, "index" | "roles" | "module")
    }
    fn docnames(&self) -> Vec<String> {
        vec!["index".into(), "roles".into(), "module".into()]
    }
}

fn normalize_warning(line: &str) -> Option<String> {
    let message = line.split("WARNING: ").nth(1)?;
    let message = message.split(" [").next()?.trim();
    // Focus on cross-reference warnings; toctree warnings are Phase 09's.
    if message.contains("isn't included in any toctree") { return None; }
    Some(message.to_owned())
}

fn oracle_warnings(fixture: &str) -> BTreeSet<String> {
    let root = workspace();
    let src = root.join("sphinx/tests/roots").join(fixture);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let out = std::env::temp_dir().join(format!("scribe-phase13-warn-{fixture}-{n}"));
    let _ = std::fs::remove_dir_all(&out);
    let status = Command::new(root.join(".oracle-venv/bin/sphinx-build"))
        .args(["-b", "pseudoxml", "-E", "-n", "-q"])
        .arg(&src).arg(&out)
        .output()
        .expect("run oracle");
    let stderr = String::from_utf8_lossy(&status.stderr);
    let warnings: BTreeSet<String> = stderr.lines().filter_map(normalize_warning).collect();
    let _ = std::fs::remove_dir_all(&out);
    warnings
}

// ----- tests ----------------------------------------------------------------

fn signature_parity(fixture: &str, docname: &str, min_ratio: f64) {
    let xml = oracle_pseudoxml(fixture);
    let oracle_xml = xml.get(docname).unwrap_or_else(|| panic!("no oracle {docname}"));
    let oracle_tree = parse_pseudoxml(oracle_xml);
    let mut oracle_sigs = Vec::new();
    collect(&oracle_tree, "desc_signature", &mut oracle_sigs);
    let oracle_structures: Vec<String> = oracle_sigs.iter().map(|sig| oracle_signature_structure(sig)).collect();

    let source = std::fs::read_to_string(
        workspace().join("sphinx/tests/roots").join(fixture).join(format!("{docname}.rst"))
    ).expect("read source");
    let our = build_our_doc(&source, fixture);
    use scribe_ast::NodeKind;
    let our_sig_ids: Vec<_> = our.preorder_ids().into_iter()
        .filter(|&id| our.get(id).is_ok_and(|node| node.kind == NodeKind::DescSignature))
        .collect();
    let our_structures: Vec<String> = our_sig_ids.iter().map(|&id| our_signature_structure(&our, id)).collect();

    // Pairwise comparison in document order; report mismatches for diagnosis.
    let mut matches = 0usize;
    let mut mismatches = Vec::new();
    for (index, (oracle, ours)) in oracle_structures.iter().zip(our_structures.iter()).enumerate() {
        if oracle == ours { matches += 1; } else {
            mismatches.push(format!("signature #{index}\n  oracle: {oracle}\n  ours:   {ours}"));
        }
    }
    let total = oracle_structures.len().max(our_structures.len()).max(1);
    let ratio = matches as f64 / total as f64;
    eprintln!("[{fixture}/{docname}] signature parity: {matches}/{total} = {:.1}%", ratio * 100.0);
    for mismatch in mismatches.iter().take(6) { eprintln!("{mismatch}"); }
    assert_eq!(oracle_structures.len(), our_structures.len(), "{fixture}: signature count mismatch");
    assert!(ratio >= min_ratio, "{fixture}/{docname}: structural parity {:.1}% < {:.0}%", ratio * 100.0, min_ratio * 100.0);
}

#[test]
fn c_domain_signature_parity() {
    signature_parity("test-domain-c", "index", 0.95);
}

#[test]
fn cpp_domain_signature_parity() {
    signature_parity("test-domain-cpp", "index", 0.80);
}

#[test]
fn js_domain_signature_parity() {
    signature_parity("test-domain-js", "roles", 0.95);
}

#[test]
fn c_domain_resolution_facts() {
    let xml = oracle_pseudoxml("test-domain-c");
    let tree = parse_pseudoxml(xml.get("index").expect("oracle index"));
    let expected = oracle_refs(&tree);
    let source = std::fs::read_to_string(
        workspace().join("sphinx/tests/roots/test-domain-c/index.rst")
    ).expect("source");
    let our = build_our_doc(&source, "test-domain-c");
    let actual = our_refs(&our);
    // Every oracle reference fact must be present in ours (we resolve a strict
    // superset because our signature xrefs are resolved as well).
    for expected_fact in &expected {
        assert!(actual.contains(expected_fact), "missing resolution fact:\n  expected: {expected_fact}\n  ours: {actual:#?}");
    }
}

#[test]
fn js_domain_resolution_facts() {
    let xml = oracle_pseudoxml("test-domain-js");
    let tree = parse_pseudoxml(xml.get("roles").expect("oracle roles"));
    let expected = oracle_refs(&tree);
    let source = std::fs::read_to_string(
        workspace().join("sphinx/tests/roots/test-domain-js/roles.rst")
    ).expect("source");
    let our = build_our_doc(&source, "test-domain-js");
    let actual = our_refs(&our);
    for expected_fact in &expected {
        assert!(actual.contains(expected_fact), "missing resolution fact:\n  expected: {expected_fact}\n  ours: {actual:#?}");
    }
}

#[test]
fn warning_parity_on_unresolved_xrefs() {
    // Oracle warnings for the C fixture that are cross-reference related.
    let expected = oracle_warnings("test-domain-c");
    let key: Vec<&String> = expected.iter().filter(|warning| warning.contains("c:identifier")).collect();
    assert!(!key.is_empty(), "fixture should produce c:identifier warnings");
    // Our nitpicky pipeline emits the same messages via DomainDiagnostic.
    let source = std::fs::read_to_string(
        workspace().join("sphinx/tests/roots/test-domain-c/anon-dup-decl.rst")
    ).expect("source");
    let mut config = Config::new();
    let _ = config.set("nitpicky", ConfigValue::Bool(true));
    let mut out = parse_with_options(&source, ParseOptions::new(Some("anon-dup-decl".to_owned())));
    let _ = resolve_output(&mut out, &DirectiveRegistry::builtin(), &Context::default());
    prepare_document(&mut out.document).expect("prepare");
    let registry = DomainRegistry::builtin();
    let sink = DiagnosticSink::default();
    let env = DomainEnvStub;
    let ctx = DomainContext::new(Some(&env), Some(&config), "anon-dup-decl", "pseudoxml", "pseudoxml", &sink);
    registry.process_document(&ctx, "anon-dup-decl", &out.document).expect("process");
    let mut report = resolve_pending_xrefs(&registry, &ctx, &mut out.document).expect("resolve");
    report.diagnostics.extend(sink.snapshot());
    let our: BTreeSet<String> = report.diagnostics.iter().map(|diagnostic| diagnostic.message.clone()).collect();
    for warning in key {
        assert!(our.iter().any(|message| message.as_str() == warning.as_str()), "missing warning {warning:?}; ours: {our:?}");
    }
}
