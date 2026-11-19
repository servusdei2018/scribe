//! Shared test helpers for the Phase 15 directive tests.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::Path;

use scribe_ast::{Document, NodeId, NodeKind};
use scribe_builders::code::highlighted_nodes;
use scribe_builders::register_code_directives;
use scribe_rst::directive_registry::IncludeResolver;
use scribe_rst::{
    parse_with_options, resolve_output, Context, DirectiveError, DirectiveRegistry, ParseOptions,
    ResolveReport,
};

/// In-memory include resolver so `literalinclude` tests need no filesystem.
#[derive(Debug, Clone, Default)]
pub struct MemoryResolver {
    /// Files keyed by the path written on the directive.
    pub files: BTreeMap<String, String>,
}

impl IncludeResolver for MemoryResolver {
    fn resolve(
        &self,
        _source_dir: Option<&Path>,
        _source_name: Option<&str>,
        path: &str,
    ) -> Result<String, DirectiveError> {
        self.files.get(path).cloned().ok_or_else(|| DirectiveError::IncludeIo {
            path: path.to_owned(),
            detail: "not found in memory resolver".to_owned(),
        })
    }
}

/// Result of parsing a snippet through the Phase 15 directive registry.
pub struct Parsed {
    /// The resolved document.
    pub document: Document,
    /// Resolve diagnostics.
    pub report: ResolveReport,
}

impl Parsed {
    /// All highlighted blocks in source order.
    pub fn highlighted(&self) -> Vec<(NodeId, scribe_builders::HighlightedCode)> {
        highlighted_nodes(&self.document)
    }

    /// Source text concatenated across all highlighted blocks.
    pub fn code_texts(&self) -> Vec<String> {
        self.highlighted()
            .into_iter()
            .map(|(_, payload)| payload.plain_text())
            .collect()
    }

    /// All messages from resolve diagnostics.
    pub fn issues(&self) -> Vec<String> {
        self.report.unresolved.iter().map(|issue| issue.message.clone()).collect()
    }

    /// Count nodes of a given kind.
    pub fn count(&self, kind: NodeKind) -> usize {
        self.document
            .preorder_ids()
            .into_iter()
            .filter(|&id| self.document.get(id).is_ok_and(|node| node.kind == kind))
            .count()
    }
}

/// Parses `source`, installs the Phase 15 handlers and resolves directives.
pub fn parse(source: &str) -> Parsed {
    parse_with_files(source, BTreeMap::new())
}

/// Like [`parse`] with injectable include-file contents.
pub fn parse_with_files(source: &str, files: BTreeMap<String, String>) -> Parsed {
    let mut output = parse_with_options(source, ParseOptions::default());
    let mut registry = DirectiveRegistry::builtin();
    register_code_directives(&mut registry);
    let context = Context {
        include_resolver: std::sync::Arc::new(MemoryResolver { files }),
        ..Context::default()
    };
    let report = resolve_output(&mut output, &registry, &context);
    Parsed {
        document: output.document,
        report,
    }
}
