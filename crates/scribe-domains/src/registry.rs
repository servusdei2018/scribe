//! Thread-safe domain registry and cross-reference dispatch.
//!
//! Resolution is a two-step contract:
//!
//! 1. [`DomainRegistry::resolve_xref`] / [`resolve_any_xref`] ask each domain
//!    for an owned [`ResolvedReference`].  No AST mutation happens here.
//! 2. [`crate::resolve::resolve_pending_xrefs`] walks the document, applies the
//!    descriptors, and emits diagnostics for unresolved references.
//!
//! This separation keeps the frozen AST borrow rules simple and lets Phase 14
//! add intersphinx fallback between steps 1 and 2.

use std::collections::BTreeMap;
use std::sync::Arc;

use dashmap::DashMap;
use parking_lot::Mutex;
use scribe_ast::{Document, NodeId};

use crate::diagnostics::{codes, DiagnosticSink, DomainDiagnostic, DomainSeverity};
use crate::error::DomainResult;
use crate::intersphinx::IntersphinxResolver;
use crate::std::{dangling_warning, py_repr, ref_missing_message, StandardDomain};
use crate::traits::{Domain, DomainContext, DomainEnv};
use crate::types::{DomainData, ObjectEntry, ResolvedReference};

/// A domain shared between the registry and its callers.
pub type SharedDomain = Arc<Mutex<Box<dyn Domain>>>;

/// The global registry of domains plus a derived object symbol table.
pub struct DomainRegistry {
    domains: DashMap<String, SharedDomain>,
    objects: DashMap<String, Vec<ObjectEntry>>,
    diagnostics: DiagnosticSink,
    seen_docs: Mutex<std::collections::BTreeSet<String>>,
    /// Optional intersphinx service consulted only after local domain
    /// resolution fails (Phase 14).  Stored on the registry so the frozen
    /// `resolve_pending_xrefs` / `DomainContext` signatures stay unchanged.
    intersphinx: Mutex<Option<Arc<IntersphinxResolver>>>,
}

impl Default for DomainRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for DomainRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut names: Vec<String> = self.domains.iter().map(|entry| entry.key().clone()).collect();
        names.sort();
        f.debug_struct("DomainRegistry")
            .field("domains", &names)
            .field("objects", &self.objects.len())
            .finish_non_exhaustive()
    }
}

impl DomainRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self {
            domains: DashMap::new(),
            objects: DashMap::new(),
            diagnostics: DiagnosticSink::default(),
            seen_docs: Mutex::new(std::collections::BTreeSet::new()),
            intersphinx: Mutex::new(None),
        }
    }

    /// Registry with the standard domain registered (the common builder setup).
    pub fn with_standard() -> Self {
        let registry = Self::new();
        registry.register(StandardDomain::new());
        registry
    }

    /// Registry with every built-in domain (Phase 13).
    ///
    /// `with_standard()` stays standard-only for Phase 11 regression tests;
    /// builders and Phase 12+ callers use `builtin()`.
    pub fn builtin() -> Self {
        let registry = Self::with_standard();
        registry.register(crate::python::PythonDomain::new());
        registry.register(crate::c::CDomain::new());
        registry.register(crate::cpp::CppDomain::new());
        registry.register(crate::javascript::JavaScriptDomain::new());
        registry.register(crate::rst::ReSTDomain::new());
        registry.register(crate::math::MathDomain::new());
        registry.register(crate::changeset::ChangesetDomain::new());
        registry.register(crate::citation::CitationDomain::new());
        registry.register(crate::index::IndexDomain::new());
        registry
    }

    /// Registers (or replaces) a domain by its `name()`.
    pub fn register<D: Domain + 'static>(&self, domain: D) {
        let name = domain.name().to_owned();
        self.domains.insert(name, Arc::new(Mutex::new(Box::new(domain))));
    }

    /// True when a domain with `name` is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.domains.contains_key(name)
    }

    /// Sorted list of registered domain names.
    pub fn domain_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.domains.iter().map(|entry| entry.key().clone()).collect();
        names.sort();
        names
    }

    /// Shared handle for a domain, allowing Phase 12+ code to call its methods
    /// directly.
    pub fn domain(&self, name: &str) -> Option<SharedDomain> {
        self.domains.get(name).map(|entry| Arc::clone(entry.value()))
    }

    /// Runs a read-only closure on a domain.
    pub fn with_domain<R>(&self, name: &str, f: impl FnOnce(&dyn Domain) -> R) -> Option<R> {
        let shared = self.domain(name)?;
        let guard = shared.lock();
        Some(f(guard.as_ref()))
    }

    /// Runs a mutating closure on a domain.
    pub fn with_domain_mut<R>(&self, name: &str, f: impl FnOnce(&mut dyn Domain) -> R) -> Option<R> {
        let shared = self.domain(name)?;
        let mut guard = shared.lock();
        Some(f(guard.as_mut()))
    }

    /// Shared diagnostic sink (also used to build `DomainContext`s).
    pub fn diagnostics(&self) -> &DiagnosticSink {
        &self.diagnostics
    }

    /// Drains all collected diagnostics in emission order.
    pub fn take_diagnostics(&self) -> Vec<DomainDiagnostic> {
        self.diagnostics.take()
    }

    /// Snapshot of diagnostics in emission order (non-destructive).
    pub fn diagnostic_snapshot(&self) -> Vec<DomainDiagnostic> {
        self.diagnostics.snapshot()
    }

    /// Attaches an intersphinx resolver to this registry.
    ///
    /// Once attached, every `resolve_pending_xrefs` call automatically tries
    /// the loaded inventories before emitting a missing-reference diagnostic.
    pub fn set_intersphinx(&self, resolver: IntersphinxResolver) {
        *self.intersphinx.lock() = Some(Arc::new(resolver));
    }

    /// Attaches a shared intersphinx resolver.
    pub fn attach_intersphinx(&self, resolver: Arc<IntersphinxResolver>) {
        *self.intersphinx.lock() = Some(resolver);
    }

    /// Borrows the attached intersphinx resolver, if any.
    pub fn intersphinx(&self) -> Option<Arc<IntersphinxResolver>> {
        self.intersphinx.lock().clone()
    }

    /// Detaches the intersphinx resolver.
    pub fn clear_intersphinx(&self) {
        *self.intersphinx.lock() = None;
    }

    /// Loads a domain inventory from a serialized snapshot.
    ///
    /// A version mismatch is treated as an empty inventory (cold build).
    pub fn load_domain_data(&self, name: &str, data: &DomainData) {
        let Some(shared) = self.domain(name) else {
            return;
        };
        let mut guard = shared.lock();
        if guard.data_version() != data.version {
            return; // stale inventory → cold domain
        }
        let _ = guard.replace_data(data);
    }

    /// Restores every domain whose inventory is present in the env side table.
    pub fn load_from_env(&self, env: &dyn DomainEnv) {
        let names = self.domain_names();
        for name in names {
            if let Some(value) = env.domain_data(&name) {
                if let Ok(data) = serde_json::from_value::<DomainData>(value.clone()) {
                    self.load_domain_data(&name, &data);
                }
            }
        }
    }

    /// Serializes all domain inventories for the env side table.
    pub fn export_domain_data(&self) -> BTreeMap<String, DomainData> {
        let mut result = BTreeMap::new();
        for entry in self.domains.iter() {
            let guard = entry.value().lock();
            if let Ok(data) = guard.export_data() {
                result.insert(entry.key().clone(), data);
            }
        }
        result
    }

    /// True when `docname` has already been registered through [`Self::process_document`].
    ///
    /// Builders register every document once up front (after domain signature
    /// expansion) so cross-document references resolve against the complete
    /// inventory.  The read-phase `SphinxDomains` transform uses this to avoid
    /// registering the same document twice, which would emit spurious duplicate
    /// object warnings.
    pub fn has_seen_doc(&self, docname: &str) -> bool {
        self.seen_docs.lock().contains(docname)
    }

    /// Registers objects found in `document` in every domain.
    pub fn process_document(
        &self,
        ctx: &DomainContext<'_>,
        docname: &str,
        document: &Document,
    ) -> DomainResult<()> {
        self.seen_docs.lock().insert(docname.to_owned());
        let names = self.domain_names();
        for name in names {
            let Some(shared) = self.domain(&name) else {
                continue;
            };
            let mut guard = shared.lock();
            guard.process_doc(ctx, docname, document)?;
        }
        self.rebuild_object_table();
        Ok(())
    }

    /// Removes all of `docname`'s objects from every domain.
    pub fn clear_doc(&self, docname: &str) {
        self.seen_docs.lock().remove(docname);
        for entry in self.domains.iter() {
            entry.value().lock().clear_doc(docname);
        }
        self.rebuild_object_table();
    }

    /// Merges parallel-build inventories for `docnames`.
    pub fn merge_domaindata(&self, docnames: &[String], other: &BTreeMap<String, DomainData>) {
        for (domain, data) in other {
            if let Some(shared) = self.domain(domain) {
                shared.lock().merge_domaindata(docnames, data);
            }
        }
        self.rebuild_object_table();
    }

    /// Rebuilds the derived name → objects symbol table from every domain.
    ///
    /// Entries are sorted by `(name, priority, objtype, docname, anchor)`, so
    /// a lower priority number (more important) wins on a name collision.
    pub fn rebuild_object_table(&self) {
        let objects = self.get_objects();
        let mut grouped: BTreeMap<String, Vec<ObjectEntry>> = BTreeMap::new();
        for entry in objects {
            grouped.entry(entry.name.to_lowercase()).or_default().push(entry);
        }
        for entries in grouped.values_mut() {
            entries.sort_by(|a, b| {
                (a.priority, a.objtype.as_str(), a.docname.as_str(), a.anchor.as_str())
                    .cmp(&(b.priority, b.objtype.as_str(), b.docname.as_str(), b.anchor.as_str()))
            });
        }
        self.objects.clear();
        for (name, entries) in grouped {
            self.objects.insert(name, entries);
        }
    }

    /// Returns the best entries for `name` from the derived symbol table.
    pub fn symbol_lookup(&self, name: &str) -> Vec<ObjectEntry> {
        self.objects
            .get(&name.to_lowercase())
            .map(|entries| entries.clone())
            .unwrap_or_default()
    }

    /// All object entries, in deterministic order (domain name, then the
    /// domain's own ordering contract).
    pub fn get_objects(&self) -> Vec<ObjectEntry> {
        let mut result = Vec::new();
        let mut names = self.domain_names();
        names.sort();
        for name in names {
            let Some(shared) = self.domain(&name) else {
                continue;
            };
            result.extend(shared.lock().get_objects());
        }
        result.sort_by(|a, b| {
            (a.name.as_str(), a.objtype.as_str(), a.docname.as_str(), a.anchor.as_str())
                .cmp(&(b.name.as_str(), b.objtype.as_str(), b.docname.as_str(), b.anchor.as_str()))
        });
        result
    }

    /// Like [`get_objects`](Self::get_objects), with whole-document entries
    /// added from `env` (upstream `StandardDomain.get_objects` yields those
    /// first; here they are sorted into the same deterministic order).
    pub fn get_objects_with_env(&self, env: &dyn DomainEnv) -> Vec<ObjectEntry> {
        let mut result = self.get_objects();
        for docname in env.docnames() {
            let title = env.title(&docname).unwrap_or_else(|| docname.clone());
            result.push(ObjectEntry::new(docname.clone(), title, "doc", docname, "", -1));
        }
        result.sort_by(|a, b| {
            (a.name.as_str(), a.objtype.as_str(), a.docname.as_str(), a.anchor.as_str())
                .cmp(&(b.name.as_str(), b.objtype.as_str(), b.docname.as_str(), b.anchor.as_str()))
        });
        result
    }

    /// Runs `check_consistency` for every domain and merges the diagnostics.
    pub fn check_consistency(&self, ctx: &DomainContext<'_>) -> Vec<DomainDiagnostic> {
        let mut diagnostics = Vec::new();
        for entry in self.domains.iter() {
            let guard = entry.value().lock();
            diagnostics.extend(guard.check_consistency(ctx));
        }
        diagnostics.sort_by_key(|diagnostic| {
            (
                diagnostic.docname.clone().unwrap_or_default(),
                diagnostic.target.clone().unwrap_or_default(),
                diagnostic.code.clone(),
            )
        });
        diagnostics
    }

    /// Resolves a typed xref by dispatching on the pending xref's `refdomain`.
    ///
    /// `typ == "any"` is always dispatched through [`resolve_any_xref`](Self::resolve_any_xref).
    pub fn resolve_xref(
        &self,
        ctx: &DomainContext<'_>,
        typ: &str,
        target: &str,
        node_id: NodeId,
        contnode_id: NodeId,
        document: &Document,
    ) -> DomainResult<Option<ResolvedReference>> {
        if typ == "any" {
            let mut candidates = self.resolve_any_xref(ctx, target, node_id, contnode_id, document)?;
            if candidates.is_empty() {
                return Ok(None);
            }
            if candidates.len() > 1 {
                let rendered: Vec<String> = candidates
                    .iter()
                    .map(|(role, reference)| {
                        let title = reference.title.clone().unwrap_or_else(|| target.to_owned());
                        format!(":{role}:`{title}`")
                    })
                    .collect();
                let mut diagnostic = DomainDiagnostic::malformed(
                    format!(
                        "more than one target found for 'any' cross-reference {}: could be {}",
                        py_repr(target),
                        rendered.join(" or ")
                    ),
                    Some("any".to_owned()),
                    Some(target.to_owned()),
                    Some(ctx.docname.to_owned()),
                    ctx.span_of(document, node_id),
                );
                diagnostic.code = codes::MALFORMED_OBJECT.to_owned();
                if let Ok(node) = document.get(node_id) {
                    if let Some(source) = &node.source {
                        diagnostic.source_path = source.source.clone();
                        diagnostic.line = source.line;
                    }
                }
                ctx.push_diagnostic(diagnostic);
            }
            return Ok(Some(candidates.remove(0).1));
        }
        let domain_name = document
            .get(node_id)
            .ok()
            .and_then(|node| node.pending_xref())
            .and_then(|xref| xref.refdomain().map(str::to_owned))
            .unwrap_or_default();
        // Hand-built documents may omit `refdomain`; treat it as `std`, which
        // owns every short role name in this phase.
        let domain_name = if domain_name.is_empty() { "std" } else { &domain_name };
        let Some(shared) = self.domain(domain_name) else {
            return Ok(None);
        };
        let guard = shared.lock();
        guard.resolve_xref(ctx, typ, target, node_id, contnode_id, document)
    }

    /// Resolves an untyped (`:any:`) xref across all domains, standard first.
    pub fn resolve_any_xref(
        &self,
        ctx: &DomainContext<'_>,
        target: &str,
        node_id: NodeId,
        contnode_id: NodeId,
        document: &Document,
    ) -> DomainResult<Vec<(String, ResolvedReference)>> {
        let mut results = Vec::new();
        let mut domains = self.domain_names();
        domains.sort_by(|a, b| {
            // Upstream gives the standard domain priority.
            (a != "std", a).cmp(&(b != "std", b))
        });
        for name in domains {
            let Some(shared) = self.domain(&name) else {
                continue;
            };
            let guard = shared.lock();
            results.extend(guard.resolve_any_xref(ctx, target, node_id, contnode_id, document)?);
        }
        Ok(results)
    }

    /// Builds the exact warning message for an unresolved reference.
    ///
    /// Mirrors `ReferencesResolver.warn_missing_reference` plus the standard
    /// domain's `warn-missing-reference` event handler.
    pub fn missing_reference_message(
        &self,
        _ctx: &DomainContext<'_>,
        domain: Option<&str>,
        typ: &str,
        target: &str,
        node_id: NodeId,
        document: &Document,
    ) -> Option<String> {
        let domain_name = domain.unwrap_or("");
        // The std domain's `warn-missing-reference` event handler owns :ref:.
        if domain_name == "std" && typ == "ref" {
            let has_anonlabel = self.with_domain("std", |std| {
                std.get_objects().iter().any(|entry| entry.objtype == "label" && entry.name == target.to_lowercase())
            }).unwrap_or(false);
            return Some(ref_missing_message(target, has_anonlabel));
        }
        if let Some(message) = dangling_warning(typ, target) {
            if domain_name == "std" || domain_name.is_empty() {
                return Some(message);
            }
        }
        if !domain_name.is_empty() && domain_name != "std" {
            Some(format!("{domain_name}:{typ} reference target not found: {target}"))
        } else if document.get(node_id).is_ok_and(|node| node.pending_xref().is_some_and(|xref| xref.refdomain().is_none())) {
            // Empty refdomain is the upstream default for `:download:` etc.
            Some(format!("'{typ}' reference target not found: {target}"))
        } else {
            Some(format!("'{typ}' reference target not found: {target}"))
        }
    }

    /// True when `suppress_warnings` disables this reference diagnostic.
    pub fn is_suppressed(&self, ctx: &DomainContext<'_>, typ: &str) -> bool {
        ctx.get_strings_flexible("suppress_warnings")
            .iter()
            .any(|entry| entry == "ref" || entry.as_str() == format!("ref.{typ}") || entry == "ref.*")
    }

    /// True when `nitpick_ignore` disables `(dtype, target)`.
    pub fn is_nitpick_ignored(&self, ctx: &DomainContext<'_>, dtype: &str, target: &str) -> bool {
        for entry in ctx.get_strings_flexible("nitpick_ignore") {
            if let Some((kind, ignored)) = parse_ignore_entry(&entry) {
                if kind == dtype && ignored == target {
                    return true;
                }
                if (dtype == "std:" || !dtype.contains(':')) && kind == dtype.trim_start_matches("std:") && ignored == target {
                    return true;
                }
            }
        }
        false
    }

    /// True when a `nitpick_ignore_regex` pattern matches `(dtype, target)`.
    pub fn is_nitpick_ignored_regex(&self, ctx: &DomainContext<'_>, dtype: &str, target: &str) -> bool {
        for entry in ctx.get_strings_flexible("nitpick_ignore_regex") {
            let Some((kind, pattern)) = parse_ignore_entry(&entry) else {
                continue;
            };
            if let (Ok(kind_re), Ok(target_re)) = (regex::Regex::new(&kind), regex::Regex::new(&pattern)) {
                if kind_re.is_match(dtype) && target_re.is_match(target) {
                    return true;
                }
                if (dtype == "std:" || !dtype.contains(':'))
                    && kind_re.is_match(dtype.trim_start_matches("std:"))
                    && target_re.is_match(target)
                {
                    return true;
                }
            }
        }
        false
    }
}

/// Parses one `nitpick_ignore` entry, accepting `"type:target"` strings as
/// well as two-element Python tuples represented as JSON-ish lists.
pub fn parse_ignore_entry(entry: &str) -> Option<(String, String)> {
    let trimmed = entry.trim();
    if let Some(inner) = trimmed.strip_prefix('[').and_then(|rest| rest.strip_suffix(']')) {
        let values: Vec<String> = inner
            .split(',')
            .map(|part| part.trim().trim_matches(['\'', '"']).to_owned())
            .filter(|part| !part.is_empty())
            .collect();
        if values.len() == 2 {
            return Some((values[0].clone(), values[1].clone()));
        }
        return None;
    }
    let (kind, target) = trimmed.rsplit_once(':')?;
    Some((kind.to_owned(), target.to_owned()))
}

/// Builds a missing-reference diagnostic using the registry's exact messages.
///
/// Returns `None` when upstream would not warn: neither `nitpicky` nor the
/// xref's `refwarn` flag is set, or `suppress_warnings` / `nitpick_ignore`
/// covers the reference.
pub fn missing_reference_diagnostic(
    registry: &DomainRegistry,
    ctx: &DomainContext<'_>,
    domain: Option<&str>,
    typ: &str,
    target: &str,
    node_id: NodeId,
    document: &Document,
) -> Option<DomainDiagnostic> {
    let nitpicky = ctx.nitpicky();
    let refwarn = document
        .get(node_id)
        .ok()
        .and_then(|node| node.pending_xref())
        .and_then(|xref| xref.refwarn())
        .unwrap_or(false);
    if !nitpicky && !refwarn {
        return None;
    }
    if registry.is_suppressed(ctx, typ) {
        return None;
    }
    let dtype = match domain {
        Some(domain) if !domain.is_empty() => format!("{domain}:{typ}"),
        _ => typ.to_owned(),
    };
    if nitpicky {
        if registry.is_nitpick_ignored(ctx, &dtype, target) {
            return None;
        }
        if registry.is_nitpick_ignored(ctx, dtype.trim_start_matches("std:"), target) {
            return None;
        }
        if registry.is_nitpick_ignored_regex(ctx, &dtype, target)
            || registry.is_nitpick_ignored_regex(ctx, dtype.trim_start_matches("std:"), target)
        {
            return None;
        }
    }
    let message = registry
        .missing_reference_message(ctx, domain, typ, target, node_id, document)
        .unwrap_or_else(|| format!("'{typ}' reference target not found: {target}"));
    Some(
        DomainDiagnostic::missing_reference(
            message,
            typ,
            target,
            Some(ctx.docname.to_owned()),
            ctx.span_of(document, node_id),
        )
        .with_source(document.get(node_id).ok().and_then(|node| node.source.as_ref())),
    )
}

/// Severity used for missing-reference diagnostics.
pub fn missing_reference_severity() -> DomainSeverity {
    DomainSeverity::Warning
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ignore_entries() {
        assert_eq!(parse_ignore_entry("py:class:foo"), Some(("py:class".into(), "foo".into())));
        assert_eq!(parse_ignore_entry("  std:doc:index  "), Some(("std:doc".into(), "index".into())));
        assert_eq!(parse_ignore_entry("['py:func', 'bar']"), Some(("py:func".into(), "bar".into())));
        assert_eq!(parse_ignore_entry("garbage"), None);
    }

    #[test]
    fn registry_starts_with_standard_domain() {
        let registry = DomainRegistry::with_standard();
        assert!(registry.contains("std"));
        assert_eq!(registry.domain_names(), ["std"]);
    }
}
