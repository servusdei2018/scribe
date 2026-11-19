//! Resolved-phase (post-) transforms (Phase 10).
//!
//! Transcribed from `sphinx/sphinx/transforms/post_transforms/__init__.py`.
//! The two signature transforms are faithful in behaviour but gated by
//! [`TransformContext::sig_element_fallback`] because upstream only performs
//! the fallback when the active translator does *not* support the signature
//! nodes (Sphinx's own `SphinxTranslator` does).

use scribe_ast::{AttributeValue, Document, Node, NodeKind};

use crate::transforms::{
    list_attr, replace_node_preserving_children, str_attr, PostTransform, TransformContext,
    TransformResult,
};

/// `ReferencesResolver` (post priority 10) — resolves every remaining frozen
/// `pending_xref` through the attached [`DomainRegistry`](scribe_domains::DomainRegistry).
///
/// Phase 11 owns standard-domain resolution; Phase 14 will add intersphinx
/// fallback here.  Without a registry the pass is an idempotent no-op.
#[derive(Debug)]
pub struct ReferencesResolver;

impl PostTransform for ReferencesResolver {
    fn name(&self) -> &'static str {
        "ReferencesResolver"
    }

    fn default_priority(&self) -> u32 {
        crate::transforms::priority::REFERENCES_RESOLVER
    }

    fn apply(&self, document: &mut Document, ctx: &TransformContext<'_>) -> TransformResult {
        crate::transforms::run_domains(document, ctx, false)
    }
}

/// `OnlyNodeTransform` (post priority 50) — filter `only` nodes by builder tags.
///
/// Matching nodes are unwrapped into their children; non-matching nodes become
/// empty `comment` nodes so preceding targets do not lose their ids (the exact
/// reason upstream inserts a comment rather than removing the node outright).
#[derive(Debug)]
pub struct OnlyNodeTransform;

impl PostTransform for OnlyNodeTransform {
    fn name(&self) -> &'static str {
        "OnlyNodeTransform"
    }

    fn default_priority(&self) -> u32 {
        crate::transforms::priority::ONLY_NODE
    }

    fn apply(&self, document: &mut Document, ctx: &TransformContext<'_>) -> TransformResult {
        let total = document
            .preorder_ids()
            .iter()
            .filter(|&&id| document.get(id).is_ok_and(|node| node.kind == NodeKind::Only))
            .count();
        let mut changed = false;
        // Re-walk after each replacement so nested `only` nodes are handled
        // even when an outer wrapper was unwrapped or replaced.
        for _ in 0..total {
            let Some(id) = document.preorder_ids().into_iter().find(|&id| {
                document.get(id).is_ok_and(|node| node.kind == NodeKind::Only)
            }) else {
                break;
            };
            let expr = document.get(id).ok().and_then(|node| str_attr(node, "expr")).unwrap_or("").to_owned();
            let keep = match ctx.tags {
                Some(tags) => match tags.eval_condition(&expr) {
                    Ok(value) => value,
                    Err(error) => {
                        ctx.warn("OnlyNodeTransform", format!("exception while evaluating only expression: {error}"), document.get(id).ok().and_then(|node| node.span));
                        true
                    }
                },
                // Without a tag set every expression is treated as true, the
                // same default `process_only_nodes` has when no tags exist.
                None => true,
            };
            if keep {
                if document.replace_with_children(id).is_ok() {
                    changed = true;
                }
                continue;
            }
            // Upstream inserts a fresh `comment` and relies on docutils'
            // id-transfer to keep preceding targets valid. Copy only the
            // id-bearing attributes (not `expr`) so pseudo-XML matches.
            let mut attrs = scribe_ast::Attributes::new();
            if let Ok(node) = document.get(id) {
                for key in ["ids", "names", "dupnames", "classes", "backrefs"] {
                    if let Some(value) = node.attrs.get(key) {
                        attrs.insert(key, value.clone());
                    }
                }
            }
            let comment = Node { kind: NodeKind::Comment, attrs, children: Vec::new(), ..Node::new(NodeKind::Comment) };
            if document.replace_node(id, comment).is_ok() {
                changed = true;
            }
        }
        if changed { TransformResult::changed() } else { TransformResult::unchanged() }
    }
}

/// `SigElementFallbackTransform` (post priority 200) — convert `desc_sig_*`
/// and `desc_inline` nodes to `inline` when the translator lacks visitors.
#[derive(Debug)]
pub struct SigElementFallbackTransform;

impl PostTransform for SigElementFallbackTransform {
    fn name(&self) -> &'static str {
        "SigElementFallbackTransform"
    }

    fn default_priority(&self) -> u32 {
        crate::transforms::priority::SIG_ELEMENT_FALLBACK
    }

    fn apply(&self, document: &mut Document, ctx: &TransformContext<'_>) -> TransformResult {
        if !ctx.sig_element_fallback {
            return TransformResult::unchanged();
        }
        let mut changed = false;
        for id in document.preorder_ids() {
            let Some(tag) = document.get(id).ok().and_then(|node| {
                if is_sig_element(node.kind) {
                    Some(node.kind.tag_name())
                } else {
                    None
                }
            }) else {
                continue;
            };
            let attrs = document.get(id).map_or_else(|_| Default::default(), |node| node.attrs.clone());
            let mut inline = Node::new(NodeKind::Inline);
            inline.attrs = attrs;
            if !matches!(inline.attrs.get("_sig_node_type"), Some(AttributeValue::Str(_))) {
                inline.attrs.set_str("_sig_node_type", tag);
            }
            if replace_node_preserving_children(document, id, inline).is_ok() {
                changed = true;
            }
        }
        if changed { TransformResult::changed() } else { TransformResult::unchanged() }
    }
}

fn is_sig_element(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::DescInline
            | NodeKind::DescSigSpace
            | NodeKind::DescSigName
            | NodeKind::DescSigOperator
            | NodeKind::DescSigPunctuation
            | NodeKind::DescSigKeyword
            | NodeKind::DescSigKeywordType
            | NodeKind::DescSigLiteralNumber
            | NodeKind::DescSigLiteralString
            | NodeKind::DescSigLiteralChar
    )
}

/// `PropagateDescDomain` (post priority 200) — add the parent domain as a CSS
/// class of every `desc_signature`.
#[derive(Debug)]
pub struct PropagateDescDomain;

impl PostTransform for PropagateDescDomain {
    fn name(&self) -> &'static str {
        "PropagateDescDomain"
    }

    fn default_priority(&self) -> u32 {
        crate::transforms::priority::SIG_ELEMENT_FALLBACK
    }

    fn apply(&self, document: &mut Document, _ctx: &TransformContext<'_>) -> TransformResult {
        let mut changed = false;
        for id in document.preorder_ids() {
            if document.get(id).is_ok_and(|node| node.kind != NodeKind::DescSignature) {
                continue;
            }
            let Some(domain) = document.parent(id).ok().flatten().and_then(|parent| {
                document.get(parent).ok().and_then(|node| str_attr(node, "domain").map(str::to_owned))
            }) else {
                continue;
            };
            let mut classes = document.get(id).map_or_else(|_| Vec::new(), |node| list_attr(node, "classes"));
            if !classes.contains(&domain) {
                classes.push(domain);
                if let Ok(node) = document.get_mut(id) {
                    node.attrs.set_list("classes", classes);
                }
                changed = true;
            }
        }
        if changed { TransformResult::changed() } else { TransformResult::unchanged() }
    }
}

/// `RemoveTranslatableInline` (post priority 999) — unwraps placeholder
/// `Inline` nodes used by extension-defined translatable markup.
#[derive(Debug)]
pub struct RemoveTranslatableInline;

impl PostTransform for RemoveTranslatableInline {
    fn name(&self) -> &'static str {
        "RemoveTranslatableInline"
    }

    fn default_priority(&self) -> u32 {
        crate::transforms::priority::REMOVE_TRANSLATABLE_INLINE
    }

    fn apply(&self, document: &mut Document, _ctx: &TransformContext<'_>) -> TransformResult {
        crate::transforms::remove_translatable_inlines(document)
    }
}
