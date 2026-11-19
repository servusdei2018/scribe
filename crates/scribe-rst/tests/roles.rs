//! Phase 06 generic-role registry and node-output tests.

mod common;

use scribe_ast::NodeKind;
use scribe_rst::{parse, resolve_output, Context, DirectiveRegistry, RoleClass, RoleRegistry};

fn pseudo(source: &str) -> String {
    let mut output = parse(source);
    let report = resolve_output(&mut output, &DirectiveRegistry::builtin(), &Context::default());
    assert!(report.is_clean(), "{:?}", report.unresolved);
    output.document.to_pseudoxml().expect("pseudo-XML")
}

#[test]
fn registry_classifies_every_required_role() {
    let registry = RoleRegistry::builtin();
    for name in [
        "ref", "doc", "download", "numref", "envvar", "option", "token", "keyword", "term",
        "abbr", "command", "dfn", "file", "guilabel", "kbd", "mailheader", "makevar", "manpage",
        "menuselection", "mimetype", "newsgroup", "program", "regexp", "samp", "pep", "rfc",
        "math", "eq",
    ] {
        assert!(registry.classify(name).is_some(), "missing role {name}");
    }
    assert!(matches!(
        registry.classify("py:func"),
        Some(RoleClass::Xref { .. })
    ));
}

#[test]
fn semantic_roles_emit_frozen_ast_nodes() {
    let out = pseudo(
        "Roles: :abbr:`HTML (HyperText Markup Language)`, :kbd:`Ctrl-C`, :guilabel:`&OK`, :menuselection:`File --> Open`, :file:`/etc/passwd`, :regexp:`a+`.\n",
    );
    assert!(out.contains("<inline classes=\"abbreviation\" explanation=\"HyperText Markup Language\">"), "{out}");
    assert_eq!(out.matches("<literal classes=\"kbd\">").count(), 2, "{out}");
    assert!(out.contains("<inline classes=\"guilabel\">"), "{out}");
    assert!(out.contains("File \u{25b8} Open"), "{out}");
    assert!(out.contains("role=\"file\""), "{out}");
    assert!(out.contains("<literal class"), "{out}");
}

#[test]
fn pep_and_rfc_emit_external_references() {
    let out = pseudo(":pep:`8` and :rfc:`2119` and :pep:`Custom Title <123>`.\n");
    assert!(out.contains("refuri=\"https://peps.python.org/pep-0008/\""), "{out}");
    assert!(out.contains("refuri=\"https://www.rfc-editor.org/rfc/rfc2119\""), "{out}");
    assert!(out.contains("classes=\"pep\""), "{out}");
    assert!(out.contains("classes=\"rfc\""), "{out}");
    assert!(out.contains("PEP 8"), "{out}");
    assert!(out.contains("RFC 2119"), "{out}");
    assert!(out.contains("Custom Title"), "{out}");
}

#[test]
fn xref_roles_keep_pending_xref_contract_and_side_table() {
    let source = "See :ref:`Label <target>` and :py:func:`.package.module.func` and :doc:`other`.\n";
    let mut output = parse(source);
    let xrefs = output.inline_xrefs().to_vec();
    assert_eq!(xrefs.len(), 3, "{:?}", output.inline_xrefs());
    let by_role = |role: &str| {
        xrefs
            .iter()
            .find(|(_, meta)| meta.role == role)
            .map(|(id, meta)| (*id, meta.clone()))
            .expect("role in side table")
    };
    let (ref_id, ref_meta) = by_role("ref");
    assert!(ref_meta.explicit_title);
    assert_eq!(ref_meta.target, "target");
    let node = output.document.get(ref_id).expect("pending_xref");
    assert_eq!(node.kind, NodeKind::PendingXref);
    let view = node.pending_xref().expect("typed view");
    assert_eq!(view.refdomain(), Some("std"));
    assert_eq!(view.reftype(), Some("ref"));
    assert_eq!(view.reftarget(), Some("target"));

    let (_, py_meta) = by_role("py:func");
    assert_eq!(py_meta.target, "package.module.func");
    assert!(py_meta.specific);

    let report = resolve_output(&mut output, &DirectiveRegistry::builtin(), &Context::default());
    assert!(report.is_clean(), "{:?}", report.unresolved);
    // Phase 11 resolves pending xrefs; Phase 06 keeps them intact.
    let pseudo = output.document.to_pseudoxml().expect("pseudo-XML");
    assert_eq!(pseudo.matches("<pending_xref").count(), 3, "{pseudo}");
}

#[test]
fn math_and_eq_roles() {
    let out = pseudo("Inline :math:`E=mc^2` and :eq:`energy`.\n");
    assert!(out.contains("<math>"), "{out}");
    assert!(out.contains("E=mc^2"), "{out}");
    assert!(out.contains("reftype=\"eq\""), "{out}");
}
