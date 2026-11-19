//! Differential tests for intersphinx resolution against the pinned oracle.
//!
//! Approach (documented in ARCHITECTURE.md §18.4): the upstream
//! `test-ext-intersphinx-role`, `test-domain-c-intersphinx` and
//! `test-domain-cpp-intersphinx` roots use remote inventories and cannot run
//! offline.  Following the task's permitted alternative, we generate the
//! oracle's canonical `INVENTORY_V2` bytes locally (fixture script) and point
//! `intersphinx_mapping` at that file on both sides.  Both builds therefore
//! consume identical bytes; the comparison covers resolved reference facts
//! (`refuri`, `reftitle`, `internal`, display text) and exact warnings.
//!
//! `:external:`-prefixed roles are constructed directly on the frozen AST
//! from the documented §9.4 `pending_xref` shape, because scribe-rst's static
//! role table is a Phase 27 extension seam (unknown roles become
//! `Problematic`).  The resolution semantics are identical to the oracle's
//! `IntersphinxRole` output.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use scribe_ast::NodeKind;
use scribe_config::{load_config_with, BridgeStrategy, ConfigValue, LoadRequest};
use scribe_domains::{
    fetch_one, ExternalRole, IntersphinxResolver, MappingEntry, XrefResolver,
};
use scribe_rst::{parse_with_options, resolve_output, Context, DirectiveRegistry, ParseOptions};

mod common;

use common::TestEnv;

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Project {
    root: PathBuf,
    source: PathBuf,
    output: PathBuf,
    inventory: PathBuf,
}

impl Project {
    fn new(name: &str, source: &str) -> Self {
        let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "scribe-intersphinx-diff-{name}-{}-{sequence}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let source_dir = root.join("src");
        std::fs::create_dir_all(&source_dir).expect("create source dir");
        let inventory = root.join("objects.inv");
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let script = workspace.join("crates/scribe-domains/tests/fixtures/gen_intersphinx_v2.py");
        let status = Command::new(workspace.join(".oracle-venv/bin/python"))
            .arg(script)
            .arg(&inventory)
            .status()
            .expect("run oracle inventory generator");
        assert!(status.success(), "oracle inventory generator failed");
        let conf = format!(
            "extensions = ['sphinx.ext.intersphinx']\n\
             master_doc = 'index'\n\
             intersphinx_mapping = {{'inv': ('https://example.org/', {path:?})}}\n\
             intersphinx_cache_limit = 0\n\
             intersphinx_disabled_reftypes = []\n",
            path = inventory.display().to_string(),
        );
        std::fs::write(source_dir.join("conf.py"), conf).expect("write conf.py");
        std::fs::write(source_dir.join("index.rst"), source).expect("write index.rst");
        Self { root: root.clone(), source: source_dir, output: root.join("out"), inventory }
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn oracle(project: &Project) -> (String, Vec<String>) {
    let binary = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.oracle-venv/bin/sphinx-build");
    assert!(binary.is_file(), "oracle binary not found: {}", binary.display());
    let output = Command::new(binary)
        .arg("-b").arg("pseudoxml")
        .arg("-E").arg("-n").arg("-q")
        .arg(&project.source)
        .arg(&project.output)
        .output()
        .expect("run oracle sphinx-build");
    assert!(output.status.success(), "oracle build failed: {}", String::from_utf8_lossy(&output.stderr));
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

fn ours(project: &Project, externals: &[(usize, &str)]) -> (String, Vec<String>, usize) {
    let request = LoadRequest {
        conf_dir: project.source.clone(),
        strategy: BridgeStrategy::Static,
        ..LoadRequest::default()
    };
    let mut config = load_config_with(request).expect("load conf.py").0;
    config.set("nitpicky", ConfigValue::Bool(true)).expect("set nitpicky");

    let source = std::fs::read_to_string(project.source.join("index.rst")).expect("read source");
    let mut output = parse_with_options(&source, ParseOptions::new(Some("index".to_owned())));
    let mut context = Context::new(&project.source, Default::default());
    context.config = Some(std::sync::Arc::new(config.clone()));
    context.source_name = Some("index".to_owned());
    let report = resolve_output(&mut output, &DirectiveRegistry::builtin(), &context);
    assert!(report.is_clean(), "resolve issues: {:?}", report.unresolved);
    output.document.metadata.docname = Some("index".to_owned());

    // Annotate the requested pending xrefs as `:external:` roles.  The index
    // counts pending xrefs in document order.
    let registry = scribe_domains::DomainRegistry::builtin();
    let xref_ids: Vec<_> = output.document.preorder_ids().into_iter()
        .filter(|&id| output.document.get(id).is_ok_and(|node| node.kind == NodeKind::PendingXref))
        .collect();
    for &(index, role) in externals {
        let id = xref_ids[index];
        let parsed = ExternalRole::parse(role).expect("parse external role");
        parsed.annotate(&mut output.document, id, &registry).expect("annotate");
    }

    let entry = MappingEntry {
        name: "inv".to_owned(),
        target_uri: "https://example.org/".to_owned(),
        locations: vec![Some(project.inventory.display().to_string())],
    };
    let outcome = fetch_one(&entry, &scribe_domains::FetchOptions::default(), None);
    assert!(outcome.inventory.is_some(), "inventory not loaded: {:?}", outcome.warnings);
    let resolver = IntersphinxResolver::new(vec![outcome], &[]);

    let env = TestEnv::default();
    let sink = scribe_domains::DiagnosticSink::default();
    let ctx = scribe_domains::DomainContext::new(Some(&env), Some(&config), "index", "pseudoxml", "pseudoxml", &sink);
    registry.process_document(&ctx, "index", &output.document).expect("process document");
    let resolution = XrefResolver::new(&registry, Some(&resolver))
        .resolve_document(&ctx, &mut output.document)
        .expect("resolve");
    let resolved_intersphinx = resolution.resolved_intersphinx;
    let warnings = resolution.diagnostics.into_iter().map(|diagnostic| diagnostic.message).collect();
    (output.document.to_pseudoxml().expect("emit pseudoxml"), warnings, resolved_intersphinx)
}

/// Reduces pseudo-XML to intersphinx-relevant resolution facts.
fn resolution_facts(pseudoxml: &str) -> Vec<String> {
    let lines: Vec<&str> = pseudoxml.lines().collect();
    let mut facts = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let is_reference = trimmed.starts_with("<reference");
        // A resolved reference may contain an inner `<inline classes="xref">`
        // carrying a replaced display name; only count unresolved fallbacks,
        // which always carry the full `xref <domain> <role>` class list.
        let is_fallback = trimmed.starts_with("<inline classes=\"xref")
            && trimmed
                .split("classes=\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .is_some_and(|classes| classes.split_whitespace().count() >= 2);
        if !is_reference && !is_fallback {
            continue;
        }
        let mut fact = String::from("ref");
        for key in ["refuri", "reftitle", "internal", "classes"] {
            let needle = format!("{key}=\"");
            if let Some(start) = trimmed.find(&needle) {
                if let Some(end) = trimmed[start + needle.len()..].find('"') {
                    let value = &trimmed[start + needle.len()..start + needle.len() + end];
                    fact.push_str(&format!(" {key}={value}"));
                }
            }
        }
        if let Some(text) = lines[index + 1..].iter().find(|next| {
            let next = next.trim();
            !next.is_empty() && !next.starts_with('<')
        }) {
            fact.push_str(&format!(" text={}", text.trim()));
        }
        facts.push(fact);
    }
    facts
}

fn check(name: &str, source: &str, externals: &[(usize, &str)], expected_external: usize) {
    let project = Project::new(name, source);
    let (oracle_xml, oracle_warnings) = oracle(&project);
    let (our_xml, our_warnings, resolved_external) = ours(&project, externals);
    assert_eq!(resolved_external, expected_external, "unexpected intersphinx resolution count");
    // In the `:external:` fixture the oracle source uses ordinary roles (our
    // RST parser cannot parse `external+...` yet, §27 seam), so its missing
    // warning omits upstream's `external ` prefix.  Normalize that one label.
    let our_warnings: Vec<String> = if externals.is_empty() {
        our_warnings
    } else {
        our_warnings
            .into_iter()
            .map(|warning| warning.strip_prefix("external ").unwrap_or(&warning).to_owned())
            .collect()
    };
    let oracle_facts = resolution_facts(&oracle_xml);
    let our_facts = resolution_facts(&our_xml);
    assert_eq!(
        our_warnings, oracle_warnings,
        "warning mismatch\noracle:\n{oracle_xml}\nours:\n{our_xml}\noracle warnings: {oracle_warnings:?}\nour warnings: {our_warnings:?}"
    );
    assert_eq!(
        our_facts, oracle_facts,
        "resolution mismatch\noracle facts: {oracle_facts:#?}\nour facts: {our_facts:#?}\noracle:\n{oracle_xml}\nours:\n{our_xml}"
    );
}

#[test]
fn differential_intersphinx_py_and_c_roles() {
    let source = "\
Title
=====

A function :py:func:`module1.func`.

A module :py:mod:`module1`.

A C function :c:func:`CFunc`.

A term :term:`a term`.

A missing function :py:func:`missing_func`.
";
    check("py-c", source, &[], 4);
}

#[test]
fn differential_external_role_explicit_inventory() {
    // The oracle fixture's `:external+inv:c:func:`CFunc`` case; the inventory
    // name is annotated directly on the pending xref (see module docs).
    let source = "\
Title
=====

An external C function :c:func:`CFunc`.

An external module :py:mod:`module1`.

A missing external function :py:func:`missing_func`.
";
    check("external", source, &[(0, "external+inv:c:func"), (1, "external+inv:py:mod"), (2, "external+inv:py:func")], 2);
}

#[test]
fn writer_fixture_is_compatible_with_oracle_inventory() {
    // Guard the differential setup itself: our reader must see the same
    // entries the oracle uses in test_ext_intersphinx.py.
    let project = Project::new("reader-check", "Title\n=====\n");
    let entry = MappingEntry {
        name: "inv".to_owned(),
        target_uri: "https://example.org/".to_owned(),
        locations: vec![Some(project.inventory.display().to_string())],
    };
    let outcome = fetch_one(&entry, &scribe_domains::FetchOptions::default(), None);
    let inventory = outcome.inventory.expect("inventory");
    assert_eq!(inventory.get("py:function", "module1.func").unwrap().uri,
        "https://example.org/sub/foo.html#module1.func");
    assert_eq!(inventory.get("std:label", "The-Julia-Domain").unwrap().uri,
        "https://example.org/write_inventory/#The-Julia-Domain");
}
