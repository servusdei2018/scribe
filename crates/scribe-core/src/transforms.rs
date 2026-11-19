//! Priority-ordered AST transform pipeline (Phase 10).
//!
//! This module is the stable seam builders call after parsing/resolution and
//! before writing. It mirrors Sphinx's two application points:
//!
//! * [`Phase::Read`] — the doctree transforms Sphinx runs while *reading* a
//!   document (`document.settings.env` is available). Canonical priorities are
//!   transcribed from `sphinx/sphinx/transforms/{__init__,references,
//!   compact_bullet_list,i18n}.py`.
//! * [`Phase::Resolved`] — the *post-transforms* Sphinx runs just before a
//!   writer (`sphinx/sphinx/transforms/post_transforms/__init__.py`).
//!
//! The frozen §7 AST and §12 `BuildEnvironment` shapes are only read or
//! mutated through their public APIs; no new node kind is introduced.
//!
//! # Determinism
//!
//! A registry executes in a stable sort by `(priority, name)`. Registration
//! order therefore never affects the result, and two transforms sharing a
//! priority (upstream has several at 210 and 500) run in alphabetical name
//! order. Entries can be disabled/re-enabled by name; a
//! [`TransformReport`] records exactly what ran and what was skipped.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::sync::Arc;

use miette::{Diagnostic, SourceSpan};
use scribe_ast::{AttributeValue, Document, Node, NodeId, NodeKind, Span};
use scribe_config::{Config, Tags};
use scribe_domains::{
    prepare_document, resolve_pending_xrefs, DiagnosticSink, DomainContext, DomainDiagnostic,
    DomainRegistry,
};
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;

use crate::env::BuildEnvironment;
use crate::i18n::{
    apply_locale, clean_extracted_message, TranslationCatalog, TranslationProgress,
};
use crate::post_transforms::{
    OnlyNodeTransform, PropagateDescDomain, ReferencesResolver, SigElementFallbackTransform,
};
use crate::smartquotes::{educate, language_supports_smartquotes};

// ---------------------------------------------------------------------------
// Canonical upstream priorities (documented in ARCHITECTURE.md §14).
// ---------------------------------------------------------------------------

/// Priority constants transcribed verbatim from the pinned oracle.
pub mod priority {
    /// `sphinx.transforms.ApplySourceWorkaround` / `i18n.PreserveTranslatableMessages`.
    pub const APPLY_SOURCE_WORKAROUND: u32 = 10;
    /// `sphinx.transforms.ExtraTranslatableNodes`.
    pub const EXTRA_TRANSLATABLE_NODES: u32 = 10;
    /// `sphinx.transforms.references.ReferencesResolver` (post-transform).
    pub const REFERENCES_RESOLVER: u32 = 10;
    /// `sphinx.transforms.i18n.Locale`.
    pub const LOCALE: u32 = 20;
    /// `sphinx.transforms.i18n.TranslationProgressTotaliser`.
    pub const TRANSLATION_PROGRESS_TOTALISER: u32 = 25;
    /// `sphinx.transforms.compact_bullet_list.RefOnlyBulletListTransform`.
    pub const REF_ONLY_BULLET_LIST: u32 = 100;
    /// `sphinx.transforms.DefaultSubstitutions` and friends.
    pub const DEFAULT_SUBSTITUTIONS: u32 = 210;
    /// `sphinx.transforms.ReorderConsecutiveTargetAndIndexNodes`.
    pub const REORDER_TARGETS_AND_INDEX: u32 = 220;
    /// Docutils `PropagateTargets` (priority 260).
    pub const PROPAGATE_TARGETS: u32 = 260;
    /// `sphinx.transforms.SortIds`.
    pub const SORT_IDS: u32 = 261;
    /// `sphinx.transforms.DoctestTransform` / `GlossarySorter`.
    pub const DOCTEST: u32 = 500;
    /// Docutils `Footnotes` (priority 620).
    pub const FOOTNOTES: u32 = 620;
    /// `sphinx.transforms.SphinxSmartQuotes`.
    pub const SMART_QUOTES: u32 = 750;
    /// `sphinx.transforms.references.SphinxDomains` — Phase 14 seam.
    pub const SPHINX_DOMAINS: u32 = 850;
    /// `sphinx.transforms.DoctreeReadEvent`.
    pub const DOCTREE_READ_EVENT: u32 = 880;
    /// `sphinx.transforms.i18n.AddTranslationClasses`.
    pub const ADD_TRANSLATION_CLASSES: u32 = 950;
    /// `sphinx.transforms.FilterSystemMessages` / `i18n.RemoveTranslatableInline`.
    pub const FILTER_SYSTEM_MESSAGES: u32 = 999;
    /// Sphinx `MetadataCollector` (doctree-read env collector). Runs after all
    /// read transforms so the bibliographic preamble can be stripped safely.
    pub const DOCINFO: u32 = 1000;
    /// `sphinx.transforms.post_transforms.OnlyNodeTransform`.
    pub const ONLY_NODE: u32 = 50;
    /// Post-transforms `SigElementFallbackTransform` / `PropagateDescDomain`.
    pub const SIG_ELEMENT_FALLBACK: u32 = 200;
    /// `sphinx.transforms.i18n.RemoveTranslatableInline` (post seam).
    pub const REMOVE_TRANSLATABLE_INLINE: u32 = 999;
}

// ---------------------------------------------------------------------------
// Diagnostics and results
// ---------------------------------------------------------------------------

/// Severity of a recoverable transform diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransformSeverity {
    /// Informational note (does not indicate a problem).
    Info,
    /// A recoverable problem; the transform best-effort continued.
    Warning,
    /// An error that could not be fully recovered from.
    Error,
}

impl std::fmt::Display for TransformSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransformSeverity::Info => f.write_str("info"),
            TransformSeverity::Warning => f.write_str("warning"),
            TransformSeverity::Error => f.write_str("error"),
        }
    }
}

/// A recoverable diagnostic emitted while a transform runs.
///
/// Carries the transform name, a human-readable message and (when known) the
/// byte span of the offending node. The optional `source` is the document text
/// attached by builders so `miette` can render a labelled snippet.
#[derive(Clone, Debug, Error, Diagnostic)]
#[error("{severity} [{transform}]: {message}")]
#[diagnostic(code(scribe::transform))]
pub struct TransformDiagnostic {
    /// Name of the transform that produced the diagnostic.
    pub transform: String,
    /// Human-readable explanation.
    pub message: String,
    /// Severity.
    pub severity: TransformSeverity,
    /// Source text, when the caller attached it for rendering.
    #[source_code]
    pub source_text: Option<String>,
    /// Byte span of the offending node, when known.
    #[label("here")]
    pub span: Option<SourceSpan>,
}

impl TransformDiagnostic {
    /// Creates a diagnostic with an optional AST span.
    pub fn new(
        transform: impl Into<String>,
        severity: TransformSeverity,
        message: impl Into<String>,
        span: Option<Span>,
    ) -> Self {
        Self {
            transform: transform.into(),
            message: message.into(),
            severity,
            source_text: None,
            span: span.map(|s| SourceSpan::new((s.start as usize).into(), s.len() as usize)),
        }
    }

    /// Creates a warning diagnostic.
    pub fn warning(transform: impl Into<String>, message: impl Into<String>, span: Option<Span>) -> Self {
        Self::new(transform, TransformSeverity::Warning, message, span)
    }

    /// Attaches the source text so `miette` can render a labelled snippet.
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source_text = Some(source.into());
        self
    }
}

/// A fatal transform error (reserved for engine-level failures).
///
/// Individual built-in transforms are best-effort and report problems through
/// [`TransformDiagnostic`] instead of failing the document.
#[derive(Debug, Error)]
pub enum TransformError {
    /// The requested transform name is not registered.
    #[error("unknown transform: {0}")]
    UnknownTransform(String),
    /// An AST operation failed while applying a transform.
    #[error("AST error in transform `{transform}`: {source}")]
    Ast {
        /// Transform name.
        transform: String,
        /// Underlying AST error.
        #[source]
        source: scribe_ast::AstError,
    },
}

/// Result of applying one transform to one document.
#[derive(Clone, Debug, Default)]
pub struct TransformResult {
    /// `true` when the transform changed the document.
    pub changed: bool,
    /// Recoverable diagnostics produced by this invocation.
    pub diagnostics: Vec<TransformDiagnostic>,
}

impl TransformResult {
    /// A no-change result.
    pub fn unchanged() -> Self {
        Self::default()
    }

    /// A result that marks the document as changed.
    pub fn changed() -> Self {
        Self { changed: true, diagnostics: Vec::new() }
    }

    /// Adds a diagnostic to this result.
    pub fn with_diagnostic(mut self, diagnostic: TransformDiagnostic) -> Self {
        self.diagnostics.push(diagnostic);
        self
    }
}

// ---------------------------------------------------------------------------
// Traits
// ---------------------------------------------------------------------------

/// A read-phase AST transform.
///
/// Implementations must be panic-free on arbitrary (including malformed)
/// documents and use only the frozen AST's checked APIs.
pub trait Transform: Send + Sync {
    /// Stable upstream name used for ordering, reporting and enable/disable.
    fn name(&self) -> &'static str;
    /// Upstream `default_priority`.
    fn default_priority(&self) -> u32;
    /// Applies the transform.
    fn apply(&self, document: &mut Document, ctx: &TransformContext<'_>) -> TransformResult;
}

/// A resolved-phase (post-) transform.
pub trait PostTransform: Send + Sync {
    /// Stable upstream name.
    fn name(&self) -> &'static str;
    /// Upstream `default_priority`.
    fn default_priority(&self) -> u32;
    /// Applies the post-transform.
    fn apply(&self, document: &mut Document, ctx: &TransformContext<'_>) -> TransformResult;
}

// ---------------------------------------------------------------------------
// Context
// ---------------------------------------------------------------------------

/// Inputs shared by every transform in one pipeline run.
///
/// Holds the resolved configuration and tags, the builder name/format, an
/// optional reference to the frozen build environment, and a diagnostic sink.
/// `RefCell` is used because transforms receive `&TransformContext` while still
/// needing to append diagnostics; a pipeline run is always sequential for a
/// single document, so no locking is required.
pub struct TransformContext<'a> {
    /// Resolved Sphinx configuration (`None` means "use upstream defaults").
    pub config: Option<&'a Config>,
    /// Build tags used by `OnlyNodeTransform`.
    pub tags: Option<&'a Tags>,
    /// Builder name (`html`, `pseudoxml`, …) used by exclusion rules.
    pub builder_name: &'a str,
    /// Builder format (`html`, `latex`, …) used by exclusion rules.
    pub builder_format: &'a str,
    /// Frozen build environment, when available (Phase 14 domain seam).
    pub env: Option<&'a BuildEnvironment>,
    /// Effective language code, derived from config unless overridden.
    pub language: String,
    /// Whether `DescSig*` nodes fall back to `Inline` (translator-dependent).
    pub sig_element_fallback: bool,
    /// Domain registry used by `SphinxDomains` / `ReferencesResolver`. When
    /// absent both seams are no-ops, preserving the Phase 10 behaviour.
    pub domains: Option<&'a DomainRegistry>,
    /// Loaded gettext catalog for the current document's domain (Phase 26).
    /// When absent the i18n transforms are inert, preserving the Phase 10/25
    /// behaviour for projects without translations.
    pub translations: Option<&'a TranslationCatalog>,
    diagnostics: RefCell<Vec<TransformDiagnostic>>,
    domain_diagnostics: DiagnosticSink,
}

impl<'a> TransformContext<'a> {
    /// Creates a context. `language` defaults to `config.language` or `"en"`.
    pub fn new(
        config: Option<&'a Config>,
        tags: Option<&'a Tags>,
        builder_name: &'a str,
        builder_format: &'a str,
    ) -> Self {
        let language = config
            .and_then(|c| c.get_str("language"))
            .filter(|lang| !lang.is_empty())
            .unwrap_or("en")
            .to_owned();
        Self {
            config,
            tags,
            builder_name,
            builder_format,
            env: None,
            language,
            sig_element_fallback: false,
            domains: None,
            translations: None,
            diagnostics: RefCell::new(Vec::new()),
            domain_diagnostics: DiagnosticSink::default(),
        }
    }

    /// Attaches a domain registry, enabling cross-reference resolution in the
    /// `SphinxDomains` (read) and `ReferencesResolver` (post) seams.
    pub fn with_domains(mut self, domains: &'a DomainRegistry) -> Self {
        self.domains = Some(domains);
        self
    }

    /// Attaches the frozen build environment.
    pub fn with_env(mut self, env: &'a BuildEnvironment) -> Self {
        self.env = Some(env);
        self
    }

    /// Overrides the language code.
    pub fn with_language(mut self, language: impl Into<String>) -> Self {
        self.language = language.into();
        self
    }

    /// Attaches the loaded translation catalog for this document (Phase 26).
    pub fn with_translations(mut self, catalog: &'a TranslationCatalog) -> Self {
        self.translations = Some(catalog);
        self
    }

    /// Enables or disables signature-element fallback (post-transform).
    pub fn with_sig_element_fallback(mut self, enabled: bool) -> Self {
        self.sig_element_fallback = enabled;
        self
    }

    /// Pushes a diagnostic into the shared sink.
    pub fn push_diagnostic(&self, diagnostic: TransformDiagnostic) {
        self.diagnostics.borrow_mut().push(diagnostic);
    }

    /// Pushes a warning attributed to `transform`.
    pub fn warn(&self, transform: &str, message: impl Into<String>, span: Option<Span>) {
        self.push_diagnostic(TransformDiagnostic::warning(transform, message, span));
    }

    /// Removes and returns all diagnostics accumulated so far.
    pub fn take_diagnostics(&self) -> Vec<TransformDiagnostic> {
        std::mem::take(&mut self.diagnostics.borrow_mut())
    }

    /// Returns a copy of the diagnostics accumulated so far.
    pub fn diagnostics(&self) -> Vec<TransformDiagnostic> {
        self.diagnostics.borrow().clone()
    }

    /// Pushes a domain diagnostic into the domain sink.
    pub fn push_domain_diagnostic(&self, diagnostic: DomainDiagnostic) {
        self.domain_diagnostics.push(diagnostic);
    }

    /// Removes and returns all domain diagnostics accumulated so far.
    pub fn take_domain_diagnostics(&self) -> Vec<DomainDiagnostic> {
        self.domain_diagnostics.take()
    }

    /// Returns a copy of the domain diagnostics accumulated so far.
    pub fn domain_diagnostics(&self) -> Vec<DomainDiagnostic> {
        self.domain_diagnostics.snapshot()
    }

    /// Domain diagnostic sink (used when constructing a `DomainContext`
    /// manually).
    pub fn domain_sink(&self) -> &DiagnosticSink {
        &self.domain_diagnostics
    }

    /// Builds a [`DomainContext`] for `docname` with this context's inputs.
    pub fn domain_context<'b>(&'b self, docname: &'b str) -> DomainContext<'b>
    where
        'a: 'b,
    {
        DomainContext::new(
            self.env.map(|env| env as &dyn scribe_domains::DomainEnv),
            self.config,
            docname,
            self.builder_name,
            self.builder_format,
            &self.domain_diagnostics,
        )
    }
}

// ---------------------------------------------------------------------------
// Registry and pipeline
// ---------------------------------------------------------------------------

/// Which of Sphinx's two transform application points to run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Doctree transforms run while reading a document.
    Read,
    /// Post-transforms run just before writing.
    Resolved,
}

impl Phase {
    /// Alias for [`Phase::Read`].
    pub fn read() -> Self {
        Phase::Read
    }

    /// Alias for [`Phase::Resolved`].
    pub fn resolved() -> Self {
        Phase::Resolved
    }
}

/// One entry in a deterministic execution plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedTransform {
    /// Transform name.
    pub name: &'static str,
    /// Effective priority.
    pub priority: u32,
    /// Application point.
    pub phase: Phase,
    /// Whether the entry is currently enabled.
    pub enabled: bool,
}

#[derive(Clone)]
struct ReadEntry {
    transform: Arc<dyn Transform>,
    priority: u32,
}

#[derive(Clone)]
struct PostEntry {
    transform: Arc<dyn PostTransform>,
    priority: u32,
}

/// An ordered, enable/disable-able collection of transforms.
#[derive(Clone)]
pub struct TransformRegistry {
    read: Vec<ReadEntry>,
    post: Vec<PostEntry>,
    disabled: BTreeSet<String>,
}

impl Default for TransformRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl TransformRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self { read: Vec::new(), post: Vec::new(), disabled: BTreeSet::new() }
    }

    /// Registers a read transform with its declared default priority.
    pub fn register_read<T: Transform + 'static>(&mut self, transform: T) {
        let priority = transform.default_priority();
        self.read.push(ReadEntry { transform: Arc::new(transform), priority });
    }

    /// Registers a read transform with an explicit priority (used by tests).
    pub fn register_read_with_priority<T: Transform + 'static>(&mut self, transform: T, priority: u32) {
        self.read.push(ReadEntry { transform: Arc::new(transform), priority });
    }

    /// Registers a post-transform with its declared default priority.
    pub fn register_post<T: PostTransform + 'static>(&mut self, transform: T) {
        let priority = transform.default_priority();
        self.post.push(PostEntry { transform: Arc::new(transform), priority });
    }

    /// Registers a post-transform with an explicit priority (used by tests).
    pub fn register_post_with_priority<T: PostTransform + 'static>(&mut self, transform: T, priority: u32) {
        self.post.push(PostEntry { transform: Arc::new(transform), priority });
    }

    /// Disables a transform by name (idempotent).
    pub fn disable(&mut self, name: &str) {
        self.disabled.insert(name.to_owned());
    }

    /// Re-enables a transform by name (idempotent).
    pub fn enable(&mut self, name: &str) {
        self.disabled.remove(name);
    }

    /// `true` when `name` is currently enabled.
    pub fn is_enabled(&self, name: &str) -> bool {
        !self.disabled.contains(name)
    }

    /// Returns the deterministic execution plan for `phase`.
    ///
    /// Entries are sorted by `(priority, name)`, never by registration order.
    pub fn plan(&self, phase: Phase) -> Vec<PlannedTransform> {
        let mut planned: Vec<PlannedTransform> = match phase {
            Phase::Read => self
                .read
                .iter()
                .map(|entry| PlannedTransform {
                    name: entry.transform.name(),
                    priority: entry.priority,
                    phase,
                    enabled: self.is_enabled(entry.transform.name()),
                })
                .collect(),
            Phase::Resolved => self
                .post
                .iter()
                .map(|entry| PlannedTransform {
                    name: entry.transform.name(),
                    priority: entry.priority,
                    phase,
                    enabled: self.is_enabled(entry.transform.name()),
                })
                .collect(),
        };
        planned.sort_by(|a, b| (a.priority, a.name).cmp(&(b.priority, b.name)));
        planned
    }

    /// Names of read-phase transforms in execution order.
    pub fn read_plan_names(&self) -> Vec<&'static str> {
        self.plan(Phase::Read).into_iter().map(|entry| entry.name).collect()
    }

    /// Names of resolved-phase transforms in execution order.
    pub fn post_plan_names(&self) -> Vec<&'static str> {
        self.plan(Phase::Resolved).into_iter().map(|entry| entry.name).collect()
    }

    /// Registers every built-in read transform, post-transform and i18n seam.
    pub fn builtin() -> Self {
        let mut registry = Self::new();
        // ----- read phase -----
        registry.register_read(ApplySourceWorkaround);
        registry.register_read(ExtraTranslatableNodes);
        registry.register_read(PreserveTranslatableMessages);
        registry.register_read(Locale);
        registry.register_read(TranslationProgressTotaliser);
        registry.register_read(RefOnlyBulletListTransform);
        registry.register_read(DefaultSubstitutions);
        registry.register_read(MoveModuleTargets);
        registry.register_read(HandleCodeBlocks);
        registry.register_read(AutoNumbering);
        registry.register_read(AutoIndexUpgrader);
        registry.register_read(ReorderConsecutiveTargetAndIndexNodes);
        registry.register_read(PropagateTargetsTransform);
        registry.register_read(SortIds);
        registry.register_read(DoctestTransform);
        registry.register_read(GlossarySorter);
        registry.register_read(FootnotesTransform);
        registry.register_read(SphinxSmartQuotes);
        registry.register_read(SphinxDomains);
        registry.register_read(DoctreeReadEvent);
        registry.register_read(AddTranslationClasses);
        registry.register_read(FilterSystemMessages);
        registry.register_read(RemoveTranslatableInlineRead);
        registry.register_read(DocInfoTransform);
        // ----- resolved phase -----
        registry.register_post(ReferencesResolver);
        registry.register_post(OnlyNodeTransform);
        registry.register_post(SigElementFallbackTransform);
        registry.register_post(PropagateDescDomain);
        registry
    }
}

/// One transform's execution record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransformRecord {
    /// Transform name.
    pub name: &'static str,
    /// Effective priority.
    pub priority: u32,
    /// Whether the document changed.
    pub changed: bool,
}

/// Why a transform was skipped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipReason {
    /// Explicitly disabled in the registry.
    Disabled,
}

/// One skipped transform's record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedTransform {
    /// Transform name.
    pub name: &'static str,
    /// Effective priority.
    pub priority: u32,
    /// Reason it was skipped.
    pub reason: SkipReason,
}

/// Report of one pipeline phase over one document.
#[derive(Clone, Debug, Default)]
pub struct TransformReport {
    /// Transforms that ran, in execution order.
    pub applied: Vec<TransformRecord>,
    /// Transforms that were skipped, in execution order.
    pub skipped: Vec<SkippedTransform>,
    /// All diagnostics emitted during the phase.
    pub diagnostics: Vec<TransformDiagnostic>,
    /// Domain diagnostics emitted during the phase (with precise
    /// `scribe::domains::*` codes).
    pub domain_diagnostics: Vec<DomainDiagnostic>,
}

impl TransformReport {
    /// Names of the transforms that ran.
    pub fn applied_names(&self) -> Vec<&'static str> {
        self.applied.iter().map(|record| record.name).collect()
    }

    /// Names of the transforms that changed the document.
    pub fn changed_names(&self) -> Vec<&'static str> {
        self.applied.iter().filter(|record| record.changed).map(|record| record.name).collect()
    }

    /// Names of the skipped transforms.
    pub fn skipped_names(&self) -> Vec<&'static str> {
        self.skipped.iter().map(|record| record.name).collect()
    }

    /// `true` when no transform ran.
    pub fn is_empty(&self) -> bool {
        self.applied.is_empty()
    }

    /// Domain diagnostics emitted during the phase.
    pub fn domain_diagnostics(&self) -> &[DomainDiagnostic] {
        &self.domain_diagnostics
    }

    /// `true` when at least one diagnostic was emitted.
    pub fn has_diagnostics(&self) -> bool {
        !self.diagnostics.is_empty() || !self.domain_diagnostics.is_empty()
    }
}

/// Executes a [`TransformRegistry`] against documents.
#[derive(Clone)]
pub struct TransformPipeline {
    registry: TransformRegistry,
}

impl TransformPipeline {
    /// Builds a pipeline from `registry`.
    pub fn new(registry: TransformRegistry) -> Self {
        Self { registry }
    }

    /// Builds a pipeline containing all built-in transforms.
    pub fn with_builtins() -> Self {
        Self::new(TransformRegistry::builtin())
    }

    /// Borrows the registry.
    pub fn registry(&self) -> &TransformRegistry {
        &self.registry
    }

    /// Mutably borrows the registry.
    pub fn registry_mut(&mut self) -> &mut TransformRegistry {
        &mut self.registry
    }

    /// Runs one phase in deterministic priority order.
    pub fn run(&self, document: &mut Document, ctx: &TransformContext<'_>, phase: Phase) -> TransformReport {
        let mut report = TransformReport::default();
        let plan = self.registry.plan(phase);
        for entry in plan {
            if !entry.enabled {
                report.skipped.push(SkippedTransform {
                    name: entry.name,
                    priority: entry.priority,
                    reason: SkipReason::Disabled,
                });
                continue;
            }
            let before = ctx.diagnostics.borrow().len();
            let domain_before = ctx.domain_diagnostics().len();
            let result = match phase {
                Phase::Read => self
                    .registry
                    .read
                    .iter()
                    .find(|item| item.transform.name() == entry.name)
                    .map(|item| item.transform.apply(document, ctx)),
                Phase::Resolved => self
                    .registry
                    .post
                    .iter()
                    .find(|item| item.transform.name() == entry.name)
                    .map(|item| item.transform.apply(document, ctx)),
            };
            let mut result = result.unwrap_or_default();
            // Fold diagnostics pushed into the context by the transform into
            // this entry's result, preserving the order they were emitted.
            let context_diagnostics = ctx.diagnostics();
            report
                .diagnostics
                .extend(context_diagnostics[before..].iter().cloned());
            let domain_after = ctx.domain_diagnostics();
            if domain_after.len() > domain_before {
                report
                    .domain_diagnostics
                    .extend(domain_after[domain_before..].iter().cloned());
            }
            report.diagnostics.append(&mut result.diagnostics);
            report.applied.push(TransformRecord {
                name: entry.name,
                priority: entry.priority,
                changed: result.changed,
            });
        }
        report
    }

    /// Runs the read phase.
    pub fn run_read(&self, document: &mut Document, ctx: &TransformContext<'_>) -> TransformReport {
        self.run(document, ctx, Phase::Read)
    }

    /// Runs the resolved (post-) phase.
    pub fn run_resolved(&self, document: &mut Document, ctx: &TransformContext<'_>) -> TransformReport {
        self.run(document, ctx, Phase::Resolved)
    }
}

/// Applies the built-in pipeline to `document` for one phase.
///
/// This is the single stable integration point for builders (Phase 17+).
pub fn apply_transforms(
    document: &mut Document,
    ctx: &TransformContext<'_>,
    phase: Phase,
) -> TransformReport {
    TransformPipeline::with_builtins().run(document, ctx, phase)
}

// ---------------------------------------------------------------------------
// Shared AST helpers
// ---------------------------------------------------------------------------

pub(crate) fn list_attr(node: &Node, key: &str) -> Vec<String> {
    match node.attrs.get(key) {
        Some(AttributeValue::List(values)) => values.clone(),
        _ => Vec::new(),
    }
}

pub(crate) fn str_attr<'a>(node: &'a Node, key: &str) -> Option<&'a str> {
    match node.attrs.get(key) {
        Some(AttributeValue::Str(value)) => Some(value.as_str()),
        _ => None,
    }
}

fn int_attr(node: &Node, key: &str) -> Option<i64> {
    match node.attrs.get(key) {
        Some(AttributeValue::Int(value)) => Some(*value),
        _ => None,
    }
}

fn set_list_attr(node: &mut Node, key: &str, values: Vec<String>) {
    if values.is_empty() {
        node.attrs.remove(key);
    } else {
        node.attrs.set_list(key, values);
    }
}

/// Replaces `id` in the tree with `new_node`, preserving children and position.
pub(crate) fn replace_node_preserving_children(
    document: &mut Document,
    id: NodeId,
    new_node: Node,
) -> Result<(), scribe_ast::AstError> {
    let Some(parent) = document.parent(id)? else {
        return Ok(());
    };
    let index = document
        .children(parent)?
        .iter()
        .position(|&child| child == id)
        .ok_or(scribe_ast::AstError::NotLive { id: id.0 })?;
    let children = document.children(id)?.to_vec();
    let new_id = document.add_node(new_node, None)?;
    for child in children {
        document.append_child(new_id, child)?;
    }
    document.splice_children(parent, index..index + 1, vec![new_id])?;
    document.remove_node(id)?;
    Ok(())
}

/// Replaces `id` with a `Text` leaf carrying `text`.
fn replace_with_text(
    document: &mut Document,
    id: NodeId,
    text: impl Into<String>,
) -> Result<(), scribe_ast::AstError> {
    replace_node_preserving_children(document, id, Node::text(text))
}

/// Collects every live, non-root node whose kind matches `kind`.
fn nodes_of_kind(document: &Document, kind: NodeKind) -> Vec<NodeId> {
    document
        .preorder_ids()
        .into_iter()
        .filter(|&id| id != document.root && document.get(id).is_ok_and(|node| node.kind == kind))
        .collect()
}

/// Generates an `idN` identifier not present anywhere in `document`.
fn fresh_id(_document: &Document, used: &mut BTreeSet<String>) -> String {
    let mut n = 1u64;
    loop {
        let candidate = format!("id{n}");
        if !used.contains(&candidate) {
            used.insert(candidate.clone());
            return candidate;
        }
        n += 1;
    }
}

fn collect_used_ids(document: &Document) -> BTreeSet<String> {
    let mut used = BTreeSet::new();
    for (_, node) in document.iter() {
        for value in list_attr(node, "ids") {
            used.insert(value);
        }
    }
    used
}

// ---------------------------------------------------------------------------
// Read-phase built-ins
// ---------------------------------------------------------------------------

/// `ApplySourceWorkaround` (priority 10) — fill missing `source`/`line`
/// metadata from the nearest ancestor, mirroring `sphinx.util.nodes`.
#[derive(Debug)]
pub struct ApplySourceWorkaround;

impl Transform for ApplySourceWorkaround {
    fn name(&self) -> &'static str {
        "ApplySourceWorkaround"
    }

    fn default_priority(&self) -> u32 {
        priority::APPLY_SOURCE_WORKAROUND
    }

    fn apply(&self, document: &mut Document, _ctx: &TransformContext<'_>) -> TransformResult {
        let mut changed = false;
        for id in document.preorder_ids() {
            let Ok(node) = document.get(id) else {
                continue;
            };
            let needs_source = node.source.is_none()
                || (node.source.as_ref().is_some_and(|pos| pos.line.is_none())
                    && node.kind != NodeKind::Document);
            if !needs_source {
                continue;
            }
            let inherited = document
                .ancestors(id)
                .skip(1)
                .find_map(|ancestor| document.get(ancestor).ok().and_then(|parent| parent.source.clone()));
            let Some(inherited) = inherited else {
                continue;
            };
            if let Ok(node) = document.get_mut(id) {
                let merged = match node.source.take() {
                    Some(mut own) => {
                        if own.source.is_none() {
                            own.source = inherited.source;
                        }
                        if own.line.is_none() {
                            own.line = inherited.line;
                        }
                        if own.offset.is_none() {
                            own.offset = inherited.offset;
                        }
                        own
                    }
                    None => inherited,
                };
                node.source = Some(merged);
                changed = true;
            }
        }
        if changed { TransformResult::changed() } else { TransformResult::unchanged() }
    }
}

/// `ExtraTranslatableNodes` (priority 10) — mark configured node kinds as
/// translatable. A thin i18n seam: without `gettext_additional_targets` it is a
/// no-op, exactly like the upstream transform.
#[derive(Debug)]
pub struct ExtraTranslatableNodes;

impl Transform for ExtraTranslatableNodes {
    fn name(&self) -> &'static str {
        "ExtraTranslatableNodes"
    }

    fn default_priority(&self) -> u32 {
        priority::EXTRA_TRANSLATABLE_NODES
    }

    fn apply(&self, document: &mut Document, ctx: &TransformContext<'_>) -> TransformResult {
        let Some(targets) = ctx.config.and_then(|config| config.get_strings("gettext_additional_targets")) else {
            return TransformResult::unchanged();
        };
        if targets.is_empty() {
            return TransformResult::unchanged();
        }
        let mut changed = false;
        for id in document.preorder_ids() {
            let Ok(kind) = document.get(id).map(|node| node.kind) else {
                continue;
            };
            let tag = match kind {
                NodeKind::LiteralBlock => "literal-block",
                NodeKind::DoctestBlock => "doctest-block",
                NodeKind::Raw => "raw",
                NodeKind::Index => "index",
                NodeKind::Image => "image",
                _ => continue,
            };
            if targets.iter().any(|target| target == tag) {
                if let Ok(node) = document.get_mut(id) {
                    node.attrs.set_bool("translatable", true);
                    changed = true;
                }
            }
        }
        if changed { TransformResult::changed() } else { TransformResult::unchanged() }
    }
}

/// `RefOnlyBulletListTransform` (priority 100) — when `html_compact_lists` is
/// false, rewrite lists whose items are a single reference paragraph to use
/// `CompactParagraph`, matching `sphinx/transforms/compact_bullet_list.py`.
#[derive(Debug)]
pub struct RefOnlyBulletListTransform;

impl Transform for RefOnlyBulletListTransform {
    fn name(&self) -> &'static str {
        "RefOnlyBulletListTransform"
    }

    fn default_priority(&self) -> u32 {
        priority::REF_ONLY_BULLET_LIST
    }

    fn apply(&self, document: &mut Document, ctx: &TransformContext<'_>) -> TransformResult {
        if ctx.config.and_then(|config| config.get_bool("html_compact_lists")).unwrap_or(true) {
            return TransformResult::unchanged();
        }
        let mut changed = false;
        for list_id in nodes_of_kind(document, NodeKind::BulletList) {
            let items = document.children(list_id).unwrap_or_default().to_vec();
            if items.is_empty()
                || !items.iter().all(|&item| {
                    document.get(item).is_ok_and(|node| {
                        node.kind == NodeKind::ListItem
                            && node.children.len() == 1
                            && document
                                .get(node.children[0])
                                .is_ok_and(|para| {
                                    para.kind == NodeKind::Paragraph
                                        && para.children.len() == 1
                                        && document.get(para.children[0]).is_ok_and(|child| {
                                            child.kind == NodeKind::PendingXref
                                                || child.kind == NodeKind::Reference
                                        })
                                })
                    })
                })
            {
                continue;
            }
            for item in items {
                let Some(para) = document.children(item).ok().and_then(|c| c.first().copied()) else {
                    continue;
                };
                let Ok(para_attrs) = document.get(para).map(|node| node.attrs.clone()) else {
                    continue;
                };
                let compact = Node { kind: NodeKind::CompactParagraph, attrs: para_attrs, ..Node::new(NodeKind::CompactParagraph) };
                if replace_node_preserving_children(document, para, compact).is_ok() {
                    changed = true;
                }
            }
        }
        if changed { TransformResult::changed() } else { TransformResult::unchanged() }
    }
}

/// `DefaultSubstitutions` (priority 210) — resolve `|version|`, `|release|`
/// and `|today|` when no explicit substitution definition exists.
#[derive(Debug)]
pub struct DefaultSubstitutions;

impl Transform for DefaultSubstitutions {
    fn name(&self) -> &'static str {
        "DefaultSubstitutions"
    }

    fn default_priority(&self) -> u32 {
        priority::DEFAULT_SUBSTITUTIONS
    }

    fn apply(&self, document: &mut Document, ctx: &TransformContext<'_>) -> TransformResult {
        let defined: BTreeSet<String> = nodes_of_kind(document, NodeKind::SubstitutionDefinition)
            .iter()
            .filter_map(|&id| document.get(id).ok())
            .flat_map(|node| list_attr(node, "names"))
            .collect();
        let today = default_today(ctx);
        let mut changed = false;
        for id in nodes_of_kind(document, NodeKind::SubstitutionReference) {
            let Some(name) = document.get(id).ok().and_then(|node| str_attr(node, "refname").map(str::to_owned)) else {
                continue;
            };
            if defined.contains(&name) {
                continue;
            }
            let value = match name.as_str() {
                "version" => ctx.config.and_then(|config| config.get_str("version")).unwrap_or("").to_owned(),
                "release" => ctx.config.and_then(|config| config.get_str("release")).unwrap_or("").to_owned(),
                "today" => today.clone(),
                "translation progress" => translation_progress(document, ctx).to_owned(),
                _ => continue,
            };
            if replace_with_text(document, id, value).is_ok() {
                changed = true;
            }
        }
        if changed { TransformResult::changed() } else { TransformResult::unchanged() }
    }
}

fn default_today(ctx: &TransformContext<'_>) -> String {
    let config = match ctx.config {
        Some(config) => config,
        None => return String::new(),
    };
    let explicit = config.get_str("today").unwrap_or("");
    if !explicit.is_empty() {
        return explicit.to_owned();
    }
    let fmt = config
        .get_str("today_fmt")
        .filter(|fmt| !fmt.is_empty())
        .unwrap_or("%b %d, %Y");
    format_utc_date(fmt)
}

fn translation_progress(document: &Document, _ctx: &TransformContext<'_>) -> String {
    match TranslationProgress::from_document(document) {
        Some(progress) => progress.format(),
        None => "could not calculate translation progress!".to_owned(),
    }
}

/// Minimal deterministic UTC strftime (no external clock dependency), used
/// only when `today` is not set explicitly. Supports the tokens Sphinx's
/// `format_date` commonly uses.
fn format_utc_date(fmt: &str) -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let months_long = [
        "January", "February", "March", "April", "May", "June", "July", "August", "September",
        "October", "November", "December",
    ];
    let mut out = String::new();
    let mut chars = fmt.chars();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some('Y') => { let _ = write!(out, "{year:04}"); }
            Some('y') => { let _ = write!(out, "{:02}", year.rem_euclid(100)); }
            Some('m') => { let _ = write!(out, "{month:02}"); }
            Some('d') => { let _ = write!(out, "{day:02}"); }
            Some('b') => out.push_str(months[(month - 1) as usize]),
            Some('B') => out.push_str(months_long[(month - 1) as usize]),
            Some('H') => { let _ = write!(out, "{:02}", tod / 3600); }
            Some('M') => { let _ = write!(out, "{:02}", (tod % 3600) / 60); }
            Some('S') => { let _ = write!(out, "{:02}", tod % 60); }
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

/// Howard Hinnant's civil-from-days algorithm (UTC, proleptic Gregorian).
fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    (year, month, day)
}

/// `MoveModuleTargets` (priority 210) — move an `ismod` target at section
/// index 2 onto the section's ids, matching upstream's Python-specific pass.
#[derive(Debug)]
pub struct MoveModuleTargets;

impl Transform for MoveModuleTargets {
    fn name(&self) -> &'static str {
        "MoveModuleTargets"
    }

    fn default_priority(&self) -> u32 {
        priority::DEFAULT_SUBSTITUTIONS
    }

    fn apply(&self, document: &mut Document, _ctx: &TransformContext<'_>) -> TransformResult {
        let mut changed = false;
        for target in nodes_of_kind(document, NodeKind::Target) {
            let Some(parent) = document.parent(target).ok().flatten() else {
                continue;
            };
            let Some(index) = document.children(parent).ok().and_then(|c| c.iter().position(|&id| id == target)) else {
                continue;
            };
            let Ok(target_node) = document.get(target) else {
                continue;
            };
            let ids = list_attr(target_node, "ids");
            let is_module = index == 2
                && matches!(target_node.attrs.get("ismod"), Some(AttributeValue::Bool(true)))
                && !ids.is_empty();
            if !is_module || document.get(parent).is_ok_and(|node| node.kind != NodeKind::Section) {
                continue;
            }
            if let Ok(section) = document.get_mut(parent) {
                let mut section_ids = list_attr(section, "ids");
                let mut merged = ids;
                merged.append(&mut section_ids);
                set_list_attr(section, "ids", merged);
            }
            if document.remove_node(target).is_ok() {
                changed = true;
            }
        }
        if changed { TransformResult::changed() } else { TransformResult::unchanged() }
    }
}

/// `HandleCodeBlocks` (priority 210) — unwrap block quotes consisting solely
/// of doctest blocks.
#[derive(Debug)]
pub struct HandleCodeBlocks;

impl Transform for HandleCodeBlocks {
    fn name(&self) -> &'static str {
        "HandleCodeBlocks"
    }

    fn default_priority(&self) -> u32 {
        priority::DEFAULT_SUBSTITUTIONS
    }

    fn apply(&self, document: &mut Document, _ctx: &TransformContext<'_>) -> TransformResult {
        let mut changed = false;
        for quote in nodes_of_kind(document, NodeKind::BlockQuote) {
            let is_doctests = document
                .children(quote)
                .is_ok_and(|children| !children.is_empty() && children.iter().all(|&child| {
                    document.get(child).is_ok_and(|node| {
                        node.kind == NodeKind::DoctestBlock
                            || (node.kind == NodeKind::LiteralBlock && list_attr(node, "classes").iter().any(|class| class == "doctest"))
                    })
                }));
            if is_doctests && document.replace_with_children(quote).is_ok() {
                changed = true;
            }
        }
        if changed { TransformResult::changed() } else { TransformResult::unchanged() }
    }
}

/// `AutoNumbering` (priority 210) — assign implicit ids to captioned figures,
/// tables and captioned code blocks so Phase 09's `numfig` pass can number them.
#[derive(Debug)]
pub struct AutoNumbering;

impl Transform for AutoNumbering {
    fn name(&self) -> &'static str {
        "AutoNumbering"
    }

    fn default_priority(&self) -> u32 {
        priority::DEFAULT_SUBSTITUTIONS
    }

    fn apply(&self, document: &mut Document, _ctx: &TransformContext<'_>) -> TransformResult {
        let mut used = collect_used_ids(document);
        let mut changed = false;
        for id in document.preorder_ids() {
            let Ok(node) = document.get(id) else {
                continue;
            };
            if !list_attr(node, "ids").is_empty() {
                continue;
            }
            let has_title = match node.kind {
                NodeKind::Figure => node.children.iter().any(|&child| {
                    document.get(child).is_ok_and(|child| child.kind == NodeKind::Caption)
                }),
                NodeKind::Table => node.children.iter().any(|&child| {
                    document.get(child).is_ok_and(|child| child.kind == NodeKind::Title)
                }),
                NodeKind::LiteralBlock => document.parent(id).ok().flatten().is_some_and(|parent| {
                    document.get(parent).is_ok_and(|parent| {
                        parent.kind == NodeKind::Container
                            && parent.children.iter().any(|&child| {
                                document.get(child).is_ok_and(|child| child.kind == NodeKind::Caption)
                            })
                    })
                }),
                _ => false,
            };
            if !has_title {
                continue;
            }
            let generated = fresh_id(document, &mut used);
            if let Ok(node) = document.get_mut(id) {
                node.attrs.set_list("ids", vec![generated]);
                changed = true;
            }
        }
        if changed { TransformResult::changed() } else { TransformResult::unchanged() }
    }
}

/// `AutoIndexUpgrader` (priority 210) — upgrade 4-column index entries to the
/// new 5-column form by appending `None`.
#[derive(Debug)]
pub struct AutoIndexUpgrader;

impl Transform for AutoIndexUpgrader {
    fn name(&self) -> &'static str {
        "AutoIndexUpgrader"
    }

    fn default_priority(&self) -> u32 {
        priority::DEFAULT_SUBSTITUTIONS
    }

    fn apply(&self, document: &mut Document, ctx: &TransformContext<'_>) -> TransformResult {
        let mut changed = false;
        for id in nodes_of_kind(document, NodeKind::Index) {
            let entries = document.get(id).map_or_else(|_| Vec::new(), |node| list_attr(node, "entries"));
            if entries.is_empty() {
                continue;
            }
            let mut upgraded = Vec::with_capacity(entries.len());
            let mut any = false;
            for entry in entries {
                if entry.split(',').count() == 4 {
                    upgraded.push(format!("{entry},None"));
                    any = true;
                } else {
                    upgraded.push(entry);
                }
            }
            if any {
                if let Ok(node) = document.get_mut(id) {
                    node.attrs.set_list("entries", upgraded);
                }
                ctx.warn("AutoIndexUpgrader", "4 column based index entry found", document.get(id).ok().and_then(|node| node.span));
                changed = true;
            }
        }
        if changed { TransformResult::changed() } else { TransformResult::unchanged() }
    }
}

/// `ReorderConsecutiveTargetAndIndexNodes` (priority 220) — stable-sort runs
/// of adjacent `Target`/`Index` nodes so indexes precede targets.
#[derive(Debug)]
pub struct ReorderConsecutiveTargetAndIndexNodes;

impl Transform for ReorderConsecutiveTargetAndIndexNodes {
    fn name(&self) -> &'static str {
        "ReorderConsecutiveTargetAndIndexNodes"
    }

    fn default_priority(&self) -> u32 {
        priority::REORDER_TARGETS_AND_INDEX
    }

    fn apply(&self, document: &mut Document, _ctx: &TransformContext<'_>) -> TransformResult {
        let mut changed = false;
        let parents: Vec<NodeId> = document.preorder_ids();
        for parent in parents {
            let Ok(children) = document.children(parent).map(<[NodeId]>::to_vec) else {
                continue;
            };
            let mut cursor = 0;
            while cursor < children.len() {
                let is_pair = |id: NodeId| document.get(id).is_ok_and(|node| {
                    matches!(node.kind, NodeKind::Target | NodeKind::Index)
                });
                if !is_pair(children[cursor]) {
                    cursor += 1;
                    continue;
                }
                let start = cursor;
                while cursor < children.len() && is_pair(children[cursor]) {
                    cursor += 1;
                }
                let run = &children[start..cursor];
                if run.len() < 2 || run.iter().all(|&id| document.get(id).is_ok_and(|node| node.kind == NodeKind::Index)) {
                    continue;
                }
                let mut indexes = Vec::new();
                let mut targets = Vec::new();
                for &id in run {
                    if document.get(id).is_ok_and(|node| node.kind == NodeKind::Index) {
                        indexes.push(id);
                    } else {
                        targets.push(id);
                    }
                }
                indexes.extend(targets);
                if document.splice_children(parent, start..cursor, indexes).is_ok() {
                    changed = true;
                }
            }
        }
        if changed { TransformResult::changed() } else { TransformResult::unchanged() }
    }
}

/// Docutils `PropagateTargets` (priority 260) — move the ids/names of a
/// standalone target onto the next block-level node and leave `refid` behind.
#[derive(Debug)]
pub struct PropagateTargetsTransform;

impl Transform for PropagateTargetsTransform {
    fn name(&self) -> &'static str {
        "PropagateTargets"
    }

    fn default_priority(&self) -> u32 {
        priority::PROPAGATE_TARGETS
    }

    fn apply(&self, document: &mut Document, _ctx: &TransformContext<'_>) -> TransformResult {
        let mut changed = false;
        for target in nodes_of_kind(document, NodeKind::Target) {
            let Ok(node) = document.get(target) else {
                continue;
            };
            let ids = list_attr(node, "ids");
            let names = list_attr(node, "names");
            if ids.is_empty() && names.is_empty() {
                continue;
            }
            // Upstream ignores block-level targets inside a TextElement.
            if document.parent(target).ok().flatten().is_some_and(|parent| {
                document.get(parent).is_ok_and(|parent| parent.kind.is_inline())
            }) {
                continue;
            }
            if !node.children.is_empty()
                || str_attr(node, "refid").is_some()
                || str_attr(node, "refuri").is_some()
                || str_attr(node, "refname").is_some()
            {
                continue;
            }
            // Find the next node in document order that can carry the target.
            let order = document.preorder_ids();
            let Some(position) = order.iter().position(|&id| id == target) else {
                continue;
            };
            let next = order[position + 1..].iter().copied().find(|&id| {
                document.get(id).is_ok_and(|node| {
                    !matches!(
                        node.kind,
                        NodeKind::SystemMessage
                            | NodeKind::Target
                            | NodeKind::Footnote
                            | NodeKind::Citation
                            | NodeKind::Label
                            | NodeKind::Title
                    ) && !node.kind.is_inline()
                })
            });
            let Some(next) = next else {
                continue;
            };
            if let Ok(recipient) = document.get_mut(next) {
                let mut recipient_ids = list_attr(recipient, "ids");
                let mut recipient_names = list_attr(recipient, "names");
                recipient_ids.extend(ids.iter().cloned());
                recipient_names.extend(names.iter().cloned());
                set_list_attr(recipient, "ids", recipient_ids);
                set_list_attr(recipient, "names", recipient_names);
            }
            if let Ok(node) = document.get_mut(target) {
                node.attrs.set_str("refid", ids.first().cloned().unwrap_or_default());
                node.attrs.remove("ids");
                node.attrs.remove("names");
            }
            changed = true;
        }
        if changed { TransformResult::changed() } else { TransformResult::unchanged() }
    }
}

/// `SortIds` (priority 261) — move a leading generated `idN` to the end of a
/// section's id list.
#[derive(Debug)]
pub struct SortIds;

impl Transform for SortIds {
    fn name(&self) -> &'static str {
        "SortIds"
    }

    fn default_priority(&self) -> u32 {
        priority::SORT_IDS
    }

    fn apply(&self, document: &mut Document, _ctx: &TransformContext<'_>) -> TransformResult {
        let mut changed = false;
        for id in nodes_of_kind(document, NodeKind::Section) {
            let Ok(node) = document.get(id) else {
                continue;
            };
            let ids = list_attr(node, "ids");
            if ids.len() > 1 && ids[0].starts_with("id") {
                let mut reordered = ids[1..].to_vec();
                reordered.push(ids[0].clone());
                if let Ok(node) = document.get_mut(id) {
                    node.attrs.set_list("ids", reordered);
                }
                changed = true;
            }
        }
        if changed { TransformResult::changed() } else { TransformResult::unchanged() }
    }
}

/// `DoctestTransform` (priority 500) — add the `doctest` class and set the
/// `pycon` language on doctest-style literal blocks.
#[derive(Debug)]
pub struct DoctestTransform;

impl Transform for DoctestTransform {
    fn name(&self) -> &'static str {
        "DoctestTransform"
    }

    fn default_priority(&self) -> u32 {
        priority::DOCTEST
    }

    fn apply(&self, document: &mut Document, _ctx: &TransformContext<'_>) -> TransformResult {
        let mut changed = false;
        for id in document.preorder_ids() {
            let Ok(node) = document.get(id) else {
                continue;
            };
            match node.kind {
                NodeKind::DoctestBlock => {
                    let mut classes = list_attr(node, "classes");
                    if !classes.iter().any(|class| class == "doctest") {
                        classes.push("doctest".to_owned());
                        if let Ok(node) = document.get_mut(id) {
                            node.attrs.set_list("classes", classes);
                        }
                        changed = true;
                    }
                }
                NodeKind::LiteralBlock
                    if list_attr(node, "classes").iter().any(|class| class == "doctest")
                        && str_attr(node, "language").is_none() =>
                {
                    if let Ok(node) = document.get_mut(id) {
                        node.attrs.set_str("language", "pycon");
                    }
                    changed = true;
                }
                _ => {}
            }
        }
        if changed { TransformResult::changed() } else { TransformResult::unchanged() }
    }
}

/// `GlossarySorter` (priority 500) — sort a glossary's definitions when the
/// directive requested `sorted`.
#[derive(Debug)]
pub struct GlossarySorter;

impl Transform for GlossarySorter {
    fn name(&self) -> &'static str {
        "GlossarySorter"
    }

    fn default_priority(&self) -> u32 {
        priority::DOCTEST
    }

    fn apply(&self, document: &mut Document, _ctx: &TransformContext<'_>) -> TransformResult {
        let mut changed = false;
        for glossary in nodes_of_kind(document, NodeKind::Glossary) {
            let sorted = document.get(glossary).is_ok_and(|node| {
                matches!(node.attrs.get("sorted"), Some(AttributeValue::Bool(true)))
            });
            if !sorted {
                continue;
            }
            let list = document.children(glossary).ok().and_then(|children| {
                children.iter().copied().find(|&id| {
                    document.get(id).is_ok_and(|node| node.kind == NodeKind::DefinitionList)
                })
            });
            let Some(list) = list else {
                continue;
            };
            let items = document.children(list).unwrap_or_default().to_vec();
            let mut decorated: Vec<(String, NodeId)> = items
                .iter()
                .copied()
                .map(|item| {
                    let key = document
                        .children(item)
                        .ok()
                        .and_then(|children| children.iter().copied().find(|&child| {
                            document.get(child).is_ok_and(|node| node.kind == NodeKind::Term)
                        }))
                        .and_then(|term| document.get(term).ok().map(|node| node.astext(document)))
                        .unwrap_or_default();
                    (key.to_lowercase().as_str().nfd().collect::<String>(), item)
                })
                .collect();
            decorated.sort_by(|a, b| a.0.cmp(&b.0));
            let ordered: Vec<NodeId> = decorated.into_iter().map(|(_, id)| id).collect();
            if ordered != items && document.replace_children(list, ordered).is_ok() {
                changed = true;
            }
        }
        if changed { TransformResult::changed() } else { TransformResult::unchanged() }
    }
}

/// `SphinxSmartQuotes` (priority 750) — educate plain text outside literal,
/// math, raw and other non-smartquotable nodes.
#[derive(Debug)]
pub struct SphinxSmartQuotes;

impl Transform for SphinxSmartQuotes {
    fn name(&self) -> &'static str {
        "SphinxSmartQuotes"
    }

    fn default_priority(&self) -> u32 {
        priority::SMART_QUOTES
    }

    fn apply(&self, document: &mut Document, ctx: &TransformContext<'_>) -> TransformResult {
        if !ctx.config.and_then(|config| config.get_bool("smartquotes")).unwrap_or(true) {
            return TransformResult::unchanged();
        }
        let language = ctx.language.clone();
        if !language_supports_smartquotes(&language) {
            return TransformResult::unchanged();
        }
        if excluded_by_config(ctx, &language) {
            return TransformResult::unchanged();
        }
        let action = ctx
            .config
            .and_then(|config| config.get_str("smartquotes_action"))
            .unwrap_or("qDe")
            .to_owned();
        let mut changed = false;
        for id in document.preorder_ids() {
            if !is_smartquotable(document, id) {
                continue;
            }
            let Some(text) = document.get(id).ok().and_then(|node| node.text_content().map(str::to_owned)) else {
                continue;
            };
            let educated = educate(&text, &language, &action);
            if educated != text {
                if let Ok(node) = document.get_mut(id) {
                    node.set_text(educated);
                }
                changed = true;
            }
        }
        if changed { TransformResult::changed() } else { TransformResult::unchanged() }
    }
}

fn excluded_by_config(ctx: &TransformContext<'_>, language: &str) -> bool {
    let Some(dict) = ctx.config.and_then(|config| config.get_dict("smartquotes_excludes")) else {
        return false;
    };
    let contains = |key: &str, value: &str| -> bool {
        dict.get(key).is_some_and(|entry| {
            entry.get_list().is_some_and(|values| {
                values.iter().filter_map(|item| item.get_str()).any(|item| item == value)
            })
        })
    };
    contains("builders", ctx.builder_name) || contains("languages", language)
}

/// True when a `Text` node's ancestors do not exclude smart quotes.
///
/// Mirrors `sphinx.util.nodes::is_smartquotable`: fixed-text/inline-literal,
/// math, raw, image and problematic parents are skipped, as are nodes with a
/// `support_smartquotes=False` attribute.
fn is_smartquotable(document: &Document, id: NodeId) -> bool {
    let Ok(node) = document.get(id) else {
        return false;
    };
    if node.kind != NodeKind::Text {
        return false;
    }
    for ancestor in document.ancestors(id).skip(1) {
        let Ok(ancestor_node) = document.get(ancestor) else {
            continue;
        };
        if matches!(
            ancestor_node.kind,
            NodeKind::LiteralBlock
                | NodeKind::DoctestBlock
                | NodeKind::Literal
                | NodeKind::Math
                | NodeKind::MathBlock
                | NodeKind::Raw
                | NodeKind::Image
                | NodeKind::Problematic
        ) {
            return false;
        }
        if matches!(ancestor_node.attrs.get("support_smartquotes"), Some(AttributeValue::Bool(false))) {
            return false;
        }
    }
    !matches!(node.attrs.get("support_smartquotes"), Some(AttributeValue::Bool(false)))
}

/// `SphinxDomains` (priority 850) — registers objects and resolves pending
/// cross-references through the frozen [`DomainRegistry`](scribe_domains::DomainRegistry).
///
/// Upstream calls `env.domains._process_doc()` here; our read-phase pass also
/// resolves xrefs so builders that only run the read phase still get resolved
/// references.  With no registry attached to the context the pass is a no-op,
/// preserving the Phase 10 behaviour.
#[derive(Debug)]
pub struct SphinxDomains;

impl Transform for SphinxDomains {
    fn name(&self) -> &'static str {
        "SphinxDomains"
    }

    fn default_priority(&self) -> u32 {
        priority::SPHINX_DOMAINS
    }

    fn apply(&self, document: &mut Document, ctx: &TransformContext<'_>) -> TransformResult {
        run_domains(document, ctx, true)
    }
}

/// Shared implementation for the two domain transform seams.
///
/// `register` controls whether objects are collected from the document (read
/// phase) or only pending xrefs are resolved (post phase).
pub(crate) fn run_domains(document: &mut Document, ctx: &TransformContext<'_>, register: bool) -> TransformResult {
    let Some(registry) = ctx.domains else {
        return TransformResult::unchanged();
    };
    let docname = document
        .metadata
        .docname
        .clone()
        .unwrap_or_else(|| "index".to_owned());
    let domain_ctx = ctx.domain_context(&docname);
    if register && !registry.has_seen_doc(&docname) {
        // Phase 12: expand `py:*` skeleton descriptions before domains walk the
        // tree; no-op for documents without Python directives.  Builders call
        // `prepare_document` for the whole project before the registration
        // pass so every object exists when the first xref is resolved; this
        // call is the fallback for single-document callers.
        if let Err(error) = prepare_document(document) {
            ctx.warn("SphinxDomains", format!("python signature expansion failed: {error}"), None);
        }
        if let Err(error) = registry.process_document(&domain_ctx, &docname, document) {
            ctx.warn("SphinxDomains", format!("domain registration failed: {error}"), None);
        }
    }
    match resolve_pending_xrefs(registry, &domain_ctx, document) {
        Ok(report) => {
            let changed = report.resolved > 0 || report.unresolved > 0 || report.passed_through > 0;
            let result = if changed {
                TransformResult::changed()
            } else {
                TransformResult::unchanged()
            };
            // Domain diagnostics keep their structured form (code, typ, source
            // path and line) so builders can render upstream's canonical
            // `path:LINE: WARNING: msg [type.subtype]` line. Flattening them to
            // `TransformDiagnostic` here would drop exactly those fields; the
            // pipeline forwards them through `TransformReport::domain_diagnostics`.
            result
        }
        Err(error) => TransformResult::unchanged().with_diagnostic(TransformDiagnostic::new(
            "SphinxDomains",
            TransformSeverity::Warning,
            format!("cross-reference resolution failed: {error}"),
            None,
        )),
    }
}

/// `DoctreeReadEvent` (priority 880) — reserved no-op seam for the
/// `doctree-read` event (Phase 27 extension bus).
#[derive(Debug)]
pub struct DoctreeReadEvent;

impl Transform for DoctreeReadEvent {
    fn name(&self) -> &'static str {
        "DoctreeReadEvent"
    }

    fn default_priority(&self) -> u32 {
        priority::DOCTREE_READ_EVENT
    }

    fn apply(&self, _document: &mut Document, _ctx: &TransformContext<'_>) -> TransformResult {
        TransformResult::unchanged()
    }
}

/// `PreserveTranslatableMessages` (priority 10).
///
/// Upstream calls `addnodes.translatable.preserve_original_messages()`. The
/// frozen §7 AST has no such node kind, so scribe preserves the cleaned
/// original text on any node explicitly marked `translatable` under the
/// additive `__original_message` attribute. This is enough to keep the message
/// stable across repeated Locale passes without changing frozen shapes.
#[derive(Debug)]
pub struct PreserveTranslatableMessages;

impl Transform for PreserveTranslatableMessages {
    fn name(&self) -> &'static str {
        "PreserveTranslatableMessages"
    }

    fn default_priority(&self) -> u32 {
        priority::APPLY_SOURCE_WORKAROUND
    }

    fn apply(&self, document: &mut Document, _ctx: &TransformContext<'_>) -> TransformResult {
        let marked: Vec<NodeId> = document
            .preorder_ids()
            .into_iter()
            .filter(|&id| {
                document.get(id).is_ok_and(|node| {
                    matches!(node.attrs.get("translatable"), Some(AttributeValue::Bool(true)))
                })
            })
            .collect();
        let mut changed = false;
        for id in marked {
            let original = document.get(id).ok().map(|node| clean_extracted_message(&node.astext(document)));
            if let (Ok(node), Some(original)) = (document.get_mut(id), original) {
                if node.attrs.get_str("__original_message").ok().flatten() != Some(original.as_str()) {
                    node.attrs.set_str("__original_message", original);
                    changed = true;
                }
            }
        }
        if changed { TransformResult::changed() } else { TransformResult::unchanged() }
    }
}

/// `Locale` (priority 20) — replaces translatable nodes with their translated
/// content using the catalog attached to [`TransformContext::translations`].
#[derive(Debug)]
pub struct Locale;

impl Transform for Locale {
    fn name(&self) -> &'static str {
        "Locale"
    }

    fn default_priority(&self) -> u32 {
        priority::LOCALE
    }

    fn apply(&self, document: &mut Document, ctx: &TransformContext<'_>) -> TransformResult {
        let Some(catalog) = ctx.translations else {
            return TransformResult::unchanged();
        };
        let (changed, _progress) = apply_locale(document, catalog, ctx.builder_name);
        if changed { TransformResult::changed() } else { TransformResult::unchanged() }
    }
}

/// `TranslationProgressTotaliser` (priority 25) — stores the per-document
/// progress on the root node so `|translation progress|` can read it.
#[derive(Debug)]
pub struct TranslationProgressTotaliser;

impl Transform for TranslationProgressTotaliser {
    fn name(&self) -> &'static str {
        "TranslationProgressTotaliser"
    }

    fn default_priority(&self) -> u32 {
        priority::TRANSLATION_PROGRESS_TOTALISER
    }

    fn apply(&self, document: &mut Document, ctx: &TransformContext<'_>) -> TransformResult {
        if ctx.builder_name == "gettext" || ctx.translations.is_none() {
            return TransformResult::unchanged();
        }
        let progress = TranslationProgress::from_document(document).unwrap_or_default();
        progress.store_on_document(document);
        TransformResult::unchanged()
    }
}

/// `AddTranslationClasses` (priority 950) — appends `translated` /
/// `untranslated` CSS classes according to `translation_progress_classes`.
#[derive(Debug)]
pub struct AddTranslationClasses;

impl Transform for AddTranslationClasses {
    fn name(&self) -> &'static str {
        "AddTranslationClasses"
    }

    fn default_priority(&self) -> u32 {
        priority::ADD_TRANSLATION_CLASSES
    }

    fn apply(&self, document: &mut Document, ctx: &TransformContext<'_>) -> TransformResult {
        if ctx.builder_name == "gettext" {
            return TransformResult::unchanged();
        }
        let (add_translated, add_untranslated) = translation_class_flags(ctx);
        if !add_translated && !add_untranslated {
            return TransformResult::unchanged();
        }
        let ids: Vec<NodeId> = document
            .preorder_ids()
            .into_iter()
            .filter(|&id| {
                document.get(id).is_ok_and(|node| {
                    matches!(node.attrs.get("translated"), Some(AttributeValue::Bool(_)))
                })
            })
            .collect();
        let mut changed = false;
        for id in ids {
            let is_translated = document
                .get(id)
                .ok()
                .and_then(|node| match node.attrs.get("translated") {
                    Some(AttributeValue::Bool(v)) => Some(*v),
                    _ => None,
                })
                .unwrap_or(false);
            let add = if is_translated { add_translated } else { add_untranslated };
            if !add {
                continue;
            }
            if let Ok(node) = document.get_mut(id) {
                let mut classes = list_attr(node, "classes");
                let class = if is_translated { "translated" } else { "untranslated" };
                if !classes.iter().any(|value| value == class) {
                    classes.push(class.to_owned());
                    set_list_attr(node, "classes", classes);
                    changed = true;
                }
            }
        }
        if changed { TransformResult::changed() } else { TransformResult::unchanged() }
    }
}

/// Reads `translation_progress_classes` (bool or one of the two string modes).
fn translation_class_flags(ctx: &TransformContext<'_>) -> (bool, bool) {
    let Some(config) = ctx.config else { return (false, false) };
    if let Some(value) = config.get_bool("translation_progress_classes") {
        return (value, value);
    }
    match config.get_str("translation_progress_classes").unwrap_or("") {
        "translated" => (true, false),
        "untranslated" => (false, true),
        "True" | "true" | "1" => (true, true),
        _ => (false, false),
    }
}

/// `RemoveTranslatableInline` (priority 999) — unwraps placeholder `Inline`
/// nodes used by extension-defined translatable markup.
///
/// The read-phase slot is registered under this wrapper name because
/// `TransformRegistry` keys by name; the resolved-phase implementation lives
/// in `post_transforms.rs` under the canonical name.
#[derive(Debug)]
pub struct RemoveTranslatableInlineRead;

impl Transform for RemoveTranslatableInlineRead {
    fn name(&self) -> &'static str {
        "RemoveTranslatableInline"
    }

    fn default_priority(&self) -> u32 {
        priority::FILTER_SYSTEM_MESSAGES
    }

    fn apply(&self, document: &mut Document, _ctx: &TransformContext<'_>) -> TransformResult {
        remove_translatable_inlines(document)
    }
}

/// Shared implementation of the read- and resolved-phase slots.
pub(crate) fn remove_translatable_inlines(document: &mut Document) -> TransformResult {
    let marked: Vec<NodeId> = document
        .preorder_ids()
        .into_iter()
        .filter(|&id| {
            document.get(id).is_ok_and(|node| {
                node.kind == NodeKind::Inline
                    && matches!(node.attrs.get("translatable"), Some(AttributeValue::Bool(_)))
            })
        })
        .collect();
    let mut changed = false;
    for id in marked {
        if document.replace_with_children(id).is_ok() {
            changed = true;
        }
    }
    if changed { TransformResult::changed() } else { TransformResult::unchanged() }
}

/// `FilterSystemMessages` (priority 999) — drop low-level system messages.
#[derive(Debug)]
pub struct FilterSystemMessages;

impl Transform for FilterSystemMessages {
    fn name(&self) -> &'static str {
        "FilterSystemMessages"
    }

    fn default_priority(&self) -> u32 {
        priority::FILTER_SYSTEM_MESSAGES
    }

    fn apply(&self, document: &mut Document, ctx: &TransformContext<'_>) -> TransformResult {
        let keep_warnings = ctx.config.and_then(|config| config.get_bool("keep_warnings")).unwrap_or(false);
        let filter_level = if keep_warnings { 2 } else { 5 };
        let mut changed = false;
        for id in nodes_of_kind(document, NodeKind::SystemMessage) {
            let level = document.get(id).ok().and_then(|node| int_attr(node, "level")).unwrap_or(0);
            if level < filter_level && document.remove_node(id).is_ok() {
                changed = true;
            }
        }
        if changed { TransformResult::changed() } else { TransformResult::unchanged() }
    }
}

/// `MetadataCollector` (priority 1000) — extract the leading docinfo field
/// list into [`scribe_ast::DocumentMetadata::docinfo`] and remove it from the
/// doctree.
///
/// Upstream's `MetadataCollector` runs on `doctree-read`, after every read
/// transform, and pops the whole `docinfo` node once its bibliographic fields
/// have been stored in `env.metadata`. Scribe represents the docinfo preamble
/// as a plain `FieldList` at the document root, so this transform mirrors the
/// same "collect then drop" behaviour. Only a field list that is the *first*
/// root child qualifies: Docutils treats later field lists as ordinary body
/// content and leaves them in the doctree.
#[derive(Debug)]
pub struct DocInfoTransform;

impl Transform for DocInfoTransform {
    fn name(&self) -> &'static str {
        "MetadataCollector"
    }

    fn default_priority(&self) -> u32 {
        priority::DOCINFO
    }

    fn apply(&self, document: &mut Document, _ctx: &TransformContext<'_>) -> TransformResult {
        strip_docinfo(document)
    }
}

/// Implements [`DocInfoTransform`] without panicking on malformed doctrees.
fn strip_docinfo(document: &mut Document) -> TransformResult {
    let root = document.root;
    let first = match document.children(root).ok().and_then(|children| children.first().copied()) {
        Some(id) => id,
        None => return TransformResult::unchanged(),
    };
    if document.get(first).map(|node| node.kind).ok() != Some(NodeKind::FieldList) {
        return TransformResult::unchanged();
    }

    let field_ids = document.children(first).unwrap_or_default().to_vec();
    let mut collected = Vec::new();
    for field in field_ids {
        if document.get(field).map(|node| node.kind).ok() != Some(NodeKind::Field) {
            continue;
        }
        let child_ids = document.children(field).unwrap_or_default().to_vec();
        let mut name = None;
        let mut body = None;
        for child in child_ids {
            match document.get(child).map(|node| node.kind).ok() {
                Some(NodeKind::FieldName) if name.is_none() => {
                    name = Some(document.get(child).map(|node| node.astext(document)).unwrap_or_default());
                }
                Some(NodeKind::FieldBody) if body.is_none() => {
                    body = Some(document.get(child).map(|node| node.astext(document)).unwrap_or_default());
                }
                _ => {}
            }
        }
        if let Some(name) = name.map(|value| value.trim().to_owned()).filter(|value| !value.is_empty()) {
            collected.push((name, body.unwrap_or_default().trim().to_owned()));
        }
    }

    if collected.is_empty() || document.remove_node(first).is_err() {
        return TransformResult::unchanged();
    }
    for (name, body) in collected {
        document.metadata.docinfo.insert(name, body);
    }
    TransformResult::changed()
}

/// Docutils `Footnotes` (priority 620) — assign numbers/symbols to footnotes,
/// add labels and link references back to their definitions.
pub use footnotes::FootnotesTransform;

mod footnotes;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_priorities_match_oracle() {
        assert_eq!(ApplySourceWorkaround.default_priority(), 10);
        assert_eq!(DefaultSubstitutions.default_priority(), 210);
        assert_eq!(SortIds.default_priority(), 261);
        assert_eq!(ReorderConsecutiveTargetAndIndexNodes.default_priority(), 220);
        assert_eq!(DoctestTransform.default_priority(), 500);
        assert_eq!(SphinxSmartQuotes.default_priority(), 750);
        assert_eq!(SphinxDomains.default_priority(), 850);
        assert_eq!(FilterSystemMessages.default_priority(), 999);
    }
}
