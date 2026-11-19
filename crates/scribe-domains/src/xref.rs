//! Phase 14 parallel cross-reference resolution engine.
//!
//! The engine processes every frozen `pending_xref` in one document (or many
//! documents in parallel with `rayon`) through the full fallback chain:
//!
//! 1. local domain resolution via [`DomainRegistry`];
//! 2. `:any:` cross-domain search;
//! 3. intersphinx inventory lookup (including `:external:` selection);
//! 4. fuzzy "did you mean" suggestions appended to the diagnostic *help*
//!    (the Phase 11 message is never changed);
//! 5. the standard nitpicky diagnostic for anything still unresolved.
//!
//! Arena mutations remain sequential per document (the frozen §7 arena is
//! append-only and `NodeId`s never move), but independent documents are
//! resolved concurrently.

use std::collections::BTreeMap;

use rayon::prelude::*;
use scribe_ast::Document;

use crate::diagnostics::DomainDiagnostic;
use crate::error::DomainResult;
use crate::intersphinx::IntersphinxResolver;
use crate::registry::DomainRegistry;
use crate::resolve::{resolve_pending_xrefs_inner, ResolveReport};
use crate::traits::DomainContext;

/// A fuzzy suggestion for an unresolved reference target.
#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    /// Original unresolved target.
    pub target: String,
    /// Suggested object name.
    pub candidate: String,
    /// Jaro–Winkler similarity in `[0, 1]`.
    pub score: f64,
}

/// Structured report produced by the Phase 14 engine.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct XrefResolutionReport {
    /// Total references replaced by a resolved reference.
    pub resolved: usize,
    /// Resolved by an owning domain.
    pub resolved_local: usize,
    /// Resolved by the `:any:` cross-domain fallback.
    pub resolved_any: usize,
    /// Resolved against an intersphinx inventory.
    pub resolved_intersphinx: usize,
    /// References that fell back to their content node.
    pub unresolved: usize,
    /// References that explicitly requested passthrough.
    pub passed_through: usize,
    /// Fuzzy suggestions produced for unresolved references.
    pub suggestions: Vec<Suggestion>,
    /// Diagnostics in deterministic `(docname, code, message)` order.
    pub diagnostics: Vec<DomainDiagnostic>,
    /// Per-document breakdown (single-document calls use one entry).
    pub per_document: BTreeMap<String, ResolveReport>,
}

impl XrefResolutionReport {
    /// Total number of visited pending xrefs.
    pub fn total(&self) -> usize {
        self.resolved + self.unresolved + self.passed_through
    }

    /// True when no reference was left unresolved.
    pub fn fully_resolved(&self) -> bool {
        self.unresolved == 0
    }
}

/// The configurable resolution engine.
#[derive(Clone, Copy)]
pub struct XrefResolver<'a> {
    registry: &'a DomainRegistry,
    intersphinx: Option<&'a IntersphinxResolver>,
    suggestions: bool,
}

impl<'a> XrefResolver<'a> {
    /// Creates an engine.  When `intersphinx` is `None` only local and `:any:`
    /// resolution (plus the standard diagnostic) are performed.
    pub fn new(registry: &'a DomainRegistry, intersphinx: Option<&'a IntersphinxResolver>) -> Self {
        Self { registry, intersphinx, suggestions: true }
    }

    /// Enables or disables fuzzy suggestions (enabled by default).
    pub fn with_suggestions(mut self, enabled: bool) -> Self {
        self.suggestions = enabled;
        self
    }

    /// True when fuzzy suggestions are enabled.
    pub fn suggestions_enabled(&self) -> bool {
        self.suggestions
    }

    /// Resolves all pending xrefs in one document.
    pub fn resolve_document(
        &self,
        ctx: &DomainContext<'_>,
        document: &mut Document,
    ) -> DomainResult<XrefResolutionReport> {
        let report = resolve_pending_xrefs_inner(self.registry, self.intersphinx, ctx, document)?;
        let docname = document
            .metadata
            .docname
            .clone()
            .unwrap_or_else(|| ctx.docname.to_owned());
        Ok(self.assemble(BTreeMap::from([(docname, report)])))
    }

    /// Resolves all pending xrefs in many documents in parallel.
    ///
    /// Each document gets a per-document `DomainContext` carrying the right
    /// `docname`; the shared registry, inventories and diagnostic sink are
    /// thread-safe.  Results are aggregated in docname order.
    pub fn resolve_documents(
        &self,
        ctx: &DomainContext<'_>,
        documents: &mut BTreeMap<String, Document>,
    ) -> DomainResult<XrefResolutionReport> {
        let mut pairs: Vec<(&String, &mut Document)> = documents.iter_mut().collect();
        let registry = self.registry;
        let intersphinx = self.intersphinx;
        let results: Vec<DomainResult<(String, ResolveReport)>> = pairs
            .par_iter_mut()
            .map(|(docname, document)| {
                let scoped = DomainContext::new(
                    ctx.env,
                    ctx.config,
                    docname.as_str(),
                    ctx.builder_name,
                    ctx.builder_format,
                    ctx.diagnostics(),
                );
                let report = resolve_pending_xrefs_inner(registry, intersphinx, &scoped, document)?;
                Ok(((*docname).clone(), report))
            })
            .collect::<Vec<_>>();
        let results: Vec<(String, ResolveReport)> = results.into_iter().collect::<DomainResult<Vec<_>>>()?;
        Ok(self.assemble(results.into_iter().collect()))
    }

    fn assemble(&self, reports: BTreeMap<String, ResolveReport>) -> XrefResolutionReport {
        let mut result = XrefResolutionReport::default();
        for (docname, report) in reports {
            result.resolved += report.resolved;
            result.resolved_local += report.resolved_local;
            result.resolved_any += report.resolved_any;
            result.resolved_intersphinx += report.resolved_intersphinx;
            result.unresolved += report.unresolved;
            result.passed_through += report.passed_through;
            result.diagnostics.extend(report.diagnostics.iter().cloned());
            result.per_document.insert(docname, report);
        }
        result.diagnostics.sort_by(|a, b| {
            (
                a.docname.as_deref().unwrap_or(""),
                &a.code,
                &a.message,
                a.target.as_deref().unwrap_or(""),
            )
                .cmp(&(
                    b.docname.as_deref().unwrap_or(""),
                    &b.code,
                    &b.message,
                    b.target.as_deref().unwrap_or(""),
                ))
        });
        if self.suggestions {
            let candidates = self.candidates();
            result.suggestions = attach_suggestions(&mut result.diagnostics, &candidates);
        }
        result
    }

    fn candidates(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .registry
            .get_objects()
            .into_iter()
            .map(|entry| entry.name)
            .collect();
        if let Some(resolver) = self.intersphinx {
            names.extend(resolver.candidate_names());
        }
        names.sort();
        names.dedup();
        names
    }
}

/// Ranks fuzzy candidates for `target` by Jaro–Winkler similarity.
///
/// A score of `0.8` mirrors the threshold used by common "did you mean"
/// implementations; results are ordered by descending score with a
/// lexicographic tie-break for determinism.
pub fn suggest(target: &str, candidates: &[String], limit: usize) -> Vec<(String, f64)> {
    let lowered = target.to_lowercase();
    let mut scored: Vec<(String, f64)> = candidates
        .iter()
        .filter_map(|candidate| {
            let score = strsim::jaro_winkler(&lowered, &candidate.to_lowercase());
            (score >= 0.8).then(|| (candidate.clone(), score))
        })
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    scored.truncate(limit);
    scored
}

fn attach_suggestions(diagnostics: &mut [DomainDiagnostic], candidates: &[String]) -> Vec<Suggestion> {
    let mut all = Vec::new();
    for diagnostic in diagnostics.iter_mut() {
        if !diagnostic.is_missing_reference() {
            continue;
        }
        let Some(target) = diagnostic.target.clone() else {
            continue;
        };
        let ranked = suggest(&target, candidates, 3);
        if ranked.is_empty() {
            continue;
        }
        let rendered = ranked
            .iter()
            .map(|(candidate, _)| format!("'{candidate}'"))
            .collect::<Vec<_>>()
            .join(", ");
        let suffix = format!(" Did you mean: {rendered}?");
        diagnostic.help = Some(format!("{}{}", diagnostic.help.clone().unwrap_or_default(), suffix));
        for (candidate, score) in ranked {
            all.push(Suggestion { target: target.clone(), candidate, score });
        }
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_suggestions_are_ranked_and_deterministic() {
        let candidates = vec![
            "some_function".to_owned(),
            "some_functoin".to_owned(),
            "unrelated".to_owned(),
        ];
        let ranked = suggest("some_functon", &candidates, 2);
        assert_eq!(ranked.len(), 2);
        assert_eq!(ranked[0].0, "some_function");
        assert_eq!(suggest("zzzz", &candidates, 3), Vec::new());
    }
}
