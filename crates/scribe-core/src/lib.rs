//! Shared core types for `scribe`.
//!
//! This crate owns the vocabulary that is stable across phases: the crate-wide
//! result alias, the base error type, and (since Phase 08) the frozen
//! [`BuildEnvironment`] contract plus source discovery and Sphinx-compatible
//! glob matching. The document AST itself lives in the frozen `scribe-ast`
//! crate; this crate only stores snapshots of it.

pub mod bridge;
pub mod discovery;
pub mod env;
pub mod events;
pub mod export_fidelity;
pub mod extensions;
pub mod glob;
pub mod i18n;
pub mod plugin;
pub mod post_transforms;
pub mod python_shim;
pub mod smartquotes;
pub mod toctree;
pub mod toctree_expand;
pub mod transforms;
pub mod warnings;

pub use bridge::InteropBridge;
pub use discovery::{
    discover_sources, doc2path, path2doc, DiscoveredDoc, DiscoveryOptions, DiscoveryResult,
    SourceSuffixMap, EXCLUDE_PATHS,
};
pub use events::{
    handler, recording_handler, value_result, ConnectionId, Event, EventDiagnostic, EventHandler,
    EventManager, HandlerError, HandlerResult, SharedJson, SharedString, CORE_EVENTS,
    DEFAULT_PRIORITY,
};
pub use extensions::{
    register_config_keys, register_directives, relative_doc_uri, ActivatedExtensions,
    DurationReport, ExtLinkRoles, GithubPagesFiles, GraphvizHtmlInfo, TodoCollector,
    ViewcodeCollector, BUILTIN_EXTENSIONS,
};
pub use env::{
    discover_for_environment, doc2path_in, extract_document_metadata, path2doc_in,
    BuildEnvironment, DocMeta, DocumentSnapshot, EnvironmentSnapshot, ExtractedMetadata,
    RelevantConfig, UpdateOptions, UpdateReport, ENV_CACHE_KEY, ENV_FORMAT_VERSION,
    DEFAULT_PARSER_VERSION,
};
pub use glob::{
    canon_path, compile_matchers, match_any, path_stabilize, translate_pattern, GlobSpec, Matcher,
};
pub use i18n::{
    apply_locale, clean_extracted_message, extract_messages, is_in_substitution_definition,
    is_translatable, ExtractedMessage, TranslationCatalog, TranslationProgress,
};
pub use plugin::{
    minimal_manifest, PluginCapabilities, PluginError, PluginManifest, WasmPluginHost,
    HOST_ADD_DIRECTIVE, HOST_ADD_ROLE, HOST_AST_GET, HOST_CONFIG_GET,
    HOST_CONNECT_EVENT, HOST_DIAGNOSTIC, HOST_LOG, PLUGIN_ABI_VERSION, PLUGIN_CALLBACKS,
};
pub use python_shim::{
    PythonExtensionShim, PyScript, ShimConfig, ShimError, EXAMPLE_EXTENSION_SCRIPT,
};
pub use toctree::{
    collect_toctrees, collect_toctrees_with_sources, EntryTarget, FigureNumbers, NavRelation, ParsedToctree, SectionNumbers,
    SectionTocEntry, TocTreeNode, ToctreeCollection, ToctreeCollector, ToctreeEntry,
    ToctreeHierarchy, ToctreeOptions, ToctreeWarning,
};
pub use toctree_expand::{expand_toctrees, ToctreeExpansion};
pub use export_fidelity::{normalize_citation_references, normalize_xref_literals};

pub use smartquotes::{educate as educate_smartquotes, language_supports_smartquotes, quote_pairs};
pub use post_transforms::{
    OnlyNodeTransform, PropagateDescDomain, ReferencesResolver, RemoveTranslatableInline,
    SigElementFallbackTransform,
};
pub use transforms::{
    apply_transforms, priority, ApplySourceWorkaround, AutoIndexUpgrader, AutoNumbering,
    DefaultSubstitutions, DoctestTransform, DoctreeReadEvent, ExtraTranslatableNodes,
    FilterSystemMessages, FootnotesTransform, GlossarySorter, HandleCodeBlocks, Locale,
    MoveModuleTargets, Phase, PlannedTransform, PostTransform, PreserveTranslatableMessages,
    PropagateTargetsTransform, RefOnlyBulletListTransform, ReorderConsecutiveTargetAndIndexNodes,
    SkipReason, SkippedTransform, SphinxDomains, SphinxSmartQuotes, SortIds,
    Transform, TransformContext, TransformDiagnostic, TransformError, TransformPipeline,
    TransformRecord, TransformRegistry, TransformReport, TransformResult, TransformSeverity,
    TranslationProgressTotaliser, AddTranslationClasses, RemoveTranslatableInlineRead,
};

pub use warnings::{
    absolute_source_path, canonical_code, classify_warning_message, duplicate_display_code,
    format_warning, is_code_suppressed, is_suppressed, missing_reference_code,
    missing_reference_display_code, suppression_patterns, WarningCode, WarningLocation,
    WARNING_CODE_MAP, SUPPORTED_SUPPRESSION_CODES,
};

pub use scribe_cache::{ContentDigest, CacheError};

use thiserror::Error;

/// The result alias used throughout the `scribe` library crates.
pub type ScribeResult<T> = Result<T, ScribeError>;

/// Base error type for library-level failures.
///
/// User-facing diagnostics are rendered with `miette` at the CLI boundary;
/// library code reports structured errors with this `thiserror` enum.
#[derive(Debug, Error)]
pub enum ScribeError {
    /// An I/O operation failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A configuration value was rejected.
    #[error("invalid configuration: {message}")]
    InvalidConfiguration {
        /// Human-readable explanation of the rejection.
        message: String,
    },

    /// A requested operation has not been implemented by the current phase.
    #[error("not implemented: {feature}")]
    NotImplemented {
        /// Name of the unimplemented feature.
        feature: &'static str,
    },

    /// Source discovery failed for a specific path.
    #[error("source discovery failed at `{}`: {message}", .path.display())]
    Discovery {
        /// Path that could not be scanned.
        path: std::path::PathBuf,
        /// Human-readable failure explanation.
        message: String,
    },

    /// The persisted build environment was unusable.
    #[error("build environment error: {message}")]
    Environment {
        /// Human-readable failure explanation.
        message: String,
    },

    /// The cache layer reported a failure that cannot be treated as cold.
    #[error(transparent)]
    Cache(#[from] CacheError),
}

#[cfg(test)]
mod tests {
    use super::ScribeError;

    #[test]
    fn error_messages_are_stable() {
        let err = ScribeError::NotImplemented { feature: "builder" };
        assert_eq!(err.to_string(), "not implemented: builder");
    }
}
