//! Mixed-project integration: one `.rst` and one `.md` file that cross-reference
//! each other. Cross-file references must survive lowering as pending xrefs
//! whose `reftarget` matches the other file's target/label at the AST level.
//! Final URI resolution belongs to Phases 08/14 and is explicitly out of scope.

mod common;

use std::fs;
use std::path::PathBuf;

use common::{nodes_of, text};
use scribe_ast::NodeKind;
use scribe_myst::{parse_with_options, ParseOptions};
use scribe_rst::parse_with_options as parse_rst;
use scribe_rst::ParseOptions as RstParseOptions;

const RST_SOURCE: &str = "RST Document\n============\n\nReference to MyST: :ref:`myst-target`.\n\n.. _rst-target: https://example.com/rst\n";

const MD_SOURCE: &str = "# MyST Document\n\nReference to RST: {ref}`rst-target`.\n\n(myst-target)=\n## Anchored Heading\n";

fn temp_project() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "scribe-myst-mixed-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    fs::create_dir_all(&dir).expect("create temp project");
    fs::write(dir.join("index.rst"), RST_SOURCE).expect("write rst");
    fs::write(dir.join("page.md"), MD_SOURCE).expect("write md");
    dir
}

#[test]
fn cross_file_pending_xrefs_survive_lowering() {
    let dir = temp_project();
    let rst_source = fs::read_to_string(dir.join("index.rst")).unwrap();
    let md_source = fs::read_to_string(dir.join("page.md")).unwrap();

    let rst_output = parse_rst(
        &rst_source,
        RstParseOptions::new(Some("index.rst".to_owned())),
    );
    let md_output = parse_with_options(
        &md_source,
        ParseOptions::new("page.md"),
    )
    .unwrap();

    // The RST file's `:ref:` target must name the MyST file's `(label)=` anchor.
    let rst_xref = rst_output
        .document
        .iter()
        .find(|(_, node)| node.kind == NodeKind::PendingXref)
        .map(|(id, _)| id)
        .expect("RST pending xref");
    let rst_target = rst_output.document.get(rst_xref).unwrap();
    let rst_reftarget = rst_target.attrs.get_str("reftarget").unwrap().unwrap();
    assert_eq!(rst_reftarget, "myst-target");
    assert!(md_output.anchors.iter().any(|anchor| anchor.label == rst_reftarget),
        "MyST anchors {:?} do not contain RST xref target {rst_reftarget:?}", md_output.anchors);

    // The MyST file's `{ref}` target must name the RST file's target.
    let (_, md_xref) = md_output
        .document
        .iter()
        .find(|(_, node)| node.kind == NodeKind::PendingXref)
        .expect("MyST pending xref");
    let md_reftarget = md_xref.attrs.get_str("reftarget").unwrap().unwrap();
    assert_eq!(md_reftarget, "rst-target");
    let rst_target_names: Vec<String> = rst_output
        .document
        .iter()
        .filter(|(_, node)| node.kind == NodeKind::Target)
        .filter_map(|(_, node)| node.attrs.get_list("names").ok().flatten())
        .flatten()
        .collect();
    assert!(rst_target_names.iter().any(|name| name == md_reftarget),
        "RST targets {rst_target_names:?} do not contain MyST xref target {md_reftarget:?}");

    // The MyST anchor is attached to the anchored heading, and the heading's
    // ids/names carry the same label, so domain resolution (Phase 11–14) can
    // bind either form.
    let anchor = md_output.anchors.iter().find(|a| a.label == "myst-target").unwrap();
    let heading = md_output.document.get(anchor.node_id).unwrap();
    assert_eq!(heading.kind, NodeKind::Section);
    assert!(heading.attrs.get_list("names").unwrap().unwrap().contains(&"myst-target".to_owned()));
    assert_eq!(text(&md_output.document, anchor.node_id), "Anchored Heading");

    // Cross-file refs are still pending after directive resolution; URI
    // resolution is owned by later phases.
    let (md_doc, _) = scribe_myst::parse_and_resolve(&md_source, ParseOptions::new("page.md")).unwrap();
    assert_eq!(nodes_of(&md_doc.document, NodeKind::PendingXref).len(), 1);
    md_doc.document.to_pseudoxml().unwrap();

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn unified_document_set_has_consistent_metadata() {
    let dir = temp_project();
    let rst = parse_rst(
        &fs::read_to_string(dir.join("index.rst")).unwrap(),
        RstParseOptions::new(Some("index.rst".to_owned())),
    );
    let md = parse_with_options(
        &fs::read_to_string(dir.join("page.md")).unwrap(),
        ParseOptions::new("page.md"),
    )
    .unwrap();
    assert_eq!(rst.document.source_name.as_deref(), Some("index.rst"));
    assert_eq!(md.document.source_name.as_deref(), Some("page.md"));
    assert_eq!(md.source_name(), Some("page.md"));
    fs::remove_dir_all(&dir).ok();
}
