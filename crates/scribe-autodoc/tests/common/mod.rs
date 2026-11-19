//! Shared helpers for autodoc differential tests.
//!
//! Both engines are reduced to a documented *structural subset* (§21.9 of
//! ARCHITECTURE.md): object identities `(module, fullname)`, ordered member
//! lists, signature name tokens and docstring field names. Generated ids,
//! source paths, namespace declarations and CSS-only attributes are removed.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use scribe_config::{Config, ConfigValue, Tags};
use scribe_domains::prepare_document;
use scribe_rst::{parse, resolve_output, Context, DirectiveRegistry};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Workspace root, derived from this crate's manifest.
pub fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("workspace root")
}

/// Oracle `sphinx-build` wrapper returning the pseudoxml for `docname`.
pub fn oracle_pseudoxml(fixture: &str, docname: &str) -> String {
    let root = workspace();
    let source = root.join("sphinx/tests/roots").join(fixture);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let out = std::env::temp_dir().join(format!("scribe-autodoc-oracle-{fixture}-{unique}"));
    let _ = fs::remove_dir_all(&out);
    let status = Command::new(root.join(".oracle-venv/bin/sphinx-build"))
        .args(["-b", "pseudoxml", "-E", "-q"])
        .arg(&source)
        .arg(&out)
        .output()
        .expect("spawn oracle sphinx-build");
    assert!(
        status.status.success(),
        "oracle failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    fs::read_to_string(out.join(format!("{docname}.pseudoxml"))).expect("oracle pseudoxml")
}

/// Builds a `Config` suitable for autodoc directives.
pub fn config(extensions: &[&str], extra: &[(&str, ConfigValue)]) -> Config {
    let mut config = Config::new();
    config
        .set("extensions", ConfigValue::List(extensions.iter().map(|name| ConfigValue::from(*name)).collect()))
        .expect("extensions");
    for (key, value) in extra {
        config.set(key, value.clone()).expect("config key");
    }
    config
}

/// Produces pseudoxml from scribe's RST → directive → domain pipeline.
pub fn scribe_pseudoxml(rst: &str, source_dir: &Path, config: &Config) -> String {
    let mut output = parse(rst);
    let registry = DirectiveRegistry::builtin();
    scribe_autodoc::register_directives(&registry);
    let mut ctx = Context::new(source_dir, Tags::new());
    ctx.config = Some(std::sync::Arc::new(config.clone()));
    ctx.source_name = Some("index".to_owned());
    let report = resolve_output(&mut output, &registry, &ctx);
    assert!(
        report.unresolved.is_empty(),
        "unresolved directives: {:?}",
        report.unresolved.iter().map(|issue| issue.message.clone()).collect::<Vec<_>>()
    );
    prepare_document(&mut output.document).expect("prepare_document");
    output.document.to_pseudoxml().expect("pseudoxml")
}

/// Minimal indentation tree for normalized pseudoxml facts.
#[derive(Debug)]
pub struct Tree {
    /// Element tag (`#text` for text nodes).
    pub tag: String,
    /// Kept attributes.
    pub attrs: BTreeMap<String, String>,
    /// Direct text content.
    pub text: String,
    /// Child elements.
    pub children: Vec<Tree>,
}

/// Parses Docutils pseudo-XML into [`Tree`]s. Only element start tags,
/// indented text lines and quoted attributes are understood; that is the same
/// subset Phase 03 freezes and Phase 11/12 differential tests use.
#[must_use]
pub fn parse_pseudoxml(xml: &str) -> Vec<Tree> {
    let mut roots: Vec<Tree> = Vec::new();
    let mut stack: Vec<(usize, Vec<usize>)> = Vec::new();
    for line in xml.lines() {
        if line.trim().is_empty() {
            continue;
        }
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

fn parse_attrs(line: &str) -> (String, BTreeMap<String, String>) {
    let body = line.trim_start().strip_prefix('<').unwrap_or(line).trim_end_matches('>');
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
                if bytes[i] == b'\\' { i += 2; continue; }
                if bytes[i] == b'"' { break; }
                i += 1;
            }
            let value = rest[value_start..i.min(rest.len())].replace("\\ ", " ");
            attrs.insert(key, value);
            i += 1;
        }
    }
    (tag, attrs)
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
    if let Some(parent) = get_mut_at(nodes, path) {
        parent.children.push(node);
    } else {
        nodes.push(node);
    }
}

/// Depth-first collection of nodes with a tag.
pub fn collect<'a>(nodes: &'a [Tree], tag: &str, out: &mut Vec<&'a Tree>) {
    for node in nodes {
        if node.tag == tag { out.push(node); }
        collect(&node.children, tag, out);
    }
}

/// Object identities `(module, class, fullname)` from `desc_signature` nodes.
#[must_use]
pub fn signature_facts(xml: &str) -> Vec<(String, String, String)> {
    let tree = parse_pseudoxml(xml);
    let mut signatures = Vec::new();
    collect(&tree, "desc_signature", &mut signatures);
    signatures
        .iter()
        .map(|node| {
            let attr = |key: &str| node.attrs.get(key).cloned().unwrap_or_default();
            (attr("module"), attr("class"), attr("fullname"))
        })
        .collect()
}

/// Sorted object identities, ignoring duplicates from overloads.
#[must_use]
pub fn object_set(xml: &str) -> Vec<(String, String, String)> {
    let mut facts = signature_facts(xml);
    facts.sort();
    facts.dedup();
    facts
}

/// Field names in the whole document, normalized to lower case.
#[must_use]
pub fn field_names(xml: &str) -> Vec<String> {
    let tree = parse_pseudoxml(xml);
    let mut fields = Vec::new();
    collect(&tree, "field_name", &mut fields);
    let mut out: Vec<String> = fields
        .iter()
        .map(|node| node.text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase())
        .filter(|name| !name.is_empty())
        .collect();
    out.sort();
    out
}

/// Signature name tokens in document order (qualified addname + name).
#[must_use]
pub fn signature_text(xml: &str) -> Vec<String> {
    let tree = parse_pseudoxml(xml);
    let mut signatures = Vec::new();
    collect(&tree, "desc_signature", &mut signatures);
    signatures
        .iter()
        .map(|signature| {
            let mut tokens = Vec::new();
            collect(&signature.children, "desc_name", &mut tokens);
            collect(&signature.children, "desc_addname", &mut tokens);
            tokens
                .into_iter()
                .map(|node| node.text.split_whitespace().collect::<Vec<_>>().join(" "))
                .collect::<Vec<_>>()
                .join("")
        })
        .collect()
}
