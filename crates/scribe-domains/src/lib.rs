//! Domain architecture and the standard domain (`std`) for `scribe`.
//!
//! **Frozen cross-phase contract (Phase 11):** the [`Domain`] trait with
//! [`Domain::resolve_xref`] / [`Domain::resolve_any_xref`] and the
//! [`DomainRegistry`] dispatch API.  Phases 12–14 add other domains and
//! intersphinx fallback against this exact shape; builders (Phase 17+) use
//! [`resolve_pending_xrefs`] through the Phase 10 transform seams.
//!
//! The dependency graph is acyclic: this crate depends on `scribe-ast` and
//! `scribe-config`; `scribe-core` depends on this crate.  Environment access
//! is therefore provided through the [`DomainEnv`] trait rather than a
//! concrete `BuildEnvironment` reference.

pub mod c;
pub mod cpp;
pub mod changeset;
pub mod citation;
pub mod diagnostics;
pub mod error;
pub mod index;
pub mod intersphinx;
pub mod javascript;
pub mod math;
pub mod python;
pub mod registry;
pub mod resolve;
pub mod rst;
pub mod sigbuild;
pub mod std;
pub mod traits;
pub mod types;
pub mod xref;

use scribe_ast::Document;

pub use diagnostics::{
    codes, DiagnosticSink, DomainDiagnostic, DomainSeverity,
};
pub use error::{DomainError, DomainResult};
pub use python::{
    parse_annotation, parse_reftarget, parse_sig_re, PyOptions, PythonDomain,
    PythonDomainData, PythonModuleEntry, PythonModuleIndex, PythonObjectEntry, SigContext,
    SignatureInfo,
};
pub use registry::{
    missing_reference_diagnostic, parse_ignore_entry, DomainRegistry, SharedDomain,
};
pub use resolve::{resolve_pending_xrefs, ResolveReport};
pub use std::{StandardDomain, StandardDomainData};
pub use traits::{docname_join, relative_uri, Domain, DomainContext, DomainEnv};
pub use types::{
    DomainData, ObjectEntry, ObjectType, ReferenceNodeKind, ResolvedReference,
};

pub use intersphinx::{
    fetch_all, fetch_all_async, fetch_all_with_runtime, fetch_one, DumpConfig, ExternalRole,
    FetchOptions, FetchOutcome, IntersphinxError, IntersphinxMapping, IntersphinxReference,
    IntersphinxResolver, Inventory, InventoryCache, InventoryItem, InventoryLocation,
    InventoryObject, MappingEntry,
};
pub use xref::{suggest, Suggestion, XrefResolutionReport, XrefResolver};

pub use c::{CDomain, CDomainData, CObjectEntry};
pub use cpp::{CppDomain, CppDomainData, CppObjectEntry};
pub use javascript::{JavaScriptDomain, JavaScriptDomainData, JavaScriptObjectEntry};
pub use rst::{ReSTDomain, ReSTDomainData, ReSTObjectEntry};
pub use math::{MathDomain, MathDomainData};
pub use changeset::ChangesetDomain;
pub use citation::CitationDomain;
pub use index::IndexDomain;

/// Expands every domain's skeleton descriptions in `document`.
///
/// Called by the Phase 10 `SphinxDomains` read transform before domains walk
/// the tree.  It is a no-op for documents without domain skeletons, and every
/// sub-pass is independently additive (one domain's failure never prevents
/// another domain's expansion from being attempted).
pub fn prepare_document(document: &mut Document) -> DomainResult<()> {
    python::prepare_document(document)?;
    c::prepare_document(document)?;
    cpp::prepare_document(document)?;
    javascript::prepare_document(document)?;
    rst::prepare_document(document)?;
    Ok(())
}

/// Version string for the frozen domain contract, useful in build metadata.
pub const DOMAIN_CONTRACT_VERSION: &str = "phase13.1";
