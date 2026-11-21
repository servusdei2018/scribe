//! Applies domain resolution results to the frozen AST.
//!
//! This is the only place in `scribe-domains` that mutates a document.  It is
//! called by the Phase 10 `SphinxDomains` (read, priority 850) and
//! `ReferencesResolver` (post, priority 10) seams.  Arena ids are preserved:
//! a `pending_xref` becomes a `reference` *in place*, which keeps any held id
//! valid (frozen §7 invariant).

use scribe_ast::{Document, Node, NodeId, NodeKind};

use crate::diagnostics::DomainDiagnostic;
use crate::error::DomainResult;
use crate::intersphinx::resolve::{
    external_missing_reference_diagnostic, inventory_not_found_diagnostic, IntersphinxResolver,
};
use crate::registry::{missing_reference_diagnostic, DomainRegistry};
use crate::traits::DomainContext;
use crate::types::ResolvedReference;

/// Outcome of one resolution pass over a document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResolveReport {
    /// Number of `pending_xref`s replaced by a resolved reference.
    pub resolved: usize,
    /// Number of references that fell back to their content node.
    pub unresolved: usize,
    /// Number of references that explicitly requested passthrough (e.g. a
    /// disabled `:numref:`), counted separately from `unresolved`.
    pub passed_through: usize,
    /// Resolved by the owning domain (`resolve_xref`).
    pub resolved_local: usize,
    /// Resolved by the `:any:` cross-domain search.
    pub resolved_any: usize,
    /// Resolved against an intersphinx inventory.
    pub resolved_intersphinx: usize,
    /// Diagnostics emitted by domains during this pass.
    pub diagnostics: Vec<DomainDiagnostic>,
}

impl ResolveReport {
    /// Total number of visited `pending_xref` nodes.
    pub fn total(&self) -> usize {
        self.resolved + self.unresolved + self.passed_through
    }
}

/// One pending xref, snapshotted before any mutation happens.
#[derive(Clone, Debug)]
struct XrefPlan {
    id: NodeId,
    typ: String,
    target: String,
    domain: String,
    /// Content for an unresolved fallback or a disabled reference (`*` condition).
    contnode: NodeId,
    /// Content for a successfully resolved reference (`resolved` condition).
    resolved_contnode: Option<NodeId>,
    program: Option<String>,
}

/// Collects every live `pending_xref` in document order.
///
/// `.. program:: name` markers are emitted as `desc` nodes with
/// `objtype = "program"`; walking in document order lets us attach the active
/// `std:program` context to every following `:option:` xref.
fn collect_plans(document: &Document) -> Vec<XrefPlan> {
    let mut plans = Vec::new();
    let mut current_program: Option<String> = None;
    for id in document.preorder_ids() {
        let Ok(node) = document.get(id) else {
            continue;
        };
        if node.kind != NodeKind::PendingXref {
            if node.get_str("objtype").ok().flatten() == Some("program") {
                let name = node
                    .names()
                    .unwrap_or_default()
                    .first()
                    .cloned()
                    .unwrap_or_else(|| node.astext(document).trim().to_owned());
                current_program = if name == "None" || name.is_empty() {
                    None
                } else {
                    Some(name.replace(' ', "-"))
                };
            }
            continue;
        }
        let Some(view) = node.pending_xref() else {
            continue;
        };
        let contnode = pick_content(document, id, Some("*"));
        let resolved_contnode = pick_content(document, id, Some("resolved"));
        plans.push(XrefPlan {
            id,
            typ: view.reftype().unwrap_or("").to_owned(),
            target: view.reftarget().unwrap_or("").to_owned(),
            domain: view.refdomain().unwrap_or("").to_owned(),
            contnode,
            resolved_contnode: (resolved_contnode != NodeId::NULL).then_some(resolved_contnode),
            program: current_program.clone(),
        });
    }
    plans
}

/// Upstream chooses the `resolved` / `*` condition node when present, else
/// the first ordinary child of the `pending_xref`.
fn pick_content(document: &Document, xref: NodeId, wanted: Option<&str>) -> NodeId {
    let Ok(node) = document.get(xref) else {
        return NodeId::NULL;
    };
    let view = node.pending_xref();
    let conditions = view.map_or_else(Vec::new, |xref| xref.resolve_conditions());
    if let Some(wanted) = wanted {
        for id in &conditions {
            if document.get(*id).is_ok_and(|node| node.condition().ok().flatten() == Some(wanted)) {
                if let Some(&child) = document.get(*id).ok().and_then(|node| node.children.first()) {
                    return child;
                }
            }
        }
    }
    node.children
        .iter()
        .copied()
        .find(|&child| document.get(child).is_ok_and(|node| node.kind != NodeKind::PendingXrefCondition))
        .unwrap_or(NodeId::NULL)
}

/// Resolves and rewrites every `pending_xref` in `document`.
///
/// Uses the intersphinx resolver attached to `registry`, if any.  The frozen
/// signatures are unchanged: callers (Phase 10 transforms and Phase 14
/// engines) only need to attach a resolver once via
/// [`DomainRegistry::set_intersphinx`].
pub fn resolve_pending_xrefs(
    registry: &DomainRegistry,
    ctx: &DomainContext<'_>,
    document: &mut Document,
) -> DomainResult<ResolveReport> {
    let intersphinx = registry.intersphinx();
    resolve_pending_xrefs_inner(registry, intersphinx.as_deref(), ctx, document)
}

/// Core resolution loop shared by the public entry point and the Phase 14
/// structured engine in [`crate::xref`].
pub(crate) fn resolve_pending_xrefs_inner(
    registry: &DomainRegistry,
    intersphinx: Option<&IntersphinxResolver>,
    ctx: &DomainContext<'_>,
    document: &mut Document,
) -> DomainResult<ResolveReport> {
    let plans = collect_plans(document);
    // Diagnostics are emitted through the context sink (which Phase 10 wires
    // to the registry sink).  Snapshot that one so the structured report is
    // complete even for callers using a private sink.
    let before = ctx.diagnostics().len();
    let mut report = ResolveReport::default();

    for plan in &plans {
        // Bind the xref's own attributes into a per-reference context.  The
        // `refdoc` is upstream's `node.setdefault('refdoc', fromdocname)`.
        let scoped = ctx.with_program(plan.program.as_deref());
        // Upstream treats a leading `!` on a cross-reference target as "show
        // this name literally, never link it" (and never warn in nitpicky
        // mode).  The parser keeps the marker in `reftarget`, so suppress the
        // reference here instead of emitting a spurious missing-target warning.
        if plan.target.starts_with('!') {
            if replace_with_content(document, plan.id, plan.contnode).is_ok() {
                report.passed_through += 1;
            }
            continue;
        }
        // `:external:`-annotated xrefs are claimed exclusively by intersphinx;
        // upstream's IntersphinxRoleResolver runs before ReferencesResolver.
        let external = document
            .get(plan.id)
            .ok()
            .and_then(|node| node.attrs.get_bool("intersphinx").ok().flatten())
            .unwrap_or(false);
        let domain = if plan.domain.is_empty() { None } else { Some(plan.domain.as_str()) };

        let resolve_local = |target: &str| -> crate::error::DomainResult<Option<crate::types::ResolvedReference>> {
            if plan.domain.is_empty() && plan.typ == "any" {
                Ok(registry
                    .resolve_any_xref(&scoped, target, plan.id, plan.contnode, document)?
                    .into_iter()
                    .next()
                    .map(|(_, reference)| reference))
            } else {
                registry.resolve_xref(&scoped, &plan.typ, target, plan.id, plan.contnode, document)
            }
        };
        let mut result = if external { None } else { resolve_local(&plan.target)? };

        // Upstream `resolve_reference_detect_inventory` rewrites a
        // `self_name:target` reference to `target` and returns `None` so the
        // ordinary resolver retries it against *local* objects.  Mirror that
        // retry here (the intersphinx resolver separately strips the prefix
        // for its own inventory lookup).
        if result.is_none() && !external {
            if let Some(self_name) = scoped
                .get_str("intersphinx_resolve_self")
                .filter(|name| !name.is_empty())
            {
                if let Some(stripped) = plan.target.strip_prefix(&format!("{self_name}:")) {
                    result = resolve_local(stripped)?;
                }
            }
        }

        match result {
            Some(reference) if !reference.passthrough && !reference.is_empty() => {
                let content = plan.resolved_contnode.unwrap_or(plan.contnode);
                if materialize(document, plan.id, content, &reference).is_ok() {
                    report.resolved += 1;
                    if plan.domain.is_empty() && plan.typ == "any" {
                        report.resolved_any += 1;
                    } else {
                        report.resolved_local += 1;
                    }
                } else {
                    report.unresolved += 1;
                }
            }
            Some(_) => {
                if replace_with_content(document, plan.id, plan.contnode).is_ok() {
                    report.passed_through += 1;
                }
            }
            None => {
                let mut resolved_externally = false;
                if let Some(resolver) = intersphinx {
                    if let Some(ext) = resolver.resolve_reference(
                        registry,
                        &scoped,
                        domain,
                        &plan.typ,
                        &plan.target,
                        plan.id,
                        plan.contnode,
                        document,
                    ) {
                        let content = plan.resolved_contnode.unwrap_or(plan.contnode);
                        if materialize(document, plan.id, content, &ext.reference).is_ok() {
                            report.resolved += 1;
                            report.resolved_intersphinx += 1;
                            resolved_externally = true;
                        }
                    }
                }
                if !resolved_externally {
                    let diagnostic = if external {
                        let named = document
                            .get(plan.id)
                            .ok()
                            .and_then(|node| node.get_str("inventory").ok().flatten());
                        if let Some(name) = named.filter(|name| {
                            intersphinx.is_some_and(|resolver| !resolver.contains_inventory(name))
                        }) {
                            Some(inventory_not_found_diagnostic(&scoped, name, plan.id, document))
                        } else {
                            Some(external_missing_reference_diagnostic(
                                &scoped,
                                domain.unwrap_or("std"),
                                &plan.typ,
                                &plan.target,
                                plan.id,
                                document,
                            ))
                        }
                    } else {
                        missing_reference_diagnostic(
                            registry,
                            &scoped,
                            domain,
                            &plan.typ,
                            &plan.target,
                            plan.id,
                            document,
                        )
                    };
                    if let Some(diagnostic) = diagnostic {
                        scoped.push_diagnostic(diagnostic);
                    }
                    if replace_with_content(document, plan.id, plan.contnode).is_ok() {
                        report.unresolved += 1;
                    }
                }
            }
        }
    }

    report.diagnostics.extend(ctx.diagnostics().snapshot()[before..].iter().cloned());
    Ok(report)
}

/// Materializes a [`ResolvedReference`] in place of the pending xref.
fn materialize(
    document: &mut Document,
    id: NodeId,
    contnode: NodeId,
    reference: &ResolvedReference,
) -> Result<(), scribe_ast::AstError> {
    let mut node = Node::new(reference.node_kind.node_kind());
    node.source = document.get(id).ok().and_then(|old| old.source.clone());
    node.span = document.get(id).ok().and_then(|old| old.span);
    if let Some(refid) = &reference.refid {
        node.attrs.set_str("refid", refid);
    }
    if let Some(refuri) = &reference.refuri {
        node.attrs.set_str("refuri", refuri);
    }
    if let Some(reftitle) = &reference.reftitle {
        // Upstream passes the numfig format as `title=` on the generated
        // `number_reference` (see StandardDomain.build_reference_node).
        if reference.node_kind == crate::types::ReferenceNodeKind::NumberReference {
            node.attrs.set_str("title", reftitle);
        } else {
            node.attrs.set_str("reftitle", reftitle);
        }
    }
    node.attrs.set_bool("internal", true);
    for (key, value) in &reference.extra_attrs {
        // `internal` is a boolean attribute in upstream's reference nodes;
        // pseudo-XML renders bool false as "0".  Intersphinx references pass
        // the value through the string-carrying `extra_attrs` map, so map the
        // two boolean spellings back to the frozen AST's `Bool` variant.
        match (key.as_str(), value.as_str()) {
            ("internal", "False") => node.attrs.set_bool("internal", false),
            ("internal", "True") => node.attrs.set_bool("internal", true),
            _ => node.attrs.set_str(key, value),
        }
    }
    if let Some(docname) = &reference.docname {
        node.attrs.set_str("docname", docname);
    }

    if let Some(title) = &reference.title {
        // Upstream's `:any:` role reuses its literal content node, so the
        // inner display is a `<code>` (with `<span class="pre">`), not a span.
        let inner_kind = if reference.classes.iter().any(|class| class == "any") {
            NodeKind::Literal
        } else {
            NodeKind::Inline
        };
        let mut inner = Node::new(inner_kind);
        let mut classes = reference.classes.clone();
        if classes.is_empty() {
            classes.push("xref".to_owned());
        }
        inner.attrs.set_list("classes", classes);
        let text_id = document.add_node(Node::text(title.clone()), None)?;
        inner.children.push(text_id);
        let inner_id = document.add_node(inner, None)?;
        node.children.push(inner_id);
    } else if document.get(contnode).is_ok() {
        // Our parser stores the xref's CSS classes on the pending_xref itself;
        // upstream stores them on the inner content node. Transfer them so the
        // emitted reference keeps `xref std std-*` classes.
        let xref_classes = document
            .get(id)
            .ok()
            .and_then(|node| node.get_list("classes").ok().flatten())
            .unwrap_or_default();
        let mut classes = xref_classes;
        for class in &reference.classes {
            if !classes.contains(class) {
                classes.push(class.clone());
            }
        }
        if let Ok(content) = document.get_mut(contnode) {
            if content.get_list("classes").ok().flatten().unwrap_or_default().is_empty()
                && !classes.is_empty()
            {
                content.attrs.set_list("classes", classes);
            }
        }
        node.children.push(contnode);
    } else if let Ok(old) = document.get(id) {
        node.children.extend(old.children.iter().copied().filter(|&child| {
            document.get(child).is_ok_and(|child| child.kind != NodeKind::PendingXrefCondition)
        }));
    }
    document.replace_node(id, node)
}

/// Replaces a pending xref with its content node (upstream's fallback).
fn replace_with_content(document: &mut Document, id: NodeId, contnode: NodeId) -> Result<(), scribe_ast::AstError> {
    let Ok(content) = document.get(contnode).cloned() else {
        let empty = Node::new(NodeKind::Inline);
        return document.replace_node(id, empty);
    };
    let mut replacement = Node::new(content.kind);
    replacement.attrs = content.attrs;
    replacement.source = content.source;
    replacement.span = content.span;
    replacement.children = content.children;
    let xref_classes = document
        .get(id)
        .ok()
        .and_then(|node| node.get_list("classes").ok().flatten())
        .unwrap_or_default();
    if replacement.get_list("classes").ok().flatten().unwrap_or_default().is_empty()
        && !xref_classes.is_empty()
    {
        replacement.attrs.set_list("classes", xref_classes);
    }
    document.replace_node(id, replacement)
}
