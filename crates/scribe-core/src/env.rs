//! The frozen Phase 08 `BuildEnvironment` API.
//!
//! This module is the cross-phase contract consumed by Phases 09 (toctree
//! collector), 10 (transforms), 11–14 (domains / xrefs) and 17–25 (builders).
//! Its public shapes are documented exhaustively in `ARCHITECTURE.md` §12 and
//! must not change without orchestrator approval.

use crate::discovery::{discover_sources, path2doc, DiscoveryOptions, DiscoveryResult};
use crate::{ScribeError, ScribeResult};
use crate::glob::{canon_path, GlobSpec};
use scribe_ast::Document;
use scribe_cache::{CacheStore, ContentDigest};
use scribe_domains::DomainEnv;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Version of the serialized environment snapshot. Bump whenever a field is
/// added, removed or retyped; incompatible caches are then treated as cold.
pub const ENV_FORMAT_VERSION: u32 = 4;

/// Cache key used for the top-level environment artifact.
pub const ENV_CACHE_KEY: &str = "environment";

/// Parser / directive-registry generation mixed into every document hash.
///
/// Bump this string whenever the RST or MyST parser or the directive registry
/// changes in a way that alters the produced doctree, so cached trees from an
/// older engine are invalidated.
pub const DEFAULT_PARSER_VERSION: &str =
    "scribe-rst=1;scribe-myst=1;directives=62;roles=74";

/// The subset of configuration that affects discovery or doctree content.
///
/// Upstream calls this the "env" config category. Values are stored in sorted
/// containers so [`Self::digest`] is deterministic across runs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelevantConfig {
    /// `exclude_patterns`.
    pub exclude_patterns: Vec<String>,
    /// `include_patterns`.
    pub include_patterns: Vec<String>,
    /// `templates_path` (excluded from source discovery upstream).
    pub templates_path: Vec<String>,
    /// `source_suffix`: suffix → parser name.
    pub source_suffix: Vec<(String, String)>,
    /// Effective master / root document name.
    pub master_doc: String,
    /// `rst_prolog`, injected into every reStructuredText source.
    pub rst_prolog: String,
    /// `rst_epilog`, appended to every reStructuredText source.
    pub rst_epilog: String,
    /// Loaded extension names.
    pub extensions: Vec<String>,
    /// Document language.
    pub language: String,
    /// Source encoding.
    pub source_encoding: String,
}

impl RelevantConfig {
    /// Extract the relevant subset from a resolved configuration.
    pub fn from_config(config: &scribe_config::Config) -> Self {
        let source_suffix = config
            .get_dict("source_suffix")
            .map(|entries| {
                entries
                    .iter()
                    .map(|(suffix, parser)| (suffix.clone(), parser.display_string()))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            exclude_patterns: config.get_strings("exclude_patterns").unwrap_or_default(),
            include_patterns: config.get_strings("include_patterns").unwrap_or_default(),
            templates_path: config.get_strings("templates_path").unwrap_or_default(),
            source_suffix,
            master_doc: config.master_doc().to_owned(),
            rst_prolog: config.get_str("rst_prolog").unwrap_or_default().to_owned(),
            rst_epilog: config.get_str("rst_epilog").unwrap_or_default().to_owned(),
            extensions: config.get_strings("extensions").unwrap_or_default(),
            language: config.get_str("language").unwrap_or_default().to_owned(),
            source_encoding: config
                .get_str("source_encoding")
                .unwrap_or("utf-8-sig")
                .to_owned(),
        }
    }

    /// Deterministic BLAKE3 digest of this configuration subset.
    pub fn digest(&self) -> ContentDigest {
        match bincode::serialize(self) {
            Ok(bytes) => ContentDigest::of_bytes(&bytes),
            Err(_) => ContentDigest::ZERO,
        }
    }

    /// Names of fields that differ from `other` (sorted, stable).
    pub fn difference(&self, other: &Self) -> Vec<String> {
        let mut differences = Vec::new();
        if self.exclude_patterns != other.exclude_patterns {
            differences.push("exclude_patterns".to_owned());
        }
        if self.include_patterns != other.include_patterns {
            differences.push("include_patterns".to_owned());
        }
        if self.templates_path != other.templates_path {
            differences.push("templates_path".to_owned());
        }
        if self.source_suffix != other.source_suffix {
            differences.push("source_suffix".to_owned());
        }
        if self.master_doc != other.master_doc {
            differences.push("master_doc".to_owned());
        }
        if self.rst_prolog != other.rst_prolog {
            differences.push("rst_prolog".to_owned());
        }
        if self.rst_epilog != other.rst_epilog {
            differences.push("rst_epilog".to_owned());
        }
        if self.extensions != other.extensions {
            differences.push("extensions".to_owned());
        }
        if self.language != other.language {
            differences.push("language".to_owned());
        }
        if self.source_encoding != other.source_encoding {
            differences.push("source_encoding".to_owned());
        }
        differences.sort();
        differences
    }
}

/// Cached metadata and content hashes for one document.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocMeta {
    /// Source path relative to the source dir (`/`-separated).
    pub relative_path: String,
    /// Source modification time in integer microseconds.
    pub mtime_micros: i64,
    /// BLAKE3 digest of the source file.
    pub source_digest: ContentDigest,
    /// Digest of the relevant configuration at the last read.
    pub config_digest: ContentDigest,
    /// Parser / directive registry generation at the last read.
    pub parser_version: String,
    /// Included file path → digest, used to invalidate includers.
    pub included_digests: BTreeMap<String, ContentDigest>,
    /// Other dependency path → digest, used to invalidate dependents.
    pub dependency_digests: BTreeMap<String, ContentDigest>,
    /// Hash of source + includes + dependencies + config + parser version.
    pub combined_digest: ContentDigest,
    /// Document title, when known.
    pub title: Option<String>,
}

/// A persisted doctree snapshot plus the metadata extracted from it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DocumentSnapshot {
    /// Sphinx docname.
    pub docname: String,
    /// Serialized doctree, when a phase has produced one.
    pub doctree: Option<Document>,
    /// Section ids in document order.
    pub sections: Vec<String>,
    /// Raw toctree entry docnames (populated by Phase 09).
    pub toctree_targets: Vec<String>,
    /// Pending cross-reference targets (populated by Phases 05/11).
    pub xrefs: Vec<String>,
    /// Anchor / label names defined in the document.
    pub anchors: Vec<String>,
}

/// The complete serializable environment snapshot.
///
/// All maps are stored as vectors sorted by key so bincode bytes are
/// deterministic. `scribe-cache` wraps this type in a versioned envelope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentSnapshot {
    /// Must equal [`ENV_FORMAT_VERSION`].
    pub format_version: u32,
    /// Canonical source directory at the time of the snapshot.
    pub source_dir: String,
    /// Parser / directive registry generation.
    pub parser_version: String,
    /// Relevant configuration at the last read.
    pub relevant_config: RelevantConfig,
    /// `docname → relative source path`.
    pub found_docs: Vec<(String, String)>,
    /// `docname → metadata`.
    pub all_docs: Vec<(String, DocMeta)>,
    /// `docname → dependency paths`.
    pub dependencies: Vec<(String, Vec<String>)>,
    /// `docname → included paths`.
    pub included: Vec<(String, Vec<String>)>,
    /// `docname → docnames to rebuild alongside it`.
    pub files_to_rebuild: Vec<(String, Vec<String>)>,
    /// `docname → toctree child docnames` (Phase 09).
    pub toctree_includes: Vec<(String, Vec<String>)>,
    /// Documents with forced re-read on every build.
    pub reread_always: Vec<String>,
    /// Documents containing a `:glob:` toctree (Phase 09).
    pub glob_toctrees: Vec<String>,
    /// Documents containing a `:numbered:` toctree (Phase 09).
    pub numbered_toctrees: Vec<String>,
    /// Per-document doctree snapshots.
    pub documents: Vec<DocumentSnapshot>,
    /// Phase 11: serialized domain inventories, sorted by domain name.
    ///
    /// Stored as JSON *text* rather than `serde_json::Value` because bincode
    /// cannot deserialize `Value` (it requires `deserialize_any`). Keeping the
    /// text here makes the whole snapshot bincode-round-trippable, which is
    /// what makes a warm incremental build possible at all.
    pub domain_data: Vec<(String, String)>,
}

/// The global build environment.
///
/// Field names mirror `sphinx.environment.BuildEnvironment` where practical:
/// `found_docs`, `all_docs`, `dependencies`, `included`, `files_to_rebuild`,
/// `reread_always`, `glob_toctrees`, `numbered_toctrees`. Additional fields
/// (`documents`, `outdated_docs`, `extra_outdated`) are Scribe-specific seams.
#[derive(Clone, Debug)]
pub struct BuildEnvironment {
    /// Discovered documents: docname → source path.
    pub found_docs: HashMap<String, PathBuf>,
    /// Read documents and their cached metadata, ordered by docname.
    pub all_docs: BTreeMap<String, DocMeta>,
    /// docname → paths this document depends on.
    pub dependencies: HashMap<String, HashSet<String>>,
    /// docname → files included by this document.
    pub included: HashMap<String, HashSet<String>>,
    /// docname → documents that must be rewritten when this one changes.
    pub files_to_rebuild: HashMap<String, HashSet<String>>,
    /// docname → toctree child docnames (populated by Phase 09).
    pub toctree_includes: BTreeMap<String, Vec<String>>,
    /// Documents to re-read unconditionally.
    pub reread_always: BTreeSet<String>,
    /// Documents with a globbed toctree.
    pub glob_toctrees: BTreeSet<String>,
    /// Documents with a numbered toctree.
    pub numbered_toctrees: BTreeSet<String>,
    /// Per-document doctree snapshots.
    pub documents: BTreeMap<String, DocumentSnapshot>,
    /// Transient set of docs in the current update's rebuild set.
    pub outdated_docs: BTreeSet<String>,
    /// Phase 27 seam: extra docs supplied by extension hooks.
    pub extra_outdated: BTreeSet<String>,
    /// Additive Phase 11 side table: serialized domain inventories.
    /// Kept private so the frozen public field shape is unchanged.
    domain_data: BTreeMap<String, serde_json::Value>,
    source_dir: PathBuf,
    relevant_config: RelevantConfig,
    parser_version: String,
}

impl BuildEnvironment {
    /// Create an empty environment for `source_dir`.
    pub fn new(source_dir: impl Into<PathBuf>) -> Self {
        Self {
            found_docs: HashMap::new(),
            all_docs: BTreeMap::new(),
            dependencies: HashMap::new(),
            included: HashMap::new(),
            files_to_rebuild: HashMap::new(),
            toctree_includes: BTreeMap::new(),
            reread_always: BTreeSet::new(),
            glob_toctrees: BTreeSet::new(),
            numbered_toctrees: BTreeSet::new(),
            documents: BTreeMap::new(),
            outdated_docs: BTreeSet::new(),
            extra_outdated: BTreeSet::new(),
            domain_data: BTreeMap::new(),
            source_dir: source_dir.into(),
            relevant_config: RelevantConfig::default(),
            parser_version: DEFAULT_PARSER_VERSION.to_owned(),
        }
    }

    /// Create an empty environment initialized from a configuration.
    pub fn with_config(source_dir: impl Into<PathBuf>, config: &scribe_config::Config) -> Self {
        let mut environment = Self::new(source_dir);
        environment.relevant_config = RelevantConfig::from_config(config);
        environment
    }

    /// Source directory this environment is bound to.
    pub fn source_dir(&self) -> &Path {
        &self.source_dir
    }

    /// Replace the source directory.
    pub fn set_source_dir(&mut self, source_dir: impl Into<PathBuf>) {
        self.source_dir = source_dir.into();
    }

    /// Relevant configuration subset currently recorded.
    pub fn relevant_config(&self) -> &RelevantConfig {
        &self.relevant_config
    }

    /// Replace the relevant configuration subset.
    pub fn set_relevant_config(&mut self, config: RelevantConfig) {
        self.relevant_config = config;
    }

    /// Parser / directive registry generation.
    pub fn parser_version(&self) -> &str {
        &self.parser_version
    }

    /// Override the parser / directive registry generation.
    pub fn set_parser_version(&mut self, version: impl Into<String>) {
        self.parser_version = version.into();
    }

    /// Effective glob specification derived from the recorded config.
    pub fn glob_to_spec(&self) -> GlobSpec {
        let mut exclude = self.relevant_config.exclude_patterns.clone();
        exclude.extend(self.relevant_config.templates_path.iter().cloned());
        exclude.extend(
            crate::discovery::EXCLUDE_PATHS
                .iter()
                .map(|pattern| (*pattern).to_owned()),
        );
        GlobSpec {
            include: self.relevant_config.include_patterns.clone(),
            exclude,
        }
    }

    /// Whether a source-relative path is excluded by the effective globs.
    ///
    /// This is the `glob` predicate used by discovery; it uses Sphinx's
    /// `Matcher` semantics (including the `**/` prefix expansion).
    pub fn glob(&self, path: &str) -> bool {
        self.glob_to_spec().is_excluded(path)
    }

    /// Discovered source paths, sorted by docname.
    pub fn found_docs_sorted(&self) -> Vec<(&str, &Path)> {
        let sorted: BTreeMap<&str, &PathBuf> = self
            .found_docs
            .iter()
            .map(|(name, path)| (name.as_str(), path))
            .collect();
        sorted.into_iter().map(|(name, path)| (name, path.as_path())).collect()
    }

    /// Docnames currently discovered, sorted.
    pub fn found_docnames(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.found_docs.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }

    /// Map a docname to its source path.
    pub fn doc2path(&self, docname: &str) -> Option<&Path> {
        self.found_docs.get(docname).map(PathBuf::as_path)
    }

    /// Map an absolute or source-relative path to a docname.
    pub fn path2doc(&self, path: &Path) -> Option<&str> {
        let canonical = canon_path(&path.to_string_lossy());
        for (docname, source) in &self.found_docs {
            if canonical == canon_path(&source.to_string_lossy()) {
                return Some(docname.as_str());
            }
            let relative = path_to_relative(&self.source_dir, source);
            if canonical == relative {
                return Some(docname.as_str());
            }
        }
        None
    }

    // ----- dependency / inclusion recording --------------------------------

    /// Record `path` as a dependency of `docname` and hash it immediately.
    pub fn note_dependency(&mut self, docname: &str, path: &Path) {
        let relative = path_to_relative(&self.source_dir, path);
        self.dependencies
            .entry(docname.to_owned())
            .or_default()
            .insert(relative.clone());
        let digest = hash_relative(&self.source_dir, &relative);
        self.all_docs
            .entry(docname.to_owned())
            .or_default()
            .dependency_digests
            .insert(relative, digest);
    }

    /// Record several dependencies for `docname`.
    pub fn note_dependencies<I, P>(&mut self, docname: &str, paths: I)
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        for path in paths {
            self.note_dependency(docname, path.as_ref());
        }
    }

    /// Record `path` as included by `docname` and hash it immediately.
    ///
    /// `included` values are canonical paths relative to the source dir. A
    /// path that is itself a source document is still stored as a path; use
    /// [`Self::includers_of`] to reverse the edge by docname or path.
    pub fn note_included(&mut self, docname: &str, path: &Path) {
        let relative = path_to_relative(&self.source_dir, path);
        self.included
            .entry(docname.to_owned())
            .or_default()
            .insert(relative.clone());
        let digest = hash_relative(&self.source_dir, &relative);
        self.all_docs
            .entry(docname.to_owned())
            .or_default()
            .included_digests
            .insert(relative, digest);
    }

    /// Record that `child` is listed in `parent`'s toctree (Phase 09 seam).
    pub fn note_toctree_edge(&mut self, parent: &str, child: &str) {
        let children = self.toctree_includes.entry(parent.to_owned()).or_default();
        if !children.iter().any(|existing| existing == child) {
            children.push(child.to_owned());
            children.sort();
        }
    }

    /// Replace a document's whole toctree child list (Phase 09 seam).
    pub fn set_toctree_includes(&mut self, parent: &str, children: &[String]) {
        let mut sorted = children.to_vec();
        sorted.sort();
        sorted.dedup();
        self.toctree_includes.insert(parent.to_owned(), sorted);
    }

    /// Record an explicit "rebuild `dependent` when `docname` changes" edge.
    pub fn note_file_to_rebuild(&mut self, docname: &str, dependent: &str) {
        self.files_to_rebuild
            .entry(docname.to_owned())
            .or_default()
            .insert(dependent.to_owned());
    }

    /// Force unconditional re-read of `docname` on the next build.
    pub fn note_reread(&mut self, docname: &str) {
        self.reread_always.insert(docname.to_owned());
    }

    /// Mark `docname` as containing a globbed toctree.
    pub fn note_glob_toctree(&mut self, docname: &str) {
        self.glob_toctrees.insert(docname.to_owned());
    }

    /// Mark `docname` as containing a numbered toctree.
    pub fn note_numbered_toctree(&mut self, docname: &str) {
        self.numbered_toctrees.insert(docname.to_owned());
    }

    /// Phase 27 seam for the `env-get-outdated` / `env-get-updated` hooks.
    pub fn add_extra_outdated<I>(&mut self, docnames: I)
    where
        I: IntoIterator<Item = String>,
    {
        self.extra_outdated.extend(docnames);
    }

    /// Mark a document as outdated in the current update.
    pub fn mark_outdated(&mut self, docname: &str) {
        self.outdated_docs.insert(docname.to_owned());
    }

    // ----- reverse dependency queries --------------------------------------

    /// Docs whose `included` set contains `name` (a docname or relative path).
    pub fn includers_of(&self, name: &str) -> BTreeSet<String> {
        let variants = self.path_variants(name);
        self.included
            .iter()
            .filter(|(_, paths)| variants.iter().any(|variant| paths.contains(variant)))
            .map(|(docname, _)| docname.clone())
            .collect()
    }

    /// Docs that declare a dependency on `path`.
    pub fn docs_depending_on(&self, path: &str) -> BTreeSet<String> {
        let variants = self.path_variants(path);
        self.dependencies
            .iter()
            .filter(|(_, paths)| variants.iter().any(|variant| paths.contains(variant)))
            .map(|(docname, _)| docname.clone())
            .collect()
    }

    /// Parents whose toctree contains `docname`.
    pub fn toctree_parents_of(&self, docname: &str) -> BTreeSet<String> {
        self.toctree_includes
            .iter()
            .filter(|(_, children)| children.iter().any(|child| child == docname))
            .map(|(parent, _)| parent.clone())
            .collect()
    }

    /// Immediate dependents of `docname` across every registered edge type.
    pub fn direct_dependents(&self, docname: &str) -> BTreeSet<String> {
        let mut result = BTreeSet::new();
        if let Some(dependents) = self.files_to_rebuild.get(docname) {
            result.extend(dependents.iter().cloned());
        }
        result.extend(self.includers_of(docname));
        result.extend(self.docs_depending_on(docname));
        result.extend(self.toctree_parents_of(docname));
        result.remove(docname);
        result
    }

    /// Transitive reverse-dependency closure, cycle-safe.
    pub fn dependent_closure<'a, I>(&self, docnames: I) -> BTreeSet<String>
    where
        I: IntoIterator<Item = &'a str>,
    {
        let seeds: BTreeSet<String> = docnames.into_iter().map(str::to_owned).collect();
        let mut visited = seeds.clone();
        let mut queue: Vec<String> = seeds.into_iter().collect();
        while let Some(docname) = queue.pop() {
            for dependent in self.direct_dependents(&docname) {
                if visited.insert(dependent.clone()) {
                    queue.push(dependent);
                }
            }
        }
        visited
    }

    /// Transitive dependents of one document, excluding the document itself.
    pub fn dependents_of(&self, docname: &str) -> BTreeSet<String> {
        let mut result = self.dependent_closure([docname]);
        result.remove(docname);
        result
    }

    /// All paths that, if changed, would invalidate `docname`.
    pub fn watched_paths_of(&self, docname: &str) -> BTreeSet<String> {
        let mut result = BTreeSet::new();
        if let Some(paths) = self.included.get(docname) {
            result.extend(paths.iter().cloned());
        }
        if let Some(paths) = self.dependencies.get(docname) {
            result.extend(paths.iter().cloned());
        }
        result
    }

    fn path_variants(&self, name: &str) -> Vec<String> {
        let mut variants = vec![name.to_owned()];
        if let Some(path) = self.found_docs.get(name) {
            variants.push(path_to_relative(&self.source_dir, path));
        }
        for (suffix, _) in &self.relevant_config.source_suffix {
            let candidate = format!("{name}{suffix}");
            if !variants.contains(&candidate) {
                variants.push(candidate);
            }
        }
        variants
    }

    // ----- document snapshots ----------------------------------------------

    /// Access one document's snapshot.
    pub fn document(&self, docname: &str) -> Option<&DocumentSnapshot> {
        self.documents.get(docname)
    }

    /// Access all document snapshots.
    pub fn documents(&self) -> &BTreeMap<String, DocumentSnapshot> {
        &self.documents
    }

    /// Install a pre-built document snapshot.
    pub fn set_document_snapshot(&mut self, snapshot: DocumentSnapshot) {
        self.documents.insert(snapshot.docname.clone(), snapshot);
    }

    /// Record a parsed doctree and refresh the associated metadata / hashes.
    ///
    /// The source digest is taken from the current metadata (populated by
    /// [`Self::update`]); included digests are refreshed from disk.
    pub fn record_document(
        &mut self,
        docname: &str,
        document: &Document,
        included_digests: BTreeMap<String, ContentDigest>,
    ) {
        let metadata = extract_document_metadata(document);
        let source_digest = self
            .all_docs
            .get(docname)
            .map(|meta| meta.source_digest)
            .unwrap_or(ContentDigest::ZERO);
        let config_digest = self.relevant_config.digest();
        let included: BTreeMap<String, ContentDigest> = included_digests
            .into_iter()
            .map(|(path, digest)| (path_to_relative(&self.source_dir, Path::new(&path)), digest))
            .collect();
        let dependency_digests = self
            .all_docs
            .get(docname)
            .map(|meta| meta.dependency_digests.clone())
            .unwrap_or_default();
        let combined_digest = hash_combined(
            &source_digest,
            &included,
            &dependency_digests,
            &config_digest,
            &self.parser_version,
        );
        let meta = self.all_docs.entry(docname.to_owned()).or_default();
        meta.source_digest = source_digest;
        meta.config_digest = config_digest;
        meta.parser_version = self.parser_version.clone();
        meta.included_digests = included;
        meta.dependency_digests = dependency_digests;
        meta.combined_digest = combined_digest;
        meta.title = metadata.title.clone();

        let snapshot = DocumentSnapshot {
            docname: docname.to_owned(),
            doctree: Some(document.clone()),
            sections: metadata.sections,
            toctree_targets: metadata.toctree_targets,
            xrefs: metadata.xrefs,
            anchors: metadata.anchors,
        };
        self.documents.insert(docname.to_owned(), snapshot);
    }

    /// Retrieve a cached doctree (the functional equivalent of Sphinx's
    /// `get_doctree()`).
    pub fn get_doctree(&self, docname: &str) -> Option<&Document> {
        self.documents.get(docname).and_then(|snapshot| snapshot.doctree.as_ref())
    }

    // ----- domain data side table (Phase 11, additive) ----------------------

    /// Serialized inventory for `domain`, when one has been saved.
    pub fn domain_data(&self, domain: &str) -> Option<&serde_json::Value> {
        self.domain_data.get(domain)
    }

    /// Replaces the serialized inventory for `domain`.
    pub fn set_domain_data(&mut self, domain: &str, value: serde_json::Value) {
        self.domain_data.insert(domain.to_owned(), value);
    }

    /// All serialized domain inventories, in deterministic domain-name order.
    pub fn domain_data_entries(&self) -> &BTreeMap<String, serde_json::Value> {
        &self.domain_data
    }

    /// Bulk-replaces the domain side table (used by the builder seam).
    pub fn set_domain_data_entries(&mut self, entries: BTreeMap<String, serde_json::Value>) {
        self.domain_data = entries;
    }

    // ----- persistence ------------------------------------------------------

    /// Convert the environment into its deterministic serializable snapshot.
    pub fn to_snapshot(&self) -> EnvironmentSnapshot {
        let to_sorted_vec = |map: &HashMap<String, HashSet<String>>| -> Vec<(String, Vec<String>)> {
            let mut entries: Vec<(String, Vec<String>)> = map
                .iter()
                .map(|(key, values)| {
                    let mut sorted: Vec<String> = values.iter().cloned().collect();
                    sorted.sort();
                    (key.clone(), sorted)
                })
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            entries
        };
        let found_docs = self.found_docs_sorted();
        EnvironmentSnapshot {
            format_version: ENV_FORMAT_VERSION,
            source_dir: canon_path(&self.source_dir.to_string_lossy()),
            parser_version: self.parser_version.clone(),
            relevant_config: self.relevant_config.clone(),
            found_docs: found_docs
                .into_iter()
                .map(|(name, path)| {
                    (name.to_owned(), path_to_relative(&self.source_dir, path))
                })
                .collect(),
            all_docs: self.all_docs.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            dependencies: to_sorted_vec(&self.dependencies),
            included: to_sorted_vec(&self.included),
            files_to_rebuild: to_sorted_vec(&self.files_to_rebuild),
            toctree_includes: self
                .toctree_includes
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            reread_always: self.reread_always.iter().cloned().collect(),
            glob_toctrees: self.glob_toctrees.iter().cloned().collect(),
            numbered_toctrees: self.numbered_toctrees.iter().cloned().collect(),
            documents: self.documents.values().cloned().collect(),
            domain_data: self
                .domain_data
                .iter()
                .map(|(domain, value)| {
                    (
                        domain.clone(),
                        serde_json::to_string(value).unwrap_or_default(),
                    )
                })
                .collect(),
        }
    }

    /// Restore an environment from a snapshot.
    ///
    /// Incompatible versions and changed source directories are rejected as
    /// `ScribeError::Environment`, which callers treat as a cold cache.
    pub fn from_snapshot(snapshot: EnvironmentSnapshot) -> ScribeResult<Self> {
        if snapshot.format_version != ENV_FORMAT_VERSION {
            return Err(ScribeError::Environment {
                message: format!(
                    "environment version {} is incompatible with {}",
                    snapshot.format_version, ENV_FORMAT_VERSION
                ),
            });
        }
        let mut environment = Self::new(PathBuf::from(&snapshot.source_dir));
        environment.parser_version = snapshot.parser_version;
        environment.relevant_config = snapshot.relevant_config;
        environment.found_docs = snapshot
            .found_docs
            .into_iter()
            .map(|(name, path)| {
                (
                    name,
                    PathBuf::from(path.replace('/', std::path::MAIN_SEPARATOR_STR)),
                )
            })
            .collect();
        environment.all_docs = snapshot.all_docs.into_iter().collect();
        environment.dependencies = vec_to_map(snapshot.dependencies);
        environment.included = vec_to_map(snapshot.included);
        environment.files_to_rebuild = vec_to_map(snapshot.files_to_rebuild);
        environment.toctree_includes = snapshot.toctree_includes.into_iter().collect();
        environment.reread_always = snapshot.reread_always.into_iter().collect();
        environment.glob_toctrees = snapshot.glob_toctrees.into_iter().collect();
        environment.numbered_toctrees = snapshot.numbered_toctrees.into_iter().collect();
        environment.documents = snapshot
            .documents
            .into_iter()
            .map(|document| (document.docname.clone(), document))
            .collect();
        environment.domain_data = snapshot
            .domain_data
            .into_iter()
            .filter_map(|(domain, text)| {
                serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .map(|value| (domain, value))
            })
            .collect();
        Ok(environment)
    }

    /// Write the complete environment to `cache_dir/environment`.
    pub fn save(&self, cache_dir: &Path) -> ScribeResult<ContentDigest> {
        let store = CacheStore::<EnvironmentSnapshot>::new(cache_dir);
        Ok(store.save(ENV_CACHE_KEY, &self.to_snapshot())?)
    }

    /// Restore the environment from `cache_dir`, treating any problem as cold.
    pub fn load(cache_dir: &Path) -> ScribeResult<Option<Self>> {
        let store = CacheStore::<EnvironmentSnapshot>::new(cache_dir);
        let Some(snapshot) = store.load(ENV_CACHE_KEY)? else {
            return Ok(None);
        };
        match Self::from_snapshot(snapshot) {
            Ok(environment) => Ok(Some(environment)),
            Err(_) => Ok(None),
        }
    }

    /// Like [`Self::load`], but reports version / source-dir mismatches.
    pub fn load_strict(cache_dir: &Path) -> ScribeResult<Option<Self>> {
        let store = CacheStore::<EnvironmentSnapshot>::new(cache_dir);
        if !store.exists(ENV_CACHE_KEY) {
            return Ok(None);
        }
        let snapshot = store.load_checked(ENV_CACHE_KEY)?;
        Self::from_snapshot(snapshot).map(Some)
    }

    // ----- incremental update ----------------------------------------------

    /// Discover sources, compare them with the cached snapshot and compute the
    /// rebuild set.
    pub fn update(
        &mut self,
        config: &scribe_config::Config,
        source_dir: &Path,
        options: &UpdateOptions,
    ) -> ScribeResult<UpdateReport> {
        // `-E`: discard any saved environment. Otherwise restore it from the
        // cache when available; a corrupt or missing artifact means a cold
        // build (an empty environment), mirroring upstream's fresh-env path.
        let mut restored = false;
        if options.freshenv {
            *self = Self::new(source_dir);
        } else if options.reuse_in_memory {
            // A long-lived caller (scribe serve) already holds the authoritative
            // live environment. Keep it and mark it restored so config-change
            // detection compares against the previous build's values.
            restored = true;
        } else if let Some(cache_dir) = &options.cache_dir {
            // A missing, corrupt or incompatible artifact means a cold build:
            // start from an empty environment rather than trusting stale
            // in-memory state.
            match Self::load(cache_dir)? {
                Some(loaded) => {
                    *self = loaded;
                    restored = true;
                }
                None => *self = Self::new(source_dir),
            }
        }

        // Extension hooks run after the environment is restored; accept their
        // extra docs here so a cache load cannot discard them.
        self.extra_outdated.extend(options.extra_outdated.iter().cloned());

        let old_config = self.relevant_config.clone();
        let old_parser_version = self.parser_version.clone();
        self.source_dir = source_dir.to_path_buf();
        self.relevant_config = RelevantConfig::from_config(config);

        let config_differences = old_config.difference(&self.relevant_config);
        let parser_changed = old_parser_version != self.parser_version;
        let config_changed = options.freshenv
            || !config_differences.is_empty()
            || parser_changed
            || (!restored && self.all_docs.is_empty());

        let mut discovery_options = DiscoveryOptions::from_config(config, source_dir);
        discovery_options.asset_paths = options.asset_paths.clone();
        discovery_options
            .exclude_patterns
            .extend(options.extra_excludes.iter().cloned());
        let discovery = discover_sources(&discovery_options)?;

        let found: BTreeSet<String> = discovery.docs.keys().cloned().collect();
        let removed: BTreeSet<String> = self
            .all_docs
            .keys()
            .filter(|docname| !found.contains(*docname))
            .cloned()
            .collect();
        for docname in &removed {
            self.purge_doc(docname);
        }

        let mut added = BTreeSet::new();
        let mut changed = BTreeSet::new();
        if config_changed || options.write_all {
            added = found.clone();
        } else {
            for (docname, doc) in &discovery.docs {
                let Some(meta) = self.all_docs.get(docname) else {
                    added.insert(docname.clone());
                    continue;
                };
                let mut outdated = doc.digest != meta.source_digest
                    || (meta.mtime_micros != 0 && doc.mtime_micros > meta.mtime_micros)
                    || self.relevant_config.digest() != meta.config_digest
                    || self.parser_version != meta.parser_version
                    || self.reread_always.contains(docname);

                if !outdated {
                    outdated = watched_files_changed(&self.source_dir, meta);
                }
                if !outdated && options.doctree_dir.is_some() {
                    let doctree_path = options
                        .doctree_dir
                        .as_ref()
                        .map(|dir| dir.join(format!("{docname}.doctree")));
                    if let Some(path) = doctree_path {
                        if !path.is_file() {
                            outdated = true;
                        }
                    }
                }
                if outdated {
                    changed.insert(docname.clone());
                }
            }
        }

        // Upstream: adding or removing a document forces globbed toctrees to
        // be re-read. Phase 09 populates `glob_toctrees`.
        if !(added.is_empty() && removed.is_empty()) {
            for docname in self.glob_toctrees.intersection(&found) {
                changed.insert(docname.clone());
            }
        }

        // Reverse edges: a changed included file / toctree child / dependency
        // invalidates its includers, parents and dependents (transitively).
        let seeds: Vec<String> = added.iter().chain(changed.iter()).cloned().collect();
        if !seeds.is_empty() {
            let closure = self.dependent_closure(seeds.iter().map(String::as_str));
            for docname in closure {
                if found.contains(&docname) && !added.contains(&docname) {
                    changed.insert(docname);
                }
            }
        }

        // Phase 27 extension seam (`env-get-outdated` / `env-get-updated`).
        for docname in &self.extra_outdated {
            if found.contains(docname) && !added.contains(docname) {
                changed.insert(docname.clone());
            }
        }
        self.extra_outdated.clear();

        // Refresh metadata baselines for every discovered document.
        let config_digest = self.relevant_config.digest();
        let mut new_found = HashMap::new();
        for (docname, doc) in &discovery.docs {
            new_found.insert(docname.clone(), doc.absolute_path.clone());
            let meta = self.all_docs.entry(docname.clone()).or_default();
            meta.relative_path = doc.relative_path.clone();
            meta.mtime_micros = doc.mtime_micros;
            meta.source_digest = doc.digest;
            meta.config_digest = config_digest;
            meta.parser_version = self.parser_version.clone();
            // Re-hash watched files using their recorded paths.
            for path in self.included.get(docname).cloned().unwrap_or_default() {
                if let Some(existing) = meta.included_digests.get_mut(&path) {
                    *existing = hash_relative(&self.source_dir, &path);
                }
            }
            for path in self.dependencies.get(docname).cloned().unwrap_or_default() {
                if let Some(existing) = meta.dependency_digests.get_mut(&path) {
                    *existing = hash_relative(&self.source_dir, &path);
                }
            }
            meta.combined_digest = hash_combined(
                &doc.digest,
                &meta.included_digests,
                &meta.dependency_digests,
                &config_digest,
                &self.parser_version,
            );
        }
        self.found_docs = new_found;
        self.outdated_docs = added.union(&changed).cloned().collect();

        if options.persist {
            if let Some(cache_dir) = &options.cache_dir {
                self.save(cache_dir)?;
            }
        }

        Ok(UpdateReport {
            added,
            changed,
            removed,
            config_changed,
            config_changed_reason: config_differences,
            forced_full: options.freshenv || options.write_all,
            warnings: discovery.warnings,
        })
    }

    /// Convenience update using the default options (no cache dir).
    pub fn update_defaults(
        &mut self,
        config: &scribe_config::Config,
        source_dir: &Path,
    ) -> ScribeResult<UpdateReport> {
        self.update(config, source_dir, &UpdateOptions::default())
    }

    /// Remove all traces of a document, mirroring `BuildEnvironment.clear_doc`.
    pub fn purge_doc(&mut self, docname: &str) {
        self.all_docs.remove(docname);
        self.found_docs.remove(docname);
        self.dependencies.remove(docname);
        self.included.remove(docname);
        self.files_to_rebuild.remove(docname);
        self.toctree_includes.remove(docname);
        self.reread_always.remove(docname);
        self.glob_toctrees.remove(docname);
        self.numbered_toctrees.remove(docname);
        self.documents.remove(docname);
        self.outdated_docs.remove(docname);
        self.extra_outdated.remove(docname);
        for dependents in self.files_to_rebuild.values_mut() {
            dependents.remove(docname);
        }
        for children in self.toctree_includes.values_mut() {
            children.retain(|child| child != docname);
        }
    }
}

/// Options controlling [`BuildEnvironment::update`].
#[derive(Clone, Debug, Default)]
pub struct UpdateOptions {
    /// `-E` / `--fresh-env`: ignore any saved environment.
    pub freshenv: bool,
    /// `-a` / `--write-all`: force every discovered document to rebuild.
    pub write_all: bool,
    /// Directory holding the environment artifact. `None` disables caching.
    pub cache_dir: Option<PathBuf>,
    /// Whether to persist the updated environment to `cache_dir`.
    pub persist: bool,
    /// Builder asset paths, excluded from discovery.
    pub asset_paths: Vec<String>,
    /// Extra exclude patterns (in addition to `config.exclude_patterns`).
    pub extra_excludes: Vec<String>,
    /// Optional doctree directory; a missing `<docname>.doctree` forces a read.
    pub doctree_dir: Option<PathBuf>,
    /// Phase 27 seam: extra docs supplied by `env-get-outdated` hooks.
    pub extra_outdated: Vec<String>,
    /// Phase 67: reuse the caller's already-populated in-memory environment
    /// instead of reloading it from `cache_dir`. Long-lived processes such as
    /// `scribe serve` set this so an incremental rebuild does not pay the
    /// full environment-cache deserialisation on every change.
    pub reuse_in_memory: bool,
}

/// Result of [`BuildEnvironment::update`].
///
/// Mirrors upstream `get_outdated_files() -> (added, changed, removed)` while
/// adding diagnostic context. `files_to_rebuild()` is the union of `added` and
/// `changed` — the set a builder should actually re-read and rewrite.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpdateReport {
    /// Newly discovered documents.
    pub added: BTreeSet<String>,
    /// Existing documents whose content, includes or dependencies changed.
    pub changed: BTreeSet<String>,
    /// Documents no longer present in the source directory.
    pub removed: BTreeSet<String>,
    /// Whether a relevant config value or the parser generation changed.
    pub config_changed: bool,
    /// Names of config fields that changed.
    pub config_changed_reason: Vec<String>,
    /// Whether `-E` or `-a` forced a full rebuild.
    pub forced_full: bool,
    /// Non-fatal discovery diagnostics.
    pub warnings: Vec<String>,
}

impl UpdateReport {
    /// Documents to rebuild: `added ∪ changed` (upstream `added | changed`).
    pub fn files_to_rebuild(&self) -> BTreeSet<String> {
        self.added.union(&self.changed).cloned().collect()
    }

    /// Documents to re-read, sorted (upstream `sorted(added | changed)`).
    pub fn docs_to_read(&self) -> Vec<String> {
        self.files_to_rebuild().into_iter().collect()
    }

    /// Whether nothing needs rebuilding.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.changed.is_empty() && self.removed.is_empty()
    }

    /// Number of added, changed and removed documents.
    pub fn counts(&self) -> (usize, usize, usize) {
        (self.added.len(), self.changed.len(), self.removed.len())
    }
}

// ----- helpers -------------------------------------------------------------

fn vec_to_map(entries: Vec<(String, Vec<String>)>) -> HashMap<String, HashSet<String>> {
    entries
        .into_iter()
        .map(|(key, values)| (key, values.into_iter().collect()))
        .collect()
}

fn path_to_relative(source_dir: &Path, path: &Path) -> String {
    let relative = if path.is_absolute() {
        path.strip_prefix(source_dir).unwrap_or(path)
    } else {
        path
    };
    canon_path(&relative.to_string_lossy())
        .trim_start_matches("./")
        .to_owned()
}

fn hash_relative(source_dir: &Path, relative: &str) -> ContentDigest {
    let native = relative.replace('/', std::path::MAIN_SEPARATOR_STR);
    ContentDigest::of_file(&source_dir.join(native)).unwrap_or(ContentDigest::ZERO)
}

fn watched_files_changed(source_dir: &Path, meta: &DocMeta) -> bool {
    for (path, recorded) in &meta.included_digests {
        if hash_relative(source_dir, path) != *recorded {
            return true;
        }
    }
    for (path, recorded) in &meta.dependency_digests {
        if hash_relative(source_dir, path) != *recorded {
            return true;
        }
    }
    false
}

fn hash_combined(
    source: &ContentDigest,
    included: &BTreeMap<String, ContentDigest>,
    dependencies: &BTreeMap<String, ContentDigest>,
    config: &ContentDigest,
    parser_version: &str,
) -> ContentDigest {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"source");
    hasher.update(source.as_bytes());
    for (path, digest) in included {
        hasher.update(b"include");
        hasher.update(&(path.len() as u64).to_le_bytes());
        hasher.update(path.as_bytes());
        hasher.update(digest.as_bytes());
    }
    for (path, digest) in dependencies {
        hasher.update(b"dependency");
        hasher.update(&(path.len() as u64).to_le_bytes());
        hasher.update(path.as_bytes());
        hasher.update(digest.as_bytes());
    }
    hasher.update(b"config");
    hasher.update(config.as_bytes());
    hasher.update(b"parser");
    hasher.update(&(parser_version.len() as u64).to_le_bytes());
    hasher.update(parser_version.as_bytes());
    ContentDigest::from_bytes(*hasher.finalize().as_bytes())
}

/// Lightweight inventory metadata extracted from a doctree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExtractedMetadata {
    /// Document title (first `Title` node's text).
    pub title: Option<String>,
    /// Section ids in sorted order.
    pub sections: Vec<String>,
    /// Raw toctree entry docnames.
    pub toctree_targets: Vec<String>,
    /// Pending cross-reference targets.
    pub xrefs: Vec<String>,
    /// Anchor / label names defined in the document.
    pub anchors: Vec<String>,
}

/// Extract lightweight inventory metadata from a doctree.
pub fn extract_document_metadata(document: &Document) -> ExtractedMetadata {
    let mut title = None;
    let mut sections = Vec::new();
    let mut toctree_targets = Vec::new();
    let mut xrefs = Vec::new();
    let mut anchors = Vec::new();

    for (_, node) in document.iter() {
        match node.kind {
            scribe_ast::NodeKind::Title => {
                if title.is_none() {
                    title = Some(node.astext(document).trim().to_owned());
                }
            }
            scribe_ast::NodeKind::Section => {
                sections.extend(node.ids().unwrap_or_default());
            }
            scribe_ast::NodeKind::Toctree => {
                collect_toctree_entries(node, &mut toctree_targets);
            }
            scribe_ast::NodeKind::PendingXref => {
                if let Some(view) = node.pending_xref() {
                    if let Some(target) = view.reftarget() {
                        xrefs.push(target.to_owned());
                    }
                }
            }
            scribe_ast::NodeKind::Target => {
                anchors.extend(node.ids().unwrap_or_default());
                anchors.extend(node.names().unwrap_or_default());
            }
            _ => {}
        }
    }
    sections.sort();
    sections.dedup();
    toctree_targets.sort();
    toctree_targets.dedup();
    xrefs.sort();
    xrefs.dedup();
    anchors.sort();
    anchors.dedup();
    ExtractedMetadata {
        title,
        sections,
        toctree_targets,
        xrefs,
        anchors,
    }
}

fn collect_toctree_entries(node: &scribe_ast::Node, output: &mut Vec<String>) {
    if let Some(entries) = node.attrs.get("entries") {
        match entries {
            scribe_ast::AttributeValue::List(values) => {
                output.extend(values.iter().cloned());
            }
            scribe_ast::AttributeValue::Str(raw) => {
                if let Ok(values) = serde_json::from_str::<Vec<String>>(raw) {
                    output.extend(values);
                }
            }
            _ => {}
        }
    }
    if let Some(scribe_ast::AttributeValue::List(values)) = node.attrs.get("includefiles") {
        output.extend(values.iter().cloned());
    }
}

/// Discover sources into a temporary result (exposed for tooling).
pub fn discover_for_environment(
    environment: &BuildEnvironment,
    config: &scribe_config::Config,
) -> ScribeResult<DiscoveryResult> {
    let mut options = DiscoveryOptions::from_config(config, environment.source_dir());
    options.exclude_patterns.extend(
        environment
            .relevant_config
            .exclude_patterns
            .iter()
            .cloned(),
    );
    discover_sources(&options)
}

/// Resolve a relative path against a discovery result (exposed for tooling).
pub fn doc2path_in<'a>(result: &'a DiscoveryResult, docname: &str) -> Option<&'a Path> {
    crate::discovery::doc2path(result, docname)
}

/// Reverse path lookup over a discovery result (exposed for tooling).
pub fn path2doc_in<'a>(result: &'a DiscoveryResult, path: &Path) -> Option<&'a str> {
    path2doc(result, path)
}

/// `DomainEnv` adapter: domain resolution reads documents and titles through
/// this narrow interface, keeping `scribe-domains` independent of `scribe-core`.
impl DomainEnv for BuildEnvironment {
    fn docnames(&self) -> Vec<String> {
        self.all_docs.keys().cloned().collect()
    }

    fn has_doc(&self, docname: &str) -> bool {
        self.all_docs.contains_key(docname)
    }

    fn title(&self, docname: &str) -> Option<String> {
        self.all_docs
            .get(docname)
            .and_then(|meta| meta.title.clone())
            .or_else(|| {
                self.documents
                    .get(docname)
                    .and_then(|snapshot| snapshot.doctree.as_ref())
                    .and_then(|tree| tree.metadata.title.clone())
            })
    }

    fn doctree(&self, docname: &str) -> Option<&scribe_ast::Document> {
        self.get_doctree(docname)
    }

    fn domain_data(&self, domain: &str) -> Option<&serde_json::Value> {
        BuildEnvironment::domain_data(self, domain)
    }
}
