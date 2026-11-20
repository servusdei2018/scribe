//! Phase 05 inline-markup seam.
//!
//! Phase 04 emitted every inline string as a single [`NodeKind::Text`] leaf.
//! Phase 05 replaces that with the full recogniser in [`crate::inline`] and
//! materialises the resulting nodes into the arena. This module keeps the
//! public entry point stable while extending it to carry the information the
//! recogniser needs (absolute byte offsets and the document name).

use scribe_ast::{NodeId, Span};

use crate::inline::{InlineNode, InlineSource};
use crate::RstResult;

/// Context handed to the inline parser.
#[derive(Clone, Debug)]
pub struct InlineContext {
    /// 1-based source line of the text, used for diagnostics and node positions.
    pub line: u32,
    /// Byte span of the text in the original (possibly augmented) source.
    pub span: Span,
    /// Parent node that receives the resulting inline children.
    pub parent: NodeId,
    /// Document name, mirrored onto `pending_xref.refdoc` and node `source`.
    pub source_name: Option<String>,
}

impl InlineContext {
    /// Creates a context with no document name (useful in unit tests).
    pub fn new(line: u32, span: Span, parent: NodeId) -> Self {
        Self {
            line,
            span,
            parent,
            source_name: None,
        }
    }
}

/// Parses a single contiguous inline string whose first byte is at `span.start`.
///
/// This is the narrow compatibility entry point retained from the Phase 04
/// seam. Callers that need exact spans across multiple source lines should use
/// [`crate::inline::InlineSource::from_lines`] together with
/// [`crate::inline::parse_inline`] and [`crate::inline::materialize_inline`].
pub fn parse_inline_placeholder(text: &str, ctx: &InlineContext) -> RstResult<Vec<InlineNode>> {
    let source = InlineSource::single(text, ctx.span.start);
    let parsed = crate::inline::parse_inline(&source, &crate::inline::InlineOptions {
        index_counter: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
        source_name: ctx.source_name.clone(),
        extlinks: crate::extlinks::empty_roles(),
        default_role: None,
        default_domain: None,
        custom_roles: std::collections::HashMap::new(),
    });
    Ok(parsed.nodes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seam_expands_emphasis() {
        let ctx = InlineContext::new(1, Span::new(0, 5), NodeId::ROOT);
        let nodes = parse_inline_placeholder("*raw*", &ctx).unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].kind, scribe_ast::NodeKind::Emphasis);
    }
}
